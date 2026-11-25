use super::*;
use sha2::Digest;

const ISSUER_URL: &str = "https://issuer.example";
const SECRET: &str = "coverage-test-secret";
const INVALID_URL: &str = "not a URL";
const DASHBOARD_ENV: &[&str] = &[
    "HERMES_DASHBOARD_AUTH_PROVIDER",
    "HERMES_DASHBOARD_AUTH_MODE",
    "HERMES_DASHBOARD_OIDC_ISSUER",
    "HERMES_DASHBOARD_OIDC_CLIENT_ID",
    "HERMES_DASHBOARD_OIDC_CLIENT_SECRET",
    "HERMES_DASHBOARD_OIDC_REDIRECT_URI",
    "HERMES_DASHBOARD_SESSION_SECRET",
    "HERMES_DASHBOARD_OIDC_SESSION_SECRET",
    "HERMES_DASHBOARD_COOKIE_SECURE",
    "HERMES_DASHBOARD_OIDC_SCOPES",
    "HERMES_DASHBOARD_OIDC_ALLOWED_EMAILS",
    "HERMES_DASHBOARD_OIDC_ALLOWED_DOMAINS",
    "HERMES_DASHBOARD_SESSION_TTL_SECONDS",
    "HERMES_DASHBOARD_OIDC_AUTHORIZATION_ENDPOINT",
    "HERMES_DASHBOARD_OIDC_TOKEN_ENDPOINT",
    "HERMES_DASHBOARD_OIDC_JWKS_URI",
];

type EnvVars<'a> = &'a [(&'a str, &'a str)];

