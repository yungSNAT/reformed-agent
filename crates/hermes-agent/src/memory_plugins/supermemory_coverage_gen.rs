use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    body: String,
}

struct MockServer {
    base_url: String,
    requests: Arc<Mutex<Vec<Req>>>,
}

impl MockServer {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&str, &str) -> (u16, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reqs = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = trimmed.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; content_length];
                if reader.read_exact(&mut body).is_err() {
                    continue;
                }
                let body = String::from_utf8_lossy(&body).to_string();
                let (status, resp_body) = handler(&method, &path);
                reqs.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(Req { method, path, body });
                let response = format!(
                    "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let stream = reader.get_mut();
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            requests,
        }
    }

    fn reqs(&self) -> Vec<Req> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn wait_for(&self, n: usize) -> Vec<Req> {
        for _ in 0..60 {
            let r = self.reqs();
            if r.len() >= n {
                return r;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.reqs()
    }
}

fn cfg(base_url: &str) -> SupermemoryConfig {
    SupermemoryConfig {
        api_key: "sm-test".to_string(),
        base_url: base_url.to_string(),
        container_tag: "hermes".to_string(),
        auto_recall: true,
        auto_capture: true,
        max_recall_results: 10,
        search_mode: "hybrid".to_string(),
        api_timeout: 5.0,
    }
}

fn active_plugin(config: SupermemoryConfig) -> SupermemoryMemoryPlugin {
    let p = SupermemoryMemoryPlugin::new();
    *p.config.lock().unwrap() = Some(config);
    *p.active.lock().unwrap() = true;
    *p.session_id.lock().unwrap() = "sess-1".to_string();
    p
}

fn dead_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let a = l.local_addr().expect("addr");
    drop(l);
    format!("http://{a}")
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).expect("valid json")
}

#[test]
fn config_load_reads_all_fields_and_clamps() {
    let _guard = config_io::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("supermemory.json"),
        r#"{"api_key":"  k1  ","container_tag":"a-b","base_url":"http://x.example/","auto_recall":false,"auto_capture":false,"max_recall_results":100,"search_mode":"memories","api_timeout":0.5}"#,
    )
    .expect("write");
    let c = SupermemoryConfig::load(tmp.path().to_str().expect("path"));
    assert_eq!(c.api_key, "k1");
    assert_eq!(c.container_tag, "a_b");
    assert_eq!(c.base_url, "http://x.example");
    assert!(!c.auto_recall);
    assert!(!c.auto_capture);
    assert_eq!(c.max_recall_results, 20);
    assert_eq!(c.search_mode, "memories");
    assert_eq!(c.api_timeout, 1.0);

    std::fs::write(
        tmp.path().join("supermemory.json"),
        r#"{"api_key":"k2","base_url":"http://y.example","search_mode":"bogus","timeout":120,"max_recall_results":0}"#,
    )
    .expect("write");
    let c = SupermemoryConfig::load(tmp.path().to_str().expect("path"));
    assert_eq!(c.api_key, "k2");
    assert_eq!(c.base_url, "http://y.example");
    assert_eq!(c.search_mode, "hybrid");
    assert_eq!(c.api_timeout, 60.0);
    assert_eq!(c.max_recall_results, 1);

    let plugin = SupermemoryMemoryPlugin::new();
    *plugin.prefetch_result.lock().unwrap() = "stale".to_string();
    *plugin.turn_count.lock().unwrap() = 7;
    plugin.initialize("s-42", tmp.path().to_str().expect("path"));
    assert!(*plugin.active.lock().unwrap());
    assert_eq!(*plugin.session_id.lock().unwrap(), "s-42");
    assert_eq!(*plugin.turn_count.lock().unwrap(), 0);
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());
}

#[test]
fn format_prefetch_context_variants() {
    assert_eq!(format_prefetch_context(&[], &[], &[]), "");
    assert_eq!(
        format_prefetch_context(&[], &[], &[json!({"memory": "   "}), json!({"other": 1})]),
        ""
    );
    let out = format_prefetch_context(&[], &["recent".to_string()], &[json!({"text": " t "})]);
    assert!(out.contains("## Recent Context\n- recent"));
    assert!(out.contains("## Relevant Memories\n- t"));
    assert!(!out.contains("User Profile"));
    assert!(out.starts_with("<supermemory-context>"));
}

