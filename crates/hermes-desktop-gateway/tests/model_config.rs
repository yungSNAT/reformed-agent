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
    // A switch that names no provider takes the configuration's provider.
    let reply = first
        .call(
            5,
            "config.set",
            json!({"session_id": switched, "key": "model", "value": "mock-model"}),
        )
        .await;
    assert_eq!(reply["result"]["value"], "mock-model", "{reply}");
    first.turn(&switched).await;
    let seen = started.seen.lock().unwrap().clone();
    let models: Vec<&str> = seen.iter().map(|(model, _)| model.as_str()).collect();
    assert_eq!(
        models,
        ["other-model", "mock-model", "other-model", "mock-model"]
    );
}

/// `reload.env` keeps a key that start-up read from the project `.env` (the working folder's)
/// whilst the project file still holds it, even once a save has created the home `.env` and
/// after a save of that key whose home line is deleted by hand; deleted from both, it goes.
#[tokio::test]
async fn reload_env_keeps_a_key_the_project_env_still_holds() {
    use tokio::io::AsyncBufReadExt;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join(".env"), "XIAOMI_API_KEY=project-key\n").unwrap();
    // Turns go to the scripted model, which records the key each one sends.
    let (model_addr, seen) = model_server().await;
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "model:\n  default: mock-model\n  provider: {SAVED}\nllm_providers:\n  {SAVED}:\n    \
             base_url: http://{model_addr}/v1\n"
        ),
    )
    .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_hermes-desktop-gateway"))
        // A failed assertion drops the child: it is killed and its stdin closes.
        .args(["--port", "0", "--exit-when-stdin-closes"])
        .current_dir(project.path())
        .env("HERMES_HOME", home.path())
        .env("HERMES_DASHBOARD_SESSION_TOKEN", TOKEN)
        .env_remove("HERMES_AGENT_ULTRA_HOME")
        .env_remove("HERMES_MANAGED_DIR")
        .env_remove("XIAOMI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("HERMES_OPENAI_API_KEY")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Held to the end, so the gateway's later readiness line meets an open pipe.
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).await.unwrap();
    let port: u16 = first
        .trim()
        .strip_prefix("HERMES_BACKEND_READY port=")
        .unwrap_or_else(|| panic!("first line: {first}"))
        .parse()
        .unwrap();
    let mut client = connect(std::net::SocketAddr::from(([127, 0, 0, 1], port))).await;
    let session = client.create().await;
    // A reload that changes nothing first: it must not mark the project key as the home file's.
    client.call(4, "reload.env", json!({})).await;
    let reply = client
        .call(
            1,
            "model.save_key",
            json!({"slug": "openai", "api_key": "saved-key"}),
        )
        .await;
    assert!(reply.get("error").is_none(), "{reply}");
    let openai_var = reply["result"]["provider"]["key_env"]
        .as_str()
        .unwrap_or_else(|| panic!("{reply}"))
        .to_string();
    client.call(2, "reload.env", json!({})).await;
    let reply = client.call(3, "model.options", json!({})).await;
    // A key saved over the project's, whose home line is then deleted by hand, falls back to
    // the project's rather than going.
    let saved = client
        .call(
            5,
            "model.save_key",
            json!({"slug": SAVED, "api_key": "saved-key"}),
        )
        .await;
    let lines = env_lines(home.path());
    assert!(lines.contains("XIAOMI_API_KEY"), "{saved}");
    client.turn(&session).await;
    // Every other line stays: the openai key saved above.
    let kept: String = lines
        .lines()
        .filter(|line| !line.starts_with("XIAOMI_API_KEY="))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(!kept.is_empty(), "{lines}");
    std::fs::write(home.path().join(".env"), kept).unwrap();
    let reloaded = client.call(6, "reload.env", json!({})).await;
    let after = client.call(7, "model.options", json!({})).await;
    client.turn(&session).await;
    // Then deleted from the project `.env` too: gone.
    std::fs::write(project.path().join(".env"), "OTHER_KEY=keep-me\n").unwrap();
    let gone = client.call(8, "reload.env", json!({})).await;
    let removed = client.call(9, "model.options", json!({})).await;
    // A saved key whose home line is deleted whilst the project `.env` holds only a value that
    // would not read back as itself (start-up had none for it): removed, not kept.
    std::fs::write(
        project.path().join(".env"),
        format!("OTHER_KEY=keep-me\n{openai_var}=${{OTHER}}\n"),
    )
    .unwrap();
    std::fs::write(home.path().join(".env"), "").unwrap();
    let unusable = client.call(10, "reload.env", json!({})).await;
    let openai_after = client.call(11, "model.options", json!({})).await;
    // Closing stdin ends the gateway normally, so a coverage run keeps its counts.
    drop(child.stdin.take());
    let exit = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await;
    assert!(exit.unwrap().unwrap().success());
    drop(stdout);
    let signed_in = |reply: &Value, slug: &str| {
        reply["result"]["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["slug"] == slug && row["authenticated"] == true)
    };
    let authenticated = |reply: &Value| signed_in(reply, SAVED);
    assert!(authenticated(&reply), "{reply}");
    assert!(authenticated(&after), "{after}");
    assert!(!authenticated(&removed), "{removed}");
    assert!(signed_in(&removed, "openai"), "{removed}");
    assert!(!signed_in(&openai_after, "openai"), "{openai_after}");
    // The saved key went back to the project's, then that went with its line.
    assert_eq!(reloaded["result"]["updated"], 1, "{reloaded}");
    assert_eq!(gone["result"]["updated"], 1, "{gone}");
    assert_eq!(unusable["result"]["updated"], 1, "{unusable}");
    // The turn after the save sent the saved key; the one after the reload, the project's.
    let keys: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, auth)| auth.clone())
        .collect();
    assert_eq!(keys, ["Bearer saved-key", "Bearer project-key"]);
}

