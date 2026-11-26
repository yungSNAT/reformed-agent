use serde_json::{json, Value};
use tempfile::tempdir;

use super::*;

fn write_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("dirs");
    }
    std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).expect("write json");
}

fn write_raw(path: &Path, raw: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("dirs");
    }
    std::fs::write(path, raw).expect("write raw");
}

#[test]
fn policies_snapshot_reports_valid_invalid_and_missing_files() {
    let dir = tempdir().expect("dir");
    let root = dir.path();
    write_json(
        &root.join("contextlattice_policy.json"),
        &json!({"preflight_required": true, "api_key": "should-hide"}),
    );
    write_raw(&root.join("quorum_policy.json"), "{not valid json");

    let payload = objective_policies_snapshot(root, true);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["mutable"], false);
    assert_eq!(payload["alpha_root"], root.display().to_string());

    let ctx = &payload["policies"]["contextlattice"];
    assert_eq!(ctx["exists"], true);
    assert_eq!(ctx["valid_json"], true);
    assert_eq!(ctx["content"]["api_key"], "[REDACTED]");
    assert_eq!(ctx["content"]["preflight_required"], true);
    assert_eq!(ctx["content_redacted"], true);

    let quorum = &payload["policies"]["quorum"];
    assert_eq!(quorum["exists"], true);
    assert_eq!(quorum["valid_json"], false);
    assert_eq!(quorum["bytes"], "{not valid json".len());
    assert!(quorum["parse_error"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));

    let sim = &payload["policies"]["simulation"];
    assert_eq!(sim["exists"], false);
    assert_eq!(sim["valid_json"], false);
    assert_eq!(payload["policies"]["ensemble"]["exists"], false);
    assert_eq!(payload["policies"]["claim_verifier"]["exists"], false);
}

#[test]
fn ledger_snapshot_takes_tail_and_handles_missing() {
    let dir = tempdir().expect("dir");
    let root = dir.path();

    let missing = objective_ledger_snapshot(root, 5, false);
    assert_eq!(missing["ledger"]["present"], false);
    assert_eq!(missing["ledger"]["status"], "missing");
    assert_eq!(missing["file"]["exists"], false);

    write_json(
        &root.join("objective_learning_ledger.json"),
        &json!({
            "updated_at": "2026-01-01T00:00:00Z",
            "entries": [{"n": 1}, {"n": 2}, {"n": 3, "password": concat!("hun", "ter2")}]
        }),
    );
    let payload = objective_ledger_snapshot(root, 2, false);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["mutable"], false);
    assert_eq!(payload["ledger"]["present"], true);
    assert_eq!(payload["ledger"]["entries"], 3);
    assert_eq!(payload["ledger"]["updated_at"], "2026-01-01T00:00:00Z");
    let tail = payload["ledger"]["tail"].as_array().expect("tail");
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0]["n"], 2);
    assert_eq!(tail[1]["n"], 3);
    assert_eq!(tail[1]["password"], "[REDACTED]");
    assert_eq!(payload["file"]["exists"], true);
    assert!(payload["file"].get("content").is_none());
}

#[test]
fn dag_snapshot_counts_statuses() {
    let dir = tempdir().expect("dir");
    let root = dir.path();

    let missing = objective_dag_snapshot(root, false);
    assert_eq!(missing["dag"]["present"], false);

    write_json(
        &root.join("objective_dag.json"),
        &json!({
            "updated_at": "t1",
            "objective_id": "obj-1",
            "auto_resume_checkpoint": "cp-9",
            "nodes": [
                {"status": "done"},
                {"objective_state": "blocked"},
                {"last_status": "done"},
                {}
            ]
        }),
    );
    let payload = objective_dag_snapshot(root, true);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["mutable"], false);
    let dag = &payload["dag"];
    assert_eq!(dag["present"], true);
    assert_eq!(dag["objective_id"], "obj-1");
    assert_eq!(dag["updated_at"], "t1");
    assert_eq!(dag["auto_resume_checkpoint"], "cp-9");
    assert_eq!(dag["nodes"], 4);
    assert_eq!(
        dag["statuses"],
        json!({"done": 2, "blocked": 1, "unknown": 1})
    );
    assert_eq!(payload["file"]["content_redacted"], true);
    assert_eq!(payload["file"]["summary"]["kind"], "object");
}

