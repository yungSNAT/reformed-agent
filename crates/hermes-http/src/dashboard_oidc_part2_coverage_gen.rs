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
