//! Google OAuth2 credential loading and access-token minting for Vertex AI.
//!
//! Upstream delegates this to the `google-auth` Python library (pinned at
//! 2.55.1). This module performs the documented protocols that library uses
//! for the credential kinds the Vertex adapter can reach:
//!
//! - service-account key: an RS256-signed JWT exchanged at Google's OAuth2
//!   token endpoint (RFC 7523 JWT bearer grant);
//! - authorised-user ADC (`gcloud auth application-default login`): a
//!   refresh-token grant at Google's OAuth2 token endpoint;
//! - Compute Engine metadata server: the instance service account's token.
//!
//! Errors never include key material, refresh tokens or access tokens.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hermes_core::subprocess::CommandNoWindowExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

use super::{env_value, EnvLookup};

pub(super) const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// The assertion audience and exchange endpoint for Google OAuth2 credentials.
pub(super) const GOOGLE_OAUTH2_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const JWT_LIFETIME_SECS: u64 = 3600;
const TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const TOKEN_ENDPOINT_ATTEMPTS: u32 = 3;
const RETRYABLE_STATUS_CODES: [u16; 5] = [500, 503, 504, 408, 429];
const RETRYABLE_ERROR_CODES: [&str; 3] = [
    "internal_failure",
    "server_error",
    "temporarily_unavailable",
];

const ADC_FILENAME: &str = "application_default_credentials.json";
const ADC_NOT_FOUND: &str = "Your default credentials were not found. To set up Application \
     Default Credentials, see https://cloud.google.com/docs/authentication/external/set-up-adc \
     for more information.";

const METADATA_DEFAULT_HOST: &str = "metadata.google.internal";
const METADATA_DEFAULT_IP: &str = "169.254.169.254";
const METADATA_FLAVOR_HEADER: &str = "metadata-flavor";
const METADATA_FLAVOR_VALUE: &str = "Google";
const METADATA_GET_ATTEMPTS: u32 = 5;
const GCE_PRODUCT_NAME_FILE: &str = "/sys/class/dmi/id/product_name";

/// A short-lived OAuth2 access token. `expires_at` is `None`, a token that
/// never expires, when the token endpoint gave no `expires_in` (as
/// google-auth treats it) or one too far ahead for `Instant` to represent.
pub(super) struct AccessToken {
    pub(super) value: String,
    pub(super) expires_at: Option<Instant>,
}

/// Credentials that can mint access tokens. Deliberately not `Debug`: the
/// variants hold a signing key and a refresh token.
pub(super) enum GoogleCredentials {
    ServiceAccount(ServiceAccountKey),
    AuthorizedUser(AuthorizedUser),
    ComputeMetadata { root: String, timeout: Duration },
}

pub(super) struct ServiceAccountKey {
    client_email: String,
    private_key_id: Option<String>,
    signing_key: EncodingKey,
}

