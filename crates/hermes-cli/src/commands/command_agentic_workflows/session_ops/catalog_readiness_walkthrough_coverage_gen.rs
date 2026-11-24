use super::*;
use std::fs;

const CHILD_CASE: &str = "HERMES_CLI_CATALOG_READINESS_TEST_CASE";

fn run_in_test_home(case: &str, boot_profile: Option<&str>) {
    let home = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let test_name = format!("{module}::isolated_catalog_readiness_case");

    // Re-exec the test harness so home and profile overrides cannot race other tests.
    let mut command = std::process::Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
    command
        .args(["--ignored", "--exact", &test_name, "--nocapture"])
        .env(CHILD_CASE, case)
        .env("HERMES_HOME", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("HERMES_OAUTH_GATE_MANIFEST_PATH")
        .env_remove("HERMES_BOOT_PROFILE");
    if let Some(profile) = boot_profile {
        command.env("HERMES_BOOT_PROFILE", profile);
    }

    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test_name} ({case}, {boot_profile:?}) returned {}:\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn boot_profile_aliases_map_to_canonical_names() {
    for (arg, profile, canonical) in [
        ("  DeV ", BootProfile::Dev, "dev"),
        ("STANDARD", BootProfile::Standard, "standard"),
        ("balanced", BootProfile::Standard, "standard"),
        ("default", BootProfile::Standard, "standard"),
        ("prod", BootProfile::Prod, "prod"),
        (" Production ", BootProfile::Prod, "prod"),
        ("strict", BootProfile::Prod, "prod"),
    ] {
        assert_eq!(BootProfile::parse(arg), Some(profile), "{arg:?}");
        assert_eq!(profile.as_str(), canonical);
    }
    for arg in ["", "  ", "development", "prod strict"] {
        assert_eq!(BootProfile::parse(arg), None, "{arg:?}");
    }
}

#[test]
fn boot_profiles_differ_on_warnings_but_all_block_failures() {
    for (profile, warn_only) in [
        (BootProfile::Dev, "PASS"),
        (BootProfile::Standard, "WARN"),
        (BootProfile::Prod, "FAIL"),
    ] {
        for (fail, warn, expected) in [
            (0, 0, "PASS"),
            (0, 2, warn_only),
            (1, 0, "FAIL"),
            (1, 2, "FAIL"),
        ] {
            assert_eq!(
                boot_profile_overall(profile, fail, warn),
                expected,
                "{profile:?} with fail={fail}, warn={warn}",
            );
        }
    }
}

#[test]
fn readiness_report_groups_checks_and_applies_the_selected_profile() {
    for profile in [None, Some("unrecognized"), Some(" DeV "), Some("strict")] {
        run_in_test_home("report", profile);
    }
}

#[test]
fn malformed_default_oauth_manifest_falls_back_to_builtin() {
    run_in_test_home("manifest", None);
}

#[test]
fn walkthrough_persistence_and_insights_follow_recorded_progress() {
    run_in_test_home("walkthrough", None);
}

#[test]
#[ignore = "invoked in a subprocess with a temporary Hermes home"]
fn isolated_catalog_readiness_case() {
    let Ok(case) = std::env::var(CHILD_CASE) else {
        return;
    };
    let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
    assert_eq!(hermes_config::hermes_home(), home);

    match case.as_str() {
        "report" => check_readiness_report(),
        "manifest" => check_manifest_fallback(&home),
        "walkthrough" => check_walkthrough_files(&home),
        _ => panic!("unexpected isolated test case: {case}"),
    }
}

fn check_readiness_report() {
    let (profile, warning_result) = match std::env::var("HERMES_BOOT_PROFILE").as_deref() {
        Ok(" DeV ") => ("dev", "PASS"),
        Ok("strict") => ("prod", "FAIL"),
        _ => ("standard", "WARN"),
    };
    assert_eq!(boot_profile_env().as_str(), profile);

    let checks = [
        ReadinessCheck {
            name: "Auth".into(),
            state: ReadinessState::Fail,
            detail: "credential missing".into(),
            remediation: "verify auth".into(),
        },
        ReadinessCheck {
            name: "Config".into(),
            state: ReadinessState::Warn,
            detail: "file missing".into(),
            remediation: "run setup".into(),
        },
        ReadinessCheck {
            name: "Hermes home".into(),
            state: ReadinessState::Pass,
            detail: "directory exists".into(),
            remediation: "none".into(),
        },
    ];
    let full = render_boot_readiness_report(&checks, false);
    assert!(full.starts_with(&format!(
        "Boot readiness gate (full)\n==========================\n\
         summary: pass=1 warn=1 fail=1\nprofile: {profile}\noverall: FAIL\n\n"
    )));
    assert_eq!(full.contains("prod_policy:"), profile == "prod");
    assert!(!full.contains("dev_policy:"));
    assert!(full.ends_with(concat!(
        "PASS:\n  - [PASS] Hermes home :: directory exists\n",
        "      remediation: none\n\n",
        "WARN:\n  - [WARN] Config :: file missing\n",
        "      remediation: run setup\n\n",
        "FAIL:\n  - [FAIL] Auth :: credential missing\n",
        "      remediation: verify auth\n\n",
        "Next actions:\n",
        "- `/auth verify`\n",
        "- `/model`\n",
        "- `/integrations status`\n",
        "- `/walkthrough start quick`\n",
    )));

    let quick = render_boot_readiness_report(&checks[1..2], true);
    assert!(quick.starts_with("Boot readiness gate (quick)\n"));
    assert!(quick.contains("summary: pass=0 warn=1 fail=0\n"));
    assert!(quick.contains(&format!("overall: {warning_result}\n")));
    assert!(!quick.lines().any(|line| matches!(line, "PASS:" | "FAIL:")));
    assert_eq!(
        quick.contains("dev_policy: warnings surfaced but do not block overall PASS."),
        profile == "dev",
    );
    assert_eq!(
        quick.contains("prod_policy: warnings are treated as launch blockers."),
        profile == "prod",
    );

    let clean = render_boot_readiness_report(&checks[2..], true);
    assert!(clean.contains("overall: PASS\n"));
    assert!(!clean.contains("_policy:"));
}

fn check_manifest_fallback(home: &std::path::Path) {
    let path = home.join("oauth-gate-manifest.json");
    assert_eq!(oauth_runtime_gate_manifest_path(), None);
    fs::write(&path, "{invalid json").unwrap();
    assert_eq!(
        oauth_runtime_gate_manifest_path().as_deref(),
        Some(path.as_path())
    );
    let (_, source) = load_oauth_runtime_gate_manifest();
    assert_eq!(source, "builtin-default");
}

fn check_walkthrough_files(home: &std::path::Path) {
    let state_path = home.join("walkthrough").join("state.json");
    let events_path = home.join("walkthrough").join("events.jsonl");
    assert_eq!(walkthrough_state_path(), state_path);
    assert_eq!(walkthrough_events_path(), events_path);
    assert!(load_walkthrough_events(10).is_empty());

    let fresh = load_walkthrough_state();
    assert!(fresh.mode.is_empty());
    assert_eq!(fresh.current_step, 0);
    assert!(fresh.completed_steps.is_empty());

    let status = render_walkthrough_status(&fresh);
    assert!(status.starts_with("Walkthrough (quick)\n"));
    assert!(status.lines().any(|line| line.starts_with("→ boot-gate ")));
    assert!(status.contains("    cmd: /boot quick\n"));
    assert!(!status.contains("boot-full"));

    let insights = render_walkthrough_insights(&fresh);
    assert!(insights.contains("events: 0\nactive_mode: quick\n"));
    assert!(insights.contains("starts: none\ndropoff: no completed steps yet\n"));
    assert!(insights.contains("resume_hint: Run /boot quick (boot-gate)\n"));
    assert!(!insights.contains("last_event_at:"));

    let full = WalkthroughState {
        mode: "full".into(),
        current_step: 1,
        completed_steps: vec!["BOOT-FULL".into()],
        updated_at: "2025-01-01T00:00:00Z".into(),
    };
    save_walkthrough_state(&full).unwrap();
    assert!(state_path.is_file());
    let restored = load_walkthrough_state();
    assert_eq!(restored.mode, full.mode);
    assert_eq!(restored.current_step, full.current_step);
    assert_eq!(restored.completed_steps, full.completed_steps);
    assert_eq!(restored.updated_at, full.updated_at);
    assert_eq!(walkthrough_steps_for_mode("FuLl")[0].id, "boot-full");

    let status = render_walkthrough_status(&restored);
    assert!(status.lines().any(|line| line.starts_with("✓ boot-full ")));
    assert!(status
        .lines()
        .any(|line| line.starts_with("→ commands-catalog ")));

    append_walkthrough_event("session-a", "start", &fresh, None).unwrap();
    let raw = fs::read_to_string(&events_path).unwrap();
    fs::write(&events_path, format!("{raw}invalid event\n")).unwrap();
    for _ in 0..2 {
        append_walkthrough_event("session-b", "start", &full, None).unwrap();
    }
    append_walkthrough_event("session-b", "done", &full, Some("boot-full")).unwrap();

    let all = load_walkthrough_events(10);
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].session_id, "session-a");
    assert_eq!(all[0].mode, "quick");
    assert_eq!(all[0].step_id, None);

    let tail = load_walkthrough_events(2);
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].action, "start");
    assert_eq!(tail[0].mode, "full");
    assert_eq!(tail[1].action, "done");
    assert_eq!(tail[1].session_id, "session-b");
    assert_eq!(tail[1].step_id.as_deref(), Some("boot-full"));
    assert_eq!(tail[1].current_step, 1);
    assert_eq!(tail[1].completed_count, 1);
    assert!(load_walkthrough_events(0).is_empty());

    let insights = render_walkthrough_insights(&restored);
    assert!(insights.contains("events: 4\nactive_mode: full\n"));
    assert!(insights.contains("starts:\n- full => 2\n- quick => 1\n"));
    assert!(insights.contains("step_completions:\n- boot-full => 1\n- commands-catalog => 0\n"));
    assert!(insights.contains("resume_hint: Run /commands (commands-catalog)\n"));
    assert!(!tail[1].at.is_empty());
    assert!(insights.ends_with(&format!("last_event_at: {}\n", tail[1].at)));

    let steps = walkthrough_steps_for_mode("full");
    let finished = WalkthroughState {
        current_step: steps.len(),
        completed_steps: steps.iter().map(|step| step.id.to_string()).collect(),
        ..restored
    };
    assert!(render_walkthrough_insights(&finished)
        .contains("resume_hint: Walkthrough complete. Start full mode for deeper checks.\n"));
}
