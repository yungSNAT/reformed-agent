//! `image.attach` (an image named by a path on the gateway's host, as the terminal UI and a
//! local desktop send it) and `image.detach` (a removed attachment chip). The model is a
//! scripted OpenAI-compatible endpoint that keeps each request body.

#[path = "support/images.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::SinkExt;
use serde_json::json;
use support::*;
use tokio_tungstenite::tungstenite::Message;

/// Each refusal names upstream's code, and none stages a file.
#[tokio::test]
async fn image_attach_refuses_bad_requests() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let notes = folder.path().join("notes.txt");
    std::fs::write(&notes, "plain text").unwrap();
    let fake = folder.path().join("fake.png");
    std::fs::write(&fake, "not an image").unwrap();
    let big = folder.path().join("big.png");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(25 * 1024 * 1024 + 1).unwrap();
    let missing = folder.path().join("missing.png");
    let text = |p: &Path| p.display().to_string();

    let cases = [
        (json!({"path": text(&notes)}), 4000),
        (json!({"session_id": 7, "path": text(&notes)}), 4000),
        (json!({"session_id": "nope", "path": text(&notes)}), 4001),
        (json!({"session_id": session}), 4015),
        (json!({"session_id": session, "path": "   "}), 4015),
        (json!({"session_id": session, "path": 3}), 4000),
        (json!({"session_id": session, "path": text(&missing)}), 4016),
        (
            json!({"session_id": session, "path": text(folder.path())}),
            4016,
        ),
        (json!({"session_id": session, "path": text(&notes)}), 4016),
        (json!({"session_id": session, "path": text(&fake)}), 4016),
        (json!({"session_id": session, "path": text(&big)}), 4018),
    ];
    for (n, (params, code)) in cases.iter().enumerate() {
        let reply = client
            .call(10 + n as u64, "image.attach", params.clone())
            .await;
        assert_eq!(reply["error"]["code"], *code, "{params}: {reply}");
    }
    let reply = client
        .call(
            30,
            "image.attach",
            json!({"session_id": session, "path": text(&missing)}),
        )
        .await;
    assert_eq!(
        reply["error"]["message"],
        format!("image not found: {}", missing.display()),
        "{reply}"
    );
    let reply = client
        .call(
            31,
            "image.attach",
            json!({"session_id": session, "path": text(&notes)}),
        )
        .await;
    assert_eq!(
        reply["error"]["message"], "unsupported image: notes.txt",
        "{reply}"
    );
    assert!(staged_images(started.home.path()).is_empty());
}

/// A path the client names is copied under the gateway's home, and the next prompt sends the
/// model the copy, as `image.attach_bytes` does with bytes.
#[tokio::test]
async fn an_image_named_by_path_goes_with_the_next_prompt() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("shot one.png");
    let image = png("by-path");
    std::fs::write(&source, &image).unwrap();

    // Quoted, as a terminal drop of a path with a space gives it, with words after it.
    let raw = format!("\"{}\" what is this", source.display());
    let reply = client
        .call(
            2,
            "image.attach",
            json!({"session_id": session, "path": raw}),
        )
        .await;
    let result = &reply["result"];
    assert_eq!(result["attached"], true, "{reply}");
    assert_eq!(result["name"], "shot one.png");
    assert_eq!(result["remainder"], "what is this");
    assert_eq!(result["text"], "what is this");
    assert_eq!(result["count"], 1);
    assert_eq!(result["bytes"], image.len());
    let staged = PathBuf::from(result["path"].as_str().unwrap());
    assert_eq!(staged_images(started.home.path()), vec![staged.clone()]);
    assert_eq!(std::fs::read(&staged).unwrap(), image);

    // The copy is what goes: a later change to the source does not reach the model.
    std::fs::write(&source, png("changed")).unwrap();
    client.turn(3, &session, "describe").await;
    assert_eq!(sent_images(&started.requests, 0), vec![data_url(&image)]);
}

