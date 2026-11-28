//! Real Home Assistant backend: REST API calls.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::homeassistant::HomeAssistantBackend;
use hermes_core::ToolError;

const BLOCKED_SERVICE_DOMAINS: &[&str] = &["shell_command", "python_script", "hassio"];

/// Home Assistant backend using the REST API.
pub struct HaRestBackend {
    client: Client,
    base_url: String,
    token: String,
}

impl HaRestBackend {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let base_url = std::env::var("HASS_URL").map_err(|_| {
            ToolError::ExecutionFailed("HASS_URL environment variable not set".into())
        })?;
        let token = std::env::var("HASS_TOKEN").map_err(|_| {
            ToolError::ExecutionFailed("HASS_TOKEN environment variable not set".into())
        })?;
        Ok(Self::new(base_url, token))
    }

    async fn get(&self, path: &str) -> Result<Value, ToolError> {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("HA API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read HA response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "HA API error ({}): {}",
                status, text
            )));
        }

        serde_json::from_str(&text)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse HA response: {}", e)))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, ToolError> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.token))
            .json(body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("HA API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read HA response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "HA API error ({}): {}",
                status, text
            )));
        }

        serde_json::from_str(&text)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse HA response: {}", e)))
    }
}

fn is_valid_ha_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn parse_entity_id(entity_id: &str) -> Result<(&str, &str), ToolError> {
    let trimmed = entity_id.trim();
    let (domain, object_id) = trimmed.split_once('.').ok_or_else(|| {
        ToolError::InvalidParams(format!(
            "Invalid entity_id '{}': expected format 'domain.object_id'",
            entity_id
        ))
    })?;
    if !is_valid_ha_component(domain) || !is_valid_ha_component(object_id) {
        return Err(ToolError::InvalidParams(format!(
            "Invalid entity_id '{}': only lowercase letters, digits, and '_' are allowed",
            entity_id
        )));
    }
    Ok((domain, object_id))
}

fn parse_service(domain_hint: &str, service: &str) -> Result<(String, String), ToolError> {
    let trimmed = service.trim();
    let (domain, action) = if let Some((d, s)) = trimmed.split_once('.') {
        (d, s)
    } else {
        (domain_hint, trimmed)
    };
    if !is_valid_ha_component(domain) || !is_valid_ha_component(action) {
        return Err(ToolError::InvalidParams(format!(
            "Invalid service '{}': use 'domain.service' with lowercase alnum/underscore",
            service
        )));
    }
    Ok((domain.to_string(), action.to_string()))
}

fn validate_service_domain(domain: &str) -> Result<(), ToolError> {
    if BLOCKED_SERVICE_DOMAINS.contains(&domain) {
        return Err(ToolError::InvalidParams(format!(
            "Service domain '{}' is blocked for safety",
            domain
        )));
    }
    Ok(())
}

