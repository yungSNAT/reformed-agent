use super::*;
use std::fs;

fn sandbox_home(test_name: &str, mode: &str) -> Option<PathBuf> {
    const MARKER: &str = "HERMES_TRIAGE_COVERAGE_TEST_HOME";

    // Run home-dependent cases in a separate test process so parallel tests
    // cannot see these environment overrides.
    if let Some(home) = std::env::var_os(MARKER) {
        let home = PathBuf::from(home);
        assert_eq!(hermes_config::hermes_home(), home);
        return Some(home);
    }

    let home = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap()) // nosemgrep: rust.lang.security.current-exe.current-exe
        .arg("--exact")
        .arg(format!("{module}::{test_name}"))
        .args(["--nocapture", "--color", "never"])
        .current_dir(home.path())
        .env(MARKER, home.path())
        .env("HERMES_HOME", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("XDG_DATA_HOME", home.path())
        .env("XDG_STATE_HOME", home.path())
        .env("HERMES_TRIGGER_TRIAGE_MODE", mode)
        .env_remove("HERMES_SUBCONSCIOUS_PROFILE")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "sandboxed test {test_name}: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

fn feedback_entry(
    source: &str,
    outcome: &str,
    delta: i32,
    note: &str,
) -> TriggerTriageLearningEntry {
    TriggerTriageLearningEntry {
        at: "2026-01-01T00:00:00Z".to_owned(),
        source: source.to_owned(),
        outcome: outcome.to_owned(),
        decision: "notify".to_owned(),
        severity: 2,
        bias_delta: delta,
        note: note.to_owned(),
    }
}

#[test]
fn feedback_aliases_and_decision_labels() {
    let cases: &[(&[&str], Option<i32>)] = &[
        (
            &["critical", "escalate", "confirmed", "true-positive", "tp"],
            Some(2),
        ),
        (&["useful", "good", "notify", "watch"], Some(1)),
        (&["neutral", "mixed"], Some(0)),
        (&["false-positive", "fp", "noise", "noisy"], Some(-2)),
        (&["drop", "ignore", "spam"], Some(-1)),
        (&["", "unrecognized"], None),
    ];
    for &(aliases, expected) in cases {
        for alias in aliases {
            assert_eq!(
                triage_feedback_delta(&format!("  {}\t", alias.to_ascii_uppercase())),
                expected,
                "feedback alias {alias:?}"
            );
        }
    }

    for (decision, label) in [
        (TriggerTriageDecision::Drop, "drop"),
        (TriggerTriageDecision::Notify, "notify"),
        (TriggerTriageDecision::Escalate, "escalate"),
        (TriggerTriageDecision::AgentRun, "agent-run"),
    ] {
        assert_eq!(decision.as_str(), label);
    }
}

#[test]
fn triage_thresholds_and_assessment_output() {
    let Some(_home) = sandbox_home("triage_thresholds_and_assessment_output", " \t ") else {
        return;
    };

    assert_eq!(trigger_triage_mode(), "off");
    for (source, payload, severity, decision, approval) in [
        ("  CRON  ", " ok ", 1, TriggerTriageDecision::Drop, false),
        ("cron", "timeout", 2, TriggerTriageDecision::Notify, false),
        ("worker", "123456", 0, TriggerTriageDecision::Notify, false),
        ("worker", "panic", 4, TriggerTriageDecision::AgentRun, false),
        (
            "cron",
            "panic error",
            7,
            TriggerTriageDecision::Escalate,
            true,
        ),
    ] {
        let assessment = evaluate_trigger_triage(source, payload);
        assert_eq!(assessment.source, source.trim());
        assert_eq!(assessment.payload, payload.trim());
        assert_eq!(assessment.severity, severity, "{source}: {payload}");
        assert_eq!(assessment.decision, decision, "{source}: {payload}");
        assert_eq!(assessment.requires_approval, approval);
    }

    let rendered =
        render_trigger_triage_assessment(&evaluate_trigger_triage("cron", "panic error"));
    assert_eq!(
        rendered,
        concat!(
            "Trigger triage assessment\n",
            "------------------------\n",
            "source: cron\n",
            "payload: panic error\n",
            "severity: 7\n",
            "decision: escalate\n",
            "requires_approval: true\n",
            "reasons:\n",
            "- runtime panic or crash\n",
            "- error signal\n",
            "- scheduled trigger\n",
        )
    );

    let prefix = "é".repeat(225);
    let payload = format!("{prefix}UNRENDERED");
    let rendered = render_trigger_triage_assessment(&evaluate_trigger_triage("worker", &payload));
    assert!(rendered.contains(&format!("payload: {}", "é".repeat(200))));
    assert!(!rendered.contains(&prefix));
    assert!(!rendered.contains("UNRENDERED"));
    assert!(rendered.ends_with("decision: notify\nrequires_approval: false\nreasons: none\n"));
}

#[test]
fn relaxed_mode_and_subconscious_scoring() {
    let Some(_home) = sandbox_home("relaxed_mode_and_subconscious_scoring", "  ReLaXeD  ") else {
        return;
    };

    assert_eq!(trigger_triage_mode(), "relaxed");
    let assessment = evaluate_trigger_triage(" worker ", " PANIC ");
    assert_eq!(assessment.severity, 3);
    assert_eq!(assessment.decision, TriggerTriageDecision::Notify);
    assert!(!assessment.requires_approval);
    assert_eq!(assessment.reasons, ["runtime panic or crash"]);

    let quiet = evaluate_trigger_triage("worker", "ok");
    assert_eq!(quiet.severity, -1);
    assert_eq!(quiet.decision, TriggerTriageDecision::Drop);

    // Include every available contract term without assuming the sandbox
    // comes with a particular default contract.
    let mut prompt = "VERIFY WALLET".to_owned();
    let mut overlap = 0.0_f64;
    if let Ok(terms) = utility_terms_from_contract() {
        for (term, weight) in terms {
            prompt.push(' ');
            prompt.push_str(&term);
            overlap += weight.max(0.0);
        }
    }
    let expected = 3.0 + overlap.min(2.5);
    assert!((score_subconscious_task(&prompt) - expected).abs() < 1e-10);
    assert!((score_subconscious_task(&prompt.to_ascii_lowercase()) - expected).abs() < 1e-10);
}

#[test]
fn feedback_persistence_evicts_old_entries_and_ranks_sources() {
    let Some(home) = sandbox_home(
        "feedback_persistence_evicts_old_entries_and_ranks_sources",
        "off",
    ) else {
        return;
    };

    let path = home.join("triage").join("learning.json");
    let empty = render_trigger_triage_learning_status();
    assert!(empty.contains("entries: 0\nsource_bias: none\n"));
    assert!(!empty.contains("last_feedback:"));

    let assessment = evaluate_trigger_triage("  WEBHOOK  ", "panic");
    assert_eq!(assessment.severity, 5);
    assert_eq!(assessment.decision, TriggerTriageDecision::AgentRun);
    assert!(matches!(
        append_triage_learning_feedback("webhook", "panic", "bogus", &assessment),
        Err(AgentError::Config(message)) if message.contains("Unknown triage feedback outcome")
    ));
    assert!(!path.exists());

    let state = TriggerTriageLearningState {
        entries: (0..400)
            .map(|i| feedback_entry(&format!("source-{i}"), "neutral", 0, ""))
            .collect(),
    };
    save_trigger_triage_learning_state(&state).unwrap();
    assert!(path.is_file());

    let entry = append_triage_learning_feedback(
        "  WEBHOOK  ",
        " one   two\tthree four five six seven eight nine ten eleven ",
        "  WATCH  ",
        &assessment,
    )
    .unwrap();
    assert_eq!(entry.source, "webhook");
    assert_eq!(entry.outcome, "watch");
    assert_eq!(entry.decision, "agent-run");
    assert_eq!(entry.severity, 5);
    assert_eq!(entry.bias_delta, 1);
    assert_eq!(
        entry.note,
        "one two three four five six seven eight nine ten"
    );

    let persisted = load_trigger_triage_learning_state();
    assert_eq!(persisted.entries.len(), 400);
    assert_eq!(persisted.entries[0].source, "source-1");
    let last = persisted.entries.last().unwrap();
    assert_eq!(last.at, entry.at);
    assert_eq!(last.note, entry.note);

    let status = render_trigger_triage_learning_status();
    assert!(status.contains("entries: 400\nsource_bias:\n- webhook => +1\n"));
    assert_eq!(
        status.lines().filter(|line| line.starts_with("- ")).count(),
        6
    );
    assert!(status.ends_with(&format!(
        "last_feedback: {} source=webhook outcome=watch delta=+1\n",
        entry.at
    )));

    let learned = evaluate_trigger_triage("webhook", "panic");
    assert_eq!(learned.severity, 6);
    assert_eq!(learned.decision, TriggerTriageDecision::AgentRun);
    assert!(learned.reasons.ends_with(&[
        "learning bias applied (+1)".to_owned(),
        "source feedback watch (1)".to_owned(),
    ]));
}

#[test]
fn learning_bias_limits_history_and_matches_notes_across_sources() {
    let Some(_home) = sandbox_home(
        "learning_bias_limits_history_and_matches_notes_across_sources",
        "off",
    ) else {
        return;
    };

    let mut entries = vec![feedback_entry("sensor", "critical", 2, "")];
    entries.extend((0..111).map(|_| feedback_entry("bystander", "useful", 1, " \t ")));
    entries.extend([
        feedback_entry("negative", "fp", -2, ""),
        feedback_entry("negative", "noise", -2, ""),
        feedback_entry("positive", "confirmed", 2, ""),
        feedback_entry("positive", "confirmed", 2, ""),
        feedback_entry("other", "useful", 1, "spool"),
        feedback_entry("sensor", "drop", -1, ""),
        feedback_entry("other", "fp", -2, "Spool"),
        feedback_entry("other", "confirmed", 2, " OverHeat "),
        feedback_entry("SeNsOr", "useful", 1, "spool"),
    ]);
    save_trigger_triage_learning_state(&TriggerTriageLearningState { entries }).unwrap();

    let (bias, reasons) = triage_learning_bias("  SENSOR  ", "  SPOOL and OVERHEAT  ");
    assert_eq!(bias, 1);
    assert_eq!(
        reasons,
        [
            "source feedback useful (1)",
            "matched prior note ' OverHeat '",
            "matched prior note 'Spool'",
        ]
    );
    assert_eq!(triage_learning_bias("sensor", "unrelated").0, 0);
    assert_eq!(triage_learning_bias("negative", "unrelated").0, -3);
    assert_eq!(triage_learning_bias("positive", "unrelated").0, 3);
}

include!("triage_background_part2_coverage_gen.rs");
