use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn clear_mem0_env() -> Vec<EnvVarGuard> {
    [
        "MEM0_HOST",
        "MEM0_BASE_URL",
        "MEM0_MODE",
        "MEM0_API_KEY",
        "MEM0_USER_ID",
        "MEM0_AGENT_ID",
    ]
    .into_iter()
    .map(EnvVarGuard::remove)
    .collect()
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..pos]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= pos + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).to_string()
}

fn mock_server(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let request = read_request(&mut stream);
            let _ = tx.send(request);
            let reason = if status < 400 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}"), rx)
}

fn dead_base_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}")
}

fn cfg(base_url: String) -> Mem0Config {
    Mem0Config {
        mode: "oss".to_string(),
        api_key: String::new(),
        user_id: "u1".to_string(),
        agent_id: "a1".to_string(),
        base_url,
        api_timeout_secs: 5.0,
        rerank: true,
    }
}

fn plugin_with(config: Mem0Config) -> Mem0MemoryPlugin {
    let plugin = Mem0MemoryPlugin::new();
    *plugin.config.lock().unwrap() = Some(config);
    plugin
}

fn recv(rx: &mpsc::Receiver<String>) -> String {
    rx.recv_timeout(Duration::from_secs(5)).expect("request")
}

fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    cond()
}

fn parse(out: &str) -> Value {
    serde_json::from_str(out).expect("json output")
}

#[test]
fn load_reads_env_base_url_and_file_overrides() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::load_reads_env_base_url_and_file_overrides"
        );
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

    let _lock = config_io::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_str().expect("utf8").to_string();
    let _cleared = clear_mem0_env();
    let _base = EnvVarGuard::set("MEM0_BASE_URL", "http://env.example/api/");

    let c = Mem0Config::load(&home);
    assert_eq!(c.base_url, "http://env.example/api");
    assert_eq!(c.mode, "platform");
    assert_eq!(c.user_id, "hermes-user");
    assert_eq!(c.agent_id, "hermes");

    let path = tmp.path().join("mem0.json");
    std::fs::write(
        &path,
        r#"{"api_key":"k-file","user_id":"file-user","agent_id":"file-agent","mode":"local","base_url":"http://file.example/","api_timeout_secs":120,"rerank":true}"#,
    )
    .expect("write");
    let c = Mem0Config::load(&home);
    assert_eq!(c.api_key, "k-file");
    assert_eq!(c.user_id, "file-user");
    assert_eq!(c.agent_id, "file-agent");
    assert_eq!(c.mode, "oss");
    assert_eq!(c.base_url, "http://file.example");
    assert!((c.api_timeout_secs - 60.0).abs() < f64::EPSILON);
    assert!(c.rerank);

    std::fs::write(
        &path,
        r#"{"api_key":"","host":"http://host.example/","timeout":0.5,"mode":"  "}"#,
    )
    .expect("write");
    let c = Mem0Config::load(&home);
    assert_eq!(c.api_key, "");
    assert_eq!(c.base_url, "http://host.example");
    assert!((c.api_timeout_secs - 1.0).abs() < f64::EPSILON);
    assert_eq!(c.mode, "platform");
    assert!(!c.rerank);

    std::fs::write(&path, r#"{"base_url":""}"#).expect("write");
    let c = Mem0Config::load(&home);
    assert_eq!(c.base_url, "http://env.example/api");

    std::fs::write(&path, "not json").expect("write");
    let c = Mem0Config::load(&home);
    assert_eq!(c.base_url, "http://env.example/api");
    assert_eq!(c.user_id, "hermes-user");
}

#[test]
fn initialize_loads_config_into_plugin() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::initialize_loads_config_into_plugin");
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

    let _lock = config_io::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let _cleared = clear_mem0_env();
    std::fs::write(
        tmp.path().join("mem0.json"),
        r#"{"user_id":"init-user","base_url":"http://127.0.0.1:9/"}"#,
    )
    .expect("write");

    let plugin = Mem0MemoryPlugin::new();
    assert!(plugin.config_snapshot().is_err());
    plugin.initialize("s1", tmp.path().to_str().expect("utf8"));
    let snap = plugin.config_snapshot().expect("initialized");
    assert_eq!(snap.user_id, "init-user");
    assert_eq!(snap.base_url, "http://127.0.0.1:9");
    let block = plugin.system_prompt_block();
    assert!(block.contains("User: init-user"));
    assert!(block.contains("self-hosted HTTP at http://127.0.0.1:9"));
}

