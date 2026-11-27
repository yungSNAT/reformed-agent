//! `llm.oneshot` after a shell-set key is disconnected. Its own test binary: it sets the key's
//! environment variable for the whole process before starting a Tokio runtime.

mod model_support;

use model_support::*;
use serde_json::json;

const KEY_ENV: &str = "DESKTOP_MODEL_CONFIG_ONESHOT_SHELL_KEY";

#[test]
fn llm_oneshot_is_refused_before_any_request_once_a_shell_key_is_disconnected() {
    // SAFETY: the only test in this binary, before its runtime starts any thread.
    unsafe { std::env::set_var(KEY_ENV, "shell-key") };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        // The scripted model answers in SSE, which `llm.oneshot` cannot parse, so the first
        // reply is a parse error by design: only the header and later pre-request refusal matter.
        let (_home, mut client, seen) =
            start_providers("xiaomi:m", &[("xiaomi", Some(KEY_ENV))]).await;
        let oneshot = json!({"input": "hello"});

        client.call(1, "llm.oneshot", oneshot.clone()).await;
        let reply = client
            .call(2, "model.disconnect", json!({"slug": "xiaomi"}))
            .await;
        assert_eq!(reply["result"]["disconnected"], true, "{reply}");
        let refused = client.call(3, "llm.oneshot", oneshot).await;
        assert_eq!(code(&refused), 5030, "{refused}");
        let message = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.starts_with("one-shot generation failed: No API key for provider 'xiaomi'"),
            "{refused}"
        );
        assert_eq!(auth_sent(&seen), ["Bearer shell-key"]);
    });
}
