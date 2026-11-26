use super::*;

#[test]
fn autonomous_directives_prioritize_verified_actions() {
    assert_eq!(
        objective_behavior_directives_for_mode("  Proactive "),
        [
            "proactively continue objective loops until blocked",
            "prefer smallest reversible patches with immediate verification",
            "only ask operator when a hard decision boundary is reached",
            "always end loops with concrete next actions",
        ]
    );
}

#[test]
fn minimal_directives_keep_blocker_reports_actionable() {
    assert_eq!(
        objective_behavior_directives_for_mode("  CONCISE "),
        [
            "keep responses concise and action-first",
            "avoid speculative detours",
            "report blockers in one line plus next action",
        ]
    );
}

#[test]
fn default_directives_use_the_balanced_checkpoints() {
    assert_eq!(
        default_objective_behavior_directives(),
        [
            "decompose objective into measurable checkpoints",
            "prefer evidence-backed decisions over inference",
            "verify changes before claiming completion",
            "escalate contradictions instead of guessing",
        ]
    );
}

#[test]
fn lifecycle_status_collapses_aliases_and_defaults_unknown_values_to_active() {
    for (input, expected) in [
        ("  BUDGET-LIMITED ", "budget_limited"),
        ("budget_limited", "budget_limited"),
        ("budgetlimited", "budget_limited"),
        ("limited", "budget_limited"),
        ("complete", "complete"),
        ("completed", "complete"),
        ("achieved", "complete"),
        ("done", "complete"),
        ("  SUCCESS ", "complete"),
        ("unmet", "unmet"),
        ("  FAILED ", "unmet"),
        ("blocked_terminal", "unmet"),
        ("unknown status", "active"),
        ("  ", "active"),
    ] {
        assert_eq!(
            canonical_objective_lifecycle_status(input),
            expected,
            "status {input:?}"
        );
    }
}

#[test]
fn custom_operator_profile_trims_the_hint_without_changing_its_case() {
    let profile = objective_profile_specialized_for(" \tAda Example\n");

    assert_eq!(profile.profile_id, "operator-custom");
    assert_eq!(profile.operator_hint, "Ada Example");
    assert_eq!(profile.default_shell, "auto");
    assert_eq!(profile.memory_backend, "contextlattice-preferred");
    assert_eq!(
        profile.specialization_note,
        "Specialized operator profile generated from runtime command."
    );
    assert!(profile.preferred_repos.is_empty());
    assert_eq!(profile.preferred_languages, ["rust", "python", "go"]);
    assert!(profile.updated_at.parse::<jiff::Timestamp>().is_ok());
}

#[test]
fn aggressive_simulation_policy_waives_shadow_and_replay_requirements() {
    let policy = simulation_policy_for_mode("  AGGRESSIVE ");

    assert_eq!(policy.mode, "aggressive");
    assert!(!policy.require_shadow_pass);
    assert_eq!(policy.min_shadow_samples, 0);
    assert!(!policy.require_replay_validation);
    assert!((policy.max_live_capital_fraction - 0.40).abs() < 1e-6);
    assert!(policy.updated_at.parse::<jiff::Timestamp>().is_ok());
}

#[test]
fn unknown_simulation_mode_uses_balanced_validation_and_capital_limit() {
    let policy = simulation_policy_for_mode("unknown simulation mode");

    assert_eq!(policy.mode, "balanced");
    assert!(policy.require_shadow_pass);
    assert_eq!(policy.min_shadow_samples, 5);
    assert!(policy.require_replay_validation);
    assert!((policy.max_live_capital_fraction - 0.25).abs() < 1e-6);
    assert!(policy.updated_at.parse::<jiff::Timestamp>().is_ok());
}

#[test]
fn single_ensemble_policy_uses_primary_model_arbitration() {
    let policy = ensemble_policy_for_mode("  SINGLE ");

    assert_eq!(policy.mode, "single");
    assert_eq!(policy.arbitration, "primary-model");
    assert_eq!(policy.min_voters, 1);
    assert!(!policy.require_disagreement_explainer);
    assert!(policy.allow_fast_path_single_model);
    assert!(policy.updated_at.parse::<jiff::Timestamp>().is_ok());
}

#[test]
fn unknown_ensemble_mode_uses_weighted_committee_arbitration() {
    let policy = ensemble_policy_for_mode("unknown ensemble mode");

    assert_eq!(policy.mode, "committee");
    assert_eq!(policy.arbitration, "weighted-confidence");
    assert_eq!(policy.min_voters, 2);
    assert!(policy.require_disagreement_explainer);
    assert!(policy.allow_fast_path_single_model);
    assert!(policy.updated_at.parse::<jiff::Timestamp>().is_ok());
}

#[test]
fn objective_state_scores_match_normalized_states_and_unknown_fallback() {
    for (state, expected) in [
        ("  FLAT ", 0.5),
        ("regressing", 0.0),
        ("unproven", 0.25),
        ("active", 0.6),
        ("pursuing", 0.6),
        ("paused", 0.45),
        ("budget_limited", 0.2),
        ("budget-limited", 0.2),
        ("complete", 1.0),
        ("achieved", 1.0),
        ("unmet", 0.0),
        ("unknown state", 0.4),
    ] {
        let actual = score_for_objective_state(state);
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "state {state:?}: expected {expected}, got {actual}"
        );
    }
}