#[async_trait]
impl HomeAssistantBackend for HaRestBackend {
    async fn list_entities(&self, domain: Option<&str>) -> Result<String, ToolError> {
        if let Some(d) = domain {
            if !is_valid_ha_component(d) {
                return Err(ToolError::InvalidParams(format!(
                    "Invalid domain '{}': use lowercase alnum/underscore",
                    d
                )));
            }
        }
        let states: Value = self.get("/api/states").await?;
        let empty = vec![];
        let entities = states.as_array().unwrap_or(&empty);

        let filtered: Vec<Value> = entities
            .iter()
            .filter(|e| {
                if let Some(d) = domain {
                    e.get("entity_id")
                        .and_then(|id| id.as_str())
                        .map(|id| id.starts_with(&format!("{}.", d)))
                        .unwrap_or(false)
                } else {
                    true
                }
            })
            .map(|e| {
                json!({
                    "entity_id": e.get("entity_id").and_then(|v| v.as_str()).unwrap_or(""),
                    "state": e.get("state").and_then(|v| v.as_str()).unwrap_or(""),
                    "friendly_name": e.get("attributes")
                        .and_then(|a| a.get("friendly_name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or(""),
                })
            })
            .collect();

        Ok(json!({"entities": filtered, "total": filtered.len()}).to_string())
    }

    async fn get_state(&self, entity_id: &str) -> Result<String, ToolError> {
        let _ = parse_entity_id(entity_id)?;
        let state: Value = self.get(&format!("/api/states/{}", entity_id)).await?;
        Ok(serde_json::to_string_pretty(&state)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize state: {}", e)))?)
    }

    async fn list_services(&self, domain: Option<&str>) -> Result<String, ToolError> {
        if let Some(d) = domain {
            if !is_valid_ha_component(d) {
                return Err(ToolError::InvalidParams(format!(
                    "Invalid domain '{}': use lowercase alnum/underscore",
                    d
                )));
            }
            validate_service_domain(d)?;
        }
        let services: Value = self.get("/api/services").await?;
        let empty = vec![];
        let all = services.as_array().unwrap_or(&empty);

        let filtered: Vec<&Value> = match domain {
            Some(d) => all
                .iter()
                .filter(|s| s.get("domain").and_then(|v| v.as_str()) == Some(d))
                .collect(),
            None => all.iter().collect(),
        };

        Ok(json!({"services": filtered, "total": filtered.len()}).to_string())
    }

    async fn call_service(
        &self,
        service: &str,
        entity_id: &str,
        data: Option<&Value>,
    ) -> Result<String, ToolError> {
        let (entity_domain, _) = parse_entity_id(entity_id)?;
        // Parse service as "domain.service" or just "service" with domain from entity_id.
        let (domain, svc) = parse_service(entity_domain, service)?;
        validate_service_domain(&domain)?;

        let mut body = data.cloned().unwrap_or(json!({}));
        if let Some(obj) = body.as_object_mut() {
            obj.insert("entity_id".to_string(), json!(entity_id));
        }

        let result = self
            .post(&format!("/api/services/{}/{}", domain, svc), &body)
            .await?;
        Ok(json!({"status": "ok", "result": result}).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn loopback_backend(server: &MockServer) -> HaRestBackend {
        let mut backend = HaRestBackend::new(format!("{}/", server.uri()), "ha-fixture".into());
        backend.client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        backend
    }

    #[tokio::test]
    async fn loopback_discovery_filters_states_and_services_without_changing_them() {
        let server = MockServer::start().await;
        let backend = loopback_backend(&server);
        let temperature = json!({
            "entity_id": "sensor.temperature",
            "state": "21",
            "attributes": {"friendly_name": "Temperature", "unit_of_measurement": "°C"},
        });
        Mock::given(method("GET"))
            .and(path("/api/states"))
            .and(header("authorization", "Bearer ha-fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                temperature.clone(),
                {"entity_id": "light.fixture", "state": "off"},
            ])))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/states/sensor.temperature"))
            .and(header("authorization", "Bearer ha-fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&temperature))
            .expect(1)
            .mount(&server)
            .await;
        let sensor_services = json!({"domain": "sensor", "services": {"refresh": {}}});
        Mock::given(method("GET"))
            .and(path("/api/services"))
            .and(header("authorization", "Bearer ha-fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                sensor_services.clone(),
                {"domain": "light", "services": {"inspect": {}}},
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let all: Value = serde_json::from_str(&backend.list_entities(None).await.unwrap()).unwrap();
        assert_eq!(all["total"], 2);
        assert_eq!(all["entities"][1]["friendly_name"], "");
        let sensors: Value =
            serde_json::from_str(&backend.list_entities(Some("sensor")).await.unwrap()).unwrap();
        assert_eq!(
            sensors,
            json!({
                "entities": [{"entity_id": "sensor.temperature", "state": "21", "friendly_name": "Temperature"}],
                "total": 1,
            })
        );
        let detail = backend.get_state("sensor.temperature").await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&detail).unwrap(), temperature);
        let services = backend.list_services(Some("sensor")).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&services).unwrap(),
            json!({"services": [sensor_services], "total": 1})
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests
            .iter()
            .all(|request| request.method.as_str() == "GET"));
    }

    #[tokio::test]
    async fn loopback_service_preserves_parameters_and_reports_response_errors() {
        let server = MockServer::start().await;
        let backend = loopback_backend(&server);
        let route = "/api/services/light/inspect";
        let result = json!([{"entity_id": "light.fixture", "state": "off"}]);
        Mock::given(method("POST"))
            .and(path(route))
            .and(header("authorization", "Bearer ha-fixture"))
            .and(body_json(
                json!({"entity_id": "light.fixture", "detail": true}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(&result))
            .expect(1)
            .mount(&server)
            .await;
        let data = json!({"detail": true});
        let response = backend
            .call_service("inspect", "light.fixture", Some(&data))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap(),
            json!({"status": "ok", "result": result})
        );
        server.verify().await;
        server.reset().await;

        for (template, expected) in [
            (
                ResponseTemplate::new(409).set_body_string("fixture is busy"),
                "HA API error (409 Conflict): fixture is busy",
            ),
            (
                ResponseTemplate::new(200).set_body_string("not a JSON reply"),
                "Failed to parse HA response:",
            ),
        ] {
            Mock::given(method("POST"))
                .and(path(route))
                .and(body_json(json!({"entity_id": "light.fixture"})))
                .respond_with(template)
                .expect(1)
                .mount(&server)
                .await;
            let error = backend
                .call_service("light.inspect", "light.fixture", None)
                .await
                .unwrap_err();
            assert!(
                matches!(&error, ToolError::ExecutionFailed(detail) if detail.starts_with(expected)),
                "{error}"
            );
            server.verify().await;
            server.reset().await;
        }
    }

    #[test]
    fn parse_entity_id_accepts_valid_ids() {
        let (domain, object_id) = parse_entity_id("light.living_room").unwrap();
        assert_eq!(domain, "light");
        assert_eq!(object_id, "living_room");
    }

    #[test]
    fn parse_entity_id_rejects_invalid_ids() {
        assert!(parse_entity_id("light").is_err());
        assert!(parse_entity_id("Light.LivingRoom").is_err());
        assert!(parse_entity_id("../etc/passwd").is_err());
    }

    #[test]
    fn parse_service_uses_entity_domain_when_missing_prefix() {
        let (domain, action) = parse_service("light", "turn_on").unwrap();
        assert_eq!(domain, "light");
        assert_eq!(action, "turn_on");
    }

    #[test]
    fn parse_service_rejects_invalid_names() {
        assert!(parse_service("light", "light.turn-on").is_err());
        assert!(parse_service("light", "shell command.run").is_err());
    }

    #[test]
    fn validate_service_domain_blocks_sensitive_domains() {
        assert!(validate_service_domain("shell_command").is_err());
        assert!(validate_service_domain("python_script").is_err());
        assert!(validate_service_domain("light").is_ok());
    }
}
