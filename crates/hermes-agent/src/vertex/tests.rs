//! Ports of upstream `tests/agent/test_vertex_adapter.py` and the runtime
//! cases of `tests/hermes_cli/test_vertex_provider.py`, plus the token
//! protocol cases the Rust implementation needs. Every token endpoint and the
//! Vertex API are loopback mocks; no test reads the process environment.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use hermes_core::{AgentError, LlmProvider, Message};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde_json::{json, Value};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

// Throwaway 2048-bit RSA key generated for these tests only. It signs nothing
// but assertions sent to the loopback mock below; the PEM armour is assembled
// at runtime from the label.
const TEST_KEY_PKCS8_BASE64: &[&str] = &[
    "MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDeLOwA1VAbn0dv",
    "D/AlfYUPejkNW2+62d3M3gSg1MHgSbhDzSCSRTg8ylU3wlo2+02jni+bMsszu8oR",
    "2k5gYDSsajWXKaSSn+j+j7yfl3tjuWGkhYD3c7SdRLW7jpg8VuWt98dPTXjCCFRm",
    "az2Ofu5Lac2OptX3H0ij1XQ/RdzJa9E2VvRX+vJjWtbySmrollCRF+K0VTJ3BfwU",
    "dxgNRAxdXP9NFa/NUZauv93PmVY1YDgZDZmnHEFk18GQjBjMsMOI24Db+iPO0nrY",
    "v0GW9RFqR6RLrCWdSw248/kDjOYxonSG2FoqHCcCHnEOzzSrXsNyghV24e4UKBvg",
    "sI28SZchAgMBAAECggEAFtejhApqbAyaOY7378LR9V9pkjf+asWPikqhSy/Upwyt",
    "RztFyAQZ3hYnYM44nEKGHvZaiqhUQANkqSU6vanmgHuC+wGrC/Ao8lMw32+hngUn",
    "RtT7B3X78ddon9nvSPTeNF6vPTYH3lsg4k7QS2A9e79rBOmeQgQTYdTGWOMD6ilc",
    "lIJvIad+EMu4BYfvplX39h+vrO07OGLfMxjv4ALHp8wYD6B6YR9an/B55gFzZP5X",
    "7jn6ECVkmn03cMVJF8VVnZfVfmOzVC0SomfYAnGovWTysYcCpi0FCclF96JW1bzI",
    "fKb84qScy9M1LqrdEXqKST7B64/Pn/J/uFc/x+/QSQKBgQDwigOftqdx0koh7JJf",
    "gUAXVMESZpxrVFoprLWCR8C7T2UN7AD/lAI6zQQGQsmy7iVBRMafatwEP6tgH2dn",
    "WSyJ1ERUjW3zYxukA06erkIVKTxdkij2EEbnWhW5AHetBYWWzHPWv4sHcxnWQ4WU",
    "lDFWrbJXDZLGwgqiSlGlwKH1twKBgQDsdL2S76UfccRPJwC4HJSFhehpi8THOdf8",
    "bV/Avn9NTyzCWpn4H2iYn0eDHDkhZlGmL3cQ82Zuiov5kXx2mOLGxyb5nXPiT5Vz",
    "PBSGQD+rD1xGZxPvJRQjEpDAsw6YOqQJ0mypWQhxrxWWZD9gjW2RGUTyhrnyNw8g",
    "RVTygAoZ5wKBgQCBcnSyzfRcz8OvcAXxubgJFkZ1JZ2GMTX4eYaQ3xZ8dyS2Ek0T",
    "Pc7ZYB2LVQQA9CI+dsnBXxuaeb07lIrsVlQetIo1SotiUPK+GD2GLddIVhGzdYXv",
    "riDKPnO0ROkixvb2iX7Jkfk7ABlmcp3/sKTAtedk69wSdLiOUBqSbhF7DwKBgBir",
    "3ZPVOU9cmXUAxsxu/vgnjqZklPWMjYKjDtiXh8TTPAJTXblL3af/BoEhkrxvZpmG",
    "XlAAdXGXNx+lFG1zakX/RYvJ5+t3Dkrlu+Z+EaaRhbk6zBOptL4sE7pndGBhzffQ",
    "YtIQnhbug7wbXZl/aC05i74aXJBvOj7NvR1ISsWFAoGBAI2RXMgiQs+jf1w6O4cv",
    "Q/yjGtqLmyMu0KAuWmiHCm+NmK++sqRZ1e6KldkQRcrntOdwfzkrq9wB0GULMll1",
    "YC6vLxaIG0p9aVD5zBw3o0Bds4BlLyw4sM7pLkfOOtC7TpSkqP8fLckh8K/9ftGD",
    "6v6Byv81ShTvAVRrRfhtB4H1",
];

