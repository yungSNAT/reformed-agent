//! `image.attach` and `image.detach`: an image the client names by a path on the gateway's
//! host, as the terminal UI and a desktop on the same machine send it, staged for the session's
//! next prompt; and a staged image taken back when the client removes it.
//!
//! `image.attach` reads the path the client names, as upstream's does: the gateway runs on the
//! client's machine and answers only its token. The file is copied under `<home>/images` and
//! checked as `image.attach_bytes` checks bytes (signature and size), so the prompt sends what
//! was attached even if the file changes afterwards.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::{
    Chat, Handler, Reply, Request, RpcError, ATTACHMENT_REQUIRED, IMAGE_INVALID, IMAGE_TOO_LARGE,
    IMAGE_UNSUPPORTED, IMAGE_WRITE_FAILED, INVALID_PARAMS, SESSION_NOT_FOUND,
};
use crate::attach::{stage_image_bytes, ImageError, MAX_IMAGE_BYTES};

pub(super) const METHODS: &[(&str, Handler)] = &[("image.attach", attach), ("image.detach", detach)];

/// Upstream's image extensions (`_IMAGE_EXTENSIONS`, hermes_cli/cli_terminal_input.py); the
/// bytes then decide whether the image is one the model is sent.
const IMAGE_EXTENSIONS: [&str; 10] = [
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "svg", "ico",
];

fn attach(req: Request) -> Reply {
    blocking(req, attach_now)
}

fn detach(req: Request) -> Reply {
    blocking(req, detach_now)
}

/// Runs `f` on the blocking pool: both methods touch the disk.
fn blocking(req: Request, f: fn(&Chat, &Map<String, Value>) -> Result<Value, RpcError>) -> Reply {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || f(&req.chat, &req.params))
            .await
            .unwrap_or_else(|e| Err((IMAGE_WRITE_FAILED, e.to_string())))
    })
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
    let (source, remainder) = image_path(raw, &cwd)
        .map_err(|token| (IMAGE_UNSUPPORTED, format!("image not found: {token}")))?;
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unsupported = || (IMAGE_UNSUPPORTED, format!("unsupported image: {name}"));
    let ext = source
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
    let read_failed = |e: std::io::Error| (IMAGE_WRITE_FAILED, format!("cannot read {name}: {e}"));
    let size = std::fs::metadata(&source).map_err(read_failed)?.len();
    if size > MAX_IMAGE_BYTES as u64 {
        return Err(too_large());
    }
    let data = std::fs::read(&source).map_err(read_failed)?;
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
    let mut queued = session.images();
    queued.push(staged);
    reply["count"] = queued.len().into();
    Ok(reply)
}

fn detach_now(chat: &Chat, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let (id, raw) = session_and_path(params)?;
    let Some(session) = chat.lock_sessions().get(id).cloned() else {
        return Err((SESSION_NOT_FOUND, "session not found".into()));
    };
    let (removed, count) = {
        let mut queued = session.images();
        let (removed, kept) = std::mem::take(&mut *queued)
            .into_iter()
            .partition::<Vec<_>, _>(|image| image.path == Path::new(raw));
        *queued = kept;
        (removed, queued.len())
    };
    // Each staged image is this gateway's own copy under `<home>/images`.
    for image in &removed {
        let _ = std::fs::remove_file(&image.path);
    }
    Ok(json!({"detached": !removed.is_empty(), "count": count}))
}

/// The image file `raw` names and the words after it, as upstream's `image.attach` reads a
/// terminal drop: a dropped path when `raw` starts like one, else its first token. `Err` holds
/// the token that named no file.
fn image_path(raw: &str, cwd: &Path) -> Result<(PathBuf, String), String> {
    if let Some(found) = dropped(raw, cwd) {
        return Ok(found);
    }
    let (token, remainder) = split_path_input(raw);
    resolve(&token, cwd).map(|path| (path, remainder)).ok_or(token)
}

/// Upstream's `_detect_file_drop`: a path at the start of `raw` (optionally quoted; `/`, `~`,
/// `./`, `../`, a drive prefix, or unquoted `file://`), with the words after it.
fn dropped(raw: &str, cwd: &Path) -> Option<(PathBuf, String)> {
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
        || (!quoted && unquoted.starts_with("file://"))
        || drive;
    if !like_path {
        return None;
    }
    if let Some(path) = resolve(text, cwd) {
        return Some((path, String::new()));
    }
    let (token, remainder) = split_path_input(text);
    if let Some(path) = resolve(&token, cwd) {
        return Some((path, remainder));
    }
    if quoted {
        return None;
    }
    // An unquoted path with spaces: the longest prefix ending before a space that names a file.
    text.match_indices(' ').rev().find_map(|(pos, _)| {
        resolve(text[..pos].trim_end(), cwd).map(|path| (path, text[pos + 1..].trim().to_string()))
    })
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
    (raw[..pos].replace("\\ ", " "), raw[pos..].trim().to_string())
}

/// Upstream's `_resolve_attachment_path`: quotes, `\ ` escapes, `file://` and a leading `~`
/// are undone, a relative path is taken from `cwd`, and only an existing file is returned.
fn resolve(raw: &str, cwd: &Path) -> Option<PathBuf> {
    let mut token = raw.trim();
    if token.len() >= 2 && token.starts_with(['"', '\'']) && token.ends_with(&token[..1]) {
        token = token[1..token.len() - 1].trim();
    }
    let token = token.replace("\\ ", " ");
    if token.is_empty() {
        return None;
    }
    let path = if token.starts_with("file://") {
        url::Url::parse(&token)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(&token))
    } else if let Some(rest) = token
        .strip_prefix('~')
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
    {
        home()?.join(rest.trim_start_matches(['/', '\\']))
    } else {
        PathBuf::from(&token)
    };
    let path = cwd.join(path);
    std::fs::metadata(&path)
        .is_ok_and(|m| m.is_file())
        .then_some(path)
}

/// The user's home folder, as `~` names it.
fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(std::env::var_os)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}
