use serde_json::json;

use super::*;

#[tokio::test]
async fn studio_verifies_replay_logs_and_diffs_export_hash_sets() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::evidence_reports_contract::studio_verifies_replay_logs_and_diffs_export_hash_sets",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let replay_dir = home.join("logs/replay");
    let log = replay_dir.join(format!("{}.jsonl", app.session_id));
    let status = command_output(&mut app, "/studio", &["replay"]).await;
    assert!(
        status.contains(&format!("session={}", app.session_id)),
        "{status}"
    );
    assert!(
        status.contains(&format!("replay_log={}", log.display())),
        "{status}"
    );
    assert!(status.contains("replay_exists=false"), "{status}");
    assert_eq!(
        command_output(&mut app, "/studio", &["replay", "verify"]).await,
        format!("Replay file not found: {}", log.display())
    );
    std::fs::create_dir_all(&replay_dir).unwrap();
    let first = json!({"seq":1,"event":"start","prev_hash":"root","event_hash":"h1"});
    let second = json!({"seq":2,"event":"end","prev_hash":"h1","event_hash":"h2"});
    std::fs::write(&log, format!("{first}\n{second}\n")).unwrap();
    let status = command_output(&mut app, "/studio", &["replay", "verify"]).await;
    assert!(
        status.ends_with("entries=2\nparse_errors=0\nchain_breaks=0\nstatus=pass"),
        "{status}"
    );
    let a = home.join("export-a.json");
    let b = home.join("export-b.json");
    let a_arg = a.to_str().unwrap();
    let b_arg = b.to_str().unwrap();
    let output = command_output(&mut app, "/raw", &["trace", "export", "2", a_arg]).await;
    assert!(output.contains("rows: 2"), "{output}");
    std::fs::write(
        &b,
        json!({"rows":[second.clone(),second,{"event_hash":"h3"},{"event_hash":3}]}).to_string(),
    )
    .unwrap();
    assert_eq!(
        command_output(&mut app, "/studio", &["replay", "verify", b_arg]).await,
        format!("Replay export verification\npath={b_arg}\nrows=4\nstatus=ok")
    );
    let diff = command_output(&mut app, "/studio", &["replay", "diff", a_arg, b_arg]).await;
    assert_eq!(
        diff,
        format!("Replay diff\nA={a_arg} rows=2 hashes=2\nB={b_arg} rows=4 hashes=2\noverlap_hashes=1\nonly_in_a=1\nonly_in_b=1")
    );
    std::fs::write(&b, json!({"rows":[]}).to_string()).unwrap();
    let empty = command_output(&mut app, "/studio", &["replay", "verify", b_arg]).await;
    assert!(empty.ends_with("rows=0\nstatus=empty"), "{empty}");
    std::fs::write(
        &log,
        format!(
            "{first}\ninvalid row\n{}\n",
            json!({"prev_hash":"wrong","event_hash":"h2"})
        ),
    )
    .unwrap();
    let status = command_output(&mut app, "/studio", &["replay", "verify"]).await;
    assert!(
        status.ends_with("entries=2\nparse_errors=1\nchain_breaks=1\nstatus=fail"),
        "{status}"
    );
    let usage = command_output(&mut app, "/studio", &["replay", "diff", a_arg]).await;
    assert_eq!(
        usage,
        "Usage: /studio replay diff <export_a.json> <export_b.json>"
    );
}

#[tokio::test]
async fn heatmap_counts_git_files_and_ranks_transcript_and_ledger_evidence() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::evidence_reports_contract::heatmap_counts_git_files_and_ranks_transcript_and_ledger_evidence",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let repo = home.join("repo");
    let repo_arg = repo.to_str().unwrap();
    assert_eq!(
        command_output(&mut app, "/heatmap", &[repo_arg]).await,
        format!("Repo path does not exist: {repo_arg}")
    );
    std::fs::create_dir_all(&repo).unwrap();
    std::env::set_var("GIT_CONFIG_GLOBAL", home.join("unused-gitconfig"));
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    std::env::set_var("GIT_ATTR_NOSYSTEM", "1");
    std::env::set_var("GIT_ALLOW_PROTOCOL", "file");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "--template="]);
    for file in [
        "covered.rs",
        "untouched-a.rs",
        "untouched-b.rs",
        "also-covered.rs",
    ] {
        std::fs::write(repo.join(file), "Synthetic source").unwrap();
    }
    git(&[
        "add",
        "--",
        "covered.rs",
        "untouched-a.rs",
        "untouched-b.rs",
        "also-covered.rs",
    ]);
    let (tracked, percent) = if hermes_core::git::read_only_git(&repo).is_some() {
        (4, "50.00")
    } else {
        (0, "0.00")
    };
    let empty = command_output(&mut app, "/heatmap", &[repo_arg]).await;
    assert!(
        empty.contains(&format!("tracked_files={tracked}")),
        "{empty}"
    );
    assert!(
        empty.ends_with("- no evidence paths recorded yet"),
        "{empty}"
    );

    let ledger = home.join("alpha/objective_runtime_ledger.jsonl");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(
        ledger,
        format!(
            "invalid row\n{}\n",
            json!({"evidence_files":[repo.join("covered.rs"),"also-covered.rs","absent.rs",home.join("outside.rs"),12]})
        ),
    )
    .unwrap();
    app.messages.push(hermes_core::Message::assistant(
        "Reviewed path=covered.rs and file=absent.rs; confirmed path=covered.rs",
    ));
    std::env::set_var("HERMES_REPO_ROOT", &repo);
    let report = command_output(&mut app, "/heatmap", &[]).await;
    assert!(report.contains(&format!("repo_root={repo_arg}\ntracked_files={tracked}\nobserved_paths=3\nverified_existing_paths=2 ({percent}% coverage of tracked files)")), "{report}");
    let evidence = report
        .lines()
        .filter(|line| line.starts_with("- hits="))
        .collect::<Vec<_>>();
    assert_eq!(
        evidence,
        [
            "- hits=3    exists=true path=covered.rs",
            "- hits=2    exists=false path=absent.rs",
            "- hits=1    exists=true path=also-covered.rs",
        ]
    );
}
