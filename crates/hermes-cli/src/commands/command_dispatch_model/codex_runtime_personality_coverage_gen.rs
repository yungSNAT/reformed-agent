use super::*;

fn parse_yaml(text: &str) -> serde_yaml::Value {
    serde_yaml::from_str(text).expect("valid YAML fixture")
}

#[test]
fn runtime_arguments_distinguish_status_queries_from_unknown_values() {
    assert_eq!(parse_codex_runtime_args(&[]), Ok(None));
    assert_eq!(parse_codex_runtime_args(&[" ", "\t"]), Ok(None));
    assert_eq!(
        parse_codex_runtime_args(&["  CoDeX-App-Server  "]),
        Ok(Some("codex_app_server"))
    );
    assert_eq!(parse_codex_runtime_args(&[" OFF "]), Ok(Some("auto")));
    assert_eq!(
        parse_codex_runtime_args(&["  Not", "Supported  "]),
        Err(
            "Unknown runtime 'not supported'. Use one of: auto, codex_app_server, on, off"
                .to_owned()
        )
    );
}

#[test]
fn legacy_model_strings_omit_empty_parts_and_keep_colons_in_the_model_id() {
    for (model, expected) in [
        (" \t ", "{}"),
        (
            " openai : gpt-example:preview ",
            "{provider: openai, default: 'gpt-example:preview'}",
        ),
        (" openai : ", "{provider: openai}"),
        (" : gpt-example ", "{default: gpt-example}"),
        (" : ", "{}"),
        (" gpt-example ", "{default: gpt-example}"),
    ] {
        assert_eq!(
            serde_yaml::Value::Mapping(model_string_to_mapping(model)),
            parse_yaml(expected),
            "legacy model: {model:?}"
        );
    }
}

#[test]
fn updating_the_runtime_migrates_model_config_without_losing_settings() {
    for (original, expected) in [
        (
            "[obsolete, root]",
            "model: {openai_runtime: codex_app_server}",
        ),
        (
            "theme: dark",
            "theme: dark\nmodel: {openai_runtime: codex_app_server}",
        ),
        (
            "theme: dark\nmodel: 'openai:gpt-example'",
            "theme: dark\nmodel: {provider: openai, default: gpt-example, openai_runtime: codex_app_server}",
        ),
        (
            "theme: dark\nmodel: [primary, fallback]",
            "theme: dark\nmodel: {default: [primary, fallback], openai_runtime: codex_app_server}",
        ),
        (
            "model: {default: gpt-example, provider: openai, openai_runtime: auto}",
            "model: {default: gpt-example, provider: openai, openai_runtime: codex_app_server}",
        ),
    ] {
        let mut root = parse_yaml(original);
        set_codex_runtime_config_value(&mut root, "codex_app_server");
        assert_eq!(root, parse_yaml(expected), "original config: {original}");
        assert_eq!(codex_runtime_from_config(&root), "codex_app_server");
    }
}

#[test]
fn a_missing_config_stays_absent_until_the_runtime_is_written() {
    let directory = tempfile::tempdir().expect("create temporary state directory");
    let path = directory.path().join("nested").join("config.yaml");

    let mut root = read_codex_runtime_config(&path).expect("read missing config");
    assert_eq!(root, parse_yaml("{}"));
    assert_eq!(codex_runtime_from_config(&root), "auto");
    assert!(!path.exists());

    set_codex_runtime_config_value(&mut root, "codex_app_server");
    write_codex_runtime_config(&path, &root).expect("write config and create its parent");

    assert!(path.is_file());
    assert_eq!(
        read_codex_runtime_config(&path).expect("read persisted config"),
        parse_yaml("model: {openai_runtime: codex_app_server}")
    );
}

#[cfg(unix)]
#[test]
#[ignore = "run by the version probe test with an isolated PATH"]
fn isolated_codex_version_probe() {
    let Ok(expected) = std::env::var("HERMES_COVERAGE_CODEX_PROBE_EXPECTED") else {
        return;
    };
    let expected: (bool, String) = serde_json::from_str(&expected).expect("decode expected status");
    assert_eq!(check_codex_binary_status(), expected);
    println!("isolated Codex version assertion passed");
}

#[cfg(unix)]
#[test]
fn codex_version_probe_uses_stdout_then_stderr_and_reports_failures() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;

    let executable = std::env::current_exe().expect("locate the test executable");
    let (_, test_module) = module_path!().split_once("::").expect("nested test module");
    let child_test = format!("{test_module}::isolated_codex_version_probe");
    let failure_status = std::process::ExitStatus::from_raw(1 << 8);
    let missing_binary = std::io::Error::from_raw_os_error(libc::ENOENT).to_string();

    for (script, expected) in [
        (
            Some("printf '  codex-cli 1.2.3 \\n'; printf 'warning\\n' >&2"),
            (true, "codex-cli 1.2.3".to_owned()),
        ),
        (
            Some("printf ' \\n'; printf '  version on stderr \\n' >&2"),
            (true, "version on stderr".to_owned()),
        ),
        (
            Some("printf 'ignored stdout\\n'; printf '  failed to load \\n' >&2; false"),
            (false, "failed to load".to_owned()),
        ),
        (
            Some("false"),
            (false, format!("codex exited with {failure_status}")),
        ),
        (None, (false, missing_binary)),
    ] {
        let directory = tempfile::tempdir().expect("create isolated probe directory");
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).expect("create fixture-only PATH directory");
        if let Some(body) = script {
            let codex = bin.join("codex");
            std::fs::write(
                &codex,
                format!("#!/bin/sh\nset -e\n[ \"$#\" -eq 1 ]\n[ \"$1\" = '--version' ]\n{body}\n"),
            )
            .expect("write fake Codex executable");
            std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700))
                .expect("make fake Codex executable runnable");
        }

        let output = std::process::Command::new(&executable)
            .args(["--ignored", "--exact", "--nocapture", child_test.as_str()])
            .current_dir(directory.path())
            .env("HOME", directory.path())
            .env("PATH", &bin)
            .env_remove("ENV")
            .env_remove("BASH_ENV")
            .env(
                "HERMES_COVERAGE_CODEX_PROBE_EXPECTED",
                serde_json::to_string(&expected).expect("encode expected status"),
            )
            .output()
            .expect("run the isolated probe test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("isolated Codex version assertion passed"),
            "version probe with {script:?} returned {}:\n{stdout}\n{stderr}",
            output.status
        );
    }
}
