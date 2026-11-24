//! Keep-alive requests that hold an idle session's cached prompt prefix.
//!
//! Off unless `prompt_caching.keep_alive.max_pings` is above 0: each
//! keep-alive is a billed request. When a run ends, one task per session
//! waits until 5/6 of the cache lifetime has passed since the last use, then
//! re-sends the session's last request with a 1-token output cap. Reading
//! the prefix renews its lifetime, so the next real turn reads it instead of
//! writing it again. The task is process-wide, keyed by session id, because
//! the gateway drops its agent after every message.
//!
//! Warming stops at the first of: `max_pings` sent; the next ping would land
//! more than `max_idle_minutes` after the run ended; a ping that came back
//! missed (nothing read, or more written than read); a ping whose worst-case
//! cost (the whole prefix written again) would take this idle stretch's
//! keep-alive spend, each ping counted at its worst case, past
//! `max_cost_usd`; an unknown price; a failed request.
//! A new run of the session cancels it first, so no ping runs beside a turn.
//! Nothing is armed for a request with a thinking budget (a 1-token cap would
//! change it), for a model without explicit cache markers, or for a cached
//! prefix under [`MIN_CACHED_PREFIX_TOKENS`].

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use hermes_core::{LlmProvider, Message, ToolSchema, UsageStats};
use serde_json::Value;

/// Smallest cached prefix worth a keep-alive. Below the provider's minimum
/// cacheable size nothing is cached at all.
pub const MIN_CACHED_PREFIX_TOKENS: u64 = 1_024;

/// Estimated cost in USD of a request with this usage on this model.
pub type CostEstimate = Arc<dyn Fn(&UsageStats, &str) -> Option<f64> + Send + Sync>;

/// A session's last successful request, as sent.
#[derive(Clone)]
pub struct CachedRequest {
    pub provider: Arc<dyn LlmProvider>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub model: String,
    pub extra_body: Option<Value>,
    pub usage: UsageStats,
}

/// Limits for one idle stretch.
#[derive(Clone)]
pub struct KeepAlivePlan {
    pub ttl: Duration,
    pub max_pings: u32,
    pub max_idle: Duration,
    pub max_cost_usd: Option<f64>,
    pub cost: CostEstimate,
    /// Multiplies the worst case: price tables carry the 5-minute write
    /// price, and a 1-hour write costs 2/1.25 of it.
    pub write_price_factor: f64,
}

/// Why warming stopped, for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepAliveStop {
    PingsSpent,
    IdleCap,
    Missed,
    CostCap,
    UnknownPrice,
    RequestFailed,
}

static LAST_REQUEST: LazyLock<Mutex<HashMap<String, CachedRequest>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ARMED: LazyLock<Mutex<HashMap<String, tokio::task::AbortHandle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sessions whose last request is kept; the oldest entry goes beyond this.
const SESSIONS_MAX: usize = 64;

/// Keep `request` as `session_id`'s last successful request.
pub fn remember(session_id: &str, request: CachedRequest) {
    if session_id.is_empty() {
        return;
    }
    let Ok(mut last) = LAST_REQUEST.lock() else {
        return;
    };
    if last.len() >= SESSIONS_MAX && !last.contains_key(session_id) {
        if let Some(any) = last.keys().next().cloned() {
            last.remove(&any);
        }
    }
    last.insert(session_id.to_string(), request);
}

/// Stop any keep-alive armed for `session_id` (a run is starting).
pub fn cancel(session_id: &str) {
    if let Some(handle) = ARMED.lock().ok().and_then(|mut armed| armed.remove(session_id)) {
        handle.abort();
    }
}

/// Forget `session_id`: no keep-alive, no kept request (session deleted).
pub fn forget(session_id: &str) {
    cancel(session_id);
    if let Ok(mut last) = LAST_REQUEST.lock() {
        last.remove(session_id);
    }
}