pub(super) struct AuthorizedUser {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

impl GoogleCredentials {
    /// Mint a fresh access token.
    pub(super) async fn mint(
        &mut self,
        http: &Client,
        metadata_http: &Client,
        google_token_endpoint: &str,
    ) -> Result<AccessToken, String> {
        match self {
            Self::ServiceAccount(key) => {
                let assertion = key.signed_assertion()?;
                let body = post_token_form(
                    http,
                    google_token_endpoint,
                    &[
                        ("grant_type", JWT_BEARER_GRANT_TYPE),
                        ("assertion", assertion.as_str()),
                    ],
                )
                .await?;
                parse_access_token(&body)
            }
            Self::AuthorizedUser(user) => {
                let body = post_token_form(
                    http,
                    google_token_endpoint,
                    &[
                        ("grant_type", "refresh_token"),
                        ("client_id", user.client_id.as_str()),
                        ("client_secret", user.client_secret.as_str()),
                        ("refresh_token", user.refresh_token.as_str()),
                    ],
                )
                .await?;
                let token = parse_access_token(&body)?;
                // The grant may rotate the refresh token; google-auth keeps the
                // new one in memory, so do the same.
                if let Some(rotated) = body.get("refresh_token").and_then(Value::as_str) {
                    user.refresh_token = rotated.to_string();
                }
                Ok(token)
            }
            Self::ComputeMetadata { root, timeout } => {
                let url = format!(
                    "{root}instance/service-accounts/default/token?scopes={CLOUD_PLATFORM_SCOPE}"
                );
                let (status, text) = metadata_get(metadata_http, &url, *timeout).await?;
                if status != StatusCode::OK {
                    return Err(format!(
                        "the Compute Engine metadata server returned HTTP {} for the access token",
                        status.as_u16()
                    ));
                }
                let body: Value = serde_json::from_str(&text).map_err(|_| {
                    "the Compute Engine metadata server returned invalid JSON for the access token"
                        .to_string()
                })?;
                parse_access_token(&body)
            }
        }
    }
}

impl ServiceAccountKey {
    fn signed_assertion(&self) -> Result<String, String> {
        let iat = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is before the Unix epoch".to_string())?
            .as_secs();
        let claims = json!({
            "iat": iat,
            "exp": iat + JWT_LIFETIME_SECS,
            "iss": self.client_email,
            "aud": GOOGLE_OAUTH2_TOKEN_ENDPOINT,
            "scope": CLOUD_PLATFORM_SCOPE,
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = self.private_key_id.clone();
        jsonwebtoken::encode(&header, &claims, &self.signing_key)
            .map_err(|_| "could not sign the service account assertion".to_string())
    }
}

/// Load a service-account key file, as `service_account.Credentials.
/// from_service_account_file` does. Returns the credentials and the
/// `project_id` embedded in the file, if any.
pub(super) fn load_service_account_file(
    path: &Path,
) -> Result<(GoogleCredentials, Option<String>), String> {
    let info = read_json_file(path, "service account")?;
    let key = service_account_from_info(&info)?;
    Ok((
        GoogleCredentials::ServiceAccount(key),
        string_field(&info, "project_id"),
    ))
}

fn service_account_from_info(info: &Value) -> Result<ServiceAccountKey, String> {
    require_fields(
        info,
        &["client_email", "private_key", "token_uri"],
        "Service account",
    )?;
    let private_key = string_field(info, "private_key").unwrap_or_default();
    let signing_key = EncodingKey::from_rsa_pem(private_key.as_bytes()).map_err(|_| {
        "Service account private_key is not a valid PEM-encoded RSA key.".to_string()
    })?;
    Ok(ServiceAccountKey {
        client_email: string_field(info, "client_email").unwrap_or_default(),
        private_key_id: string_field(info, "private_key_id"),
        signing_key,
    })
}

fn authorized_user_from_info(info: &Value) -> Result<AuthorizedUser, String> {
    require_fields(
        info,
        &["client_id", "client_secret", "refresh_token"],
        "Authorized user",
    )?;
    Ok(AuthorizedUser {
        client_id: string_field(info, "client_id").unwrap_or_default(),
        client_secret: string_field(info, "client_secret").unwrap_or_default(),
        refresh_token: string_field(info, "refresh_token").unwrap_or_default(),
    })
}

/// google-auth's `load_credentials_from_file`, limited to the two file types
/// this port implements. Other ADC types are refused by name.
fn load_adc_file(path: &Path, origin: &str) -> Result<(GoogleCredentials, Option<String>), String> {
    if !path.exists() {
        return Err(format!(
            "The credentials file named by {origin} was not found."
        ));
    }
    let info = read_json_file(path, "credentials")?;
    match info.get("type").and_then(Value::as_str) {
        Some("authorized_user") => Ok((
            GoogleCredentials::AuthorizedUser(authorized_user_from_info(&info)?),
            None,
        )),
        Some("service_account") => Ok((
            GoogleCredentials::ServiceAccount(service_account_from_info(&info)?),
            string_field(&info, "project_id"),
        )),
        Some(
            kind @ ("external_account"
            | "external_account_authorized_user"
            | "impersonated_service_account"
            | "gdch_service_account"),
        ) => Err(format!(
            "The {origin} credentials have type '{kind}', which this Rust port does not \
             support. Use a service account key (VERTEX_CREDENTIALS_PATH) or \
             'gcloud auth application-default login'."
        )),
        _ => Err(format!(
            "The {origin} credentials file does not have a valid type. Expected one of \
             authorized_user, service_account."
        )),
    }
}

/// Application Default Credentials discovery, in google-auth's `default()`
/// order: `GOOGLE_APPLICATION_CREDENTIALS`, the Cloud SDK well-known file,
/// then the Compute Engine metadata server. (The App Engine gen-1 check
/// applies only to the legacy Python 2.7 runtime and has no Rust equivalent.)
///
/// Returns the credentials and the project ID, with `GOOGLE_CLOUD_PROJECT` or
/// `GCLOUD_PROJECT` taking precedence as in google-auth. When
/// `project_known` is true the caller already has a project override, so the
/// `gcloud config get project` lookup is skipped.
pub(super) async fn application_default(
    env: &EnvLookup,
    http: &Client,
    project_known: bool,
    explicit_file: Option<&Path>,
) -> Result<(GoogleCredentials, Option<String>), String> {
    let explicit_project =
        env_value(env, "GOOGLE_CLOUD_PROJECT").or_else(|| env_value(env, "GCLOUD_PROJECT"));
    let skip_gcloud = project_known || explicit_project.is_some();
    let sdk_path = cloud_sdk_adc_path(env);

    let (credentials, project) = match explicit_file {
        Some(file) if sdk_path.as_deref() != Some(file) => {
            load_adc_file(file, "GOOGLE_APPLICATION_CREDENTIALS")?
        }
        _ => match sdk_path.filter(|path| path.is_file()) {
            Some(path) => {
                tracing::debug!("Vertex ADC: using the Cloud SDK credentials file");
                let (credentials, project) = load_adc_file(&path, "Cloud SDK")?;
                let project = match project {
                    Some(project) => Some(project),
                    None if skip_gcloud => None,
                    None => gcloud_config_project().await,
                };
                (credentials, project)
            }
            None if metadata_server_available(env, http).await? => {
                tracing::debug!("Vertex ADC: using the Compute Engine metadata server");
                let root = metadata_root(env);
                let timeout = metadata_timeout(env);
                let project = metadata_project_id(http, &root, timeout).await;
                (
                    GoogleCredentials::ComputeMetadata { root, timeout },
                    project,
                )
            }
            None => return Err(ADC_NOT_FOUND.to_string()),
        },
    };
    Ok((credentials, explicit_project.or(project)))
}

pub(super) fn has_local_adc(env: &EnvLookup) -> bool {
    cloud_sdk_adc_path(env).is_some_and(|path| path.is_file())
        || (env("NO_GCE_CHECK").as_deref() != Some("true")
            && (["GCE_METADATA_HOST", "GCE_METADATA_ROOT", "GCE_METADATA_IP"]
                .into_iter()
                .any(|key| env_value(env, key).is_some())
                || gce_product_name()))
}

/// `_cloud_sdk.get_application_default_credentials_path`.
fn cloud_sdk_adc_path(env: &EnvLookup) -> Option<PathBuf> {
    let config_dir = match env("CLOUDSDK_CONFIG") {
        Some(dir) => PathBuf::from(dir),
        None if cfg!(windows) => match env("APPDATA") {
            Some(appdata) => PathBuf::from(appdata).join("gcloud"),
            None => {
                let drive = env("SystemDrive").unwrap_or_else(|| "C:".to_string());
                PathBuf::from(format!("{drive}\\")).join("gcloud")
            }
        },
        None => env("HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir)?
            .join(".config")
            .join("gcloud"),
    };
    Some(config_dir.join(ADC_FILENAME))
}

/// `_cloud_sdk.get_project_id`: the active gcloud project, or `None` when
/// gcloud is missing or has no project set.
async fn gcloud_config_project() -> Option<String> {
    let program = if cfg!(windows) {
        "gcloud.cmd"
    } else {
        "gcloud"
    };
    let output = tokio::process::Command::new(program)
        .args(["config", "get", "project"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .suppress_windows_console()
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let project = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!project.is_empty()).then_some(project)
}

fn metadata_root(env: &EnvLookup) -> String {
    let host = env_value(env, "GCE_METADATA_HOST")
        .or_else(|| env_value(env, "GCE_METADATA_ROOT"))
        .unwrap_or_else(|| METADATA_DEFAULT_HOST.to_string());
    format!("http://{host}/computeMetadata/v1/")
}

/// `GCE_METADATA_TIMEOUT` in seconds, default 3.
fn metadata_timeout(env: &EnvLookup) -> Duration {
    let secs = env_value(env, "GCE_METADATA_TIMEOUT")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3);
    Duration::from_secs(secs)
}

/// `_metadata.is_on_gce`: ping the metadata server, then on Linux fall back to
/// the SMBIOS product name.
async fn metadata_server_available(env: &EnvLookup, http: &Client) -> Result<bool, String> {
    if env("NO_GCE_CHECK").as_deref() == Some("true") {
        return Ok(false);
    }
    // google-auth's default mode is plain HTTP; its mTLS modes need client
    // certificates this port does not load, so refuse rather than downgrade.
    if let Some(mode) = env_value(env, "GCE_METADATA_MTLS_MODE") {
        if !mode.eq_ignore_ascii_case("none") {
            return Err(format!(
                "GCE_METADATA_MTLS_MODE={mode} requires mutual TLS to the metadata server, \
                 which this Rust port does not implement."
            ));
        }
    }
    let timeout = metadata_timeout(env);
    let attempts = env_value(env, "GCE_METADATA_DETECT_RETRIES")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(3);
    let ip = env_value(env, "GCE_METADATA_IP").unwrap_or_else(|| METADATA_DEFAULT_IP.to_string());
    let url = format!("http://{ip}");
    for attempt in 1..=attempts {
        match http
            .get(&url)
            .header(METADATA_FLAVOR_HEADER, METADATA_FLAVOR_VALUE)
            .timeout(timeout)
            .send()
            .await
        {
            Ok(response) => {
                let flavor_ok = response
                    .headers()
                    .get(METADATA_FLAVOR_HEADER)
                    .and_then(|v| v.to_str().ok())
                    == Some(METADATA_FLAVOR_VALUE);
                return Ok(response.status() == StatusCode::OK && flavor_ok);
            }
            Err(err) => {
                tracing::debug!(
                    "Compute Engine metadata server unavailable on attempt {attempt} of {attempts}: {err}"
                );
                if attempt < attempts {
                    tokio::time::sleep(backoff_delay(attempt)).await;
                }
            }
        }
    }
    Ok(gce_product_name())
}

fn gce_product_name() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string(GCE_PRODUCT_NAME_FILE)
            .is_ok_and(|name| name.trim().starts_with("Google"))
}

async fn metadata_project_id(http: &Client, root: &str, timeout: Duration) -> Option<String> {
    let url = format!("{root}project/project-id");
    match metadata_get(http, &url, timeout).await {
        Ok((StatusCode::OK, text)) => Some(text.trim().to_string()).filter(|p| !p.is_empty()),
        _ => None,
    }
}

/// `_metadata.get`: retries transport errors and retryable statuses.
async fn metadata_get(
    http: &Client,
    url: &str,
    timeout: Duration,
) -> Result<(StatusCode, String), String> {
    let mut attempt = 1;
    loop {
        let result = http
            .get(url)
            .header(METADATA_FLAVOR_HEADER, METADATA_FLAVOR_VALUE)
            .timeout(timeout)
            .send()
            .await;
        let retry_reason = match result {
            Ok(response) if !RETRYABLE_STATUS_CODES.contains(&response.status().as_u16()) => {
                let status = response.status();
                if status == StatusCode::OK
                    && response
                        .headers()
                        .get(METADATA_FLAVOR_HEADER)
                        .and_then(|value| value.to_str().ok())
                        != Some(METADATA_FLAVOR_VALUE)
                {
                    return Err(
                        "the Compute Engine metadata response omitted Metadata-Flavor: Google"
                            .to_string(),
                    );
                }
                let text = response.text().await.map_err(|err| {
                    format!("could not read the Compute Engine metadata response: {err}")
                })?;
                return Ok((status, text));
            }
            Ok(response) => format!("HTTP {}", response.status().as_u16()),
            Err(err) => err.to_string(),
        };
        if attempt >= METADATA_GET_ATTEMPTS {
            return Err(format!(
                "the Compute Engine metadata server is unavailable ({retry_reason})"
            ));
        }
        tokio::time::sleep(backoff_delay(attempt)).await;
        attempt += 1;
    }
}

/// `_client._token_endpoint_request`: a form POST, retried on transient
/// failures. Returns the decoded success body.
async fn post_token_form(
    http: &Client,
    token_uri: &str,
    form: &[(&str, &str)],
) -> Result<Value, String> {
    let mut attempt = 1;
    loop {
        let response = http
            .post(token_uri)
            .timeout(TOKEN_REQUEST_TIMEOUT)
            .form(form)
            .send()
            .await
            .map_err(|err| format!("token request failed: {}", err.without_url()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or(Value::Null);
        if status == StatusCode::OK {
            return Ok(body);
        }
        let retryable = RETRYABLE_STATUS_CODES.contains(&status.as_u16())
            || ["error", "error_description"].iter().any(|field| {
                body.get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|code| RETRYABLE_ERROR_CODES.contains(&code))
            });
        if !retryable || attempt >= TOKEN_ENDPOINT_ATTEMPTS {
            return Err(token_endpoint_error(status, &body));
        }
        tracing::warn!(
            "Google token endpoint returned HTTP {} (attempt {attempt} of {TOKEN_ENDPOINT_ATTEMPTS}); retrying",
            status.as_u16()
        );
        tokio::time::sleep(backoff_delay(attempt)).await;
        attempt += 1;
    }
}

/// Report only the OAuth2 `error` and `error_description` fields; any other
/// response content stays out of the message.
fn token_endpoint_error(status: StatusCode, body: &Value) -> String {
    match body.get("error").and_then(Value::as_str) {
        Some(code) => match body.get("error_description").and_then(Value::as_str) {
            Some(description) => {
                format!("token endpoint rejected the request: {code}: {description}")
            }
            None => format!("token endpoint rejected the request: {code}"),
        },
        None => format!("token endpoint returned HTTP {}", status.as_u16()),
    }
}

/// `_client.jwt_grant` / `_parse_expiry`.
fn parse_access_token(body: &Value) -> Result<AccessToken, String> {
    let value = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "No access token in response.".to_string())?
        .to_string();
    let expires_in = match body.get("expires_in") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(
            n.as_i64()
                .or_else(|| n.as_f64().map(|secs| secs as i64))
                .ok_or_else(invalid_expiry)?,
        ),
        Some(Value::String(s)) => Some(s.trim().parse::<i64>().map_err(|_| invalid_expiry())?),
        Some(_) => return Err(invalid_expiry()),
    };
    // An expiry too far ahead for `Instant` is as good as none.
    let expires_at = expires_in.and_then(|secs| {
        Instant::now().checked_add(Duration::from_secs(secs.max(0).unsigned_abs()))
    });
    Ok(AccessToken { value, expires_at })
}

fn invalid_expiry() -> String {
    "the token response has an invalid expires_in".to_string()
}

/// google-auth's exponential backoff: 1 s, then doubling.
fn backoff_delay(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << (attempt - 1).min(5))
}

/// Parse a JSON credentials file without echoing its content in errors.
fn read_json_file(path: &Path, kind: &str) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read the {kind} file: {}", err.kind()))?;
    match serde_json::from_str::<Value>(&raw) {
        Ok(value @ Value::Object(_)) => Ok(value),
        _ => Err(format!("The {kind} file is not a valid JSON object.")),
    }
}

fn require_fields(info: &Value, fields: &[&str], kind: &str) -> Result<(), String> {
    let missing: Vec<&str> = fields
        .iter()
        .copied()
        .filter(|field| string_field(info, field).is_none())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{kind} info was not in the expected format, missing fields {}.",
            missing.join(", ")
        ))
    }
}

fn string_field(info: &Value, field: &str) -> Option<String> {
    info.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
