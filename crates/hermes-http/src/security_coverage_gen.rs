use super::*;
use axum::http::{HeaderValue, Method};
use axum::{middleware, routing::any, Router};
use std::process::Command;
use tower::ServiceExt;

const ENV_CASE: &str = "HERMES_SECURITY_COVERAGE_ENV_CASE";
const ENV_CHECKED: &str = "security coverage child assertions completed";

fn run_env_case(test: &str, case: &str, settings: &[(&str, &str)]) {
    let (_, module) = module_path!().split_once("::").unwrap();
    let test_filter = format!("{module}::{test}");
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe

    // Isolate the fixture without mutating the environment of parallel tests.
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("HERMES_")
        {
            command.env_remove(name);
        }
    }

    let output = command
        .args(["--exact", test_filter.as_str(), "--nocapture"])
        .envs(settings.iter().copied())
        .env(ENV_CASE, case)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(ENV_CHECKED),
        "{test_filter} ({case}) did not complete its assertions:\n{stdout}\n{stderr}"
    );
}

#[test]
fn policy_guard_and_ip_allowlist_from_env() {
    let (key, require_admin, export_require_admin, actors, ips) =
        match std::env::var(ENV_CASE).as_deref() {
            Ok("primary") => (
                "primary-token",
                true,
                false,
                &["ops", "trainer"][..],
                Some(vec![
                    "127.0.0.1".parse::<IpAddr>().unwrap(),
                    "::1".parse::<IpAddr>().unwrap(),
                ]),
            ),
            Ok("legacy") => ("legacy-token", false, true, &[][..], None),
            _ => {
                let test = "policy_guard_and_ip_allowlist_from_env";
                run_env_case(
                    test,
                    "primary",
                    &[
                        ("HERMES_POLICY_ADMIN_TOKEN", " primary-token "),
                        ("HERMES_HTTP_POLICY_ADMIN_KEY", " legacy-token "),
                        ("HERMES_HTTP_POLICY_REQUIRE_ADMIN", "1"),
                        ("HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN", "true"),
                        ("HERMES_HTTP_POLICY_ALLOWED_ACTORS", " ops, , trainer, "),
                        ("HERMES_HTTP_ALLOWED_IPS", " 127.0.0.1, invalid, ::1 "),
                    ],
                );
                run_env_case(
                    test,
                    "legacy",
                    &[
                        ("HERMES_POLICY_ADMIN_TOKEN", "  "),
                        ("HERMES_HTTP_POLICY_ADMIN_KEY", " legacy-token "),
                        ("HERMES_HTTP_POLICY_REQUIRE_ADMIN", "0"),
                        ("HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN", "1"),
                        ("HERMES_HTTP_ALLOWED_IPS", " invalid, , "),
                    ],
                );
                return;
            }
        };

    let policy = PolicyGuardConfig::from_env();
    assert_eq!(policy.admin_key.as_deref(), Some(key));
    assert_eq!(policy.require_admin, require_admin);
    assert_eq!(policy.export_require_admin, export_require_admin);
    let actual_actors: Vec<_> = policy.allowed_actors.iter().map(String::as_str).collect();
    assert_eq!(actual_actors.as_slice(), actors);
    assert_eq!(parse_allowed_ips_from_env(), ips);
    println!("{ENV_CHECKED}");
}

#[test]
fn policy_guard_rejects_blank_actors_and_enforces_admin_requirements() {
    let empty_headers = HeaderMap::new();
    let mut policy = PolicyGuardConfig {
        require_admin: true,
        ..PolicyGuardConfig::default()
    };

    assert_eq!(policy.check_actor(" \t "), Err("actor must be non-empty"));
    assert_eq!(
        policy.check_mutation_admin(&empty_headers),
        Err(
            "HERMES_HTTP_POLICY_REQUIRE_ADMIN is set but HERMES_POLICY_ADMIN_TOKEN (or HERMES_HTTP_POLICY_ADMIN_KEY) is empty"
        )
    );
    assert_eq!(policy.check_export_admin(&empty_headers), Ok(()));

    policy.export_require_admin = true;
    assert_eq!(
        policy.check_export_admin(&empty_headers),
        Err(
            "HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN is set but HERMES_POLICY_ADMIN_TOKEN (or HERMES_HTTP_POLICY_ADMIN_KEY) is empty"
        )
    );

    policy.admin_key = Some(Arc::from("policy-token"));
    let invalid = Err("missing or invalid X-Hermes-Policy-Admin");
    assert_eq!(policy.check_export_admin(&empty_headers), invalid);

    let mut headers = HeaderMap::new();
    assert!(headers
        .insert("x-hermes-policy-admin", HeaderValue::from_static("wrong"))
        .is_none());
    assert_eq!(policy.check_export_admin(&headers), invalid);

    assert!(headers
        .insert(
            "x-hermes-policy-admin",
            HeaderValue::from_static(" policy-token "),
        )
        .is_some());
    assert_eq!(policy.check_export_admin(&headers), Ok(()));
    assert_eq!(policy.check_mutation_admin(&headers), Ok(()));
}