const TEST_PUBLIC_KEY_BASE64: &[&str] = &[
    "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA3izsANVQG59Hbw/wJX2F",
    "D3o5DVtvutndzN4EoNTB4Em4Q80gkkU4PMpVN8JaNvtNo54vmzLLM7vKEdpOYGA0",
    "rGo1lymkkp/o/o+8n5d7Y7lhpIWA93O0nUS1u46YPFblrffHT014wghUZms9jn7u",
    "S2nNjqbV9x9Io9V0P0XcyWvRNlb0V/ryY1rW8kpq6JZQkRfitFUydwX8FHcYDUQM",
    "XVz/TRWvzVGWrr/dz5lWNWA4GQ2ZpxxBZNfBkIwYzLDDiNuA2/ojztJ62L9BlvUR",
    "akekS6wlnUsNuPP5A4zmMaJ0hthaKhwnAh5xDs80q17DcoIVduHuFCgb4LCNvEmX",
    "IQIDAQAB",
];

const SA_EMAIL: &str = "vertex-test@sa-project.iam.gserviceaccount.com";
const MODEL: &str = "google/gemini-3-flash-preview";

fn pem(label: &str, lines: &[&str]) -> String {
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}

/// Environment with ADC sources switched off: no metadata server and an empty
/// Cloud SDK directory.
fn isolated_env(dir: &Path) -> HashMap<String, String> {
    HashMap::from([
        ("NO_GCE_CHECK".to_string(), "true".to_string()),
        (
            "CLOUDSDK_CONFIG".to_string(),
            dir.join("gcloud").to_string_lossy().into_owned(),
        ),
    ])
}

fn auth_for(vars: HashMap<String, String>, server: &MockServer) -> Arc<VertexAuth> {
    let mut auth = VertexAuth::with_env(Arc::new(move |key: &str| vars.get(key).cloned()));
    auth.google_token_endpoint = format!("{}/token", server.uri());
    auth.api_origin_override = Some(server.uri());
    Arc::new(auth)
}

fn write_json(path: &Path, value: &Value) -> PathBuf {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, serde_json::to_vec_pretty(value).expect("json")).expect("write");
    path.to_path_buf()
}

fn service_account_json(project: Option<&str>, token_uri: &str) -> Value {
    let mut info = json!({
        "type": "service_account",
        "private_key_id": "test-key-id",
        "private_key": pem("PRIVATE KEY", TEST_KEY_PKCS8_BASE64),
        "client_email": SA_EMAIL,
        "token_uri": token_uri,
    });
    if let Some(project) = project {
        info["project_id"] = json!(project);
    }
    info
}

async fn mount_token(server: &MockServer, token: &str, expires_in: u64) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": token,
            "expires_in": expires_in,
            "token_type": "Bearer",
        })))
        .mount(server)
        .await;
}

async fn token_requests(server: &MockServer) -> Vec<HashMap<String, String>> {
    server
        .received_requests()
        .await
        .expect("recording on")
        .into_iter()
        .filter(|request| request.url.path() == "/token")
        .map(|request| {
            url::form_urlencoded::parse(&request.body)
                .into_owned()
                .collect::<HashMap<_, _>>()
        })
        .collect()
}

fn auth_error_text(result: Result<VertexAccess, AgentError>) -> String {
    match result {
        Err(AgentError::AuthFailed(text)) => text,
        Err(other) => panic!("expected AuthFailed, got {other}"),
        Ok(_) => panic!("expected an error"),
    }
}

fn chat_completion_body(content: &str) -> Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
    })
}

const CHAT_PATH: &str =
    "/v1beta1/projects/sa-project/locations/global/endpoints/openapi/chat/completions";

// --- upstream test_vertex_adapter.py -------------------------------------

#[test]
fn build_base_url_global() {
    assert_eq!(
        build_vertex_base_url("proj", "global"),
        "https://aiplatform.googleapis.com/v1beta1/projects/proj/locations/global/endpoints/openapi"
    );
}