/// True when `request` may be warmed at all.
fn warmable(request: &CachedRequest) -> bool {
    let thinking = request
        .extra_body
        .as_ref()
        .is_some_and(|extra| extra.get("thinking").is_some());
    let prefix = request
        .usage
        .cache_read_tokens
        .saturating_add(request.usage.cache_write_tokens);
    !thinking
        && request.model.to_ascii_lowercase().contains("claude")
        && prefix >= MIN_CACHED_PREFIX_TOKENS
}

/// Arm warming for `session_id` from now, the end of a run. Needs a Tokio
/// runtime; does nothing when `plan.max_pings` is 0 or the session's last
/// request cannot be warmed.
pub fn arm(session_id: &str, plan: KeepAlivePlan) {
    cancel(session_id);
    if plan.max_pings == 0 || session_id.is_empty() {
        return;
    }
    let Some(request) = LAST_REQUEST
        .lock()
        .ok()
        .and_then(|last| last.get(session_id).cloned())
    else {
        return;
    };
    if !warmable(&request) {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let session = session_id.to_string();
    let task = runtime.spawn(async move {
        let stop = warm(&session, request, &plan).await;
        tracing::info!(session = %session, ?stop, "prompt cache keep-alive stopped");
        if let Ok(mut armed) = ARMED.lock() {
            armed.remove(&session);
        }
        stop
    });
    if let Ok(mut armed) = ARMED.lock() {
        armed.insert(session_id.to_string(), task.abort_handle());
    }
}

/// The keep-alive loop for one idle stretch.
pub(crate) async fn warm(
    session_id: &str,
    request: CachedRequest,
    plan: &KeepAlivePlan,
) -> KeepAliveStop {
    let idle_from = tokio::time::Instant::now();
    let mut last_use = idle_from;
    let interval = plan.ttl / 6 * 5;
    let prefix = request
        .usage
        .cache_read_tokens
        .saturating_add(request.usage.cache_write_tokens);
    let worst_case = UsageStats {
        prompt_tokens: prefix,
        completion_tokens: 1,
        total_tokens: prefix + 1,
        estimated_cost: None,
        cache_read_tokens: 0,
        cache_write_tokens: prefix,
    };
    let mut spent = 0.0_f64;
    for ping in 1..=plan.max_pings {
        let at = last_use + interval;
        if at.duration_since(idle_from) > plan.max_idle {
            return KeepAliveStop::IdleCap;
        }
        let Some(worst) = (plan.cost)(&worst_case, &request.model) else {
            return KeepAliveStop::UnknownPrice;
        };
        let worst = worst * plan.write_price_factor;
        if plan.max_cost_usd.is_some_and(|cap| spent + worst > cap) {
            return KeepAliveStop::CostCap;
        }
        tokio::time::sleep_until(at).await;
        let response = request
            .provider
            .chat_completion(
                &request.messages,
                &request.tools,
                Some(1),
                None,
                Some(&request.model),
                request.extra_body.as_ref(),
            )
            .await;
        let Some(usage) = response.ok().and_then(|r| r.usage) else {
            return KeepAliveStop::RequestFailed;
        };
        // Each ping counts at its worst case, so the cap holds whatever the
        // estimate of its actual usage says.
        spent += worst;
        let cost = (plan.cost)(&usage, &request.model).unwrap_or(worst);
        tracing::info!(
            session = %session_id,
            ping,
            cache_read_tokens = usage.cache_read_tokens,
            cache_write_tokens = usage.cache_write_tokens,
            cost_usd = cost,
            "prompt cache keep-alive sent"
        );
        if usage.cache_read_tokens == 0 || usage.cache_write_tokens > usage.cache_read_tokens {
            return KeepAliveStop::Missed;
        }
        super::tool_result_prune::record_cache_touch(session_id);
        last_use = tokio::time::Instant::now();
    }
    KeepAliveStop::PingsSpent
}

#[cfg(test)]
mod tests;
