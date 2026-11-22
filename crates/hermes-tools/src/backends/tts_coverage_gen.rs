use super::*;

#[test]
fn command_provider_lookup_handles_flat_nested_and_missing_entries() {
    let flat = json!({"Studio": {"type": " CoMmAnD ", "command": "  synth  "}});
    let expected = CommandTtsProviderConfig {
        name: "Studio".to_owned(),
        command: "synth".to_owned(),
        config: flat["Studio"].clone(),
    };
    assert_eq!(
        resolve_command_provider_config("studio", &flat),
        Some(expected.clone())
    );
    assert_eq!(resolve_command_provider_config("missing", &flat), None);

    let mut nested = flat;
    nested["providers"] = json!({"Other": {"command": "other-synth"}, "Invalid": false});
    assert_eq!(
        resolve_command_provider_config("STUDIO", &nested),
        Some(expected)
    );
    assert_eq!(resolve_command_provider_config("missing", &nested), None);
    assert_eq!(resolve_command_provider_config("invalid", &nested), None);
    assert!(!is_command_provider_config(&json!("synth")));
    assert!(iter_command_providers(&Value::Null).is_empty());
}

#[test]
fn command_provider_iteration_recovers_a_valid_case_collision() {
    let config = json!({
        "providers": {
            "Studio": null,
            "studio": {"command": "  nested-synth  "}
        },
        "STUDIO": {"command": "flat-synth"}
    });

    assert_eq!(resolve_command_provider_config("studio", &config), None);
    assert_eq!(
        iter_command_providers(&config),
        vec![CommandTtsProviderConfig {
            name: "studio".to_owned(),
            command: "nested-synth".to_owned(),
            config: config["providers"]["studio"].clone(),
        }]
    );
}

#[test]
fn nested_settings_take_precedence_and_limits_require_numbers() {
    let config = json!({
        "openai": {"max_text_length": 77},
        "providers": {"OpenAI": {"max_text_length": "88", "speed": " 1.75 "}},
        "speed": " 0.1 ",
        "piper": {"speed": true}
    });

    assert_eq!(
        named_tts_provider_config(&config, "OPENAI"),
        Some(&config["providers"]["OpenAI"])
    );
    assert_eq!(resolve_max_text_length(Some(" OPENAI "), &config), 4_096);
    assert_eq!(tts_speed_for_provider("openai", &config, None), Some(1.75));
    assert_eq!(tts_speed_for_provider("piper", &config, None), Some(0.25));
    assert_eq!(
        tts_speed_for_provider("piper", &json!({"speed": false}), None),
        None
    );
}

#[test]
fn elevenlabs_limit_honors_override_and_uses_multilingual_default() {
    let config = json!({
        "providers": {
            "ElevenLabs": {"model_id": "eleven_flash_v2_5", "max_text_length": 123}
        }
    });
    assert_eq!(elevenlabs_max_text_length(&config), 123);
    assert_eq!(elevenlabs_max_text_length(&json!({})), 10_000);
    for model in ["eleven_multilingual_v2", "unknown-model"] {
        assert_eq!(
            elevenlabs_max_text_length(&json!({"elevenlabs": {"model_id": model}})),
            10_000
        );
    }
}

#[test]
fn command_output_format_prefers_supported_extension_then_config() {
    let config = json!({"output_format": " FlAc ", "format": "ogg"});
    assert_eq!(
        command_tts_output_format(&config, Some(Path::new("clip.WAV"))),
        "wav"
    );
    assert_eq!(
        command_tts_output_format(&config, Some(Path::new("clip.bin"))),
        "flac"
    );
    assert_eq!(command_tts_output_format(&config, None), "flac");
    assert_eq!(
        command_tts_output_format(&json!({"format": " OgG "}), Some(Path::new("clip"))),
        "ogg"
    );
    assert_eq!(
        command_tts_output_format(&json!({"output_format": "aac"}), None),
        "mp3"
    );
}

#[test]
fn voice_compatibility_accepts_only_recognized_flags() {
    for value in [
        json!(true),
        json!(" 1 "),
        json!(" TRUE "),
        json!("YeS"),
        json!("on"),
    ] {
        assert!(command_tts_voice_compatible(
            &json!({"voice_compatible": value})
        ));
    }
    for value in [json!(false), json!("off"), json!("sometimes"), json!(1)] {
        assert!(!command_tts_voice_compatible(
            &json!({"voice_compatible": value})
        ));
    }
    assert!(!command_tts_voice_compatible(&json!({})));
}

#[test]
fn shell_template_preserves_quote_context_and_unknown_placeholders() {
    let values = BTreeMap::from([
        ("empty", String::new()),
        ("voice", "O'Brien $voice".to_owned()),
    ]);
    assert_eq!(
        render_command_tts_template(
            r#"tts "café \"{voice}" '{voice}' {empty} {unknown} {"#,
            &values
        ),
        r#"tts "café \"O'Brien \$voice" 'O'\''Brien $voice' '' {unknown} {"#
    );
}

