use super::*;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Instant;

fn read_request(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= pos + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

fn serve(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let req = read_request(&mut stream);
            let _ = tx.send(req);
            let reason = if (200..300).contains(&status) {
                "OK"
            } else {
                "Error"
            };
            let resp = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}"), rx)
}

fn recv(rx: &mpsc::Receiver<String>) -> String {
    rx.recv_timeout(Duration::from_secs(5)).expect("request")
}

fn state_at(base: &str, dir: &Path) -> RetainState {
    RetainState {
        client: Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client"),
        base: base.to_string(),
        token: "test-token".to_string(),
        project: "proj".to_string(),
        user_id: "user-1".to_string(),
        agent_id: "agent-1".to_string(),
        session_id: "session-1".to_string(),
        queue_db_path: dir.join("retaindb_queue.db"),
    }
}

fn plugin_with(st: RetainState) -> RetainDbMemoryPlugin {
    let p = RetainDbMemoryPlugin::new();
    *p.state.lock().unwrap() = Some(st);
    p
}

fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

fn err_of(s: &str) -> String {
    let v: Value = serde_json::from_str(s).expect("json");
    v["error"].as_str().unwrap_or("").to_string()
}

fn set_env(key: &str, value: Option<&OsStr>) {
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        Self {
            saved: keys.iter().map(|k| (*k, std::env::var_os(k))).collect(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            set_env(k, v.as_deref());
        }
    }
}

#[test]
fn default_plugin_is_uninitialized() {
    if run_isolated("default_plugin_is_uninitialized") {
        return;
    }

    let p = RetainDbMemoryPlugin::default();
    assert!(p.system_prompt_block().contains("Project: retaindb."));
    p.queue_prefetch("anything", "s");
    assert_eq!(p.prefetch("anything", "s"), "");
    p.sync_turn("hello", "world", "s");
    assert_eq!(
        err_of(&p.handle_tool_call("retaindb_profile", &json!({}))),
        "RetainDB not initialized"
    );
    let schema = p.get_config_schema().expect("schema");
    let arr = schema.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["key"], "api_key");
    assert_eq!(arr[1]["default"], DEFAULT_BASE);
    assert_eq!(arr[2]["env_var"], "RETAINDB_PROJECT");
    p.shutdown();
    assert!(p.state.lock().unwrap().is_none());
}

#[test]
fn initialized_plugin_validates_and_shuts_down() {
    if run_isolated("initialized_plugin_validates_and_shuts_down") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let p = plugin_with(state_at("http://127.0.0.1:9", tmp.path()));
    assert!(p.system_prompt_block().contains("Project: proj."));
    assert_eq!(
        err_of(&p.handle_tool_call("nope", &json!({}))),
        "Unknown tool: nope"
    );
    assert_eq!(
        err_of(&p.handle_tool_call("retaindb_search", &json!({}))),
        "query is required"
    );
    assert_eq!(
        err_of(&p.handle_tool_call("retaindb_context", &json!({"query": ""}))),
        "query is required"
    );
    assert_eq!(
        err_of(&p.handle_tool_call("retaindb_remember", &json!({}))),
        "content is required"
    );
    assert_eq!(
        err_of(&p.handle_tool_call("retaindb_forget", &json!({}))),
        "memory_id is required"
    );
    assert_eq!(
        err_of(&p.handle_tool_call("retaindb_read_file", &json!({}))),
        "file_id is required"
    );

    *p.prefetch_ctx.lock().unwrap() = "cached ctx".to_string();
    assert_eq!(p.prefetch("q", "s"), "cached ctx");
    assert_eq!(p.prefetch("q", "s"), "");

    p.shutdown();
    assert!(p.state.lock().unwrap().is_none());
    assert!(p.system_prompt_block().contains("Project: retaindb."));
}

