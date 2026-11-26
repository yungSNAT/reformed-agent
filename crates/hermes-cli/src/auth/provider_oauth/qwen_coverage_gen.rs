use super::*;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_LOCK: Mutex<()> = Mutex::new(());

const MANAGED_VARS: [&str; 4] = [
    "HERMES_QWEN_CLI_AUTH_FILE",
    "HERMES_QWEN_OAUTH_TOKEN_URL",
    "HERMES_QWEN_OAUTH_CLIENT_ID",
    "HERMES_QWEN_BASE_URL",
];

fn set_env(key: &str, value: &str) {
    std::env::set_var(key, value)
}

fn remove_env(key: &str) {
    std::env::remove_var(key)
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = MANAGED_VARS
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect::<Vec<_>>();
        for k in MANAGED_VARS {
            remove_env(k);
        }
        EnvGuard { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => set_env(k, &val.to_string_lossy()),
                None => remove_env(k),
            }
        }
    }
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime")
        .block_on(fut)
}

fn expect_err<T>(r: Result<T, AgentError>) -> AgentError {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

fn auth_failed_msg(err: AgentError) -> String {
    match err {
        AgentError::AuthFailed(m) => m,
        other => panic!("expected AuthFailed, got {other:?}"),
    }
}

fn config_msg(err: AgentError) -> String {
    match err {
        AgentError::Config(m) => m,
        other => panic!("expected Config, got {other:?}"),
    }
}

fn sample_tokens(refresh: Option<&str>) -> QwenCliTokens {
    QwenCliTokens {
        access_token: "old-access".to_string(),
        refresh_token: refresh.map(str::to_string), // nosemgrep: codacy.yaml.security.hard-coded-tokens
        token_type: "Bearer".to_string(),
        resource_url: "portal.example.test".to_string(),
        expiry_date: Some(1_000),
    }
}

#[test]
fn auth_path_uses_env_override_or_home_default() {
    if run_isolated("auth_path_uses_env_override_or_home_default") {
        return;
    }

    let _g = EnvGuard::new();
    set_env("HERMES_QWEN_CLI_AUTH_FILE", "  /tmp/qwen-cov/creds.json  ");
    assert_eq!(
        qwen_cli_auth_path(),
        PathBuf::from("/tmp/qwen-cov/creds.json")
    );

    let suffix = Path::new(".qwen").join("oauth_creds.json");
    set_env("HERMES_QWEN_CLI_AUTH_FILE", "   ");
    assert!(qwen_cli_auth_path().ends_with(&suffix));

    remove_env("HERMES_QWEN_CLI_AUTH_FILE");
    assert!(qwen_cli_auth_path().ends_with(&suffix));
}

#[test]
fn read_tokens_reports_missing_file() {
    if run_isolated("read_tokens_reports_missing_file") {
        return;
    }

    let _g = EnvGuard::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("missing.json");
    set_env("HERMES_QWEN_CLI_AUTH_FILE", file.to_str().unwrap());
    let msg = auth_failed_msg(expect_err(read_qwen_cli_tokens()));
    assert!(msg.contains("not found"), "{msg}");
}

#[test]
fn read_tokens_rejects_non_object_and_missing_access_token() {
    if run_isolated("read_tokens_rejects_non_object_and_missing_access_token") {
        return;
    }

    let _g = EnvGuard::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("creds.json");
    set_env("HERMES_QWEN_CLI_AUTH_FILE", file.to_str().unwrap());

    std::fs::write(&file, "[1, 2, 3]").unwrap();
    let msg = config_msg(expect_err(read_qwen_cli_tokens()));
    assert!(msg.contains("invalid Qwen CLI credentials"), "{msg}");

    std::fs::write(&file, r#"{"refresh_token":"r","access_token":"   "}"#).unwrap();
    let msg = auth_failed_msg(expect_err(read_qwen_cli_tokens()));
    assert!(msg.contains("access_token missing"), "{msg}");
}

#[test]
fn save_tokens_writes_file_that_reads_back() {
    if run_isolated("save_tokens_writes_file_that_reads_back") {
        return;
    }

    let _g = EnvGuard::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("saved.json");
    set_env("HERMES_QWEN_CLI_AUTH_FILE", file.to_str().unwrap());

    let tokens = QwenCliTokens {
        access_token: "acc-123".to_string(),
        refresh_token: Some("ref-456".to_string()), // nosemgrep: codacy.yaml.security.hard-coded-tokens
        token_type: "Custom".to_string(),
        resource_url: "res.example.test".to_string(),
        expiry_date: Some(987_654_321),
    };
    let written = save_qwen_cli_tokens(&tokens).expect("save");
    assert_eq!(written, file);
    let raw = std::fs::read_to_string(&file).unwrap();
    assert!(raw.ends_with('\n'));
    assert!(raw.contains("acc-123"));

    let back = read_qwen_cli_tokens().expect("read back");
    assert_eq!(back.access_token, "acc-123");
    assert_eq!(back.refresh_token.as_deref(), Some("ref-456"));
    assert_eq!(back.token_type, "Custom");
    assert_eq!(back.resource_url, "res.example.test");
    assert_eq!(back.expiry_date, Some(987_654_321));
}

#[test]
fn refresh_requires_refresh_token() {
    if run_isolated("refresh_requires_refresh_token") {
        return;
    }

    let _g = EnvGuard::new();
    let tokens = sample_tokens(Some("   "));
    let msg = auth_failed_msg(expect_err(block_on(refresh_qwen_cli_tokens(&tokens, 20.0))));
    assert!(msg.contains("refresh token missing"), "{msg}");
}

#[test]
fn refresh_success_updates_and_persists_tokens() {
    if run_isolated("refresh_success_updates_and_persists_tokens") {
        return;
    }

    let _g = EnvGuard::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("refreshed.json");
    set_env("HERMES_QWEN_CLI_AUTH_FILE", file.to_str().unwrap());
    set_env("HERMES_QWEN_OAUTH_CLIENT_ID", "  test-client  ");

    let refreshed = block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .and(body_string_contains("client_id=test-client"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "expires_in": "3600"
            })))
            .expect(1)
            .mount(&server)
            .await;
        set_env(
            "HERMES_QWEN_OAUTH_TOKEN_URL",
            &format!("{}/token", server.uri()),
        );
        let tokens = sample_tokens(Some("old-refresh"));
        refresh_qwen_cli_tokens(&tokens, f64::NAN).await
    })
    .expect("refresh ok");

    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("new-refresh"));
    assert_eq!(refreshed.token_type, "Bearer");
    assert_eq!(refreshed.resource_url, "portal.example.test");
    let expiry = refreshed.expiry_date.expect("expiry");
    assert!(expiry > jiff::Timestamp::now().as_millisecond() + 3_000_000);

    let on_disk = read_qwen_cli_tokens().expect("persisted");
    assert_eq!(on_disk.access_token, "new-access");
    assert_eq!(on_disk.refresh_token.as_deref(), Some("new-refresh"));
}

