fn contextlattice_connect_system_hint(messages: &[Message]) -> Option<String> {
    if !detect_contextlattice_connect_intent(messages) {
        return None;
    }
    Some(
        "[SYSTEM] ContextLattice integration intent detected. Execute this order: \
         (1) If available, inspect local instructions file from `HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH` \
         (2) call `contextlattice_search` for a direct connectivity probe; \
         (3) if needed call `contextlattice_context_pack` for broader grounding; \
         (4) call `contextlattice_write` to checkpoint what was verified. \
         Never use terminal command `contextlattice` for this workflow."
            .to_string(),
    )
}

fn contextlattice_intelligence_system_hint(
    messages: &[Message],
    tool_schemas: &[ToolSchema],
) -> Option<String> {
    let has_context_tools = tool_schemas.iter().any(|t| {
        matches!(
            t.name.as_str(),
            "contextlattice_search"
                | "contextlattice_context_pack"
                | "contextlattice_write"
                | "memory"
        )
    });
    if !has_context_tools {
        return None;
    }

    let objective_active = objective_guard_policy(messages).0;
    let repo_intent = detect_repo_review_intent(messages);
    let connect_intent = detect_contextlattice_connect_intent(messages);
    if !(objective_active || repo_intent || connect_intent) {
        return None;
    }

    Some(
        "[SYSTEM] ContextLattice-first intelligence policy active.\n\
         1) Start with scoped retrieval (`contextlattice_search`) using project + topic path.\n\
         2) If scoped retrieval is empty/degraded, run one broader retrieval in the same project and compare.\n\
         3) For broad or multi-file tasks, run `contextlattice_context_pack` before deep tool loops.\n\
         4) During long execution, checkpoint durable progress with `contextlattice_write`.\n\
         5) Before final answer, run one scoped readback and report contradictions as `unproven` rather than guessing.\n\
         6) Copy numeric facts verbatim; do not normalize or round unless explicitly requested."
            .to_string(),
    )
}

fn is_contextlattice_shell_invocation(raw_args: &str) -> bool {
    let Ok(args) = serde_json::from_str::<Value>(raw_args) else {
        return false;
    };
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    let lower = command.to_ascii_lowercase();
    lower == "contextlattice" || lower.starts_with("contextlattice ")
}

fn is_safe_background_review_message(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed.is_empty() || trimmed.len() > 200 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    // Suppress operational/status leakage from background passes.
    if lower.contains("status:")
        || lower.contains("status=")
        || lower.contains("token")
        || lower.contains("api_key")
        || lower.contains("secret")
        || lower.contains("credential")
    {
        return false;
    }
    // Skip verbose object-like payloads.
    if (trimmed.contains('{') && trimmed.contains('}')) || trimmed.contains('\n') {
        return false;
    }
    true
}

fn summarize_background_review_result(messages: &[Message]) -> Option<String> {
    let mut actions: Vec<String> = Vec::new();
    for msg in messages {
        if !matches!(msg.role, hermes_core::MessageRole::Tool) {
            continue;
        }
        let Some(raw) = msg.content.as_deref() else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if !data
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let message = data
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let target = data
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if message.is_empty() {
            continue;
        }
        if !is_safe_background_review_message(&message) {
            continue;
        }
        let lower = message.to_ascii_lowercase();
        if lower.contains("created") || lower.contains("updated") {
            actions.push(message);
        } else if lower.contains("added")
            || (!target.is_empty() && lower.contains("add"))
            || lower.contains("removed")
            || lower.contains("replaced")
        {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        }
    }
    if actions.is_empty() {
        return None;
    }
    let mut deduped: Vec<String> = Vec::new();
    for action in actions {
        if !deduped.iter().any(|a| a == &action) {
            deduped.push(action);
        }
    }
    Some(format!("💾 {}", deduped.join(" · ")))
}

fn default_model_cost_per_million(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") || m.contains("4.1-mini") || m.contains("haiku") {
        return Some((0.15, 0.60));
    }
    if m.contains("gpt-4o") || m.contains("4.1") || m.contains("sonnet") {
        return Some((2.5, 10.0));
    }
    if m.contains("o3") {
        return Some((10.0, 40.0));
    }
    None
}

fn extract_objective_state_marker(text: &str) -> String {
    for line in text.lines() {
        let lowered = line.trim().to_ascii_lowercase();
        if let Some(rest) = lowered.split("objective_state=").nth(1) {
            let token = rest
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == ')' || c == '.');
            if !token.is_empty() {
                return token.to_string();
            }
        }
        if let Some(rest) = lowered.split("objective_state:").nth(1) {
            let token = rest
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == ')' || c == '.');
            if !token.is_empty() {
                return token.to_string();
            }
        }
    }
    "unspecified".to_string()
}

