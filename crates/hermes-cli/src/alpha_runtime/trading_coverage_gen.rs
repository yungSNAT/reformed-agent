use super::*;
use std::collections::HashMap;
use std::fs;
use std::process::Command;

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn spec_for(id: &str, path: &std::path::Path) -> TradingProjectSpec {
    TradingProjectSpec {
        id: id.to_string(),
        path: path.display().to_string(),
        enabled: true,
    }
}

#[test]
fn expand_home_handles_tilde_and_plain_paths() {
    assert_eq!(expand_home("/abs/path"), "/abs/path");
    let expanded = expand_home("~/some/dir");
    match std::env::var_os("HOME") {
        Some(home) => {
            let expected = std::path::PathBuf::from(home).join("some/dir");
            assert_eq!(expanded, expected.to_string_lossy().to_string());
        }
        None => assert_eq!(expanded, "~/some/dir"),
    }
}

#[test]
fn discover_recent_json_files_filters_sorts_and_truncates() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(discover_recent_json_files(dir.path(), 5).is_empty());

    let logs = dir.path().join("logs").join("run_context");
    fs::create_dir_all(&logs).expect("mkdir");
    fs::write(logs.join("a.json"), "{}").expect("write");
    fs::write(logs.join("b.json"), "{}").expect("write");
    fs::write(logs.join("notes.txt"), "x").expect("write");

    let all = discover_recent_json_files(dir.path(), 10);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0], logs.join("b.json"));
    assert_eq!(all[1], logs.join("a.json"));

    let limited = discover_recent_json_files(dir.path(), 0);
    assert_eq!(limited, vec![logs.join("b.json")]);
}

#[test]
fn find_numeric_hint_walks_nested_values() {
    let v = serde_json::json!({
        "outer": [
            {"pnl_sol": "not-a-number"},
            {"inner": {"PNL_SOL": 3}}
        ]
    });
    assert_eq!(find_numeric_hint(&v, &["pnl_sol"]), Some(3.0));

    let v2 = serde_json::json!({"Wallet_Balance_SOL": 2.5});
    assert_eq!(find_numeric_hint(&v2, &["wallet_balance_sol"]), Some(2.5));

    let v3 = serde_json::json!({"other": [1, 2, {"x": true}]});
    assert_eq!(find_numeric_hint(&v3, &["pnl_sol"]), None);

    let v4 = serde_json::json!("pnl_sol");
    assert_eq!(find_numeric_hint(&v4, &["pnl_sol"]), None);

    let v5 = serde_json::json!([{"a": 1}, {"pnl_sol": -4.0}]);
    assert_eq!(find_numeric_hint(&v5, &["pnl_sol"]), Some(-4.0));
}

#[test]
fn compute_regime_covers_all_branches() {
    assert_eq!(compute_regime(0.5, 1.0, 0.2), "execution-stressed");
    assert_eq!(compute_regime(1.5, 0.0, 0.0), "high-vol-trending");
    assert_eq!(compute_regime(1.5, -1.0, 0.0), "high-vol-adverse");
    assert_eq!(compute_regime(0.1, 0.0, 0.0), "low-vol-range");
    assert_eq!(compute_regime(0.8, 0.0, 0.0), "neutral");
}

#[test]
fn compute_incident_class_covers_all_branches() {
    assert_eq!(compute_incident_class(0.0, 0.0, 0.9), "critical-anomaly");
    assert_eq!(compute_incident_class(0.1, 0.0, 0.1), "order-reject-spike");
    assert_eq!(
        compute_incident_class(0.0, 2000.0, 0.1),
        "latency-degradation"
    );
    assert_eq!(compute_incident_class(0.0, 100.0, 0.1), "none");
}

#[test]
fn analyze_project_missing_path_is_unproven() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist");
    let r = analyze_project(&spec_for("gone", &missing));
    assert!(!r.exists);
    assert_eq!(r.objective_state, "unproven");
    assert_eq!(r.incident_class, "missing-project-path");
    assert_eq!(r.patch_recommendations.len(), 2);
}

