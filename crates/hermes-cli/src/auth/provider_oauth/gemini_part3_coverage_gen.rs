// Browserless login: the parent test plays the browser and the token endpoint.
const LOGIN_EXPECTED_ACCESS: &str = "HERMES_GEMINI_COVERAGE_EXPECTED_ACCESS";

fn login_client_secret() -> &'static str {
    concat!("fixture", "-login-", "client")
}

fn browserless_login_options() -> GeminiOAuthLoginOptions {
    GeminiOAuthLoginOptions {
        open_browser: false,
        timeout_seconds: 5.0,
    }
}

// A stand-in HTTPS proxy that records the request line and refuses it,
// so the user-info lookup fails on loopback instead of reaching the network.
async fn refusing_proxy() -> (String, impl std::future::Future<Output = String>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind proxy stand-in");
    let address = format!("http://{}", listener.local_addr().expect("proxy address"));
    let request = async move {
        let (mut stream, _) = listener.accept().await.expect("proxy connection");
        let mut line = String::new();
        let mut request = BufReader::new((&mut stream).take(4096));
        request
            .read_line(&mut line)
            .await
            .expect("proxy request line");
        let mut header = String::new();
        loop {
            let count = request.read_line(&mut header).await.expect("proxy header");
            assert!(count > 0, "incomplete proxy request");
            if header == "\r\n" {
                break;
            }
            header.clear();
        }
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .expect("refuse proxy request");
        line
    };
    (address, async move {
        tokio::time::timeout(Duration::from_secs(30), request)
            .await
            .expect("user-info lookup reached the proxy stand-in")
    })
}

struct BrowserlessLogin {
    authorize: BTreeMap<String, String>,
    output: Output,
}

async fn drive_browserless_login(
    test: &str,
    home: &Path,
    env: &[(&str, String)],
    code: &str,
) -> BrowserlessLogin {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    let mut command = tokio::process::Command::from(child_command(test));
    command
        .env_clear()
        .env(CHILD_TEST, test)
        .env("HERMES_HOME", home)
        .env("HOME", home)
        .env("HERMES_GEMINI_OAUTH_FILE", home.join("oauth.json"))
        .env("HERMES_GEMINI_CLIENT_ID", " fixture-client ")
        .env("HERMES_GEMINI_CLIENT_SECRET", login_client_secret())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in env {
        command.env(name, value);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let mut child = command.spawn().expect("spawn login child");
    let mut lines = BufReader::new(child.stdout.take().expect("child stdout")).lines();
    let mut stderr = child.stderr.take().expect("child stderr");
    let mut errors = Vec::new();
    let mut printed = Vec::new();
    let interaction = async {
        let url = loop {
            let line = lines
                .next_line()
                .await
                .expect("read child stdout")
                .unwrap_or_else(|| {
                    panic!("child exited before printing the authorisation URL: {printed:?}")
                });
            printed.push(line);
            let line = printed.last().expect("captured stdout").trim();
            if line.starts_with(GEMINI_AUTH_ENDPOINT) {
                break reqwest::Url::parse(line).expect("authorisation URL");
            }
        };
        let authorize: BTreeMap<_, _> = url.query_pairs().into_owned().collect();

        let mut redirect = reqwest::Url::parse(&authorize["redirect_uri"]).expect("redirect URI");
        assert_eq!(redirect.scheme(), "http");
        assert_eq!(redirect.host_str(), Some(GEMINI_CALLBACK_HOST));
        assert_eq!(redirect.path(), GEMINI_CALLBACK_PATH);
        redirect
            .query_pairs_mut()
            .append_pair("state", &authorize["state"])
            .append_pair("code", code);
        let reply = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .expect("callback client")
            .get(redirect)
            .send()
            .await
            .expect("deliver callback");
        assert_eq!(reply.status(), reqwest::StatusCode::OK);
        assert!(reply
            .text()
            .await
            .expect("callback page")
            .contains("Signed in to Google"));

        while let Some(line) = lines.next_line().await.expect("read remaining stdout") {
            printed.push(line);
        }
        (authorize, child.wait().await.expect("child exit status"))
    };
    let ((authorize, status), stderr_read) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(interaction, stderr.read_to_end(&mut errors))
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "login child timed out: {printed:?}, {}",
            String::from_utf8_lossy(&errors)
        )
    });
    stderr_read.expect("read child stderr");
    BrowserlessLogin {
        authorize,
        output: Output {
            status,
            stdout: printed.join("\n").into_bytes(),
            stderr: errors,
        },
    }
}

fn posted_form(request: &wiremock::Request) -> BTreeMap<String, String> {
    let mut parsed = reqwest::Url::parse("http://127.0.0.1/").expect("base URL");
    parsed.set_query(Some(
        std::str::from_utf8(&request.body).expect("UTF-8 form"),
    ));
    parsed.query_pairs().into_owned().collect()
}