#[test]
fn mode_label_variants() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::mode_label_variants");
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

    let mut c = cfg("http://localhost:1".to_string());
    assert_eq!(mem0_mode_label(&c), "OSS/self-hosted");
    c.mode = "platform".to_string();
    c.base_url = "https://api.mem0.ai/".to_string();
    assert_eq!(mem0_mode_label(&c), "platform");
    c.base_url = MEM0_CLOUD_BASE_URL.to_string();
    let plugin = plugin_with(c);
    assert!(plugin.system_prompt_block().contains("Mode: platform."));
}

#[test]
fn validate_memory_id_rejects_empty() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::validate_memory_id_rejects_empty");
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

    let err = validate_mem0_memory_id("   ").unwrap_err();
    assert_eq!(err, "Missing required parameter: memory_id");
    assert!(validate_mem0_memory_id("a#b").is_err());
}

#[test]
fn default_plugin_is_uninitialized() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::default_plugin_is_uninitialized");
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

    let plugin = Mem0MemoryPlugin::default();
    assert_eq!(
        plugin.config_snapshot().unwrap_err(),
        "Mem0 plugin is not initialized"
    );
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 0);
    plugin.shutdown();
    assert!(plugin.config_snapshot().is_err());
}

#[test]
fn breaker_without_deadline_and_with_expired_deadline() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::breaker_without_deadline_and_with_expired_deadline"
        );
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

    let plugin = Mem0MemoryPlugin::new();
    plugin
        .consecutive_failures
        .store(BREAKER_THRESHOLD, Ordering::Relaxed);
    assert!(!plugin.is_breaker_open());
    assert_eq!(
        plugin.consecutive_failures.load(Ordering::Relaxed),
        BREAKER_THRESHOLD
    );

    *plugin.breaker_open_until.lock().unwrap() = Some(Instant::now());
    assert!(!plugin.is_breaker_open());
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 0);
}

struct CountingSubscriber {
    events: Arc<AtomicUsize>,
}

impl tracing::Subscriber for CountingSubscriber {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, _: &tracing::Event<'_>) {
        self.events.fetch_add(1, Ordering::SeqCst);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn breaker_trips_and_blocks_operations() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::breaker_trips_and_blocks_operations");
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

    let events = Arc::new(AtomicUsize::new(0));
    let subscriber = CountingSubscriber {
        events: Arc::clone(&events),
    };
    let plugin = plugin_with(cfg(dead_base_url()));
    tracing::subscriber::with_default(subscriber, || {
        for _ in 0..BREAKER_THRESHOLD {
            plugin.record_failure();
        }
        plugin.shutdown();
    });
    assert!(events.load(Ordering::SeqCst) >= 1);
    assert!(plugin.is_breaker_open());

    let out = parse(&plugin.handle_tool_call("mem0_list", &json!({})));
    assert!(out["error"].as_str().unwrap().contains("circuit breaker"));

    plugin.queue_prefetch("hello", "s");
    plugin.sync_turn("u", "a", "s");
    assert_eq!(
        plugin.consecutive_failures.load(Ordering::Relaxed),
        BREAKER_THRESHOLD
    );
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());
}

#[test]
fn unwrap_results_other_keys() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::unwrap_results_other_keys");
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

    assert_eq!(
        Mem0MemoryPlugin::unwrap_results(json!({"memories":[1, 2]})).len(),
        2
    );
    assert_eq!(
        Mem0MemoryPlugin::unwrap_results(json!({"items":[1]})).len(),
        1
    );
    assert_eq!(
        Mem0MemoryPlugin::unwrap_results(json!({"data":[1, 2, 3]})).len(),
        3
    );
    assert!(Mem0MemoryPlugin::unwrap_results(json!({"other":1})).is_empty());
    assert!(Mem0MemoryPlugin::unwrap_results(json!("str")).is_empty());
}

#[test]
fn send_json_auth_query_and_empty_body() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::send_json_auth_query_and_empty_body");
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

    let (base, rx) = mock_server(vec![(200, "")]);
    let mut c = cfg(base);
    c.api_key = "k1".to_string();
    let q = vec![("a", "b".to_string())];
    let v = Mem0MemoryPlugin::send_json(&c, Method::GET, "/memories", None, Some(&q)).expect("ok");
    assert_eq!(v, json!({}));
    let req = recv(&rx);
    assert!(req.starts_with("GET /memories?a=b "));
    let lower = req.to_ascii_lowercase();
    assert!(lower.contains("authorization: bearer k1"));
    assert!(lower.contains("x-api-key: k1"));
}