#[test]
fn eval_snapshot_averages_scores_and_tails() {
    let dir = tempdir().expect("dir");
    let root = dir.path();

    let missing = objective_eval_snapshot(root, 5, false);
    assert_eq!(missing["eval"]["present"], false);

    write_json(
        &root.join("objective_eval_trend.json"),
        &json!({
            "updated_at": "t2",
            "samples": [{"score": 3.0}, {"score": 1.0}, {"name": "no-score"}]
        }),
    );
    let payload = objective_eval_snapshot(root, 2, false);
    assert_eq!(payload["status"], "ok");
    let eval = &payload["eval"];
    assert_eq!(eval["present"], true);
    assert_eq!(eval["updated_at"], "t2");
    assert_eq!(eval["samples"], 3);
    assert_eq!(eval["avg_score"].as_f64(), Some(2.0));
    let tail = eval["tail"].as_array().expect("tail");
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[1]["name"], "no-score");
    assert_eq!(payload["file"]["exists"], true);

    let empty = eval_summary(&json!({"samples": []}), 5);
    assert!(empty["avg_score"].is_null());
    assert_eq!(empty["samples"], 0);
}

#[test]
fn loops_snapshot_summarizes_loops() {
    let dir = tempdir().expect("dir");
    let root = dir.path();

    let missing = mission_loops_snapshot(root, false);
    assert_eq!(missing["loops"]["present"], false);
    assert_eq!(missing["file"]["exists"], false);

    write_json(
        &root.join("loops.json"),
        &json!([
            {"id": "a", "enabled": true, "trading_sensitive": true},
            {"id": "b", "enabled": false}
        ]),
    );
    let payload = mission_loops_snapshot(root, false);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["loops"]["loops"], 2);
    assert_eq!(payload["loops"]["enabled"], 1);
    assert_eq!(payload["loops"]["trading_sensitive"], 1);
    assert_eq!(payload["loops"]["public"], 1);
    assert_eq!(payload["loops"]["ids"], json!(["a", "b"]));
    assert_eq!(
        payload["file"]["summary"],
        json!({"kind": "array", "len": 2})
    );
    assert!(payload["file"].get("content").is_none());
}

#[test]
fn queue_snapshot_handles_missing_file() {
    let dir = tempdir().expect("dir");
    let root = dir.path();
    let payload = mission_queue_snapshot(root, 5, true);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["mutable"], false);
    assert_eq!(payload["queue"]["present"], false);
    assert_eq!(payload["queue"]["events"], 0);
    assert_eq!(payload["queue"]["statuses"], json!({}));
    assert_eq!(payload["queue"]["tail"], json!([]));
    assert_eq!(payload["file"]["exists"], false);
    assert!(payload["file"].get("lines").is_none());
    assert!(payload["file"].get("content").is_none());
}

#[test]
fn queue_snapshot_counts_invalid_lines_and_redacts_content() {
    let dir = tempdir().expect("dir");
    let root = dir.path();
    let raw = "{\"id\":\"e1\",\"status\":\"queued\",\"token\":\"abc\"}\nnot json at all\n\n{\"id\":\"e2\",\"status\":\"done\"}";
    write_raw(&root.join("loop_queue.jsonl"), raw);

    let payload = mission_queue_snapshot(root, 1, true);
    let queue = &payload["queue"];
    assert_eq!(queue["present"], true);
    assert_eq!(queue["events"], 2);
    assert_eq!(queue["invalid_lines"], 1);
    assert_eq!(queue["statuses"], json!({"done": 1, "queued": 1}));
    let tail = queue["tail"].as_array().expect("tail");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0]["id"], "e2");

    let file = &payload["file"];
    assert_eq!(file["exists"], true);
    assert_eq!(file["lines"], 4);
    assert_eq!(file["bytes"], raw.len());
    assert_eq!(file["content_redacted"], true);
    let content = file["content"].as_str().expect("content");
    let lines: Vec<&str> = content.split('\n').collect();
    assert_eq!(lines.len(), 4);
    let first: Value = serde_json::from_str(lines[0]).expect("json line");
    assert_eq!(first["token"], "[REDACTED]");
    assert_eq!(first["id"], "e1");
    assert_eq!(lines[1], "not json at all");
    assert_eq!(lines[2], "");
    assert!(!content.contains("abc"));
}

