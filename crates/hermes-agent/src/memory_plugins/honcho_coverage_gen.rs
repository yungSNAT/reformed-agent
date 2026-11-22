use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct Recorded {
    request_line: String,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Recorded {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            let len = head
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        v.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            while buf.len() < pos + 4 + len {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body = String::from_utf8_lossy(&buf[pos + 4..]).to_string();
            let request_line = head.lines().next().unwrap_or("").to_string();
            return Recorded { request_line, body };
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => {
                return Recorded {
                    request_line: String::from_utf8_lossy(&buf).to_string(),
                    body: String::new(),
                }
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn spawn_server(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<Recorded>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let req = read_request(&mut stream);
            let reason = if status == 200 { "OK" } else { "Error" };
            let resp = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
            let _ = tx.send(req);
        }
    });
    (format!("http://{addr}"), rx)
}

fn test_config(base_url: &str) -> HonchoConfig {
    HonchoConfig {
        api_key: String::new(),
        base_url: base_url.to_string(),
        enabled: true,
        recall_mode: "hybrid".to_string(),
        context_tokens: Some(800),
        workspace_id: "ws".to_string(),
        peer_name: None,
        ai_peer: "hermes".to_string(),
        pin_user_peer: false,
        user_peer_aliases: HashMap::new(),
        runtime_peer_prefix: String::new(),
        timeout_secs: 10.0,
        endpoints: HashMap::new(),
        host_had_explicit_api_key: false,
        host: "hermes".to_string(),
        config_path: PathBuf::from("/nonexistent-honcho-dir/honcho.json"),
        oauth: None,
    }
}

fn plugin_with(config: HonchoConfig) -> HonchoMemoryPlugin {
    let plugin = HonchoMemoryPlugin::new();
    *plugin.config.lock().unwrap() = Some(config);
    *plugin.session_key.lock().unwrap() = "sess-1".to_string();
    plugin
}

fn cred(access: &str, expires_at: f64) -> HonchoOAuthCredential {
    HonchoOAuthCredential {
        access_token: access.to_string(),
        refresh_token: "refresh-1".to_string(),
        expires_at,
        client_id: "client-1".to_string(),
        token_endpoint: "http://127.0.0.1:9/token".to_string(),
        scope: "write".to_string(),
        token_type: "Bearer".to_string(),
    }
}

fn parse(s: String) -> Value {
    serde_json::from_str(&s).expect("valid json output")
}

fn wait_for<F: Fn() -> bool>(f: F) -> bool {
    for _ in 0..1000 {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// OAuth credential parsing
// ---------------------------------------------------------------------------

#[test]
fn from_host_block_rejects_non_oauth_token() {
    let block = json!({
        "apiKey": "plain-key",
        "oauth": {"refreshToken": "r", "tokenEndpoint": "e", "clientId": "c"}
    });
    assert!(HonchoOAuthCredential::from_host_block(&block).is_none());
}

#[test]
fn from_host_block_rejects_blank_fields() {
    let block = json!({
        "apiKey": "hch-at-abc",
        "oauth": {"refreshToken": "   ", "tokenEndpoint": "e", "clientId": "c"}
    });
    assert!(HonchoOAuthCredential::from_host_block(&block).is_none());
}

#[test]
fn from_host_block_parses_string_expiry_and_defaults() {
    let block = json!({
        "apiKey": "hch-at-abc",
        "oauth": {"refreshToken": "r", "tokenEndpoint": "https://e/token", "clientId": "c", "expiresAt": "1234.5"}
    });
    let parsed = HonchoOAuthCredential::from_host_block(&block).expect("credential");
    assert_eq!(parsed.expires_at, 1234.5);
    assert_eq!(parsed.scope, "write");
    assert_eq!(parsed.token_type, "Bearer");
    assert_eq!(parsed.access_token, "hch-at-abc");
}

#[test]
fn json_number_or_string_variants() {
    assert_eq!(
        json_number_or_string_as_f64(Some(&json!("12.5"))),
        Some(12.5)
    );
    assert_eq!(json_number_or_string_as_f64(Some(&json!("nope"))), None);
    assert_eq!(json_number_or_string_as_f64(Some(&json!(true))), None);
    assert_eq!(json_number_or_string_as_f64(None), None);
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

#[test]
fn from_config_file_reads_top_level_oauth_and_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("honcho.json");
    std::fs::write(
        &path,
        json!({
            "apiKey": "hch-at-top",
            "oauth": {"refreshToken": "r", "tokenEndpoint": "https://auth.example/token", "clientId": "c", "expiresAt": 999},
            "baseUrl": "https://example.test/v1",
            "recallMode": "auto",
            "contextTokens": 10,
            "workspace": "ws-x",
            "endpoints": {"search": "/custom/search", "blank": "  ", "num": 5},
            "enabled": true
        })
        .to_string(),
    )
    .unwrap();
    let config = HonchoConfig::from_config_file(dir.path().to_str().unwrap());
    let oauth = config.oauth.as_ref().expect("oauth credential");
    assert_eq!(oauth.access_token, "hch-at-top");
    assert_eq!(oauth.refresh_token, "r");
    assert_eq!(config.config_path, path);
    assert_eq!(config.base_url, "https://example.test");
    assert_eq!(config.api_key, "hch-at-top");
    assert_eq!(config.recall_mode, "hybrid");
    assert_eq!(config.context_tokens, Some(32));
    assert_eq!(config.workspace_id, "ws-x");
    assert_eq!(
        config.endpoints.get("search").map(String::as_str),
        Some("/custom/search")
    );
    assert!(!config.endpoints.contains_key("blank"));
    assert!(!config.endpoints.contains_key("num"));
    assert!(config.enabled);
}

#[test]
fn apply_config_value_handles_edge_values() {
    let mut config = test_config("");
    HonchoConfig::apply_config_value(
        &mut config,
        &json!({
            "recallMode": "weird",
            "contextTokens": 100000,
            "workspace": "w2",
            "userPeerAliases": {"good": " peer-a ", "  ": "x", "empty": "  ", "num": 3},
            "endpoints": {"flush": "/f", "bad": ""},
            "timeout": 0.1
        }),
    );
    assert_eq!(config.recall_mode, "hybrid");
    assert_eq!(config.context_tokens, Some(4000));
    assert_eq!(config.workspace_id, "w2");
    assert_eq!(config.user_peer_aliases.len(), 1);
    assert_eq!(
        config.user_peer_aliases.get("good").map(String::as_str),
        Some("peer-a")
    );
    assert_eq!(
        config.endpoints.get("flush").map(String::as_str),
        Some("/f")
    );
    assert!(!config.endpoints.contains_key("bad"));
    assert_eq!(config.timeout_secs, 1.0);
    assert!(!config.enabled);

    HonchoConfig::apply_config_value(&mut config, &json!({"recallMode": "tools"}));
    assert_eq!(config.recall_mode, "tools");
    HonchoConfig::apply_config_value(&mut config, &json!({"recallMode": "auto"}));
    assert_eq!(config.recall_mode, "hybrid");
}

// ---------------------------------------------------------------------------
// OAuth refresh
// ---------------------------------------------------------------------------

#[test]
fn refresh_skips_when_api_key_is_not_oauth() {
    let mut config = test_config("https://x.test");
    config.api_key = "plain".to_string();
    config.oauth = Some(cred("hch-at-a", 0.0));
    assert_eq!(config.refresh_oauth_if_needed(), Ok(false));
    assert_eq!(config.api_key, "plain");
    assert_eq!(config.oauth, Some(cred("hch-at-a", 0.0)));
}

#[test]
fn refresh_uses_cached_unexpired_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("honcho.json");
    let host = "cache-host-coverage";
    seed_oauth_cache(
        &path,
        host,
        &cred("hch-at-cached", epoch_seconds() + 10_000.0),
    );
    let mut config = test_config("https://x.test");
    config.config_path = path;
    config.host = host.to_string();
    config.api_key = "hch-at-stale".to_string();
    config.oauth = Some(cred("hch-at-stale", 0.0));
    assert_eq!(config.refresh_oauth_if_needed(), Ok(false));
    assert_eq!(config.api_key, "hch-at-cached");
}

#[test]
fn persist_oauth_replaces_non_object_hosts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("honcho.json");
    std::fs::write(&path, r#"{"hosts": 5, "other": 1}"#).unwrap();
    let credential = cred("hch-at-new", 5000.0);
    persist_oauth_credential(&path, "hermes", &credential).expect("persist");
    assert_eq!(read_oauth_credential(&path, "hermes"), Some(credential));
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw["other"], json!(1));
    assert_eq!(raw["hosts"]["hermes"]["apiKey"], json!("hch-at-new"));
}

#[test]
fn persist_oauth_replaces_non_object_host_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("honcho.json");
    std::fs::write(
        &path,
        r#"{"hosts": {"hermes": "str", "keep": {"apiKey": "k"}}}"#,
    )
    .unwrap();
    let credential = cred("hch-at-b", 7000.0);
    persist_oauth_credential(&path, "hermes", &credential).expect("persist");
    assert_eq!(read_oauth_credential(&path, "hermes"), Some(credential));
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw["hosts"]["keep"]["apiKey"], json!("k"));
}

