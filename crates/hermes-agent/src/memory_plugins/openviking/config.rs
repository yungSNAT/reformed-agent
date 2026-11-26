#[derive(Debug, Clone)]
struct OpenVikingConfig {
    endpoint: String,
    api_key: String,
    api_key_type: String,
    account: String,
    user: String,
    agent: String,
    recall: OpenVikingRecallConfig,
}

#[derive(Debug, Clone)]
struct OpenVikingRecallConfig {
    limit: usize,
    score_threshold: f64,
    max_injected_chars: usize,
    timeout: Duration,
    request_timeout: Duration,
    full_read_limit: usize,
    prefer_abstract: bool,
    include_resources: bool,
}

impl Default for OpenVikingRecallConfig {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RECALL_LIMIT,
            score_threshold: DEFAULT_RECALL_SCORE_THRESHOLD,
            max_injected_chars: DEFAULT_RECALL_MAX_INJECTED_CHARS,
            timeout: DEFAULT_RECALL_TIMEOUT,
            request_timeout: DEFAULT_RECALL_REQUEST_TIMEOUT,
            full_read_limit: DEFAULT_RECALL_FULL_READ_LIMIT,
            prefer_abstract: false,
            include_resources: false,
        }
    }
}

impl OpenVikingConfig {
    fn config_path(hermes_home: &str) -> PathBuf {
        Path::new(hermes_home).join("openviking.json")
    }

    fn default_config_path() -> PathBuf {
        config_io::default_hermes_home().join("openviking.json")
    }

    fn configured_at(path: &Path) -> bool {
        let object = config_io::read_json_object(path);
        if object
            .get("enabled")
            .and_then(Value::as_bool)
            .is_some_and(|enabled| enabled)
        {
            return true;
        }
        ["endpoint", "api_key", "root_api_key"].iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
    }

