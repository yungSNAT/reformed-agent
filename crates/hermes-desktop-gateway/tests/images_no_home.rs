//! `image.attach` with no home folder: `~` stays as written, so `~/shot.png` is a folder named
//! `~` under the gateway's working folder. One test, since it clears process-wide variables.

#[path = "support/images.rs"]
mod support;

use serde_json::json;
use support::{connect, png, start};

/// With no `HOME` or `USERPROFILE`, `~/shot.png` is read from `./~/shot.png`.
#[test]
fn with_no_home_a_tilde_path_stays_as_written() {
    std::env::remove_var("HOME");
    std::env::remove_var("USERPROFILE");
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir(cwd.path().join("~")).unwrap();
    std::fs::write(cwd.path().join("~").join("shot.png"), png("no home")).unwrap();
    std::env::set_current_dir(cwd.path()).unwrap();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let started = start().await;
        let mut client = connect(started.addr).await;
        let session = client.create().await;
        let reply = client
            .call(
                2,
                "image.attach",
                json!({"session_id": session, "path": "~/shot.png"}),
            )
            .await;
        assert_eq!(reply["result"]["attached"], true, "{reply}");
        assert_eq!(reply["result"]["count"], 1, "{reply}");
    });
}