#[test]
fn config_refresh_lock_acquires_and_fails_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let ok_path = dir.path().join("honcho.json");
    {
        let lock = ConfigRefreshLock::acquire(&ok_path);
        assert!(lock.file.is_some());
    }
    assert!(dir.path().join("honcho.json.lock").exists());

    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "x").unwrap();
    let bad_path = blocker.join("honcho.json");
    let lock = ConfigRefreshLock::acquire(&bad_path);
    assert!(lock.file.is_none());
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

#[test]
fn host_key_helpers() {
    assert_eq!(profile_host_key(Some("!!!")), "hermes_profile");
    assert_eq!(legacy_profile_host_key("hermes_  "), None);
    assert_eq!(
        legacy_profile_host_key("hermes_work"),
        Some("hermes.work".to_string())
    );
}

#[test]
fn base_url_helpers() {
    assert_eq!(normalize_honcho_base_url("ftp://host"), "");
    assert_eq!(strip_honcho_base_url_version("nohost"), "nohost");
    assert_eq!(
        strip_honcho_base_url_version("https://x.test/vbeta"),
        "https://x.test/vbeta"
    );
    assert_eq!(
        strip_honcho_base_url_version("https://x.test/v"),
        "https://x.test/v"
    );
}

#[test]
fn resolve_user_peer_id_paths() {
    let mut config = test_config("");
    config.pin_user_peer = true;
    config.peer_name = Some("   ".to_string());
    assert_eq!(
        HonchoMemoryPlugin::resolve_user_peer_id(&config, "telegram:42", &[]),
        "user-telegram-42"
    );

    let mut config = test_config("");
    config.peer_name = Some(" Bob Smith ".to_string());
    assert_eq!(
        HonchoMemoryPlugin::resolve_user_peer_id(&config, "x:y", &[]),
        "Bob-Smith"
    );

    let mut config = test_config("");
    config
        .user_peer_aliases
        .insert("rt1".to_string(), "  ".to_string());
    assert_eq!(
        HonchoMemoryPlugin::resolve_user_peer_id(&config, "", &["rt1".to_string()]),
        "rt1"
    );
    assert_eq!(
        HonchoMemoryPlugin::resolve_user_peer_id(&config, "", &["a b".to_string()]),
        "a-b"
    );
}

