//! `image.attach` with no home folder or an empty `HOME`: with neither `HOME` nor `USERPROFILE`,
//! `~` stays as written (`./~/shot.png`); with `HOME=""` and a valid `USERPROFILE`, `~` falls
//! back to `USERPROFILE`. One test on a current-thread runtime, since it mutates process-wide
//! environment variables.

#[path = "support/images.rs"]
mod support;

use serde_json::json;
use support::{connect, png, start};

fn attach_tilde(path: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let started = start().await;
        let mut client = connect(started.addr).await;
        let session = client.create().await;
        let reply = client
            .call(
                2,
                "image.attach",
                json!({"session_id": session, "path": path}),
            )
            .await;
        assert_eq!(reply["result"]["attached"], true, "{reply}");
        assert_eq!(reply["result"]["count"], 1, "{reply}");
    });
}

/// With no `HOME` or `USERPROFILE`, `~/shot.png` is read from `./~/shot.png`; with `HOME=""`,
/// `~` falls back to `USERPROFILE`.
#[test]
fn with_no_home_a_tilde_path_stays_as_written() {
    let back = std::env::current_dir().unwrap();
    std::env::remove_var("HOME");
    std::env::remove_var("USERPROFILE");
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir(cwd.path().join("~")).unwrap();
    std::fs::write(cwd.path().join("~").join("shot.png"), png("no home")).unwrap();
    std::env::set_current_dir(cwd.path()).unwrap();

    attach_tilde("~/shot.png");

    let profile = tempfile::tempdir().unwrap();
    std::fs::write(profile.path().join("from_profile.png"), png("profile")).unwrap();
    std::env::set_var("HOME", "");
    std::env::set_var("USERPROFILE", profile.path());

    attach_tilde("~/from_profile.png");

    // Leave the folder before it is removed: Windows keeps a working folder.
    std::env::set_current_dir(back).unwrap();
}
