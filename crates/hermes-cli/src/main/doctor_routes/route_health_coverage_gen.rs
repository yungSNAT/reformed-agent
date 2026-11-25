use super::*;

fn test_cli() -> Cli {
    <Cli as clap::Parser>::try_parse_from(["hermes"]).expect("parse default cli")
}

fn stats(
    samples: u32,
    updated_at_unix_ms: i64,
    success_rate: f64,
    avg_latency_ms: f64,
    consecutive_failures: u32,
) -> RouteLearningStatsRecord {
    serde_json::from_value(serde_json::json!({
        "samples": samples,
        "updated_at_unix_ms": updated_at_unix_ms,
        "success_rate": success_rate,
        "avg_latency_ms": avg_latency_ms,
        "consecutive_failures": consecutive_failures,
    }))
    .expect("build stats record")
}

fn entry(key: &str) -> RouteHealthEntry {
    RouteHealthEntry {
        key: key.to_string(),
        health_score: 0.5,
        tier: "watch".to_string(),
        reasons: Vec::new(),
        stats: stats(5, 1, 0.9, 1000.0, 0),
    }
}

fn entries(n: usize) -> Vec<RouteHealthEntry> {
    (0..n).map(|i| entry(&format!("route-{i}"))).collect()
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn effective_stats_zero_samples_is_none() {
    let s = stats(0, 1_000, 0.5, 1000.0, 0);
    assert!(route_learning_effective_stats(&s, 2_000).is_none());
}

#[test]
fn effective_stats_without_timestamp_is_unchanged() {
    let s = stats(3, 0, 0.4, 4000.0, 2);
    let out = route_learning_effective_stats(&s, 123_456_789).expect("some");
    assert_eq!(out.samples, 3);
    assert_eq!(out.consecutive_failures, 2);
    assert!(approx(out.success_rate, 0.4));
    assert!(approx(out.avg_latency_ms, 4000.0));
}

#[test]
fn effective_stats_zero_age_is_unchanged() {
    let s = stats(10, 1_000_000, 0.5, 3800.0, 4);
    let out = route_learning_effective_stats(&s, 1_000_000).expect("some");
    assert_eq!(out.samples, 10);
    assert_eq!(out.consecutive_failures, 4);
    assert!(approx(out.success_rate, 0.5));
    assert!(approx(out.avg_latency_ms, 3800.0));
}

#[test]
fn effective_stats_expired_by_ttl_is_none() {
    let updated = 1_000_000;
    let eight_days_ms = 8 * 24 * 60 * 60 * 1000;
    let s = stats(10, updated, 0.5, 3800.0, 4);
    assert!(route_learning_effective_stats(&s, updated + eight_days_ms).is_none());
}

#[test]
fn effective_stats_decays_toward_baseline() {
    let updated = 1_000_000;
    let one_day_ms = 24 * 60 * 60 * 1000;
    let s = stats(10, updated, 0.5, 3800.0, 4);
    let out = route_learning_effective_stats(&s, updated + one_day_ms).expect("some");
    assert!((out.success_rate - 0.7).abs() < 1e-6);
    assert!((out.avg_latency_ms - 2800.0).abs() < 1e-6);
    assert_eq!(out.consecutive_failures, 2);
    assert_eq!(out.samples, 5);
}

#[test]
fn route_health_tier_low_success_is_critical() {
    let s = stats(10, 1, 0.4, 1000.0, 0);
    let (tier, reasons, _) = route_health_tier(&s, 0.9);
    assert_eq!(tier, "critical");
    assert!(reasons.contains(&"low_success_rate".to_string()));
}

#[test]
fn route_health_tier_watch_reasons() {
    let s = stats(10, 1, 0.9, 4000.0, 3);
    let (_, reasons, _) = route_health_tier(&s, 0.5);
    assert!(reasons.contains(&"failure_streak_watch".to_string()));
    assert!(reasons.contains(&"latency_watch".to_string()));

    let s2 = stats(10, 1, 0.9, 6000.0, 0);
    let (_, reasons2, _) = route_health_tier(&s2, 0.5);
    assert_eq!(reasons2, vec!["high_latency".to_string()]);
}

#[test]
fn route_health_tier_score_bands() {
    let s = stats(10, 1, 0.9, 1000.0, 0);
    let (t1, r1, h1) = route_health_tier(&s, 0.8);
    assert_eq!(t1, "healthy");
    assert!(r1.is_empty());
    assert!((h1 - (1.1 / 1.2)).abs() < 1e-9);
    assert_eq!(route_health_tier(&s, 0.4).0, "watch");
    assert_eq!(route_health_tier(&s, 0.2).0, "degraded");
    let (t4, _, h4) = route_health_tier(&s, 0.0);
    assert_eq!(t4, "critical");
    assert!((h4 - 0.25).abs() < 1e-9);
}

#[test]
fn load_state_missing_file_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("missing.json");
    match load_route_learning_state_for_cli(&p) {
        Ok(state) => {
            assert_eq!(state.schema_version, 1);
            assert!(state.entries.is_empty());
        }
        Err(_) => panic!("expected ok"),
    }
}

