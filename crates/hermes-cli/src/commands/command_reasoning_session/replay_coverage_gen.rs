use super::*;

fn write_log(dir: &std::path::Path, name: &str, lines: &[String]) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut body = lines.join("\n");
    if !lines.is_empty() {
        body.push('\n');
    }
    std::fs::write(&path, body).expect("write replay log");
    path
}

fn full_row(seq: u64, event: &str, trace: &str, prev: &str, hash: &str, turn: u64) -> String {
    serde_json::json!({
        "seq": seq,
        "event": event,
        "trace_id": trace,
        "prev_hash": prev,
        "event_hash": hash,
        "payload": {"turn": turn, "note": "hello"}
    })
    .to_string()
}

#[test]
fn replay_enabled_runtime_reads_env_values() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::replay_enabled_runtime_reads_env_values");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
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
        return;
    }

    let key = "HERMES_REPLAY_ENABLED";
    let old = std::env::var_os(key);

    std::env::set_var(key, "Yes");
    assert!(replay_enabled_runtime());
    std::env::set_var(key, " on ");
    assert!(replay_enabled_runtime());
    std::env::set_var(key, "TRUE");
    assert!(replay_enabled_runtime());
    std::env::set_var(key, "0");
    assert!(!replay_enabled_runtime());
    std::env::set_var(key, "maybe");
    assert!(!replay_enabled_runtime());
    std::env::remove_var(key);
    assert!(!replay_enabled_runtime());

    match old {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

#[test]
fn replay_log_path_sanitizes_session_id() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::replay_log_path_sanitizes_session_id");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
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
        return;
    }

    let p = replay_log_path_for_session("a/b-c_d e");
    assert_eq!(
        p.file_name().and_then(|s| s.to_str()),
        Some("a_b-c_d_e.jsonl")
    );
    let parent = p.parent().expect("parent");
    assert!(parent.ends_with(std::path::Path::new("logs").join("replay")));

    let empty = replay_log_path_for_session("   ");
    assert_eq!(
        empty.file_name().and_then(|s| s.to_str()),
        Some("session.jsonl")
    );
}

#[test]
fn render_tail_formats_rows_and_handles_errors() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::render_tail_formats_rows_and_handles_errors");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
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
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let lines = vec![
        full_row(1, "tool_call", "t-1", "p0", "h1", 2),
        "not json at all".to_string(),
        serde_json::json!({"other": 1}).to_string(),
    ];
    let path = write_log(dir.path(), "tail.jsonl", &lines);

    let out = render_replay_trace_tail(&path, 10).expect("tail");
    assert!(out.contains("event=tool_call"));
    assert!(out.contains("trace=t-1"));
    assert!(out.contains("prev=p0"));
    assert!(out.contains("hash=h1"));
    assert!(out.contains("turn=2"));
    assert!(out.contains("not json at all"));
    assert!(out.contains("#?"));
    assert!(out.contains("event=unknown"));
    assert!(out.contains("trace=missing"));
    assert!(out.contains("prev=? hash=?"));
    assert!(out.contains("turn=-"));
    assert!(!out.ends_with('\n'));

    // limit 0 is clamped to 1: only the last row.
    let last = render_replay_trace_tail(&path, 0).expect("tail one");
    assert!(last.contains("trace=missing"));
    assert!(!last.contains("t-1"));

    let empty = write_log(dir.path(), "empty.jsonl", &[]);
    assert_eq!(
        render_replay_trace_tail(&empty, 5).expect("empty"),
        "Replay log is empty."
    );

    assert!(render_replay_trace_tail(&dir.path().join("missing.jsonl"), 5).is_err());
}

#[test]
fn replay_entries_skips_invalid_and_respects_limit() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::replay_entries_skips_invalid_and_respects_limit");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
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
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let lines = vec![
        full_row(1, "a", "t1", "0", "h1", 1),
        "garbage".to_string(),
        full_row(2, "b", "t2", "h1", "h2", 2),
        full_row(3, "c", "t3", "h2", "h3", 3),
    ];
    let path = write_log(dir.path(), "entries.jsonl", &lines);

    let all = replay_entries(&path, 100).expect("entries");
    assert_eq!(all.len(), 3);
    assert_eq!(all[0]["seq"].as_u64(), Some(1));
    assert_eq!(all[2]["seq"].as_u64(), Some(3));

    let two = replay_entries(&path, 2).expect("entries");
    assert_eq!(two.len(), 2);
    assert_eq!(two[0]["seq"].as_u64(), Some(2));

    assert!(replay_entries(&dir.path().join("nope.jsonl"), 3).is_err());
}

