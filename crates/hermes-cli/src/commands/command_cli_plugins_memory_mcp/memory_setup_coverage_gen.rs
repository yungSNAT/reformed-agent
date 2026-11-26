use super::*;
use serde_json::{json, Value};
use std::process::{Command, Stdio};

fn assert_config_error<T: std::fmt::Debug>(result: Result<T, AgentError>, expected: &str) {
    match result.expect_err("setup should reject this input") {
        AgentError::Config(message) => assert_eq!(message, expected),
        other => panic!("expected a configuration error, got {other:?}"),
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("read saved config"))
        .expect("parse saved config")
}

#[test]
fn setup_booleans_recognize_false_and_preserve_the_default_for_other_inputs() {
    for input in ["0", " FALSE ", "No", "off"] {
        assert!(!memory_setup_bool_value(input, true), "{input:?}");
    }
    for input in [" \t", "maybe"] {
        assert!(memory_setup_bool_value(input, true), "{input:?}");
        assert!(!memory_setup_bool_value(input, false), "{input:?}");
    }
}

#[test]
fn mem0_specific_options_ignore_blank_strings_and_yes() {
    let mut options = MemorySetupCliOptions {
        mode: Some(" \t".into()),
        host: Some(" ".into()),
        api_key: Some("\n".into()),
        ..MemorySetupCliOptions::yes_only(true)
    };
    assert!(!options.has_mem0_specific_options());

    options.mode = Some(" oss ".into());
    assert!(options.has_mem0_specific_options());
    options.mode = None;

    options.host = Some(" mem0.example.invalid ".into());
    assert!(options.has_mem0_specific_options());
    options.host = None;

    options.api_key = Some(" test-key ".into());
    assert!(options.has_mem0_specific_options());
    options.api_key = None;

    options.dry_run = true;
    assert!(options.has_mem0_specific_options());
}

#[test]
fn mem0_oss_aliases_normalize_to_oss_and_unknown_modes_are_rejected() {
    for input in [" OSS ", "open-source", "open_source", "OpenSource"] {
        let mode = normalize_mem0_setup_mode(input).expect("recognize OSS mode");
        assert_eq!(mode, Mem0SetupMode::Oss);
        assert_eq!(mode.prompt_default(), "oss");
        assert_eq!(mode.config_mode(), "oss");
        assert!(mode.uses_host());
        assert!(!mode.requires_cloud_key());
    }
    assert_eq!(normalize_mem0_setup_mode("other"), None);
}

#[test]
fn honcho_legacy_host_keys_require_a_nonblank_profile_suffix() {
    assert_eq!(legacy_honcho_host_key_for_cli("hermes"), None);
    assert_eq!(legacy_honcho_host_key_for_cli("hermes_ \t"), None);
    assert_eq!(
        legacy_honcho_host_key_for_cli("hermes_work-2").as_deref(),
        Some("hermes.work-2")
    );
}

#[test]
fn honcho_oauth_grant_needs_an_access_key_and_complete_refresh_metadata() {
    for block in [
        json!({"apiKey": 42}),
        json!({"apiKey": "ordinary-key"}),
        json!({"apiKey": "hch-at-test", "oauth": []}),
        json!({
            "apiKey": "hch-at-test",
            "oauth": {
                "refreshToken": " ",
                "clientId": "test-client",
                "tokenEndpoint": "https://auth.example.invalid/token"
            }
        }),
    ] {
        assert!(!honcho_host_value_has_oauth_grant(&block), "{block}");
    }
}

#[test]
fn honcho_grant_lookup_handles_flat_legacy_and_current_configs() {
    let home = tempfile::tempdir().expect("temporary config directory");
    let path = home.path().join("honcho.json");
    let grant = json!({
        "apiKey": "hch-at-test",
        "oauth": {
            "refreshToken": "test-refresh",
            "clientId": "test-client",
            "tokenEndpoint": "https://auth.example.invalid/token"
        }
    });

    assert!(!honcho_config_has_oauth_grant(&path, "hermes_work"));
    std::fs::write(&path, "{not json").expect("write malformed config");
    assert!(!honcho_config_has_oauth_grant(&path, "hermes_work"));

    std::fs::write(&path, grant.to_string()).expect("write flat config");
    assert!(honcho_config_has_oauth_grant(&path, "hermes_work"));

    let legacy = json!({"hosts": {"hermes.work": grant}});
    std::fs::write(&path, legacy.to_string()).expect("write legacy host config");
    assert!(honcho_config_has_oauth_grant(&path, "hermes_work"));
    assert!(!honcho_config_has_oauth_grant(&path, "hermes_other"));

    let current = json!({"hosts": {
        "hermes.work": grant,
        "hermes_work": {"apiKey": "ordinary-key"}
    }});
    std::fs::write(&path, current.to_string()).expect("write competing host entries");
    assert!(!honcho_config_has_oauth_grant(&path, "hermes_work"));
}

