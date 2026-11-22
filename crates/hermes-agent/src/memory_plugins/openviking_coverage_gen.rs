use super::*;
use crate::memory_manager::MemoryProviderPlugin;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct MockServer {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl MockServer {
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn find_subsequence(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&buf).into_owned(),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let chunked = head.contains("transfer-encoding: chunked");
    loop {
        let complete = if chunked {
            buf.ends_with(b"0\r\n\r\n")
        } else {
            buf.len() >= header_end + content_length
        };
        if complete {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn start_server(status: u16, body: &str) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let body = body.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let recorded = Arc::clone(&recorded);
            let body = body.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let request = read_http_request(&mut stream);
                if request.is_empty() {
                    return;
                }
                recorded.lock().unwrap().push(request);
                let response = format!(
                    "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    MockServer {
        endpoint: format!("http://{addr}"),
        requests,
    }
}

fn unreachable_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}")
}

fn make_state(endpoint: &str, session_id: &str) -> VikingState {
    VikingState {
        client: Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client"),
        endpoint: endpoint.to_string(),
        api_key: "test-key".to_string(),
        account: "acct".to_string(),
        user: "alice".to_string(),
        agent: "hermes".to_string(),
        session_id: session_id.to_string(),
        turn_count: 0,
    }
}

fn plugin_with_state(st: VikingState) -> OpenVikingMemoryPlugin {
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.state.lock().unwrap() = Some(st);
    plugin
}

fn drain(plugin: &OpenVikingMemoryPlugin, sid: &str) -> bool {
    drain_writers_for_session(&plugin.inflight_writers, sid, Duration::from_secs(10))
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).expect("valid json")
}

fn turn_count(plugin: &OpenVikingMemoryPlugin) -> u32 {
    plugin
        .state
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.turn_count)
        .expect("state")
}

#[test]
fn multipart_headers_have_tenant_headers_but_no_content_type() {
    let st = make_state("http://x", "s");
    let h = viking_multipart_headers(&st);
    assert!(h.get("content-type").is_none());
    assert_eq!(
        h.get("x-openviking-account").and_then(|v| v.to_str().ok()),
        Some("acct")
    );
    assert_eq!(
        h.get("x-openviking-user").and_then(|v| v.to_str().ok()),
        Some("alice")
    );
    assert_eq!(
        h.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer test-key")
    );

    let mut no_key = make_state("http://x", "s");
    no_key.api_key = String::new();
    let h = viking_multipart_headers(&no_key);
    assert!(h.get("authorization").is_none());
    assert!(h.get("x-api-key").is_none());
}

#[test]
fn uri_segment_defaults_when_blank() {
    assert_eq!(viking_uri_segment("   "), "default");
    assert_eq!(viking_uri_segment(""), "default");
    assert_eq!(viking_uri_segment("a b"), "a_b");
}

#[test]
fn memory_subdir_for_unknown_target_is_default() {
    assert_eq!(
        memory_subdir_for_target("something-else"),
        DEFAULT_MEMORY_SUBDIR
    );
    assert_eq!(memory_subdir_for_target("memory"), "patterns");
}

#[test]
fn memory_segment_index_handles_user_peer_memories() {
    let parts = ["user", "alice", "peers", "bob", "memories", "x.md"];
    assert_eq!(memory_segment_index(&parts), Some(4));
    let none = ["resources", "x"];
    assert_eq!(memory_segment_index(&none), None);
}

#[test]
fn drain_with_blank_session_returns_true() {
    let writers: InflightWriters = Arc::new(Mutex::new(HashMap::new()));
    assert!(drain_writers_for_session(
        &writers,
        "   ",
        Duration::from_millis(10)
    ));
}

#[test]
fn drain_joins_panicked_writer_and_clears_session() {
    let writers: InflightWriters = Arc::new(Mutex::new(HashMap::new()));
    let handle: std::thread::JoinHandle<()> = std::thread::spawn(|| panic!("writer boom"));
    for _ in 0..200 {
        if handle.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    writers
        .lock()
        .unwrap()
        .insert("s-panic".to_string(), vec![handle]);
    assert!(drain_writers_for_session(
        &writers,
        "s-panic",
        Duration::from_secs(2)
    ));
    assert!(writers.lock().unwrap().is_empty());
}

#[test]
fn commit_session_with_blank_id_is_false() {
    let st = make_state(&unreachable_endpoint(), "   ");
    assert!(!commit_openviking_session(&st));
}

#[test]
fn commit_session_success_and_http_error() {
    let ok = start_server(200, "{}");
    let st = make_state(&ok.endpoint, "s-commit");
    assert!(commit_openviking_session(&st));
    assert!(ok
        .requests()
        .iter()
        .any(|r| r.starts_with("POST /api/v1/sessions/s-commit/commit")));

    let bad = start_server(500, "{}");
    let st = make_state(&bad.endpoint, "s-commit");
    assert!(!commit_openviking_session(&st));
    assert!(!bad.requests().is_empty());
}

#[test]
fn default_plugin_is_uninitialized() {
    let plugin = OpenVikingMemoryPlugin::default();
    assert!(plugin.state.lock().unwrap().is_none());
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
    assert_eq!(plugin.name(), "openviking");
}

#[test]
fn spawn_session_writer_ignores_blank_session() {
    let plugin = OpenVikingMemoryPlugin::new();
    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);
    plugin.spawn_session_writer("  ".to_string(), move || {
        flag_clone.store(true, Ordering::SeqCst);
    });
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
    assert!(!flag.load(Ordering::SeqCst));
}

