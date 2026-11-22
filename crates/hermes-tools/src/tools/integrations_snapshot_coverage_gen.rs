use super::*;
use std::fs;
use tempfile::tempdir;

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.old {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn write_plugin(root: &Path, dir: &str, manifest: &str) -> PathBuf {
    let plugin_dir = root.join(dir);
    fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    fs::write(plugin_dir.join("plugin.yaml"), manifest).expect("write manifest");
    plugin_dir
}

#[test]
fn truncate_chars_handles_short_tiny_and_long_inputs() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::truncate_chars_handles_short_tiny_and_long_inputs"
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

    assert_eq!(truncate_chars("abc", 5), "abc");
    assert_eq!(truncate_chars("abc", 3), "abc");
    assert_eq!(truncate_chars("abcdef", 3), "...");
    assert_eq!(truncate_chars("abcdef", 2), "..");
    assert_eq!(truncate_chars("abcdefgh", 6), "abc...");
    assert_eq!(truncate_chars("ééééé", 4), "é...");
}

#[test]
fn coerce_memory_provider_kind_prefers_explicit_then_probes_init() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::coerce_memory_provider_kind_prefers_explicit_then_probes_init"
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

    let dir = tempdir().expect("dir");
    let p = dir.path();

    assert_eq!(
        coerce_memory_provider_kind(p, Some("  tool  ".to_string())),
        Some("tool".to_string())
    );
    // blank kind and no __init__.py
    assert_eq!(
        coerce_memory_provider_kind(p, Some("   ".to_string())),
        None
    );
    assert_eq!(coerce_memory_provider_kind(p, None), None);

    fs::write(p.join("__init__.py"), "def setup():\n    pass\n").expect("write init");
    assert_eq!(coerce_memory_provider_kind(p, None), None);

    fs::write(
        p.join("__init__.py"),
        "def setup(ctx):\n    ctx.register_memory_provider(Foo())\n",
    )
    .expect("write init");
    assert_eq!(
        coerce_memory_provider_kind(p, None),
        Some("exclusive".to_string())
    );

    fs::write(
        p.join("__init__.py"),
        "class Foo(MemoryProvider):\n    pass\n",
    )
    .expect("write init");
    assert_eq!(
        coerce_memory_provider_kind(p, Some(String::new())),
        Some("exclusive".to_string())
    );
}

#[test]
fn scan_plugin_manifest_root_reads_valid_manifests_only() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::scan_plugin_manifest_root_reads_valid_manifests_only"
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

    let dir = tempdir().expect("dir");
    let root = dir.path();

    assert!(scan_plugin_manifest_root(&root.join("missing"), "user").is_empty());

    fs::write(root.join("loose.txt"), "not a plugin").expect("write loose");
    fs::create_dir_all(root.join("empty")).expect("empty dir");
    write_plugin(root, "broken", "name: [unclosed\n");
    write_plugin(root, "nameless", "version: \"1.0\"\n");
    let alpha = write_plugin(
        root,
        "alpha",
        "name: alpha\nversion: \"1.0.0\"\ndescription: first\nkind: tool\n",
    );
    fs::write(alpha.join(".disabled"), "").expect("disable alpha");
    let beta = write_plugin(root, "beta", "name: beta\n");
    fs::write(
        beta.join("__init__.py"),
        "class X(MemoryProvider):\n    pass\n",
    )
    .expect("init");

    let mut rows = scan_plugin_manifest_root(root, "user");
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(rows.len(), 2);

    assert_eq!(rows[0].name, "alpha");
    assert_eq!(rows[0].version, "1.0.0");
    assert_eq!(rows[0].description, "first");
    assert_eq!(rows[0].kind.as_deref(), Some("tool"));
    assert_eq!(rows[0].source, "user");
    assert!(!rows[0].enabled);
    assert!(rows[0].path.ends_with("alpha"));

    assert_eq!(rows[1].name, "beta");
    assert_eq!(rows[1].version, "");
    assert_eq!(rows[1].description, "");
    assert_eq!(rows[1].kind.as_deref(), Some("exclusive"));
    assert!(rows[1].enabled);
}

#[test]
fn repair_plan_warns_when_memory_unhealthy_and_passes_when_healthy() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::repair_plan_warns_when_memory_unhealthy_and_passes_when_healthy"
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

    let auth = json!({"status": "PASS"});
    let plan = repair_plan(&auth, &json!({"status": "warn"}));
    assert_eq!(plan["overall"], "ACTION_REQUIRED");
    assert_eq!(plan["steps"][0]["status"], "PASS");
    assert_eq!(plan["steps"][1]["area"], "contextlattice");
    assert_eq!(plan["steps"][1]["status"], "WARN");
    assert!(plan["steps"][1]["next"]
        .as_str()
        .unwrap()
        .contains("CONTEXTLATTICE_ORCHESTRATOR_URL"));
    assert_eq!(plan["steps"][2]["area"], "tools");

    let plan = repair_plan(&auth, &json!({"status": "pass"}));
    assert_eq!(plan["overall"], "PASS");
    assert_eq!(plan["steps"][1]["status"], "pass");
}

