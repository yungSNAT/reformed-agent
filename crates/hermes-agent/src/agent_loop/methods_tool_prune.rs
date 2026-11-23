impl AgentLoop {
    /// Where pruned tool results are archived: only when the agent has
    /// `read_file` to open them again.
    fn tool_result_archive_dir(&self) -> Option<PathBuf> {
        self.tool_registry.get("read_file")?;
        Some(
            crate::context::resolve_hermes_home(self.config.hermes_home.as_deref())
                .join(tool_result_prune::PRUNED_SUBDIR),
        )
    }

    /// Turn start: once this session's prompt cache has expired, rewriting old
    /// messages breaks no cached prefix, so archive and shorten old tool
    /// results before the first request.
    fn prune_tool_results_on_cold_cache(&self, ctx: &mut ContextManager) {
        let Some(session_id) = self.config.session_id.as_deref().filter(|s| !s.is_empty())
        else {
            return;
        };
        let ttl = tool_result_prune::cache_ttl_for_model(&self.config.model);
        if !tool_result_prune::cache_expired(session_id, ttl) {
            return;
        }
        let Some(dir) = self.tool_result_archive_dir() else {
            return;
        };
        let pruned =
            ctx.prune_tool_results(&dir, session_id, tool_result_prune::COLD_MIN_RECLAIM_CHARS);
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

    /// Start this session's prompt-cache clock again: a run just sent requests.
    fn note_prompt_cache_use(&self) {
        if let Some(session_id) = self.config.session_id.as_deref() {
            tool_result_prune::record_cache_touch(session_id);
        }
    }
}