const CHILD_ROOT: &str = "HERMES_TTS_COVERAGE_TEST_ROOT";

// The test subprocess has its own environment, including its home and temp directories.
fn run_isolated(test: &str, configure: impl FnOnce(&mut std::process::Command, &Path)) {
    let root = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env(CHILD_ROOT, root.path())
        .env(
            "HERMES_TTS_CONFIG_JSON",
            r#"{"provider":"coverage-fixture"}"#,
        )
        .args(["--ignored", "--exact", "--nocapture"])
        .arg(format!("{module}::{test}"));
    for key in [
        "HOME",
        "USERPROFILE",
        "HERMES_HOME",
        "XDG_CONFIG_HOME",
        "APPDATA",
        "LOCALAPPDATA",
        "TMPDIR",
        "TMP",
        "TEMP",
        "PATH",
    ] {
        command.env(key, root.path());
    }
    for key in ["LLVM_PROFILE_FILE", "SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    configure(&mut command, root.path());

    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test}: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn execution_failure(error: ToolError) -> String {
    match error {
        ToolError::ExecutionFailed(message) => message,
        other => panic!("expected an execution failure, got {other}"),
    }
}

#[test]
fn remote_backends_report_errors_without_sending_requests() {
    run_isolated("missing_remote_credentials", |_, _| {});
    run_isolated("invalid_remote_requests", |command, _| {
        command
            .env("ELEVENLABS_API_KEY", "invalid\nheader")
            .env("VOICE_TOOLS_OPENAI_KEY", "fixture-key")
            .env("OPENAI_BASE_URL", "not-a-url");
    });
}

#[tokio::test]
#[ignore = "run with isolated credentials by the parent test"]
async fn missing_remote_credentials() {
    let backend = MultiTtsBackend::default();
    assert_eq!(backend.tts_config, json!({"provider": "coverage-fixture"}));
    assert_eq!(
        execution_failure(
            backend
                .synthesize_elevenlabs("hello", "fixture-voice")
                .await
                .unwrap_err()
        ),
        "ELEVENLABS_API_KEY not set"
    );

    let message = execution_failure(
        backend
            .openai_tts("hello", "alloy", None, None)
            .await
            .unwrap_err(),
    );
    assert!(message.contains("HERMES_OPENAI_API_KEY"), "{message}");
    assert!(
        message.contains("no managed openai-audio gateway"),
        "{message}"
    );
}

#[tokio::test]
#[ignore = "run with deliberately invalid request settings by the parent test"]
async fn invalid_remote_requests() {
    let backend = MultiTtsBackend::default();
    let message = execution_failure(
        backend
            .synthesize_elevenlabs("hello", "fixture-voice")
            .await
            .unwrap_err(),
    );
    assert!(
        message.starts_with("ElevenLabs API failed: builder error"),
        "{message}"
    );

    let message = execution_failure(
        backend
            .openai_tts("hello", "alloy", None, Some(1.25))
            .await
            .unwrap_err(),
    );
    assert!(
        message.starts_with("OpenAI TTS API failed: builder error"),
        "{message}"
    );
}

#[cfg(unix)]
fn child_root() -> PathBuf {
    PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap())
}

#[cfg(unix)]
#[test]
fn piper_uses_local_fixture_and_reports_process_and_file_errors() {
    use std::os::unix::fs::PermissionsExt;

    run_isolated("piper_fixture_results", |command, root| {
        let binary = root.join("piper-fixture");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
case "$2" in
    diagnostic) cat >/dev/null; printf '  fixture diagnostic  \n' >&2; false ;;
    silent) cat >/dev/null; false ;;
    no-output) cat >/dev/null ;;
    *) { printf '%s\n' "$@"; cat; } > "$4" ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        command
            .env("PATH", "/usr/bin:/bin")
            .env("PIPER_BINARY", binary)
            .env("PIPER_CONFIG", " fixture config ")
            .env("PIPER_SPEAKER", " 3 ")
            .env("PIPER_LENGTH_SCALE", " 1.2 ")
            .env("PIPER_NOISE_SCALE", " 0.4 ")
            .env("PIPER_NOISE_W", " 0.7 ");
    });
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "run with a temporary Piper executable by the parent test"]
async fn piper_fixture_results() {
    let root = child_root();
    let backend = MultiTtsBackend::default();
    let result = backend
        .piper_tts("local speech", Some(" model voice "), None)
        .await
        .unwrap();
    let data: Value = serde_json::from_str(&result).unwrap();
    let file = Path::new(data["file"].as_str().unwrap());
    assert_eq!(file.parent(), Some(root.as_path()));
    assert_eq!(file.extension().unwrap(), "wav");
    let expected = format!(
        "--model\nmodel voice\n--output_file\n{}\n--config\nfixture config\n\
         --speaker\n3\n--length_scale\n1.2\n--noise_scale\n0.4\n--noise_w\n0.7\nlocal speech\n",
        file.display()
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), expected);
    assert_eq!(data["provider"], "piper");
    assert_eq!(data["transport"], "local");
    assert_eq!(data["voice"], "model voice");
    assert_eq!(data["bytes"], json!(expected.len()));

    let missing = root.join("missing.wav");
    let mut messages = Vec::new();
    for model in ["diagnostic", "silent", "no-output"] {
        let error = backend
            .piper_tts("local speech", Some(model), missing.to_str())
            .await
            .unwrap_err();
        messages.push(execution_failure(error));
        assert!(!missing.exists(), "{model} unexpectedly wrote output");
    }
    assert!(messages[1].starts_with("piper exited with status "));
    assert_eq!(messages[0], format!("{}: fixture diagnostic", messages[1]));
    assert!(messages[2].starts_with(&format!(
        "piper completed but failed reading output {}:",
        missing.display()
    )));

    let binary = root.join("piper-fixture");
    std::fs::remove_file(&binary).unwrap();
    let message = execution_failure(
        backend
            .piper_tts("local speech", Some("model voice"), missing.to_str())
            .await
            .unwrap_err(),
    );
    assert!(message.starts_with(&format!(
        "Failed to start piper binary '{}':",
        binary.display()
    )));
    assert!(!missing.exists());
}