#[test]
fn initialize_replaces_previous_state() {
    let home = tempfile::tempdir().expect("tempdir");
    let home_str = home.path().to_string_lossy().to_string();
    let expected = OpenVikingConfig::load(&home_str);
    let plugin = plugin_with_state(make_state("http://stale.invalid", "old"));
    plugin.initialize("sess-init", &home_str);
    let guard = plugin.state.lock().unwrap();
    assert!(guard
        .as_ref()
        .is_none_or(|st| st.endpoint == expected.endpoint
            && st.session_id == "sess-init"
            && st.turn_count == 0));
}

#[test]
fn system_prompt_block_depends_on_state() {
    let empty = OpenVikingMemoryPlugin::new();
    assert_eq!(empty.system_prompt_block(), "");

    let plugin = plugin_with_state(make_state("http://viking.local:1933", "s"));
    let block = plugin.system_prompt_block();
    assert!(block.starts_with("# OpenViking Knowledge Base"));
    assert!(block.contains("Endpoint: http://viking.local:1933."));
}

#[test]
fn prefetch_returns_empty_without_state_or_short_query_or_failures() {
    let empty = OpenVikingMemoryPlugin::new();
    assert_eq!(empty.prefetch("what do I like to drink?", "s"), "");

    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    assert_eq!(plugin.prefetch("hi", "s"), "");

    let server = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    assert_eq!(
        plugin.prefetch("what do I like to drink in the morning?", "s"),
        ""
    );
}

#[test]
fn queue_prefetch_is_noop() {
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.prefetch.lock().unwrap() = "keep".to_string();
    plugin.queue_prefetch("some query here", "s");
    assert_eq!(*plugin.prefetch.lock().unwrap(), "keep");
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
}

#[test]
fn sync_turn_skips_blank_user_and_missing_state() {
    let none = OpenVikingMemoryPlugin::new();
    none.sync_turn("hello there", "reply", "s");
    assert!(none.state.lock().unwrap().is_none());
    assert!(none.inflight_writers.lock().unwrap().is_empty());

    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    plugin.sync_turn("   ", "reply", "s");
    assert_eq!(turn_count(&plugin), 0);
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
}

#[test]
fn sync_turn_skips_when_no_session_id_available() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), ""));
    plugin.sync_turn("hello there friend", "reply", "   ");
    assert_eq!(turn_count(&plugin), 0);
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
}

#[test]
fn sync_turn_with_messages_posts_to_server() {
    let server = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "orig"));
    let messages = vec![
        json!({"role": "user", "content": "hello there friend"}),
        json!({"role": "assistant", "content": "sure thing"}),
    ];
    plugin.sync_turn_with_messages("hello there friend", "sure thing", "sess-sync", &messages);
    assert_eq!(turn_count(&plugin), 1);
    assert!(drain(&plugin, "sess-sync"));
    let reqs = server.requests();
    assert!(!reqs.is_empty());
    assert!(reqs.iter().any(|r| r.contains("sess-sync")));
}

#[test]
fn sync_turn_falls_back_when_server_errors() {
    let server = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s-fail"));
    plugin.sync_turn("hello there friend", "sure thing", "");
    assert_eq!(turn_count(&plugin), 1);
    assert!(drain(&plugin, "s-fail"));
    assert!(!server.requests().is_empty());
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
}

#[test]
fn on_session_end_without_state_or_turns_does_nothing() {
    let none = OpenVikingMemoryPlugin::new();
    none.on_session_end(&[]);
    assert!(none.state.lock().unwrap().is_none());

    let server = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s0"));
    plugin.on_session_end(&[]);
    assert!(server.requests().is_empty());
    assert_eq!(turn_count(&plugin), 0);
}

