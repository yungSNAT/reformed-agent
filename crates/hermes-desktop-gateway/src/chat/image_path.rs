//! Safe host-path parsing and handle-relative, no-follow traversal for `image.attach`.

use std::path::{Component, Path, PathBuf};

/// A path refused before any file system call: a network share or device namespace path, a
/// Windows drive-relative path, or a Windows reserved device name. Holds the client's token.
pub(super) struct Refused(pub(super) String);

/// A resolved local image: `path` keeps the client-named path (`cwd.join(...)`) for display name
/// and extension checks, whilst `file` is the already-opened regular file handle returned by
/// handle-relative traversal so the path is never re-opened.
pub(super) struct Found {
    pub(super) path: PathBuf,
    pub(super) file: std::fs::File,
}

/// Maximum symlink hops resolved whilst walking a path (POSIX `SYMLOOP_MAX`).
const MAX_SYMLINK_DEPTH: usize = 40;

/// The image file `raw` names and the words after it, as upstream's `image.attach` reads a
/// terminal drop: a dropped path when `raw` starts like one, else its first token. `None` when
/// it names no file.
pub(super) fn image_path(raw: &str, cwd: &Path) -> Result<Option<(Found, String)>, Refused> {
    if let Some(found) = dropped(raw, cwd)? {
        return Ok(Some(found));
    }
    let (token, remainder) = split_path_input(raw);
    Ok(resolve(&token, cwd)?.map(|found| (found, remainder)))
}

fn starts_like_path(text: &str, quoted: bool) -> bool {
    let unquoted = if quoted { &text[1..] } else { text };
    let bytes = unquoted.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    ["/", "~", "./", "../"]
        .iter()
        .any(|p| unquoted.starts_with(p))
        || (!quoted && is_file_url(unquoted))
        || drive
}

/// Upstream's `_detect_file_drop`: a path at the start of `raw` (optionally quoted; `/`, `~`,
/// `./`, `../`, a drive prefix, or unquoted `file://`), with the words after it.
pub(super) fn dropped(raw: &str, cwd: &Path) -> Result<Option<(Found, String)>, Refused> {
    let text = raw.trim();
    let quoted = text.starts_with(['"', '\'']);
    if !starts_like_path(text, quoted) {
        return Ok(None);
    }
    let (token, remainder) = split_path_input(text);
    if quoted {
        return Ok(resolve(&token, cwd)?.map(|found| (found, remainder)));
    }
    let mut refused = None;
    let candidates = [(text, ""), (token.as_str(), remainder.as_str())]
        .into_iter()
        .chain(
            text.match_indices(' ')
                .rev()
                .map(|(pos, _)| (text[..pos].trim_end(), text[pos + 1..].trim())),
        );
    for (candidate, rest) in candidates {
        match resolve(candidate, cwd) {
            Ok(Some(found)) => return Ok(Some((found, rest.to_string()))),
            Ok(None) => {}
            Err(err) => {
                if refused.is_none() {
                    refused = Some(err);
                }
            }
        }
    }
    match refused {
        Some(err) => Err(err),
        None => Ok(None),
    }
}

/// Upstream's `_split_path_input`: a leading path token, quoted or with `\ ` escapes, and the
/// text after it.
pub(super) fn split_path_input(raw: &str) -> (String, String) {
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    if let Some(&quote @ (b'"' | b'\'')) = bytes.first() {
        let mut pos = 1;
        while pos < bytes.len() {
            if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
                pos += 2;
                continue;
            }
            if bytes[pos] == quote {
                return (raw[1..pos].to_string(), raw[pos + 1..].trim().to_string());
            }
            pos += 1;
        }
        return (raw[1..].to_string(), String::new());
    }
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' if bytes.get(pos + 1) == Some(&b' ') => pos += 2,
            b' ' => break,
            _ => pos += 1,
        }
    }
    (
        raw[..pos].replace("\\ ", " "),
        raw[pos..].trim().to_string(),
    )
}

/// Whether `text` begins with the `file://` URI scheme (case-insensitive).
fn is_file_url(text: &str) -> bool {
    text.get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
}

fn is_drive_relative(bytes: &[u8]) -> bool {
    let [b'a'..=b'z' | b'A'..=b'Z', b':', rest @ ..] = bytes else {
        return false;
    };
    !matches!(rest.first(), Some(b'/' | b'\\'))
}

