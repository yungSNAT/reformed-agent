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
use std::path::Path;

use serde_json::{json, Map, Value};

use super::session::{MAX_STAGED_BYTES, MAX_STAGED_IMAGES};
use super::{
    ready, Chat, Handler, Origin, Reply, Request, RpcError, ATTACHMENT_REQUIRED, IMAGE_INVALID,
    IMAGE_TOO_LARGE, IMAGE_UNSUPPORTED, IMAGE_WRITE_FAILED, INVALID_PARAMS, SESSION_NOT_FOUND,
};
use crate::attach::{stage_image_bytes, ImageError, MAX_IMAGE_BYTES};

#[path = "image_path.rs"]
mod path;

use path::{image_path, split_path_input, Found, Refused};

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

fn session_full() -> RpcError {
    (
        IMAGE_TOO_LARGE,
        format!("a session holds at most {MAX_STAGED_IMAGES} images ({MAX_STAGED_BYTES} bytes)"),
    )
}

fn resolve_for_attach(raw: &str) -> Result<(Found, String), RpcError> {
    let cwd = std::env::current_dir()
        .map_err(|e| (IMAGE_WRITE_FAILED, format!("no working folder: {e}")))?;
    match image_path(raw, &cwd) {
        Ok(Some(found)) => Ok(found),
        Ok(None) => {
            let (token, _) = split_path_input(raw);
            Err((IMAGE_UNSUPPORTED, format!("image not found: {token}")))
        }
        Err(Refused(token)) => Err((
            IMAGE_UNSUPPORTED,
            format!(
                "image path refused: network shares, device paths, drive-relative paths and \
                 reserved device names are not read: {token}"
            ),
        )),
    }
}

fn attach_reply(name: &str, path: &Path, bytes: usize, remainder: &str, count: usize) -> Value {
    json!({
        "attached": true,
        "name": name,
        "path": path.display().to_string(),
        "bytes": bytes,
        "remainder": remainder,
        "text": if remainder.is_empty() {
            format!("[User attached image: {name}]")
        } else {
            remainder.to_string()
        },
        "count": count,
    })
}

fn attach_now(chat: &Chat, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let (id, raw) = session_and_path(params)?;
    let Some(session) = chat.lock_sessions().get(id).cloned() else {
        return Err((SESSION_NOT_FOUND, "session not found".into()));
    };
    let (found, remainder) = resolve_for_attach(raw)?;
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
    if !session.has_room_for_image(0) {
        return Err(session_full());
    }
    let data = read_image(found.file)
        .map_err(|e| (IMAGE_WRITE_FAILED, format!("cannot read {name}: {e}")))?;
    let staged = stage_image_bytes(&chat.home.join("images"), &data).map_err(|e| match e {
        ImageError::Unsupported => unsupported(),
        ImageError::TooLarge => (
            IMAGE_TOO_LARGE,
            format!("image larger than {MAX_IMAGE_BYTES} bytes"),
        ),
        ImageError::Invalid(message) => (IMAGE_INVALID, message),
        ImageError::Write(message) => (IMAGE_WRITE_FAILED, message),
    })?;
    let (path, bytes) = (staged.path.clone(), staged.bytes);
    match session.stage_image(staged) {
        Ok(count) => Ok(attach_reply(&name, &path, bytes, &remainder, count)),
        Err(refused) => {
            let _ = std::fs::remove_file(&refused.path);
            Err(session_full())
        }
    }
}

/// The bytes of the already-opened regular file `file`, at most one byte past [`MAX_IMAGE_BYTES`]
/// (a file may report a smaller size than it reads), so staging refuses one too large.
fn read_image(file: std::fs::File) -> std::io::Result<Vec<u8>> {
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
    let mut queued = session.images();
    let Some(index) = queued.iter().position(|image| image.path == Path::new(raw)) else {
        return Ok(json!({"detached": false, "count": queued.len()}));
    };
    // Each staged image is this gateway's own copy under `<home>/images`. Hold `queued` across
    // unlink so a concurrent attach or prompt cannot observe the slot free whilst removal fails.
    match std::fs::remove_file(&queued[index].path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err((
            IMAGE_WRITE_FAILED,
            format!("cannot remove staged image: {e}"),
        )),
        Ok(()) | Err(_) => {
            queued.remove(index);
            Ok(json!({"detached": true, "count": queued.len()}))
        }
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