#[test]
fn analyze_project_parses_run_context_metrics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let logs = dir.path().join("logs").join("run_context");
    fs::create_dir_all(&logs).expect("mkdir");
    fs::write(
        logs.join("a.json"),
        r#"{"wallet_sol": 10.0, "pnl_sol": 1.0, "latency_ms": 100}"#,
    )
    .expect("write");
    fs::write(
        logs.join("b.json"),
        r#"{"wallet_sol": 5.0, "pnl_sol": -1.0, "latency_ms": 2000, "reject_rate": 0.2,
            "slippage_bps": -80.0, "impact_bps": 10, "fee_sol": 0.01, "funding_sol": 0.02}"#,
    )
    .expect("write");
    fs::write(logs.join("notes.txt"), "ignored").expect("write");

    let r = analyze_project(&spec_for("p1", dir.path()));
    assert!(r.exists);
    assert_eq!(r.id, "p1");
    assert_eq!(r.run_context_files, 2);
    assert!(approx(r.latest_wallet_sol, 5.0));
    assert!(approx(r.latest_pnl_sol, -1.0));
    assert!(approx(r.drawdown_pct, 0.5));
    assert!(approx(r.volatility_score, 1.0));
    assert!(approx(r.latency_p95_ms, 2000.0));
    assert!(approx(r.latency_p50_ms, 2000.0));
    assert!(approx(r.reject_rate, 0.2));
    assert!(approx(r.slippage_bps, 80.0));
    assert!(approx(r.impact_bps, 10.0));
    assert!(approx(r.fee_drag_sol, 0.01));
    assert!(approx(r.funding_drag_sol, 0.02));
    assert!(approx(r.anomaly_score, 0.575));
    assert_eq!(r.regime, "execution-stressed");
    assert_eq!(r.incident_class, "order-reject-spike");
    assert_eq!(r.objective_state, "regressing");
    assert_eq!(r.patch_recommendations.len(), 3);
    assert!(r.patch_recommendations[0].contains("slippage"));
    assert!(r.patch_recommendations[1].contains("retry backoff"));
    assert!(r.patch_recommendations[2].contains("drawdown"));
}

#[test]
fn analyze_project_with_positive_pnl_is_advancing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let logs = dir.path().join("logs").join("run_context");
    fs::create_dir_all(&logs).expect("mkdir");
    fs::write(
        logs.join("a.json"),
        r#"{"wallet_sol": 2.0, "pnl_sol": 0.5}"#,
    )
    .expect("write");
    fs::write(logs.join("b.json"), "not json").expect("write");

    let r = analyze_project(&spec_for("adv", dir.path()));
    assert_eq!(r.run_context_files, 2);
    assert!(approx(r.latest_pnl_sol, 0.5));
    assert!(approx(r.drawdown_pct, 0.0));
    assert_eq!(r.objective_state, "advancing");
    assert_eq!(r.regime, "low-vol-range");
    assert_eq!(r.incident_class, "none");
    assert_eq!(
        r.patch_recommendations,
        vec!["maintain current controls; continue telemetry burn-in".to_string()]
    );
}

#[test]
fn analyze_project_without_files_is_flat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let r = analyze_project(&spec_for("flat", dir.path()));
    assert!(r.exists);
    assert_eq!(r.run_context_files, 0);
    assert_eq!(r.objective_state, "flat");
    assert!(approx(r.latency_p50_ms, 0.0));
}

#[test]
fn derive_hypotheses_produces_sorted_entries() {
    let report = TradingAlphaReport {
        projects: vec![
            TradingProjectReport {
                id: "p".to_string(),
                slippage_bps: 100.0,
                reject_rate: 0.1,
                drawdown_pct: 0.5,
                ..TradingProjectReport::default()
            },
            TradingProjectReport {
                id: "calm".to_string(),
                ..TradingProjectReport::default()
            },
        ],
        ..TradingAlphaReport::default()
    };
    let hyps = derive_hypotheses(&report);
    assert_eq!(hyps.len(), 3);
    assert_eq!(hyps[0].id, "hyp-p-rejects");
    assert!(approx(hyps[0].novelty_score, 0.8));
    assert!(approx(hyps[0].expected_gain_sol, 0.06));
    assert_eq!(hyps[1].id, "hyp-p-slippage");
    assert!(approx(hyps[1].novelty_score, 0.5));
    assert!(approx(hyps[1].expected_gain_sol, 0.1));
    assert_eq!(hyps[2].id, "hyp-p-drawdown");
    assert!(approx(hyps[2].expected_gain_sol, 0.2));
    assert!(hyps[2].statement.starts_with("p: drawdown profile"));
}