    fn load(hermes_home: &str) -> Self {
        let raw = config_io::read_json_object(&Self::config_path(hermes_home));
        let mut config = Self {
            endpoint: std::env::var("OPENVIKING_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string()),
            api_key: std::env::var("OPENVIKING_API_KEY").unwrap_or_default(),
            api_key_type: std::env::var("OPENVIKING_API_KEY_TYPE")
                .unwrap_or_else(|_| "user".to_string()),
            account: std::env::var("OPENVIKING_ACCOUNT").unwrap_or_else(|_| "default".into()),
            user: std::env::var("OPENVIKING_USER").unwrap_or_else(|_| "default".into()),
            agent: std::env::var("OPENVIKING_AGENT").unwrap_or_else(|_| DEFAULT_AGENT.into()),
            recall: OpenVikingRecallConfig::resolve(&raw),
        };
        apply_openviking_config_map(&mut config, &raw);

        config.endpoint = normalize_openviking_endpoint(&config.endpoint);
        config.api_key_type = normalize_openviking_key_type(&config.api_key_type);
        config.account = nonempty_or(&config.account, "default");
        config.user = nonempty_or(&config.user, "default");
        config.agent = nonempty_or(&config.agent, DEFAULT_AGENT);
        config
    }
}

impl OpenVikingRecallConfig {
    /// As the Python plugin's `_setting` (OpenViking `e855796c`, `examples/hermes-plugin`):
    /// per setting, a non-blank environment variable beats the config file; a value that
    /// does not parse gives the built-in default, not the file value; then the schema
    /// bounds apply.
    fn resolve(raw: &serde_json::Map<String, Value>) -> Self {
        let setting = |key: &str, env_var: &str| match std::env::var(env_var) {
            Ok(value) if !value.trim().is_empty() => Some(Value::String(value)),
            _ => raw.get(key).cloned(),
        };
        let number = |key: &str, env_var: &str, default: f64, min: f64, max: f64| {
            setting(key, env_var)
                .as_ref()
                .and_then(setting_number)
                .map_or(default, |value| value.clamp(min, max))
        };
        let count = |key: &str, env_var: &str, default: usize, min: usize, max: usize| {
            setting(key, env_var)
                .as_ref()
                .and_then(setting_number)
                .filter(|value| value.fract() == 0.0)
                .map_or(default, |value| {
                    value.clamp(min as f64, max as f64) as usize
                })
        };
        let seconds = |key: &str, env_var: &str, default: Duration| {
            Duration::from_secs_f64(number(
                key,
                env_var,
                default.as_secs_f64(),
                RECALL_TIMEOUT_FLOOR.as_secs_f64(),
                RECALL_TIMEOUT_CAP.as_secs_f64(),
            ))
        };
        let flag = |key: &str, env_var: &str| {
            setting(key, env_var)
                .as_ref()
                .and_then(json_boolish)
                .unwrap_or(false)
        };
        Self {
            limit: count(
                "recall_limit",
                "OPENVIKING_RECALL_LIMIT",
                DEFAULT_RECALL_LIMIT,
                1,
                100,
            ),
            score_threshold: number(
                "recall_score_threshold",
                "OPENVIKING_RECALL_SCORE_THRESHOLD",
                DEFAULT_RECALL_SCORE_THRESHOLD,
                0.0,
                1.0,
            ),
            max_injected_chars: count(
                "recall_max_injected_chars",
                "OPENVIKING_RECALL_MAX_INJECTED_CHARS",
                DEFAULT_RECALL_MAX_INJECTED_CHARS,
                100,
                50_000,
            ),
            timeout: seconds(
                "recall_timeout_seconds",
                "OPENVIKING_RECALL_TIMEOUT_SECONDS",
                DEFAULT_RECALL_TIMEOUT,
            ),
            request_timeout: seconds(
                "recall_request_timeout_seconds",
                "OPENVIKING_RECALL_REQUEST_TIMEOUT_SECONDS",
                DEFAULT_RECALL_REQUEST_TIMEOUT,
            ),
            full_read_limit: count(
                "recall_full_read_limit",
                "OPENVIKING_RECALL_FULL_READ_LIMIT",
                DEFAULT_RECALL_FULL_READ_LIMIT,
                0,
                100,
            ),
            prefer_abstract: flag(
                "recall_prefer_abstract",
                "OPENVIKING_RECALL_PREFER_ABSTRACT",
            ),
            include_resources: flag("recall_resources", "OPENVIKING_RECALL_RESOURCES"),
        }
    }
}

fn apply_openviking_config_map(
    config: &mut OpenVikingConfig,
    raw: &serde_json::Map<String, Value>,
) {
    if let Some(endpoint) = raw
        .get("endpoint")
        .or(raw.get("base_url"))
        .or(raw.get("baseUrl"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        config.endpoint = endpoint.to_string();
    }
    if let Some(api_key) = raw
        .get("api_key")
        .or(raw.get("apiKey"))
        .or(raw.get("root_api_key"))
        .or(raw.get("rootApiKey"))
        .and_then(Value::as_str)
    {
        config.api_key = api_key.to_string();
    }
    if let Some(key_type) = raw
        .get("api_key_type")
        .or(raw.get("apiKeyType"))
        .and_then(Value::as_str)
    {
        config.api_key_type = key_type.to_string();
    }
    if let Some(account) = raw.get("account").and_then(Value::as_str) {
        config.account = account.to_string();
    }
    if let Some(user) = raw.get("user").and_then(Value::as_str) {
        config.user = user.to_string();
    }
    if let Some(agent) = raw.get("agent").and_then(Value::as_str) {
        config.agent = agent.to_string();
    }
}

/// As `_parse_setting_value` for numbers: a JSON number or numeric string that is finite;
/// a bool is not a number.
fn setting_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

fn json_boolish(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        value
            .as_str()
            .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            })
    })
}

fn normalize_openviking_endpoint(raw: &str) -> String {
    let value = raw.trim();
    let with_scheme = if value.is_empty() {
        DEFAULT_ENDPOINT.to_string()
    } else if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

fn normalize_openviking_key_type(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "root" | "root_api_key" | "root-api-key" => "root".to_string(),
        "none" | "dev" | "local" | "no_api_key" | "no-api-key" => "none".to_string(),
        _ => "user".to_string(),
    }
}

fn nonempty_or(raw: &str, default: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}