#[test]
fn openviking_defaults_to_no_key_only_for_local_endpoints_without_a_key() {
    assert_eq!(
        normalize_openviking_setup_endpoint(" \t"),
        "http://127.0.0.1:1933"
    );
    for endpoint in ["http://127.0.0.1", "http://localhost"] {
        assert!(openviking_setup_endpoint_is_local(endpoint));
        assert_eq!(
            normalize_openviking_setup_key_type("", endpoint, " "),
            "none"
        );
        assert_eq!(
            normalize_openviking_setup_key_type("", endpoint, "test-key"),
            "user"
        );
    }
    let endpoint = "https://memory.example.invalid";
    assert!(!openviking_setup_endpoint_is_local(endpoint));
    assert_eq!(
        normalize_openviking_setup_key_type("", endpoint, ""),
        "user"
    );
    assert_eq!(
        normalize_openviking_setup_key_type("unknown", "http://localhost", ""),
        "user"
    );
    assert_config_error(
        build_openviking_setup_config(OpenVikingSetupConfigInput {
            endpoint,
            api_key: " ",
            api_key_type: "",
            account: "",
            user: "",
            agent: "",
        }),
        "OpenViking user setup requires an API key.",
    );
}

const CHILD_CASE: &str = "HERMES_MEMORY_SETUP_COVERAGE_CASE";

fn run_setup_case(case: &str, input: &str, env: &[(&str, &str)]) -> String {
    let temp = tempfile::tempdir().expect("temporary Hermes home");
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let test_name = format!("{module}::isolated_setup_worker");
    let mut command = Command::new(std::env::current_exe().expect("test executable")); // nosemgrep: rust.lang.security.current-exe.current-exe
    command
        .args(["--exact", "--ignored", "--nocapture", test_name.as_str()])
        .env(CHILD_CASE, case)
        .env("HERMES_HOME", temp.path())
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for key in [
        "MEM0_MODE",
        "MEM0_HOST",
        "MEM0_BASE_URL",
        "MEM0_API_KEY",
        "MEM0_USER_ID",
        "MEM0_AGENT_ID",
        "HERMES_MEM0_SETUP_REACHABILITY_TIMEOUT_MS",
        "HERMES_HONCHO_HOST",
        "HERMES_PROFILE",
        "HERMES_USER",
        "HONCHO_API_KEY",
        "HONCHO_BASE_URL",
        "SUPERMEMORY_API_KEY",
        "OPENVIKING_ENDPOINT",
        "OPENVIKING_API_KEY",
        "OPENVIKING_API_KEY_TYPE",
        "OPENVIKING_ACCOUNT",
        "OPENVIKING_USER",
        "OPENVIKING_AGENT",
    ] {
        command.env_remove(key);
    }
    command.envs(env.iter().copied());

    let mut child = command.spawn().expect("spawn isolated setup test");
    child
        .stdin
        .take()
        .expect("piped setup input")
        .write_all(input.as_bytes())
        .expect("write setup input");
    let output = child.wait_with_output().expect("collect setup output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("isolated setup checks passed"),
        "{case} ({status}):\n{stdout}\n{stderr}",
        status = output.status,
        stderr = String::from_utf8_lossy(&output.stderr),
    );
    stdout.into_owned()
}

#[test]
fn mem0_cloud_defaults_and_validation_are_applied_before_saving() {
    let _output = run_setup_case("mem0-validation", "", &[]);
}