#[test]
fn refresh_reports_server_and_payload_errors() {
    if run_isolated("refresh_reports_server_and_payload_errors") {
        return;
    }

    let _g = EnvGuard::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("unused.json");
    set_env("HERMES_QWEN_CLI_AUTH_FILE", file.to_str().unwrap());

    let msgs = block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/e401"))
            .respond_with(ResponseTemplate::new(401).set_body_string("denied by server"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/bad"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/arr"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[1]"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/noat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"token_type": "x"})),
            )
            .mount(&server)
            .await;

        let tokens = sample_tokens(Some("r"));
        let mut out = Vec::new();
        for p in ["/e401", "/bad", "/arr", "/noat"] {
            set_env(
                "HERMES_QWEN_OAUTH_TOKEN_URL",
                &format!("{}{}", server.uri(), p),
            );
            out.push(auth_failed_msg(expect_err(
                refresh_qwen_cli_tokens(&tokens, 10.0).await,
            )));
        }
        out
    });

    assert!(msgs[0].contains("401"), "{}", msgs[0]);
    assert!(msgs[0].contains("Qwen OAuth refresh failed"), "{}", msgs[0]);
    assert!(msgs[1].contains("JSON parse failed"), "{}", msgs[1]);
    assert!(msgs[2].contains("not a JSON object"), "{}", msgs[2]);
    assert!(msgs[3].contains("missing access_token"), "{}", msgs[3]);
    assert!(!file.exists());
}

#[test]
fn resolve_refreshes_expiring_tokens() {
    if run_isolated("resolve_refreshes_expiring_tokens") {
        return;
    }

    let _g = EnvGuard::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("creds.json");
    set_env("HERMES_QWEN_CLI_AUTH_FILE", file.to_str().unwrap());
    set_env("HERMES_QWEN_BASE_URL", "  https://qwen.example.test/v1/  ");
    std::fs::write(
        &file,
        r#"{"access_token":"stale","refresh_token":"keep-me","expiry_date":0}"#,
    )
    .unwrap();

    let creds = block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh",
                "token_type": "Bearer2",
                "resource_url": "new.example.test",
                "expires_in": 120
            })))
            .expect(1)
            .mount(&server)
            .await;
        set_env(
            "HERMES_QWEN_OAUTH_TOKEN_URL",
            &format!("{}/token", server.uri()),
        );
        resolve_qwen_runtime_credentials(false, true, 60).await
    })
    .expect("resolve ok");

    assert_eq!(creds.api_key, "fresh");
    assert_eq!(creds.provider, "qwen-oauth");
    assert_eq!(creds.source, "qwen-cli");
    assert_eq!(creds.base_url, "https://qwen.example.test/v1");
    assert_eq!(creds.refresh_token.as_deref(), Some("keep-me"));
    assert_eq!(creds.token_type, "Bearer2");
    assert_eq!(creds.auth_file, file);
    assert!(creds.expires_at_ms.is_some());
}

#[test]
fn auth_status_reports_logged_in_and_errors() {
    if run_isolated("auth_status_reports_logged_in_and_errors") {
        return;
    }

    let _g = EnvGuard::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("creds.json");
    set_env("HERMES_QWEN_CLI_AUTH_FILE", file.to_str().unwrap());

    let missing = block_on(get_qwen_auth_status());
    assert!(!missing.logged_in);
    assert_eq!(missing.auth_file, file);
    assert!(missing.source.is_none());
    assert!(missing.api_key.is_none());
    assert!(missing.expires_at_ms.is_none());
    assert!(missing.error.as_deref().unwrap_or("").contains("not found"));

    std::fs::write(&file, r#"{"access_token":"abc","expiry_date":"123"}"#).unwrap();
    let ok = block_on(get_qwen_auth_status());
    assert!(ok.logged_in);
    assert_eq!(ok.auth_file, file);
    assert_eq!(ok.source.as_deref(), Some("qwen-cli"));
    assert_eq!(ok.api_key.as_deref(), Some("abc"));
    assert_eq!(ok.expires_at_ms, Some(123));
    assert!(ok.error.is_none());
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path.split_once("::").map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary")) // nosemgrep: rust.lang.security.current-exe.current-exe
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
