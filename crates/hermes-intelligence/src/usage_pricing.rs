//! Usage pricing — per-model pricing data, cost calculation, and formatting.
//!
//! Ported from `agent/usage_pricing.py`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Canonical usage breakdown from a single API call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub request_count: u64,
}

impl CanonicalUsage {
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens().saturating_add(self.output_tokens)
    }
}

/// Billing routing information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingRoute {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub billing_mode: BillingMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingMode {
    OfficialDocsSnapshot,
    ProviderModelsApi,
    SubscriptionIncluded,
    Unknown,
}

/// Per-model pricing entry (costs per 1M tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingEntry {
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
    pub cache_read_cost_per_million: Option<f64>,
    pub cache_write_cost_per_million: Option<f64>,
    pub request_cost: Option<f64>,
    pub source: CostSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostSource {
    OfficialDocsSnapshot,
    ProviderModelsApi,
    UserOverride,
    None,
}

/// Result of a cost estimation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostResult {
    pub amount_usd: Option<f64>,
    pub status: CostStatus,
    pub source: CostSource,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostStatus {
    Actual,
    Estimated,
    Included,
    Unknown,
}

// ---------------------------------------------------------------------------
// Official docs pricing database
// ---------------------------------------------------------------------------

/// Build the official pricing database.
fn official_pricing_db() -> HashMap<(&'static str, &'static str), PricingEntry> {
    let mut db = HashMap::new();

    let add = |db: &mut HashMap<(&str, &str), PricingEntry>,
               provider: &'static str,
               model: &'static str,
               input: f64,
               output: f64,
               cache_read: Option<f64>,
               cache_write: Option<f64>| {
        db.insert(
            (provider, model),
            PricingEntry {
                input_cost_per_million: Some(input),
                output_cost_per_million: Some(output),
                cache_read_cost_per_million: cache_read,
                cache_write_cost_per_million: cache_write,
                request_cost: None,
                source: CostSource::OfficialDocsSnapshot,
            },
        );
    };

    // OpenAI
    add(&mut db, "openai", "gpt-4o", 2.50, 10.0, Some(1.25), None);
    add(
        &mut db,
        "openai",
        "gpt-4o-mini",
        0.15,
        0.60,
        Some(0.075),
        None,
    );
    add(&mut db, "openai", "gpt-4.1", 2.00, 8.0, Some(0.50), None);
    add(
        &mut db,
        "openai",
        "gpt-4.1-mini",
        0.40,
        1.60,
        Some(0.10),
        None,
    );
    add(
        &mut db,
        "openai",
        "gpt-4.1-nano",
        0.10,
        0.40,
        Some(0.025),
        None,
    );
    add(&mut db, "openai", "o3", 10.0, 40.0, Some(2.50), None);
    add(&mut db, "openai", "o3-mini", 1.10, 4.40, Some(0.55), None);

    // Google Gemini
    add(&mut db, "google", "gemini-2.5-pro", 1.25, 10.0, None, None);
    add(
        &mut db,
        "google",
        "gemini-2.5-flash",
        0.15,
        0.60,
        None,
        None,
    );
    add(
        &mut db,
        "google",
        "gemini-2.0-flash",
        0.10,
        0.40,
        None,
        None,
    );

    // DeepSeek
    add(&mut db, "deepseek", "deepseek-chat", 0.14, 0.28, None, None);
    add(
        &mut db,
        "deepseek",
        "deepseek-reasoner",
        0.55,
        2.19,
        None,
        None,
    );
    add(
        &mut db,
        "deepseek",
        "deepseek-v4-pro",
        1.74,
        3.48,
        Some(0.0145),
        None,
    );

    db
}

// ---------------------------------------------------------------------------
// Core functions
// ---------------------------------------------------------------------------