/// The ways a terminal drop spells a path: escaped spaces, a `file://` URL, a path relative to
/// the working folder, and an unquoted path with spaces followed by words.
#[tokio::test]
async fn dropped_path_spellings_resolve() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("my shot.png");
    std::fs::write(&source, png("spelling")).unwrap();
    let escaped = source.display().to_string().replace(' ', "\\ ");
    let url = url::Url::from_file_path(&source).unwrap().to_string();
    let mixed_url = url.replacen("file://", "File://", 1);
    let spellings = [
        (format!("{escaped} and words"), "and words"),
        (url, ""),
        (mixed_url, ""),
        (format!("{} tell me", source.display()), "tell me"),
    ];
    for (n, (raw, remainder)) in spellings.iter().enumerate() {
        let reply = client
            .call(
                10 + n as u64,
                "image.attach",
                json!({"session_id": session, "path": raw}),
            )
            .await;
        assert_eq!(reply["result"]["name"], "my shot.png", "{raw}: {reply}");
        assert_eq!(reply["result"]["remainder"], *remainder, "{raw}");
    }
    let reply = client
        .call(
            20,
            "image.attach",
            json!({"session_id": session, "path": "my shot.png"}),
        )
        .await;
    assert_eq!(
        reply["error"]["code"], 4016,
        "relative to the gateway's folder: {reply}"
    );
    // With no words after the path, the text is upstream's attachment line.
    let reply = client
        .call(
            21,
            "image.attach",
            json!({"session_id": session, "path": source.display().to_string()}),
        )
        .await;
    assert_eq!(
        reply["result"]["text"],
        "[User attached image: my shot.png]"
    );
    assert_eq!(reply["result"]["count"], 5);
}

/// A detached image is not sent and its staged copy is removed; the others still go.
#[tokio::test]
async fn a_detached_image_is_not_sent() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let (kept, dropped) = (png("kept"), png("dropped"));
    let attach = |bytes: &[u8]| json!({"session_id": session, "content_base64": b64(bytes)});
    let reply = client.call(2, "image.attach_bytes", attach(&kept)).await;
    assert_eq!(reply["result"]["count"], 1, "{reply}");
    let reply = client.call(3, "image.attach_bytes", attach(&dropped)).await;
    let dropped_path = reply["result"]["path"].as_str().unwrap().to_string();

    let detach = json!({"session_id": session, "path": dropped_path});
    let reply = client.call(4, "image.detach", detach.clone()).await;
    assert_eq!(
        reply["result"],
        json!({"detached": true, "count": 1}),
        "{reply}"
    );
    assert!(!Path::new(&dropped_path).exists());
    let reply = client.call(5, "image.detach", detach).await;
    assert_eq!(
        reply["result"],
        json!({"detached": false, "count": 1}),
        "{reply}"
    );

    client.turn(6, &session, "describe").await;
    assert_eq!(sent_images(&started.requests, 0), vec![data_url(&kept)]);
}

#[tokio::test]
async fn image_detach_refuses_bad_requests() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let cases = [
        (json!({"path": "/x.png"}), 4000),
        (json!({"session_id": "nope", "path": "/x.png"}), 4001),
        (json!({"session_id": session}), 4015),
        (json!({"session_id": session, "path": ""}), 4015),
        (json!({"session_id": session, "path": false}), 4000),
    ];
    for (n, (params, code)) in cases.iter().enumerate() {
        let reply = client
            .call(10 + n as u64, "image.detach", params.clone())
            .await;
        assert_eq!(reply["error"]["code"], *code, "{params}: {reply}");
    }
}

/// A PNG signature followed by zeros, `len` bytes long, made without writing them.
fn sparse_png(path: &Path, len: u64) {
    std::fs::write(path, png("")).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(len)
        .unwrap();
}