#[test]
fn explicit_user_peer_ids_collects_peer_and_aliases() {
    let mut config = test_config("");
    config.peer_name = Some(" Bob ".to_string());
    config
        .user_peer_aliases
        .insert("x".to_string(), "carol".to_string());
    config
        .user_peer_aliases
        .insert("y".to_string(), "  ".to_string());
    assert_eq!(
        explicit_user_peer_ids(&config),
        vec!["Bob".to_string(), "carol".to_string()]
    );
}

#[test]
fn generated_runtime_peer_id_variants() {
    let config = test_config("");
    assert_eq!(generated_runtime_peer_id(&config, "p-", "abc"), "p-abc");

    let hex = format!("{:x}", Sha256::digest(b"p-abc"));

    let mut config = test_config("");
    config.peer_name = Some("p-abc".to_string());
    config
        .user_peer_aliases
        .insert("a".to_string(), format!("p-abc-{}", &hex[..8]));
    assert_eq!(
        generated_runtime_peer_id(&config, "p-", "abc"),
        format!("p-abc-{}", &hex[..12])
    );

    let mut config = test_config("");
    config.peer_name = Some("p-abc".to_string());
    for len in PEER_ID_HASH_ESCALATION_LENGTHS {
        config
            .user_peer_aliases
            .insert(format!("k{len}"), format!("p-abc-{}", &hex[..*len]));
    }
    assert_eq!(
        generated_runtime_peer_id(&config, "p-", "abc"),
        format!("p-abc-{hex}")
    );
}