#[test]
fn build_base_url_regional() {
    assert_eq!(
        build_vertex_base_url("proj", "us-central1"),
        "https://us-central1-aiplatform.googleapis.com/v1beta1/projects/proj/locations/us-central1/endpoints/openapi"
    );
}

#[tokio::test]
async fn adc_authorized_user_mints_token_with_default_region() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    mount_token(&server, "ya29.adc-token", 3600).await;
    write_json(
        &dir.path()
            .join("gcloud")
            .join("application_default_credentials.json"),
        &json!({
            "type": "authorized_user",
            "client_id": "adc-client.apps.googleusercontent.com",
            "client_secret": "adc-client-secret",
            "refresh_token": "1//adc-refresh-token",
        }),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert("GOOGLE_CLOUD_PROJECT".into(), "adc-project".into());
    let auth = auth_for(vars, &server);

    let access = auth.access().await.expect("ADC access");
    assert_eq!(access.token, "ya29.adc-token");
    assert_eq!(
        access.base_url,
        format!(
            "{}/v1beta1/projects/adc-project/locations/global/endpoints/openapi",
            server.uri()
        )
    );
    let requests = token_requests(&server).await;
    assert_eq!(requests.len(), 1);
    let form = &requests[0];
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], "1//adc-refresh-token");
    assert_eq!(form["client_id"], "adc-client.apps.googleusercontent.com");
    assert_eq!(form["client_secret"], "adc-client-secret");
    assert!(
        !form.contains_key("scope"),
        "user credentials are not re-scoped"
    );
}

#[tokio::test]
async fn project_and_region_settings_route_the_base_url() {
    // config.yaml `vertex.project_id`/`vertex.region` reach the adapter as
    // VERTEX_PROJECT_ID/VERTEX_REGION (hermes-config bridges them; its tests
    // cover that environment variables win over config.yaml).
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    mount_token(&server, "ya29.sa-token", 3600).await;
    let key = write_json(
        &dir.path().join("sa.json"),
        &service_account_json(Some("sa-project"), &format!("{}/token", server.uri())),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        key.to_string_lossy().into(),
    );
    vars.insert("VERTEX_PROJECT_ID".into(), "cfg-project".into());
    vars.insert("VERTEX_REGION".into(), "europe-west4".into());
    let mut auth = VertexAuth::with_env(Arc::new(move |key: &str| vars.get(key).cloned()));
    auth.google_token_endpoint = format!("{}/token", server.uri());

    let access = auth.access().await.expect("access");
    assert_eq!(
        access.base_url,
        "https://europe-west4-aiplatform.googleapis.com/v1beta1/projects/cfg-project/locations/europe-west4/endpoints/openapi"
    );
}

#[test]
fn has_credentials_via_project_setting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut vars = isolated_env(dir.path());
    vars.insert("VERTEX_PROJECT_ID".into(), "p".into());
    let auth = VertexAuth::with_env(Arc::new(move |key: &str| vars.get(key).cloned()));
    assert!(auth.has_credentials());
}

#[test]
fn has_credentials_false_when_nothing_set() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vars = isolated_env(dir.path());
    let auth = VertexAuth::with_env(Arc::new(move |key: &str| vars.get(key).cloned()));
    assert!(!auth.has_credentials());
}

#[test]
fn has_credentials_ignores_a_missing_key_file_and_accepts_an_existing_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key = write_json(&dir.path().join("sa.json"), &json!({}));
    let mut missing = isolated_env(dir.path());
    missing.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        dir.path().join("absent.json").to_string_lossy().into(),
    );
    let auth = VertexAuth::with_env(Arc::new(move |key: &str| missing.get(key).cloned()));
    assert!(!auth.has_credentials());

    let mut present = isolated_env(dir.path());
    present.insert(
        "GOOGLE_APPLICATION_CREDENTIALS".into(),
        key.to_string_lossy().into(),
    );
    let auth = VertexAuth::with_env(Arc::new(move |key: &str| present.get(key).cloned()));
    assert!(auth.has_credentials());
}