#[test]
fn send_json_non_success_status() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::send_json_non_success_status");
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

    let (base, rx) = mock_server(vec![(500, "boom")]);
    let c = cfg(base);
    let err =
        Mem0MemoryPlugin::send_json(&c, Method::POST, "x", Some(&json!({})), None).unwrap_err();
    assert!(err.contains("returned 500"));
    assert!(err.contains("boom"));
    assert!(recv(&rx).starts_with("POST /x "));
}

#[test]
fn search_memories_success_fallback_and_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::search_memories_success_fallback_and_failure"
        );
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

    let (base, rx) = mock_server(vec![(200, r#"{"results":[{"memory":"m1"}]}"#)]);
    let res = Mem0MemoryPlugin::search_memories(&cfg(base), "q", false, 99).expect("ok");
    assert_eq!(res.len(), 1);
    let req = recv(&rx);
    assert!(req.starts_with("POST /memories/search "));
    assert!(req.contains("\"top_k\":50"));
    assert!(req.contains("\"user_id\":\"u1\""));

    let (base, rx) = mock_server(vec![(404, ""), (200, "[]")]);
    let res = Mem0MemoryPlugin::search_memories(&cfg(base), "q", true, 3).expect("ok");
    assert!(res.is_empty());
    assert!(recv(&rx).starts_with("POST /memories/search "));
    assert!(recv(&rx).starts_with("POST /memory/search "));

    let err = Mem0MemoryPlugin::search_memories(&cfg(dead_base_url()), "q", false, 1).unwrap_err();
    assert!(err.contains("Mem0 request POST"));
}

#[test]
fn list_memories_success_fallback_and_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::list_memories_success_fallback_and_failure"
        );
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

    let (base, rx) = mock_server(vec![(200, r#"{"memories":[{"memory":"x"}]}"#)]);
    let res = Mem0MemoryPlugin::list_memories(&cfg(base), 0, 500).expect("ok");
    assert_eq!(res.len(), 1);
    let req = recv(&rx);
    assert!(req.starts_with("GET /memories?"));
    assert!(req.contains("page=1"));
    assert!(req.contains("page_size=200"));
    assert!(req.contains("user_id=u1"));
    assert!(req.contains("agent_id=a1"));

    let (base, rx) = mock_server(vec![
        (404, ""),
        (404, ""),
        (200, r#"{"results":[{"memory":"y"}]}"#),
    ]);
    let res = Mem0MemoryPlugin::list_memories(&cfg(base), 1, 10).expect("ok");
    assert_eq!(res.len(), 1);
    assert!(recv(&rx).starts_with("GET /memories?"));
    assert!(recv(&rx).starts_with("GET /memory?"));
    assert!(recv(&rx).starts_with("POST /memories/search "));

    let err = Mem0MemoryPlugin::list_memories(&cfg(dead_base_url()), 1, 10).unwrap_err();
    assert!(err.contains("fallback search failed"));
}

#[test]
fn add_messages_success_and_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::add_messages_success_and_failure");
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

    let (base, rx) = mock_server(vec![(200, r#"{"ok":true}"#)]);
    Mem0MemoryPlugin::add_messages(
        &cfg(base),
        vec![json!({"role":"user","content":"hi"})],
        Some(false),
    )
    .expect("ok");
    let req = recv(&rx);
    assert!(req.starts_with("POST /memories "));
    assert!(req.contains("\"infer\":false"));

    let err = Mem0MemoryPlugin::add_messages(&cfg(dead_base_url()), vec![], None).unwrap_err();
    assert!(err.contains("Mem0 request POST"));
}

#[test]
fn update_and_delete_errors() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::update_and_delete_errors");
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

    let c = cfg(dead_base_url());
    assert_eq!(
        Mem0MemoryPlugin::update_memory(&c, "m1", "  ").unwrap_err(),
        "Missing required parameter: text"
    );
    assert!(Mem0MemoryPlugin::update_memory(&c, "m1", "text")
        .unwrap_err()
        .contains("Mem0 request"));
    assert!(Mem0MemoryPlugin::delete_memory(&c, "m1")
        .unwrap_err()
        .contains("Mem0 request"));
}

#[test]
fn prefetch_returns_and_clears_result() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::prefetch_returns_and_clears_result");
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

    let plugin = Mem0MemoryPlugin::new();
    assert_eq!(plugin.prefetch("q", "s"), "");
    *plugin.prefetch_result.lock().unwrap() = "- x".to_string();
    assert_eq!(plugin.prefetch("q", "s"), "## Mem0 Memory\n- x");
    assert_eq!(plugin.prefetch("q", "s"), "");
}

#[test]
fn queue_prefetch_early_returns() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::queue_prefetch_early_returns");
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

    let plugin = Mem0MemoryPlugin::new();
    plugin.queue_prefetch("   ", "s");
    plugin.queue_prefetch("query", "s");
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 0);
}

#[test]
fn queue_prefetch_success_populates_result() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::queue_prefetch_success_populates_result");
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

    let (base, rx) = mock_server(vec![(
        200,
        r#"{"results":[{"memory":"fact one"},{"other":1}]}"#,
    )]);
    let plugin = plugin_with(cfg(base));
    plugin.queue_prefetch("  topic  ", "s");
    assert!(wait_until(|| !plugin
        .prefetch_result
        .lock()
        .unwrap()
        .is_empty()));
    let req = recv(&rx);
    assert!(req.contains("\"query\":\"topic\""));
    assert_eq!(plugin.prefetch("q", "s"), "## Mem0 Memory\n- fact one");
}

#[test]
fn queue_prefetch_failure_records_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::queue_prefetch_failure_records_failure");
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

    let plugin = plugin_with(cfg(dead_base_url()));
    plugin.queue_prefetch("topic", "s");
    assert!(wait_until(|| plugin
        .consecutive_failures
        .load(Ordering::Relaxed)
        == 1));
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());
}