#[test]
fn runtime_snapshot_counts_last_status() {
    let dir = tempdir().expect("dir");
    let root = dir.path();

    let missing = mission_runtime_snapshot(root, false);
    assert_eq!(missing["runtime"]["present"], false);

    write_json(
        &root.join("loop_runtime.json"),
        &json!({
            "updated_at": "t3",
            "loops": [{"last_status": "ok"}, {"last_status": "ok"}, {"id": "x"}],
            "queue_pending": 2,
            "queue_replayable": 1,
            "orphaned_events": 0
        }),
    );
    let payload = mission_runtime_snapshot(root, false);
    let rt = &payload["runtime"];
    assert_eq!(rt["present"], true);
    assert_eq!(rt["loops"], 3);
    assert_eq!(rt["queue_pending"], 2);
    assert_eq!(rt["statuses"], json!({"ok": 2, "unknown": 1}));
    assert_eq!(payload["file"]["exists"], true);
}

#[test]
fn trading_snapshot_summarizes_config_and_report() {
    let dir = tempdir().expect("dir");
    let root = dir.path();

    let missing = trading_snapshot(root, false);
    assert_eq!(missing["trading"]["config"]["present"], false);
    assert_eq!(missing["trading"]["last_report"]["present"], false);
    assert_eq!(missing["files"].as_array().map(Vec::len), Some(3));

    write_json(
        &root.join("trading").join("runtime_config.json"),
        &json!({
            "updated_at": "t4",
            "starting_wallet_sol": 1.5,
            "target_wallet_sol": 10.0,
            "projects": [{"enabled": true}, {"enabled": false}, {}]
        }),
    );
    write_json(
        &root.join("trading").join("last_report.json"),
        &json!({"generated_at": "t5", "projects": [{}], "risk_governor": {"mode": "defensive"}}),
    );
    let payload = trading_snapshot(root, true);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["secret_values_emitted"], false);
    let cfg = &payload["trading"]["config"];
    assert_eq!(cfg["present"], true);
    assert_eq!(cfg["updated_at"], "t4");
    assert_eq!(cfg["starting_wallet_sol"].as_f64(), Some(1.5));
    assert_eq!(cfg["target_wallet_sol"].as_f64(), Some(10.0));
    assert_eq!(cfg["projects"], 3);
    assert_eq!(cfg["enabled_projects"], 1);
    assert_eq!(
        payload["trading"]["last_report"]["risk_governor_mode"],
        "defensive"
    );
    let files = payload["files"].as_array().expect("files");
    assert_eq!(files[0]["exists"], true);
    assert_eq!(files[0]["content_redacted"], true);
    assert_eq!(files[1]["exists"], true);
    assert_eq!(files[2]["exists"], false);

    let no_projects = trading_config_summary(&json!({}));
    assert_eq!(no_projects["enabled_projects"], 0);
    assert_eq!(no_projects["projects"], 0);
}

#[test]
fn status_snapshot_includes_profile_summary() {
    let dir = tempdir().expect("dir");
    let root = dir.path();
    write_json(
        &root.join("objective_profile.json"),
        &json!({
            "profile_id": "p-1",
            "memory_backend": "sqlite",
            "default_shell": "bash",
            "preferred_repos": ["a", "b"],
            "preferred_languages": ["rust"]
        }),
    );
    let payload = objective_status_snapshot(root, false, 5);
    let profile = &payload["profile"];
    assert_eq!(profile["present"], true);
    assert_eq!(profile["profile_id"], "p-1");
    assert_eq!(profile["memory_backend"], "sqlite");
    assert_eq!(profile["default_shell"], "bash");
    assert_eq!(profile["preferred_repos"], 2);
    assert_eq!(profile["preferred_languages"], 1);
}