/// Resolve billing route from model/provider/base_url.
pub fn resolve_billing_route(
    model_name: &str,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> BillingRoute {
    let provider_name = provider.unwrap_or("").trim().to_lowercase();
    let base = base_url.unwrap_or("").trim().to_lowercase();
    let model = model_name.trim().to_string();

    if provider_name == "openai-codex" {
        return BillingRoute {
            provider: "openai-codex".into(),
            model,
            base_url: base,
            billing_mode: BillingMode::SubscriptionIncluded,
        };
    }
    if provider_name == "openrouter" || base.contains("openrouter.ai") {
        return BillingRoute {
            provider: "openrouter".into(),
            model,
            base_url: base,
            billing_mode: BillingMode::ProviderModelsApi,
        };
    }
    if provider_name == "anthropic" {
        let bare = model.split('/').next_back().unwrap_or(&model).to_string();
        return BillingRoute {
            provider: "anthropic".into(),
            model: bare,
            base_url: base,
            billing_mode: BillingMode::OfficialDocsSnapshot,
        };
    }
    if provider_name == "openai" {
        let bare = model.split('/').next_back().unwrap_or(&model).to_string();
        return BillingRoute {
            provider: "openai".into(),
            model: bare,
            base_url: base,
            billing_mode: BillingMode::OfficialDocsSnapshot,
        };
    }

    BillingRoute {
        provider: if provider_name.is_empty() {
            "unknown".into()
        } else {
            provider_name
        },
        model: model.split('/').next_back().unwrap_or(&model).to_string(),
        base_url: base,
        billing_mode: BillingMode::Unknown,
    }
}

// Anthropic pricing in USD per 1M tokens. Source and access date:
// https://platform.claude.com/docs/en/about-claude/pricing (20 November 2026).
// The four columns are base input, output, cache reads and 5-minute writes.
const ANTHROPIC_PRICES: &[(&str, [f64; 4])] = &[
    ("claude-fable-5-2", [8.0, 40.0, 0.20, 10.0]),
    ("claude-mythos-5-2", [8.0, 40.0, 0.20, 10.0]),
    ("claude-fable-5-1", [10.0, 50.0, 0.25, 12.50]),
    ("claude-mythos-5-1", [10.0, 50.0, 0.25, 12.50]),
    ("claude-fable-5", [10.0, 50.0, 1.00, 12.50]),
    ("claude-mythos-5", [10.0, 50.0, 1.00, 12.50]),
    ("claude-opus-5-5", [4.0, 20.0, 0.20, 5.0]),
    ("claude-opus-5", [5.0, 25.0, 0.50, 6.25]),
    ("claude-opus-4-8", [5.0, 25.0, 0.50, 6.25]),
    ("claude-opus-4-7", [5.0, 25.0, 0.50, 6.25]),
    ("claude-opus-4-6", [5.0, 25.0, 0.50, 6.25]),
    ("claude-opus-4-5", [5.0, 25.0, 0.50, 6.25]),
    ("claude-opus-4-1", [15.0, 75.0, 1.50, 18.75]),
    ("claude-opus-4", [15.0, 75.0, 1.50, 18.75]),
    ("claude-sonnet-5-5", [2.0, 10.0, 0.10, 2.50]),
    ("claude-sonnet-5", [2.0, 10.0, 0.20, 2.50]),
    ("claude-sonnet-4-6", [3.0, 15.0, 0.30, 3.75]),
    ("claude-sonnet-4-5", [3.0, 15.0, 0.30, 3.75]),
    ("claude-sonnet-4", [3.0, 15.0, 0.30, 3.75]),
    ("claude-haiku-5-5", [0.10, 0.50, 0.01, 0.125]),
    ("claude-haiku-4-5", [1.0, 5.0, 0.10, 1.25]),
    // Historical snapshot rates retained for old usage and cloud routes.
    ("claude-3-5-sonnet", [3.0, 15.0, 0.30, 3.75]),
    ("claude-3-5-haiku", [0.80, 4.0, 0.08, 1.00]),
    ("claude-3-opus", [15.0, 75.0, 1.50, 18.75]),
    ("claude-3-haiku", [0.25, 1.25, 0.03, 0.30]),
];

/// Cache read and five-minute write ratios for custom Anthropic-compatible models.
pub const ANTHROPIC_STANDARD_CACHE_RATIOS: (f64, f64) = (0.10, 1.25);

/// Look up the Anthropic list price for one prompt, including any cached input.
/// Cache write estimates use the default five-minute TTL.
pub fn anthropic_pricing_for_prompt(model_name: &str, prompt_tokens: u64) -> Option<PricingEntry> {
    let lower = model_name.trim().to_ascii_lowercase();
    let bare = lower.rsplit('/').next().unwrap_or(&lower);
    let bare = bare.strip_prefix("anthropic:").unwrap_or(bare);
    let cloud = bare.strip_prefix("anthropic.").or_else(|| {
        let (region, name) = bare.split_once(".anthropic.")?;
        matches!(region, "global" | "ap" | "apac" | "au" | "eu" | "jp" | "us").then_some(name)
    });
    let bare = cloud.map_or(bare, |model| {
        model
            .strip_suffix("-v1:0")
            .or_else(|| model.strip_suffix("-v2:0"))
            .unwrap_or(model)
    });
    let bare = bare.strip_suffix("-latest").unwrap_or(bare);
    let family = match bare.rsplit_once(['-', '@']) {
        Some((family, suffix))
            if suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()) =>
        {
            family
        }
        _ => bare,
    };
    let family = family.replace('.', "-");
    let [input, output, read, write] = if family == "claude-haiku-5-5" && prompt_tokens > 100_000 {
        [0.50, 2.50, 0.05, 0.625]
    } else {
        ANTHROPIC_PRICES
            .iter()
            .find(|(model, _)| *model == family)?
            .1
    };
    Some(PricingEntry {
        input_cost_per_million: Some(input),
        output_cost_per_million: Some(output),
        cache_read_cost_per_million: Some(read),
        cache_write_cost_per_million: Some(write),
        request_cost: None,
        source: CostSource::OfficialDocsSnapshot,
    })
}

