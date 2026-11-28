//! `image.attach` and `image.detach`: an image the client names by a path on the gateway's
//! host, as the terminal UI and a desktop on the same machine send it, staged for the session's
//! next prompt; and a staged image taken back when the client removes it.
//!
//! `image.attach` serves only local clients (`Origin::Local`), because it reads a host path the
//! client names. The file is copied under `<home>/images` and checked as `image.attach_bytes`
//! checks bytes (signature and size), so the prompt sends what was attached even if the file
//! changes afterwards.
//!
//! `image.detach` serves any authenticated connection (matching `image.attach_bytes`), because it
//! removes only staged copies already queued on the named session under `<home>/images` and never
//! accesses arbitrary host paths.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Map, Value};

use super::session::{MAX_STAGED_BYTES, MAX_STAGED_IMAGES};
use super::{
    ready, Chat, Handler, Origin, Reply, Request, RpcError, ATTACHMENT_REQUIRED, IMAGE_INVALID,
    IMAGE_TOO_LARGE, IMAGE_UNSUPPORTED, IMAGE_WRITE_FAILED, INVALID_PARAMS, SESSION_NOT_FOUND,
};
use crate::attach::{stage_image_bytes, ImageError, MAX_IMAGE_BYTES};

pub(super) const METHODS: &[(&str, Handler)] =
    &[("image.attach", attach), ("image.detach", detach)];

/// Upstream's image extensions (`_IMAGE_EXTENSIONS`, hermes_cli/cli_terminal_input.py); the
/// bytes then decide whether the image is one the model is sent.
const IMAGE_EXTENSIONS: [&str; 10] = [
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "svg", "ico",
];

fn attach(req: Request) -> Reply {
    ready(in_place(|| {
        local_only(req.conn.origin)?;
        attach_now(&req.chat, &req.params)
    }))
}

fn detach(req: Request) -> Reply {
    ready(in_place(|| detach_now(&req.chat, &req.params)))
}

/// Runs `f`, which touches the disk, before the method returns, as `image.attach_bytes` does:
/// the connection takes its next request only after a method that answers at once, so an
/// image attached before a pipelined `prompt.submit` goes with that prompt.
fn in_place<T>(f: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current().map(|handle| handle.runtime_flavor()) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(f),
        _ => f(),
    }
}

/// `image.attach` reads a host path only for a client on this machine. Upstream has no such
/// check; its gateway serves only local clients.
fn local_only(origin: Origin) -> Result<(), RpcError> {
    if origin == Origin::Local {
        return Ok(());
    }
    Err((
        IMAGE_UNSUPPORTED,
        "image.attach reads a path only for a client on this machine; send the image with \
         image.attach_bytes"
            .into(),
    ))
}

/// The `session_id` and non-empty `path` strings of `params`.
fn session_and_path(params: &Map<String, Value>) -> Result<(&str, &str), RpcError> {
    let Some(Value::String(id)) = params.get("session_id") else {
        return Err((INVALID_PARAMS, "session_id must be a string".into()));
    };
    let path = match params.get("path") {
        Some(Value::String(path)) => path.trim(),
        None | Some(Value::Null) => "",
        Some(_) => return Err((INVALID_PARAMS, "path must be a string".into())),
    };
    if path.is_empty() {
        return Err((ATTACHMENT_REQUIRED, "path required".into()));
    }
    Ok((id, path))
}