#[test]
fn experiment_specs_and_backtest_matrix_are_formatted() {
    let hyps = vec![
        StrategyHypothesis {
            id: "h1".to_string(),
            expected_gain_sol: 0.1,
            ..StrategyHypothesis::default()
        },
        StrategyHypothesis {
            id: "h2".to_string(),
            expected_gain_sol: 0.0,
            ..StrategyHypothesis::default()
        },
    ];
    let specs = compile_experiment_specs(&hyps);
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].id, "exp-1");
    assert_eq!(specs[0].hypothesis_id, "h1");
    assert_eq!(specs[0].metric, "net_pnl_after_costs_sol");
    assert_eq!(specs[0].control, "current_config");
    assert_eq!(specs[0].treatment, "treatment_from_h1");
    assert_eq!(specs[0].pass_criterion, "delta_sol > 0.0350");
    assert_eq!(specs[1].pass_criterion, "delta_sol > 0.0005");

    let matrix = build_backtest_matrix(&specs);
    assert_eq!(
        matrix[0],
        "exp-1 | metric=net_pnl_after_costs_sol | control=current_config | treatment=treatment_from_h1 | pass=delta_sol > 0.0350"
    );
    assert_eq!(matrix.len(), 2);
}

#[test]
fn collect_repo_drift_on_plain_directory_is_not_git() {
    let dir = tempfile::tempdir().expect("tempdir");
    let drift = collect_repo_drift(
        &spec_for("plain", dir.path()),
        &TradingDriftBaseline::default(),
    );
    assert_eq!(drift.project_id, "plain");
    assert!(drift.git_head.is_empty());
    assert_eq!(drift.drift_state, "not-a-git-repo");
    assert!(!drift.changed_since_baseline);
}

#[test]
fn collect_repo_drift_detects_head_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let init = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(dir.path())
        .output();
    match init {
        Ok(o) if o.status.success() => {}
        _ => return,
    }
    let commit = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ])
        .output();
    match commit {
        Ok(o) if o.status.success() => {}
        _ => return,
    }
    let mut baseline = TradingDriftBaseline::default();
    baseline
        .heads
        .insert("repo".to_string(), "0000000".to_string());
    let spec = spec_for("repo", dir.path());
    let drift = collect_repo_drift(&spec, &baseline);
    if drift.git_head.is_empty() || drift.dirty_files > 0 {
        return;
    }
    assert_eq!(drift.baseline_head, "0000000");
    assert!(drift.changed_since_baseline);
    assert_eq!(drift.drift_state, "head-changed");

    let mut same = TradingDriftBaseline::default();
    same.heads
        .insert("repo".to_string(), drift.git_head.clone());
    let stable = collect_repo_drift(&spec, &same);
    assert_eq!(stable.drift_state, "stable");
    assert!(!stable.changed_since_baseline);
}

#[test]
fn run_context_audit_reports_all_present_metrics() {
    let p = TradingProjectReport {
        id: "full".to_string(),
        run_context_files: 3,
        latest_wallet_sol: 1.0,
        latest_pnl_sol: 0.5,
        reject_rate: 0.01,
        slippage_bps: 5.0,
        latency_p95_ms: 100.0,
        ..TradingProjectReport::default()
    };
    let audit = collect_run_context_audit(&p);
    assert_eq!(audit.project_id, "full");
    assert_eq!(audit.files_scanned, 3);
    assert_eq!(audit.required_metrics_present.len(), 5);
    assert!(audit.missing_metrics.is_empty());
    assert!(audit.passed);
}

#[test]
fn parse_env_kv_skips_comments_and_blank_lines() {
    let kv = parse_env_kv("# comment\n\nA=1\n  B = two  \nnoequals\n");
    assert_eq!(kv.len(), 2);
    assert_eq!(kv.get("A").map(String::as_str), Some("1"));
    assert_eq!(kv.get("B").map(String::as_str), Some("two"));
}

#[test]
fn env_provenance_clean_when_values_agree() {
    let dir = tempfile::tempdir().expect("tempdir");
    // `.env` as a directory: it exists but cannot be read as a file.
    fs::create_dir_all(dir.path().join(".env")).expect("mkdir");
    let knob = dir.path().join("logs").join("knob_tuner");
    let nightly = dir.path().join("logs").join("nightly_tuner");
    fs::create_dir_all(&knob).expect("mkdir");
    fs::create_dir_all(&nightly).expect("mkdir");
    fs::write(
        knob.join("overrides.env"),
        "# tuned\nPRE_TRADE_MAX_SLIPPAGE_BPS=50\n",
    )
    .expect("write");
    fs::write(
        nightly.join("overrides.env"),
        "PRE_TRADE_MAX_SLIPPAGE_BPS = 50\n",
    )
    .expect("write");

    let gate = collect_env_provenance(&spec_for("env", dir.path()));
    assert_eq!(gate.inspected_files.len(), 3);
    assert!(gate.conflicting_keys.is_empty());
    assert!(gate.passed);
    assert_eq!(gate.decision, "provenance-clean");
}