/// Look up pricing for a model.
pub fn get_pricing_entry(
    model_name: &str,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> Option<PricingEntry> {
    let route = resolve_billing_route(model_name, provider, base_url);

    if route.billing_mode == BillingMode::SubscriptionIncluded {
        return Some(PricingEntry {
            input_cost_per_million: Some(0.0),
            output_cost_per_million: Some(0.0),
            cache_read_cost_per_million: Some(0.0),
            cache_write_cost_per_million: Some(0.0),
            request_cost: None,
            source: CostSource::None,
        });
    }

    if route.provider == "anthropic" {
        return anthropic_pricing_for_prompt(&route.model, 0);
    }

    let db = official_pricing_db();
    let model_lower = route.model.to_lowercase();

    // Exact match
    if let Some(entry) = db.get(&(route.provider.as_str(), model_lower.as_str())) {
        return Some(entry.clone());
    }

    // Substring match (longest key first)
    let mut candidates: Vec<_> = db
        .iter()
        .filter(|((p, m), _)| *p == route.provider.as_str() && model_lower.contains(*m))
        .collect();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0 .1.len()));
    candidates.first().map(|(_, entry)| (*entry).clone())
}

/// Calculate cost for given usage.
pub fn calculate_cost(
    model_name: &str,
    usage: &CanonicalUsage,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> CostResult {
    let route = resolve_billing_route(model_name, provider, base_url);

    if route.billing_mode == BillingMode::SubscriptionIncluded {
        return CostResult {
            amount_usd: Some(0.0),
            status: CostStatus::Included,
            source: CostSource::None,
            label: "included".into(),
        };
    }

    let entry = if route.provider == "anthropic" {
        anthropic_pricing_for_prompt(&route.model, usage.prompt_tokens())
    } else {
        get_pricing_entry(model_name, provider, base_url)
    };
    let entry = match entry {
        Some(e) => e,
        None => {
            return CostResult {
                amount_usd: None,
                status: CostStatus::Unknown,
                source: CostSource::None,
                label: "n/a".into(),
            }
        }
    };

    let million = 1_000_000.0_f64;
    let mut amount = 0.0_f64;

    if let Some(input_cost) = entry.input_cost_per_million {
        amount += (usage.input_tokens as f64) * input_cost / million;
    }
    if let Some(output_cost) = entry.output_cost_per_million {
        amount += (usage.output_tokens as f64) * output_cost / million;
    }
    if usage.cache_read_tokens > 0 && entry.cache_read_cost_per_million.is_none() {
        return CostResult {
            amount_usd: None,
            status: CostStatus::Unknown,
            source: CostSource::None,
            label: "n/a".into(),
        };
    }
    if usage.cache_write_tokens > 0 && entry.cache_write_cost_per_million.is_none() {
        return CostResult {
            amount_usd: None,
            status: CostStatus::Unknown,
            source: CostSource::None,
            label: "n/a".into(),
        };
    }
    if let Some(cache_read_cost) = entry.cache_read_cost_per_million {
        amount += (usage.cache_read_tokens as f64) * cache_read_cost / million;
    }
    if let Some(cache_write_cost) = entry.cache_write_cost_per_million {
        amount += (usage.cache_write_tokens as f64) * cache_write_cost / million;
    }
    if let Some(request_cost) = entry.request_cost {
        amount += (usage.request_count as f64) * request_cost;
    }

    CostResult {
        amount_usd: Some(amount),
        status: CostStatus::Estimated,
        source: entry.source,
        label: format_cost(amount),
    }
}

/// Format a cost in USD for human display.
pub fn format_cost(usd: f64) -> String {
    if usd == 0.0 {
        "included".to_string()
    } else if usd < 0.01 {
        format!("~${:.4}", usd)
    } else {
        format!("~${:.2}", usd)
    }
}

/// Check whether we have pricing data for a model+route.
pub fn has_known_pricing(model_name: &str, provider: Option<&str>, base_url: Option<&str>) -> bool {
    let route = resolve_billing_route(model_name, provider, base_url);
    if route.billing_mode == BillingMode::SubscriptionIncluded {
        return true;
    }
    get_pricing_entry(model_name, provider, base_url).is_some()
}

/// Backward-compatible thin wrapper returning input/output pricing.
pub fn get_pricing(model_name: &str, provider: Option<&str>, base_url: Option<&str>) -> (f64, f64) {
    match get_pricing_entry(model_name, provider, base_url) {
        Some(entry) => (
            entry.input_cost_per_million.unwrap_or(0.0),
            entry.output_cost_per_million.unwrap_or(0.0),
        ),
        None => (0.0, 0.0),
    }
}

/// Format a token count compactly (e.g. 1.5K, 2.3M).
pub fn format_token_count_compact(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    if value >= 1_000_000_000 {
        return format!("{:.1}B", value as f64 / 1_000_000_000.0);
    }
    if value >= 1_000_000 {
        return format!("{:.1}M", value as f64 / 1_000_000.0);
    }
    let scaled = value as f64 / 1_000.0;
    if scaled < 10.0 {
        format!("{:.2}K", scaled)
    } else if scaled < 100.0 {
        format!("{:.1}K", scaled)
    } else {
        format!("{:.0}K", scaled)
    }
}

/// Format a duration compactly.
pub fn format_duration_compact(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{:.0}s", seconds)
    } else if seconds < 3600.0 {
        format!("{:.0}m", seconds / 60.0)
    } else if seconds < 86400.0 {
        let hours = (seconds / 3600.0) as u32;
        let remaining_min = ((seconds % 3600.0) / 60.0) as u32;
        if remaining_min > 0 {
            format!("{}h {}m", hours, remaining_min)
        } else {
            format!("{}h", hours)
        }
    } else {
        format!("{:.1}d", seconds / 86400.0)
    }
}

