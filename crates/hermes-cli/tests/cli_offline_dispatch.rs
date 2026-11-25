#[path = "support/isolated_hermes.rs"]
mod support;

use std::time::Duration;

use support::isolated_hermes;

#[tokio::test]
async fn offline_tool_toggles_persist_without_duplicate_config_entries() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("hermes/config.yaml");
    for (args, expected) in [
        (&["tools", "list", "--summary"][..], "enabled: (none)"),
        (&["tools", "disable", "todo"], "Disabled tool 'todo'"),
        (&["tools", "disable", "todo"], "Disabled tool 'todo'"),
        (&["tools", "list"], "todo [disabled]"),
        (
            &["tools", "list", "--platform", "telegram", "--summary"],
            "Tool summary (platform=telegram)",
        ),
        (&["tools", "enable", "todo"], "Enabled tool 'todo'"),
        (&["tools", "enable", "todo"], "Enabled tool 'todo'"),
        (&["tools", "list"], "todo [enabled]"),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(root.path()).args(args).output(),
        )
        .await
        .expect("tool configuration command timed out")
        .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(expected), "{args:?}: {stdout}");
        if args.get(1) == Some(&"disable") {
            let config = hermes_config::load_user_config_file(&config_path).unwrap();
            assert_eq!(config.tools_config.disabled, ["todo"]);
            assert!(config.tools_config.enabled.is_empty());
        }
        if args.get(1) == Some(&"enable") {
            let config = hermes_config::load_user_config_file(&config_path).unwrap();
            assert_eq!(config.tools_config.enabled, ["todo"]);
            assert!(config.tools_config.disabled.is_empty());
        }
    }
    let saved = std::fs::read(&config_path).unwrap();
    for action in ["disable", "enable"] {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(root.path())
                .args(["tools", action])
                .output(),
        )
        .await
        .expect("missing tool name timed out")
        .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("tools {action}: usage")));
        assert_eq!(std::fs::read(&config_path).unwrap(), saved);
    }
}

#[tokio::test]
async fn offline_session_eval_applies_the_cli_limit_and_reports_write_errors() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("sample-sessions");
    std::fs::create_dir(&sessions).unwrap();
    let fixture = r#"{"messages":[{"role":"user","content":"Read the fixture"},{"role":"assistant","content":"<tool_call>read_fixture</tool_call>"}]}"#;
    for file in ["alpha.json", "beta.json"] {
        std::fs::write(sessions.join(file), fixture).unwrap();
    }

    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(root.path())
            .args([
                "session-eval",
                "--sessions-dir",
                "sample-sessions",
                "--max-sessions",
                "1",
                "--out-json",
                "report.json",
                "--json",
            ])
            .output(),
    )
    .await
    .expect("session-eval timed out")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["summary"]["sessions_analyzed"], 1);
    let reported_sessions = std::path::Path::new(report["sessions_dir"].as_str().unwrap());
    assert_eq!(
        reported_sessions.canonicalize().unwrap(),
        sessions.canonicalize().unwrap()
    );
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(report, on_disk);

    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(root.path())
            .args(["session-eval", "--out-json", "report.json/nested"])
            .output(),
    )
    .await
    .expect("session-eval timed out")
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error:") && stderr.contains("report.json"),
        "{stderr}"
    );
    let preserved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(preserved, report);
}

#[tokio::test]
async fn offline_agent_import_plans_then_applies_into_the_explicit_home() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("fixture-agent");
    std::fs::create_dir(&source).unwrap();
    let guidance = "# Agent guide\n- Verify fixtures before publishing.\n";
    std::fs::write(source.join("AGENTS.md"), guidance).unwrap();
    let selected = root.path().join("selected");
    let memory = selected.join("memories/MEMORY.md");

    for (apply, expected) in [(false, "would import"), (true, "imported")] {
        let mut command = isolated_hermes(root.path());
        command
            .arg("--config-dir")
            .arg(&selected)
            .args(["import-agent", "codex", "--source"])
            .arg(&source);
        if apply {
            command.arg("--apply");
        }
        let output = tokio::time::timeout(Duration::from_secs(15), command.output())
            .await
            .expect("import-agent timed out")
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.lines().any(|line| {
            line.starts_with(expected) && line.contains("AGENTS.md: 1 of 1 entries")
        }));
        assert_eq!(memory.is_file(), apply);
    }
    assert!(std::fs::read_to_string(memory)
        .unwrap()
        .contains("Verify fixtures before publishing."));
    assert_eq!(
        std::fs::read_to_string(source.join("AGENTS.md")).unwrap(),
        guidance
    );
    assert!(!root.path().join("hermes/memories/MEMORY.md").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn offline_slo_violations_reach_the_exit_code_without_rolling_back_in_dry_run() {
    let root = tempfile::tempdir().unwrap();
    for (check, exit_code, violated) in [("exit 0", 0, false), ("exit 7", 1, true)] {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(root.path())
                .args([
                    "slo-rollback",
                    "--check-cmd",
                    check,
                    "--rollback-cmd",
                    "echo ran > rollback-marker",
                    "--dry-run",
                    "--report-path",
                    "slo.json",
                    "--json",
                ])
                .output(),
        )
        .await
        .expect("slo-rollback timed out")
        .unwrap();
        assert_eq!(output.status.code(), Some(exit_code));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["violated"], violated);
        assert_eq!(report["check"]["exit_code"], if violated { 7 } else { 0 });
        if violated {
            assert_eq!(report["rollback"]["reason"], "dry_run");
        }
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("slo.json")).unwrap()).unwrap();
        assert_eq!(report, on_disk);
        assert!(!root.path().join("rollback-marker").exists());
    }
}