fn extract_marker_values(text: &str, marker: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(idx) = line.find(marker) else {
            continue;
        };
        let rest = &line[idx + marker.len()..];
        let value = rest
            .split(|c: char| c.is_whitespace() || c == ')' || c == ',' || c == ';' || c == '|')
            .next()
            .unwrap_or("")
            .trim();
        if value.is_empty() {
            continue;
        }
        let normalized = value.trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
        if normalized.is_empty() || out.iter().any(|v| v == normalized) {
            continue;
        }
        out.push(normalized.to_string());
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn estimate_usage_cost_usd(usage: &UsageStats, model: &str, config: &AgentConfig) -> Option<f64> {
    if let Some(v) = usage.estimated_cost {
        return Some(v.max(0.0));
    }
    let anthropic = hermes_intelligence::usage_pricing::anthropic_pricing_for_prompt(
        model, usage.prompt_tokens,
    );
    let listed = match anthropic {
        Some(_) => None,
        None => listed_pricing(model, config.provider.as_deref()),
    };
    let base_input = anthropic.as_ref().and_then(|entry| entry.input_cost_per_million);
    let base_output = anthropic.as_ref().and_then(|entry| entry.output_cost_per_million);
    let (input, output) = match (
        config.prompt_cost_per_million_usd,
        config.completion_cost_per_million_usd,
    ) {
        (Some(i), Some(o)) => (i, o),
        _ => base_input
            .zip(base_output)
            .or_else(|| {
                let entry = listed.as_ref()?;
                entry.input_cost_per_million.zip(entry.output_cost_per_million)
            })
            .or_else(|| default_model_cost_per_million(model))?,
    };
    let cache_tokens = usage.cache_read_tokens.saturating_add(usage.cache_write_tokens);
    let uncached = usage.prompt_tokens.saturating_sub(cache_tokens);
    let mut cost = uncached as f64 * input + usage.completion_tokens as f64 * output;
    if cache_tokens > 0 {
        let (read, write) = if let Some(entry) = anthropic {
            // Custom base rates retain the known model's cache-to-input price ratios.
            let scale = input / base_input?;
            (scale * entry.cache_read_cost_per_million?, scale * entry.cache_write_cost_per_million?)
        } else if model.to_ascii_lowercase().contains("claude") {
            let (read, write) =
                hermes_intelligence::usage_pricing::ANTHROPIC_STANDARD_CACHE_RATIOS;
            (input * read, input * write)
        } else {
            // The model's own listed cache prices, scaled to `input`; an unlisted one is
            // charged at full input, so an unknown discount never makes the estimate low.
            let scaled = |pick: fn(&PricingEntry) -> Option<f64>| {
                let entry = listed.as_ref()?;
                let base = entry.input_cost_per_million.filter(|base| *base > 0.0)?;
                Some(pick(entry)? * input / base)
            };
            (
                scaled(|e| e.cache_read_cost_per_million).unwrap_or(input),
                scaled(|e| e.cache_write_cost_per_million).unwrap_or(input),
            )
        };
        cost += read * usage.cache_read_tokens as f64 + write * usage.cache_write_tokens as f64;
    }
    Some(cost / 1_000_000.0)
}

use hermes_intelligence::usage_pricing::PricingEntry;

/// The official price table's entry for `model`, looked up under the configured provider,
/// then a `provider:` prefix, then a `vendor/` prefix (an OpenRouter route such as
/// `openai/gpt-4o` is billed at its vendor's prices). None for a model the table does not
/// list, or lists at no charge (a subscription route); its cached tokens are then charged at
/// full input.
fn listed_pricing(model: &str, provider: Option<&str>) -> Option<PricingEntry> {
    let (prefix, rest) = match model.split_once(':') {
        Some((prefix, rest)) => (Some(prefix), rest),
        None => (None, model),
    };
    let vendor = rest.split_once('/').map(|(vendor, _)| vendor);
    [provider, prefix, vendor]
        .into_iter()
        .flatten()
        .filter_map(|p| hermes_intelligence::usage_pricing::get_pricing_entry(rest, Some(p), None))
        .find(|entry| entry.input_cost_per_million.is_some_and(|input| input > 0.0))
}

#[cfg(test)]
mod non_anthropic_cache_pricing_tests {
    use super::*;

    #[test]
    fn openai_cache_reads_cost_the_listed_cached_price() {
        // gpt-4o: $2.50 input, $1.25 cached input, $10 output per million.
        let usage = UsageStats {
            prompt_tokens: 100_000,
            completion_tokens: 1_000,
            cache_read_tokens: 90_000,
            ..UsageStats::default()
        };
        let cost = estimate_usage_cost_usd(&usage, "openai:gpt-4o", &AgentConfig::default());
        assert!((cost.unwrap() - 0.1475).abs() < 1e-9, "{cost:?}");
    }

    #[test]
    fn gpt_4_1_is_priced_from_its_listed_rates() {
        // gpt-4.1: $2 input, $0.50 cached input, $8 output per million.
        let usage = UsageStats {
            prompt_tokens: 100_000,
            completion_tokens: 1_000,
            cache_read_tokens: 90_000,
            ..UsageStats::default()
        };
        let cost = estimate_usage_cost_usd(&usage, "openai:gpt-4.1", &AgentConfig::default());
        assert!((cost.unwrap() - 0.073).abs() < 1e-9, "{cost:?}");
    }

    #[test]
    fn a_model_routed_through_openrouter_takes_its_vendors_cached_price() {
        // OpenRouter passes gpt-4o through at OpenAI's prices: $2.50 input, $1.25 cached.
        let usage = UsageStats {
            prompt_tokens: 100_000,
            completion_tokens: 1_000,
            cache_read_tokens: 90_000,
            ..UsageStats::default()
        };
        let config = AgentConfig {
            provider: Some("openrouter".into()),
            ..AgentConfig::default()
        };
        for model in ["openai/gpt-4o", "openai:gpt-4o", "openrouter:openai/gpt-4o"] {
            let cost = estimate_usage_cost_usd(&usage, model, &config);
            assert!((cost.unwrap() - 0.1475).abs() < 1e-9, "{model}: {cost:?}");
        }
    }

    #[test]
    fn a_subscription_route_is_not_priced_at_zero() {
        let usage = UsageStats {
            prompt_tokens: 100_000,
            completion_tokens: 1_000,
            ..UsageStats::default()
        };
        let config = AgentConfig {
            provider: Some("openai-codex".into()),
            ..AgentConfig::default()
        };
        let cost = estimate_usage_cost_usd(&usage, "gpt-4o", &config);
        assert!(cost.unwrap() > 0.0, "{cost:?}");
    }

    /// What the cost guard and the session cap are given is never below the list price
    /// of a listed model, cached or not.
    #[test]
    fn the_spend_guards_never_see_less_than_list_price() {
        let usage = UsageStats {
            prompt_tokens: 100_000,
            completion_tokens: 1_000,
            cache_read_tokens: 90_000,
            ..UsageStats::default()
        };
        for model in [
            "openai:gpt-4o",
            "openai:gpt-4o-mini",
            "openai:gpt-4.1",
            "openai:gpt-4.1-mini",
            "openai:gpt-4.1-nano",
            "openai:o3",
            "openai:o3-mini",
            "deepseek:deepseek-chat",
            "deepseek:deepseek-reasoner",
            "deepseek:deepseek-v4-pro",
        ] {
            let entry = listed_pricing(model, None).expect(model);
            let input = entry.input_cost_per_million.unwrap();
            let read = entry.cache_read_cost_per_million.unwrap_or(input);
            let list = (10_000.0 * input
                + 90_000.0 * read
                + 1_000.0 * entry.output_cost_per_million.unwrap())
                / 1_000_000.0;
            let cost = estimate_usage_cost_usd(&usage, model, &AgentConfig::default());
            assert!(cost.unwrap() >= list - 1e-12, "{model}: {cost:?} < {list}");
        }
    }

    #[test]
    fn unlisted_cache_prices_charge_full_input() {
        let cfg = AgentConfig {
            prompt_cost_per_million_usd: Some(1.0),
            completion_cost_per_million_usd: Some(2.0),
            ..AgentConfig::default()
        };
        let usage = UsageStats {
            prompt_tokens: 1_000_000,
            cache_read_tokens: 900_000,
            ..UsageStats::default()
        };
        let cost = estimate_usage_cost_usd(&usage, "some-local-model", &cfg);
        assert!((cost.unwrap() - 1.0).abs() < 1e-9, "{cost:?}");
    }
}

/// Merge two UsageStats, summing token counts and keeping the latest cost estimate.
fn merge_usage(existing: Option<UsageStats>, new: &UsageStats) -> UsageStats {
    match existing {
        Some(prev) => UsageStats {
            prompt_tokens: prev.prompt_tokens + new.prompt_tokens,
            completion_tokens: prev.completion_tokens + new.completion_tokens,
            total_tokens: prev.total_tokens + new.total_tokens,
            estimated_cost: match (prev.estimated_cost, new.estimated_cost) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
            cache_read_tokens: prev.cache_read_tokens + new.cache_read_tokens,
            cache_write_tokens: prev.cache_write_tokens + new.cache_write_tokens,
        },
        None => new.clone(),
    }
}