/// An empty image file and a session at its staging limits are refused, and nothing more is
/// staged: 16 images, or 64 MiB, whichever comes first.
#[tokio::test]
async fn image_attach_keeps_each_session_within_its_limits() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let attach = |p: &Path| json!({"session_id": session, "path": p.display().to_string()});

    let empty = folder.path().join("empty.png");
    std::fs::write(&empty, b"").unwrap();
    let reply = client.call(2, "image.attach", attach(&empty)).await;
    assert_eq!(reply["error"]["code"], 4017, "{reply}");

    // 24 MiB each: the third would pass 64 MiB.
    let big = folder.path().join("big.png");
    sparse_png(&big, 24 << 20);
    for n in 0..2 {
        let reply = client.call(3 + n, "image.attach", attach(&big)).await;
        assert_eq!(reply["result"]["count"], n + 1, "{reply}");
    }
    let reply = client.call(5, "image.attach", attach(&big)).await;
    assert_eq!(reply["error"]["code"], 4018, "{reply}");
    assert_eq!(staged_images(started.home.path()).len(), 2);

    let session = client.create().await;
    let small = folder.path().join("small.png");
    std::fs::write(&small, png("small")).unwrap();
    let attach = json!({"session_id": session, "path": small.display().to_string()});
    for n in 0..16 {
        let reply = client.call(10 + n, "image.attach", attach.clone()).await;
        assert_eq!(reply["result"]["count"], n + 1, "{reply}");
    }
    let reply = client.call(30, "image.attach", attach).await;
    assert_eq!(reply["error"]["code"], 4018, "{reply}");
    assert_eq!(staged_images(started.home.path()).len(), 18);
}

/// Sends each of `paths` to `image.attach` and expects the refusal for a path that is never
/// read, at once.
async fn expect_refused(client: &mut Client, session: &str, paths: &[String]) {
    for (n, path) in paths.iter().enumerate() {
        let params = json!({"session_id": session, "path": path});
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            client.call(10 + n as u64, "image.attach", params),
        )
        .await
        .expect("refused at once");
        assert_eq!(reply["error"]["code"], 4016, "{path}: {reply}");
        let message = reply["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.starts_with("image path refused:"),
            "{path}: {reply}"
        );
    }
}

/// A network share or device namespace path, with 2 leading separators in any mix or the
/// `\??\` prefix, is refused without being opened. The strings hold real doubled backslashes.
#[tokio::test]
async fn image_attach_refuses_share_and_device_paths() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let paths = [
        r"\\nohost.invalid\share\a.png",
        r"\\?\C:\a.png",
        r"\\.\pipe\a.png",
        r"\??\UNC\nohost.invalid\share\a.png",
        r"/\nohost.invalid\share\a.png",
        r"\/nohost.invalid/share/a.png",
        "//nohost.invalid/share/a.png",
        r#""\\nohost.invalid\share\a b.png" and words"#,
        "file://nohost.invalid/share/a.png",
        "FILE://nohost.invalid/share/a.png",
        "File://nohost.invalid/share/a.png",
    ];
    assert!(paths[0].as_bytes().starts_with(br"\\"));
    let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
    expect_refused(&mut client, &session, &paths).await;
}

/// A Windows reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM0` to
/// `COM9`, `LPT0` to `LPT9`, and `COM` or `LPT` with a superscript 1, 2 or 3), in any case and
/// with or without an extension, is refused before it is opened, on every platform, even where
/// a file of that name exists.
#[tokio::test]
async fn image_attach_refuses_reserved_device_names() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for name in [
        "NUL.png",
        "con.png",
        "Aux.jpg",
        "prn.gif",
        "COM1.png",
        "lpt9.webp",
        "nul",
        "CONIN$.png",
        "conout$.png",
        "COM0.png",
        "lpt0.png",
        "COM\u{b9}.png",
        "com\u{b2}.png",
        "COM\u{b3}",
        "LPT\u{b9}.png",
        "lpt\u{b2}.png",
        "LPT\u{b3}.jpg",
    ] {
        let path = folder.path().join(name);
        #[cfg(not(windows))]
        std::fs::write(&path, png(name)).unwrap();
        paths.push(path.display().to_string());
    }
    paths.push("NUL.png".to_string());
    // Intermediate reserved device components are refused too, even where the folder exists.
    for dir_name in ["CON", "NUL", "COM1", "COM1.txt"] {
        let nested = folder.path().join(dir_name).join("shot.png");
        #[cfg(not(windows))]
        {
            std::fs::create_dir(folder.path().join(dir_name)).unwrap();
            std::fs::write(&nested, png(dir_name)).unwrap();
        }
        paths.push(nested.display().to_string());
    }
    paths.push(r"C:\temp\NUL\shot.png".to_string());
    expect_refused(&mut client, &session, &paths).await;

    // A name that only starts like one is served.
    for (n, name) in ["console.png", "com10.png", "lpt.png"].iter().enumerate() {
        let path = folder.path().join(name);
        std::fs::write(&path, png(name)).unwrap();
        let params = json!({"session_id": session, "path": path.display().to_string()});
        let reply = client.call(40 + n as u64, "image.attach", params).await;
        assert_eq!(reply["result"]["attached"], true, "{name}: {reply}");
    }
}