#[test]
fn gateway_panel_reports_config_error_for_invalid_yaml() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::gateway_panel_reports_config_error_for_invalid_yaml"
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

    let dir = tempdir().expect("dir");
    let config_path = dir.path().join("config.yaml");
    fs::write(&config_path, "platforms: [\n  unterminated: {\n").expect("write config");

    let registry = ToolRegistry::new();
    let panel = gateway_panel_with_config_path(&registry, false, Some(&config_path));
    assert_eq!(panel["config_loaded"], false);
    assert!(panel["config_error"].is_string());
    assert!(!panel["config_error"].as_str().unwrap().is_empty());
    assert_eq!(panel["platform_adapters"], 0);
    assert_eq!(panel["mcp_servers"], 0);
    assert_eq!(panel["plugins"]["included"], false);
    assert_eq!(panel["plugins"]["count"], 0);
}

#[tokio::test]
async fn memory_panel_reports_pass_for_healthy_endpoint() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::memory_panel_reports_pass_for_healthy_endpoint"
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

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let url = format!("{}/", server.uri());
    let panel = memory_panel(&url, 2_000, true).await;
    assert_eq!(panel["probe"], "PASS");
    assert_eq!(panel["status"], "pass");
    assert_eq!(panel["http_status"], 200);
    assert_eq!(panel["contextlattice_url"], server.uri());
    assert_eq!(panel["health_url"], format!("{}/health", server.uri()));
}

#[tokio::test]
async fn memory_panel_reports_warn_for_error_status() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::memory_panel_reports_warn_for_error_status"
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

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let panel = memory_panel(&server.uri(), 2_000, true).await;
    assert_eq!(panel["probe"], "WARN");
    assert_eq!(panel["status"], "warn");
    assert_eq!(panel["http_status"], 503);

    let plan = repair_plan(&json!({"status": "PASS"}), &panel);
    assert_eq!(plan["overall"], "ACTION_REQUIRED");
}

#[tokio::test]
async fn memory_panel_reports_warn_with_error_when_unreachable() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::memory_panel_reports_warn_with_error_when_unreachable"
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

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let url = format!("http://127.0.0.1:{port}");
    let panel = memory_panel(&url, 100, true).await;
    assert_eq!(panel["probe"], "WARN");
    assert_eq!(panel["status"], "warn");
    assert!(panel.get("http_status").is_none());
    let error = panel["error"].as_str().expect("error string");
    assert!(!error.is_empty());
    assert!(error.chars().count() <= 160);
}

#[test]
fn discover_plugins_and_execute_actions_use_hermes_home() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::discover_plugins_and_execute_actions_use_hermes_home"
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

    let _global_env = hermes_config::managed_gateway::test_lock::lock();
    let home = tempdir().expect("home");
    let _home = EnvGuard::set("HERMES_HOME", home.path().to_string_lossy().as_ref());
    let _project = EnvGuard::set("HERMES_ENABLE_PROJECT_PLUGINS", "1");

    let plugins = home.path().join("plugins");
    write_plugin(&plugins, "zeta", "name: beta\n");
    write_plugin(&plugins, "first", "name: Alpha\n");

    let rows = discover_plugin_surface();
    let user: Vec<&str> = rows
        .iter()
        .filter(|row| row.source == "user")
        .map(|row| row.name.as_str())
        .collect();
    assert_eq!(user, ["Alpha", "beta"]);
    assert!(rows.windows(2).all(|w| w[0].source <= w[1].source));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (gateway_raw, status_raw) = runtime.block_on(async {
        let registry = ToolRegistry::new();
        let handler = IntegrationsSnapshotHandler::new(&registry);
        let gateway = handler
            .execute(json!({"action": "gateway", "include_plugins": true}))
            .await;
        let status = handler
            .execute(json!({
                "action": "status",
                "provider": "  openrouter  ",
                "model": " openrouter:some-model ",
                "memory_url": " http://127.0.0.1:9/ ",
                "probe_memory": false,
                "include_plugins": true
            }))
            .await;
        (gateway, status)
    });

    let gateway: Value = serde_json::from_str(&gateway_raw.expect("gateway")).expect("json");
    assert_eq!(gateway["status"], "ok");
    assert_eq!(gateway["action"], "gateway");
    assert_eq!(gateway["gateway"]["plugins"]["included"], true);
    assert!(gateway["gateway"]["plugins"]["count"].as_u64().unwrap() >= 2);
    let names: Vec<&str> = gateway["gateway"]["plugins"]["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(names.contains(&"Alpha"));
    assert!(names.contains(&"beta"));

    let status: Value = serde_json::from_str(&status_raw.expect("status")).expect("json");
    assert_eq!(status["status"], "ok");
    assert_eq!(status["action"], "status");
    assert_eq!(status["auth"]["provider"], "openrouter");
    assert_eq!(status["memory"]["contextlattice_url"], "http://127.0.0.1:9");
    assert_eq!(status["memory"]["status"], "skipped");
    assert_eq!(status["snapshot_file_written"], false);
    assert_eq!(status["mutable"], false);
    assert_eq!(status["secret_values_emitted"], false);
    assert!(status["gateway"]["plugins"]["count"].as_u64().unwrap() >= 2);
    assert_eq!(status["repair"]["steps"].as_array().unwrap().len(), 3);
}
