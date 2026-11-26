//! WeCom (Enterprise WeChat) API adapter.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::platforms::helpers::download_media_url;

const WECOM_API_BASE: &str = "https://qyapi.weixin.qq.com/cgi-bin";

/// Heuristic: WeCom group chat ids often start with `wr` / `ww` / `wm` (length > 8).
fn looks_like_wecom_group_chat_id(id: &str) -> bool {
    let id = id.trim();
    if id.len() < 8 {
        return false;
    }
    id.starts_with("wr") || id.starts_with("ww") || id.starts_with("wm")
}

fn normalized_image_content_type(content_type: Option<&str>) -> Option<String> {
    let normalized = content_type?
        .split(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_ascii_lowercase();
    if normalized.starts_with("image/") {
        Some(normalized)
    } else {
        None
    }
}

fn image_extension_from_content_type(content_type: Option<&str>) -> Option<&'static str> {
    let normalized = normalized_image_content_type(content_type)?;
    match normalized.as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "image/svg+xml" => Some("svg"),
        "image/heic" => Some("heic"),
        "image/heif" => Some("heif"),
        "image/avif" => Some("avif"),
        _ => None,
    }
}

fn remote_image_file_name(image_url: &str, content_type: Option<&str>) -> String {
    let stripped = image_url
        .split('#')
        .next()
        .unwrap_or(image_url)
        .split('?')
        .next()
        .unwrap_or(image_url)
        .trim_end_matches('/');
    let base = stripped.rsplit('/').next().unwrap_or("").trim();
    let mut file_name = if base.is_empty() {
        "image".to_string()
    } else {
        base.to_string()
    };

    let has_extension = std::path::Path::new(&file_name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some();
    if !has_extension {
        let ext = image_extension_from_content_type(content_type).unwrap_or("png");
        file_name.push('.');
        file_name.push_str(ext);
    }
    file_name
}

fn image_fallback_text(image_url: &str, caption: Option<&str>) -> String {
    match caption.map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => format!("{c}\n{image_url}"),
        None => image_url.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeComConfig {
    pub corp_id: String,
    pub agent_id: String,
    pub secret: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct WeComAdapter {
    base: BasePlatformAdapter,
    config: WeComConfig,
    client: Client,
    api_base: String,
    access_token: RwLock<Option<String>>,
    stop_signal: Arc<Notify>,
}

impl WeComAdapter {
    pub fn new(config: WeComConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.corp_id).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            base,
            config,
            client,
            api_base: WECOM_API_BASE.into(),
            access_token: RwLock::new(None),
            stop_signal: Arc::new(Notify::new()),
        })
    }

    pub fn config(&self) -> &WeComConfig {
        &self.config
    }

    /// Get or refresh the access token.
    pub async fn get_access_token(&self) -> Result<String, GatewayError> {
        if let Some(ref token) = *self.access_token.read().await {
            return Ok(token.clone());
        }

        let result = super::helpers::wecom_access_token_reply(
            &self.client,
            &self.api_base,
            &self.config.corp_id,
            &self.config.secret,
        )
        .await
        .map_err(|e| GatewayError::Auth(format!("WeCom auth failed: {e}")))?;

        let token = result
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::Auth("No access_token in WeCom response".into()))?
            .to_string();

        *self.access_token.write().await = Some(token.clone());
        Ok(token)
    }

    /// Send a text message via WeCom API.
    ///
    /// If `user_id` looks like a group chat id (`wr…` / `ww…` / `wm…`), uses `chatid`
    /// (group broadcast) instead of `touser` (single-user / pipe-separated user list).
    pub async fn send_text(&self, user_id: &str, text: &str) -> Result<(), GatewayError> {
        let token = self.get_access_token().await?;
        let url = format!("{}/message/send?access_token={}", self.api_base, token);

        let agent_id = self.config.agent_id.parse::<i64>().unwrap_or(0);
        let body = if looks_like_wecom_group_chat_id(user_id) {
            serde_json::json!({
                "chatid": user_id,
                "msgtype": "text",
                "agentid": agent_id,
                "text": { "content": text }
            })
        } else {
            serde_json::json!({
                "touser": user_id,
                "msgtype": "text",
                "agentid": agent_id,
                "text": { "content": text }
            })
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("WeCom send failed: {}", e.without_url()))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WeCom API error: {}",
                text
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for WeComAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("WeCom adapter starting (corp_id: {})", self.config.corp_id);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("WeCom adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        debug!("WeCom does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        use crate::platforms::helpers::{media_category, mime_from_extension};

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let token = self.get_access_token().await?;

        let media_type = match media_category(ext) {
            "image" => "image",
            "video" => "video",
            "audio" => "voice",
            _ => "file",
        };

        // Step 1: Upload media to WeCom
        let upload_url = format!(
            "{}/media/upload?access_token={}&type={}",
            self.api_base, token, media_type
        );

        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str(mime)
            .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;
        let form = reqwest::multipart::Form::new().part("media", part);

        let resp = self
            .client
            .post(&upload_url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("WeCom media upload failed: {}", e.without_url()))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WeCom upload error: {text}"
            )));
        }

        let result: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("WeCom upload parse failed: {}", e.without_url()))
        })?;
        let media_id = result
            .get("media_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::SendFailed("No media_id in WeCom response".into()))?;

        // Step 2: Send message with media
        let send_url = format!("{}/message/send?access_token={}", self.api_base, token);
        let agent_id = self.config.agent_id.parse::<i64>().unwrap_or(0);
        let group = looks_like_wecom_group_chat_id(chat_id);

        let body = match media_type {
            "image" => {
                if group {
                    serde_json::json!({
                        "chatid": chat_id,
                        "msgtype": "image",
                        "agentid": agent_id,
                        "image": { "media_id": media_id }
                    })
                } else {
                    serde_json::json!({
                        "touser": chat_id,
                        "msgtype": "image",
                        "agentid": agent_id,
                        "image": { "media_id": media_id }
                    })
                }
            }
            "voice" => {
                if group {
                    serde_json::json!({
                        "chatid": chat_id,
                        "msgtype": "voice",
                        "agentid": agent_id,
                        "voice": { "media_id": media_id }
                    })
                } else {
                    serde_json::json!({
                        "touser": chat_id,
                        "msgtype": "voice",
                        "agentid": agent_id,
                        "voice": { "media_id": media_id }
                    })
                }
            }
            "video" => {
                if group {
                    serde_json::json!({
                        "chatid": chat_id,
                        "msgtype": "video",
                        "agentid": agent_id,
                        "video": { "media_id": media_id, "title": caption.unwrap_or(file_name) }
                    })
                } else {
                    serde_json::json!({
                        "touser": chat_id,
                        "msgtype": "video",
                        "agentid": agent_id,
                        "video": { "media_id": media_id, "title": caption.unwrap_or(file_name) }
                    })
                }
            }
            _ => {
                if group {
                    serde_json::json!({
                        "chatid": chat_id,
                        "msgtype": "file",
                        "agentid": agent_id,
                        "file": { "media_id": media_id }
                    })
                } else {
                    serde_json::json!({
                        "touser": chat_id,
                        "msgtype": "file",
                        "agentid": agent_id,
                        "file": { "media_id": media_id }
                    })
                }
            }
        };

        let resp = self
            .client
            .post(&send_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("WeCom media send failed: {}", e.without_url()))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WeCom media send error: {text}"
            )));
        }
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        if let Some(path) = image_url.strip_prefix("file://") {
            let decoded_path = urlencoding::decode(path)
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| path.to_string());
            return self.send_file(chat_id, &decoded_path, caption).await;
        }

        let downloaded = download_media_url(&self.base, image_url).await;

        let (bytes, content_type) = match downloaded {
            Ok(result) => result,
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "WeCom image-url download failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                return self
                    .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await;
            }
        };

        let file_name = remote_image_file_name(image_url, content_type.as_deref());
        let suffix = std::path::Path::new(&file_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_else(|| ".png".to_string());
        let temp_path = std::env::temp_dir().join(format!(
            "hermes_wecom_img_{}{}",
            uuid::Uuid::new_v4(),
            suffix
        ));
        tokio::fs::write(&temp_path, &bytes).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to write temp image file: {e}"))
        })?;

        let temp_path_str = temp_path.to_string_lossy().to_string();
        let send_result = self.send_file(chat_id, &temp_path_str, caption).await;
        if let Err(err) = tokio::fs::remove_file(&temp_path).await {
            warn!(
                path = %temp_path.display(),
                error = %err,
                "Failed to remove temporary WeCom image file"
            );
        }

        match send_result {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "WeCom image upload failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                self.send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await
            }
        }
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "wecom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, body_string_contains, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture_adapter(server: &MockServer) -> WeComAdapter {
        let mut adapter = WeComAdapter::new(WeComConfig {
            corp_id: "fixture-company".into(),
            agent_id: "42".into(),
            secret: "fixture".into(),
            proxy: AdapterProxyConfig::default(),
        })
        .unwrap();
        adapter.api_base = server.uri();
        adapter.client = Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        adapter
    }

    async fn mount_access_token(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/gettoken"))
            .and(query_param("corpid", "fixture-company"))
            .and(query_param("corpsecret", "fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fixture-session",
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn wecom_reuses_its_token_and_routes_group_and_direct_text() {
        let server = MockServer::start().await;
        mount_access_token(&server).await;
        let adapter = fixture_adapter(&server);
        assert_eq!(adapter.config().agent_id, "42");
        assert_eq!(adapter.platform_name(), "wecom");
        assert!(!adapter.is_running());
        adapter.start().await.unwrap();
        assert!(adapter.is_running());

        for (recipient, id_field) in [("person", "touser"), ("wr-group-fixture", "chatid")] {
            Mock::given(method("POST"))
                .and(path("/message/send"))
                .and(query_param("access_token", "fixture-session"))
                .and(body_json(json!({
                    (id_field): recipient,
                    "msgtype": "text",
                    "agentid": 42,
                    "text": {"content": "Fixture message"}
                })))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
            adapter
                .send_message(recipient, "Fixture message", None)
                .await
                .unwrap();
        }
        adapter.stop().await.unwrap();
        assert!(!adapter.is_running());
        server.verify().await;
    }

    #[tokio::test]
    async fn wecom_uploads_local_media_then_sends_the_returned_id() {
        let server = MockServer::start().await;
        mount_access_token(&server).await;
        let adapter = fixture_adapter(&server);
        let directory = tempfile::tempdir().unwrap();

        for (ext, category) in [
            ("png", "image"),
            ("wav", "voice"),
            ("mp4", "video"),
            ("txt", "file"),
        ] {
            let file_name = format!("fixture.{ext}");
            let file = directory.path().join(&file_name);
            let contents = format!("bytes for {file_name}");
            std::fs::write(&file, &contents).unwrap();
            let media_id = format!("uploaded-{category}");
            Mock::given(method("POST"))
                .and(path("/media/upload"))
                .and(query_param("access_token", "fixture-session"))
                .and(query_param("type", category))
                .and(body_string_contains(format!(
                    "name=\"media\"; filename=\"{file_name}\""
                )))
                .and(body_string_contains(contents))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "media_id": media_id
                })))
                .expect(2)
                .mount(&server)
                .await;

            for (recipient, id_field, caption) in [
                ("person", "touser", None),
                ("wm-group-fixture", "chatid", Some("Group clip")),
            ] {
                let media = if category == "video" {
                    json!({"media_id": media_id, "title": caption.unwrap_or(&file_name)})
                } else {
                    json!({"media_id": media_id})
                };
                Mock::given(method("POST"))
                    .and(path("/message/send"))
                    .and(query_param("access_token", "fixture-session"))
                    .and(body_json(json!({
                        (id_field): recipient,
                        "msgtype": category,
                        "agentid": 42,
                        (category): media
                    })))
                    .respond_with(ResponseTemplate::new(200))
                    .expect(1)
                    .mount(&server)
                    .await;
                adapter
                    .send_file(recipient, file.to_str().unwrap(), caption)
                    .await
                    .unwrap();
            }
        }
        server.verify().await;
    }

    #[tokio::test]
    async fn wecom_surfaces_auth_and_failed_upload_without_sending_a_message() {
        let server = MockServer::start().await;
        let adapter = fixture_adapter(&server);
        Mock::given(method("POST"))
            .and(path("/message/send"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let token_mock = Mock::given(method("GET"))
            .and(path("/gettoken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"errcode": 1})))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        assert!(matches!(
            adapter.send_text("person", "Fixture message").await,
            Err(GatewayError::Auth(message)) if message.contains("No access_token")
        ));
        drop(token_mock);

        mount_access_token(&server).await;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("fixture.txt");
        std::fs::write(&file, b"fixture file").unwrap();
        for (response, expected) in [
            (
                ResponseTemplate::new(503).set_body_string("fixture upload unavailable"),
                "fixture upload unavailable",
            ),
            (
                ResponseTemplate::new(200).set_body_json(json!({})),
                "No media_id in WeCom response",
            ),
        ] {
            let _upload = Mock::given(method("POST"))
                .and(path("/media/upload"))
                .respond_with(response)
                .expect(1)
                .mount_as_scoped(&server)
                .await;
            assert!(matches!(
                adapter.send_file("person", file.to_str().unwrap(), None).await,
                Err(GatewayError::SendFailed(message)) if message.contains(expected)
            ));
        }
        server.verify().await;
    }

    #[test]
    fn remote_image_file_name_keeps_extension() {
        let file_name = remote_image_file_name(
            "https://cdn.example.com/path/diagram.png?token=abc",
            Some("image/png"),
        );
        assert_eq!(file_name, "diagram.png");
    }

    #[test]
    fn remote_image_file_name_adds_extension_from_content_type() {
        let file_name =
            remote_image_file_name("https://cdn.example.com/path/diagram", Some("image/jpeg"));
        assert_eq!(file_name, "diagram.jpg");
    }

    #[test]
    fn image_fallback_text_with_caption() {
        let text = image_fallback_text("https://cdn.example.com/path/diagram", Some("Figure 1"));
        assert_eq!(text, "Figure 1\nhttps://cdn.example.com/path/diagram");
    }
}