/// The read is bounded by what it reads, not by the size the file reports: the kernel's page
/// map reports 0 bytes and holds far more. The kernel refuses a page-map read that is not a
/// whole number of 8-byte entries, so the bounded read's last, odd-sized read fails (5027)
/// rather than reaching the size limit (4018); either way nothing is staged, where an
/// unbounded read takes gigabytes.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn image_attach_bounds_the_read_itself() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let link = folder.path().join("map.png");
    std::os::unix::fs::symlink("/proc/self/pagemap", &link).unwrap();
    let params = json!({"session_id": session, "path": link.display().to_string()});
    let reply = client.call(2, "image.attach", params).await;
    let code = reply["error"]["code"].as_i64();
    assert!(matches!(code, Some(4018 | 5027)), "{reply}");
    assert!(staged_images(started.home.path()).is_empty());
}

/// A local symlink pointing to a network share or reserved device component is refused before
/// following it to its target.
#[tokio::test]
async fn image_attach_refuses_links_to_refused_paths() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    #[cfg(unix)]
    {
        let unc_bs = folder.path().join("unc_bs.png");
        std::os::unix::fs::symlink(r"\\nohost.invalid\share\a.png", &unc_bs).unwrap();
        let unc_fs = folder.path().join("unc_fs.png");
        std::os::unix::fs::symlink("//nohost.invalid/share/a.png", &unc_fs).unwrap();
        let unc_dir = folder.path().join("unc_dir");
        std::os::unix::fs::symlink("//nohost.invalid/share", &unc_dir).unwrap();
        let reserved = folder.path().join("reserved.png");
        std::os::unix::fs::symlink(folder.path().join("NUL").join("shot.png"), &reserved).unwrap();
        paths.push(unc_bs.display().to_string());
        paths.push(unc_fs.display().to_string());
        paths.push(unc_dir.join("a.png").display().to_string());
        paths.push(reserved.display().to_string());
    }
    #[cfg(windows)]
    {
        let unc_file = folder.path().join("unc_file.png");
        std::os::windows::fs::symlink_file(r"\\nohost.invalid\share\a.png", &unc_file)
            .expect("a symlink on Windows needs Developer Mode or an elevated shell");
        let unc_dir = folder.path().join("unc_dir");
        std::os::windows::fs::symlink_dir(r"\\nohost.invalid\share", &unc_dir)
            .expect("a symlink on Windows needs Developer Mode or an elevated shell");
        paths.push(unc_file.display().to_string());
        paths.push(unc_dir.join("a.png").display().to_string());
    }
    expect_refused(&mut client, &session, &paths).await;
    assert!(staged_images(started.home.path()).is_empty());
}

/// A FIFO is refused at once instead of waiting for a writer.
#[cfg(unix)]
#[tokio::test]
async fn image_attach_refuses_a_fifo() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let fifo = folder.path().join("pipe.png");
    let name = std::ffi::CString::new(fifo.display().to_string()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let params = json!({"session_id": session, "path": fifo.display().to_string()});
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        client.call(2, "image.attach", params),
    )
    .await
    .expect("refused at once");
    assert_eq!(reply["error"]["code"], 4016, "{reply}");
}

