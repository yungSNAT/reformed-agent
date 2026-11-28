use std::sync::Arc;

use futures::FutureExt as _;
use serde_json::json;

use super::super::{Chat, Conn, Origin, Request};
use super::*;

/// A browser session of the Webapp, and a connection whose listener set no origin, are
/// refused before anything is read; only a client on this machine is served.
#[test]
fn only_a_local_client_attaches_by_path() {
    let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
    let unset = Conn::new(1, outbox, false).origin;
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
/// walked back to a shorter one, as an unquoted one is; a quoted name resolves, and an empty,
/// directory-shaped or unparsable `file://` token names nothing (and is not refused).
#[test]
fn path_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    std::fs::write(cwd.join("a"), b"x").unwrap();
    std::fs::write(cwd.join("shot.png"), b"x").unwrap();
    std::fs::create_dir(cwd.join("sub")).unwrap();

    assert_eq!(split_path_input("'a b c"), ("a b c".into(), String::new()));
    assert!(matches!(dropped("'./a b' c", cwd), Ok(None)));
    assert_eq!(
        dropped("./a b c", cwd)
            .ok()
            .flatten()
            .map(|(f, rest)| (f.path, rest)),
        Some((cwd.join("./a"), "b c".into()))
    );
    assert_eq!(
        resolve("'shot.png'", cwd).ok().flatten().map(|f| f.path),
        Some(cwd.join("shot.png"))
    );
    for nothing in [
        "''",
        "file://[",
        "shot.png/child",
        "shot.png/.",
        "shot.png/",
        r"shot.png\",
        "sub",
    ] {
        assert!(matches!(resolve(nothing, cwd), Ok(None)), "{nothing}");
    }
    assert_eq!(
        resolve("./sub/../shot.png", cwd)
            .ok()
            .flatten()
            .map(|f| f.path),
        Some(cwd.join("./sub/../shot.png"))
    );
}

/// Relative symlinks resolve; symlink loops return `Ok(None)` without hanging.
#[cfg(unix)]
#[test]
fn relative_and_loop_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    std::fs::create_dir(cwd.join("sub")).unwrap();
    std::fs::write(cwd.join("sub/real.png"), b"x").unwrap();
    std::os::unix::fs::symlink("sub/real.png", cwd.join("rel.png")).unwrap();
    assert_eq!(
        resolve("rel.png", cwd).ok().flatten().map(|f| f.path),
        Some(cwd.join("rel.png"))
    );
    std::os::unix::fs::symlink(cwd.join("loop_b.png"), cwd.join("loop_a.png")).unwrap();
    std::os::unix::fs::symlink(cwd.join("loop_a.png"), cwd.join("loop_b.png")).unwrap();
    assert!(matches!(resolve("loop_a.png", cwd), Ok(None)));
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
        resolve(&format!("~/{name}"), cwd.path())
            .ok()
            .flatten()
            .map(|f| f.path),
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
        dropped("C:/d.png later", dir.path())
            .ok()
            .flatten()
            .map(|(f, rest)| (f.path, rest)),
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
    let reply = attach(local_attach(home.path()))
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
        let refused = attach(request)
            .now_or_never()
            .expect("answered at once")
            .unwrap_err();
        assert_eq!(refused.0, IMAGE_UNSUPPORTED, "{origin:?}");
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
    let staged_reply = attach(request)
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
    let reply = detach(Request {
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
    let reply = detach(Request { chat, conn, params })
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
    let refused = attach(request)
        .now_or_never()
        .expect("answered at once")
        .unwrap_err();
    assert_eq!(refused.0, IMAGE_WRITE_FAILED, "{}", refused.1);
    assert!(chat
        .lock_sessions()
        .values()
        .all(|session| session.images().is_empty()));
}

/// Replacing an already-validated ancestor directory with a symlink to a refused path after
/// resolution must not redirect the read to the forbidden target (S8).
#[cfg(unix)]
#[test]
fn ancestor_swap_after_resolve_does_not_read_redirected_target() {
    let dir = tempfile::tempdir().unwrap();
    let ok_dir = dir.path().join("ok");
    let nul_dir = dir.path().join("NUL");
    std::fs::create_dir(&ok_dir).unwrap();
    std::fs::create_dir(&nul_dir).unwrap();
    let safe_bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, b'O', b'K'];
    let bad_bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, b'N', b'O'];
    let shot = ok_dir.join("shot.png");
    std::fs::write(&shot, safe_bytes).unwrap();
    std::fs::write(nul_dir.join("shot.png"), bad_bytes).unwrap();

    let found = resolve(shot.to_str().unwrap(), dir.path())
        .unwrap()
        .unwrap();
    std::fs::rename(&ok_dir, dir.path().join("ok_old")).unwrap();
    std::os::unix::fs::symlink(&nul_dir, &ok_dir).unwrap();

    let bytes = read_image(&found.target).unwrap();
    assert_eq!(bytes, safe_bytes, "read followed swapped ancestor link");
}

/// A failing `image.detach` at session capacity must never free a slot for a concurrent attach or
/// reorder the session's image queue (S9).
#[tokio::test(flavor = "multi_thread")]
async fn failing_detach_at_capacity_keeps_queue_order_and_cap() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let home = tempfile::tempdir().unwrap();
    let request = local_attach(home.path());
    let chat = Arc::clone(&request.chat);
    let id = request.params["session_id"].as_str().unwrap().to_string();
    let session = chat.lock_sessions().get(&id).cloned().unwrap();
    let png_bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, b'x'];
    let images_dir = home.path().join("images");
    for _ in 0..MAX_STAGED_IMAGES {
        let staged = stage_image_bytes(&images_dir, &png_bytes).unwrap();
        session.stage_image(staged).unwrap();
    }
    let first_path = session.images()[0].path.clone();
    std::fs::remove_file(&first_path).unwrap();
    std::fs::create_dir(&first_path).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let racing_session = Arc::clone(&session);
    let racing_stop = Arc::clone(&stop);
    let raced_over_cap = std::thread::spawn(move || {
        let mut admitted = false;
        while !racing_stop.load(Ordering::Relaxed) {
            let probe = crate::attach::StagedImage {
                path: std::path::PathBuf::from("probe.png"),
                mime: "image/png",
                bytes: 1,
            };
            if racing_session.stage_image(probe).is_ok() {
                admitted = true;
                break;
            }
        }
        admitted
    });

    let params = json!({"session_id": id, "path": first_path.display().to_string()})
        .as_object()
        .unwrap()
        .clone();
    for _ in 0..80 {
        let err = detach_now(&chat, &params).unwrap_err();
        assert_eq!(err.0, IMAGE_WRITE_FAILED, "{}", err.1);
        let queued = session.images();
        assert_eq!(queued.len(), MAX_STAGED_IMAGES);
        assert_eq!(queued[0].path, first_path, "queue order changed");
    }
    stop.store(true, Ordering::Relaxed);
    assert!(!raced_over_cap.join().unwrap(), "concurrent attach took slot");
    std::fs::remove_dir(&first_path).unwrap();
}