#[tokio::test]
async fn service_account_from_google_application_credentials_when_adc_is_absent() {
    // upstream test_adc_failure_falls_back_to_service_account: a resolvable
    // service-account path is used directly, with its embedded project.
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    mount_token(&server, "ya29.sa-token", 3600).await;
    let key = write_json(
        &dir.path().join("sa.json"),
        &service_account_json(Some("sa-project"), &format!("{}/token", server.uri())),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "GOOGLE_APPLICATION_CREDENTIALS".into(),
        key.to_string_lossy().into(),
    );
    let auth = auth_for(vars, &server);

    let access = auth.access().await.expect("service account access");
    assert_eq!(access.token, "ya29.sa-token");
    assert!(access.base_url.contains("/projects/sa-project/"));
}

// --- upstream test_vertex_provider.py (runtime cases) ---------------------

#[tokio::test]
async fn unresolved_credentials_raise_the_oauth2_auth_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let auth = auth_for(isolated_env(dir.path()), &server);

    let text = auth_error_text(auth.access().await);
    assert!(text.contains("OAuth2"), "{text}");
    assert!(text.contains("not a static API key"), "{text}");
    assert!(
        text.contains("default credentials were not found"),
        "{text}"
    );
    assert!(token_requests(&server).await.is_empty());
}

// --- token protocol -------------------------------------------------------

#[tokio::test]
async fn service_account_assertion_is_an_rs256_jwt_for_the_cloud_platform_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    mount_token(&server, "ya29.sa-token", 3600).await;
    let key = write_json(
        &dir.path().join("sa.json"),
        &service_account_json(Some("sa-project"), &format!("{}/token", server.uri())),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        key.to_string_lossy().into(),
    );
    let auth = auth_for(vars, &server);

    auth.access().await.expect("access");
    let requests = token_requests(&server).await;
    assert_eq!(requests.len(), 1);
    let form = &requests[0];
    assert_eq!(
        form["grant_type"],
        "urn:ietf:params:oauth:grant-type:jwt-bearer"
    );
    let assertion = form["assertion"].as_str();
    let header = decode_header(assertion).expect("header");
    assert_eq!(header.alg, Algorithm::RS256);
    assert_eq!(header.kid.as_deref(), Some("test-key-id"));

    let public_key = pem("PUBLIC KEY", TEST_PUBLIC_KEY_BASE64);
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&["https://oauth2.googleapis.com/token"]);
    let claims = decode::<Value>(
        assertion,
        &DecodingKey::from_rsa_pem(public_key.as_bytes()).expect("public key"),
        &validation,
    )
    .expect("signature verifies with the key pair")
    .claims;
    assert_eq!(claims["iss"], SA_EMAIL);
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/cloud-platform"
    );
    let lifetime = claims["exp"].as_u64().expect("exp") - claims["iat"].as_u64().expect("iat");
    assert_eq!(lifetime, 3600);
}

#[tokio::test]
async fn vertex_credentials_path_takes_precedence_over_google_application_credentials() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    mount_token(&server, "ya29.sa-token", 3600).await;
    let token_uri = format!("{}/token", server.uri());
    let vertex_key = write_json(
        &dir.path().join("vertex.json"),
        &service_account_json(Some("vertex-project"), &token_uri),
    );
    let google_key = write_json(
        &dir.path().join("google.json"),
        &service_account_json(Some("google-project"), &token_uri),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        vertex_key.to_string_lossy().into(),
    );
    vars.insert(
        "GOOGLE_APPLICATION_CREDENTIALS".into(),
        google_key.to_string_lossy().into(),
    );
    let auth = auth_for(vars, &server);

    let access = auth.access().await.expect("access");
    assert!(
        access.base_url.contains("/projects/vertex-project/"),
        "{}",
        access.base_url
    );
}

#[tokio::test]
async fn metadata_server_supplies_token_and_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("metadata-flavor", "Google"))
        .respond_with(ResponseTemplate::new(200).insert_header("metadata-flavor", "Google"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/computeMetadata/v1/project/project-id"))
        .and(header("metadata-flavor", "Google"))
        .respond_with(ResponseTemplate::new(200).set_body_string("gce-project"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/computeMetadata/v1/instance/service-accounts/default/token",
        ))
        .and(query_param(
            "scopes",
            "https://www.googleapis.com/auth/cloud-platform",
        ))
        .and(header("metadata-flavor", "Google"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.gce-token",
            "expires_in": 3599,
            "token_type": "Bearer",
        })))
        .mount(&server)
        .await;
    let address = server.address().to_string();
    let mut vars = isolated_env(dir.path());
    vars.remove("NO_GCE_CHECK");
    vars.insert("GCE_METADATA_IP".into(), address.clone());
    vars.insert("GCE_METADATA_HOST".into(), address);
    let auth = auth_for(vars, &server);

    let access = auth.access().await.expect("metadata access");
    assert_eq!(access.token, "ya29.gce-token");
    assert!(
        access.base_url.contains("/projects/gce-project/"),
        "{}",
        access.base_url
    );
}

