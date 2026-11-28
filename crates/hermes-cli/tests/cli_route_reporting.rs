#[path = "support/isolated_hermes.rs"]
mod support;

use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

async fn run(sandbox: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(
        Duration::from_secs(15),
        support::isolated_hermes(sandbox).args(args).output(),
    )
    .await
    .expect("route command finishes")
    .expect("run route command")
}

fn stdout(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "failed route command: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout).unwrap()
}

fn stats(samples: u32, success: f64, latency_ms: u32, failures: u32, updated_at_ms: i64) -> Value {
    json!({
        "samples": samples,
        "success_rate": success,
        "avg_latency_ms": latency_ms,
        "consecutive_failures": failures,
        "updated_at_unix_ms": updated_at_ms,
    })
}

#[tokio::test]
async fn cli_reports_route_health_and_round_trips_autotune_artifacts() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    let logs = home.join("hermes/logs");
    let learning = logs.join("route-learning.json");
    let health = logs.join("route-health.json");
    let autotune = logs.join("route-autotune.json");
    let overrides = logs.join("route-autotune.env");
    assert!(stdout(&run(home, &["route-learning"]).await).contains("(no learned routes yet)"));

    let now_ms = jiff::Timestamp::now().as_millisecond();
    fs::create_dir_all(&logs).unwrap();
    let mut evidence = json!({
        "schema_version": 1,
        "saved_at_unix_ms": now_ms,
        "entries": {
            "fixture-fastest": stats(20, 1.0, 150, 0, 0),
            "fixture-watch": stats(20, 0.85, 3500, 2, 0),
            "fixture-degraded": stats(20, 0.66, 4200, 3, 0),
            "fixture-critical": stats(20, 0.35, 7000, 5, 0),
            "fixture-expired": stats(20, 1.0, 10, 0, now_ms - 8 * 24 * 60 * 60 * 1000),
            "fixture-empty": stats(0, 0.0, 0, 0, 0),
        },
    });
    fs::write(&learning, evidence.to_string()).unwrap();
    let denied = run(home, &["route-autotune", "apply", "--strict"]).await;
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("requires at least 5 learned routes"));
    assert!(!autotune.exists());
    assert!(!overrides.exists());
    evidence["entries"]["fixture-fast"] = stats(20, 0.99, 300, 0, 0);
    fs::write(&learning, evidence.to_string()).unwrap();

    let list = run(home, &["route-learning", "list"]).await;
    let text = stdout(&list);
    assert!(text.contains("entries=5"));
    assert!(text.contains("fixture-fastest"));
    assert!(!text.contains("fixture-expired"));
    assert!(!text.contains("fixture-empty"));
    let baseline = run(home, &["route-learning", "show", "--json"]).await;
    let baseline: Value = serde_json::from_str(stdout(&baseline)).unwrap();

    let report = run(home, &["route-health", "show", "--json"]).await;
    let report: Value = serde_json::from_str(stdout(&report)).unwrap();
    assert_eq!(report["summary"]["overall"], "critical");
    for (tier, count) in [
        ("healthy", 2),
        ("watch", 1),
        ("degraded", 1),
        ("critical", 1),
    ] {
        assert_eq!(report["summary"][tier], count, "{tier}");
    }
    let entries = report["entries"].as_array().unwrap();
    let ranking: Vec<_> = entries
        .iter()
        .map(|row| row["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        ranking,
        [
            "fixture-fastest",
            "fixture-fast",
            "fixture-watch",
            "fixture-degraded",
            "fixture-critical"
        ]
    );
    assert_eq!(
        entries[4]["reasons"],
        json!([
            "low_success_rate",
            "failure_streak_critical",
            "high_latency"
        ])
    );
    let saved: Value = serde_json::from_slice(&fs::read(&health).unwrap()).unwrap();
    assert_eq!(saved, report);
    let text = run(home, &["route-health", "show"]).await;
    assert!(stdout(&text).contains("Overall=critical entries=5"));

    let slash = run(home, &["--oneshot", "/qos health"]).await;
    let slash_health: Value = serde_json::from_str(stdout(&slash)).unwrap();
    assert_eq!(slash_health["summary"], report["summary"]);
    assert_eq!(slash_health["entries"], report["entries"]);

    let slash = run(home, &["--oneshot", "/qos autotune"]).await;
    let slash_plan: Value = serde_json::from_str(stdout(&slash)).unwrap();
    assert_eq!(slash_plan["action"], "plan");
    assert_eq!(slash_plan["applied"], false);
    assert_eq!(slash_plan["plan"]["confidence"], "medium");
    assert!(!overrides.exists());

    let plan = run(home, &["route-autotune", "plan"]).await;
    assert!(stdout(&plan).contains("confidence=medium applied=no"));
    assert!(autotune.is_file());
    assert!(!overrides.exists());
    let apply = run(home, &["route-autotune", "apply", "--strict", "--json"]).await;
    let apply: Value = serde_json::from_str(stdout(&apply)).unwrap();
    assert_eq!(apply["applied"], true);
    assert_eq!(apply["plan"]["reasons"][0], "critical_routes_detected");
    let saved_plan: Value = serde_json::from_slice(&fs::read(&autotune).unwrap()).unwrap();
    assert_eq!(saved_plan, apply["plan"]);
    assert!(overrides.is_file());
    let settings = apply["plan"]["overrides"].as_object().unwrap();
    assert_eq!(settings.len(), 5);
    let loaded = run(home, &["route-learning", "show", "--json"]).await;
    let loaded: Value = serde_json::from_str(stdout(&loaded)).unwrap();
    for (field, setting) in [
        ("ttl_secs", "TTL_SECS"),
        ("half_life_secs", "HALF_LIFE_SECS"),
    ] {
        let key = format!("HERMES_SMART_ROUTING_LEARNING_{setting}");
        let value = settings[&key].as_str().unwrap().parse::<i64>().unwrap();
        assert_eq!(loaded[field], value, "{field}");
        assert_ne!(loaded[field], baseline[field], "{field}");
    }

    let removed = run(home, &["route-autotune", "clear", "--json"]).await;
    let removed: Value = serde_json::from_str(stdout(&removed)).unwrap();
    assert_eq!(removed["ok"], true);
    assert!(!autotune.exists());
    assert!(!overrides.exists());
    let restored = run(home, &["route-learning", "show", "--json"]).await;
    let restored: Value = serde_json::from_str(stdout(&restored)).unwrap();
    for field in ["ttl_secs", "half_life_secs"] {
        assert_eq!(restored[field], baseline[field], "{field}");
    }

    let slash = run(home, &["--oneshot", "/qos autotune wrong"]).await;
    assert!(stdout(&slash).contains("Usage: /qos autotune [plan|apply]"));
    assert!(!autotune.exists());
    let slash = run(home, &["--oneshot", "/qos autotune apply"]).await;
    let slash_apply: Value = serde_json::from_str(stdout(&slash)).unwrap();
    assert_eq!(slash_apply["action"], "apply");
    assert_eq!(slash_apply["applied"], true);
    assert_eq!(slash_apply["plan"]["overrides"], apply["plan"]["overrides"]);
    assert!(overrides.is_file());

    assert!(stdout(&run(home, &["route-health", "reset"]).await).contains("report cleared"));
    assert!(!health.exists());
    let cleared = run(home, &["route-learning", "clear", "--json"]).await;
    let cleared: Value = serde_json::from_str(stdout(&cleared)).unwrap();
    assert_eq!(cleared["ok"], true);
    assert!(!learning.exists());
}