#[test]
fn extract_text_result_variants() {
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"result": "r"})),
        Some("r".to_string())
    );
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"context": "c"})),
        Some("c".to_string())
    );
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"answer": "a"})),
        Some("a".to_string())
    );
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"results": [
            {"memory": " m1 "}, {"content": "c2"}, {"text": "t3"}, {"text": "  "}, {"other": 1}
        ]})),
        Some("m1\nc2\nt3".to_string())
    );
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"results": [{"text": " "}]})),
        None
    );
    assert_eq!(HonchoMemoryPlugin::extract_text_result(&json!({})), None);
}

// ---------------------------------------------------------------------------
// Plugin lifecycle
// ---------------------------------------------------------------------------

#[test]
fn default_plugin_and_simple_accessors() {
    let plugin = HonchoMemoryPlugin::default();
    assert_eq!(plugin.name(), "honcho");
    assert!(plugin.config.lock().unwrap().is_none());
    assert_eq!(plugin.recall_mode.lock().unwrap().as_str(), "hybrid");

    let expected = dirs::home_dir()
        .map(|home| vec![home.join(".honcho")])
        .unwrap_or_default();
    assert_eq!(plugin.backup_paths(), expected);

    plugin.on_turn_start(7, "hi");
    assert_eq!(*plugin.turn_count.lock().unwrap(), 7);

    let schema = plugin.get_config_schema().expect("schema");
    let arr = schema.as_array().expect("array");
    assert_eq!(arr.len(), 8);
    assert_eq!(arr[0]["key"], json!("api_key"));
    assert_eq!(arr[3]["default"], json!(DEFAULT_TIMEOUT_SECS));
}

#[test]
fn shutdown_keeps_state() {
    let plugin = plugin_with(test_config("https://x.test"));
    plugin.shutdown();
    assert!(plugin.config.lock().unwrap().is_some());
    assert_eq!(plugin.session_key.lock().unwrap().as_str(), "sess-1");
}

#[test]
fn initialize_disabled_leaves_plugin_inactive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("honcho.json"), r#"{"enabled": false}"#).unwrap();
    let plugin = HonchoMemoryPlugin::new();
    plugin.initialize("s1", dir.path().to_str().unwrap());
    assert!(plugin.config.lock().unwrap().is_none());
    assert_eq!(plugin.session_key.lock().unwrap().as_str(), "");
    let out = parse(plugin.handle_tool_call("honcho_profile", &json!({})));
    assert_eq!(
        out["error"],
        json!("Honcho is not active for this session.")
    );
}

#[test]
fn prefetch_modes() {
    let plugin = HonchoMemoryPlugin::new();
    *plugin.recall_mode.lock().unwrap() = "tools".to_string();
    *plugin.prefetch_result.lock().unwrap() = "stored".to_string();
    assert_eq!(plugin.prefetch("q", "s"), "");
    assert_eq!(plugin.prefetch_result.lock().unwrap().as_str(), "stored");

    *plugin.recall_mode.lock().unwrap() = "hybrid".to_string();
    assert_eq!(plugin.prefetch("q", "s"), "## Honcho Context\nstored");
    assert_eq!(plugin.prefetch("q", "s"), "");
}

#[test]
fn on_session_switch_updates_state() {
    let plugin = plugin_with(test_config("https://x.test"));
    *plugin.turn_count.lock().unwrap() = 5;
    *plugin.prefetch_result.lock().unwrap() = "old".to_string();
    plugin.on_session_switch("   ", "parent", false);
    assert_eq!(plugin.session_key.lock().unwrap().as_str(), "sess-1");
    assert_eq!(*plugin.turn_count.lock().unwrap(), 5);

    plugin.on_session_switch("  new-sess ", "parent", true);
    assert_eq!(plugin.session_key.lock().unwrap().as_str(), "new-sess");
    assert_eq!(*plugin.turn_count.lock().unwrap(), 0);
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());
}