#[test]
fn initialize_reads_env_and_seeds_soul() {
    if run_isolated("initialize_reads_env_and_seeds_soul") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (server_base, rx) = serve(vec![(200, r#"{"ok":true}"#)]);
    let home_a = tmp.path().join(".hermes");
    std::fs::create_dir_all(&home_a).expect("mkdir a");
    std::fs::write(home_a.join("SOUL.md"), "   \n").expect("soul a");
    let home_b = tmp.path().join("work");
    std::fs::create_dir_all(&home_b).expect("mkdir b");
    std::fs::write(home_b.join("SOUL.md"), "Be precise.\n").expect("soul b");

    let p0 = RetainDbMemoryPlugin::new();
    let p1 = RetainDbMemoryPlugin::new();
    let p2 = RetainDbMemoryPlugin::new();
    {
        let _guard = EnvGuard::new(&["RETAINDB_API_KEY", "RETAINDB_BASE_URL", "RETAINDB_PROJECT"]);
        set_env("RETAINDB_API_KEY", Some(OsStr::new("")));
        p0.initialize("s0", home_a.to_str().expect("utf8"));
        set_env("RETAINDB_API_KEY", Some(OsStr::new("Bearer k")));
        let base_with_slash = format!("{server_base}/");
        set_env("RETAINDB_BASE_URL", Some(OsStr::new(&base_with_slash)));
        set_env("RETAINDB_PROJECT", None);
        p1.initialize("sess-a", home_a.to_str().expect("utf8"));
        p2.initialize("sess-b", home_b.to_str().expect("utf8"));
    }

    assert!(p0.state.lock().unwrap().is_none());

    let st1 = p1.state.lock().unwrap().clone().expect("state a");
    assert_eq!(st1.base, server_base);
    assert_eq!(st1.project, "default");
    assert_eq!(st1.token, "Bearer k");
    assert_eq!(st1.session_id, "sess-a");
    assert_eq!(st1.user_id, "default");
    assert_eq!(st1.agent_id, "hermes");
    assert_eq!(st1.queue_db_path, home_a.join("retaindb_queue.db"));
    assert!(st1.queue_db_path.exists());
    assert!(p1.system_prompt_block().contains("Project: default."));
    assert!(p2.system_prompt_block().contains("Project: hermes-work."));

    let seed = recv(&rx);
    assert!(seed.starts_with("POST /v1/memory/agent/hermes/seed HTTP/1.1"));
    assert!(seed.contains("Be precise."));
    assert!(seed.contains("\"source\":\"soul_md\""));
    assert!(seed.contains("hermes-work"));
    assert!(seed.to_ascii_lowercase().contains("x-api-key: k"));
    std::thread::sleep(Duration::from_millis(50));
}

#[test]
fn sync_turn_enqueues_and_flushes_with_default_session() {
    if run_isolated("sync_turn_enqueues_and_flushes_with_default_session") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (base, rx) = serve(vec![(200, r#"{"ok":true}"#)]);
    let st = state_at(&base, tmp.path());
    let db = st.queue_db_path.clone();
    let p = plugin_with(st);
    p.sync_turn("   ", "ignored", "");
    assert!(!db.exists());

    p.sync_turn("hello there", "hi back", "");
    let req = recv(&rx);
    assert!(req.starts_with("POST /v1/memory/ingest/session HTTP/1.1"));
    assert!(req.contains("hello there"));
    assert!(req.contains("hi back"));
    assert!(req.contains("\"session_id\":\"session-1\""));
    assert!(req.contains("\"write_mode\":\"sync\""));
    assert!(wait_until(|| pending_retaindb_rows(&db)
        .map(|r| r.is_empty())
        .unwrap_or(false)));
    std::thread::sleep(Duration::from_millis(50));
}

#[test]
fn sync_turn_uses_explicit_session_id() {
    if run_isolated("sync_turn_uses_explicit_session_id") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (base, rx) = serve(vec![(200, r#"{"ok":true}"#)]);
    let st = state_at(&base, tmp.path());
    let db = st.queue_db_path.clone();
    let p = plugin_with(st);
    p.sync_turn("question", "answer", "sess-x");
    let req = recv(&rx);
    assert!(req.contains("\"session_id\":\"sess-x\""));
    assert!(wait_until(|| pending_retaindb_rows(&db)
        .map(|r| r.is_empty())
        .unwrap_or(false)));
    std::thread::sleep(Duration::from_millis(50));
}

#[test]
fn queue_prefetch_builds_full_overlay() {
    if run_isolated("queue_prefetch_builds_full_overlay") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (base, rx) = serve(vec![
        (200, r#"{"results":[{"content":"rel mem"}]}"#),
        (200, r#"{"memories":[{"content":"likes tea"}]}"#),
        (200, r#"{"answer":" synthesized "}"#),
        (200, r#"{"memory_count":2,"persona":"calm"}"#),
    ]);
    let p = plugin_with(state_at(&base, tmp.path()));
    p.queue_prefetch("what do I like?", "s");
    let mut got = String::new();
    assert!(wait_until(|| {
        got = p.prefetch("q", "s");
        !got.is_empty()
    }));
    assert!(got.contains("[RetainDB Context]"));
    assert!(got.contains("- likes tea"));
    assert!(got.contains("- rel mem"));
    assert!(got.contains("[RetainDB User Synthesis]\nsynthesized"));
    assert!(got.contains("[RetainDB Agent Self-Model]\nPersona: calm"));

    assert!(recv(&rx).starts_with("POST /v1/context/query HTTP/1.1"));
    assert!(recv(&rx)
        .starts_with("GET /v1/memory/profile/user-1?project=proj&include_pending=true HTTP/1.1"));
    let ask = recv(&rx);
    assert!(ask.starts_with("POST /v1/memory/profile/user-1/ask HTTP/1.1"));
    assert!(ask.contains("\"reasoning_level\":\"low\""));
    assert!(recv(&rx).starts_with("GET /v1/memory/agent/agent-1/model?project=proj HTTP/1.1"));
}

#[test]
fn prefetch_overlay_error_paths() {
    if run_isolated("prefetch_overlay_error_paths") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (base, rx) = serve(vec![
        (500, "{}"),
        (200, "{}"),
        (500, "{}"),
        (500, "{}"),
        (500, "{}"),
    ]);
    let st = state_at(&base, tmp.path());
    let err = retaindb_prefetch_overlay(&st, "q").unwrap_err();
    assert!(err.starts_with("context query 500"), "{err}");
    let text = retaindb_prefetch_overlay(&st, "q").expect("overlay");
    assert_eq!(
        text,
        "[RetainDB Context]\nProfile:\n- None\nRelevant memories:\n- None"
    );
    for _ in 0..5 {
        let _ = recv(&rx);
    }
}

#[test]
fn build_overlay_truncates_and_limits() {
    if run_isolated("build_overlay_truncates_and_limits") {
        return;
    }

    let long = "a".repeat(300);
    let profile = json!({"memories": [
        {"id": 1},
        {"content": long},
        {"content": "b"},
        {"content": "c"},
        {"content": "d"},
        {"content": "e"}
    ]});
    let out = build_overlay(&profile, &json!({}));
    let expected = format!(
        "[RetainDB Context]\nProfile:\n- {}\n- b\n- c\n- d\nRelevant memories:\n- None",
        "a".repeat(200)
    );
    assert_eq!(out, expected);
    let out2 = build_overlay(
        &json!({}),
        &json!({"results": [{"content": "r1"}, {"x": 1}]}),
    );
    assert_eq!(
        out2,
        "[RetainDB Context]\nProfile:\n- None\nRelevant memories:\n- r1"
    );
}

#[test]
fn urlencoding_escapes_reserved_chars() {
    if run_isolated("urlencoding_escapes_reserved_chars") {
        return;
    }

    assert_eq!(urlencoding_query("a b/c~"), "a%20b%2Fc~");
    assert_eq!(urlencoding_query("Az09-_."), "Az09-_.");
}

include!("retaindb_part2_coverage_gen.rs");