#[test]
fn default_plugin_and_simple_hooks() {
    let p = SupermemoryMemoryPlugin::default();
    assert!(!*p.active.lock().unwrap());
    assert_eq!(p.system_prompt_block(), "");
    assert_eq!(p.prefetch("hello", "s"), "");
    p.on_turn_start(5, "msg");
    assert_eq!(*p.turn_count.lock().unwrap(), 5);
    p.shutdown();
    assert_eq!(*p.turn_count.lock().unwrap(), 5);
    let err = parse(&p.handle_tool_call("supermemory-save", &json!({"content": "x"})));
    assert_eq!(err["error"], "Supermemory is not configured");
}

#[test]
fn with_source_metadata_non_object_and_none() {
    assert_eq!(
        with_source_metadata(Some(json!("plain"))),
        json!({"value": "plain", "sm_source": "hermes"})
    );
    assert_eq!(with_source_metadata(None), json!({"sm_source": "hermes"}));
}

#[test]
fn system_prompt_block_active() {
    let mut c = cfg("http://unused");
    c.container_tag = "work".to_string();
    let p = active_plugin(c);
    let block = p.system_prompt_block();
    assert!(block.contains("Container: work."));
    assert!(block.contains("supermemory-search"));

    let p2 = SupermemoryMemoryPlugin::new();
    *p2.active.lock().unwrap() = true;
    assert!(p2.system_prompt_block().contains("Container: hermes."));
}

#[test]
fn build_url_trims_slashes() {
    let c = cfg("http://h.example/");
    assert_eq!(
        SupermemoryMemoryPlugin::build_url(&c, "/v3/x"),
        "http://h.example/v3/x"
    );
}

#[test]
fn send_json_empty_invalid_and_refused() {
    let server = MockServer::start(|_m, p| match p {
        "/empty" => (200, String::new()),
        "/bad" => (200, "not json".to_string()),
        _ => (200, "{\"ok\":true}".to_string()),
    });
    let c = cfg(&server.base_url);
    assert_eq!(
        SupermemoryMemoryPlugin::send_json(&c, Method::GET, "/empty", None).unwrap(),
        json!({})
    );
    let err = SupermemoryMemoryPlugin::send_json(&c, Method::GET, "/bad", None).unwrap_err();
    assert!(err.contains("parse error"));
    assert_eq!(
        SupermemoryMemoryPlugin::send_json(&c, Method::POST, "/ok", Some(&json!({"a": 1})))
            .unwrap(),
        json!({"ok": true})
    );
    let reqs = server.reqs();
    assert_eq!(reqs.last().unwrap().body, "{\"a\":1}");

    let dead = cfg(&dead_url());
    let err = SupermemoryMemoryPlugin::send_json(&dead, Method::GET, "/x", None).unwrap_err();
    assert!(err.contains("failed"));
}

#[test]
fn add_memory_falls_back_and_errors() {
    let server = MockServer::start(|_m, p| match p {
        "/v3/memories" => (500, "boom".to_string()),
        _ => (200, "{\"id\":\"m1\"}".to_string()),
    });
    let c = cfg(&server.base_url);
    let v = SupermemoryMemoryPlugin::add_memory(&c, "hello", None, Some("tag2")).unwrap();
    assert_eq!(v["id"], "m1");
    let reqs = server.reqs();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].path, "/v3/memories");
    assert_eq!(reqs[1].path, "/v3/documents");
    let body = parse(&reqs[1].body);
    assert_eq!(body["container_tag"], "tag2");
    assert_eq!(body["metadata"]["sm_source"], "hermes");

    let fail = MockServer::start(|_m, _p| (500, "nope".to_string()));
    let err =
        SupermemoryMemoryPlugin::add_memory(&cfg(&fail.base_url), "x", None, None).unwrap_err();
    assert!(err.contains("/v1/memories"));
    assert!(err.contains("500"));
    assert_eq!(fail.reqs().len(), 3);
}

#[test]
fn search_memories_response_shapes() {
    let server = MockServer::start(|_m, p| match p {
        "/v3/search/memories" => (500, "x".to_string()),
        _ => (200, "{\"results\":[{\"id\":\"a\"}]}".to_string()),
    });
    let c = cfg(&server.base_url);
    let r = SupermemoryMemoryPlugin::search_memories(&c, "q", 100, None).unwrap();
    assert_eq!(r, vec![json!({"id": "a"})]);
    let body = parse(&server.reqs()[1].body);
    assert_eq!(body["limit"], 20);
    assert_eq!(body["container_tag"], "hermes");

    let arr = MockServer::start(|_m, _p| (200, "[{\"id\":\"b\"}]".to_string()));
    let r = SupermemoryMemoryPlugin::search_memories(&cfg(&arr.base_url), "q", 0, None).unwrap();
    assert_eq!(r, vec![json!({"id": "b"})]);
    assert_eq!(parse(&arr.reqs()[0].body)["limit"], 1);

    let obj = MockServer::start(|_m, _p| (200, "{\"x\":1}".to_string()));
    assert!(
        SupermemoryMemoryPlugin::search_memories(&cfg(&obj.base_url), "q", 5, None)
            .unwrap()
            .is_empty()
    );

    let fail = MockServer::start(|_m, _p| (404, "nf".to_string()));
    let err =
        SupermemoryMemoryPlugin::search_memories(&cfg(&fail.base_url), "q", 5, None).unwrap_err();
    assert!(err.contains("/v1/memory/search"));
}