fn attach_now(chat: &Chat, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let (id, raw) = session_and_path(params)?;
    let Some(session) = chat.lock_sessions().get(id).cloned() else {
        return Err((SESSION_NOT_FOUND, "session not found".into()));
    };
    let cwd = std::env::current_dir()
        .map_err(|e| (IMAGE_WRITE_FAILED, format!("no working folder: {e}")))?;
    let (found, remainder) = match image_path(raw, &cwd) {
        Ok(Some(found)) => found,
        Ok(None) => {
            let (token, _) = split_path_input(raw);
            return Err((IMAGE_UNSUPPORTED, format!("image not found: {token}")));
        }
        Err(Refused(token)) => {
            return Err((
                IMAGE_UNSUPPORTED,
                format!(
                    "image path refused: network shares, device paths and reserved device \
                     names are not read: {token}"
                ),
            ));
        }
    };
    let name = found
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unsupported = || (IMAGE_UNSUPPORTED, format!("unsupported image: {name}"));
    let ext = found
        .path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if !ext.is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str())) {
        return Err(unsupported());
    }
    let too_large = || {
        (
            IMAGE_TOO_LARGE,
            format!("image larger than {MAX_IMAGE_BYTES} bytes"),
        )
    };
    let full = || {
        (
            IMAGE_TOO_LARGE,
            format!(
                "a session holds at most {MAX_STAGED_IMAGES} images ({MAX_STAGED_BYTES} bytes)"
            ),
        )
    };
    if !session.has_room_for_image(0) {
        return Err(full());
    }
    let data = read_image(&found.target)
        .map_err(|e| (IMAGE_WRITE_FAILED, format!("cannot read {name}: {e}")))?;
    let staged = stage_image_bytes(&chat.home.join("images"), &data).map_err(|e| match e {
        ImageError::Unsupported => unsupported(),
        ImageError::TooLarge => too_large(),
        ImageError::Invalid(message) => (IMAGE_INVALID, message),
        ImageError::Write(message) => (IMAGE_WRITE_FAILED, message),
    })?;
    let mut reply = json!({
        "attached": true,
        "name": name,
        "path": staged.path.display().to_string(),
        "bytes": staged.bytes,
        "remainder": remainder,
        "text": if remainder.is_empty() {
            format!("[User attached image: {name}]")
        } else {
            remainder.clone()
        },
    });
    match session.stage_image(staged) {
        Ok(count) => reply["count"] = count.into(),
        Err(refused) => {
            let _ = std::fs::remove_file(&refused.path);
            return Err(full());
        }
    }
    Ok(reply)
}

/// The bytes of the regular file at `path`, read through one handle and at most one byte past
/// [`MAX_IMAGE_BYTES`] (a file may report a smaller size than it reads), so staging refuses one
/// too large. Opens without following a final link or reparse point, returns an error when the
/// handle is not a regular file (one swapped in after [`resolve`] checked the path), and opens a
/// FIFO without waiting for a writer.
fn read_image(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOFOLLOW,
    );
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::custom_flags(
        &mut options,
        windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
    );
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut data = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut data)?;
    Ok(data)
}

fn detach_now(chat: &Chat, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let (id, raw) = session_and_path(params)?;
    let Some(session) = chat.lock_sessions().get(id).cloned() else {
        return Err((SESSION_NOT_FOUND, "session not found".into()));
    };
    let (removed, count) = {
        let mut queued = session.images();
        let removed = queued
            .iter()
            .position(|image| image.path == Path::new(raw))
            .map(|index| queued.remove(index));
        (removed, queued.len())
    };
    let Some(image) = removed else {
        return Ok(json!({"detached": false, "count": count}));
    };
    // Each staged image is this gateway's own copy under `<home>/images`.
    match std::fs::remove_file(&image.path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            session.images().push(image);
            Err((
                IMAGE_WRITE_FAILED,
                format!("cannot remove staged image: {e}"),
            ))
        }
        Ok(()) | Err(_) => Ok(json!({"detached": true, "count": count})),
    }
}

/// A path refused before any file system call: a network share or device namespace path, or
/// a Windows reserved device name. Holds the client's token.
struct Refused(String);

/// A resolved local image: `path` keeps the client-named path (`cwd.join(...)`) for display name
/// and extension checks, whilst `target` is the link-free regular file opened by [`read_image`].
struct Found {
    path: PathBuf,
    target: PathBuf,
}

/// Maximum symlink hops resolved whilst walking a path (POSIX `SYMLOOP_MAX`).
const MAX_SYMLINK_DEPTH: usize = 40;

/// The image file `raw` names and the words after it, as upstream's `image.attach` reads a
/// terminal drop: a dropped path when `raw` starts like one, else its first token. `None` when
/// it names no file.
fn image_path(raw: &str, cwd: &Path) -> Result<Option<(Found, String)>, Refused> {
    if let Some(found) = dropped(raw, cwd)? {
        return Ok(Some(found));
    }
    let (token, remainder) = split_path_input(raw);
    Ok(resolve(&token, cwd)?.map(|found| (found, remainder)))
}

