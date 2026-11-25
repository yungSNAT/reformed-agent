use super::*;

fn dict(pairs: Vec<(&'static str, Py)>) -> Py {
    Py::dict(pairs)
}

fn metrics_of(pairs: &[(&'static str, f64)]) -> Metrics {
    pairs.iter().map(|(k, v)| ((*k), *v)).collect()
}

fn bare_gate(pass: bool) -> Gate {
    Gate {
        pass,
        mode: "legacy".into(),
        checks: Vec::new(),
        applied_rule: None,
    }
}

fn find_check<'a>(gate: &'a Gate, metric: &str) -> &'a Check {
    gate.checks
        .iter()
        .find(|c| c.metric == metric)
        .expect("check present")
}

fn has_detail(check: &Check, key: &str, value: &str) -> bool {
    check
        .details
        .iter()
        .any(|(k, v)| *k == key && py::str_of(v) == value)
}

fn functional_clean() -> Vec<(&'static str, f64)> {
    vec![
        ("min_test_coverage_tracked_behavior_ratio", 1.0),
        ("min_sota_harness_domain_coverage_ratio", 1.0),
        ("min_behavioral_similarity_ratio", 1.0),
        ("min_deep_problem_solving_ratio", 1.0),
    ]
}

#[test]
fn evaluate_flat_thresholds_statuses_and_mode() {
    let metrics = metrics_of(&[("max_a", 3.0), ("min_b", 0.2)]);
    let thresholds = dict(vec![
        ("max_a", Py::from(5.0)),
        ("min_b", Py::from(0.5)),
        ("max_missing", Py::from(1.0)),
        ("gate_mode", Py::from(" Strict ")),
        (
            "required_workstreams_complete",
            Py::List(vec![Py::from("x")]),
        ),
        ("special_rules", dict(vec![])),
    ]);
    let gate = Gate::evaluate(&metrics, &thresholds).unwrap();
    assert_eq!(gate.checks.len(), 3);
    assert_eq!(find_check(&gate, "max_a").status, "pass");
    assert_eq!(find_check(&gate, "min_b").status, "fail");
    assert_eq!(find_check(&gate, "max_missing").status, "missing");
    assert!(!gate.pass);
    assert_eq!(gate.mode, "strict");
    assert!(gate.applied_rule.is_none());
}

#[test]
fn evaluate_nested_thresholds_defaults_to_legacy() {
    let metrics = metrics_of(&[("max_a", 3.0), ("min_b", 0.7)]);
    let thresholds = dict(vec![(
        "metric_thresholds",
        dict(vec![("max_a", Py::from(5.0)), ("min_b", Py::from(0.5))]),
    )]);
    let gate = Gate::evaluate(&metrics, &thresholds).unwrap();
    assert_eq!(gate.checks.len(), 2);
    assert!(gate.pass);
    assert_eq!(gate.mode, "legacy");
    assert_eq!(number(&find_check(&gate, "max_a").actual).unwrap(), 3.0);
}

#[test]
fn require_workstreams_fails_when_incomplete() {
    let mut gate = bare_gate(true);
    let thresholds = dict(vec![(
        "required_workstreams_complete",
        Py::List(vec![Py::from("a"), Py::from("b")]),
    )]);
    let completion = dict(vec![("a", Py::from(true)), ("b", Py::from(false))]);
    gate.require_workstreams(&thresholds, &completion).unwrap();
    assert!(!gate.pass);
    let check = find_check(&gate, "required_workstreams_complete");
    assert_eq!(check.status, "fail");
    assert!(truth(check.actual.get("a").unwrap()));
    assert!(!truth(check.actual.get("b").unwrap()));
    assert_eq!(py::str_of(&check.limit), "all true");
}

#[test]
fn require_workstreams_passes_without_requirements() {
    let mut gate = bare_gate(true);
    gate.require_workstreams(&dict(vec![]), &dict(vec![]))
        .unwrap();
    assert!(gate.pass);
    assert_eq!(
        find_check(&gate, "required_workstreams_complete").status,
        "pass"
    );
}

#[test]
fn require_workstreams_rejects_non_strings() {
    let mut gate = bare_gate(true);
    let thresholds = dict(vec![(
        "required_workstreams_complete",
        Py::List(vec![Py::from(1.0)]),
    )]);
    let err = gate
        .require_workstreams(&thresholds, &dict(vec![]))
        .err()
        .unwrap();
    assert_eq!(err, "required workstreams must be strings");
}

#[test]
fn into_report_without_rule() {
    let metrics = metrics_of(&[("max_a", 3.0)]);
    let thresholds = dict(vec![("max_a", Py::from(5.0))]);
    let gate = Gate::evaluate(&metrics, &thresholds).unwrap();
    let report = gate.into_report();
    assert!(truth(report.get("pass").unwrap()));
    assert_eq!(py::str_of(report.get("mode").unwrap()), "legacy");
    assert!(report.get("special_rules_applied").is_none());
    match report.get("checks") {
        Some(Py::List(list)) => {
            assert_eq!(list.len(), 1);
            assert_eq!(py::str_of(list[0].get("metric").unwrap()), "max_a");
            assert_eq!(py::str_of(list[0].get("status").unwrap()), "pass");
            assert_eq!(number(list[0].get("limit").unwrap()).unwrap(), 5.0);
        }
        _ => panic!("checks should be a list"),
    }
}

#[test]
fn apply_ci_rules_without_special_rules_is_noop() {
    let metrics = metrics_of(&[("max_a", 9.0)]);
    let thresholds = dict(vec![("max_a", Py::from(5.0))]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    apply_ci_rules(&metrics, &mut ci, &bare_gate(true), &thresholds).unwrap();
    assert!(!ci.pass);
    assert!(ci.applied_rule.is_none());
}

#[test]
fn tree_drift_rule_applies_with_defaults() {
    let mut pairs = functional_clean();
    pairs.push(("max_commits_behind", 5.0));
    let metrics = metrics_of(&pairs);
    let thresholds = dict(vec![
        ("max_commits_behind", Py::from(0.0)),
        (
            "special_rules",
            dict(vec![(TREE_DRIFT, dict(vec![("enabled", Py::from(true))]))]),
        ),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    assert!(!ci.pass);
    apply_ci_rules(&metrics, &mut ci, &bare_gate(true), &thresholds).unwrap();
    assert!(ci.pass);
    let check = find_check(&ci, "max_commits_behind");
    assert_eq!(check.status, "warn");
    assert!(has_detail(check, "special_rule", TREE_DRIFT));
    let report = ci.into_report();
    match report.get("special_rules_applied") {
        Some(Py::List(list)) => {
            assert_eq!(list.len(), 1);
            assert_eq!(py::str_of(list[0].get("rule").unwrap()), TREE_DRIFT);
        }
        _ => panic!("rule should be reported"),
    }
}

#[test]
fn tree_drift_rule_custom_metrics_and_release_permit() {
    let mut pairs = functional_clean();
    pairs.push(("max_files_only_upstream", 20.0));
    pairs.push(("max_queue_pending_commits", 3.0));
    let metrics = metrics_of(&pairs);
    let thresholds = dict(vec![
        ("max_files_only_upstream", Py::from(10.0)),
        (
            "special_rules",
            dict(vec![(
                TREE_DRIFT,
                dict(vec![
                    ("enabled", Py::from(true)),
                    (
                        "metrics",
                        Py::List(vec![Py::from(" max_files_only_upstream "), Py::from("")]),
                    ),
                    ("requires_release_gate_pass", Py::from(false)),
                    ("requires_max_queue_pending_commits", Py::from(5.0)),
                ]),
            )]),
        ),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    apply_ci_rules(&metrics, &mut ci, &bare_gate(false), &thresholds).unwrap();
    assert!(ci.pass);
    assert_eq!(find_check(&ci, "max_files_only_upstream").status, "warn");
    assert!(ci.applied_rule.is_some());
}

#[test]
fn tree_drift_rule_blocked_by_release_falls_through() {
    let mut pairs = functional_clean();
    pairs.push(("max_commits_behind", 5.0));
    let metrics = metrics_of(&pairs);
    let thresholds = dict(vec![
        ("max_commits_behind", Py::from(0.0)),
        (
            "special_rules",
            dict(vec![
                (TREE_DRIFT, dict(vec![("enabled", Py::from(true))])),
                (FILE_OVERSHOOT, dict(vec![("enabled", Py::from(false))])),
            ]),
        ),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    apply_ci_rules(&metrics, &mut ci, &bare_gate(false), &thresholds).unwrap();
    assert!(!ci.pass);
    assert_eq!(find_check(&ci, "max_commits_behind").status, "fail");
    assert!(ci.applied_rule.is_none());
}

#[test]
fn tree_drift_with_no_failures_and_overshoot_rule_is_noop() {
    let mut pairs = functional_clean();
    pairs.push(("max_commits_behind", 0.0));
    let metrics = metrics_of(&pairs);
    let thresholds = dict(vec![
        ("max_commits_behind", Py::from(0.0)),
        (
            "special_rules",
            dict(vec![
                (TREE_DRIFT, dict(vec![("enabled", Py::from(true))])),
                (FILE_OVERSHOOT, dict(vec![("enabled", Py::from(true))])),
            ]),
        ),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    assert!(ci.pass);
    apply_ci_rules(&metrics, &mut ci, &bare_gate(true), &thresholds).unwrap();
    assert!(ci.pass);
    assert!(ci.applied_rule.is_none());
    assert_eq!(find_check(&ci, "max_commits_behind").status, "pass");
}

fn overshoot_thresholds(config: Vec<(&'static str, Py)>) -> Py {
    dict(vec![
        ("max_files_only_upstream", Py::from(10.0)),
        ("special_rules", dict(vec![(FILE_OVERSHOOT, dict(config))])),
    ])
}

#[test]
fn file_overshoot_rule_applies() {
    let metrics = metrics_of(&[
        ("max_files_only_upstream", 12.0),
        ("min_test_intent_mapping_ratio", 0.95),
    ]);
    let thresholds = overshoot_thresholds(vec![
        ("enabled", Py::from(true)),
        ("max_overshoot", Py::from(5.0)),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    assert!(!ci.pass);
    apply_ci_rules(&metrics, &mut ci, &bare_gate(true), &thresholds).unwrap();
    assert!(ci.pass);
    let check = find_check(&ci, "max_files_only_upstream");
    assert_eq!(check.status, "warn");
    assert!(has_detail(check, "special_rule", FILE_OVERSHOOT));
    let overshoot = check
        .details
        .iter()
        .find(|(k, _)| *k == "overshoot")
        .map(|(_, v)| number(v).unwrap());
    assert_eq!(overshoot, Some(2.0));
    let rule = ci.applied_rule.as_ref().unwrap();
    assert_eq!(py::str_of(rule.get("rule").unwrap()), FILE_OVERSHOOT);
    assert_eq!(number(rule.get("actual").unwrap()).unwrap(), 12.0);
    assert_eq!(number(rule.get("limit").unwrap()).unwrap(), 10.0);
    assert_eq!(number(rule.get("max_overshoot").unwrap()).unwrap(), 5.0);
}

#[test]
fn file_overshoot_rule_with_release_permit() {
    let metrics = metrics_of(&[
        ("max_files_only_upstream", 10.5),
        ("min_test_intent_mapping_ratio", 0.95),
    ]);
    let thresholds = overshoot_thresholds(vec![
        ("enabled", Py::from(true)),
        ("max_overshoot", Py::from(1.0)),
        ("requires_release_gate_pass", Py::from(false)),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    apply_ci_rules(&metrics, &mut ci, &bare_gate(false), &thresholds).unwrap();
    assert!(ci.pass);
    assert_eq!(find_check(&ci, "max_files_only_upstream").status, "warn");
}

#[test]
fn file_overshoot_rule_rejects_too_large_overshoot() {
    let metrics = metrics_of(&[
        ("max_files_only_upstream", 30.0),
        ("min_test_intent_mapping_ratio", 0.95),
    ]);
    let thresholds = overshoot_thresholds(vec![
        ("enabled", Py::from(true)),
        ("max_overshoot", Py::from(5.0)),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    apply_ci_rules(&metrics, &mut ci, &bare_gate(true), &thresholds).unwrap();
    assert!(!ci.pass);
    assert_eq!(find_check(&ci, "max_files_only_upstream").status, "fail");
    assert!(ci.applied_rule.is_none());
}

#[test]
fn file_overshoot_rule_rejects_failed_release() {
    let metrics = metrics_of(&[
        ("max_files_only_upstream", 11.0),
        ("min_test_intent_mapping_ratio", 0.95),
    ]);
    let thresholds = overshoot_thresholds(vec![
        ("enabled", Py::from(true)),
        ("max_overshoot", Py::from(5.0)),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    apply_ci_rules(&metrics, &mut ci, &bare_gate(false), &thresholds).unwrap();
    assert!(!ci.pass);
    assert!(ci.applied_rule.is_none());
}

#[test]
fn file_overshoot_rule_rejects_unmet_limits() {
    let metrics = metrics_of(&[("max_files_only_upstream", 11.0)]);
    let thresholds = overshoot_thresholds(vec![
        ("enabled", Py::from(true)),
        ("max_overshoot", Py::from(5.0)),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    apply_ci_rules(&metrics, &mut ci, &bare_gate(true), &thresholds).unwrap();
    assert!(!ci.pass);
    assert_eq!(find_check(&ci, "max_files_only_upstream").status, "fail");
}

#[test]
fn file_overshoot_rule_ignores_multiple_failures() {
    let metrics = metrics_of(&[
        ("max_files_only_upstream", 11.0),
        ("max_other", 9.0),
        ("min_test_intent_mapping_ratio", 0.95),
    ]);
    let thresholds = dict(vec![
        ("max_files_only_upstream", Py::from(10.0)),
        ("max_other", Py::from(1.0)),
        (
            "special_rules",
            dict(vec![(
                FILE_OVERSHOOT,
                dict(vec![
                    ("enabled", Py::from(true)),
                    ("max_overshoot", Py::from(5.0)),
                ]),
            )]),
        ),
    ]);
    let mut ci = Gate::evaluate(&metrics, &thresholds).unwrap();
    apply_ci_rules(&metrics, &mut ci, &bare_gate(true), &thresholds).unwrap();
    assert!(!ci.pass);
    assert!(ci.applied_rule.is_none());
}

#[test]
fn helper_functions_behave() {
    let config = dict(vec![("requires_max_queue_pending_commits", Py::from(7.0))]);
    let limits = metric_limits(&config, FILE_LIMITS).unwrap();
    assert_eq!(limits, vec![0.0, 0.0, 7.0, 0.9]);

    let good = metrics_of(&[("min_test_intent_mapping_ratio", 0.9)]);
    assert!(matches_limits(&good, FILE_LIMITS, &limits));
    let bad = metrics_of(&[
        ("min_test_intent_mapping_ratio", 0.9),
        ("max_queue_pending_commits", 8.0),
    ]);
    assert!(!matches_limits(&bad, FILE_LIMITS, &limits));

    let rules = dict(vec![
        ("on", dict(vec![("enabled", Py::from(true))])),
        ("off", dict(vec![("enabled", Py::from(false))])),
    ]);
    assert!(enabled_rule(&rules, "on").is_some());
    assert!(enabled_rule(&rules, "off").is_none());
    assert!(enabled_rule(&rules, "absent").is_none());

    assert!(permits_without_release(&dict(vec![(
        "requires_release_gate_pass",
        Py::from(false)
    )])));
    assert!(!permits_without_release(&dict(vec![(
        "requires_release_gate_pass",
        Py::from(true)
    )])));
    assert!(!permits_without_release(&dict(vec![])));
}

#[test]
fn check_warn_and_report() {
    let mut check = Check {
        metric: "m".into(),
        status: "fail",
        actual: Py::from(2.0),
        limit: Py::from(1.0),
        details: Vec::new(),
    };
    check.warn("some_rule");
    assert_eq!(check.status, "warn");
    let report = check.into_report();
    assert_eq!(py::str_of(report.get("status").unwrap()), "warn");
    assert_eq!(py::str_of(report.get("special_rule").unwrap()), "some_rule");
    assert_eq!(py::str_of(report.get("metric").unwrap()), "m");
    assert_eq!(number(report.get("actual").unwrap()).unwrap(), 2.0);
}
