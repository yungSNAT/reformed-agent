use super::{enforce_message_size, is_event_stream, sse_event_fields, SseResponseFinder};
use crate::http_request_headers::{add_protocol_headers, protocol_owned_header, ParameterHeader};
use crate::McpError;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

pub(super) struct ModernHttp {
    client: Client,
    url: Url,
    auth_token: Option<String>,
    configured_headers: Vec<(String, String)>,
    timeout: Duration,
    max_message_bytes: usize,
    tool_headers: HashMap<String, Vec<ParameterHeader>>,
}

impl ModernHttp {
    pub(super) fn new(transport: &super::HttpSseTransport) -> Result<Self, McpError> {
        let url = Url::parse(&transport.url)
            .map_err(|_| McpError::Config("Invalid modern MCP HTTP endpoint".into()))?;
        let loopback = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(McpError::Config(
                "Modern MCP requires HTTPS or loopback HTTP and separate authentication".into(),
            ));
        }
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("HTTP client builds as Client::new() does"),
            url,
            auth_token: transport.auth_token.clone(),
            configured_headers: transport.headers.clone(),
            timeout: transport.sse_read_timeout,
            max_message_bytes: transport.max_message_bytes,
            tool_headers: HashMap::new(),
        })
    }

    pub(super) async fn exchange(
        &mut self,
        message: &Value,
        probing: bool,
    ) -> Result<Option<Value>, McpError> {
        let id = message.get("id").ok_or_else(|| {
            McpError::InvalidParams("Modern MCP HTTP requires a request ID".to_string())
        })?;
        let method = message["method"].as_str().unwrap_or_default();
        let annotations = if method == "tools/call" {
            let name = message["params"]["name"].as_str().unwrap_or_default();
            self.tool_headers
                .get(name)
                .ok_or_else(|| McpError::MethodNotFound("Unknown modern MCP tool".to_string()))?
                .as_slice()
        } else {
            &[]
        };

        let headers = request_headers(self, message, annotations)?;
        let body =
            serde_json::to_vec(message).map_err(|err| McpError::Serialization(err.to_string()))?;
        enforce_message_size(body.len(), self.max_message_bytes)?;
        let mut response = self
            .client
            .post(self.url.clone())
            .headers(headers)
            .timeout(self.timeout)
            .body(body)
            .send()
            .await
            .map_err(|_| McpError::ConnectionError("Modern MCP HTTP request failed".into()))?;
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(McpError::Forbidden(format!(
                "MCP HTTP status {}",
                status.as_u16()
            )));
        }
        let compatibility_status = matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        );
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let is_sse = is_event_stream(content_type);
        let is_json = content_type
            .split(';')
            .next()
            .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
        let limit = self.max_message_bytes;

        let mut reply = if status == StatusCode::OK && is_sse {
            read_scoped_stream(&mut response, id, limit, self.timeout).await?
        } else if status == StatusCode::OK && is_json {
            let bytes = read_bounded_body(&mut response, limit).await?;
            serde_json::from_slice(&bytes).map_err(|_| invalid_result())?
        } else if compatibility_status {
            let bytes = read_bounded_body(&mut response, limit).await?;
            let parsed = serde_json::from_slice::<Value>(&bytes).ok();
            let code = parsed
                .as_ref()
                .and_then(|reply| reply.pointer("/error/code"))
                .and_then(Value::as_i64);
            let is_modern_error = matches!(code, Some(-32022..=-32020))
                || (status == StatusCode::NOT_FOUND && code == Some(-32601));
            match (parsed, is_modern_error, probing) {
                (Some(reply), true, _) => reply,
                (_, false, true) => return Ok(None),
                _ => return Err(http_status_error(status)),
            }
        } else {
            return Err(http_status_error(status));
        };
        if !response_matches(&reply, id) {
            return Err(invalid_result());
        }
        if let Some(error) = reply.get_mut("error") {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .ok_or_else(invalid_result)?;
            // Remote error text may contain request parameters or credentials.
            *error = serde_json::json!({"code": code, "message": format!("MCP HTTP error {code}")});
        }
        if method == "tools/list"
            && reply.pointer("/result/resultType").and_then(Value::as_str) == Some("complete")
        {
            self.accept_tool_schemas(&mut reply);
        }
        Ok(Some(reply))
    }

    fn accept_tool_schemas(&mut self, reply: &mut Value) {
        let Some(tools) = reply
            .pointer_mut("/result/tools")
            .and_then(Value::as_array_mut)
        else {
            return;
        };
        self.tool_headers.clear();
        let mut seen = HashSet::new();
        let mut duplicates = HashSet::new();
        for tool in tools.iter() {
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                if !seen.insert(name) {
                    duplicates.insert(name.to_string());
                }
            }
        }
        tools.retain(|tool| {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                return false;
            };
            if duplicates.contains(name) {
                tracing::warn!("Ignoring an MCP tool with a duplicate name");
                return false;
            }
            match ParameterHeader::from_schema(&tool["inputSchema"]) {
                Ok(headers) => {
                    self.tool_headers.insert(name.to_string(), headers);
                    true
                }
                Err(reason) => {
                    tracing::warn!("Ignoring an MCP tool with invalid header schema: {reason}");
                    false
                }
            }
        });
    }
}

