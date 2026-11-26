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

include!("openviking_part2_coverage_gen.rs");