#[test]
fn get_profile_post_then_get_fallback() {
    let server = MockServer::start(|m, _p| {
        if m == "GET" {
            (200, "{\"static\":[\"s\"]}".to_string())
        } else {
            (500, "x".to_string())
        }
    });
    let c = cfg(&server.base_url);
    let v = SupermemoryMemoryPlugin::get_profile(&c, Some("abc"), Some("tg")).unwrap();
    assert_eq!(v["static"][0], "s");
    let reqs = server.reqs();
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[1].method, "GET");
    assert_eq!(reqs[1].path, "/v3/profile?container_tag=tg&q=abc");

    let fail = MockServer::start(|_m, _p| (500, "x".to_string()));
    let err = SupermemoryMemoryPlugin::get_profile(&cfg(&fail.base_url), None, None).unwrap_err();
    assert!(err.contains("/v1/profile"));
    assert_eq!(fail.reqs().len(), 4);
}

#[test]
fn forget_memory_and_ingest() {
    let server = MockServer::start(|_m, p| {
        if p.starts_with("/v3/memories/") {
            (404, "x".to_string())
        } else {
            (200, "{}".to_string())
        }
    });
    let c = cfg(&server.base_url);
    SupermemoryMemoryPlugin::forget_memory(&c, "id1").unwrap();
    let reqs = server.reqs();
    assert_eq!(reqs[0].method, "DELETE");
    assert_eq!(reqs[1].path, "/v1/memories/id1");

    let fail = MockServer::start(|_m, _p| (500, "x".to_string()));
    assert!(SupermemoryMemoryPlugin::forget_memory(&cfg(&fail.base_url), "z").is_err());

    let ok = MockServer::start(|_m, _p| (200, "{}".to_string()));
    SupermemoryMemoryPlugin::ingest_conversation(
        &cfg(&ok.base_url),
        "sid",
        &[json!({"role": "user", "content": "hi"})],
    )
    .unwrap();
    let r = ok.reqs();
    assert_eq!(r[0].path, "/v4/conversations");
    let body = parse(&r[0].body);
    assert_eq!(body["conversationId"], "sid");
    assert_eq!(body["containerTags"][0], "hermes");
}

#[test]
fn extract_profile_lists_shapes() {
    let nested = json!({
        "profile": {"static": ["a", 1, "b"], "dynamic": ["c"]},
        "search_results": {"results": [{"memory": "m"}]}
    });
    let (s, d, r) = SupermemoryMemoryPlugin::extract_profile_lists(&nested);
    assert_eq!(s, vec!["a", "b"]);
    assert_eq!(d, vec!["c"]);
    assert_eq!(r.len(), 1);

    let flat = json!({"static": ["x"], "dynamic": [], "searchResults": [{"a": 1}, {"b": 2}]});
    let (s, d, r) = SupermemoryMemoryPlugin::extract_profile_lists(&flat);
    assert_eq!(s, vec!["x"]);
    assert!(d.is_empty());
    assert_eq!(r.len(), 2);

    let (s, d, r) = SupermemoryMemoryPlugin::extract_profile_lists(&json!({}));
    assert!(s.is_empty() && d.is_empty() && r.is_empty());
}

