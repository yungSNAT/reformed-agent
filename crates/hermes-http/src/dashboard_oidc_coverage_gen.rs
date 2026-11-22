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

    let executable = std::env::current_exe().unwrap();
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

#[tokio::test]
async fn malformed_provider_urls_fail_before_network_io() {
    let Some(case) = isolated_env_case(
        "malformed_provider_urls_fail_before_network_io",
        &[
            ("HERMES_DASHBOARD_OIDC_ISSUER", INVALID_URL),
            ("HERMES_DASHBOARD_OIDC_CLIENT_ID", "dashboard"),
            ("HERMES_DASHBOARD_SESSION_SECRET", SECRET),
        ],
        &[
            ("discovery", &[]),
            (
                "overrides",
                &[
                    ("HERMES_DASHBOARD_OIDC_AUTHORIZATION_ENDPOINT", INVALID_URL),
                    ("HERMES_DASHBOARD_OIDC_TOKEN_ENDPOINT", INVALID_URL),
                    ("HERMES_DASHBOARD_OIDC_JWKS_URI", INVALID_URL),
                ],
            ),
        ],
    ) else {
        return;
    };

    let prefix = match case.as_str() {
        "discovery" => "OIDC discovery failed:",
        "overrides" => "invalid authorization_endpoint:",
        _ => panic!("unexpected environment case: {case}"),
    };
    let response = oidc_login(Query(OidcLoginParams { return_to: None })).await;
    assert!(!response.headers().contains_key(SET_COOKIE));
    let body = response_json(response, StatusCode::SERVICE_UNAVAILABLE).await;
    assert!(body["error"].as_str().unwrap().starts_with(prefix));

    let mut config = DashboardOidcConfig::from_env().unwrap();
    if case == "discovery" {
        let login = LoginCookie {
            state: "state".into(),
            nonce: "nonce".into(),
            code_verifier: "verifier".into(),
            return_to: "/".into(),
            exp: jiff::Timestamp::now().as_second() + 600,
        };
        let signed = sign_json(&login, SECRET.as_bytes()).unwrap();
        let response = oidc_callback(
            callback_params(Some("code"), Some("state")),
            cookie_headers(LOGIN_COOKIE, &signed),
        )
        .await;
        let body = response_json(response, StatusCode::SERVICE_UNAVAILABLE).await;
        assert!(body["error"].as_str().unwrap().starts_with(prefix));
    } else {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let metadata = resolve_metadata(&client, &mut config).await.unwrap();
        assert_eq!(metadata.issuer, INVALID_URL);
        assert_eq!(metadata.authorization_endpoint, INVALID_URL);
        assert_eq!(metadata.token_endpoint, INVALID_URL);
        assert_eq!(metadata.jwks_uri, INVALID_URL);
        let error = verify_id_token(&client, "unused", &config, &metadata, Some("nonce"))
            .await
            .unwrap_err();
        assert!(error.starts_with("OIDC JWKS fetch failed:"), "{error}");
    }
}

#[tokio::test]
async fn callback_rejects_provider_errors_and_blank_parameters() {
    let mut provider_error = callback_params(None, None);
    provider_error.0.error = Some("access_denied".into());
    provider_error.0.error_description = Some("Consent declined".into());
    let response = oidc_callback(provider_error, HeaderMap::new()).await;
    assert_eq!(
        response_json(response, StatusCode::UNAUTHORIZED).await,
        json!({"error": "access_denied", "error_description": "Consent declined"})
    );

    for (code, state, error) in [
        (Some(" \t "), Some("state"), "missing code"),
        (Some("code"), Some(" \t "), "missing state"),
    ] {
        let response = oidc_callback(callback_params(code, state), HeaderMap::new()).await;
        assert_eq!(
            response_json(response, StatusCode::BAD_REQUEST).await,
            json!({"error": error})
        );
    }
}

#[tokio::test]
async fn logout_expires_both_auth_cookies() {
    let response = auth_logout().await;
    let cookies: Vec<_> = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    assert_eq!(
        cookies,
        [
            "hermes_dashboard_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
            "hermes_dashboard_oidc_login=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
        ]
    );
    assert_eq!(
        response_json(response, StatusCode::OK).await,
        json!({"status": "ok", "logged_out": true})
    );
}