/// Refuses 2 leading separators in any mix (`\\host\share`, `\\?\`, `\\.\`, `//host`, `/\host`,
/// `\/host`), the `\??\` prefix, Windows drive-relative paths (`C:foo`, `C:..\bar`, `C:`), or any
/// component naming a Windows reserved device name.
fn check_safe_path(path: &Path, raw: &str) -> Result<(), Refused> {
    let shown = path.as_os_str().to_string_lossy();
    let mut lead = shown.chars();
    let separator = |c: Option<char>| matches!(c, Some('/' | '\\'));
    if (separator(lead.next()) && separator(lead.next()))
        || shown.starts_with(r"\??\")
        || is_drive_relative(shown.as_bytes())
        || shown.split(['/', '\\']).any(reserved_name)
    {
        return Err(Refused(raw.trim().to_string()));
    }
    Ok(())
}

fn has_dir_suffix(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    let trimmed = bytes.strip_suffix(b".").unwrap_or(bytes);
    trimmed.ends_with(b"/") || trimmed.ends_with(b"\\")
}

#[cfg(unix)]
enum Step {
    Root,
    Parent,
    Name(std::ffi::OsString),
}

#[cfg(unix)]
enum WalkError {
    NotFound,
    Refused(Refused),
}

#[cfg(unix)]
impl From<rustix::io::Errno> for WalkError {
    fn from(_: rustix::io::Errno) -> Self {
        Self::NotFound
    }
}

#[cfg(unix)]
impl From<Refused> for WalkError {
    fn from(err: Refused) -> Self {
        Self::Refused(err)
    }
}

#[cfg(unix)]
fn collect_steps(path: &Path) -> Vec<Step> {
    path.components()
        .filter_map(|part| match part {
            Component::RootDir => Some(Step::Root),
            Component::ParentDir => Some(Step::Parent),
            Component::Normal(name) => Some(Step::Name(name.to_os_string())),
            _ => None,
        })
        .collect()
}

#[cfg(unix)]
fn open_root() -> rustix::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    rustix::fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

#[cfg(unix)]
fn open_dir_at(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
) -> rustix::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(dir, name, flags, Mode::empty())
}

#[cfg(unix)]
fn open_leaf_at(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
) -> rustix::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    rustix::fs::openat(dir, name, flags, Mode::empty())
}

#[cfg(unix)]
fn follow_link(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    raw: &str,
    is_leaf: bool,
    hops: &mut usize,
    queue: &mut std::collections::VecDeque<Step>,
) -> Result<(), WalkError> {
    use std::os::unix::ffi::OsStrExt as _;
    let raw_target = rustix::fs::readlinkat(dir, name, Vec::new())?;
    let target = Path::new(std::ffi::OsStr::from_bytes(raw_target.as_bytes()));
    if is_leaf && has_dir_suffix(target) {
        return Err(WalkError::NotFound);
    }
    *hops += 1;
    if *hops > MAX_SYMLINK_DEPTH {
        return Err(WalkError::NotFound);
    }
    check_safe_path(target, raw)?;
    for step in collect_steps(target).into_iter().rev() {
        queue.push_front(step);
    }
    Ok(())
}

/// Traverses `path` component by component relative to pinned directory file descriptors using
/// `openat` with `O_NOFOLLOW`, validating every `readlinkat` target with [`check_safe_path`]
/// before following it, and returns the opened regular file handle directly.
#[cfg(unix)]
fn open_unix(path: &Path, raw: &str) -> Result<std::fs::File, WalkError> {
    if has_dir_suffix(path) {
        return Err(WalkError::NotFound);
    }
    let mut dir = open_root()?;
    let mut queue: std::collections::VecDeque<Step> = collect_steps(path).into();
    let mut hops = 0usize;
    while let Some(step) = queue.pop_front() {
        match step {
            Step::Root => dir = open_root()?,
            Step::Parent => dir = open_dir_at(&dir, std::ffi::OsStr::new(".."))?,
            Step::Name(name) if queue.is_empty() => match open_leaf_at(&dir, &name) {
                Ok(fd) => {
                    let file = std::fs::File::from(fd);
                    return file
                        .metadata()
                        .is_ok_and(|m| m.is_file())
                        .then_some(file)
                        .ok_or(WalkError::NotFound);
                }
                Err(_) => follow_link(&dir, &name, raw, true, &mut hops, &mut queue)?,
            },
            Step::Name(name) => match open_dir_at(&dir, &name) {
                Ok(next) => dir = next,
                Err(_) => follow_link(&dir, &name, raw, false, &mut hops, &mut queue)?,
            },
        }
    }
    Err(WalkError::NotFound)
}

#[cfg(unix)]
fn open_target(path: &Path, raw: &str) -> Result<Option<std::fs::File>, Refused> {
    match open_unix(path, raw) {
        Ok(file) => Ok(Some(file)),
        Err(WalkError::NotFound) => Ok(None),
        Err(WalkError::Refused(err)) => Err(err),
    }
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(windows)]
fn open_dir_pinned(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    // Omit `FILE_SHARE_DELETE` so NTFS blocks rename/replacement whilst ancestor handles stay open.
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(windows)]
fn open_windows_leaf(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(windows)]
fn expand_windows_link(
    cur: &mut PathBuf,
    raw: &str,
    hops: &mut usize,
    tail: &Path,
) -> Result<Option<PathBuf>, Refused> {
    let Ok(target) = std::fs::read_link(cur.as_path()) else {
        return Ok(None);
    };
    if tail.as_os_str().is_empty() && has_dir_suffix(&target) {
        return Ok(None);
    }
    *hops += 1;
    if *hops > MAX_SYMLINK_DEPTH {
        return Ok(None);
    }
    check_safe_path(&target, raw)?;
    cur.pop();
    let next = cur.join(target);
    Ok(Some(if tail.as_os_str().is_empty() {
        next
    } else {
        next.join(tail)
    }))
}