#[test]
fn queue_prefetch_skips_and_fetches() {
    let plugin = HonchoMemoryPlugin::new();
    plugin.queue_prefetch("question", "s");
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());

    let (url, rx) = spawn_server(vec![(200, r#"{"result":"user likes tea"}"#)]);
    let plugin = plugin_with(test_config(&url));
    *plugin.recall_mode.lock().unwrap() = "tools".to_string();
    plugin.queue_prefetch("question", "sess-1");
    *plugin.recall_mode.lock().unwrap() = "hybrid".to_string();
    plugin.queue_prefetch("    ", "sess-1");
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());

    plugin.queue_prefetch("  what do they like  ", "sess-1");
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/context/query"));
    assert!(req.body.contains("\"query\":\"what do they like\""));
    assert!(wait_for(|| !plugin
        .prefetch_result
        .lock()
        .unwrap()
        .is_empty()));
    assert_eq!(plugin.prefetch("", ""), "## Honcho Context\nuser likes tea");
}

#[test]
fn sync_turn_skips_and_posts_messages() {
    let plugin = HonchoMemoryPlugin::new();
    plugin.sync_turn("hi", "there", "s");
    assert!(plugin.config.lock().unwrap().is_none());

    let (url, rx) = spawn_server(vec![(200, "{}")]);
    let mut config = test_config(&url);
    config
        .endpoints
        .insert("messages".to_string(), "/m/{session}".to_string());
    let plugin = plugin_with(config);
    plugin.sync_turn("hello", "   ", "sess-1");
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());

    plugin.sync_turn("  hello  ", " world ", "sess-1");
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /m/sess-1"));
    assert!(req.body.contains("\"content\":\"hello\""));
    assert!(req.body.contains("\"content\":\"world\""));
    assert!(req.body.contains("\"workspace_id\":\"ws\""));
}

#[test]
fn on_memory_write_mirrors_user_additions() {
    let plugin = HonchoMemoryPlugin::new();
    plugin.on_memory_write("add", "user", "fact");
    assert!(plugin.config.lock().unwrap().is_none());

    let (url, rx) = spawn_server(vec![(200, "{}")]);
    let plugin = plugin_with(test_config(&url));
    plugin.on_memory_write("remove", "user", "fact");
    plugin.on_memory_write("add", "memory", "fact");
    plugin.on_memory_write("add", "user", "   ");
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());

    plugin.on_memory_write("add", "user", "  likes tea  ");
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/conclusions"));
    assert!(req.body.contains("\"conclusion\":\"likes tea\""));
    assert!(req.body.contains("memory_write_hook"));
}

#[test]
fn on_session_end_flushes_session() {
    let plugin = HonchoMemoryPlugin::new();
    plugin.on_session_end(&[]);
    assert!(plugin.session_flush().is_none());

    let (url, rx) = spawn_server(vec![(500, "boom")]);
    let mut config = test_config(&url);
    config.api_key = "secret".to_string();
    let plugin = plugin_with(config);
    plugin.on_session_end(&[]);
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req
        .request_line
        .starts_with("POST /v1/sessions/sess-1/flush"));
    assert!(req.body.contains("\"session_id\":\"sess-1\""));
}

#[test]
fn prepare_session_end_runs_flush_future() {
    let plugin = HonchoMemoryPlugin::new();
    assert!(plugin.prepare_session_end(&[]).is_none());

    let (url, rx) = spawn_server(vec![(500, "nope")]);
    let plugin = plugin_with(test_config(&url));
    let fut = plugin.prepare_session_end(&[]).expect("future");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(fut);
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req
        .request_line
        .starts_with("POST /v1/sessions/sess-1/flush"));
}

// ---------------------------------------------------------------------------
// Tool calls
// ---------------------------------------------------------------------------

#[test]
fn handle_tool_call_unknown_tool() {
    let plugin = plugin_with(test_config("https://x.test"));
    let out = parse(plugin.handle_tool_call("nope", &json!({})));
    assert_eq!(out["error"], json!("Unknown tool: nope"));
}