#[tokio::test]
async fn cached_token_is_reused_until_within_the_refresh_margin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_uri_server = MockServer::start().await;
    mount_token(&token_uri_server, "ya29.long-lived", 3600).await;
    let key = write_json(
        &dir.path().join("sa.json"),
        &service_account_json(
            Some("sa-project"),
            &format!("{}/token", token_uri_server.uri()),
        ),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        key.to_string_lossy().into(),
    );
    let auth = auth_for(vars.clone(), &token_uri_server);
    auth.access().await.expect("first");
    auth.access().await.expect("second");
    assert_eq!(
        token_requests(&token_uri_server).await.len(),
        1,
        "fresh token is cached"
    );

    // A token with less than five minutes left is re-minted on every call.
    let short_server = MockServer::start().await;
    mount_token(&short_server, "ya29.short-lived", 120).await;
    let short_key = write_json(
        &dir.path().join("short.json"),
        &service_account_json(Some("sa-project"), &format!("{}/token", short_server.uri())),
    );
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        short_key.to_string_lossy().into(),
    );
    let auth = auth_for(vars, &short_server);
    auth.access().await.expect("first");
    auth.access().await.expect("second");
    assert_eq!(
        token_requests(&short_server).await.len(),
        2,
        "near-expiry token is refreshed"
    );
}

#[tokio::test]
async fn token_endpoint_error_is_reported_without_secrets_and_not_cached() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Invalid JWT Signature.",
        })))
        .mount(&server)
        .await;
    let key = write_json(
        &dir.path().join("sa.json"),
        &service_account_json(Some("sa-project"), &format!("{}/token", server.uri())),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        key.to_string_lossy().into(),
    );
    let auth = auth_for(vars, &server);

    let text = auth_error_text(auth.access().await);
    assert!(
        text.contains("invalid_grant: Invalid JWT Signature."),
        "{text}"
    );
    assert!(
        !text.contains(TEST_KEY_PKCS8_BASE64[1]),
        "key material leaked: {text}"
    );
    assert!(!text.contains("assertion"), "{text}");
    auth_error_text(auth.access().await);
    assert_eq!(
        token_requests(&server).await.len(),
        2,
        "failed credentials are dropped"
    );
}

#[tokio::test]
async fn transient_token_endpoint_failure_is_retried() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_token(&server, "ya29.after-retry", 3600).await;
    let key = write_json(
        &dir.path().join("sa.json"),
        &service_account_json(Some("sa-project"), &format!("{}/token", server.uri())),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        key.to_string_lossy().into(),
    );
    let auth = auth_for(vars, &server);

    let access = auth.access().await.expect("access after retry");
    assert_eq!(access.token, "ya29.after-retry");
    assert_eq!(token_requests(&server).await.len(), 2);
}

#[tokio::test]
async fn malformed_credentials_files_are_rejected_without_echoing_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let cases: [(&str, String, &str); 3] = [
        (
            "not-json.json",
            "{\"private_key\": \"secret-material\"".to_string(),
            "is not a valid JSON object",
        ),
        (
            "missing.json",
            json!({"type": "service_account", "private_key": "secret-material"}).to_string(),
            "missing fields client_email, token_uri",
        ),
        (
            "bad-key.json",
            json!({
                "client_email": SA_EMAIL,
                "token_uri": format!("{}/token", server.uri()),
                "private_key": "secret-material",
            })
            .to_string(),
            "private_key is not a valid PEM-encoded RSA key",
        ),
    ];
    for (name, content, expected) in cases {
        let file = dir.path().join(name);
        std::fs::write(&file, content).expect("write");
        let mut vars = isolated_env(dir.path());
        vars.insert(
            "VERTEX_CREDENTIALS_PATH".into(),
            file.to_string_lossy().into(),
        );
        let auth = auth_for(vars, &server);
        let text = auth_error_text(auth.access().await);
        assert!(text.contains(expected), "{name}: {text}");
        assert!(
            !text.contains("secret-material"),
            "{name} leaked content: {text}"
        );
    }
    assert!(token_requests(&server).await.is_empty());
}