#[test]
fn env_provenance_reports_conflicts() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join(".env"), "PRE_TRADE_MIN_LIQUIDITY_USD=100\n").expect("write");
    let knob = dir.path().join("logs").join("knob_tuner");
    fs::create_dir_all(&knob).expect("mkdir");
    fs::write(
        knob.join("overrides.env"),
        "PRE_TRADE_MIN_LIQUIDITY_USD=200\n",
    )
    .expect("write");
    let gate = collect_env_provenance(&spec_for("env", dir.path()));
    assert!(!gate.passed);
    assert_eq!(
        gate.conflicting_keys,
        vec!["PRE_TRADE_MIN_LIQUIDITY_USD".to_string()]
    );
    assert_eq!(gate.decision, "conflicts: PRE_TRADE_MIN_LIQUIDITY_USD");
}

#[test]
fn risk_governor_de_risk_paths() {
    let g = compute_risk_governor(&[], 0.4, 0.1);
    assert_eq!(g.mode, "de-risk");
    assert!(!g.halt_new_entries);
    assert!(approx(g.max_portfolio_drawdown_pct, 0.18));
    assert!(approx(g.max_project_drawdown_pct, 0.12));
    assert!(approx(g.max_ruin_probability, 0.35));

    let projects = vec![TradingProjectReport {
        drawdown_pct: 0.3,
        ..TradingProjectReport::default()
    }];
    let g2 = compute_risk_governor(&projects, 0.1, 0.1);
    assert_eq!(g2.mode, "de-risk");
    assert!(g2.reason.contains("reduce exposure"));
}

#[test]
fn replay_canary_fails_for_low_quality() {
    let projects = vec![TradingProjectReport {
        id: "bad".to_string(),
        reject_rate: 0.5,
        ..TradingProjectReport::default()
    }];
    let rows = compute_replay_canary(&projects);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision, "fail");
    assert_eq!(rows[0].sample_size, 1);
    assert!(approx(rows[0].pass_rate, 0.5));
}

#[test]
fn remediation_runbook_covers_priorities_and_halt() {
    let projects = vec![
        TradingProjectReport {
            id: "reg".to_string(),
            path: "/r".to_string(),
            objective_state: "regressing".to_string(),
            incident_class: "inc".to_string(),
            regime: "reg".to_string(),
            patch_recommendations: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ..TradingProjectReport::default()
        },
        TradingProjectReport {
            id: "quiet".to_string(),
            path: "/q".to_string(),
            ..TradingProjectReport::default()
        },
    ];
    let governor = PortfolioRiskGovernor {
        halt_new_entries: true,
        reason: "too risky".to_string(),
        ..PortfolioRiskGovernor::default()
    };
    let out = build_remediation_runbook(&projects, &governor);
    assert_eq!(out.len(), 4);
    assert_eq!(out[0].priority, "p0");
    assert_eq!(out[0].title, "a");
    assert_eq!(out[0].rationale, "incident=inc regime=reg");
    assert_eq!(out[1].title, "b");
    assert_eq!(out[2].project_id, "quiet");
    assert_eq!(out[2].priority, "p2");
    assert_eq!(out[2].title, "continue telemetry burn-in");
    assert_eq!(out[2].command, "cd /q && ls logs/run_context | tail -n 20");
    assert_eq!(out[3].project_id, "portfolio");
    assert_eq!(out[3].priority, "p0");
    assert_eq!(out[3].rationale, "too risky");
}

#[test]
fn count_files_respects_depth() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a"), "1").expect("write");
    let sub = dir.path().join("sub");
    let deep = sub.join("deep");
    fs::create_dir_all(&deep).expect("mkdir");
    fs::write(sub.join("b"), "2").expect("write");
    fs::write(deep.join("c"), "3").expect("write");

    assert_eq!(count_files(dir.path(), 0), 0);
    assert_eq!(count_files(dir.path(), 1), 1);
    assert_eq!(count_files(dir.path(), 2), 2);
    assert_eq!(count_files(dir.path(), 3), 3);
    assert_eq!(count_files(&dir.path().join("nope"), 3), 0);
}

#[test]
fn ingest_research_sources_counts_existing_dirs() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("docs")).expect("mkdir");
    fs::write(dir.path().join("docs").join("x.md"), "x").expect("write");
    fs::create_dir_all(dir.path().join("scripts")).expect("mkdir");
    fs::write(dir.path().join("scripts").join("y.sh"), "y").expect("write");
    fs::write(dir.path().join("scripts").join("z.sh"), "z").expect("write");

    let rows = ingest_research_sources(&spec_for("res", dir.path()));
    assert_eq!(rows.len(), 5);
    let by_name: HashMap<&str, &ResearchSourceIngestion> =
        rows.iter().map(|r| (r.source.as_str(), r)).collect();
    assert!(by_name["docs"].found);
    assert_eq!(by_name["docs"].items, 1);
    assert!(by_name["scripts"].found);
    assert_eq!(by_name["scripts"].items, 2);
    assert!(!by_name["notebooks"].found);
    assert_eq!(by_name["notebooks"].items, 0);
    assert_eq!(by_name["docs"].project_id, "res");
}

