//! Attach the terminal backend to a running Daytona V0 sandbox.

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::redirect::Policy;
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode, Url};

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

const CLOUD_TOOLBOX_URL: &str = "https://proxy.app.daytona.io/toolbox";
const CLOUD_API_URL: &str = "https://app.daytona.io/api";

/// A terminal backend for an explicitly selected Daytona sandbox.
pub struct DaytonaBackend {
    toolbox: Result<Url, &'static str>,
    api_key: Option<String>,
    default_timeout: u64,
    max_output_size: usize,
    client: Client,
}

impl DaytonaBackend {
    /// Attach using a trusted Toolbox base URL before the sandbox ID.
    ///
    /// Missing arguments use DAYTONA_TOOLBOX_URL, DAYTONA_API_KEY and
    /// DAYTONA_SANDBOX_ID. DAYTONA_WORKSPACE_ID remains an alias for the ID.
    /// An explicitly customised control plane also needs a Toolbox URL.
    pub fn new(
        toolbox_url: Option<String>,
        api_key: Option<String>,
        sandbox_id: Option<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Self {
        let toolbox_url = toolbox_url.or_else(|| std::env::var("DAYTONA_TOOLBOX_URL").ok());
        let sandbox_id = sandbox_id
            .or_else(|| std::env::var("DAYTONA_SANDBOX_ID").ok())
            .or_else(|| std::env::var("DAYTONA_WORKSPACE_ID").ok());
        let custom_control_plane = toolbox_url.is_none()
            && std::env::var("DAYTONA_API_URL").is_ok_and(|value| {
                let url = value.trim().trim_end_matches('/');
                !url.is_empty() && url != CLOUD_API_URL
            });
        let toolbox = if custom_control_plane {
            Err("Set DAYTONA_TOOLBOX_URL for a custom Daytona control plane")
        } else {
            sandbox_url(
                toolbox_url.as_deref().unwrap_or(CLOUD_TOOLBOX_URL),
                sandbox_id.as_deref(),
            )
        };
        Self {
            toolbox,
            api_key: api_key.or_else(|| std::env::var("DAYTONA_API_KEY").ok()),
            default_timeout,
            max_output_size,
            client: Client::builder()
                .redirect(Policy::none())
                .build()
                .expect("initialise Daytona HTTP client"),
        }
    }

    fn request(
        &self,
        method: Method,
        path: &[&str],
        timeout_secs: u64,
    ) -> Result<RequestBuilder, AgentError> {
        let mut url = self
            .toolbox
            .as_ref()
            .map_err(|message| AgentError::Config((*message).into()))?
            .clone();
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| AgentError::Config("DAYTONA_API_KEY is required".into()))?;
        url.path_segments_mut()
            .expect("validated Daytona Toolbox URL")
            .extend(path.iter().copied());
        Ok(self
            .client
            .request(method, url)
            .bearer_auth(api_key)
            .timeout(Duration::from_secs(timeout_secs.saturating_add(10))))
    }
}

fn sandbox_url(toolbox: &str, id: Option<&str>) -> Result<Url, &'static str> {
    let id = id
        .map(str::trim)
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or("DAYTONA_SANDBOX_ID must be a sandbox ID")?;
    let mut url = Url::parse(toolbox.trim()).map_err(|_| "Invalid Daytona Toolbox URL")?;
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    });
    if url.host_str().is_none()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Daytona Toolbox URL must be HTTPS, or HTTP on loopback, without user information, query or fragment");
    }
    url.path_segments_mut()
        .map_err(|_| "Invalid Daytona Toolbox URL path")?
        .pop_if_empty()
        .push(id);
    Ok(url)
}

#[derive(serde::Serialize)]
struct ExecuteRequest<'a> {
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
    timeout: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteResponse {
    exit_code: i32,
    result: Option<String>,
}

fn request_error(operation: &str, error: reqwest::Error) -> AgentError {
    if error.is_timeout() {
        AgentError::Timeout(format!("Daytona {operation} timed out"))
    } else {
        AgentError::Io(format!("Daytona {operation} request failed"))
    }
}

fn checked(response: Response, operation: &str) -> Result<Response, AgentError> {
    let status = response.status();
    if status == StatusCode::REQUEST_TIMEOUT {
        return Err(AgentError::Timeout(format!(
            "Daytona {operation} timed out"
        )));
    }
    if !status.is_success() {
        return Err(AgentError::Io(format!(
            "Daytona {operation} returned {status}"
        )));
    }
    Ok(response)
}

#[async_trait]
impl TerminalBackend for DaytonaBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        _background: bool,
        _pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let timeout = timeout.unwrap_or(self.default_timeout);
        let response = self
            .request(Method::POST, &["process", "execute"], timeout)?
            .json(&ExecuteRequest {
                command,
                cwd: workdir,
                timeout,
            })
            .send()
            .await
            .map_err(|error| request_error("execute", error))?;
        let data = checked(response, "execute")?
            .json::<ExecuteResponse>()
            .await
            .map_err(|error| request_error("execute response", error))?;
        let mut stdout = data.result.unwrap_or_default();
        stdout.truncate(stdout.floor_char_boundary(self.max_output_size));
        Ok(CommandOutput {
            exit_code: data.exit_code,
            stdout,
            stderr: String::new(),
        })
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        let response = self
            .request(Method::GET, &["files", "download"], self.default_timeout)?
            .query(&[("path", path)])
            .send()
            .await
            .map_err(|error| request_error("download", error))?;
        let content = checked(response, "download")?
            .text()
            .await
            .map_err(|error| request_error("download response", error))?;
        if offset.is_none() && limit.is_none() {
            return Ok(content);
        }

        let start = usize::try_from(offset.unwrap_or(0)).unwrap_or(usize::MAX);
        let count = limit
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(usize::MAX);
        Ok(content
            .lines()
            .skip(start)
            .take(count)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        let response = self
            .request(Method::POST, &["files", "upload-v2"], self.default_timeout)?
            .query(&[("path", path)])
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(content.to_owned())
            .send()
            .await
            .map_err(|error| request_error("upload", error))?;
        checked(response, "upload")?;
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        let response = self
            .request(Method::GET, &["files", "info"], self.default_timeout)?
            .query(&[("path", path)])
            .send()
            .await
            .map_err(|error| request_error("file info", error))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        checked(response, "file info")?;
        Ok(true)
    }
}