#[tokio::test]
async fn unsupported_adc_credential_type_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    write_json(
        &dir.path()
            .join("gcloud")
            .join("application_default_credentials.json"),
        &json!({"type": "external_account", "audience": "//iam.googleapis.com/test"}),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert("VERTEX_PROJECT_ID".into(), "p".into());
    let auth = auth_for(vars, &server);

    let text = auth_error_text(auth.access().await);
    assert!(text.contains("'external_account'"), "{text}");
    assert!(text.contains("does not support"), "{text}");
}

#[tokio::test]
async fn missing_google_application_credentials_file_is_an_adc_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "GOOGLE_APPLICATION_CREDENTIALS".into(),
        dir.path().join("absent.json").to_string_lossy().into(),
    );
    let auth = auth_for(vars, &server);

    let text = auth_error_text(auth.access().await);
    assert!(
        text.contains("GOOGLE_APPLICATION_CREDENTIALS was not found"),
        "{text}"
    );
}

#[tokio::test]
async fn missing_project_id_names_the_settings_to_use() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    mount_token(&server, "ya29.sa-token", 3600).await;
    let key = write_json(
        &dir.path().join("sa.json"),
        &service_account_json(None, &format!("{}/token", server.uri())),
    );
    let mut vars = isolated_env(dir.path());
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        key.to_string_lossy().into(),
    );
    let auth = auth_for(vars, &server);

    let text = auth_error_text(auth.access().await);
    assert!(text.contains("vertex.project_id"), "{text}");
    assert!(text.contains("VERTEX_PROJECT_ID"), "{text}");
}

// --- the request the provider sends ---------------------------------------

async fn provider_fixture(server: &MockServer, dir: &Path) -> VertexProvider {
    let key = write_json(
        &dir.join("sa.json"),
        &service_account_json(Some("sa-project"), &format!("{}/token", server.uri())),
    );
    let mut vars = isolated_env(dir);
    vars.insert(
        "VERTEX_CREDENTIALS_PATH".into(),
        key.to_string_lossy().into(),
    );
    VertexProvider::with_auth(auth_for(vars, server), MODEL)
}

#[tokio::test]
async fn chat_request_goes_to_the_project_endpoint_with_the_minted_bearer_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    mount_token(&server, "ya29.sa-token", 3600).await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(header("authorization", "Bearer ya29.sa-token"))
        .and(body_string_contains(format!("\"model\":\"{MODEL}\"")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_completion_body("hello from vertex")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_fixture(&server, dir.path()).await;

    let response = provider
        .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
        .await
        .expect("chat completion");
    assert_eq!(
        response.message.content.as_deref(),
        Some("hello from vertex")
    );
}

#[tokio::test]
async fn unauthorised_response_re_mints_the_token_and_retries_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.revoked", "expires_in": 3600,
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_token(&server, "ya29.replacement", 3600).await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(header("authorization", "Bearer ya29.revoked"))
        .respond_with(ResponseTemplate::new(401).set_body_string("token revoked"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(header("authorization", "Bearer ya29.replacement"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_completion_body("after refresh")),
        )
        .mount(&server)
        .await;
    let provider = provider_fixture(&server, dir.path()).await;

    let response = provider
        .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
        .await
        .expect("retried completion");
    assert_eq!(response.message.content.as_deref(), Some("after refresh"));
    assert_eq!(token_requests(&server).await.len(), 2);
}

#[tokio::test]
async fn streaming_request_re_mints_after_401_and_streams_the_reply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.revoked", "expires_in": 3600,
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_token(&server, "ya29.replacement", 3600).await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(header("authorization", "Bearer ya29.revoked"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let sse = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices": [{"index": 0, "delta": {"content": "streamed"}}]})
    );
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(header("authorization", "Bearer ya29.replacement"))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;
    let provider = provider_fixture(&server, dir.path()).await;

    let chunks: Vec<_> = provider
        .chat_completion_stream(&[Message::user("hi")], &[], None, None, None, None)
        .collect()
        .await;
    let text: String = chunks
        .into_iter()
        .map(|chunk| chunk.expect("chunk"))
        .filter_map(|chunk| chunk.delta.and_then(|delta| delta.content))
        .collect();
    assert_eq!(text, "streamed");
}
