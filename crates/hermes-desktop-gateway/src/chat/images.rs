//! `image.attach` and `image.detach`: an image the client names by a path on the gateway's
//! host, as the terminal UI and a desktop on the same machine send it, staged for the session's
//! next prompt; and a staged image taken back when the client removes it.
//!
//! `image.attach` reads the path the client names, as upstream's does: the gateway runs on the
//! client's machine and answers only its token. The file is copied under `<home>/images` and
//! checked as `image.attach_bytes` checks bytes (signature and size), so the prompt sends what
//! was attached even if the file changes afterwards.

use std::io::Read;
use std::path::{Path, PathBuf};

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
    let (source, remainder) = match image_path(raw, &cwd) {
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
    let data = read_image(&source)
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
/// too large. An error when the handle is not a regular file (one swapped in after [`resolve`]
/// checked the path); a FIFO is opened without waiting for a writer.
fn read_image(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
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

/// A path refused before any file system call: a network share or device namespace path, or
/// a Windows reserved device name. Holds the client's token.
struct Refused(String);

/// The image file `raw` names and the words after it, as upstream's `image.attach` reads a
/// terminal drop: a dropped path when `raw` starts like one, else its first token. `None` when
/// it names no file.
fn image_path(raw: &str, cwd: &Path) -> Result<Option<(PathBuf, String)>, Refused> {
    if let Some(found) = dropped(raw, cwd)? {
        return Ok(Some(found));
    }
    let (token, remainder) = split_path_input(raw);
    Ok(resolve(&token, cwd)?.map(|path| (path, remainder)))
}

/// Upstream's `_detect_file_drop`: a path at the start of `raw` (optionally quoted; `/`, `~`,
/// `./`, `../`, a drive prefix, or unquoted `file://`), with the words after it.
fn dropped(raw: &str, cwd: &Path) -> Result<Option<(PathBuf, String)>, Refused> {
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
        return Ok(None);
    }
    if let Some(path) = resolve(text, cwd)? {
        return Ok(Some((path, String::new())));
    }
    let (token, remainder) = split_path_input(text);
    if let Some(path) = resolve(&token, cwd)? {
        return Ok(Some((path, remainder)));
    }
    if quoted {
        return Ok(None);
    }
    // An unquoted path with spaces: the longest prefix ending before a space that names a file.
    for (pos, _) in text.match_indices(' ').rev() {
        if let Some(path) = resolve(text[..pos].trim_end(), cwd)? {
            return Ok(Some((path, text[pos + 1..].trim().to_string())));
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

/// Upstream's `_resolve_attachment_path`: quotes, `\ ` escapes, `file://` and a leading `~`
/// are undone, a relative path is taken from `cwd`, and only an existing file is returned.
/// Before anything touches it, [`Refused`]: 2 leading separators in any mix (`\\host\share`,
/// `\\?\`, `\\.\`, `//host`, `/\host`, `\/host`), the `\??\` prefix, a `file://` URL naming a
/// host, or a reserved device name ([`reserved_name`]).
fn resolve(raw: &str, cwd: &Path) -> Result<Option<PathBuf>, Refused> {
    let mut token = raw.trim();
    if token.len() >= 2 && token.starts_with(['"', '\'']) && token.ends_with(&token[..1]) {
        token = token[1..token.len() - 1].trim();
    }
    let token = token.replace("\\ ", " ");
    if token.is_empty() {
        return Ok(None);
    }
    let refused = || Refused(raw.trim().to_string());
    let path = if token.starts_with("file://") {
        match url::Url::parse(&token) {
            Ok(url)
                if url
                    .host_str()
                    .is_some_and(|h| !h.is_empty() && h != "localhost") =>
            {
                return Err(refused());
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
    let shown = path.as_os_str().to_string_lossy();
    let mut lead = shown.chars();
    let separator = |c: Option<char>| matches!(c, Some('/' | '\\'));
    if (separator(lead.next()) && separator(lead.next())) || shown.starts_with(r"\??\") {
        return Err(refused());
    }
    if path
        .file_name()
        .is_some_and(|name| reserved_name(&name.to_string_lossy()))
    {
        return Err(refused());
    }
    let path = cwd.join(path);
    Ok(std::fs::metadata(&path)
        .is_ok_and(|m| m.is_file())
        .then_some(path))
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

/// The user's home folder, as `~` names it.
fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(std::env::var_os)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A browser session of the Webapp, and a connection whose listener set no origin, are
    /// refused before anything is read; only a client on this machine is served.
    #[test]
    fn only_a_local_client_attaches_by_path() {
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let unset = super::super::Conn::new(1, outbox, false).origin;
        for origin in [Origin::Webapp, unset] {
            assert_eq!(
                local_only(origin).unwrap_err().0,
                IMAGE_UNSUPPORTED,
                "{origin:?}"
            );
        }
        assert!(local_only(Origin::Local).is_ok());
    }

    /// An unclosed quote takes the rest as the path; a quoted path that names no file is not
    /// walked back to a shorter one, as an unquoted one is; a quoted name resolves, and an empty
    /// or unparsable `file://` token names nothing (and is not refused).
    #[test]
    fn path_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        std::fs::write(cwd.join("a"), b"x").unwrap();
        std::fs::write(cwd.join("shot.png"), b"x").unwrap();

        assert_eq!(split_path_input("'a b c"), ("a b c".into(), String::new()));
        assert!(matches!(dropped("'./a b' c", cwd), Ok(None)));
        assert_eq!(
            dropped("./a b c", cwd).ok().flatten(),
            Some((cwd.join("./a"), "b c".into()))
        );
        assert_eq!(
            resolve("'shot.png'", cwd).ok().flatten(),
            Some(cwd.join("shot.png"))
        );
        for nothing in ["''", "file://[", "shot.png/.", "."] {
            assert!(matches!(resolve(nothing, cwd), Ok(None)), "{nothing}");
        }
        std::fs::create_dir(cwd.join("sub")).unwrap();
        assert_eq!(
            resolve("./sub/../shot.png", cwd).ok().flatten(),
            Some(cwd.join("./sub/../shot.png"))
        );
        #[cfg(unix)]
        {
            std::fs::write(cwd.join("sub/real.png"), b"x").unwrap();
            std::os::unix::fs::symlink("sub/real.png", cwd.join("rel.png")).unwrap();
            assert_eq!(
                resolve("rel.png", cwd).ok().flatten(),
                Some(cwd.join("rel.png"))
            );
            std::os::unix::fs::symlink(cwd.join("loop_b.png"), cwd.join("loop_a.png")).unwrap();
            std::os::unix::fs::symlink(cwd.join("loop_a.png"), cwd.join("loop_b.png")).unwrap();
            assert!(matches!(resolve("loop_a.png", cwd), Ok(None)));
        }
    }

    /// `~/` names the home folder.
    #[test]
    fn a_tilde_path_is_taken_from_home() {
        let home = home().expect("a home folder");
        let file = tempfile::Builder::new()
            .suffix(".png")
            .tempfile_in(&home)
            .unwrap();
        let name = file.path().file_name().unwrap().to_string_lossy();
        let cwd = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve(&format!("~/{name}"), cwd.path()).ok().flatten(),
            Some(home.join(&*name))
        );
    }

    /// A drive-prefixed drop is a path; on Unix `C:` is an ordinary folder name.
    #[cfg(unix)]
    #[test]
    fn a_drive_prefix_starts_a_dropped_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("C:")).unwrap();
        std::fs::write(dir.path().join("C:/d.png"), b"x").unwrap();
        assert_eq!(
            dropped("C:/d.png later", dir.path()).ok().flatten(),
            Some((dir.path().join("C:/d.png"), "later".into()))
        );
    }

    /// A handle that is not a regular file is not read.
    #[test]
    fn a_folder_is_not_read_as_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let refused = read_image(dir.path());
        // Unix opens a folder, and the handle is checked; Windows refuses to open it as a file.
        #[cfg(unix)]
        assert_eq!(refused.unwrap_err().to_string(), "not a regular file");
        #[cfg(not(unix))]
        assert!(refused.is_err());
    }
}

#[cfg(test)]
mod answers_at_once {
    use std::sync::Arc;

    use futures::FutureExt as _;
    use serde_json::json;

    use super::super::{Chat, Conn, Origin, Request};

    /// An `image.attach` request from a local client for a PNG at `<home>/shot.png`.
    fn local_attach(home: &std::path::Path) -> Request {
        std::fs::write(home.join("config.yaml"), "model:\n  default: mock-model\n").unwrap();
        let config = hermes_config::load_config(home.to_str()).unwrap();
        let chat = Arc::new(Chat::new(config));
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut conn = Conn::new(1, outbox, false);
        conn.origin = Origin::Local;
        let id = chat.create(&conn)["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        let image = home.join("shot.png");
        std::fs::write(
            &image,
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, b'x'],
        )
        .unwrap();
        let params = json!({"session_id": id, "path": image.display().to_string()})
            .as_object()
            .unwrap()
            .clone();
        Request { chat, conn, params }
    }

    /// `image.attach` stages the image before it returns, so a `prompt.submit` the connection
    /// reads next takes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn image_attach_answers_in_its_first_poll() {
        let home = tempfile::tempdir().unwrap();
        let reply = super::attach(local_attach(home.path()))
            .now_or_never()
            .expect("answered at once");
        assert_eq!(reply.unwrap()["count"], 1);
    }

    /// `image.attach` refuses non-local origins before reading or staging anything.
    #[tokio::test(flavor = "multi_thread")]
    async fn attach_refuses_non_local_origins() {
        for origin in [Origin::Webapp, Origin::Remote] {
            let home = tempfile::tempdir().unwrap();
            let mut request = local_attach(home.path());
            request.conn.origin = origin;
            let chat = Arc::clone(&request.chat);
            let refused = super::attach(request)
                .now_or_never()
                .expect("answered at once")
                .unwrap_err();
            assert_eq!(refused.0, super::super::IMAGE_UNSUPPORTED, "{origin:?}");
            assert!(chat
                .lock_sessions()
                .values()
                .all(|session| session.images().is_empty()));
        }
    }

    /// A Webapp client can detach an image staged on its session (for example from
    /// `image.attach_bytes`), and cannot touch any unstaged host file.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_webapp_client_detaches_staged_images_only() {
        let home = tempfile::tempdir().unwrap();
        let request = local_attach(home.path());
        let chat = Arc::clone(&request.chat);
        let id = request.params["session_id"].as_str().unwrap().to_string();
        let host_file = home.path().join("shot.png");
        let staged_reply = super::attach(request)
            .now_or_never()
            .expect("answered at once")
            .unwrap();
        let staged_path = staged_reply["path"].as_str().unwrap().to_string();

        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut conn = Conn::new(2, outbox, false);
        conn.origin = Origin::Webapp;

        // An unstaged host path is untouched.
        let params = json!({"session_id": id, "path": host_file.display().to_string()})
            .as_object()
            .unwrap()
            .clone();
        let reply = super::detach(Request {
            chat: Arc::clone(&chat),
            conn: conn.clone(),
            params,
        })
        .now_or_never()
        .expect("answered at once")
        .unwrap();
        assert_eq!(reply, json!({"detached": false, "count": 1}));
        assert!(host_file.exists());

        // The session's staged copy is removed.
        let params = json!({"session_id": id, "path": staged_path})
            .as_object()
            .unwrap()
            .clone();
        let reply = super::detach(Request { chat, conn, params })
            .now_or_never()
            .expect("answered at once")
            .unwrap();
        assert_eq!(reply, json!({"detached": true, "count": 0}));
        assert!(!std::path::Path::new(&staged_path).exists());
    }

    /// A staging folder that cannot be made is a write failure (5027), and nothing is staged.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unwritable_staging_folder_is_5027() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("images"), b"not a folder").unwrap();
        let request = local_attach(home.path());
        let chat = Arc::clone(&request.chat);
        let refused = super::attach(request)
            .now_or_never()
            .expect("answered at once")
            .unwrap_err();
        assert_eq!(refused.0, super::super::IMAGE_WRITE_FAILED, "{}", refused.1);
        assert!(chat
            .lock_sessions()
            .values()
            .all(|session| session.images().is_empty()));
    }
}
