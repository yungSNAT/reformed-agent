use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::{redirect::Policy, Client, Url};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::provider_profiles::clamp_effort;

const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_EFFORT_LEVELS: usize = 32;
const CATALOG_TIMEOUT: Duration = Duration::from_secs(2);
const CATALOG_TTL: Duration = Duration::from_secs(300);
const UNAVAILABLE_TTL: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub(super) struct CopilotCatalog {
    url: Url,
    client: Client,
    cache: Mutex<Option<CachedCatalog>>,
}

#[derive(Debug)]
struct CachedCatalog {
    expires: Instant,
    models: Option<Vec<Value>>,
}

pub(super) enum EffortSupport {
    Unknown,
    Unsupported,
    Supported(Vec<String>),
}

impl CopilotCatalog {
    pub(super) fn for_base_url(base_url: &str) -> Option<Arc<Self>> {
        Self::new(catalog_url(base_url)?)
    }

    pub(super) fn new(url: Url) -> Option<Arc<Self>> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(CATALOG_TIMEOUT)
            .build()
            .ok()?;
        Some(Arc::new(Self {
            url,
            client,
            cache: Mutex::new(None),
        }))
    }

    pub(super) async fn effort_support(&self, api_key: &str, model: &str) -> EffortSupport {
        // Clones share this catalogue. The owning provider has one immutable credential.
        let mut cache = self.cache.lock().await;
        if let Some(entry) = cache
            .as_ref()
            .filter(|entry| entry.expires > Instant::now())
        {
            return effort_support(entry.models.as_deref(), model);
        }

        let models = self.fetch(api_key).await;
        let result = effort_support(models.as_deref(), model);
        let ttl = if models.is_some() {
            CATALOG_TTL
        } else {
            UNAVAILABLE_TTL
        };
        *cache = Some(CachedCatalog {
            expires: Instant::now() + ttl,
            models,
        });
        result
    }

    async fn fetch(&self, api_key: &str) -> Option<Vec<Value>> {
        let mut response = self
            .client
            .get(self.url.clone())
            .bearer_auth(api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::USER_AGENT,
                concat!("HermesAgent/", env!("CARGO_PKG_VERSION")),
            )
            .header("X-GitHub-Api-Version", "2026-08-01")
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if chunk.len() > MAX_CATALOG_BYTES - body.len() {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        let payload: Value = serde_json::from_slice(&body).ok()?;
        match payload {
            Value::Array(models) => Some(models),
            Value::Object(mut fields) => match fields.remove("data")? {
                Value::Array(models) => Some(models),
                _ => None,
            },
            _ => None,
        }
    }
}

fn catalog_url(base_url: &str) -> Option<Url> {
    let mut url = Url::parse(base_url).ok()?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some(
                "api.githubcopilot.com"
                    | "api.individual.githubcopilot.com"
                    | "api.business.githubcopilot.com"
                    | "api.enterprise.githubcopilot.com"
            )
        )
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "/" | "/v1" | "/v1/")
    {
        return None;
    }
    url.set_path("/models");
    Some(url)
}

fn effort_support(models: Option<&[Value]>, model: &str) -> EffortSupport {
    let Some(models) = models else {
        return EffortSupport::Unknown;
    };
    let mut matches = models
        .iter()
        .filter(|entry| entry.get("id").and_then(Value::as_str) == Some(model));
    let Some(entry) = matches.next() else {
        return EffortSupport::Unknown;
    };
    if matches.next().is_some() {
        return EffortSupport::Unknown;
    }
    match entry.get("capabilities") {
        Some(Value::Object(fields)) => match fields.get("supports") {
            None => EffortSupport::Unsupported,
            Some(Value::Object(supports)) => advertised_levels(supports.get("reasoning_effort")),
            _ => EffortSupport::Unknown,
        },
        Some(Value::Array(flags)) if flags.iter().all(Value::is_string) => {
            if flags.iter().any(|flag| {
                flag.as_str()
                    .is_some_and(|flag| flag.trim().eq_ignore_ascii_case("reasoning"))
            }) {
                legacy_support(model)
            } else {
                EffortSupport::Unsupported
            }
        }
        _ => EffortSupport::Unknown,
    }
}

fn advertised_levels(field: Option<&Value>) -> EffortSupport {
    let Some(field) = field else {
        return EffortSupport::Unsupported;
    };
    let Some(values) = field
        .as_array()
        .filter(|values| values.len() <= MAX_EFFORT_LEVELS)
    else {
        return EffortSupport::Unknown;
    };
    let mut levels = Vec::new();
    for value in values {
        let Some(level) = value.as_str().map(str::trim).filter(|level| {
            !level.is_empty()
                && level.len() <= 32
                && level
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }) else {
            return EffortSupport::Unknown;
        };
        let level = level.to_ascii_lowercase();
        if !levels.contains(&level) {
            levels.push(level);
        }
    }
    if levels.is_empty() {
        EffortSupport::Unsupported
    } else {
        EffortSupport::Supported(levels)
    }
}

fn legacy_support(model: &str) -> EffortSupport {
    let lower = model.to_ascii_lowercase();
    let name = lower.strip_prefix("openai/").unwrap_or(&lower);
    let levels: &[&str] = if ["o1", "o3", "o4"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        &["low", "medium", "high"]
    } else if matches!(name, "gpt-6-astra" | "gpt-6-astra-900k") {
        &["low", "medium", "high", "xhigh", "max"]
    } else if name.starts_with("gpt-5") {
        &["minimal", "low", "medium", "high"]
    } else {
        return EffortSupport::Unknown;
    };
    EffortSupport::Supported(levels.iter().map(|level| (*level).to_string()).collect())
}

pub(super) fn reconcile_effort(effort: &str, levels: &[String]) -> Option<String> {
    let levels: Vec<&str> = levels.iter().map(String::as_str).collect();
    let first = levels.first()?;
    let adjusted = clamp_effort(effort, &levels);
    if levels.contains(&adjusted.as_str()) {
        Some(adjusted)
    } else if levels.contains(&"medium") {
        Some("medium".to_string())
    } else {
        Some((*first).to_string())
    }
}