#[test]
fn load_state_parses_valid_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("route-learning.json");
    let body = serde_json::json!({
        "schema_version": 1,
        "saved_at_unix_ms": 5,
        "entries": {
            "openai/gpt": {
                "samples": 7,
                "updated_at_unix_ms": 42,
                "success_rate": 0.8,
                "avg_latency_ms": 1500.0,
                "consecutive_failures": 1,
            }
        }
    });
    std::fs::write(&p, serde_json::to_string(&body).expect("ser")).expect("write");
    match load_route_learning_state_for_cli(&p) {
        Ok(state) => {
            assert_eq!(state.saved_at_unix_ms, 5);
            assert_eq!(state.entries.len(), 1);
            let s = state.entries.get("openai/gpt").expect("entry");
            assert_eq!(s.samples, 7);
            assert_eq!(s.updated_at_unix_ms, 42);
            assert_eq!(s.consecutive_failures, 1);
        }
        Err(_) => panic!("expected ok"),
    }
}

#[test]
fn load_state_invalid_json_is_config_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("route-learning.json");
    std::fs::write(&p, "not json {").expect("write");
    match load_route_learning_state_for_cli(&p) {
        Err(AgentError::Config(msg)) => assert!(msg.contains("parse")),
        _ => panic!("expected config error"),
    }
}

#[test]
fn load_state_directory_is_io_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("subdir");
    std::fs::create_dir_all(&p).expect("mkdir");
    match load_route_learning_state_for_cli(&p) {
        Err(AgentError::Io(msg)) => assert!(msg.contains("read")),
        _ => panic!("expected io error"),
    }
}

#[test]
fn autotune_plan_degraded_single_route() {
    let cli = test_cli();
    let learning = std::path::PathBuf::from("/tmp/learn.json");
    let report = std::path::PathBuf::from("/tmp/report.json");
    let summary = serde_json::json!({"degraded": 1, "average_score": 0.8});
    let plan = build_route_autotune_plan(&cli, &learning, &report, &entries(1), &summary);
    assert_eq!(plan.confidence, "low");
    assert_eq!(
        plan.reasons,
        vec![
            "low_evidence_sample".to_string(),
            "degraded_routes_detected".to_string(),
            "average_health_high".to_string()
        ]
    );
    assert_eq!(plan.learning_path, learning.display().to_string());
    assert_eq!(plan.health_report_path, report.display().to_string());
    let o = &plan.overrides;
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS"], "0.140");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN"], "0.050");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_ALPHA"], "0.280");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_TTL_SECS"], "518400");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS"], "64800");
}

#[test]
fn autotune_plan_watch_medium_confidence() {
    let cli = test_cli();
    let p = std::path::PathBuf::from("/tmp/x.json");
    let summary = serde_json::json!({"watch": 1, "healthy": 4, "average_score": 0.6});
    let plan = build_route_autotune_plan(&cli, &p, &p, &entries(5), &summary);
    assert_eq!(plan.confidence, "medium");
    assert_eq!(plan.reasons, vec!["watch_routes_detected".to_string()]);
    let o = &plan.overrides;
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS"], "0.080");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN"], "0.040");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_ALPHA"], "0.240");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_TTL_SECS"], "604800");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS"], "79200");
}

