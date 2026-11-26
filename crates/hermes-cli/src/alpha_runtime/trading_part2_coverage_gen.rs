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