#[tokio::test]
async fn browserless_login_exchanges_the_callback_code_and_saves_credentials() {
    const TEST: &str = "browserless_login_exchanges_the_callback_code_and_saves_credentials";
    if in_child(TEST) {
        let expected_access = std::env::var(LOGIN_EXPECTED_ACCESS).expect("expected access");
        let creds = login_google_gemini_cli_oauth(browserless_login_options())
            .await
            .expect("login succeeds");
        assert_eq!(creds.provider, "google-gemini-cli");
        assert_eq!(creds.base_url, DEFAULT_GEMINI_CLOUDCODE_BASE_URL);
        assert_eq!(creds.source, "google-oauth");
        assert_eq!(creds.api_key, expected_access);
        assert_eq!(creds.auth_file, gemini_cli_auth_path());
        assert_eq!(creds.email, None);
        assert_eq!(creds.project_id.as_deref(), Some("fixture-project"));
        assert!(creds.refresh_token.is_some());
        return;
    }

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let access = format!("fixture-access-{}", std::process::id());
    let refresh = format!("fixture-refresh-{}", std::process::id());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": format!(" {access} "),
            "refresh_token": format!(" {refresh} "),
            "expires_in": 5,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (proxy, proxied) = refusing_proxy().await;
    let home = tempfile::tempdir().expect("temporary home");
    let started = jiff::Timestamp::now().as_millisecond();
    let env = [
        (
            "HERMES_GEMINI_OAUTH_TOKEN_URL",
            format!(" {}/token ", server.uri()),
        ),
        ("HERMES_GEMINI_PROJECT_ID", " fixture-project ".to_string()),
        ("HTTPS_PROXY", proxy),
        (LOGIN_EXPECTED_ACCESS, access.clone()),
    ];
    let (login, head) = tokio::join!(
        drive_browserless_login(TEST, home.path(), &env, " fixture-code "),
        proxied,
    );
    assert_child_success(&login.output);
    let stdout = String::from_utf8_lossy(&login.output.stdout);
    assert!(stdout.contains("Authorize Hermes with Google (Gemini CLI OAuth)."));
    assert!(!stdout.contains("Opened browser"));

    let authorize = &login.authorize;
    assert_eq!(authorize["client_id"], "fixture-client");
    assert_eq!(authorize["response_type"], "code");
    assert_eq!(authorize["scope"], GEMINI_OAUTH_SCOPE);
    assert_eq!(authorize["code_challenge_method"], "S256");
    assert_eq!(authorize["access_type"], "offline");
    assert_eq!(authorize["prompt"], "consent");

    // The token request carries the PKCE verifier matching the challenge.
    let requests = server.received_requests().await.expect("recorded requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers[reqwest::header::ACCEPT],
        "application/json"
    );
    let form = posted_form(&requests[0]);
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "fixture-code");
    assert_eq!(form["client_id"], "fixture-client");
    assert_eq!(form["client_secret"], login_client_secret());
    assert_eq!(form["redirect_uri"], authorize["redirect_uri"]);
    assert_eq!(
        BASE64_URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
        authorize["code_challenge"]
    );

    // The user-info lookup went to the proxy stand-in and was refused.
    let host = reqwest::Url::parse(GEMINI_USERINFO_ENDPOINT)
        .expect("user-info URL")
        .host_str()
        .expect("user-info host")
        .to_string();
    assert!(
        head.starts_with(&format!("CONNECT {host}:443 ")),
        "unexpected proxy request: {head}"
    );

    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join("oauth.json")).expect("saved credentials"),
    )
    .expect("saved JSON");
    // The refresh token is packed with the project and an empty managed project.
    assert_eq!(saved["refresh"], format!("{refresh}|fixture-project|"));
    assert_eq!(saved["access"], access.as_str());
    assert_eq!(saved["project_id"], "fixture-project");
    assert!(saved["email"].is_null());
    assert!(saved["managed_project_id"].is_null());
    // A short lifetime is raised to the 60 second floor.
    let expires = saved["expires"].as_i64().expect("expiry");
    assert!(expires >= started + 60_000);
    assert!(expires <= jiff::Timestamp::now().as_millisecond() + 60_000);
}

#[tokio::test]
async fn browserless_login_rejects_bad_token_responses_without_saving_credentials() {
    const TEST: &str = "browserless_login_rejects_bad_token_responses_without_saving_credentials";
    const EXPECTED_PREFIX: &str = "HERMES_GEMINI_COVERAGE_EXPECTED_PREFIX";
    const EXPECTED_DETAIL: &str = "HERMES_GEMINI_COVERAGE_EXPECTED_DETAIL";
    if in_child(TEST) {
        let prefix = std::env::var(EXPECTED_PREFIX).expect("expected prefix");
        let message = assert_auth_failure(
            login_google_gemini_cli_oauth(browserless_login_options()).await,
            &prefix,
        );
        if let Ok(detail) = std::env::var(EXPECTED_DETAIL) {
            assert!(message.contains(&detail), "{message:?} lacks {detail:?}");
        }
        assert!(!gemini_cli_auth_path().exists());
        return;
    }

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let access = format!("fixture-access-{}", std::process::id());
    let cases: [(u16, String, &str, Option<&str>); 4] = [
        (
            400,
            serde_json::json!({
                "error": "invalid_grant",
                "error_description": "fixture exchange declined",
            })
            .to_string(),
            "Google OAuth token exchange failed (400 Bad Request):",
            Some("fixture exchange declined"),
        ),
        (
            200,
            "not json".to_string(),
            "invalid Google OAuth token response:",
            None,
        ),
        (
            200,
            serde_json::json!({ "access_token": " ", "refresh_token": "kept" }).to_string(),
            "Google OAuth token response missing access_token",
            None,
        ),
        (
            200,
            serde_json::json!({ "access_token": access }).to_string(),
            "Google OAuth token response missing refresh_token",
            Some("grant offline access"),
        ),
    ];
    for (status, body, prefix, detail) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        let home = tempfile::tempdir().expect("temporary home");
        let mut env = vec![
            (
                "HERMES_GEMINI_OAUTH_TOKEN_URL",
                format!("{}/token", server.uri()),
            ),
            (EXPECTED_PREFIX, prefix.to_string()),
        ];
        if let Some(detail) = detail {
            env.push((EXPECTED_DETAIL, detail.to_string()));
        }
        let login = drive_browserless_login(TEST, home.path(), &env, "fixture-code").await;
        assert_child_success(&login.output);
        server.verify().await;
        assert!(!home.path().join("oauth.json").exists());
    }
}
