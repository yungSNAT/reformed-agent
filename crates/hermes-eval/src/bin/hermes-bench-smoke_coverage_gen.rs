// Cargo also auto-discovers this file as a standalone binary. The test below
// runs only in the smoke binary's harness, where `crate::main` is the real entry point.
#[allow(dead_code)]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use std::process::{Command, ExitCode};

#[cfg(test)]
fn contains_string(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(text) => text == expected,
        Value::Array(items) => items.iter().any(|item| contains_string(item, expected)),
        Value::Object(fields) => fields
            .values()
            .any(|field| contains_string(field, expected)),
        _ => false,
    }
}

#[test]
fn smoke_reports_selected_and_default_models() {
    if env!("CARGO_BIN_NAME") != "hermes-bench-smoke" {
        return;
    }

    const CHILD: &str = "HERMES_EVAL_SMOKE_COVERAGE_CHILD";

    if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
        assert_eq!(crate::main(), ExitCode::SUCCESS);
        return;
    }

    for (model, expected) in [
        (Some("coverage-test-model"), "coverage-test-model"),
        (None, "tblite-smoke"),
    ] {
        let sandbox = tempfile::tempdir().expect("create smoke sandbox");
        // Re-run just this test so the environment overrides are local to the child.
        let mut command = Command::new(std::env::current_exe().expect("locate test executable"));
        command
            .args([
                "--exact",
                "coverage_gen::smoke_reports_selected_and_default_models",
                "--nocapture",
            ])
            .current_dir(sandbox.path())
            .env(CHILD, "1")
            .env("HERMES_EVAL_MAX_TASKS", "1");

        for key in [
            "HOME",
            "USERPROFILE",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
        ] {
            command.env(key, sandbox.path());
        }

        if let Some(model) = model {
            command.env("HERMES_EVAL_MODEL", model);
        } else {
            command.env_remove("HERMES_EVAL_MODEL");
        }

        let output = command.output().expect("run smoke test in child");
        let stdout = String::from_utf8(output.stdout).expect("smoke stdout should be UTF-8");
        assert!(
            output.status.success(),
            "smoke run failed for {expected:?} ({}):\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );

        // Read one JSON value without consuming the test harness's trailing output.
        let record = stdout
            .match_indices("{\n")
            .find_map(|(start, _)| {
                serde_json::Deserializer::from_str(&stdout[start..])
                    .into_iter::<Value>()
                    .next()
                    .and_then(Result::ok)
            })
            .unwrap_or_else(|| panic!("smoke run did not print a JSON record:\n{stdout}"));
        assert!(
            contains_string(&record, expected),
            "smoke record did not report model {expected:?}: {record}",
        );
    }
}
