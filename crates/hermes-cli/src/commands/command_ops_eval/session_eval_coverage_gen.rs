use super::*;

fn env_snapshot(keys: &[&str]) -> Vec<(String, Option<String>)> {
    keys.iter()
        .map(|k| (k.to_string(), std::env::var(k).ok()))
        .collect()
}

fn env_restore(snapshot: Vec<(String, Option<String>)>) {
    for (k, v) in snapshot {
        match v {
            Some(val) => std::env::set_var(&k, val),
            None => std::env::remove_var(&k),
        }
    }
}

#[test]
fn skills_execution_tier_parse_and_as_str() {
    if run_isolated("skills_execution_tier_parse_and_as_str") {
        return;
    }

    assert_eq!(
        SkillsExecutionTier::parse("  Trusted "),
        Some(SkillsExecutionTier::Trusted)
    );
    assert_eq!(
        SkillsExecutionTier::parse("BALANCED"),
        Some(SkillsExecutionTier::Balanced)
    );
    assert_eq!(
        SkillsExecutionTier::parse("open"),
        Some(SkillsExecutionTier::Open)
    );
    assert_eq!(
        SkillsExecutionTier::parse("permissive"),
        Some(SkillsExecutionTier::Open)
    );
    assert_eq!(SkillsExecutionTier::parse("nope"), None);

    assert_eq!(SkillsExecutionTier::Trusted.as_str(), "trusted");
    assert_eq!(SkillsExecutionTier::Balanced.as_str(), "balanced");
    assert_eq!(SkillsExecutionTier::Open.as_str(), "open");
}

#[test]
fn skills_tier_env_readers_match_environment() {
    if run_isolated("skills_tier_env_readers_match_environment") {
        return;
    }

    let expected_tier = std::env::var("HERMES_SKILLS_EXECUTION_TIER")
        .ok()
        .as_deref()
        .and_then(SkillsExecutionTier::parse)
        .unwrap_or(SkillsExecutionTier::Balanced);
    assert_eq!(skills_execution_tier(), expected_tier);

    let expected_bypass = std::env::var("HERMES_SKILLS_TIER_BYPASS")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });
    assert_eq!(skills_tier_bypass_enabled(), expected_bypass);
}

#[test]
fn repo_review_budget_profile_parse_and_as_str() {
    if run_isolated("repo_review_budget_profile_parse_and_as_str") {
        return;
    }

    assert_eq!(
        RepoReviewBudgetProfile::parse(" Relaxed "),
        Some(RepoReviewBudgetProfile::Relaxed)
    );
    assert_eq!(
        RepoReviewBudgetProfile::parse("OFF"),
        Some(RepoReviewBudgetProfile::Off)
    );
    assert_eq!(RepoReviewBudgetProfile::parse("unknown-profile"), None);
    assert_eq!(RepoReviewBudgetProfile::Relaxed.as_str(), "relaxed");
    assert_eq!(RepoReviewBudgetProfile::Off.as_str(), "off");
}

#[test]
fn apply_repo_review_budget_profile_relaxed_and_off() {
    if run_isolated("apply_repo_review_budget_profile_relaxed_and_off") {
        return;
    }

    let snapshot = env_snapshot(&[
        REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD,
        REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD,
        REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT,
        REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL,
        REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE,
        REPO_REVIEW_BUDGET_ENV_PROFILE,
    ]);

    apply_repo_review_budget_profile(RepoReviewBudgetProfile::Relaxed);
    let relaxed_profile = std::env::var(REPO_REVIEW_BUDGET_ENV_PROFILE).ok();
    let relaxed_repeat = std::env::var(REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD).ok();
    let relaxed_keep_low = std::env::var(REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL).ok();
    let relaxed_min = std::env::var(REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE).ok();

    apply_repo_review_budget_profile(RepoReviewBudgetProfile::Off);
    let off_profile = std::env::var(REPO_REVIEW_BUDGET_ENV_PROFILE).ok();
    let off_repeat = std::env::var(REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD).ok();
    let off_min = std::env::var(REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE).ok();

    env_restore(snapshot);

    assert_eq!(relaxed_profile.as_deref(), Some("relaxed"));
    assert_eq!(relaxed_repeat.as_deref(), Some("3"));
    assert_eq!(relaxed_keep_low.as_deref(), Some("2"));
    assert_eq!(relaxed_min.as_deref(), Some("0.150"));
    assert_eq!(off_profile.as_deref(), Some("off"));
    assert_eq!(off_repeat.as_deref(), Some("12"));
    assert_eq!(off_min.as_deref(), Some("0.010"));
}

