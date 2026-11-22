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

#[allow(unused_unsafe)]
fn set_env(key: &str, value: Option<&OsStr>) {
    unsafe {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
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

#[test]
fn self_model_rendering_edge_cases() {
    if run_isolated("self_model_rendering_edge_cases") {
        return;
    }

    assert_eq!(retaindb_agent_self_model_from_value(&json!({})), "");
    assert_eq!(
        retaindb_agent_self_model_from_value(&json!({
            "memory_count": 1,
            "persona": "  ",
            "persistent_instructions": ["  "],
            "working_style": " "
        })),
        ""
    );
    assert_eq!(
        retaindb_agent_self_model_from_value(&json!({
            "memory_count": 1,
            "persistent_instructions": ["one", " two "],
            "working_style": " calm "
        })),
        "[RetainDB Agent Self-Model]\nInstructions:\n- one\n- two\nWorking style: calm"
    );
}

#[test]
fn queue_handles_bad_paths_and_invalid_payloads() {
    if run_isolated("queue_handles_bad_paths_and_invalid_payloads") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, "x").expect("write");
    let bad = blocker.join("sub").join("q.db");
    ensure_retaindb_queue(&bad);
    assert!(!bad.exists());
    assert_eq!(enqueue_retaindb_turn(&bad, &json!({"a": 1})), None);
    assert!(pending_retaindb_rows(&bad).is_err());

    let mut st = state_at("http://127.0.0.1:9", tmp.path());
    st.queue_db_path = bad.clone();
    drain_retaindb_queue(&st);
    assert!(pending_retaindb_rows(&bad).is_err());

    let dir_path = tmp.path().join("adir");
    std::fs::create_dir_all(&dir_path).expect("mkdir");
    ensure_retaindb_queue(&dir_path);
    assert_eq!(enqueue_retaindb_turn(&dir_path, &json!({"a": 1})), None);

    let db = tmp.path().join("q.db");
    ensure_retaindb_queue(&db);
    let conn = Connection::open(&db).expect("open");
    conn.execute(
        "INSERT INTO pending (payload_json, created_at) VALUES (?1, ?2)",
        params!["not json", "now"],
    )
    .expect("insert");
    let rows = pending_retaindb_rows(&db).expect("rows");
    assert_eq!(rows, vec![(1, json!({}))]);
}

#[test]
fn drain_stops_on_failure_and_marks_error() {
    if run_isolated("drain_stops_on_failure_and_marks_error") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (base, rx) = serve(vec![(500, "{}")]);
    let st = state_at(&base, tmp.path());
    let id1 = enqueue_retaindb_turn(&st.queue_db_path, &json!({"messages": []})).expect("id1");
    let id2 = enqueue_retaindb_turn(&st.queue_db_path, &json!({"messages": []})).expect("id2");
    drain_retaindb_queue(&st);
    assert!(recv(&rx).starts_with("POST /v1/memory/ingest/session HTTP/1.1"));
    let rows = pending_retaindb_rows(&st.queue_db_path).expect("rows");
    assert_eq!(rows.len(), 2);
    let conn = Connection::open(&st.queue_db_path).expect("open");
    let e1: Option<String> = conn
        .query_row(
            "SELECT last_error FROM pending WHERE id = ?1",
            params![id1],
            |r| r.get(0),
        )
        .expect("e1");
    assert!(e1.expect("marked").starts_with("ingest 500"));
    let e2: Option<String> = conn
        .query_row(
            "SELECT last_error FROM pending WHERE id = ?1",
            params![id2],
            |r| r.get(0),
        )
        .expect("e2");
    assert_eq!(e2, None);
}

#[test]
fn drain_flushes_all_rows_on_success() {
    if run_isolated("drain_flushes_all_rows_on_success") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (base, rx) = serve(vec![(200, "{}"), (200, "{}")]);
    let st = state_at(&base, tmp.path());
    enqueue_retaindb_turn(&st.queue_db_path, &json!({"session_id": "a"})).expect("id1");
    enqueue_retaindb_turn(&st.queue_db_path, &json!({"session_id": "b"})).expect("id2");
    drain_retaindb_queue(&st);
    assert!(recv(&rx).contains("\"session_id\":\"a\""));
    assert!(recv(&rx).contains("\"session_id\":\"b\""));
    assert!(pending_retaindb_rows(&st.queue_db_path)
        .expect("rows")
        .is_empty());
}

#[test]
fn mime_types_cover_known_extensions() {
    if run_isolated("mime_types_cover_known_extensions") {
        return;
    }

    let cases = [
        ("a.TXT", "text/plain"),
        ("b.text", "text/plain"),
        ("c.md", "text/markdown"),
        ("d.markdown", "text/markdown"),
        ("e.json", "application/json"),
        ("f.csv", "text/csv"),
        ("g.yaml", "application/yaml"),
        ("h.yml", "application/yaml"),
        ("i.xml", "application/xml"),
        ("j.html", "text/html"),
        ("k.htm", "text/html"),
        ("l.pdf", "application/pdf"),
        ("m.png", "image/png"),
        ("n.jpg", "image/jpeg"),
        ("o.jpeg", "image/jpeg"),
        ("p.gif", "image/gif"),
        ("q.webp", "image/webp"),
        ("r.bin", "application/octet-stream"),
        ("noext", "application/octet-stream"),
    ];
    for (name, mime) in cases {
        assert_eq!(retain_file_mime_type(Path::new(name)), mime, "{name}");
    }
}

#[test]
fn file_text_detection() {
    if run_isolated("file_text_detection") {
        return;
    }

    assert!(retain_file_is_text(
        &json!({"mime_type": "application/json"})
    ));
    assert!(retain_file_is_text(&json!({"mimeType": "TEXT/x-custom"})));
    assert!(retain_file_is_text(
        &json!({"mime_type": "application/x-yaml"})
    ));
    assert!(retain_file_is_text(&json!({"name": "a.CSV"})));
    assert!(retain_file_is_text(&json!({"filename": "x.yml"})));
    assert!(!retain_file_is_text(
        &json!({"mime_type": "image/png", "name": "x.png"})
    ));
    assert!(!retain_file_is_text(&json!({})));
}

#[test]
fn upload_local_validation() {
    if run_isolated("upload_local_validation") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let st = state_at("http://127.0.0.1:9", tmp.path());
    let err = tool_dispatch(
        &st,
        "retaindb_upload_file",
        &json!({"local_path": tmp.path().display().to_string()}),
    )
    .unwrap_err();
    assert!(err.starts_with("Not a regular file"), "{err}");

    let file = tmp.path().join("doc.txt");
    std::fs::write(&file, "data").expect("write");
    let err = tool_dispatch(
        &st,
        "retaindb_upload_file",
        &json!({"local_path": file.display().to_string(), "scope": "team"}),
    )
    .unwrap_err();
    assert_eq!(err, "scope must be USER, PROJECT, or ORG");
}

#[test]
fn upload_failures_and_ingest_variants() {
    if run_isolated("upload_failures_and_ingest_variants") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("doc.txt");
    std::fs::write(&file, "data").expect("write");
    let (base, rx) = serve(vec![
        (500, "{}"),
        (200, r#"{"ok":true}"#),
        (200, r#"{"file":{"id":"f 1"}}"#),
        (500, "{}"),
        (200, r#"{"ingested":1}"#),
    ]);
    let st = state_at(&base, tmp.path());
    let path = file.display().to_string();

    let err = tool_dispatch(&st, "retaindb_upload_file", &json!({"local_path": path})).unwrap_err();
    assert!(err.starts_with("upload file 500"), "{err}");
    let up1 = recv(&rx);
    assert!(up1.starts_with("POST /v1/files HTTP/1.1"));
    assert!(up1.contains("/doc.txt"));

    let ok = tool_dispatch(
        &st,
        "retaindb_upload_file",
        &json!({"local_path": path, "ingest": true}),
    )
    .expect("upload ok");
    assert_eq!(ok, json!({"ok": true}));
    let _ = recv(&rx);

    let err = tool_dispatch(
        &st,
        "retaindb_upload_file",
        &json!({"local_path": path, "ingest": true}),
    )
    .unwrap_err();
    assert!(err.starts_with("ingest file 500"), "{err}");
    let _ = recv(&rx);
    assert!(recv(&rx).starts_with("POST /v1/files/f%201/ingest HTTP/1.1"));

    let ingested =
        tool_dispatch(&st, "retaindb_ingest_file", &json!({"file_id": "f2"})).expect("ingest");
    assert_eq!(ingested["ingested"], 1);
    assert!(recv(&rx).starts_with("POST /v1/files/f2/ingest HTTP/1.1"));
}

#[test]
fn memory_tools_success_and_failure() {
    if run_isolated("memory_tools_success_and_failure") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (base, rx) = serve(vec![
        (200, r#"{"memories":[{"content":"m1"}]}"#),
        (500, "{}"),
        (200, r#"{"results":[]}"#),
        (500, "{}"),
        (200, r#"{"results":[{"content":"r1"}]}"#),
        (200, r#"{"memories":[]}"#),
        (500, "{}"),
        (200, r#"{"id":"mem-1"}"#),
        (500, "{}"),
        (200, ""),
        (500, "{}"),
    ]);
    let st = state_at(&base, tmp.path());
    let p = plugin_with(st.clone());

    let profile: Value =
        serde_json::from_str(&p.handle_tool_call("retaindb_profile", &json!({}))).expect("json");
    assert_eq!(profile["memories"][0]["content"], "m1");
    let preq = recv(&rx);
    assert!(preq
        .starts_with("GET /v1/memory/profile/user-1?project=proj&include_pending=true HTTP/1.1"));
    assert!(preq.to_ascii_lowercase().contains("x-api-key: test-token"));
    let err = tool_dispatch(&st, "retaindb_profile", &json!({})).unwrap_err();
    assert!(err.starts_with("profile 500"), "{err}");
    let _ = recv(&rx);

    let found = tool_dispatch(
        &st,
        "retaindb_search",
        &json!({"query": "tea", "top_k": 100}),
    )
    .expect("search");
    assert_eq!(found["results"], json!([]));
    let sreq = recv(&rx);
    assert!(sreq.starts_with("POST /v1/memory/search HTTP/1.1"));
    assert!(sreq.contains("\"top_k\":20"));
    let err = tool_dispatch(&st, "retaindb_search", &json!({"query": "tea"})).unwrap_err();
    assert!(err.starts_with("search 500"), "{err}");
    let _ = recv(&rx);

    let ctx = tool_dispatch(&st, "retaindb_context", &json!({"query": "tea"})).expect("ctx");
    let ctx_text = ctx["context"].as_str().expect("context");
    assert!(ctx_text.contains("Profile:\n- None"));
    assert!(ctx_text.contains("- r1"));
    assert_eq!(ctx["raw"]["results"][0]["content"], "r1");
    assert!(recv(&rx).starts_with("POST /v1/context/query HTTP/1.1"));
    assert!(recv(&rx).starts_with("GET /v1/memory/profile/"));
    let err = tool_dispatch(&st, "retaindb_context", &json!({"query": "tea"})).unwrap_err();
    assert!(err.starts_with("context 500"), "{err}");
    let _ = recv(&rx);

    let rem = tool_dispatch(&st, "retaindb_remember", &json!({"content": "likes tea"}))
        .expect("remember");
    assert_eq!(rem["id"], "mem-1");
    let rreq = recv(&rx);
    assert!(rreq.starts_with("POST /v1/memory HTTP/1.1"));
    assert!(rreq.contains("\"memory_type\":\"factual\""));
    assert!(rreq.contains("\"importance\":0.7"));
    let err = tool_dispatch(&st, "retaindb_remember", &json!({"content": "x"})).unwrap_err();
    assert!(err.starts_with("remember 500"), "{err}");
    let _ = recv(&rx);

    let forgot =
        tool_dispatch(&st, "retaindb_forget", &json!({"memory_id": "a/b"})).expect("forget");
    assert_eq!(forgot, json!({"ok": true}));
    assert!(recv(&rx).starts_with("DELETE /v1/memory/a%2Fb HTTP/1.1"));
    let err = tool_dispatch(&st, "retaindb_forget", &json!({"memory_id": "m"})).unwrap_err();
    assert!(err.starts_with("forget 500"), "{err}");
    let _ = recv(&rx);
}

#[test]
fn file_store_tools_error_and_binary_paths() {
    if run_isolated("file_store_tools_error_and_binary_paths") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let (base, rx) = serve(vec![
        (500, "{}"),
        (500, "{}"),
        (200, r#"{"file":{"name":"a.txt"}}"#),
        (500, "{}"),
        (
            200,
            r#"{"file":{"name":"img.png","mime_type":"image/png","rdb_uri":"rdb://img"}}"#,
        ),
        (200, "PNGDATA"),
        (200, r#"{"filename":"notes.txt","rdb_uri":"rdb://n"}"#),
        (200, "plain text"),
        (500, "{}"),
    ]);
    let st = state_at(&base, tmp.path());

    let err = tool_dispatch(&st, "retaindb_list_files", &json!({})).unwrap_err();
    assert!(err.starts_with("list files 500"), "{err}");
    assert!(recv(&rx).starts_with("GET /v1/files?limit=50 HTTP/1.1"));

    let err = tool_dispatch(&st, "retaindb_read_file", &json!({"file_id": "f1"})).unwrap_err();
    assert!(err.starts_with("read file metadata 500"), "{err}");
    let _ = recv(&rx);

    let err = tool_dispatch(&st, "retaindb_read_file", &json!({"file_id": "f1"})).unwrap_err();
    assert!(err.starts_with("read file content 500"), "{err}");
    let _ = recv(&rx);
    assert!(recv(&rx).starts_with("GET /v1/files/f1/content HTTP/1.1"));

    let bin = tool_dispatch(&st, "retaindb_read_file", &json!({"file_id": "img"})).expect("bin");
    assert_eq!(bin["content"], Value::Null);
    assert_eq!(bin["name"], "img.png");
    assert_eq!(bin["rdb_uri"], "rdb://img");
    assert_eq!(bin["file_id"], "img");
    assert!(bin["note"].as_str().expect("note").contains("Binary file"));
    let _ = recv(&rx);
    let _ = recv(&rx);

    let txt = tool_dispatch(&st, "retaindb_read_file", &json!({"file_id": "n"})).expect("txt");
    assert_eq!(txt["content"], "plain text");
    assert_eq!(txt["name"], "notes.txt");
    assert_eq!(txt["rdb_uri"], "rdb://n");
    assert_eq!(txt["truncated"], json!(false));
    let _ = recv(&rx);
    let _ = recv(&rx);

    let err = tool_dispatch(&st, "retaindb_delete_file", &json!({"file_id": "f1"})).unwrap_err();
    assert!(err.starts_with("delete file 500"), "{err}");
    assert!(recv(&rx).starts_with("DELETE /v1/files/f1 HTTP/1.1"));
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path
        .split_once("::")
        .map_or(path.as_str(), |(_, rest)| rest);
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
    true
}