#[test]
fn mem0_setup_warns_but_saves_when_the_host_cannot_form_a_request() {
    let output = run_setup_case(
        "mem0-malformed-host",
        "",
        &[("HERMES_MEM0_SETUP_REACHABILITY_TIMEOUT_MS", "50")],
    );
    assert!(output.contains("Mem0 self-hosted reachability check warning: builder error"));
    assert!(!output.contains("reachability check passed"));
}

#[test]
fn honcho_interactive_hybrid_setup_uses_defaults_and_masks_the_key() {
    let key = "test-honcho-local-key";
    let output = run_setup_case(
        "honcho-hybrid",
        "  LOCAL  \n \t\n\n  alice  \n HyBrid \n runtime- \n web=alice, bad, chat=bob \n",
        &[
            ("HONCHO_API_KEY", key),
            ("HERMES_HONCHO_HOST", " \t"),
            ("HERMES_PROFILE", " .!? "),
        ],
    );
    assert!(output.contains("Honcho local baseUrl [http://localhost:8000]: "));
    assert!(output.contains("Honcho local JWT/API key (blank for no-auth local) [set]: "));
    assert!(output.contains("Honcho peerName: "));
    assert!(!output.contains(key));
}

#[test]
fn honcho_multi_setup_uses_the_configured_local_url_and_explicit_host() {
    let _output = run_setup_case(
        "honcho-multi",
        "\n\n\n\nmulti\n session- \n",
        &[
            ("HONCHO_BASE_URL", "http://honcho.example.invalid:8000"),
            ("HERMES_HONCHO_HOST", "  hermes_explicit  "),
            ("HERMES_PROFILE", "ignored"),
        ],
    );
}

#[test]
fn honcho_and_supermemory_report_missing_cloud_credentials() {
    let _output = run_setup_case("honcho-no-key", "", &[]);
    let output = run_setup_case("supermemory-no-key", "\n", &[]);
    assert!(output.contains(
        "Get your Supermemory API key at https://app.supermemory.ai/integrations?connect=hermes"
    ));
}

#[test]
fn openviking_setup_saves_inferred_user_and_explicit_root_configs() {
    let common_env = [
        ("OPENVIKING_ENDPOINT", "https://memory.example.invalid///"),
        ("OPENVIKING_API_KEY", "test-openviking-key"),
        ("OPENVIKING_ACCOUNT", "team"),
        ("OPENVIKING_USER", "alice"),
    ];
    let _user_output = run_setup_case("openviking-user", "", &common_env);
    let mut root_env = common_env.to_vec();
    root_env.push(("OPENVIKING_API_KEY_TYPE", "root-api-key"));
    let _root_output = run_setup_case("openviking-root", "", &root_env);
}

fn check_mem0_validation(home: &Path) {
    let defaults = MemorySetupCliOptions::yes_only(true);
    assert_config_error(
        setup_memory_provider_target("  Unknown  ", &defaults),
        "Unsupported memory provider setup target 'unknown'. Supported: byterover, honcho, mem0, openviking, supermemory",
    );
    assert_config_error(
        setup_mem0_provider(&MemorySetupCliOptions {
            mode: Some("invalid".into()),
            ..defaults.clone()
        }),
        "Mem0 setup mode must be one of: platform, selfhosted, oss.",
    );
    assert_config_error(
        setup_mem0_provider(&MemorySetupCliOptions {
            mode: Some("oss".into()),
            host: Some(" /// ".into()),
            ..defaults.clone()
        }),
        "Mem0 self-hosted setup requires --host or MEM0_HOST.",
    );
    assert_config_error(
        setup_mem0_provider(&defaults),
        "Mem0 setup requires MEM0_API_KEY or an API key entered at the prompt.",
    );
    assert!(!home.join("mem0.json").exists());
    assert!(!home.join(".env").exists());

    let result = setup_mem0_provider(&MemorySetupCliOptions {
        api_key: Some("test-mem0-cloud-key".into()),
        ..defaults
    })
    .expect("configure Mem0 platform");
    assert!(!result.dry_run);
    assert_eq!(result.config_path, home.join("mem0.json"));
    let config = read_json(&result.config_path);
    assert_eq!(config["mode"], "platform");
    assert_eq!(config["host"], "");
    assert_eq!(config["base_url"], "https://api.mem0.ai/v1");
    assert_eq!(config["api_key"], "");
    let env_path = result
        .env_path
        .expect("API key should be stored in the env file");
    assert_eq!(env_path, home.join(".env"));
    let env = std::fs::read_to_string(env_path).expect("read saved Mem0 API key");
    assert!(env
        .lines()
        .any(|line| line == "MEM0_API_KEY=test-mem0-cloud-key"));
}