/// A symlink to a FIFO is refused at once without blocking or staging anything.
#[cfg(unix)]
#[tokio::test]
async fn image_attach_refuses_a_symlink_to_a_fifo() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let fifo = folder.path().join("pipe");
    let name = std::ffi::CString::new(fifo.display().to_string()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let link = folder.path().join("link.png");
    std::os::unix::fs::symlink(&fifo, &link).unwrap();
    let params = json!({"session_id": session, "path": link.display().to_string()});
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        client.call(2, "image.attach", params),
    )
    .await
    .expect("refused at once");
    assert_eq!(reply["error"]["code"], 4016, "{reply}");
    assert!(staged_images(started.home.path()).is_empty());
}

/// Detaching an already-missing staged file succeeds; a removal failure (for example a file
/// replaced by a folder) returns 5027 and keeps the entry so a retry cleans it up.
#[tokio::test]
async fn image_detach_reports_removal_failure_and_keeps_the_entry() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let attach = |tag| json!({"session_id": session, "content_base64": b64(&png(tag))});
    let gone_path = client.call(2, "image.attach_bytes", attach("gone")).await["result"]["path"]
        .as_str()
        .unwrap()
        .to_string();
    let blocked_path = client.call(3, "image.attach_bytes", attach("blocked")).await["result"]
        ["path"]
        .as_str()
        .unwrap()
        .to_string();

    // Already removed on disk (`NotFound`) still detaches cleanly.
    std::fs::remove_file(&gone_path).unwrap();
    let reply = client
        .call(
            4,
            "image.detach",
            json!({"session_id": session, "path": gone_path}),
        )
        .await;
    assert_eq!(
        reply["result"],
        json!({"detached": true, "count": 1}),
        "{reply}"
    );

    // Replacing the staged file with a folder makes `remove_file` fail; 5027 and retained.
    std::fs::remove_file(&blocked_path).unwrap();
    std::fs::create_dir(&blocked_path).unwrap();
    let detach = json!({"session_id": session, "path": blocked_path});
    let reply = client.call(5, "image.detach", detach.clone()).await;
    assert_eq!(reply["error"]["code"], 5027, "{reply}");

    // Once the obstruction is gone, retrying detaches the retained entry.
    std::fs::remove_dir(&blocked_path).unwrap();
    let reply = client.call(6, "image.detach", detach).await;
    assert_eq!(
        reply["result"],
        json!({"detached": true, "count": 0}),
        "{reply}"
    );
}

/// A session cannot take back another session's staged image.
#[tokio::test]
async fn image_detach_reaches_only_its_own_session() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let first = client.create().await;
    let second = client.create().await;
    let attach = json!({"session_id": first, "content_base64": b64(&png("mine"))});
    let reply = client.call(2, "image.attach_bytes", attach).await;
    let path = reply["result"]["path"].as_str().unwrap().to_string();

    let detach = json!({"session_id": second, "path": path});
    let reply = client.call(3, "image.detach", detach).await;
    assert_eq!(
        reply["result"],
        json!({"detached": false, "count": 0}),
        "{reply}"
    );
    assert!(Path::new(&path).exists());
}

/// An image attached by path just before a pipelined prompt goes with that prompt.
#[tokio::test]
async fn a_pipelined_prompt_takes_the_image_attached_before_it() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let shot = folder.path().join("shot.png");
    std::fs::write(&shot, png("first")).unwrap();
    let frames = [
        json!({"jsonrpc": "2.0", "id": 2, "method": "image.attach",
               "params": {"session_id": session, "path": shot.display().to_string()}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "prompt.submit",
               "params": {"session_id": session, "text": "look"}}),
    ];
    for frame in frames {
        client
            .socket
            .send(Message::text(frame.to_string()))
            .await
            .unwrap();
    }
    loop {
        let frame = client.next_json().await;
        assert!(frame["error"].is_null(), "{frame}");
        if frame["params"]["type"] == "message.complete" {
            break;
        }
    }
    assert_eq!(sent_images(&started.requests, 0), [data_url(&png("first"))]);
}
