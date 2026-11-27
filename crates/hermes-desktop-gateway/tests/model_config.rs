//! The model picker's methods (`model.options`, `model.save_key`, `model.disconnect`), `reload.env`
//! and `config.set model`, as upstream's tui_gateway answers them. Refusals first.

mod model_support;

use model_support::*;
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread")]
async fn bad_requests_are_refused_with_upstreams_codes_and_change_nothing() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let before = env_lines(started.home.path());

    let reply = client
        .call(1, "model.save_key", json!({"slug": SAVED}))
        .await;
    assert_eq!(code(&reply), 4001);
    let reply = client
        .call(2, "model.save_key", json!({"slug": " ", "api_key": "k"}))
        .await;
    assert_eq!(code(&reply), 4001);
    let reply = client
        .call(
            3,
            "model.save_key",
            json!({"slug": "no-such-provider", "api_key": "k"}),
        )
        .await;
    assert_eq!(code(&reply), 4002);
    // A provider whose key does not come from an environment variable.
    let reply = client
        .call(
            4,
            "model.save_key",
            json!({"slug": "ollama", "api_key": "k"}),
        )
        .await;
    assert_eq!(code(&reply), 4004);
    let reply = client.call(5, "model.disconnect", json!({})).await;
    assert_eq!(code(&reply), 4001);
    // A known provider with no key saved, unless the machine running the tests exports one.
    if std::env::var_os("DEEPSEEK_API_KEY").is_none() {
        let reply = client
            .call(6, "model.disconnect", json!({"slug": "deepseek"}))
            .await;
        assert_eq!(code(&reply), 4005);
    }
    assert_eq!(env_lines(started.home.path()), before);

    // config.set: a model switch needs a live session; unknown keys and empty values are refused.
    let reply = client
        .call(
            7,
            "config.set",
            json!({"key": "model", "value": "other-model"}),
        )
        .await;
    assert_eq!(code(&reply), 4001);
    let reply = client
        .call(
            8,
            "config.set",
            json!({"session_id": "no-such-session", "key": "model", "value": "other-model"}),
        )
        .await;
    assert_eq!(code(&reply), 4001);
    let session = client.create().await;
    let reply = client
        .call(
            9,
            "config.set",
            json!({"session_id": session, "key": "model", "value": ""}),
        )
        .await;
    assert_eq!(code(&reply), 4002);
    let reply = client
        .call(
            10,
            "config.set",
            json!({"session_id": session, "key": "no.such.key", "value": "1"}),
        )
        .await;
    assert_eq!(code(&reply), 4002);
    let reply = client
        .call(
            11,
            "config.set",
            json!({"session_id": session, "key": "model", "value": "other-model --once"}),
        )
        .await;
    assert_eq!(
        code(&reply),
        4002,
        "a switch flag this gateway does not honour"
    );
    // None of the refused switches changed the session's model.
    client.turn(&session).await;
    let seen = started.seen.lock().unwrap().clone();
    let models: Vec<&str> = seen.iter().map(|(model, _)| model.as_str()).collect();
    assert_eq!(models, ["mock-model"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_a_env_file_cannot_keep_is_refused_before_anything_is_written() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let before = env_lines(started.home.path());
    for (n, key) in [
        // A newline or carriage return would start another assignment.
        "k\nOPENAI_BASE_URL=https://elsewhere.example/v1",
        "k\rOPENAI_BASE_URL=https://elsewhere.example/v1",
        // A NUL would stop every later start: setting it in the environment panics.
        "a\u{0}b",
        "a\tb",
        // The start-up sanitiser would split this into a second assignment.
        "k OPENAI_BASE_URL=https://elsewhere.example/v1",
        // The reader would take the quotes off or resolve the reference.
        "\"quoted-key\"",
        "'quoted-key'",
        "${OTHER_SECRET}",
    ]
    .into_iter()
    .enumerate()
    {
        let reply = client
            .call(
                n as u64 + 1,
                "model.save_key",
                json!({"slug": SAVED, "api_key": key}),
            )
            .await;
        assert_eq!(code(&reply), 4002, "{key:?}");
        let message = reply["error"]["message"].as_str().unwrap();
        assert!(!message.contains(key.trim()), "names the key: {message}");
        assert_eq!(env_lines(started.home.path()), before, "{key:?}");
    }
    let reply = client.call(20, "model.options", json!({})).await;
    let rows = reply["result"]["providers"].as_array().unwrap();
    assert!(!rows.iter().any(|row| row["slug"] == SAVED), "{reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn model_options_lists_the_configured_providers_and_the_current_model() {
    let started = start().await;
    let mut client = connect(started.addr).await;

    let reply = client.call(1, "model.options", json!({})).await;
    let result = &reply["result"];
    assert_eq!(result["model"], "mock-model", "{reply}");
    assert_eq!(result["provider"], "mock");
    let rows = result["providers"].as_array().unwrap();
    // Only configured providers by default, and known ones whose key the machine running the
    // tests exports.
    let configured: Vec<&Value> = rows.iter().filter(|row| row["source"] != "env").collect();
    assert_eq!(configured.len(), 1, "{reply}");
    let mock = configured[0];
    assert_eq!(mock["slug"], "mock");
    assert_eq!(mock["is_current"], true);
    assert_eq!(mock["authenticated"], true);
    assert_eq!(mock["key_env"], KEY_ENV);
    assert_eq!(mock["models"], json!(["mock-model", "other-model"]));
    assert_eq!(mock["total_models"], 2);

    let reply = client
        .call(2, "model.options", json!({"include_unconfigured": true}))
        .await;
    let rows = reply["result"]["providers"].as_array().unwrap();
    // A known provider no test sets a key for.
    let saved = rows
        .iter()
        .find(|row| row["slug"] == "xai")
        .unwrap_or_else(|| panic!("{reply}"));
    assert_eq!(saved["models"], json!([]));
    assert_eq!(saved["is_current"], false);
    if std::env::var_os("XAI_API_KEY").is_none() {
        assert_eq!(saved["source"], "canonical");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_key_goes_to_the_profiles_env_file_and_disconnect_removes_it() {
    let started = start().await;
    let home = started.home.path();
    let mut client = connect(started.addr).await;
    let exported = std::env::var_os("XIAOMI_API_KEY");

    let reply = client
        .call(
            1,
            "model.save_key",
            json!({"slug": SAVED, "api_key": "  saved-key  "}),
        )
        .await;
    let row = &reply["result"]["provider"];
    assert_eq!(row["slug"], SAVED, "{reply}");
    assert_eq!(row["authenticated"], true);
    let env = env_lines(home);
    assert!(env.contains(&format!("{KEY_ENV}=first-key\n")), "{env}");
    assert!(env.contains("XIAOMI_API_KEY=saved-key\n"), "{env}");
    // The next turns get the key; the process environment, read by other threads, is untouched.
    assert_eq!(std::env::var_os("XIAOMI_API_KEY"), exported);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join(".env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The saved provider now lists as configured.
    let reply = client.call(2, "model.options", json!({})).await;
    let rows = reply["result"]["providers"].as_array().unwrap();
    assert!(rows
        .iter()
        .any(|row| row["slug"] == SAVED && row["authenticated"] == true));

    let reply = client
        .call(3, "model.disconnect", json!({"slug": SAVED}))
        .await;
    assert_eq!(
        reply["result"],
        json!({"slug": SAVED, "name": SAVED, "disconnected": true})
    );
    let env = env_lines(home);
    assert!(!env.contains("XIAOMI_API_KEY"), "{env}");
    assert!(env.contains(&format!("{KEY_ENV}=first-key\n")), "{env}");
    let reply = client
        .call(4, "model.disconnect", json!({"slug": SAVED}))
        .await;
    assert_eq!(code(&reply), 4005);
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_env_makes_the_next_turn_use_the_new_key() {
    let started = start_with(RELOAD_KEY_ENV).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    client.turn(&session).await;

    let home_before = std::env::var_os("HERMES_HOME");
    // Only the key variables are re-read: this process's launch settings stay as started.
    std::fs::write(
        started.home.path().join(".env"),
        format!(
            "{RELOAD_KEY_ENV}=second-key\nHERMES_HOME=/elsewhere\n\
             HERMES_DASHBOARD_SESSION_TOKEN=another-token\n"
        ),
    )
    .unwrap();
    let reply = client.call(3, "reload.env", json!({})).await;
    assert_eq!(reply["result"], json!({"updated": 1}), "{reply}");
    assert_eq!(std::env::var_os("HERMES_HOME"), home_before);
    assert_eq!(std::env::var(RELOAD_KEY_ENV).unwrap(), "first-key");
    // The gateway's token still opens a connection.
    connect(started.addr).await;
    client.turn(&session).await;
    let seen = started.seen.lock().unwrap().clone();
    let keys: Vec<&str> = seen.iter().map(|(_, auth)| auth.as_str()).collect();
    assert_eq!(keys, ["Bearer first-key", "Bearer second-key"]);
    // Nothing changed since: nothing to update.
    let reply = client.call(4, "reload.env", json!({})).await;
    assert_eq!(reply["result"], json!({"updated": 0}));
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_env_reaches_a_key_config_yaml_takes_from_the_environment() {
    // Start-up expands `${VAR}` in `config.yaml`, so the provider's key arrives as the value.
    let started = start_from(
        REFERENCE_KEY_ENV,
        &format!("    api_key: ${{{REFERENCE_KEY_ENV}}}\n"),
    )
    .await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    client.turn(&session).await;
    std::fs::write(
        started.home.path().join(".env"),
        format!("{REFERENCE_KEY_ENV}=second-key\n"),
    )
    .unwrap();
    let reply = client.call(3, "reload.env", json!({})).await;
    assert_eq!(reply["result"], json!({"updated": 1}), "{reply}");
    client.turn(&session).await;
    let seen = started.seen.lock().unwrap().clone();
    let keys: Vec<&str> = seen.iter().map(|(_, auth)| auth.as_str()).collect();
    assert_eq!(keys, ["Bearer first-key", "Bearer second-key"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_switch_changes_only_that_sessions_next_turns() {
    let started = start().await;
    let mut first = connect(started.addr).await;
    let mut second = connect(started.addr).await;
    let switched = first.create().await;
    let other = second.create().await;

    let reply = first
        .call(
            3,
            "config.set",
            json!({"session_id": switched, "key": "model", "value": "other-model --provider mock"}),
        )
        .await;
    assert_eq!(
        reply["result"],
        json!({"key": "model", "value": "other-model", "warning": "",
               "confirm_required": false, "confirm_message": "", "scope": "session"}),
        "{reply}"
    );
    first.turn(&switched).await;
    second.turn(&other).await;
    // A switch this gateway does not honour leaves the switched model in place.
    let reply = first
        .call(
            4,
            "config.set",
            json!({"session_id": switched, "key": "model", "value": "mock-model --global"}),
        )
        .await;
    assert_eq!(code(&reply), 4002);
    first.turn(&switched).await;
    let seen = started.seen.lock().unwrap().clone();
    let models: Vec<&str> = seen.iter().map(|(model, _)| model.as_str()).collect();
    assert_eq!(models, ["other-model", "mock-model", "other-model"]);
}
