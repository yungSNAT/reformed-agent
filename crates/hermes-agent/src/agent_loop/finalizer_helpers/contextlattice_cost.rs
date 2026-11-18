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

/// The price of a cache read as a fraction of the model's input price, from
/// Anthropic's prompt caching page (Pricing, footnotes 1 and 2, read
/// 18 November 2026): 0.025 for Claude Fable and Mythos 5.1 and 5.2, 0.05 for
/// Claude Opus and Sonnet 5.5, and 0.1 for every other model. Keep the list
/// to the page's footnotes. A dated snapshot id such as `claude-fable-5-20260101`
/// names version 5, not 5.2.
fn anthropic_cache_read_multiplier(model: &str) -> f64 {
    let model = model.to_ascii_lowercase().replace('.', "-");
    let parts: Vec<&str> = model.split(['-', '/', ':', '@']).collect();
    for name in parts.windows(3) {
        match (name[0], name[1], name[2]) {
            ("fable" | "mythos", "5", "1" | "2") => return 0.025,
            ("opus" | "sonnet", "5", "5") => return 0.05,
            _ => {}
        }
    }
    0.1
}

fn estimate_usage_cost_usd(usage: &UsageStats, model: &str, config: &AgentConfig) -> Option<f64> {
    if let Some(v) = usage.estimated_cost {
        return Some(v.max(0.0));
    }
    let (in_pm, out_pm) = match (
        config.prompt_cost_per_million_usd,
        config.completion_cost_per_million_usd,
    ) {
        (Some(i), Some(o)) => (i, o),
        _ => default_model_cost_per_million(model)?,
    };
    // shortcut: Anthropic's rates, as only the Anthropic provider reports
    // cache counts here (an Anthropic-compatible endpoint is assumed to price
    // as Anthropic does): a cache read costs the model's read multiplier and
    // a 5-minute cache write 1.25 of the input price. A 1-hour write (2x) is
    // priced as a 5-minute one, as no request asks for a 1-hour cache; carry
    // the provider's rates with the counts once another provider reports them.
    let cached = usage.cache_read_tokens + usage.cache_write_tokens;
    let uncached = usage.prompt_tokens.saturating_sub(cached);
    let prompt_tokens = uncached as f64
        + anthropic_cache_read_multiplier(model) * usage.cache_read_tokens as f64
        + 1.25 * usage.cache_write_tokens as f64;
    let prompt_cost = (prompt_tokens / 1_000_000.0) * in_pm;
    let completion_cost = (usage.completion_tokens as f64 / 1_000_000.0) * out_pm;
    Some(prompt_cost + completion_cost)
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