#[tokio::test]
async fn rate_limiter_expires_old_entries_without_discarding_recent_ones() {
    let limiter = RateLimiter::new(2);
    let now = Instant::now();
    let expired = now - Duration::from_secs(61);
    let recent = now - Duration::from_secs(45);
    let key = "first-client".to_owned();

    assert!(limiter
        .inner
        .lock()
        .await
        .insert(key.clone(), vec![expired, recent])
        .is_none());

    assert!(limiter.allow(key.clone()).await);
    assert!(!limiter.allow(key.clone()).await);
    assert!(limiter.allow("second-client".to_owned()).await);

    let counts = limiter.inner.lock().await;
    let first = &counts[&key];
    assert_eq!(first.len(), 2);
    assert_eq!(first[0], recent);
    assert!(first[1] >= now);
    assert_eq!(counts["second-client"].len(), 1);
}

fn oidc_required_security() -> HttpSecurity {
    HttpSecurity {
        api_key: Some(Arc::from("coverage-api-key")),
        metrics_require_auth: false,
        rate_limit_per_minute: 1,
        allowed_ips: None,
        dashboard_auth: DashboardAuthConfig {
            mode: crate::dashboard_oidc::DashboardAuthMode::Oidc,
            oidc: None,
            config_error: Some("OIDC fixture is not configured".to_owned()),
        },
    }
}

fn guard_request(
    method: Method,
    path: &str,
    peer: Option<SocketAddr>,
    authenticated: bool,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(peer) = peer {
        builder = builder.extension(ConnectInfo(peer));
    }
    if authenticated {
        builder = builder.header(AUTHORIZATION, "Bearer coverage-api-key");
    }
    builder.body(Body::empty()).unwrap()
}

#[test]
fn ip_allowlist_uses_the_connection_ip_without_the_port() {
    let mut security = oidc_required_security();
    let no_peer = guard_request(Method::GET, "/", None, false);
    security.allowed_ips = Some(Vec::new());
    assert!(ip_allowed(&security, &no_peer));

    let allowed: SocketAddr = "[::1]:12345".parse().unwrap();
    let denied: SocketAddr = "192.0.2.5:12345".parse().unwrap();
    security.allowed_ips = Some(vec![allowed.ip()]);
    let allowed_req = guard_request(Method::GET, "/", Some(allowed), false);
    let denied_req = guard_request(Method::GET, "/", Some(denied), false);
    assert_eq!(client_ip(&allowed_req), Some(allowed.ip()));
    assert_eq!(client_ip(&no_peer), None);
    assert!(ip_allowed(&security, &allowed_req));
    assert!(!ip_allowed(&security, &denied_req));
    assert!(!ip_allowed(&security, &no_peer));
}

async fn assert_guard_response(
    app: &Router,
    request: Request<Body>,
    expected_status: StatusCode,
    expected_body: &str,
) {
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), expected_status);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), expected_body.as_bytes());
}