#[test]
#[ignore = "invoked with isolated environment and piped stdin by the setup coverage tests"]
fn isolated_setup_worker() {
    let Ok(case) = std::env::var(CHILD_CASE) else {
        return;
    };
    let home = PathBuf::from(std::env::var_os("HERMES_HOME").expect("isolated Hermes home"));

    match case.as_str() {
        "mem0-validation" => check_mem0_validation(&home),
        "mem0-malformed-host" => {
            assert_eq!(
                probe_mem0_self_hosted_reachability(" /// ", ""),
                Err("empty host".into())
            );
            // URL parsing fails before reqwest can open a connection.
            let malformed_host = "http://[";
            let error = probe_mem0_self_hosted_reachability(malformed_host, "test-bearer-key")
                .expect_err("malformed URL must not produce a request");
            assert!(error.contains("builder error"), "{error}");

            let result = setup_mem0_provider(&MemorySetupCliOptions {
                yes: true,
                mode: Some("oss".into()),
                host: Some(malformed_host.into()),
                ..MemorySetupCliOptions::default()
            })
            .expect("reachability warnings should not abort setup");
            assert_eq!(result.config_path, home.join("mem0.json"));
            assert!(result.env_path.is_none());
            assert_eq!(read_json(&result.config_path)["host"], malformed_host);
        }
        "honcho-hybrid" => {
            let result = setup_honcho_provider(false).expect("configure hybrid Honcho");
            assert_eq!(result.config_path, home.join("honcho.json"));
            let config = read_json(&result.config_path);
            assert_eq!(config["baseUrl"], "http://localhost:8000");
            let host = &config["hosts"]["hermes_profile"];
            assert_eq!(host["apiKey"], "test-honcho-local-key");
            assert_eq!(host["aiPeer"], "profile");
            assert_eq!(host["peerName"], "alice");
            assert_eq!(host["pinUserPeer"], false);
            assert_eq!(host["runtimePeerPrefix"], "runtime-");
            assert_eq!(
                host["userPeerAliases"],
                json!({"web": "alice", "chat": "bob"})
            );
        }
        "honcho-multi" => {
            let result = setup_honcho_provider(false).expect("configure multi-peer Honcho");
            let config = read_json(&result.config_path);
            assert_eq!(config["baseUrl"], "http://honcho.example.invalid:8000");
            let host = &config["hosts"]["hermes_explicit"];
            assert_eq!(host["aiPeer"], "explicit");
            assert_eq!(host["pinUserPeer"], false);
            assert_eq!(host["runtimePeerPrefix"], "session-");
            assert!(host.get("userPeerAliases").is_none());
        }
        "honcho-no-key" => {
            assert_config_error(
                setup_honcho_provider(true),
                "Honcho cloud setup requires HONCHO_API_KEY or an API key entered at the prompt.",
            );
            assert!(!home.join("honcho.json").exists());
        }
        "supermemory-no-key" => {
            assert_config_error(
                setup_supermemory_provider(false),
                "Supermemory setup requires SUPERMEMORY_API_KEY or an API key from https://app.supermemory.ai/integrations?connect=hermes.",
            );
            assert!(!home.join("supermemory.json").exists());
        }
        "openviking-user" | "openviking-root" => {
            let result = setup_openviking_provider(true).expect("configure OpenViking");
            assert_eq!(result.config_path, home.join("openviking.json"));
            let config = read_json(&result.config_path);
            let expected_type = if case == "openviking-root" {
                "root"
            } else {
                "user"
            };
            assert_eq!(config["api_key_type"], expected_type);
            assert_eq!(config["endpoint"], "https://memory.example.invalid");
            assert_eq!(config["account"], "team");
            assert_eq!(config["user"], "alice");
        }
        _ => panic!("unknown isolated setup case: {case}"),
    }
    println!("isolated setup checks passed");
}
