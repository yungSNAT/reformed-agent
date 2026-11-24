impl AgentLoop {
    /// True when this session's prompt cache has expired since its last use.
    fn session_cache_cold(&self) -> bool {
        let Some(session_id) = self.config.session_id.as_deref().filter(|s| !s.is_empty())
        else {
            return false;
        };
        let ttl = tool_result_prune::cache_ttl_for_model(
            &self.config.model,
            self.config.prompt_caching.cache_ttl,
        );
        tool_result_prune::cache_expired(session_id, ttl)
    }

    /// Run start: let `ctx` archive pruned tool results when this turn offers
    /// `read_file` and it reads this machine's files. Then, once this
    /// session's prompt cache has expired (rewriting old messages breaks no
    /// cached prefix), archive and shorten old tool results before the first
    /// request, and with `prompt_caching.compact_when_cold` compact a history
    /// still at least half the compression threshold.
    fn prune_tool_results_on_cold_cache(
        &self,
        ctx: &mut ContextManager,
        tool_schemas: &[ToolSchema],
    ) {
        let cold = self.session_cache_cold();
        if tool_schemas.iter().any(|t| t.name == "read_file")
            && tool_result_prune::file_tools_read_this_machine()
        {
            ctx.set_tool_result_archive(
                crate::context::resolve_hermes_home(self.config.hermes_home.as_deref())
                    .join(tool_result_prune::PRUNED_SUBDIR),
            );
            if let Some(session_id) = self.config.session_id.as_deref().filter(|_| cold) {
                let pruned =
                    ctx.prune_tool_results(session_id, tool_result_prune::COLD_MIN_RECLAIM_CHARS);
                if pruned.pruned > 0 {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Prompt cache expired: archived {} old tool results ({} chars)",
                            pruned.pruned, pruned.reclaimed_chars
                        ),
                    );
                }
            }
        }
        let max_c = ctx.max_context_chars().max(1);
        // Half of the 80% threshold `auto_compress_if_over_threshold` uses.
        if cold
            && self.config.prompt_caching.compact_when_cold
            && ctx.total_chars() >= max_c / 10 * 4
        {
            let message = format!(
                "Prompt cache expired with context at {}%: compacting before the first request",
                (ctx.total_chars() * 100) / max_c
            );
            self.compress_with_status(ctx, &message, true);
        }
    }

    /// Before compressing: archive old tool results, which loses nothing.
    /// Returns true when that alone left the history well under the budget
    /// (70%), so no summary is needed.
    fn prune_before_compression(&self, ctx: &mut ContextManager) -> bool {
        // Archives are named for their session, so delete_session can find them.
        let Some(session) = self.config.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return false;
        };
        let pruned = ctx.prune_tool_results(session, 0);
        if pruned.pruned == 0 {
            return false;
        }
        self.emit_status(
            "lifecycle",
            &format!(
                "Archived {} old tool results ({} chars) before compressing",
                pruned.pruned, pruned.reclaimed_chars
            ),
        );
        ctx.total_chars() <= ctx.max_context_chars() / 10 * 7
    }

    /// Keep this request as the session's last, for keep-alive requests.
    fn remember_cache_request(
        &self,
        target: &AttemptRuntime,
        messages: &[Message],
        tools: &[ToolSchema],
        usage: Option<&hermes_core::UsageStats>,
    ) {
        let (Some(session_id), Some(usage)) = (self.config.session_id.as_deref(), usage) else {
            return;
        };
        if self.config.prompt_caching.keep_alive.pings() == 0 {
            return;
        }
        crate::context::cache_keep_alive::remember(
            session_id,
            crate::context::cache_keep_alive::CachedRequest {
                provider: target.provider.clone(),
                messages: messages.to_vec(),
                tools: tools.to_vec(),
                model: target.wire_model.clone(),
                extra_body: target.extra_body.clone(),
                usage: usage.clone(),
            },
        );
    }

    /// Run start: no keep-alive runs beside a turn.
    fn cancel_cache_keep_alive(&self) {
        if let Some(session_id) = self.config.session_id.as_deref() {
            crate::context::cache_keep_alive::cancel(session_id);
        }
    }

    /// Run end: warm the session's cache while it is idle, when opted in.
    fn arm_cache_keep_alive(&self) {
        let keep_alive = self.config.prompt_caching.keep_alive;
        let Some(session_id) = self.config.session_id.as_deref() else {
            return;
        };
        if keep_alive.pings() == 0 {
            return;
        }
        let config = self.config.clone();
        crate::context::cache_keep_alive::arm(
            session_id,
            crate::context::cache_keep_alive::KeepAlivePlan {
                ttl: tool_result_prune::cache_ttl_for_model(
                    &self.config.model,
                    self.config.prompt_caching.cache_ttl,
                ),
                max_pings: keep_alive.pings(),
                max_idle: Duration::from_secs(keep_alive.max_idle_seconds()),
                max_cost_usd: self.config.max_cost_usd,
                cost: Arc::new(move |usage, model| estimate_usage_cost_usd(usage, model, &config)),
                write_price_factor: match self.config.prompt_caching.cache_ttl {
                    hermes_config::PromptCacheTtl::FiveMinutes => 1.0,
                    hermes_config::PromptCacheTtl::OneHour => 2.0 / 1.25,
                },
            },
        );
    }
}