#[tokio::test]
async fn request_guard_handles_preflight_and_rejects_disallowed_requests() {
    let allowed: SocketAddr = "192.0.2.10:12345".parse().unwrap();
    let denied: SocketAddr = "192.0.2.20:12345".parse().unwrap();
    let mut security = oidc_required_security();
    security.allowed_ips = Some(vec![allowed.ip()]);
    let rate = Arc::new(RateLimiter::new(security.rate_limit_per_minute));
    let security = Arc::new(security);
    let app = Router::new()
        .route("/v1/rpc", any(|| async { "rpc handler" }))
        .route("/metrics", any(|| async { "metrics handler" }))
        .layer(middleware::from_fn(
            move |req: Request<Body>, next: Next| {
                request_guard(Arc::clone(&security), Arc::clone(&rate), req, next)
            },
        ));

    for (request, status, body) in [
        (
            guard_request(Method::OPTIONS, "/v1/rpc", Some(denied), false),
            StatusCode::OK,
            "rpc handler",
        ),
        (
            guard_request(Method::GET, "/metrics", None, false),
            StatusCode::FORBIDDEN,
            "client IP not in allowlist",
        ),
        (
            guard_request(Method::GET, "/metrics", Some(allowed), false),
            StatusCode::OK,
            "metrics handler",
        ),
        (
            guard_request(Method::GET, "/v1/rpc", Some(denied), true),
            StatusCode::FORBIDDEN,
            "client IP not in allowlist",
        ),
        (
            guard_request(Method::GET, "/v1/rpc", Some(allowed), false),
            StatusCode::SERVICE_UNAVAILABLE,
            "OIDC fixture is not configured",
        ),
        (
            guard_request(Method::GET, "/v1/rpc", Some(allowed), true),
            StatusCode::OK,
            "rpc handler",
        ),
        (
            guard_request(Method::GET, "/v1/rpc", Some(allowed), true),
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded",
        ),
    ] {
        assert_guard_response(&app, request, status, body).await;
    }
}

#[test]
fn request_auth_rejects_missing_bearer_or_oidc_session() {
    match std::env::var(ENV_CASE).as_deref() {
        Ok("bearer") => {
            let security = HttpSecurity::from_env();
            assert!(!security.dashboard_auth.oidc_enabled());
            let missing = guard_request(Method::GET, "/v1/rpc", None, false);
            assert_eq!(
                request_auth_rejection(&security, "/v1/rpc", missing.headers()),
                Some((
                    StatusCode::UNAUTHORIZED,
                    "missing or invalid Authorization: Bearer token".to_owned(),
                ))
            );
            let authenticated = guard_request(Method::GET, "/v1/rpc", None, true);
            assert_eq!(
                request_auth_rejection(&security, "/v1/rpc", authenticated.headers()),
                None
            );
        }
        Ok("oidc") => {
            let security = HttpSecurity::from_env();
            assert!(security.dashboard_auth.oidc_enabled());
            let (status, reason) = request_auth_rejection(&security, "/v1/rpc", &HeaderMap::new())
                .expect("an OIDC-protected route must reject a missing session");

            if security.dashboard_auth.oidc.is_some() {
                assert_eq!(status, StatusCode::UNAUTHORIZED);
                let session_error = reason
                    .strip_prefix("dashboard OIDC session required: ")
                    .expect("the rejection should explain that a session is required");
                assert!(!session_error.is_empty());
            } else {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(
                    Some(reason.as_str()),
                    security.dashboard_auth.config_error.as_deref()
                );
            }
        }
        _ => {
            let test = "request_auth_rejects_missing_bearer_or_oidc_session";
            run_env_case(
                test,
                "bearer",
                &[("HERMES_HTTP_API_KEY", "coverage-api-key")],
            );
            let callback = "https://dashboard.example.test/auth/callback";
            let secret = format!("redaction-{}-fixture", std::process::id());
            run_env_case(
                test,
                "oidc",
                &[
                    ("HERMES_DASHBOARD_AUTH_MODE", "oidc"),
                    (
                        "HERMES_DASHBOARD_OIDC_ISSUER",
                        "https://issuer.example.test",
                    ),
                    ("HERMES_DASHBOARD_OIDC_CLIENT_ID", "coverage-client"),
                    (
                        "HERMES_DASHBOARD_OIDC_CLIENT_SECRET",
                        "coverage-client-secret",
                    ),
                    ("HERMES_DASHBOARD_OIDC_REDIRECT_URI", callback),
                    ("HERMES_DASHBOARD_OIDC_REDIRECT_URL", callback),
                    ("HERMES_DASHBOARD_OIDC_CALLBACK_URL", callback),
                    ("HERMES_DASHBOARD_OIDC_SESSION_SECRET", &secret),
                    ("HERMES_DASHBOARD_SESSION_SECRET", &secret),
                    ("HERMES_DASHBOARD_OIDC_ALLOWED_EMAILS", "ops@example.test"),
                    (
                        "HERMES_DASHBOARD_OIDC_ALLOWED_EMAIL_DOMAINS",
                        "example.test",
                    ),
                ],
            );
            return;
        }
    }
    println!("{ENV_CHECKED}");
}