#[test]
fn autotune_plan_watch_ratio_high_bias() {
    let cli = test_cli();
    let p = std::path::PathBuf::from("/tmp/x.json");
    let summary = serde_json::json!({"watch": 1, "healthy": 1, "average_score": 0.6});
    let plan = build_route_autotune_plan(&cli, &p, &p, &entries(2), &summary);
    assert_eq!(
        plan.overrides["HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS"],
        "0.110"
    );
}

#[test]
fn autotune_plan_healthy_high_confidence() {
    let cli = test_cli();
    let p = std::path::PathBuf::from("/tmp/x.json");
    let summary = serde_json::json!({"healthy": 12, "average_score": 0.8});
    let plan = build_route_autotune_plan(&cli, &p, &p, &entries(12), &summary);
    assert_eq!(plan.confidence, "high");
    assert_eq!(
        plan.reasons,
        vec![
            "routes_healthy".to_string(),
            "average_health_high".to_string()
        ]
    );
    let o = &plan.overrides;
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS"], "0.060");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN"], "0.030");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_ALPHA"], "0.200");
    assert_eq!(o["HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS"], "86400");
    assert_eq!(plan.summary, summary);
}

#[test]
fn autotune_plan_no_routes() {
    let cli = test_cli();
    let p = std::path::PathBuf::from("/tmp/x.json");
    let summary = serde_json::json!({});
    let plan = build_route_autotune_plan(&cli, &p, &p, &[], &summary);
    assert_eq!(plan.confidence, "low");
    assert_eq!(
        plan.reasons,
        vec![
            "low_evidence_sample".to_string(),
            "no_routes_learned".to_string(),
            "average_health_low".to_string()
        ]
    );
    assert_eq!(
        plan.overrides["HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS"],
        "0.080"
    );
    assert_eq!(
        plan.overrides["HERMES_SMART_ROUTING_LEARNING_TTL_SECS"],
        "604800"
    );
}

#[test]
fn run_route_learning_rejects_unknown_action() {
    let err = run_route_learning(test_cli(), Some("  Bogus ".to_string()), false)
        .expect_err("should fail");
    match err {
        AgentError::Config(msg) => {
            assert!(msg.contains("route-learning: unsupported action 'bogus'"))
        }
        _ => panic!("expected config error"),
    }
}

#[test]
fn run_route_health_rejects_unknown_action() {
    let err =
        run_route_health(test_cli(), Some("nope".to_string()), true).expect_err("should fail");
    match err {
        AgentError::Config(msg) => assert!(msg.contains("route-health: unsupported action 'nope'")),
        _ => panic!("expected config error"),
    }
}

#[test]
fn run_route_autotune_rejects_unknown_action() {
    let err = run_route_autotune(test_cli(), Some("WHAT".to_string()), false, false, false)
        .expect_err("should fail");
    match err {
        AgentError::Config(msg) => {
            assert!(msg.contains("route-autotune: unsupported action 'what'"))
        }
        _ => panic!("expected config error"),
    }
}

#[test]
fn run_incident_pack_missing_snapshot_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-snapshot.json");
    let err = run_incident_pack(test_cli(), Some(missing.display().to_string()), None, false)
        .expect_err("should fail");
    match err {
        AgentError::Config(msg) => assert!(msg.contains("incident-pack snapshot not found")),
        _ => panic!("expected config error"),
    }
}

#[test]
fn route_autotune_state_path_is_under_logs() {
    let cli = test_cli();
    let p = route_autotune_state_path_for_cli(&cli);
    assert!(p.ends_with(std::path::Path::new("logs").join("route-autotune.json")));
    assert_eq!(p.parent(), route_health_state_path_for_cli(&cli).parent());
}
