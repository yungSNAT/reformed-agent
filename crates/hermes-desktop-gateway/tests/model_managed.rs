//! `reload.env` under an administrator-pinned managed `.env`. Its own test binary: it sets
//! `HERMES_MANAGED_DIR` for the whole process.

mod model_support;

use model_support::*;
use serde_json::json;

const MANAGED_KEY_ENV: &str = "DESKTOP_MODEL_MANAGED_TEST_KEY";

#[test]
fn a_reload_never_overrides_the_administrator_pinned_env() {
    let managed = tempfile::tempdir().unwrap();
    std::fs::write(
        managed.path().join(".env"),
        format!("{MANAGED_KEY_ENV}=managed-key\n"),
    )
    .unwrap();
    // SAFETY: the only test in this binary, before its runtime starts any thread.
    unsafe { std::env::set_var("HERMES_MANAGED_DIR", managed.path()) };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let started = start_with(MANAGED_KEY_ENV).await;
        let mut client = connect(started.addr).await;
        let session = client.create().await;
        client.turn(&session).await;

        std::fs::write(
            started.home.path().join(".env"),
            format!("{MANAGED_KEY_ENV}=home-key\n"),
        )
        .unwrap();
        let reply = client.call(3, "reload.env", json!({})).await;
        assert_eq!(reply["result"], json!({"updated": 0}), "{reply}");
        client.turn(&session).await;
        let seen = started.seen.lock().unwrap().clone();
        let keys: Vec<&str> = seen.iter().map(|(_, auth)| auth.as_str()).collect();
        assert_eq!(keys, ["Bearer managed-key", "Bearer managed-key"]);
    });
}
