// ---------------------------------------------------------------------------
// PromptCachingConfig (`prompt_caching:` in config.yaml)
// ---------------------------------------------------------------------------

/// How long the provider keeps a cached prompt prefix after its last use.
/// Applies to Claude models, whose requests carry explicit cache markers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptCacheTtl {
    /// The provider's default lifetime; requests carry no `ttl`.
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    /// The 1-hour tier: cache writes cost 2x base input instead of 1.25x.
    #[serde(rename = "1h")]
    OneHour,
}

impl PromptCacheTtl {
    /// The lifetime in seconds.
    pub fn seconds(self) -> u64 {
        match self {
            Self::FiveMinutes => 300,
            Self::OneHour => 3_600,
        }
    }
}

/// Largest `keep_alive.max_pings` honoured.
pub const KEEP_ALIVE_MAX_PINGS_LIMIT: u32 = 12;
/// Largest `keep_alive.max_idle_minutes` honoured.
pub const KEEP_ALIVE_MAX_IDLE_MINUTES_LIMIT: u32 = 240;

/// Keep-alive requests that hold an idle session's cached prefix. Each one
/// is a billed request: off unless `max_pings` is above 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCacheKeepAliveConfig {
    /// Keep-alive requests per idle stretch; 0 turns keep-alive off.
    #[serde(default)]
    pub max_pings: u32,
    /// No keep-alive is sent once the session has been idle this long.
    #[serde(default = "default_keep_alive_max_idle_minutes")]
    pub max_idle_minutes: u32,
}

fn default_keep_alive_max_idle_minutes() -> u32 {
    60
}

impl Default for PromptCacheKeepAliveConfig {
    fn default() -> Self {
        Self {
            max_pings: 0,
            max_idle_minutes: default_keep_alive_max_idle_minutes(),
        }
    }
}

impl PromptCacheKeepAliveConfig {
    /// `max_pings`, capped at [`KEEP_ALIVE_MAX_PINGS_LIMIT`].
    pub fn pings(&self) -> u32 {
        self.max_pings.min(KEEP_ALIVE_MAX_PINGS_LIMIT)
    }

    /// `max_idle_minutes` in seconds, capped at [`KEEP_ALIVE_MAX_IDLE_MINUTES_LIMIT`].
    pub fn max_idle_seconds(&self) -> u64 {
        u64::from(self.max_idle_minutes.min(KEEP_ALIVE_MAX_IDLE_MINUTES_LIMIT)) * 60
    }
}

/// The `prompt_caching:` section.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCachingConfig {
    #[serde(default)]
    pub cache_ttl: PromptCacheTtl,
    #[serde(default)]
    pub keep_alive: PromptCacheKeepAliveConfig,
    /// At the first run after the cache expired, compact a history that is at
    /// least half the compression threshold before the first request (the
    /// cold prefix is rewritten anyway). Compaction is lossy, so off by default.
    #[serde(default)]
    pub compact_when_cold: bool,
}

impl PromptCachingConfig {
    /// True when every setting is its default; a saved config then omits it.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