#[test]
fn latest_json_report_skips_non_matching_files() {
    if run_isolated("latest_json_report_skips_non_matching_files") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("gate-a.json"), "{}").unwrap();
    std::fs::write(dir.path().join("gate-b.json"), "{}").unwrap();
    std::fs::write(dir.path().join("gate-c.txt"), "x").unwrap();
    std::fs::write(dir.path().join("other.json"), "{}").unwrap();
    let latest = latest_json_report(dir.path(), "gate-").unwrap();
    assert_eq!(latest, dir.path().join("gate-b.json"));
    assert!(latest_json_report(dir.path(), "missing-").is_none());
}

#[test]
fn summarize_gate_report_formats_status() {
    if run_isolated("summarize_gate_report_formats_status") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pass = dir.path().join("pass.json");
    std::fs::write(
        &pass,
        r#"{"ok":true,"generated_at":"2024-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    assert_eq!(
        summarize_gate_report(&pass, "gate").as_deref(),
        Some("gate=pass @ 2024-01-01T00:00:00Z (pass.json)")
    );

    let fail = dir.path().join("fail.json");
    std::fs::write(&fail, r#"{"ok":false}"#).unwrap();
    assert_eq!(
        summarize_gate_report(&fail, "k").as_deref(),
        Some("k=fail @ unknown (fail.json)")
    );

    let unknown = dir.path().join("unknown.json");
    std::fs::write(&unknown, r#"{"other":1}"#).unwrap();
    assert_eq!(
        summarize_gate_report(&unknown, "k").as_deref(),
        Some("k=unknown @ unknown (unknown.json)")
    );

    assert!(summarize_gate_report(&dir.path().join("absent.json"), "k").is_none());
}

#[test]
fn normalize_session_role_variants() {
    if run_isolated("normalize_session_role_variants") {
        return;
    }

    assert_eq!(normalize_session_role(Some(" System ")), "system");
    assert_eq!(normalize_session_role(Some("TOOL")), "tool");
    assert_eq!(normalize_session_role(Some("bot")), "unknown");
    assert_eq!(normalize_session_role(None), "unknown");
}

#[test]
fn session_stats_and_report_with_system_messages() {
    if run_isolated("session_stats_and_report_with_system_messages") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "not a session").unwrap();
    std::fs::write(
        dir.path().join("s1.json"),
        r#"{"messages":[
            {"role":"system","content":"setup"},
            {"role":"user","content":"hi /objective"},
            {"role":"user","content":"again"}
        ]}"#,
    )
    .unwrap();

    let stats = load_latest_session_eval_stats(dir.path(), 10);
    assert_eq!(stats.len(), 1);
    let s = &stats[0];
    assert_eq!(s.name, "s1");
    assert_eq!(s.message_count, 3);
    assert_eq!(s.user_count, 2);
    assert_eq!(s.assistant_count, 0);
    assert!(s.has_objective_activity);
    assert!(!s.has_tool_activity);

    let report = build_session_eval_report(dir.path(), &stats);
    assert_eq!(report["ok"], serde_json::json!(false));
    let reasons: Vec<String> = report["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(reasons.contains(&"assistant_turns_below_user_turns".to_string()));
    assert!(reasons.contains(&"low_tool_activity_ratio".to_string()));
    assert_eq!(report["summary"]["user_turns"], serde_json::json!(2));
}

#[test]
fn median_usize_even_and_odd() {
    if run_isolated("median_usize_even_and_odd") {
        return;
    }

    assert_eq!(median_usize(&[4, 1, 3, 2]), 2.5);
    assert_eq!(median_usize(&[5, 1, 3]), 3.0);
    assert_eq!(median_usize(&[]), 0.0);
}