// ---------------------------------------------------------------------------
// Normalize raw API usage
// ---------------------------------------------------------------------------

/// Normalize raw API response usage into canonical token buckets.
pub fn normalize_usage(
    raw_usage: &serde_json::Value,
    provider: Option<&str>,
    api_mode: Option<&str>,
) -> CanonicalUsage {
    let provider_name = provider.unwrap_or("").trim().to_lowercase();
    let mode = api_mode.unwrap_or("").trim().to_lowercase();

    let get_u64 = |obj: &serde_json::Value, key: &str| -> u64 {
        obj.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
    };

    if mode == "anthropic_messages" || provider_name == "anthropic" {
        let input_tokens = get_u64(raw_usage, "input_tokens");
        let output_tokens = get_u64(raw_usage, "output_tokens");
        let cache_read = get_u64(raw_usage, "cache_read_input_tokens");
        let cache_write = get_u64(raw_usage, "cache_creation_input_tokens");
        return CanonicalUsage {
            input_tokens,
            output_tokens,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            reasoning_tokens: 0,
            request_count: 1,
        };
    }

    if mode == "codex_responses" {
        let input_total = get_u64(raw_usage, "input_tokens");
        let output_tokens = get_u64(raw_usage, "output_tokens");
        let details = raw_usage.get("input_tokens_details");
        let cache_read = details
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_write = details
            .and_then(|d| d.get("cache_creation_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return CanonicalUsage {
            input_tokens: input_total
                .saturating_sub(cache_read)
                .saturating_sub(cache_write),
            output_tokens,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            reasoning_tokens: 0,
            request_count: 1,
        };
    }

    // OpenAI Chat Completions (default)
    let prompt_total = get_u64(raw_usage, "prompt_tokens");
    let completion_tokens = get_u64(raw_usage, "completion_tokens");
    let details = raw_usage.get("prompt_tokens_details");
    let mut cache_read = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if cache_read == 0 {
        cache_read = get_u64(raw_usage, "cache_read_input_tokens");
    }
    let mut cache_write = details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if cache_write == 0 {
        cache_write = get_u64(raw_usage, "cache_creation_input_tokens");
    }

    let reasoning_tokens = raw_usage
        .get("output_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    CanonicalUsage {
        input_tokens: prompt_total
            .saturating_sub(cache_read)
            .saturating_sub(cache_write),
        output_tokens: completion_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens,
        request_count: 1,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_model_snapshot_ids_select_the_whole_version() {
        for (model, expected_read) in [
            ("claude-fable-5-2", 0.20),
            ("global.anthropic.claude-fable-5-2", 0.20),
            ("claude-fable-5-2@20261001", 0.20),
            ("anthropic/claude-mythos-5.1", 0.25),
            ("claude-fable-5-1-20261001", 0.25),
            ("claude-fable-5-20260101", 1.00),
            ("anthropic:claude-sonnet-5.5", 0.10),
            ("claude-sonnet-5", 0.20),
            ("anthropic.claude-haiku-4-5-20251001-v1:0", 0.10),
            ("us.anthropic.claude-haiku-4-5-20251001-v1:0", 0.10),
            ("ap.anthropic.claude-3-5-sonnet-20241022-v2:0", 0.30),
            ("apac.anthropic.claude-haiku-4-5-20251001-v1:0", 0.10),
            ("anthropic.claude-3-5-sonnet-20241022-v2:0", 0.30),
            ("claude-opus-5-5", 0.20),
            ("claude-opus-5", 0.50),
        ] {
            let entry = get_pricing_entry(model, Some("anthropic"), None)
                .unwrap_or_else(|| panic!("missing pricing for {model}"));
            assert_eq!(
                entry.cache_read_cost_per_million,
                Some(expected_read),
                "{model}"
            );
        }
        for model in [
            "claude-fable-5-20",
            "claude-opus-5-50",
            "other-claude-opus-5",
        ] {
            assert!(
                get_pricing_entry(model, Some("anthropic"), None).is_none(),
                "{model}"
            );
        }
    }

    #[test]
    fn haiku_prompt_size_includes_all_cached_tokens_in_the_rate_tier() {
        let mut usage = CanonicalUsage {
            input_tokens: 10_000,
            cache_read_tokens: 80_000,
            cache_write_tokens: 10_000,
            output_tokens: 10_000,
            ..CanonicalUsage::default()
        };
        let estimate = |usage: &CanonicalUsage| {
            calculate_cost("claude-haiku-5-5", usage, Some("anthropic"), None)
                .amount_usd
                .expect("known Haiku pricing")
        };
        assert!((estimate(&usage) - 0.00805).abs() < 1e-9);
        usage.cache_read_tokens += 1;
        assert!((estimate(&usage) - 0.040_250_05).abs() < 1e-9);
    }

    #[test]
    fn test_calculate_cost_openai() {
        let usage = CanonicalUsage {
            input_tokens: 1000,
            output_tokens: 500,
            request_count: 1,
            ..Default::default()
        };
        let result = calculate_cost("gpt-4o", &usage, Some("openai"), None);
        assert_eq!(result.status, CostStatus::Estimated);
        assert!(result.amount_usd.unwrap() > 0.0);
    }

    #[test]
    fn test_subscription_included() {
        let usage = CanonicalUsage {
            input_tokens: 1000,
            output_tokens: 500,
            request_count: 1,
            ..Default::default()
        };
        let result = calculate_cost("gpt-4o", &usage, Some("openai-codex"), None);
        assert_eq!(result.status, CostStatus::Included);
        assert_eq!(result.amount_usd, Some(0.0));
    }

    #[test]
    fn test_unknown_model() {
        let usage = CanonicalUsage::default();
        let result = calculate_cost("unknown-model", &usage, Some("unknown"), None);
        assert_eq!(result.status, CostStatus::Unknown);
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.0), "included");
        assert_eq!(format_cost(0.005), "~$0.0050");
        assert_eq!(format_cost(1.50), "~$1.50");
    }

    #[test]
    fn test_format_token_count() {
        assert_eq!(format_token_count_compact(500), "500");
        assert_eq!(format_token_count_compact(1500), "1.50K");
        assert_eq!(format_token_count_compact(1_500_000), "1.5M");
    }

    #[test]
    fn codex_responses_keep_cached_tokens_separate_from_uncached_input() {
        let raw = serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 8,
            "input_tokens_details": { "cached_tokens": 12, "cache_creation_tokens": 5 }
        });
        let usage = normalize_usage(&raw, None, Some(" CODEX_RESPONSES "));
        assert_eq!(usage.input_tokens, 83);
        assert_eq!(usage.output_tokens, 8);
        assert_eq!(usage.cache_read_tokens, 12);
        assert_eq!(usage.cache_write_tokens, 5);
        assert_eq!(usage.request_count, 1);

        let raw = serde_json::json!({ "input_tokens": 21, "output_tokens": 3 });
        let usage = normalize_usage(&raw, None, Some("codex_responses"));
        assert_eq!(usage.input_tokens, 21);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_write_tokens, 0);
    }

    #[test]
    fn inconsistent_cache_totals_cannot_overflow_uncached_input_accounting() {
        for (mode, raw) in [
            (
                "codex_responses",
                serde_json::json!({
                    "input_tokens": 10,
                    "output_tokens": 2,
                    "input_tokens_details": { "cached_tokens": u64::MAX, "cache_creation_tokens": 1 }
                }),
            ),
            (
                "chat_completions",
                serde_json::json!({
                    "prompt_tokens": 10,
                    "completion_tokens": 2,
                    "prompt_tokens_details": { "cached_tokens": u64::MAX, "cache_write_tokens": 1 }
                }),
            ),
        ] {
            let usage = normalize_usage(&raw, None, Some(mode));
            assert_eq!(usage.input_tokens, 0, "{mode}");
            assert_eq!(usage.cache_read_tokens, u64::MAX, "{mode}");
            assert_eq!(usage.cache_write_tokens, 1, "{mode}");
            assert_eq!(usage.output_tokens, 2, "{mode}");
            assert_eq!(usage.prompt_tokens(), u64::MAX, "{mode}");
            assert_eq!(usage.total_tokens(), u64::MAX, "{mode}");
        }
    }

    #[test]
    fn test_normalize_usage_anthropic() {
        let raw = serde_json::json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 50,
        });
        let usage = normalize_usage(&raw, Some("anthropic"), None);
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.cache_write_tokens, 50);
    }

    #[test]
    fn test_normalize_usage_anthropic_keeps_cache_buckets_separate() {
        let raw = serde_json::json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 2000,
            "cache_creation_input_tokens": 400,
        });
        let usage = normalize_usage(&raw, Some("anthropic"), Some("anthropic_messages"));
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 2000);
        assert_eq!(usage.cache_write_tokens, 400);
        assert_eq!(usage.prompt_tokens(), 3400);
    }

    #[test]
    fn test_normalize_usage_openai_subtracts_cached_prompt_tokens() {
        let raw = serde_json::json!({
            "prompt_tokens": 3000,
            "completion_tokens": 700,
            "prompt_tokens_details": { "cached_tokens": 1800 }
        });
        let usage = normalize_usage(&raw, Some("openai"), Some("chat_completions"));
        assert_eq!(usage.input_tokens, 1200);
        assert_eq!(usage.cache_read_tokens, 1800);
        assert_eq!(usage.output_tokens, 700);
    }

    #[test]
    fn test_normalize_usage_openai_top_level_write_with_nested_read() {
        let raw = serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "prompt_tokens_details": { "cached_tokens": 500 },
            "cache_creation_input_tokens": 300
        });
        let usage = normalize_usage(&raw, Some("openrouter"), Some("chat_completions"));
        assert_eq!(usage.cache_read_tokens, 500);
        assert_eq!(usage.cache_write_tokens, 300);
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 200);
    }

    #[test]
    fn test_normalize_usage_openai_top_level_cache_fields_when_details_missing() {
        let raw = serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "cache_read_input_tokens": 500,
            "cache_creation_input_tokens": 300
        });
        let usage = normalize_usage(&raw, Some("openrouter"), Some("chat_completions"));
        assert_eq!(usage.cache_read_tokens, 500);
        assert_eq!(usage.cache_write_tokens, 300);
        assert_eq!(usage.input_tokens, 200);
    }

    #[test]
    fn test_normalize_usage_openai_prefers_nested_details_over_top_level_fallback() {
        let raw = serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "prompt_tokens_details": {
                "cached_tokens": 600,
                "cache_write_tokens": 150
            },
            "cache_read_input_tokens": 999,
            "cache_creation_input_tokens": 999
        });
        let usage = normalize_usage(&raw, Some("openrouter"), Some("chat_completions"));
        assert_eq!(usage.cache_read_tokens, 600);
        assert_eq!(usage.cache_write_tokens, 150);
        assert_eq!(usage.input_tokens, 250);
    }

    #[test]
    fn test_cache_usage_without_matching_rate_is_unknown() {
        let usage = CanonicalUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: 100,
            request_count: 1,
            ..Default::default()
        };
        let result = calculate_cost("gemini-2.5-pro", &usage, Some("google"), None);
        assert_eq!(result.status, CostStatus::Unknown);
        assert_eq!(result.amount_usd, None);
    }

    #[test]
    fn test_deepseek_v4_pro_pricing_entry_exists() {
        let entry = get_pricing_entry("deepseek-v4-pro", Some("deepseek"), None).unwrap();
        assert_eq!(entry.input_cost_per_million, Some(1.74));
        assert_eq!(entry.output_cost_per_million, Some(3.48));
        assert_eq!(entry.cache_read_cost_per_million, Some(0.0145));
    }

    #[test]
    fn test_deepseek_v4_pro_calculate_cost() {
        let usage = CanonicalUsage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            request_count: 1,
            ..Default::default()
        };
        let result = calculate_cost("deepseek-v4-pro", &usage, Some("deepseek"), None);
        assert_eq!(result.status, CostStatus::Estimated);
        assert_eq!(result.amount_usd, Some(3.48));
    }

    #[test]
    fn test_has_known_pricing() {
        assert!(has_known_pricing("gpt-4o", Some("openai"), None));
        assert!(has_known_pricing(
            "claude-sonnet-4-20250514",
            Some("anthropic"),
            None
        ));
        assert!(!has_known_pricing("unknown-model", Some("unknown"), None));
    }
}