#[test]
fn render_board_includes_every_section_row() {
    let mut weights = HashMap::new();
    weights.insert("p1".to_string(), 0.5);
    let report = TradingAlphaReport {
        generated_at: "now".to_string(),
        postmortem: "Postmortem packet\n- end".to_string(),
        strategy_weights: weights,
        capital_allocator: vec![CapitalAllocationRow {
            project_id: "p1".to_string(),
            target_weight: 0.5,
            target_capital_sol: 1.0,
            max_loss_budget_sol: 0.1,
            throttle_factor: 0.9,
        }],
        canary_pipeline: vec![CanaryPromotionStep {
            stage: "telemetry-audit".to_string(),
            passed: true,
            detail: "d".to_string(),
        }],
        hypotheses: vec![StrategyHypothesis {
            id: "h1".to_string(),
            statement: "s".to_string(),
            novelty_score: 0.5,
            expected_gain_sol: 0.1,
        }],
        walkforward_checks: vec!["wf-line".to_string()],
        meta_ranking: vec!["p1 score=1.0".to_string()],
        repo_drift: vec![RepoDriftSentinel {
            project_id: "p1".to_string(),
            git_head: "abc".to_string(),
            baseline_head: "abd".to_string(),
            dirty_files: 2,
            changed_since_baseline: true,
            drift_state: "stable".to_string(),
        }],
        run_context_audits: vec![
            RunContextAudit {
                project_id: "ok".to_string(),
                files_scanned: 1,
                passed: true,
                ..RunContextAudit::default()
            },
            RunContextAudit {
                project_id: "bad".to_string(),
                files_scanned: 0,
                missing_metrics: vec!["x".to_string(), "y".to_string()],
                ..RunContextAudit::default()
            },
        ],
        env_provenance: vec![EnvProvenanceGate {
            project_id: "p1".to_string(),
            inspected_files: vec!["a".to_string(), "b".to_string()],
            conflicting_keys: vec!["K".to_string()],
            passed: false,
            decision: "conflicts: K".to_string(),
        }],
        replay_canary: vec![ReplayCanaryResult {
            project_id: "p1".to_string(),
            sample_size: 3,
            pass_rate: 0.75,
            decision: "pass".to_string(),
        }],
        remediation_runbook: vec![RemediationRunbookAction {
            project_id: "p1".to_string(),
            priority: "p0".to_string(),
            title: "fix it".to_string(),
            command: "cd x".to_string(),
            rationale: "r".to_string(),
        }],
        research_sources: vec![ResearchSourceIngestion {
            project_id: "p1".to_string(),
            source: "docs".to_string(),
            path: "/x/docs".to_string(),
            found: true,
            items: 4,
        }],
        ..TradingAlphaReport::default()
    };
    let out = render_trading_alpha_board(&report);
    assert!(out.contains("generated_at: now\n"));
    assert!(out.contains("- p1: 0.5000\n"));
    assert!(out.contains(
        "- p1 weight=0.5000 capital_sol=1.000000 max_loss_sol=0.100000 throttle=0.900\n"
    ));
    assert!(out.contains("- telemetry-audit passed=true detail=d\n"));
    assert!(out.contains("- hypotheses=1 experiments=0 matrix_rows=0\n"));
    assert!(out.contains("  - h1 novelty=0.500 expected_gain_sol=0.1000\n"));
    assert!(out.contains("- wf-line\n"));
    assert!(out.contains("- p1 score=1.0\n"));
    assert!(out.contains(
        "- p1 state=stable head=abc baseline=abd dirty_files=2 changed_since_baseline=true\n"
    ));
    assert!(out.contains("- ok passed=true files_scanned=1 missing=none\n"));
    assert!(out.contains("- bad passed=false files_scanned=0 missing=x,y\n"));
    assert!(out.contains("- p1 passed=false inspected_files=2 decision=conflicts: K\n"));
    assert!(out.contains("- p1 sample_size=3 pass_rate=0.750 decision=pass\n"));
    assert!(out.contains("- [p0] p1 :: fix it | cd x\n"));
    assert!(out.contains("- p1:docs found=true items=4 path=/x/docs\n"));
    assert!(out.ends_with("Postmortem packet\n- end"));
}