#[test]
fn on_session_end_commits_and_resets_turn_count() {
    let server = start_server(200, "{}");
    let mut st = make_state(&server.endpoint, "s-end");
    st.turn_count = 2;
    let plugin = plugin_with_state(st);
    plugin.on_session_end(&[]);
    assert_eq!(turn_count(&plugin), 0);
    assert!(server
        .requests()
        .iter()
        .any(|r| r.starts_with("POST /api/v1/sessions/s-end/commit")));
}

#[test]
fn on_session_end_keeps_turn_count_on_commit_failure() {
    let server = start_server(500, "{}");
    let mut st = make_state(&server.endpoint, "s-end");
    st.turn_count = 2;
    let plugin = plugin_with_state(st);
    plugin.on_session_end(&[]);
    assert_eq!(turn_count(&plugin), 2);
}

#[test]
fn on_session_switch_early_returns() {
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.prefetch.lock().unwrap() = "cached".to_string();
    plugin.on_session_switch("   ", "", false);
    assert_eq!(*plugin.prefetch.lock().unwrap(), "cached");

    plugin.on_session_switch("new", "", false);
    assert_eq!(*plugin.prefetch.lock().unwrap(), "");
    assert!(plugin.state.lock().unwrap().is_none());

    let mut st = make_state(&unreachable_endpoint(), "s1");
    st.turn_count = 3;
    let plugin = plugin_with_state(st);
    plugin.on_session_switch("s1", "", false);
    assert_eq!(turn_count(&plugin), 3);
    assert_eq!(
        plugin
            .state
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.session_id.clone()),
        Some("s1".to_string())
    );
}

#[test]
fn on_memory_write_ignores_non_add_and_missing_state() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    plugin.on_memory_write("replace", "memory", "likes tea");
    plugin.on_memory_write("add", "memory", "   ");
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());

    let none = OpenVikingMemoryPlugin::new();
    none.on_memory_write("add", "memory", "likes tea");
    assert!(none.inflight_writers.lock().unwrap().is_empty());
}

#[test]
fn on_memory_write_posts_content() {
    let server = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s-mem"));
    plugin.on_memory_write(" ADD ", "memory", "likes green tea");
    assert!(drain(&plugin, "s-mem"));
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/content/write"))
        .expect("write request");
    assert!(req.contains("likes green tea"));
    assert!(req.contains("viking://user/alice/memories/patterns/mem_"));
    assert!(req
        .to_ascii_lowercase()
        .contains("x-openviking-user: alice"));
}

#[test]
fn handle_tool_call_requires_initialization() {
    let plugin = OpenVikingMemoryPlugin::new();
    let out = parse(&plugin.handle_tool_call("viking_search", &json!({"query": "x"})));
    assert_eq!(out["error"], "OpenViking not initialized");
}

#[test]
fn handle_tool_call_unknown_tool() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("nope", &json!({})));
    assert_eq!(out["error"], "Unknown tool: nope");
}

#[test]
fn viking_search_paths() {
    let body =
        r#"{"result":{"memories":[{"uri":"viking://user/alice/memories/preferences/a.md"}]}}"#;
    let server = start_server(200, body);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));

    let out = parse(&plugin.handle_tool_call("viking_search", &json!({"query": ""})));
    assert_eq!(out["error"], "query is required");

    let out = parse(&plugin.handle_tool_call(
        "viking_search",
        &json!({"query": "tea", "mode": "deep", "scope": "viking://user/alice", "limit": 3}),
    ));
    assert_eq!(out, parse(body));
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/search/find"))
        .expect("search request");
    assert!(req.contains("\"mode\":\"deep\""));
    assert!(req.contains("\"target_uri\":\"viking://user/alice\""));
    assert!(req.contains("\"top_k\":3"));

    let bad = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_search", &json!({"query": "tea"})));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 500"));

    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_search", &json!({"query": "tea"})));
    assert!(!out["error"].as_str().unwrap().is_empty());
}

#[test]
fn viking_read_requires_uri() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_read", &json!({})));
    assert_eq!(out["error"], "uri is required");
    let out = parse(&plugin.handle_tool_call("viking_read", &json!({"uris": [], "uri": "  "})));
    assert_eq!(out["error"], "uri is required");
}

#[test]
fn viking_read_single_uri_error_returns_entry() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let uri = "viking://user/alice/memories/preferences/a.md";
    let out = parse(&plugin.handle_tool_call("viking_read", &json!({"uri": format!("  {uri}  ")})));
    assert_eq!(out["uri"], uri);
    assert!(out.get("error").is_some());
}

#[test]
fn viking_read_single_uri_success_returns_result() {
    let server = start_server(200, r#"{"result":"hello content"}"#);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    let out = plugin.handle_tool_call(
        "viking_read",
        &json!({"uri": "viking://user/alice/memories/preferences/a.md"}),
    );
    assert!(out.contains("hello content"));
    assert!(!server.requests().is_empty());
}