#[test]
fn sync_turn_early_returns() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::sync_turn_early_returns");
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

    let plugin = Mem0MemoryPlugin::new();
    plugin.sync_turn("  ", "a", "s");
    plugin.sync_turn("u", " ", "s");
    plugin.sync_turn("u", "a", "s");
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 0);
}

#[test]
fn sync_turn_success_sends_truncated_messages() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::sync_turn_success_sends_truncated_messages"
        );
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

    let (base, rx) = mock_server(vec![(200, r#"{"ok":true}"#)]);
    let plugin = plugin_with(cfg(base));
    let long_user = "a".repeat(5000);
    plugin.sync_turn(&long_user, "assistant reply", "s");
    let req = recv(&rx);
    assert!(req.starts_with("POST /memories "));
    assert!(req.contains(&"a".repeat(4000)));
    assert!(!req.contains(&"a".repeat(4001)));
    assert!(req.contains("assistant reply"));
}

#[test]
fn sync_turn_failure_records_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::sync_turn_failure_records_failure");
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

    let plugin = plugin_with(cfg(dead_base_url()));
    plugin.sync_turn("user", "assistant", "s");
    assert!(wait_until(|| plugin
        .consecutive_failures
        .load(Ordering::Relaxed)
        == 1));
}

#[test]
fn handle_tool_call_without_config_and_missing_params() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::handle_tool_call_without_config_and_missing_params"
        );
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

    let plugin = Mem0MemoryPlugin::new();
    let not_init = "Mem0 plugin is not initialized";
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_list", &json!({})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_search", &json!({"query":"q"})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_search", &json!({})))["error"],
        "Missing required parameter: query"
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_add", &json!({})))["error"],
        "Missing required parameter: content"
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_add", &json!({"content":"c"})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_update", &json!({"memory_id":"m"})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_delete", &json!({"memory_id":"m"})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("nope", &json!({})))["error"],
        "Unknown tool: nope"
    );
}

#[test]
fn handle_list_success_variants() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::handle_list_success_variants");
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

    let (base, rx) = mock_server(vec![(
        200,
        r#"{"results":[{"id":"m1","memory":"likes tea"},{"id":"m2"}]}"#,
    )]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_list", &json!({"page":2,"page_size":5})));
    assert_eq!(out["count"], 1);
    assert_eq!(out["results"][0]["id"], "m1");
    assert_eq!(out["results"][0]["memory"], "likes tea");
    assert_eq!(out["page"], 2);
    assert_eq!(out["page_size"], 5);
    let req = recv(&rx);
    assert!(req.contains("page=2"));

    let (base, _rx) = mock_server(vec![(200, r#"[{"memory":"a"},{"content":"b"}]"#)]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_profile", &json!({})));
    assert_eq!(out["result"], "a\nb");
    assert_eq!(out["count"], 2);

    let (base, _rx) = mock_server(vec![(200, "[]")]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_list", &json!({})));
    assert_eq!(out["result"], "No memories stored yet.");
}

#[test]
fn handle_list_failure_records_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::handle_list_failure_records_failure");
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

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_list", &json!({})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Failed to fetch profile:"));
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 1);
}