#[test]
fn cookie_helpers_preserve_attributes_and_reject_invalid_headers() {
    let payload = json!({"sub": "dashboard-user"});
    let cookie = sign_cookie(SESSION_COOKIE, &payload, SECRET.as_bytes(), 42, false).unwrap();
    let (pair, attributes) = cookie.split_once(';').unwrap();
    assert_eq!(attributes, " Path=/; Max-Age=42; HttpOnly; SameSite=Lax");
    let (name, signed) = pair.split_once('=').unwrap();
    assert_eq!(name, SESSION_COOKIE);
    assert_eq!(
        verify_signed_json::<Value>(signed, SECRET.as_bytes()).unwrap(),
        payload
    );

    let mut response = redirect_with_cookie("/dashboard\r\n", "bad\ncookie");
    assert_eq!(response.status(), StatusCode::FOUND);
    assert!(!response.headers().contains_key(LOCATION));
    assert!(!response.headers().contains_key(SET_COOKIE));
    clear_cookie(&mut response, "invalid\nname");
    assert!(!response.headers().contains_key(SET_COOKIE));
}

fn claims_config() -> DashboardOidcConfig {
    DashboardOidcConfig {
        issuer: ISSUER_URL.into(),
        client_id: "dashboard".into(),
        client_secret: None,
        redirect_uri: "https://dashboard.example/auth/oidc/callback".into(),
        scopes: vec!["openid".into()],
        allowed_emails: HashSet::new(),
        allowed_domains: HashSet::new(),
        session_ttl_seconds: 900,
        authorization_endpoint: None,
        token_endpoint: None,
        jwks_uri: None,
        cookie_secure: true,
        session_secret: SECRET.as_bytes().to_vec(),
    }
}

#[test]
fn allowlist_only_requires_an_email_when_restricted() {
    let mut config = claims_config();
    let mut claims = DashboardIdTokenClaims {
        iss: config.issuer.clone(),
        sub: "dashboard-user".into(),
        aud: Audience::One(config.client_id.clone()),
        exp: 4_102_444_800,
        nonce: None,
        email: None,
        name: None,
    };
    assert!(claim_allowed(&claims, &config));

    config.allowed_domains.insert("example.com".into());
    assert!(!claim_allowed(&claims, &config));
    claims.email = Some("missing-domain".into());
    assert!(!claim_allowed(&claims, &config));
    claims.email = Some("USER@EXAMPLE.COM".into());
    assert!(claim_allowed(&claims, &config));
}

#[test]
fn asymmetric_key_selection_and_nonce_validation() {
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use jsonwebtoken::{encode, EncodingKey, Header};

    let config = claims_config();
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
            .unwrap();
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref()).unwrap();
    let point = pair.public_key().as_ref();
    let jwks: JwkSet = serde_json::from_value(json!({
        "keys": [
            {
                "kty": "EC", "kid": "p256", "crv": "P-256",
                "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
                "y": URL_SAFE_NO_PAD.encode(&point[33..65])
            },
            {
                "kty": "oct", "kid": "symmetric",
                "k": URL_SAFE_NO_PAD.encode(b"test-key")
            }
        ]
    }))
    .unwrap();

    assert_eq!(
        select_jwk(&jwks, &None).unwrap_err(),
        "OIDC token has no kid and JWKS has multiple keys"
    );
    assert_eq!(
        select_jwk(&jwks, &Some("absent".into())).unwrap_err(),
        "OIDC JWKS did not contain kid absent"
    );
    assert_eq!(
        select_jwk(&jwks, &Some("symmetric".into())).unwrap_err(),
        "OIDC ID tokens must use asymmetric signing keys"
    );

    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("p256".into());
    let token = encode(
        &header,
        &json!({
            "iss": config.issuer,
            "sub": "dashboard-user",
            "aud": ["another-client", config.client_id],
            "exp": 4_102_444_800_u64,
            "nonce": "original-nonce"
        }),
        &EncodingKey::from_ec_der(pkcs8.as_ref()),
    )
    .unwrap();

    let claims = verify_id_token_with_jwks(&token, &jwks, &config, Some("original-nonce")).unwrap();
    assert_eq!(claims.sub, "dashboard-user");
    assert!(claims.aud.contains(&config.client_id));
    assert!(!claims.aud.contains("unlisted-client"));
    assert_eq!(
        verify_id_token_with_jwks(&token, &jwks, &config, Some("different-nonce")).unwrap_err(),
        "OIDC token nonce mismatch"
    );
    let claims = verify_id_token_with_jwks(&token, &jwks, &config, None).unwrap();
    assert_eq!(claims.nonce.as_deref(), Some("original-nonce"));
}