#[test]
fn handle_tool_call_profile_variants() {
    let (url, rx) = spawn_server(vec![
        (200, r#"{"card":["likes tea","lives in Oslo"]}"#),
        (200, r#"{"card":[]}"#),
        (200, r#"{"ok":true}"#),
        (500, "boom"),
    ]);
    let plugin = plugin_with(test_config(&url));

    let out = parse(plugin.handle_tool_call("honcho_profile", &json!({"peer": "alice"})));
    assert_eq!(out["count"], json!(2));
    assert_eq!(out["result"], json!(["likes tea", "lives in Oslo"]));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("GET /v1/peers/alice/card?"));
    assert!(req.request_line.contains("workspace_id=ws"));
    assert!(req.request_line.contains("peer=alice"));

    let out = parse(plugin.handle_tool_call("honcho_profile", &json!({"peer": "alice"})));
    assert_eq!(out["count"], json!(0));
    assert_eq!(out["result"], json!([]));
    assert!(out.get("hint").is_some());
    rx.recv_timeout(WAIT).expect("request");

    let out = parse(plugin.handle_tool_call(
        "honcho_profile",
        &json!({"peer": "alice", "card": ["new fact"]}),
    ));
    assert_eq!(out["result"], json!({"ok": true}));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/peers/alice/card"));
    assert!(req.body.contains("\"card\":[\"new fact\"]"));

    let out = parse(plugin.handle_tool_call("honcho_profile", &json!({"peer": "alice"})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho profile failed"));
}

#[test]
fn handle_tool_call_search_variants() {
    let (url, rx) = spawn_server(vec![
        (200, r#"{"results":[{"memory":"m1"},{"content":"c2"}]}"#),
        (200, ""),
        (500, "x"),
    ]);
    let plugin = plugin_with(test_config(&url));

    let out = parse(plugin.handle_tool_call("honcho_search", &json!({"peer": "alice"})));
    assert_eq!(out["error"], json!("Missing required parameter: query"));

    let out = parse(plugin.handle_tool_call(
        "honcho_search",
        &json!({"peer": "alice", "query": "tea", "max_tokens": 5}),
    ));
    assert_eq!(out["result"], json!("m1\nc2"));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/context/search"));
    assert!(req.body.contains("\"max_tokens\":64"));

    let out =
        parse(plugin.handle_tool_call("honcho_search", &json!({"peer": "alice", "query": "tea"})));
    assert_eq!(out["result"], json!("No relevant context found."));
    assert_eq!(out["raw"], json!({}));

    let out =
        parse(plugin.handle_tool_call("honcho_search", &json!({"peer": "alice", "query": "tea"})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho search failed"));
}

#[test]
fn handle_tool_call_context_variants() {
    let (url, rx) = spawn_server(vec![
        (200, r#"{"answer":"they like tea"}"#),
        (200, r#"{"results":[]}"#),
        (500, "x"),
    ]);
    let plugin = plugin_with(test_config(&url));

    let out = parse(plugin.handle_tool_call("honcho_context", &json!({"peer": "ai"})));
    assert_eq!(out["error"], json!("Missing required parameter: query"));

    let out =
        parse(plugin.handle_tool_call("honcho_context", &json!({"peer": "ai", "query": "what?"})));
    assert_eq!(out["result"], json!("they like tea"));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/context/query"));
    assert!(req.body.contains("\"peer\":\"hermes\""));
    assert!(req.body.contains("\"max_tokens\":800"));

    let out =
        parse(plugin.handle_tool_call("honcho_context", &json!({"peer": "ai", "query": "what?"})));
    assert_eq!(out["result"], json!("No result from Honcho."));

    let out =
        parse(plugin.handle_tool_call("honcho_context", &json!({"peer": "ai", "query": "what?"})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho context failed"));
}

#[test]
fn handle_tool_call_conclude_variants() {
    let (url, rx) = spawn_server(vec![
        (200, "{}"),
        (500, "x"),
        (200, r#"{"id":"n1"}"#),
        (500, "y"),
    ]);
    let plugin = plugin_with(test_config(&url));

    let out = parse(plugin.handle_tool_call("honcho_conclude", &json!({"peer": "alice"})));
    assert_eq!(out["error"], json!("Provide conclusion or delete_id"));

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "conclusion": "c", "delete_id": "d"}),
    ));
    assert_eq!(
        out["error"],
        json!("Provide either conclusion or delete_id, not both")
    );

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "delete_id": " c-1 "}),
    ));
    assert_eq!(out["result"], json!("Conclusion deleted"));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("DELETE /v1/conclusions/c-1?"));
    assert!(req.request_line.contains("peer=alice"));

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "delete_id": "c-2"}),
    ));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho delete failed"));
    rx.recv_timeout(WAIT).expect("request");

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "conclusion": " likes tea "}),
    ));
    assert_eq!(out["result"], json!("Conclusion saved."));
    assert_eq!(out["raw"], json!({"id": "n1"}));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/conclusions"));
    assert!(req.body.contains("\"conclusion\":\"likes tea\""));

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "conclusion": "x"}),
    ));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho conclude failed"));
}

#[test]
fn context_query_returns_parsed_value() {
    let (url, rx) = spawn_server(vec![(200, r#"{"context":"ctx"}"#)]);
    let config = test_config(&url);
    let value = HonchoMemoryPlugin::context_query(&config, "sess-9", "q", 128, "bob")
        .expect("context query");
    assert_eq!(value, json!({"context": "ctx"}));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.body.contains("\"session_id\":\"sess-9\""));
    assert!(req.body.contains("\"peer\":\"bob\""));
    assert!(req.body.contains("\"max_tokens\":128"));
}
