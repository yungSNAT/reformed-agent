use std::ffi::OsStr;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, CWD};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::roots::{io_refusal, sensitive, Refusal, Resolved};

/// The largest file `read-data-url` returns (upstream's `WEBAPP_ATTACHMENT_MAX_BYTES`).
pub(crate) const DATA_URL_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// The most entries one listing returns; past it the answer says `truncated`.
pub(crate) const LIST_MAX_ENTRIES: usize = 10_000;

/// Folder names a listing leaves out (upstream's `_FS_READDIR_HIDDEN`).
const LIST_HIDDEN: [&str; 13] = [
    ".git",
    ".hg",
    ".svn",
    ".cache",
    ".next",
    ".turbo",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "venv",
];

/// What a walk opened.
pub(crate) enum Opened {
    Dir(OwnedFd),
    File(std::fs::File, Stat),
}

pub(crate) fn openat(dir: impl AsFd, name: &OsStr, flags: OFlags) -> std::io::Result<OwnedFd> {
    Ok(rustix::fs::openat(
        dir,
        name,
        flags | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?)
}

/// Opens `resolved` by walking it from its root's descriptor, one segment at a time without
/// following a link, so what opens is what was checked. Only a folder or a regular file opens;
/// a FIFO or device is refused without blocking.
pub(crate) fn open_resolved(resolved: &Resolved) -> Result<Opened, Refusal> {
    let rel = resolved
        .path
        .strip_prefix(&resolved.root)
        .map_err(|_| Refusal::Outside)?;
    let mut dir = openat(
        CWD,
        resolved.root.as_os_str(),
        OFlags::RDONLY | OFlags::DIRECTORY,
    )
    .map_err(|e| io_refusal(&e))?;
    let parts: Vec<&OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(n) => Ok(n),
            _ => Err(Refusal::Outside),
        })
        .collect::<Result<_, _>>()?;
    let Some((last, folders)) = parts.split_last() else {
        return Ok(Opened::Dir(dir));
    };
    for part in folders {
        dir = openat(&dir, part, OFlags::RDONLY | OFlags::DIRECTORY).map_err(|e| io_refusal(&e))?;
    }
    let fd = openat(&dir, last, OFlags::RDONLY | OFlags::NONBLOCK).map_err(|e| io_refusal(&e))?;
    let st = rustix::fs::fstat(&fd).map_err(|e| io_refusal(&e.into()))?;
    match FileType::from_raw_mode(st.st_mode) {
        FileType::Directory => Ok(Opened::Dir(fd)),
        FileType::RegularFile => Ok(Opened::File(std::fs::File::from(fd), st)),
        _ => Err(Refusal::NotAFile),
    }
}

/// The entries of an opened folder, as upstream lists them: folders first, then by name;
/// build folders, sensitive entries and refused trees left out; links listed by their own type.
pub(crate) fn list_entries(
    dir: OwnedFd,
    shown: &Path,
    refused: &[PathBuf],
) -> std::io::Result<(Vec<Value>, bool)> {
    let stream = Dir::read_from(&dir)?;
    let mut entries: Vec<(bool, String)> = Vec::new();
    let mut truncated = false;
    for entry in stream.flatten() {
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let os = OsStr::from_bytes(bytes);
        let text = os.to_string_lossy().into_owned();
        let entry_path = shown.join(os);
        if LIST_HIDDEN.contains(&text.as_str())
            || sensitive(&entry_path)
            || refused.iter().any(|r| entry_path.starts_with(r))
        {
            continue;
        }
        if entries.len() == LIST_MAX_ENTRIES {
            truncated = true;
            break;
        }
        let is_dir = rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|st| FileType::from_raw_mode(st.st_mode) == FileType::Directory);
        entries.push((is_dir, text));
    }
    entries.sort_by(|a, b| (!a.0, a.1.to_lowercase(), &a.1).cmp(&(!b.0, b.1.to_lowercase(), &b.1)));
    let list = entries
        .into_iter()
        .map(|(is_dir, name)| {
            json!({"name": name, "path": shown.join(&name).to_string_lossy(), "isDirectory": is_dir})
        })
        .collect();
    Ok((list, truncated))
}

pub(crate) fn mime_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/vnd.microsoft.icon",
        Some("avif") => "image/avif",
        Some("heic") => "image/heic",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js" | "mjs") => "text/javascript",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

pub(crate) fn etag(path: &Path, st: &Stat) -> String {
    let mut hash = Sha256::new();
    hash.update(path.as_os_str().as_bytes());
    hash.update(format!(
        "\0{}\0{}\0{}\0{}.{}",
        st.st_dev, st.st_ino, st.st_size, st.st_mtime, st.st_mtime_nsec
    ));
    let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{}\"", &hex[..32])
}

/// A regular file's bytes, read through its descriptor and capped by what is read.
pub(crate) fn read_capped(file: std::fs::File, max: u64) -> Result<Vec<u8>, Refusal> {
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_refusal(&e))?;
    if bytes.len() as u64 > max {
        return Err(Refusal::TooLarge);
    }
    Ok(bytes)
}