#[cfg(unix)]
fn command_fixture(command: &str, config: Value) -> CommandTtsProviderConfig {
    CommandTtsProviderConfig {
        name: "fixture".to_owned(),
        command: command.to_owned(),
        config,
    }
}

#[cfg(unix)]
#[test]
fn command_tts_handles_local_output_diagnostics_and_timeout() {
    run_isolated("command_fixture_results", |command, _| {
        command.env("PATH", "/usr/bin:/bin");
    });
    run_isolated("command_fixture_spawn_failure", |_, _| {});
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "run in a temporary environment by the parent test"]
async fn command_fixture_results() {
    let root = child_root();
    let backend = MultiTtsBackend::default();
    let provider = command_fixture(
        "cat {input_path} > {output_path}",
        json!({"format": " OGG ", "voice_compatible": " off "}),
    );
    let text = "café audio\n";
    let result = backend
        .command_tts(text, None, &provider, None, None)
        .await
        .unwrap();
    let data: Value = serde_json::from_str(&result).unwrap();
    let file = Path::new(data["file"].as_str().unwrap());
    assert_eq!(file.parent(), Some(root.as_path()));
    assert_eq!(file.extension().unwrap(), "ogg");
    assert_eq!(std::fs::read_to_string(file).unwrap(), text);
    assert_eq!(data["file_path"], data["file"]);
    assert_eq!(data["format"], "ogg");
    assert_eq!(data["bytes"], json!(text.len()));
    assert_eq!(data["voice_compatible"], false);
    assert_eq!(
        data["media_tag"],
        json!(format!("[[audio]]{}", file.display()))
    );

    let requested = root.join("requested clip.wav");
    let no_output = "TTS command provider 'fixture' produced no output";
    for (template, expected, empty_file) in [
        (
            "printf '  fixture diagnostic  \\n' >&2; false",
            "TTS command provider 'fixture' exited with code 1: fixture diagnostic".to_owned(),
            false,
        ),
        (
            "false",
            "TTS command provider 'fixture' exited with code 1".to_owned(),
            false,
        ),
        (
            ":",
            format!("{no_output} at {}:", requested.display()),
            false,
        ),
        (": > {output_path}", no_output.to_owned(), true),
    ] {
        let provider = command_fixture(template, Value::Null);
        let message = execution_failure(
            backend
                .command_tts(text, None, &provider, requested.to_str(), None)
                .await
                .unwrap_err(),
        );
        if template == ":" {
            assert!(message.starts_with(&expected), "{message}");
        } else {
            assert_eq!(message, expected);
        }
        assert_eq!(requested.exists(), empty_file, "{template}");
        if empty_file {
            assert_eq!(std::fs::metadata(&requested).unwrap().len(), 0);
            std::fs::remove_file(&requested).unwrap();
        }
    }

    // Stop only the fixture shell so it cannot finish before the backend times out.
    let provider = command_fixture("kill -STOP \"$$\"", json!({"timeout_seconds": 0.08}));
    let error = timeout(
        Duration::from_secs(5),
        backend.command_tts(text, None, &provider, requested.to_str(), None),
    )
    .await
    .expect("the backend's short timeout did not fire")
    .unwrap_err();
    assert_eq!(
        execution_failure(error),
        "TTS command provider 'fixture' timed out after 0.1s"
    );
    assert!(!requested.exists());
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "run with a PATH that contains no shell by the parent test"]
async fn command_fixture_spawn_failure() {
    let backend = MultiTtsBackend::default();
    let output = child_root().join("clip.wav");
    let provider = command_fixture(":", Value::Null);
    let message = execution_failure(
        backend
            .command_tts("hello", None, &provider, output.to_str(), None)
            .await
            .unwrap_err(),
    );
    assert!(
        message.starts_with("TTS command provider 'fixture' failed to start:"),
        "{message}"
    );
    assert!(!output.exists());
}
