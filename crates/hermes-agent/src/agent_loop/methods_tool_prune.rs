impl AgentLoop {
    /// Run start: let `ctx` archive pruned tool results when this turn offers
    /// `read_file` and it reads this machine's files. Then, once this
    /// session's prompt cache has expired (rewriting old messages breaks no
    /// cached prefix), archive and shorten old tool results before the first
    /// request.
    fn prune_tool_results_on_cold_cache(
        &self,
        ctx: &mut ContextManager,
        tool_schemas: &[ToolSchema],
    ) {
        if !tool_schemas.iter().any(|t| t.name == "read_file")
            || !tool_result_prune::file_tools_read_this_machine()
        {
            return;
        }
        ctx.set_tool_result_archive(
            crate::context::resolve_hermes_home(self.config.hermes_home.as_deref())
                .join(tool_result_prune::PRUNED_SUBDIR),
        );
        let Some(session_id) = self.config.session_id.as_deref().filter(|s| !s.is_empty())
        else {
            return;
        };
        let ttl = tool_result_prune::cache_ttl_for_model(&self.config.model);
        if !tool_result_prune::cache_expired(session_id, ttl) {
            return;
        }
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
}