#[cfg(windows)]
fn open_windows_step(path: &Path, is_leaf: bool) -> Option<(std::fs::File, std::fs::Metadata)> {
    let handle = if is_leaf {
        open_windows_leaf(path).ok()?
    } else {
        open_dir_pinned(path).ok()?
    };
    let meta = handle.metadata().ok()?;
    Some((handle, meta))
}

/// Walks `path` component by component on Windows, pinning every traversed ancestor directory
/// without `FILE_SHARE_DELETE` so no ancestor can be swapped with a symlink or junction before
/// the leaf is opened with `FILE_FLAG_OPEN_REPARSE_POINT`.
#[cfg(windows)]
fn open_target(path: &Path, raw: &str) -> Result<Option<std::fs::File>, Refused> {
    let mut work = path.to_path_buf();
    let mut hops = 0usize;
    'walk: loop {
        if has_dir_suffix(&work) {
            return Ok(None);
        }
        let mut cur = PathBuf::new();
        let mut pinned = Vec::new();
        let mut parts = work.components().peekable();
        while let Some(part) = parts.next() {
            match part {
                Component::ParentDir => {
                    cur.pop();
                    pinned.pop();
                }
                Component::Normal(name) => {
                    cur.push(name);
                    let is_leaf = parts.peek().is_none();
                    let Some((handle, meta)) = open_windows_step(&cur, is_leaf) else {
                        return Ok(None);
                    };
                    if is_reparse_point(&meta) {
                        let tail: PathBuf = parts.collect();
                        drop((handle, pinned));
                        let Some(next) = expand_windows_link(&mut cur, raw, &mut hops, &tail)?
                        else {
                            return Ok(None);
                        };
                        work = next;
                        continue 'walk;
                    }
                    if is_leaf {
                        return Ok(meta.is_file().then_some(handle));
                    }
                    if !meta.is_dir() {
                        return Ok(None);
                    }
                    pinned.push(handle);
                }
                other => cur.push(other.as_os_str()),
            }
        }
        return Ok(None);
    }
}

/// Upstream's `_resolve_attachment_path`: quotes, `\ ` escapes, `file://` and a leading `~`
/// are undone, a relative path is taken from `cwd`, and only an existing regular file is returned.
/// Before anything touches it, [`Refused`]: 2 leading separators in any mix (`\\host\share`,
/// `\\?\`, `\\.\`, `//host`, `/\host`, `\/host`), the `\??\` prefix, a `file://` URL naming a
/// host, a drive-relative path, a reserved device name ([`reserved_name`]) in any component, or
/// a symlink/junction targeting any of those.
pub(super) fn resolve(raw: &str, cwd: &Path) -> Result<Option<Found>, Refused> {
    let mut token = raw.trim();
    if token.len() >= 2 && token.starts_with(['"', '\'']) && token.ends_with(&token[..1]) {
        token = token[1..token.len() - 1].trim();
    }
    let token = token.replace("\\ ", " ");
    if token.is_empty() {
        return Ok(None);
    }
    let path = if is_file_url(&token) {
        match url::Url::parse(&token) {
            Ok(url)
                if url
                    .host_str()
                    .is_some_and(|h| !h.is_empty() && h != "localhost") =>
            {
                return Err(Refused(raw.trim().to_string()));
            }
            Ok(url) => url
                .to_file_path()
                .unwrap_or_else(|()| PathBuf::from(&token)),
            Err(_) => PathBuf::from(&token),
        }
    } else if let Some(expanded) = token
        .strip_prefix('~')
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
        .and_then(|rest| Some(home()?.join(rest.trim_start_matches(['/', '\\']))))
    {
        // With no `HOME` or `USERPROFILE`, `~` stays as written.
        expanded
    } else {
        PathBuf::from(&token)
    };
    check_safe_path(&path, raw)?;
    let path = cwd.join(path);
    Ok(open_target(&path, raw)?.map(|file| Found { path, file }))
}

/// Whether `name` is a Windows reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `CONIN$`,
/// `CONOUT$`, `COM0` to `COM9`, `LPT0` to `LPT9`, and `COM` or `LPT` with a superscript 1, 2
/// or 3), in any case, with or without an extension or trailing spaces and dots. Refused on
/// every platform, so a path means the same file everywhere.
fn reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default();
    let stem = stem.trim_end_matches([' ', '.']).to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let Some(port) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut chars = port.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

/// The user's home folder, as `~` names it. An empty `HOME` falls back to `USERPROFILE`.
pub(super) fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|key| std::env::var_os(key).filter(|home| !home.is_empty()))
        .map(PathBuf::from)
}