#[test]
fn value_kind_and_generic_summary_cover_scalars() {
    assert_eq!(value_kind(&Value::Null), "null");
    assert_eq!(value_kind(&json!(true)), "bool");
    assert_eq!(value_kind(&json!(1)), "number");
    assert_eq!(value_kind(&json!("s")), "string");
    assert_eq!(value_kind(&json!([])), "array");
    assert_eq!(value_kind(&json!({})), "object");

    assert_eq!(generic_value_summary(&json!(42)), json!({"kind": "number"}));
    assert_eq!(
        generic_value_summary(&json!("x")),
        json!({"kind": "string"})
    );

    let dir = tempdir().expect("dir");
    write_raw(&dir.path().join("scalar.json"), "7");
    let row = file_snapshot(dir.path(), "scalar", "scalar.json", false);
    assert_eq!(row["valid_json"], true);
    assert_eq!(row["summary"], json!({"kind": "number"}));
}

#[test]
fn field_counts_handles_none_and_some() {
    assert_eq!(field_counts(None, "last_status"), json!({}));
    let items = vec![json!({"s": "a"}), json!({"s": "a"}), json!({})];
    assert_eq!(
        field_counts(Some(&items), "s"),
        json!({"a": 2, "unknown": 1})
    );
}

#[test]
fn text_file_snapshot_missing_and_present() {
    let dir = tempdir().expect("dir");
    let missing = text_file_snapshot(dir.path(), "q", "nope.jsonl", true);
    assert_eq!(missing["exists"], false);
    assert_eq!(missing["name"], "q");
    assert!(missing.get("content").is_none());

    write_raw(&dir.path().join("q.jsonl"), "hello\nworld");
    let row = text_file_snapshot(dir.path(), "q", "q.jsonl", true);
    assert_eq!(row["exists"], true);
    assert_eq!(row["lines"], 2);
    assert_eq!(row["content"], "hello\nworld");
    assert_eq!(row["content_redacted"], true);
}

#[test]
fn redaction_of_secret_strings_matches_credential_guard() {
    let candidates = [
        concat!("ghp", "_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij"),
        concat!("sk-", "proj-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH"),
        concat!(
            "sk-",
            "ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH"
        ),
        concat!("AKI", "AIOSFODNN7EXAMPLE"),
        concat!(
            "xox",
            "b-123456789012-123456789012-abcdefghijklmnopqrstuvwx"
        ),
        concat!("-----BEGIN ", "RSA PRIVATE KEY-----"),
    ];
    for candidate in candidates {
        let detected = crate::credential_guard::detect_secrets(candidate).is_some();
        let redacted = redact_json_value(&json!({"note": candidate, "list": [candidate]}));
        let expected = if detected { "[REDACTED]" } else { candidate };
        assert_eq!(redacted["note"], expected);
        assert_eq!(redacted["list"][0], expected);

        let text = format!("plain line\n{candidate}");
        let out = redact_text_content(&text);
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines[0], "plain line");
        assert_eq!(lines[1], expected);
    }
    assert_eq!(redact_json_value(&json!("harmless")), json!("harmless"));
    assert_eq!(redact_text_content("just text"), "just text");
}

#[test]
fn redact_text_content_redacts_json_lines() {
    let out = redact_text_content("{\"secret\":\"x\",\"ok\":1}\nfree text");
    let lines: Vec<&str> = out.split('\n').collect();
    assert_eq!(lines.len(), 2);
    let parsed: Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(parsed["secret"], "[REDACTED]");
    assert_eq!(parsed["ok"], 1);
    assert_eq!(lines[1], "free text");
}

#[tokio::test]
async fn handlers_dispatch_to_snapshot_actions() {
    let dir = tempdir().expect("dir");
    let root = dir.path().display().to_string();

    for action in ["policies", "ledger", "dag", "eval"] {
        let raw = ObjectiveSnapshotHandler::new()
            .execute(json!({"action": action, "alpha_root": root}))
            .await
            .expect("execute");
        let payload: Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["mutable"], false);
    }
    for action in ["loops", "queue", "runtime", "trading"] {
        let raw = MissionSnapshotHandler::new()
            .execute(json!({"action": action, "alpha_root": root}))
            .await
            .expect("execute");
        let payload: Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["mutable"], false);
    }
}