#[test]
fn render_focus_filters_by_trace() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::render_focus_filters_by_trace");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
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
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let lines = vec![
        full_row(1, "start", "trace-abc-1", "0", "h1", 4),
        full_row(2, "other", "trace-xyz", "h1", "h2", 5),
        serde_json::json!({"trace_id": "trace-abc-2"}).to_string(),
    ];
    let path = write_log(dir.path(), "focus.jsonl", &lines);

    assert_eq!(
        render_replay_trace_focus(&path, "   ", 10).expect("usage"),
        "Usage: /raw trace focus <trace-id> [N]"
    );

    let none = render_replay_trace_focus(&path, "zzz", 10).expect("none");
    assert!(none.starts_with("No replay events found for trace 'zzz'"));

    let out = render_replay_trace_focus(&path, " abc ", 10).expect("focus");
    assert!(out.starts_with("Replay trace focus: abc"));
    assert!(out.contains("events: 2"));
    assert!(out.contains("event=start"));
    assert!(out.contains("turn=4"));
    assert!(out.contains("\"note\":\"hello\""));
    assert!(out.contains("event=unknown"));
    assert!(out.contains("{}"));
    assert!(!out.contains("trace-xyz"));
    assert!(!out.contains("event=other"));

    assert!(render_replay_trace_focus(&dir.path().join("x.jsonl"), "abc", 10).is_err());
}

#[test]
fn render_graph_outputs_lineage() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::render_graph_outputs_lineage");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
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
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let lines = vec![
        full_row(7, "turn_start", "tr-1", "h0", "h7", 1),
        serde_json::json!({"x": 1}).to_string(),
    ];
    let path = write_log(dir.path(), "graph.jsonl", &lines);

    let out = render_replay_trace_graph(&path, 50).expect("graph");
    assert!(out.starts_with("Replay lineage graph"));
    assert!(out.contains("--------------------"));
    assert!(out.contains("window=2"));
    assert!(out.contains("trace=tr-1 h0 -> h7"));
    assert!(out.contains("#7"));
    assert!(out.contains("#?"));
    assert!(out.contains("trace=missing - -> -"));

    let empty = write_log(dir.path(), "g_empty.jsonl", &["junk".to_string()]);
    assert_eq!(
        render_replay_trace_graph(&empty, 10).expect("empty graph"),
        "Replay graph: no entries in current window."
    );

    assert!(render_replay_trace_graph(&dir.path().join("none.jsonl"), 10).is_err());
}

#[test]
fn integrity_counts_entries_errors_and_breaks() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::integrity_counts_entries_errors_and_breaks");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
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
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let lines = vec![
        serde_json::json!({"prev_hash": "0", "event_hash": "a"}).to_string(),
        serde_json::json!({"prev_hash": "a", "event_hash": "b"}).to_string(),
        "{broken".to_string(),
        serde_json::json!({"prev_hash": "x", "event_hash": "c"}).to_string(),
    ];
    let path = write_log(dir.path(), "integrity.jsonl", &lines);
    assert_eq!(replay_trace_integrity(&path).expect("integrity"), (3, 1, 1));

    let good = write_log(
        dir.path(),
        "good.jsonl",
        &[
            serde_json::json!({"prev_hash": "0", "event_hash": "a"}).to_string(),
            serde_json::json!({"prev_hash": "a", "event_hash": "b"}).to_string(),
        ],
    );
    assert_eq!(replay_trace_integrity(&good).expect("integrity"), (2, 0, 0));

    assert!(replay_trace_integrity(&dir.path().join("missing.jsonl")).is_err());
}

#[test]
fn export_writes_json_and_reports_errors() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::export_writes_json_and_reports_errors");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
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
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let lines = vec![
        full_row(1, "a", "t", "0", "h1", 1),
        full_row(2, "b", "t", "h1", "h2", 2),
        "bad line".to_string(),
        full_row(3, "c", "t", "h2", "h3", 3),
    ];
    let path = write_log(dir.path(), "export.jsonl", &lines);
    let out_path = dir.path().join("nested").join("deeper").join("out.json");

    let written = export_replay_trace_json(&path, 3, &out_path).expect("export");
    assert_eq!(written, 2);
    let raw = std::fs::read_to_string(&out_path).expect("read export");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("parse export");
    let rows = v["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["seq"].as_u64(), Some(2));
    assert_eq!(rows[1]["seq"].as_u64(), Some(3));
    assert_eq!(
        v["source_replay"].as_str(),
        Some(path.display().to_string().as_str())
    );
    assert!(v["generated_at"].as_str().is_some_and(|s| !s.is_empty()));

    // Missing source.
    assert!(export_replay_trace_json(&dir.path().join("none.jsonl"), 3, &out_path).is_err());

    // Parent is a regular file: directory creation fails.
    let blocker = dir.path().join("blocker.txt");
    std::fs::write(&blocker, "x").expect("write blocker");
    assert!(export_replay_trace_json(&path, 3, &blocker.join("out.json")).is_err());

    // Output path is an existing directory: write fails.
    let as_dir = dir.path().join("as_dir");
    std::fs::create_dir_all(&as_dir).expect("mkdir");
    assert!(export_replay_trace_json(&path, 3, &as_dir).is_err());
}
