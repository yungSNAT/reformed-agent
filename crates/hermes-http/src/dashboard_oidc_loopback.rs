const LOOPBACK_OIDC_ISSUER: &str = "HERMES_HTTP_OIDC_FIXTURE_ISSUER";
const LOOPBACK_CLIENT: &str = "fixture-dashboard";

struct LoopbackIdentityProvider {
    issuer: String,
    authorisation: tokio::sync::Mutex<Option<(String, String)>>,
    signing_key: jsonwebtoken::EncodingKey,
    jwks: Value,
}

impl LoopbackIdentityProvider {
    fn new(issuer: String) -> Self {
        use aws_lc_rs::rand::SystemRandom;
        use aws_lc_rs::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

        let secret =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .unwrap();
        let pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, secret.as_ref()).unwrap();
        let point = pair.public_key().as_ref();
        let jwks = json!({"keys": [{
            "kty": "EC", "kid": "fixture", "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
            "y": URL_SAFE_NO_PAD.encode(&point[33..65]),
        }]});
        Self {
            issuer,
            authorisation: tokio::sync::Mutex::new(None),
            signing_key: jsonwebtoken::EncodingKey::from_ec_der(secret.as_ref()),
            jwks,
        }
    }
}

type LoopbackIssuer = std::sync::Arc<LoopbackIdentityProvider>;
type OidcForm = std::collections::HashMap<String, String>;

async fn loopback_discovery(
    axum::extract::State(provider): axum::extract::State<LoopbackIssuer>,
) -> axum::Json<Value> {
    let issuer = &provider.issuer;
    axum::Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
    }))
}

async fn loopback_authorise(
    axum::extract::State(provider): axum::extract::State<LoopbackIssuer>,
    Query(query): Query<OidcForm>,
) -> StatusCode {
    assert_eq!(query["client_id"], LOOPBACK_CLIENT);
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(
        query["redirect_uri"],
        format!("{}/callback", provider.issuer)
    );
    *provider.authorisation.lock().await =
        Some((query["nonce"].clone(), query["code_challenge"].clone()));
    StatusCode::NO_CONTENT
}

async fn loopback_exchange(
    axum::extract::State(provider): axum::extract::State<LoopbackIssuer>,
    axum::Form(form): axum::Form<OidcForm>,
) -> axum::Json<Value> {
    let (nonce, challenge) = provider.authorisation.lock().await.take().unwrap();
    assert_eq!(form["client_id"], LOOPBACK_CLIENT);
    assert_eq!(form["code"], "fixture-authorization");
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(
        form["redirect_uri"],
        format!("{}/callback", provider.issuer)
    );
    assert_eq!(
        challenge,
        URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(&form["code_verifier"]))
    );
    let mut header = jsonwebtoken::Header::new(Algorithm::ES256);
    header.kid = Some("fixture".into());
    let token = jsonwebtoken::encode(
        &header,
        &json!({
            "iss": provider.issuer, "sub": "fixture-user", "aud": LOOPBACK_CLIENT,
            "email": "reader@example.test", "name": "Fixture Reader", "nonce": nonce,
            "exp": jiff::Timestamp::now().as_second() + 300,
        }),
        &provider.signing_key,
    )
    .unwrap();
    axum::Json(json!({"id_token": token}))
}

#[tokio::test]
async fn loopback_oidc_login_discovers_provider_and_creates_a_dashboard_session() {
    if let Ok(issuer) = std::env::var(LOOPBACK_OIDC_ISSUER) {
        confirm_dashboard_session(&issuer).await;
        return;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let provider = LoopbackIssuer::new(LoopbackIdentityProvider::new(issuer.clone()));
    let router = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(loopback_discovery),
        )
        .route("/authorize", axum::routing::get(loopback_authorise))
        .route("/token", axum::routing::post(loopback_exchange))
        .route(
            "/jwks",
            axum::routing::get(
                |axum::extract::State(provider): axum::extract::State<LoopbackIssuer>| async move {
                    axum::Json(provider.jwks.clone())
                },
            ),
        )
        .with_state(provider);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let redirect = format!("{issuer}/callback");
        let common = [
            (LOOPBACK_OIDC_ISSUER, issuer.as_str()),
            ("HERMES_DASHBOARD_AUTH_PROVIDER", "oidc"),
            ("HERMES_DASHBOARD_OIDC_ISSUER", issuer.as_str()),
            ("HERMES_DASHBOARD_OIDC_CLIENT_ID", LOOPBACK_CLIENT),
            ("HERMES_DASHBOARD_OIDC_REDIRECT_URI", redirect.as_str()),
            ("HERMES_DASHBOARD_SESSION_SECRET", SECRET),
            ("HERMES_DASHBOARD_COOKIE_SECURE", "true"),
            (
                "HERMES_DASHBOARD_OIDC_ALLOWED_EMAILS",
                "reader@example.test",
            ),
            ("NO_PROXY", "127.0.0.1"),
            ("no_proxy", "127.0.0.1"),
        ];
        assert!(isolated_env_case(
            "loopback_oidc_login_discovers_provider_and_creates_a_dashboard_session",
            &common,
            &[("loopback", &[])],
        )
        .is_none());
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

async fn confirm_dashboard_session(issuer: &str) {
    let expected = Url::parse(issuer).unwrap();
    assert_eq!(expected.scheme(), "http");
    assert_eq!(expected.host_str(), Some("127.0.0.1"));
    assert!(expected.port().is_some());
    assert_eq!(
        std::env::var("HERMES_DASHBOARD_OIDC_ISSUER").unwrap(),
        issuer
    );

    let login = oidc_login(Query(OidcLoginParams {
        return_to: Some("/settings".into()),
    }))
    .await;
    assert_eq!(login.status(), StatusCode::FOUND);
    let authorise = Url::parse(login.headers()[LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(authorise.origin(), expected.origin());
    assert_eq!(authorise.path(), "/authorize");
    let state = authorise
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    assert_eq!(
        client.get(authorise).send().await.unwrap().status(),
        StatusCode::NO_CONTENT
    );

    let login_cookie = login.headers()[SET_COOKIE].to_str().unwrap();
    let signed = login_cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix(&format!("{LOGIN_COOKIE}="))
        .unwrap();
    let response = oidc_callback(
        callback_params(Some("fixture-authorization"), Some(&state)),
        cookie_headers(LOGIN_COOKIE, signed),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers()[LOCATION], "/settings");
    let cookies: Vec<_> = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .map(|cookie| cookie.to_str().unwrap())
        .collect();
    assert_eq!(cookies.len(), 2);
    assert!(cookies[1].starts_with(&format!("{LOGIN_COOKIE}=;")));
    assert!(cookies[1].contains("Max-Age=0"));
    assert!(cookies[0].contains("HttpOnly; SameSite=Lax; Secure"));
    let signed = cookies[0]
        .split(';')
        .next()
        .unwrap()
        .strip_prefix(&format!("{SESSION_COOKIE}="))
        .unwrap();
    let headers = cookie_headers(SESSION_COOKIE, signed);
    let status = response_json(auth_status(headers).await, StatusCode::OK).await;
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["identity"]["sub"], "fixture-user");
    assert_eq!(status["identity"]["email"], "reader@example.test");
    assert_eq!(status["identity"]["issuer"], issuer);
}