#[test]
fn handle_tool_call_save() {
    let server = MockServer::start(|_m, _p| (200, "{\"id\":\"n1\"}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    let v = parse(&p.handle_tool_call("supermemory-save", &json!({"content": "  "})));
    assert_eq!(v["error"], "content is required");

    let v = parse(&p.handle_tool_call(
        "supermemory_store",
        &json!({"content": " remember me ", "metadata": "str", "container_tag": "ct"}),
    ));
    assert_eq!(v["saved"], true);
    assert_eq!(v["preview"], "remember me");
    assert_eq!(v["raw"]["id"], "n1");
    let body = parse(&server.reqs()[0].body);
    assert_eq!(body["metadata"], json!({"sm_source": "hermes"}));
    assert_eq!(body["container_tag"], "ct");

    p.handle_tool_call(
        "supermemory-save",
        &json!({"content": "x", "metadata": {"k": "v"}}),
    );
    let body = parse(&server.reqs()[1].body);
    assert_eq!(body["metadata"]["k"], "v");

    let dead = active_plugin(cfg(&dead_url()));
    let v = parse(&dead.handle_tool_call("supermemory-save", &json!({"content": "x"})));
    assert!(v["error"].as_str().unwrap().contains("failed"));
}

#[test]
fn handle_tool_call_search_profile_unknown() {
    let server = MockServer::start(|_m, p| {
        if p.starts_with("/v3/profile") {
            (
                200,
                "{\"static\":[\"a\",\"b\"],\"dynamic\":[\"c\"],\"search_results\":[{\"memory\":\"m\"}]}"
                    .to_string(),
            )
        } else {
            (
                200,
                "{\"results\":[{\"id\":\"1\"},{\"id\":\"2\"}]}".to_string(),
            )
        }
    });
    let p = active_plugin(cfg(&server.base_url));
    let v = parse(&p.handle_tool_call("supermemory-search", &json!({})));
    assert_eq!(v["error"], "query is required");
    let v = parse(&p.handle_tool_call("supermemory_search", &json!({"query": "q", "limit": 3})));
    assert_eq!(v["count"], 2);
    assert_eq!(parse(&server.reqs()[0].body)["limit"], 3);
    p.handle_tool_call("supermemory-search", &json!({"query": "q"}));
    assert_eq!(parse(&server.reqs()[1].body)["limit"], 10);

    let v = parse(&p.handle_tool_call("supermemory-profile", &json!({"query": "who"})));
    assert_eq!(v["static_count"], 2);
    assert_eq!(v["dynamic_count"], 1);
    assert_eq!(v["search_count"], 1);

    let v = parse(&p.handle_tool_call("nope", &json!({})));
    assert_eq!(v["error"], "Unknown tool: nope");

    let dead = active_plugin(cfg(&dead_url()));
    let v = parse(&dead.handle_tool_call("supermemory-search", &json!({"query": "q"})));
    assert!(v.get("error").is_some());
    let v = parse(&dead.handle_tool_call("supermemory_profile", &json!({})));
    assert!(v.get("error").is_some());

    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    let v = parse(&noconf.handle_tool_call("supermemory-search", &json!({"query": "q"})));
    assert_eq!(v["error"], "Supermemory is not configured");
}

#[test]
fn handle_tool_call_forget() {
    let server = MockServer::start(|m, p| {
        if m == "DELETE" {
            (200, "{}".to_string())
        } else if p == "/v3/search/memories" {
            (
                200,
                "{\"results\":[{\"memory\":\"x\"},{\"id\":\"mem-9\"}]}".to_string(),
            )
        } else {
            (500, "x".to_string())
        }
    });
    let p = active_plugin(cfg(&server.base_url));
    let v = parse(&p.handle_tool_call("supermemory-forget", &json!({})));
    assert_eq!(v["error"], "Provide either id or query");
    let v = parse(&p.handle_tool_call("supermemory-forget", &json!({"id": "abc"})));
    assert_eq!(v, json!({"forgotten": true, "id": "abc"}));
    let v = parse(&p.handle_tool_call("supermemory_forget", &json!({"query": "find"})));
    assert_eq!(v, json!({"forgotten": true, "id": "mem-9"}));
    assert!(server
        .reqs()
        .iter()
        .any(|r| r.method == "DELETE" && r.path == "/v3/memories/mem-9"));

    let noid = MockServer::start(|_m, _p| (200, "{\"results\":[{\"memory\":\"x\"}]}".to_string()));
    let p2 = active_plugin(cfg(&noid.base_url));
    let v = parse(&p2.handle_tool_call("supermemory-forget", &json!({"query": "q"})));
    assert_eq!(v["error"], "No matching memory id found");

    let delfail = MockServer::start(|m, _p| {
        if m == "DELETE" {
            (500, "x".to_string())
        } else {
            (200, "[{\"id\":\"q1\"}]".to_string())
        }
    });
    let p3 = active_plugin(cfg(&delfail.base_url));
    let v = parse(&p3.handle_tool_call("supermemory-forget", &json!({"query": "q"})));
    assert!(v["error"].as_str().unwrap().contains("500"));

    let dead = active_plugin(cfg(&dead_url()));
    let v = parse(&dead.handle_tool_call("supermemory-forget", &json!({"id": "z"})));
    assert!(v.get("error").is_some());
    let v = parse(&dead.handle_tool_call("supermemory-forget", &json!({"query": "z"})));
    assert!(v.get("error").is_some());
}

#[test]
fn prefetch_paths() {
    let server = MockServer::start(|_m, _p| {
        (
            200,
            "{\"profile\":{\"static\":[\"likes tea\"],\"dynamic\":[\"debugging\"]},\"searchResults\":{\"results\":[{\"content\":\"remember x\"}]}}"
                .to_string(),
        )
    });
    let p = active_plugin(cfg(&server.base_url));
    *p.prefetch_result.lock().unwrap() = "queued ctx".to_string();
    assert_eq!(p.prefetch("ignored", "s"), "queued ctx");
    assert!(p.prefetch_result.lock().unwrap().is_empty());

    assert_eq!(p.prefetch("   ", "s"), "");
    assert!(server.reqs().is_empty());

    let long = "a".repeat(300);
    let out = p.prefetch(&long, "s");
    assert!(out.contains("likes tea"));
    assert!(out.contains("## Recent Context\n- debugging"));
    assert!(out.contains("remember x"));
    let body = parse(&server.reqs()[0].body);
    assert_eq!(body["q"].as_str().unwrap().len(), 200);

    let mut c = cfg(&server.base_url);
    c.auto_recall = false;
    let off = active_plugin(c);
    assert_eq!(off.prefetch("q", "s"), "");

    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    assert_eq!(noconf.prefetch("q", "s"), "");

    let dead = active_plugin(cfg(&dead_url()));
    assert_eq!(dead.prefetch("q", "s"), "");
}

#[test]
fn sync_turn_posts_in_background() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    p.sync_turn(" hi there ", " hello back ", "s");
    let reqs = server.wait_for(1);
    assert_eq!(reqs.len(), 1);
    let body = parse(&reqs[0].body);
    let content = body["content"].as_str().unwrap();
    assert!(content.contains("[role: user]\nhi there\n[user:end]"));
    assert!(content.contains("[role: assistant]\nhello back\n[assistant:end]"));
    assert_eq!(body["metadata"]["type"], "conversation_turn");
}

#[test]
fn sync_turn_early_returns() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let inactive = SupermemoryMemoryPlugin::new();
    *inactive.config.lock().unwrap() = Some(cfg(&server.base_url));
    inactive.sync_turn("u", "a", "s");
    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    noconf.sync_turn("u", "a", "s");
    let mut c = cfg(&server.base_url);
    c.auto_capture = false;
    active_plugin(c).sync_turn("u", "a", "s");
    let p = active_plugin(cfg(&server.base_url));
    p.sync_turn("  ", "a", "s");
    p.sync_turn("u", "", "s");
    std::thread::sleep(Duration::from_millis(100));
    assert!(server.reqs().is_empty());
}