/// The `Authorization` header a turn on `model` sends after `model.save_key` for `slug`, and the
/// slug of the row the save answers, with `llm_providers` `entries` (name, key variable), each
/// pointed at the scripted model.
async fn sent_after_save(
    entries: &[(&str, Option<&str>)],
    model: &str,
    slug: &str,
) -> (String, Value) {
    let (model_addr, seen) = model_server().await;
    let home = tempfile::tempdir().unwrap();
    let mut config = hermes_config::GatewayConfig {
        model: Some(model.into()),
        home_dir: Some(home.path().to_string_lossy().into_owned()),
        ..hermes_config::GatewayConfig::default()
    };
    for (name, var) in entries {
        let entry = json!({"base_url": format!("http://{model_addr}/v1"), "api_key_env": var});
        config
            .llm_providers
            .insert((*name).into(), serde_json::from_value(entry).unwrap());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        hermes_desktop_gateway::Chat::new(config),
    ));
    let mut client = connect(addr).await;
    let session = client.create().await;
    let reply = client
        .call(
            3,
            "model.save_key",
            json!({"slug": slug, "api_key": "saved-key"}),
        )
        .await;
    assert!(reply.get("error").is_none(), "{reply}");
    client.turn(&session).await;
    let seen = seen.lock().unwrap();
    let sent = seen
        .last()
        .map(|(_, auth)| auth.clone())
        .unwrap_or_default();
    (sent, reply["result"]["provider"]["slug"].clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_sends_the_key_saved_for_its_provider_whichever_entry_spelling_it_matches() {
    let (a, b) = (
        Some("DESKTOP_MODEL_CONFIG_SPELLING_A_KEY"),
        Some("DESKTOP_MODEL_CONFIG_SPELLING_B_KEY"),
    );
    // No entry spelt as the provider: the build reads the runtime name's (`mimo` runs as
    // `xiaomi`); an entry with no key variable reads the registry's, never another entry's.
    // The save answers the row of the entry the key went to.
    for (entries, model, row) in [
        ([("mimo", a), ("xiaomi", b)], "MIMO:m", "xiaomi"),
        ([("MIMO", a), ("xiaomi", b)], "mimo:m", "xiaomi"),
        ([("mimo", None), ("xiaomi", b)], "mimo:m", "mimo"),
    ] {
        let (slug, _) = model.split_once(':').unwrap();
        let sent = sent_after_save(&entries, model, slug).await;
        assert_eq!(
            sent,
            ("Bearer saved-key".into(), json!(row)),
            "{entries:?} {model}"
        );
    }
    // Only other spellings: the same one every time, though each new map iterates in its own
    // order.
    for _ in 0..32 {
        let sent = sent_after_save(&[("Mimo", a), ("MIMO", b)], "mimo:m", "mimo").await;
        assert_eq!(sent, ("Bearer saved-key".into(), json!("MIMO")));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn llm_oneshot_sends_the_key_a_turn_would_and_none_once_disconnected() {
    let (model_addr, seen) = model_server().await;
    let home = tempfile::tempdir().unwrap();
    let mut config = hermes_config::GatewayConfig {
        model: Some("xiaomi:m".into()),
        home_dir: Some(home.path().to_string_lossy().into_owned()),
        ..hermes_config::GatewayConfig::default()
    };
    let entry = json!({"base_url": format!("http://{model_addr}/v1"),
        "api_key_env": "DESKTOP_MODEL_CONFIG_ONESHOT_KEY"});
    config
        .llm_providers
        .insert("xiaomi".into(), serde_json::from_value(entry).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        hermes_desktop_gateway::Chat::new(config),
    ));
    let mut client = connect(addr).await;
    let oneshot = json!({"input": "hello"});

    let reply = client
        .call(
            1,
            "model.save_key",
            json!({"slug": "xiaomi", "api_key": "saved-key"}),
        )
        .await;
    assert!(reply.get("error").is_none(), "{reply}");
    client.call(2, "llm.oneshot", oneshot.clone()).await;
    let reply = client
        .call(3, "model.disconnect", json!({"slug": "xiaomi"}))
        .await;
    assert_eq!(reply["result"]["disconnected"], true, "{reply}");
    client.call(4, "llm.oneshot", oneshot).await;
    // As a turn's: the key just saved, then none once it is removed.
    let sent: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, auth)| auth.clone())
        .collect();
    assert_eq!(sent, ["Bearer saved-key", ""]);
}