#[test]
fn viking_browse_paths() {
    let body = r#"{"result":[{"uri":"viking://user"}]}"#;
    let server = start_server(200, body);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    for (action, path) in [
        ("tree", "/api/v1/fs/tree"),
        ("stat", "/api/v1/fs/stat"),
        ("list", "/api/v1/fs/ls"),
    ] {
        let out = parse(&plugin.handle_tool_call(
            "viking_browse",
            &json!({"action": action, "path": "viking://user"}),
        ));
        assert_eq!(out, parse(body));
        assert!(server
            .requests()
            .iter()
            .any(|r| r.starts_with(&format!("GET {path}?uri="))));
    }

    let bad = start_server(404, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_browse", &json!({})));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 404"));

    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_browse", &json!({"action": "tree"})));
    assert!(!out["error"].as_str().unwrap().is_empty());
}

#[test]
fn viking_remember_paths() {
    let server = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));

    let out = parse(&plugin.handle_tool_call("viking_remember", &json!({"content": ""})));
    assert_eq!(out["error"], "content is required");

    let out = parse(&plugin.handle_tool_call(
        "viking_remember",
        &json!({"content": "Alice works at Acme", "category": "entity"}),
    ));
    assert_eq!(out["status"], "stored");
    let uri = out["uri"].as_str().unwrap().to_string();
    assert!(uri.starts_with("viking://user/alice/memories/entities/mem_"));
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/content/write"))
        .expect("write request");
    assert!(req.contains(&uri));
    assert!(req.contains("Alice works at Acme"));

    let bad = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_remember", &json!({"content": "x"})));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 500"));

    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_remember", &json!({"content": "x"})));
    assert!(!out["error"].as_str().unwrap().is_empty());
}

#[test]
fn viking_forget_paths() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call(
        "viking_forget",
        &json!({"uri": "viking://resources/doc.md"}),
    ));
    assert_eq!(
        out["error"],
        "viking_forget only deletes user memory file URIs"
    );

    let uri = "viking://user/alice/memories/preferences/mem_1.md";
    let out = parse(&plugin.handle_tool_call("viking_forget", &json!({"uri": uri})));
    assert!(!out["error"].as_str().unwrap().is_empty());

    let body = r#"{"result":{"estimated_deleted_count":1,"semantic_status":"queued","uri":"viking://user/alice/memories/preferences/mem_other.md"}}"#;
    let server = start_server(200, body);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_forget", &json!({"uri": uri})));
    assert_eq!(out["status"], "deleted");
    assert_eq!(out["estimated_deleted_count"], 1);
    assert_eq!(out["semantic_status"], "queued");
    assert_eq!(
        out["uri"],
        "viking://user/alice/memories/preferences/mem_other.md"
    );
    assert!(out.get("memory_cleanup").is_none());
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("DELETE /api/v1/fs?"))
        .expect("delete request");
    assert!(req.contains("recursive=false"));

    let plain = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&plain.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_forget", &json!({"uri": uri})));
    assert_eq!(out, json!({"status": "deleted", "uri": uri}));

    let bad = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_forget", &json!({"uri": uri})));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 500"));
}

#[test]
fn viking_add_resource_paths() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_add_resource", &json!({"url": ""})));
    assert_eq!(out["error"], "url is required");

    let out = parse(&plugin.handle_tool_call(
        "viking_add_resource",
        &json!({"url": "/definitely/not/a/real/path/openviking_cov_missing.txt"}),
    ));
    assert!(out.get("error").is_some());

    let out = parse(&plugin.handle_tool_call(
        "viking_add_resource",
        &json!({"url": "https://example.com/doc"}),
    ));
    assert!(!out["error"].as_str().unwrap().is_empty());

    let body = r#"{"status":"ok","root_uri":"viking://resources/doc"}"#;
    let server = start_server(200, body);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call(
        "viking_add_resource",
        &json!({"url": "https://example.com/doc", "reason": "docs"}),
    ));
    assert_eq!(out, parse(body));
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/resources"))
        .expect("resources request");
    assert!(req.contains("https://example.com/doc"));

    let bad = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call(
        "viking_add_resource",
        &json!({"url": "https://example.com/doc"}),
    ));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 500"));
}

#[test]
fn shutdown_drains_writers_and_clears_state() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s1"));
    let handle: std::thread::JoinHandle<()> = std::thread::spawn(|| {});
    plugin
        .inflight_writers
        .lock()
        .unwrap()
        .insert("s1".to_string(), vec![handle]);
    plugin.shutdown();
    assert!(plugin.state.lock().unwrap().is_none());
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
}