#[test]
fn on_session_end_ingests_cleaned_messages() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    p.on_session_end(&[
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": " hi "}),
        json!({"role": "assistant", "content": "hello"}),
        json!({"role": "user", "content": "  "}),
        json!({"content": "no role"}),
    ]);
    let reqs = server.wait_for(1);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/v4/conversations");
    let body = parse(&reqs[0].body);
    assert_eq!(body["conversationId"], "sess-1");
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": "hi"}, {"role": "assistant", "content": "hello"}])
    );
}

#[test]
fn on_session_end_early_returns() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let msgs = [json!({"role": "user", "content": "hi"})];
    let inactive = SupermemoryMemoryPlugin::new();
    *inactive.config.lock().unwrap() = Some(cfg(&server.base_url));
    inactive.on_session_end(&msgs);
    let p = active_plugin(cfg(&server.base_url));
    p.on_session_end(&[]);
    p.on_session_end(&[json!({"role": "system", "content": "x"})]);
    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    noconf.on_session_end(&msgs);
    std::thread::sleep(Duration::from_millis(100));
    assert!(server.reqs().is_empty());
}

#[test]
fn on_memory_write_mirrors_adds() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    p.on_memory_write("add", "memory", "  a fact  ");
    let reqs = server.wait_for(1);
    assert_eq!(reqs.len(), 1);
    let body = parse(&reqs[0].body);
    assert_eq!(body["content"], "a fact");
    assert_eq!(body["metadata"]["source"], "hermes_memory");
    assert_eq!(body["metadata"]["type"], "explicit_memory");
}

#[test]
fn on_memory_write_early_returns() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    p.on_memory_write("remove", "memory", "x");
    p.on_memory_write("add", "memory", "   ");
    let inactive = SupermemoryMemoryPlugin::new();
    *inactive.config.lock().unwrap() = Some(cfg(&server.base_url));
    inactive.on_memory_write("add", "memory", "x");
    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    noconf.on_memory_write("add", "memory", "x");
    std::thread::sleep(Duration::from_millis(100));
    assert!(server.reqs().is_empty());
}