#[test]
fn handle_search_success_variants() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::handle_search_success_variants");
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

    let (base, rx) = mock_server(vec![(
        200,
        r#"{"results":[{"id":"s1","memory":"x","score":0.9},{"id":"s2","content":"y","similarity":0.5},{"id":"s3"}]}"#,
    )]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call(
        "mem0_search",
        &json!({"query":"q","rerank":false,"top_k":100}),
    ));
    assert_eq!(out["count"], 2);
    assert_eq!(out["results"][0]["id"], "s1");
    assert_eq!(out["results"][0]["score"], 0.9);
    assert_eq!(out["results"][1]["memory"], "y");
    assert_eq!(out["results"][1]["score"], 0.5);
    let req = recv(&rx);
    assert!(req.contains("\"rerank\":false"));
    assert!(req.contains("\"top_k\":50"));

    let (base, rx) = mock_server(vec![(200, r#"{"results":[]}"#)]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_search", &json!({"query":"q"})));
    assert_eq!(out["result"], "No relevant memories found.");
    let req = recv(&rx);
    assert!(req.contains("\"rerank\":true"));
    assert!(req.contains("\"top_k\":10"));
}

#[test]
fn handle_search_failure_records_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::handle_search_failure_records_failure");
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

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_search", &json!({"query":"q"})));
    assert!(out["error"].as_str().unwrap().starts_with("Search failed:"));
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 1);
}

#[test]
fn handle_add_success_and_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::handle_add_success_and_failure");
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

    let (base, rx) = mock_server(vec![(200, r#"{"ok":true}"#)]);
    let plugin = plugin_with(cfg(base));
    plugin.consecutive_failures.store(2, Ordering::Relaxed);
    let out = parse(&plugin.handle_tool_call("mem0_conclude", &json!({"conclusion":"fact z"})));
    assert_eq!(out["result"], "Fact stored.");
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 0);
    let req = recv(&rx);
    assert!(req.contains("fact z"));
    assert!(req.contains("\"infer\":false"));

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_add", &json!({"content":"c"})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Failed to store:"));
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 1);
}

#[test]
fn handle_update_success_and_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::handle_update_success_and_failure");
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

    let (base, rx) = mock_server(vec![(200, r#"{"status":"ok"}"#)]);
    let plugin = plugin_with(cfg(base));
    let out =
        parse(&plugin.handle_tool_call("mem0_update", &json!({"memory_id":"m9","text":"new"})));
    assert_eq!(out["result"], "Memory updated.");
    assert_eq!(out["memory_id"], "m9");
    assert_eq!(out["response"]["status"], "ok");
    assert!(recv(&rx).starts_with("PATCH /memories/m9 "));

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_update", &json!({"memory_id":"m9"})));
    assert_eq!(
        out["error"],
        "Update failed: Missing required parameter: text"
    );
    let out = parse(&plugin.handle_tool_call("mem0_update", &json!({"memory_id":"m9","text":"t"})));
    assert!(out["error"].as_str().unwrap().starts_with("Update failed:"));
}

#[test]
fn handle_delete_success_and_failure() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::handle_delete_success_and_failure");
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

    let (base, rx) = mock_server(vec![(200, r#"{"status":"gone"}"#)]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_delete", &json!({"memory_id":"d1"})));
    assert_eq!(out["result"], "Memory deleted.");
    assert_eq!(out["memory_id"], "d1");
    assert_eq!(out["response"]["status"], "gone");
    assert!(recv(&rx).starts_with("DELETE /memories/d1 "));

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_delete", &json!({"memory_id":"d1"})));
    assert!(out["error"].as_str().unwrap().starts_with("Delete failed:"));
    let out = parse(&plugin.handle_tool_call("mem0_delete", &json!({})));
    assert_eq!(
        out["error"],
        "Delete failed: Missing required parameter: memory_id"
    );
}
