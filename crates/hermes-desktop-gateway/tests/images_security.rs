//! Security boundary tests for `image.attach`: network shares, device namespaces, reserved
//! device names, symlink target validation, bounded reads and FIFOs.

#[path = "support/images.rs"]
mod support;

use std::time::Duration;

use serde_json::json;
use support::*;

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

const RESERVED_NAMES: [&str; 17] = [
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
];

/// A Windows reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM0` to
/// `COM9`, `LPT0` to `LPT9`, and `COM` or `LPT` with a superscript 1, 2 or 3), in any case and
/// with or without an extension, is refused before it is opened, on every platform, even where
/// a file or intermediate folder of that name exists.
#[tokio::test]
async fn image_attach_refuses_reserved_device_names() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for (i, name) in RESERVED_NAMES.iter().enumerate() {
        let sub = folder.path().join(format!("f{i}"));
        let path = sub.join(name);
        #[cfg(not(windows))]
        {
            std::fs::create_dir(&sub).unwrap();
            std::fs::write(&path, png(name)).unwrap();
        }
        paths.push(path.display().to_string());
    }
    paths.push("NUL.png".to_string());
    // Intermediate reserved device components are refused too, even where the folder exists.
    for (i, dir_name) in ["CON", "NUL", "COM1", "COM1.txt"].iter().enumerate() {
        let dir = folder.path().join(format!("d{i}")).join(dir_name);
        let nested = dir.join("shot.png");
        #[cfg(not(windows))]
        {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&nested, png(dir_name)).unwrap();
        }
        paths.push(nested.display().to_string());
    }
    paths.push(r"C:\temp\NUL\shot.png".to_string());
    expect_refused(&mut client, &session, &paths).await;
}

/// A name that only starts like a Windows reserved device name is served.
#[tokio::test]
async fn image_attach_allows_names_with_reserved_prefixes() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    for (n, name) in ["console.png", "com10.png", "lpt.png"].iter().enumerate() {
        let path = folder.path().join(name);
        std::fs::write(&path, png(name)).unwrap();
        let params = json!({"session_id": session, "path": path.display().to_string()});
        let reply = client.call(40 + n as u64, "image.attach", params).await;
        assert_eq!(reply["result"]["attached"], true, "{name}: {reply}");
    }
}

/// Drive-relative paths (`C:NUL.png`, `C:..\shot.png`, bare `C:`) and unquoted missing paths with
/// spaces ending in a reserved component are refused before touching the file system.
#[tokio::test]
async fn image_attach_refuses_drive_relative_paths() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let paths = vec![
        "C:NUL.png".to_string(),
        "c:com1.png".to_string(),
        r"C:NUL\shot.png".to_string(),
        r"C:..\shot.png".to_string(),
        r"c:..\shot.png".to_string(),
        "C:".to_string(),
        format!(
            "{} dir/NUL/shot.png",
            folder.path().join("missing").display()
        ),
    ];
    expect_refused(&mut client, &session, &paths).await;
    assert!(staged_images(started.home.path()).is_empty());
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

#[cfg(unix)]
fn refused_link_paths(folder: &std::path::Path) -> Vec<String> {
    let unc_bs = folder.join("unc_bs.png");
    std::os::unix::fs::symlink(r"\\nohost.invalid\share\a.png", &unc_bs).unwrap();
    let unc_fs = folder.join("unc_fs.png");
    std::os::unix::fs::symlink("//nohost.invalid/share/a.png", &unc_fs).unwrap();
    let unc_dir = folder.join("unc_dir");
    std::os::unix::fs::symlink("//nohost.invalid/share", &unc_dir).unwrap();
    let reserved = folder.join("reserved.png");
    std::os::unix::fs::symlink(folder.join("NUL").join("shot.png"), &reserved).unwrap();
    let drive_rel = folder.join("drive_rel.png");
    std::os::unix::fs::symlink(r"C:..\shot.png", &drive_rel).unwrap();
    vec![
        unc_bs.display().to_string(),
        unc_fs.display().to_string(),
        unc_dir.join("a.png").display().to_string(),
        reserved.display().to_string(),
        drive_rel.display().to_string(),
    ]
}

#[cfg(windows)]
fn refused_link_paths(folder: &std::path::Path) -> Vec<String> {
    let unc_file = folder.join("unc_file.png");
    std::os::windows::fs::symlink_file(r"\\nohost.invalid\share\a.png", &unc_file)
        .expect("a symlink on Windows needs Developer Mode or an elevated shell");
    let unc_dir = folder.join("unc_dir");
    std::os::windows::fs::symlink_dir(r"\\nohost.invalid\share", &unc_dir)
        .expect("a symlink on Windows needs Developer Mode or an elevated shell");
    let real_dir = folder.join("real");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::write(real_dir.join("shot.png"), png("junction")).unwrap();
    let junc = folder.join("junc");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junc)
        .arg(&real_dir)
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");
    vec![
        unc_file.display().to_string(),
        unc_dir.join("a.png").display().to_string(),
        junc.join("shot.png").display().to_string(),
    ]
}

/// A local symlink or Windows directory junction pointing to a refused path is refused before
/// following it to its target.
#[tokio::test]
async fn image_attach_refuses_links_to_refused_paths() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let paths = refused_link_paths(folder.path());
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
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
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
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
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