/// Upstream's `_detect_file_drop`: a path at the start of `raw` (optionally quoted; `/`, `~`,
/// `./`, `../`, a drive prefix, or unquoted `file://`), with the words after it.
fn dropped(raw: &str, cwd: &Path) -> Result<Option<(Found, String)>, Refused> {
    let text = raw.trim();
    let quoted = text.starts_with(['"', '\'']);
    let unquoted = if quoted { &text[1..] } else { text };
    let bytes = unquoted.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    let like_path = ["/", "~", "./", "../"]
        .iter()
        .any(|p| unquoted.starts_with(p))
        || (!quoted && is_file_url(unquoted))
        || drive;
    if !like_path {
        return Ok(None);
    }
    if let Some(found) = resolve(text, cwd)? {
        return Ok(Some((found, String::new())));
    }
    let (token, remainder) = split_path_input(text);
    if let Some(found) = resolve(&token, cwd)? {
        return Ok(Some((found, remainder)));
    }
    if quoted {
        return Ok(None);
    }
    // An unquoted path with spaces: the longest prefix ending before a space that names a file.
    for (pos, _) in text.match_indices(' ').rev() {
        if let Some(found) = resolve(text[..pos].trim_end(), cwd)? {
            return Ok(Some((found, text[pos + 1..].trim().to_string())));
        }
    }
    Ok(None)
}

/// Upstream's `_split_path_input`: a leading path token, quoted or with `\ ` escapes, and the
/// text after it.
fn split_path_input(raw: &str) -> (String, String) {
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

/// Refuses 2 leading separators in any mix (`\\host\share`, `\\?\`, `\\.\`, `//host`, `/\host`,
/// `\/host`), the `\??\` prefix, or any component naming a Windows reserved device name.
fn check_safe_path(path: &Path, raw: &str) -> Result<(), Refused> {
    let shown = path.as_os_str().to_string_lossy();
    let mut lead = shown.chars();
    let separator = |c: Option<char>| matches!(c, Some('/' | '\\'));
    if (separator(lead.next()) && separator(lead.next()))
        || shown.starts_with(r"\??\")
        || shown.split(['/', '\\']).any(reserved_name)
    {
        return Err(Refused(raw.trim().to_string()));
    }
    Ok(())
}

/// Walks `path` component by component using `symlink_metadata`, validating every `read_link`
/// target with [`check_safe_path`] before following it. Returns the resolved link-free path when
/// the final entry is a regular file, `Ok(None)` when missing, non-regular or a link loop, and
/// `Err(Refused)` when any link points to a refused path.
fn resolve_target(path: &Path, raw: &str) -> Result<Option<PathBuf>, Refused> {
    let mut work = path.to_path_buf();
    let mut hops = 0usize;
    'walk: loop {
        let bytes = work.as_os_str().as_encoded_bytes();
        let trimmed = bytes.strip_suffix(b".").unwrap_or(bytes);
        if trimmed.ends_with(b"/") || trimmed.ends_with(b"\\") {
            return Ok(None);
        }
        let mut cur = PathBuf::new();
        let mut is_file = false;
        let mut parts = work.components();
        while let Some(part) = parts.next() {
            if is_file {
                return Ok(None);
            }
            match part {
                Component::ParentDir => {
                    cur.pop();
                }
                Component::Normal(name) => {
                    cur.push(name);
                    let Ok(meta) = std::fs::symlink_metadata(&cur) else {
                        return Ok(None);
                    };
                    if !meta.is_file() && !meta.is_dir() {
                        let Ok(target) = std::fs::read_link(&cur) else {
                            return Ok(None);
                        };
                        hops += 1;
                        if hops > MAX_SYMLINK_DEPTH {
                            return Ok(None);
                        }
                        check_safe_path(&target, raw)?;
                        cur.pop();
                        let mut next = cur.join(target);
                        next.extend(parts);
                        work = next;
                        continue 'walk;
                    }
                    is_file = meta.is_file();
                }
                other => cur.push(other.as_os_str()),
            }
        }
        return Ok(is_file.then_some(cur));
    }
}

/// Upstream's `_resolve_attachment_path`: quotes, `\ ` escapes, `file://` and a leading `~`
/// are undone, a relative path is taken from `cwd`, and only an existing file is returned.
/// Before anything touches it, [`Refused`]: 2 leading separators in any mix (`\\host\share`,
/// `\\?\`, `\\.\`, `//host`, `/\host`, `\/host`), the `\??\` prefix, a `file://` URL naming a
/// host, a reserved device name ([`reserved_name`]) in any component, or a symlink/junction
/// targeting any of those.
fn resolve(raw: &str, cwd: &Path) -> Result<Option<Found>, Refused> {
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
    Ok(resolve_target(&path, raw)?.map(|target| Found { path, target }))
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
fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|key| std::env::var_os(key).filter(|home| !home.is_empty()))
        .map(PathBuf::from)
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