#[test]
fn write_json_report_success_and_failure() {
    if run_isolated("write_json_report_success_and_failure") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("r.json");
    write_json_report(&path, &serde_json::json!({"a": 1})).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.ends_with('\n'));
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["a"], serde_json::json!(1));

    let err = write_json_report(std::path::Path::new(""), &serde_json::json!({}));
    assert!(err.is_err());
}

#[test]
fn duration_json_to_secs_variants() {
    if run_isolated("duration_json_to_secs_variants") {
        return;
    }

    assert_eq!(duration_json_to_secs(Some(&serde_json::json!(2.5))), 2.5);
    assert_eq!(duration_json_to_secs(Some(&serde_json::json!(" 3s"))), 3.0);
    assert_eq!(duration_json_to_secs(Some(&serde_json::json!("abc"))), 0.0);
    assert_eq!(duration_json_to_secs(Some(&serde_json::json!(true))), 0.0);
    assert_eq!(duration_json_to_secs(None), 0.0);
    assert_eq!(
        duration_json_to_secs(Some(&serde_json::json!({"secs": 1, "nanos": 500_000_000}))),
        1.5
    );
}

#[test]
fn latest_eval_files_ignores_non_json() {
    if run_isolated("latest_eval_files_ignores_non_json") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.json"), "{}").unwrap();
    std::fs::write(dir.path().join("b.txt"), "x").unwrap();
    let files = latest_eval_files(dir.path());
    assert_eq!(files, vec![dir.path().join("a.json")]);
}

#[test]
fn relative_change_with_zero_baseline() {
    if run_isolated("relative_change_with_zero_baseline") {
        return;
    }

    assert_eq!(relative_change(0.0, 0.0), 0.0);
    assert_eq!(relative_change(-1.0, 0.0), 0.0);
    assert_eq!(relative_change(5.0, 0.0), 1.0);
    assert_eq!(relative_change(3.0, 2.0), 0.5);
}

#[test]
fn eval_trend_gate_errors_on_unreadable_current() {
    if run_isolated("eval_trend_gate_errors_on_unreadable_current") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let current = dir.path().join("current.json");
    let baseline = dir.path().join("baseline.json");
    std::fs::write(&current, "not json").unwrap();
    std::fs::write(&baseline, r#"{"metrics":{"total":1}}"#).unwrap();
    let out = dir.path().join("out.json");
    let result = run_eval_trend_gate_native(
        dir.path(),
        Some(&current),
        Some(&baseline),
        Some(&out),
        EvalTrendGateOptions::default(),
    );
    match result {
        Err(AgentError::Config(msg)) => {
            assert!(msg.contains("read eval report"));
            assert!(msg.contains("current.json"));
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert!(!out.exists());
}

#[test]
fn eval_trend_gate_errors_on_unreadable_baseline() {
    if run_isolated("eval_trend_gate_errors_on_unreadable_baseline") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let current = dir.path().join("current.json");
    let baseline = dir.path().join("baseline.json");
    std::fs::write(&current, r#"{"metrics":{"total":1}}"#).unwrap();
    std::fs::write(&baseline, "{{ broken").unwrap();
    let out = dir.path().join("out.json");
    let result = run_eval_trend_gate_native(
        dir.path(),
        Some(&current),
        Some(&baseline),
        Some(&out),
        EvalTrendGateOptions::default(),
    );
    match result {
        Err(AgentError::Config(msg)) => {
            assert!(msg.contains("read eval report"));
            assert!(msg.contains("baseline.json"));
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn tail_chars_truncates_long_input() {
    if run_isolated("tail_chars_truncates_long_input") {
        return;
    }

    assert_eq!(tail_chars("héllo", 3), "llo");
    assert_eq!(tail_chars("héllo", 4), "éllo");
    assert_eq!(tail_chars("ab", 5), "ab");
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path.split_once("::").map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary")) // nosemgrep: rust.lang.security.current-exe.current-exe
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
        .output()
        .expect("run the isolated test");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}
