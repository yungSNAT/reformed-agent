// Google Gemini OAuth through `auth add` and `auth login`. The child prints
// each authorisation URL; the parent plays the browser and the token endpoint.

const GEMINI_CHILD: &str = "HERMES_PORTAL_GEMINI_COVERAGE_CHILD";
const GEMINI_EXPECTED_ACCESS: &str = "HERMES_PORTAL_GEMINI_EXPECTED_ACCESS";
const GOOGLE_AUTHORIZE_PREFIX: &str = "https://accounts.google.com/o/oauth2/v2/auth?";

// A stand-in HTTPS proxy that records each request head and refuses it, so the
// user-info lookups fail on loopback instead of reaching the network.
fn refusing_proxy(connections: usize) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy stand-in");
    let address = format!("http://{}", listener.local_addr().expect("proxy address"));
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..connections {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let read = stream.read(&mut buf).unwrap_or(0);
            let _ = sender.send(String::from_utf8_lossy(&buf[..read]).into_owned());
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        }
    });
    (address, receiver)
}

async fn deliver_google_callback(authorize_line: &str, code: &str) -> String {
    let authorize = reqwest::Url::parse(authorize_line).expect("authorisation URL");
    let pairs: std::collections::BTreeMap<_, _> = authorize.query_pairs().into_owned().collect();
    let mut redirect = reqwest::Url::parse(&pairs["redirect_uri"]).expect("redirect URI");
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    redirect
        .query_pairs_mut()
        .append_pair("state", &pairs["state"])
        .append_pair("code", code);
    let reply = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("callback client")
        .get(redirect)
        .send()
        .await
        .expect("deliver callback");
    assert_eq!(reply.status(), reqwest::StatusCode::OK);
    pairs["redirect_uri"].clone()
}

#[tokio::test]
async fn gemini_oauth_add_and_login_save_pool_vault_and_auth_state() {
    const TEST: &str = "gemini_oauth_add_and_login_save_pool_vault_and_auth_state";
    if std::env::var(GEMINI_CHILD).is_ok_and(|value| value == TEST) {
        let home = isolated_home();
        let access = std::env::var(GEMINI_EXPECTED_ACCESS).expect("expected access");
        run_auth(
            cli_in(&home).expect("cli"),
            opt("add"),
            opt("google-gemini-cli"),
            None,
            opt("oauth"),
            None,
            None,
            false,
        )
        .await
        .expect("auth add");
        let pool = load_auth_pool_store(&auth_pool_path_for_cli(&cli_in(&home).unwrap()))
            .expect("load pool");
        let entries = &pool.providers["google-gemini-cli"];
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "google-gemini-cli-1");
        assert_eq!(entries[0].auth_type, "oauth");
        assert_eq!(entries[0].source, "google-oauth");
        assert_eq!(entries[0].access_token, access);
        assert_eq!(entries[0].id.len(), 6);

        auth(&home, "login", "google-gemini-cli")
            .await
            .expect("auth login");
        let credential = vault_credential(&home, "google-gemini-cli")
            .await
            .expect("vault credential");
        assert_eq!(credential.access_token, access);
        assert!(credential.refresh_token.is_some());
        assert_eq!(credential.token_type, "bearer");
        assert!(credential.expires_at.is_some());
        let state = read_provider_auth_state("google-gemini-cli")
            .expect("read auth store")
            .expect("gemini auth state");
        assert_eq!(state["access_token"], access.as_str());
        assert_eq!(state["source"], "google-oauth");
        assert_eq!(state["project_id"], "fixture-project");
        assert!(state["email"].is_null());
        return;
    }

    use tokio::io::{AsyncBufReadExt, BufReader};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let access = format!("fixture-gemini-access-{}", std::process::id());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": access,
            "refresh_token": format!("fixture-gemini-refresh-{}", std::process::id()),
            "expires_in": 3600,
        })))
        .expect(2)
        .mount(&server)
        .await;
    let (proxy, proxied) = refusing_proxy(2);
    let root = tempfile::tempdir().expect("isolated profile");
    let managed = root.path().join("managed");
    let no_tools = root.path().join("empty-path");
    std::fs::create_dir(&managed).expect("empty managed configuration");
    std::fs::create_dir(&no_tools).expect("empty PATH folder");

    let mut command = tokio::process::Command::new(
        std::env::current_exe().expect("test executable"), // nosemgrep: rust.lang.security.current-exe.current-exe
    );
    command
        .args([
            "--exact",
            &format!("{PORTAL_MODULE}::{TEST}"),
            "--nocapture",
            "--color",
            "never",
        ])
        .current_dir(root.path())
        .env_clear()
        .env(GEMINI_CHILD, TEST)
        .env("HOME", root.path())
        .env("HERMES_HOME", root.path())
        .env("HERMES_MANAGED_DIR", &managed)
        // An empty PATH makes the browser launcher fail instead of opening one.
        .env("PATH", &no_tools)
        .env("HERMES_GEMINI_CLIENT_ID", "fixture-client")
        .env("HERMES_GEMINI_OAUTH_TOKEN_URL", format!("{}/token", server.uri()))
        .env("HERMES_GEMINI_PROJECT_ID", "fixture-project")
        .env("HTTPS_PROXY", &proxy)
        .env("https_proxy", &proxy)
        .env(GEMINI_EXPECTED_ACCESS, &access)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let mut child = command.spawn().expect("spawn gemini child");
    let mut lines = BufReader::new(child.stdout.take().expect("child stdout")).lines();
    let mut printed = Vec::new();
    let mut redirects = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while let Some(line) = lines.next_line().await.expect("read child stdout") {
            let trimmed = line.trim().to_string();
            printed.push(line);
            if trimmed.starts_with(GOOGLE_AUTHORIZE_PREFIX) {
                redirects.push(deliver_google_callback(&trimmed, "fixture-code").await);
            }
        }
    })
    .await
    .expect("child finished printing");
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("child exited")
        .expect("child output");
    let stdout = printed.join("\n");
    assert!(
        output.status.success() && stdout.contains(&format!("test {PORTAL_MODULE}::{TEST} ... ok")),
        "child status: {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(redirects.len(), 2, "one authorisation per command");
    assert!(stdout.contains("Added Google Gemini OAuth credential (id="));
    assert!(stdout.contains(
        "Google Gemini OAuth login complete; credential saved as provider 'google-gemini-cli'."
    ));
    assert_eq!(
        stdout
            .matches("Could not open browser automatically: ")
            .count(),
        2,
        "{stdout}"
    );
    let auth_file = root.path().join("auth").join("google_oauth.json");
    assert!(stdout.contains(&format!("Google auth file: {}", auth_file.display())));
    assert!(auth_file.exists());

    let requests = server.received_requests().await.expect("recorded requests");
    assert_eq!(requests.len(), 2);
    for (request, redirect) in requests.iter().zip(&redirects) {
        let body = std::str::from_utf8(&request.body).expect("UTF-8 form");
        let form: std::collections::BTreeMap<_, _> = reqwest::Url::parse(&format!(
            "http://127.0.0.1/?{body}"
        ))
        .expect("form as query")
        .query_pairs()
        .into_owned()
        .collect();
        assert_eq!(form["code"], "fixture-code");
        assert_eq!(form["client_id"], "fixture-client");
        assert_eq!(&form["redirect_uri"], redirect);
    }
    for _ in 0..2 {
        let head = proxied
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("user-info lookup reached the proxy stand-in");
        assert!(head.starts_with("CONNECT www.googleapis.com:443 "), "{head}");
    }
}