fn request_headers(
    transport: &ModernHttp,
    message: &Value,
    annotations: &[ParameterHeader],
) -> Result<HeaderMap, McpError> {
    let invalid = || McpError::Config("Invalid configured MCP HTTP header".into());
    let mut headers = HeaderMap::new();
    for (raw_name, raw_value) in &transport.configured_headers {
        let name = HeaderName::from_bytes(raw_name.as_bytes()).map_err(|_| invalid())?;
        if protocol_owned_header(&name) {
            return Err(McpError::Config(
                "Configured header is reserved by MCP HTTP".into(),
            ));
        }
        let mut value = HeaderValue::from_str(raw_value).map_err(|_| invalid())?;
        value.set_sensitive(true);
        if headers.insert(name, value).is_some() {
            return Err(McpError::Config(
                "Duplicate configured MCP HTTP header".into(),
            ));
        }
    }
    if let Some(token) = &transport.auth_token {
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| invalid())?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    add_protocol_headers(&mut headers, message, annotations)?;
    Ok(headers)
}

async fn read_bounded_body(
    response: &mut reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, McpError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| invalid_result())? {
        enforce_message_size(bytes.len().saturating_add(chunk.len()), limit)?;
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_scoped_stream(
    response: &mut reqwest::Response,
    id: &Value,
    limit: usize,
    timeout: Duration,
) -> Result<Value, McpError> {
    let mut bytes = Vec::new();
    let mut events = SseResponseFinder::default();
    loop {
        let chunk = tokio::time::timeout(timeout, response.chunk())
            .await
            .map_err(|_| invalid_result())?
            .map_err(|_| invalid_result())?
            .ok_or_else(invalid_result)?;
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        while let Some(event) = events.next_event(&bytes) {
            let text = std::str::from_utf8(&bytes[event]).map_err(|_| invalid_result())?;
            if let Some((_, data)) = sse_event_fields(text) {
                let message: Value = serde_json::from_str(&data).map_err(|_| invalid_result())?;
                if response_matches(&message, id) {
                    return Ok(message);
                }
                if message.get("id").is_some() && message.get("method").is_some() {
                    return Err(invalid_result());
                }
            }
        }
        enforce_message_size(
            bytes
                .len()
                .saturating_add(chunk.len().saturating_sub(remaining)),
            limit,
        )?;
    }
}

fn response_matches(reply: &Value, id: &Value) -> bool {
    reply.get("id") == Some(id) && super::valid_modern_response_envelope(reply)
}

fn invalid_result() -> McpError {
    McpError::Protocol {
        code: -32700,
        message: "Malformed or incomplete modern MCP HTTP response".into(),
    }
}

fn http_status_error(status: StatusCode) -> McpError {
    McpError::ConnectionError(format!(
        "Unexpected MCP HTTP response ({})",
        status.as_u16()
    ))
}