// Run only the named test in each child so other tests cannot race with its environment.
fn isolated_env_case(
    test: &str,
    common: EnvVars<'_>,
    cases: &[(&str, EnvVars<'_>)],
) -> Option<String> {
    const CASE: &str = "HERMES_HTTP_DASHBOARD_OIDC_COVERAGE_CASE";
    if let Ok(case) = std::env::var(CASE) {
        return Some(case);
    }

    let executable = std::env::current_exe().unwrap(); // nosemgrep: rust.lang.security.current-exe.current-exe
    let (_, module) = module_path!().split_once("::").unwrap();
    for &(case, vars) in cases {
        let mut command = std::process::Command::new(&executable);
        for key in DASHBOARD_ENV {
            command.env_remove(key);
        }
        let output = command
            .args(["--exact", &format!("{module}::{test}"), "--color=never"])
            .envs(common.iter().copied())
            .envs(vars.iter().copied())
            .env(CASE, case)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{test} ({case}): {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    None
}

fn cookie_headers(name: &str, value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&format!("other=ignored; bare-segment; {name}={value}")).unwrap(),
    );
    headers
}

fn callback_params(code: Option<&str>, state: Option<&str>) -> Query<OidcCallbackParams> {
    Query(OidcCallbackParams {
        code: code.map(str::to_owned),
        state: state.map(str::to_owned),
        error: None,
        error_description: None,
    })
}

async fn response_json(response: Response, status: StatusCode) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "application/json"
    );
    let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn issuer_autodetection_uses_normalized_defaults() {
    if isolated_env_case(
        "issuer_autodetection_uses_normalized_defaults",
        &[],
        &[(
            "defaults",
            &[
                (
                    "HERMES_DASHBOARD_OIDC_ISSUER",
                    " https://issuer.example/// ",
                ),
                ("HERMES_DASHBOARD_OIDC_CLIENT_ID", " dashboard "),
                ("HERMES_DASHBOARD_OIDC_SESSION_SECRET", SECRET),
                ("HERMES_DASHBOARD_COOKIE_SECURE", " unknown "),
                ("HERMES_DASHBOARD_SESSION_TTL_SECONDS", "-1"),
                ("HERMES_DASHBOARD_OIDC_SCOPES", " , , "),
            ],
        )],
    )
    .is_none()
    {
        return;
    }

    let auth = DashboardAuthConfig::from_env();
    assert!(auth.oidc_enabled());
    assert_eq!(auth.config_error, None);
    let config = auth.oidc.unwrap();
    assert_eq!(config.issuer, ISSUER_URL);
    assert_eq!(config.client_id, "dashboard");
    assert_eq!(config.client_secret, None);
    assert_eq!(
        config.redirect_uri,
        "http://127.0.0.1:8787/auth/oidc/callback"
    );
    assert_eq!(config.scopes, ["openid", "email", "profile"]);
    assert!(config.allowed_emails.is_empty());
    assert!(config.allowed_domains.is_empty());
    assert_eq!(config.session_ttl_seconds, DEFAULT_SESSION_TTL_SECONDS);
    assert_eq!(config.session_secret(), SECRET.as_bytes());
    assert!(!config.cookie_secure);
}

#[tokio::test]
async fn absent_or_invalid_oidc_configuration_is_reported() {
    let Some(case) = isolated_env_case(
        "absent_or_invalid_oidc_configuration_is_reported",
        &[],
        &[
            ("bearer", &[]),
            (
                "disabled",
                &[
                    ("HERMES_DASHBOARD_AUTH_MODE", " OFF "),
                    ("HERMES_DASHBOARD_OIDC_ISSUER", ISSUER_URL),
                ],
            ),
            ("missing", &[("HERMES_DASHBOARD_AUTH_PROVIDER", " OpenID ")]),
            (
                "short",
                &[
                    ("HERMES_DASHBOARD_OIDC_ISSUER", ISSUER_URL),
                    ("HERMES_DASHBOARD_OIDC_CLIENT_ID", "dashboard"),
                    ("HERMES_DASHBOARD_SESSION_SECRET", "too-short"),
                ],
            ),
        ],
    ) else {
        return;
    };

    let (mode, config_error) = match case.as_str() {
        "bearer" => ("bearer", None),
        "disabled" => ("none", None),
        "missing" => (
            "oidc",
            Some("HERMES_DASHBOARD_OIDC_ISSUER is required for dashboard OIDC"),
        ),
        "short" => (
            "oidc",
            Some("HERMES_DASHBOARD_SESSION_SECRET must be at least 16 bytes"),
        ),
        _ => panic!("unexpected environment case: {case}"),
    };
    let status = response_json(auth_status(HeaderMap::new()).await, StatusCode::OK).await;
    assert_eq!(
        status,
        json!({
            "status": "ok",
            "mode": mode,
            "configured": config_error.is_none(),
            "config_error": config_error,
            "authenticated": false,
            "identity": null,
            "oidc": null
        })
    );

    let expected = json!({
        "error": config_error.unwrap_or("dashboard OIDC is not configured")
    });
    let login = oidc_login(Query(OidcLoginParams { return_to: None })).await;
    assert_eq!(
        response_json(login, StatusCode::SERVICE_UNAVAILABLE).await,
        expected
    );
    let callback = oidc_callback(
        callback_params(Some("code"), Some("state")),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(
        response_json(callback, StatusCode::SERVICE_UNAVAILABLE).await,
        expected
    );
}

#[tokio::test]
async fn configured_status_and_login_use_signed_cookies() {
    const REDIRECT: &str = "https://dashboard.example/auth/oidc/callback";
    if isolated_env_case(
        "configured_status_and_login_use_signed_cookies",
        &[],
        &[(
            "configured",
            &[
                ("HERMES_DASHBOARD_AUTH_PROVIDER", "sso"),
                ("HERMES_DASHBOARD_OIDC_ISSUER", ISSUER_URL),
                ("HERMES_DASHBOARD_OIDC_CLIENT_ID", "dashboard"),
                ("HERMES_DASHBOARD_OIDC_CLIENT_SECRET", " client-secret "),
                ("HERMES_DASHBOARD_OIDC_REDIRECT_URI", REDIRECT),
                ("HERMES_DASHBOARD_SESSION_SECRET", SECRET),
                ("HERMES_DASHBOARD_SESSION_TTL_SECONDS", "900"),
                ("HERMES_DASHBOARD_OIDC_SCOPES", "openid, email  groups"),
                (
                    "HERMES_DASHBOARD_OIDC_ALLOWED_EMAILS",
                    "USER@EXAMPLE.COM, other@example.com",
                ),
                ("HERMES_DASHBOARD_OIDC_ALLOWED_DOMAINS", "EXAMPLE.NET"),
                (
                    "HERMES_DASHBOARD_OIDC_AUTHORIZATION_ENDPOINT",
                    "https://issuer.example/authorize?tenant=demo",
                ),
                ("HERMES_DASHBOARD_OIDC_TOKEN_ENDPOINT", INVALID_URL),
                ("HERMES_DASHBOARD_OIDC_JWKS_URI", INVALID_URL),
            ],
        )],
    )
    .is_none()
    {
        return;
    }

    let config = DashboardOidcConfig::from_env().unwrap();
    assert_eq!(config.client_secret.as_deref(), Some("client-secret"));
    assert!(config.allowed_emails.contains("user@example.com"));
    assert!(config.allowed_domains.contains("example.net"));

    let mut session = DashboardSessionClaims {
        sub: "dashboard-user".into(),
        email: Some("user@example.com".into()),
        name: None,
        iss: ISSUER_URL.into(),
        exp: jiff::Timestamp::now().as_second() + 900,
    };
    let signed = sign_json(&session, SECRET.as_bytes()).unwrap();
    let response = auth_status(cookie_headers(SESSION_COOKIE, &signed)).await;
    assert_eq!(
        response_json(response, StatusCode::OK).await,
        json!({
            "status": "ok",
            "mode": "oidc",
            "configured": true,
            "config_error": null,
            "authenticated": true,
            "identity": {
                "sub": "dashboard-user",
                "email": "user@example.com",
                "name": null,
                "issuer": ISSUER_URL,
                "expires_at_unix": session.exp
            },
            "oidc": {
                "issuer": ISSUER_URL,
                "client_id_configured": true,
                "redirect_uri": REDIRECT,
                "scopes": ["openid", "email", "groups"],
                "allowed_email_count": 2,
                "allowed_domain_count": 1,
                "authorization_endpoint_configured": true,
                "token_endpoint_configured": true,
                "jwks_uri_configured": true,
                "cookie_secure": true,
                "session_ttl_seconds": 900
            }
        })
    );

    session.exp = 0;
    let expired = sign_json(&session, SECRET.as_bytes()).unwrap();
    let headers = cookie_headers(SESSION_COOKIE, &expired);
    assert_eq!(
        verify_session_from_headers(&headers, &config).unwrap_err(),
        "session expired"
    );
    let status = response_json(auth_status(headers).await, StatusCode::OK).await;
    assert_eq!(status["authenticated"], false);
    assert!(status["identity"].is_null());

    let before = jiff::Timestamp::now().as_second();
    let response = oidc_login(Query(OidcLoginParams {
        return_to: Some(" /dashboard?tab=chat ".into()),
    }))
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let url = Url::parse(response.headers()[LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("issuer.example"));
    assert_eq!(url.path(), "/authorize");

    let set_cookie = response.headers()[SET_COOKIE].to_str().unwrap();
    let (pair, attributes) = set_cookie.split_once(';').unwrap();
    assert_eq!(
        attributes,
        " Path=/; Max-Age=600; HttpOnly; SameSite=Lax; Secure"
    );
    let (name, signed) = pair.split_once('=').unwrap();
    assert_eq!(name, LOGIN_COOKIE);
    let mut login: LoginCookie = verify_signed_json(signed, SECRET.as_bytes()).unwrap();
    assert_eq!(login.return_to, "/dashboard?tab=chat");
    assert!((before + 600..=jiff::Timestamp::now().as_second() + 600).contains(&login.exp));
    assert_ne!(login.state, login.nonce);
    assert!(Uuid::parse_str(&login.state).is_ok());
    assert!(Uuid::parse_str(&login.nonce).is_ok());

    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(login.code_verifier.as_bytes()));
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    for (name, expected) in [
        ("tenant", "demo"),
        ("response_type", "code"),
        ("client_id", "dashboard"),
        ("redirect_uri", REDIRECT),
        ("scope", "openid email groups"),
        ("state", login.state.as_str()),
        ("nonce", login.nonce.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ] {
        assert_eq!(
            query.get(name).map(String::as_str),
            Some(expected),
            "{name}"
        );
    }

    let valid_headers = cookie_headers(LOGIN_COOKIE, signed);
    let state_with_spaces = format!(" {} ", login.state);
    let response = oidc_callback(
        callback_params(Some(" code "), Some(&state_with_spaces)),
        valid_headers.clone(),
    )
    .await;
    let body = response_json(response, StatusCode::BAD_GATEWAY).await;
    assert!(body["error"]
        .as_str()
        .unwrap()
        .starts_with("OIDC token exchange failed:"));

    login.exp = 0;
    let expired_login = sign_json(&login, SECRET.as_bytes()).unwrap();
    for (headers, state, error) in [
        (
            cookie_headers("other_cookie", "not-a-login-cookie"),
            login.state.as_str(),
            "missing OIDC login cookie",
        ),
        (
            cookie_headers(LOGIN_COOKIE, "malformed"),
            login.state.as_str(),
            "signed cookie is malformed",
        ),
        (
            cookie_headers(LOGIN_COOKIE, &expired_login),
            login.state.as_str(),
            "OIDC login cookie expired",
        ),
        (valid_headers, "different-state", "OIDC state mismatch"),
    ] {
        let response = oidc_callback(callback_params(Some("code"), Some(state)), headers).await;
        assert_eq!(
            response_json(response, StatusCode::UNAUTHORIZED).await,
            json!({"error": error})
        );
    }
}

include!("dashboard_oidc_part2_coverage_gen.rs");
include!("dashboard_oidc_loopback.rs");
