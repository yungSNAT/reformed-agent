/// The tool whose late result carries recovered sub-agent results.
const RECOVERED_RESULTS_TOOL: &str = "delegate_task";

impl AgentLoop {
    /// Populate [`AgentConfig::stored_system_prompt`] from SQLite (`sessions.system_prompt`) for session continuation.
    ///
    /// Call before [`AgentLoop::run`] when resuming a gateway/CLI session so Anthropic prefix cache matches Python.
    pub fn hydrate_stored_system_prompt_from_hermes_home(
        config: &mut AgentConfig,
        hermes_home: &std::path::Path,
    ) -> Result<(), AgentError> {
        let Some(ref sid) = config.session_id else {
            return Ok(());
        };
        if sid.trim().is_empty() {
            return Ok(());
        }
        let sp = crate::session_persistence::SessionPersistence::new(hermes_home);
        sp.ensure_db()?;
        if let Some(prompt) = sp.get_system_prompt(sid)? {
            config.stored_system_prompt = Some(prompt);
        }
        Ok(())
    }

    /// Build [`AgentResult`] messages for return / persistence (applies `persist_user_message` override).
    fn messages_for_persisted_result(
        &self,
        ctx: &ContextManager,
        persist_user_idx: Option<usize>,
        prefill_range: Option<Range<usize>>,
    ) -> Vec<Message> {
        let mut msgs = ctx.get_messages().to_vec();
        if let (Some(idx), Some(override_text)) = (
            persist_user_idx,
            self.config.persist_user_message.as_deref(),
        ) {
            // `idx` indexes the history before any compression; find that
            // message again if one has moved it. If one summarised it away,
            // there is no message left to override.
            // shortcut: found by value, so a later user message with the same
            // text would take the override instead; tracking the index through
            // each compression and checkpoint rollback would remove that.
            let idx = match ctx.compressed_transcripts().first() {
                None => Some(idx),
                Some(before) => before
                    .get(idx)
                    .and_then(|user| msgs.iter().rposition(|m| m == user)),
            };
            if let Some(msg) = idx.and_then(|i| msgs.get_mut(i)) {
                if msg.role == MessageRole::User {
                    msg.content = Some(override_text.to_string());
                }
            }
        }
        if let Some(range) = prefill_range {
            if range.start <= range.end && range.end <= msgs.len() {
                msgs.drain(range);
            }
        }
        msgs
    }

    fn named_tool_result_message(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Message {
        Message::tool_result_with_name(tool_call_id, tool_name, content)
    }

    fn tool_result_message_from_execution(result: &ToolResult, tool_calls: &[ToolCall]) -> Message {
        tool_calls
            .iter()
            .find(|tc| tc.id == result.tool_call_id)
            .map(|tc| {
                Self::named_tool_result_message(
                    &result.tool_call_id,
                    &tc.function.name,
                    &result.content,
                )
            })
            .unwrap_or_else(|| Message::tool_result(&result.tool_call_id, &result.content))
    }

    // Called from 11 exit points in the run loops, each passing its own loop
    // state; bundling that state into a struct would be a run-loop redesign.
    #[allow(clippy::too_many_arguments)]
    fn graceful_interrupt_result(
        &self,
        ctx: &mut ContextManager,
        total_turns: u32,
        tool_errors: &[hermes_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        prefill_range: Option<Range<usize>>,
    ) -> AgentResult {
        self.memory_on_session_end(ctx.get_messages());
        AgentResult {
            messages: self.messages_for_persisted_result(ctx, persist_user_idx, prefill_range),
            finished_naturally: false,
            total_turns,
            tool_errors: tool_errors.to_vec(),
            usage: accumulated_usage,
            interrupted: true,
            session_cost_usd: Some(session_cost_usd),
            session_started_hooks_fired,
            compressed_transcripts: ctx.take_compressed_transcripts(),
        }
    }

    /// Before a main model call, deliver the steer text sent since the last
    /// one, as upstream does: appended to the turn's latest tool result, the
    /// one slot that keeps roles alternating. Before the turn's first tool
    /// result it stays pending. `steers.turn_user` is the user message that
    /// opened the turn; user messages the loop adds itself, such as a
    /// finaliser's request to re-issue the answer, do not hide the tool
    /// results before them. The end-of-turn summary calls do not deliver. The
    /// text delivered is recorded in `steers`, as an auto-rollback to an
    /// earlier checkpoint must restore it.
    fn deliver_pending_steer(&self, ctx: &mut ContextManager, steers: &mut TurnSteers) {
        let Some(text) = self.interrupt.take_steer() else {
            return;
        };
        let messages = ctx.get_messages_mut();
        // shortcut: found by value, as in `messages_for_persisted_result`;
        // if compression summarised the opening message away, the whole
        // history is searched. Tracking its index through each compression
        // would remove that.
        let turn_start = steers
            .turn_user
            .as_ref()
            .and_then(|user| messages.iter().rposition(|message| message == user))
            .map_or(0, |at| at + 1);
        let tool_result = messages[turn_start..]
            .iter_mut()
            .rev()
            .find(|message| message.role == MessageRole::Tool);
        match tool_result {
            Some(message) => {
                message
                    .content
                    .get_or_insert_with(String::new)
                    .push_str(&format_steer_marker(&text));
                steers.delivered(text);
            }
            None => self.interrupt.restore_steer(text),
        }
    }

    /// Create a new agent loop.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        let route_learning = Arc::new(Mutex::new(Self::load_route_learning_state(&config)));
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
            memory_manager: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh_backoff: Arc::new(Mutex::new(HashMap::new())),
            sub_agent_orchestrator: None,
            code_index,
            lsp_context,
            route_learning,
        }
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        let route_learning = Arc::new(Mutex::new(Self::load_route_learning_state(&config)));
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt,
            memory_manager: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh_backoff: Arc::new(Mutex::new(HashMap::new())),
            sub_agent_orchestrator: None,
            code_index,
            lsp_context,
            route_learning,
        }
    }

    fn init_code_index(config: &AgentConfig) -> Option<Arc<Mutex<CodeIndex>>> {
        if !config.code_index_enabled {
            return None;
        }
        let workspace_root = std::env::var("TERMINAL_CWD")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        if !workspace_root.exists() {
            return None;
        }
        // No scan here: constructors run on async workers, and a full scan can
        // take seconds. `run` and `run_stream` refresh it on the blocking pool.
        let index = CodeIndex::default_for_workspace(workspace_root);
        Some(Arc::new(Mutex::new(index)))
    }

    fn build_lsp_context_config(config: &AgentConfig) -> LspContextConfig {
        let mut cfg = LspContextConfig::from_env();
        cfg.enabled = cfg.enabled && config.lsp_context_enabled;
        cfg.max_chars = config.lsp_context_max_chars.max(400);
        cfg
    }

    fn load_route_learning_state(config: &AgentConfig) -> HashMap<String, RouteLearningStats> {
        if !smart_routing_learning_enabled() {
            return HashMap::new();
        }
        let path = route_learning_state_path(config);
        let raw = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => return HashMap::new(),
        };
        let parsed: RouteLearningState = match serde_json::from_str(&raw) {
            Ok(state) => state,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "failed to parse route-learning state; starting empty"
                );
                return HashMap::new();
            }
        };
        let mut entries = parsed.entries;
        let now_ms = now_unix_ms();
        let _ = Self::prune_route_learning_locked(&mut entries, now_ms);
        entries
    }

    fn save_route_learning_state(&self, entries: &HashMap<String, RouteLearningStats>) {
        let path = route_learning_state_path(&self.config);
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    path = %parent.display(),
                    error = %err,
                    "failed to create route-learning state directory"
                );
                return;
            }
        }
        let body = RouteLearningState {
            schema_version: 1,
            saved_at_unix_ms: now_unix_ms(),
            entries: entries.clone(),
        };
        let serialized = match serde_json::to_vec_pretty(&body) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(error = %err, "failed to serialize route-learning state");
                return;
            }
        };
        if let Err(err) = hermes_config::atomic_write_bytes(&path, &serialized) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "failed to write route-learning state"
            );
        }
    }

    fn route_learning_effective_stats(
        stats: &RouteLearningStats,
        now_ms: i64,
    ) -> Option<RouteLearningStats> {
        if stats.samples == 0 {
            return None;
        }
        let mut out = stats.clone();
        if out.updated_at_unix_ms <= 0 {
            return Some(out);
        }
        let age_ms = now_ms.saturating_sub(out.updated_at_unix_ms).max(0);
        let ttl_secs = smart_routing_learning_ttl_secs();
        if ttl_secs > 0 {
            let ttl_ms = ttl_secs.saturating_mul(1000);
            if age_ms >= ttl_ms {
                return None;
            }
        }
        let half_life_secs = smart_routing_learning_half_life_secs();
        if half_life_secs <= 0 || age_ms <= 0 {
            return Some(out);
        }
        let half_life_ms = (half_life_secs.saturating_mul(1000)) as f64;
        let decay = (0.5_f64)
            .powf((age_ms as f64) / half_life_ms)
            .clamp(0.0, 1.0);
        let baseline_success = 0.90;
        let baseline_latency = 1800.0;
        out.success_rate = baseline_success + (out.success_rate - baseline_success) * decay;
        out.avg_latency_ms = baseline_latency + (out.avg_latency_ms - baseline_latency) * decay;
        out.consecutive_failures = ((out.consecutive_failures as f64) * decay).round() as u32;
        out.samples = ((out.samples as f64) * decay).round().max(1.0) as u32;
        Some(out)
    }

    fn prune_route_learning_locked(
        map: &mut HashMap<String, RouteLearningStats>,
        now_ms: i64,
    ) -> bool {
        let before = map.len();
        map.retain(|_, stats| Self::route_learning_effective_stats(stats, now_ms).is_some());
        map.len() != before
    }

    /// Attach an in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are actually executed by the orchestrator instead of just
    /// returning a signal envelope. See
    /// [`crate::sub_agent_orchestrator::SubAgentOrchestrator`].
    pub fn with_sub_agent_orchestrator(
        mut self,
        orchestrator: Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>,
    ) -> Self {
        self.sub_agent_orchestrator = Some(orchestrator);
        self
    }

    /// Hand the model this session's sub-agent results that an earlier turn
    /// never received (it crashed, restarted or failed first). Delivery is at
    /// least once: a process that stops after the model saw them but before
    /// the run ends hands them over again next time. They arrive as a result
    /// of `delegate_task`, so only on a turn that offers that tool; otherwise
    /// they wait for a later turn. The pair goes just before the newest user
    /// message at or after `from`, so it closes the earlier turn and the
    /// current one starts with the user, as thinking modes expect; a run
    /// with no such message leaves them waiting too.
    fn add_recovered_sub_agent_results(
        &self,
        ctx: &mut ContextManager,
        tool_schemas: &[ToolSchema],
        from: usize,
    ) {
        let offered = tool_schemas.iter().any(|t| t.name == RECOVERED_RESULTS_TOOL);
        let at = ctx
            .get_messages()
            .iter()
            .rposition(|m| m.role == MessageRole::User)
            .filter(|&i| i >= from);
        let Some(note) = self
            .sub_agent_orchestrator
            .as_ref()
            .and_then(|o| o.recover_undelivered(offered && at.is_some()))
        else {
            return;
        };
        self.emit_status(
            "lifecycle",
            "Recovered sub-agent results from an earlier turn that did not finish",
        );
        // As a tool call and its result, so a child's answer keeps the tool
        // role and no more trust than that.
        let id = format!("recovered_{}", uuid::Uuid::new_v4().simple());
        let call = ToolCall {
            id: id.clone(),
            function: hermes_core::FunctionCall {
                name: RECOVERED_RESULTS_TOOL.to_string(),
                arguments: r#"{"task":"results of earlier sub-agents"}"#.to_string(),
            },
            extra_content: None,
        };
        let mut assistant = Message::assistant_with_tool_calls(None, vec![call]);
        // Present but empty: a thinking provider expects the field echoed on
        // every earlier tool call, and this call had no reasoning. Request
        // bodies for other providers leave an empty one out.
        assistant.reasoning_content = Some(String::new());
        let result = Self::named_tool_result_message(id, RECOVERED_RESULTS_TOOL, note);
        let at = at.unwrap_or_default();
        ctx.get_messages_mut().splice(at..at, [assistant, result]);
    }

    /// A run that ended normally received its sub-agents' results; one that
    /// failed counts against handing them over again.
    fn note_sub_agent_results_delivered(&self, result: &Result<AgentResult, AgentError>) {
        let Some(orchestrator) = self.sub_agent_orchestrator.as_ref() else {
            return;
        };
        match result {
            Ok(_) => orchestrator.mark_delivered(),
            Err(_) => orchestrator.note_run_failed(),
        }
    }

    /// Attach the primary runtime credential pool (API key rotation).
    pub fn with_primary_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.primary_credential_pool = Some(pool);
        self
    }

    /// Set the memory manager.
    pub fn with_memory(mut self, mm: Arc<std::sync::Mutex<MemoryManager>>) -> Self {
        self.memory_manager = Some(mm);
        self
    }

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Set the callbacks.
    pub fn with_callbacks(mut self, cb: AgentCallbacks) -> Self {
        self.callbacks = Arc::new(cb);
        self
    }

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

    /// Return a copy of this loop bound to `tool_registry`, dropping any
    /// inherited sub-agent orchestrator so delegated runs cannot use an older
    /// wider registry.
    pub fn clone_with_tool_registry(&self, tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            config: self.config.clone(),
            tool_registry,
            llm_provider: self.llm_provider.clone(),
            interrupt: self.interrupt.clone(),
            memory_manager: self.memory_manager.clone(),
            plugin_manager: self.plugin_manager.clone(),
            callbacks: self.callbacks.clone(),
            delegate_depth: self.delegate_depth,
            primary_credential_pool: self.primary_credential_pool.clone(),
            evolution_counters: self.evolution_counters.clone(),
            oauth_refresh_backoff: self.oauth_refresh_backoff.clone(),
            sub_agent_orchestrator: None,
            code_index: self.code_index.clone(),
            lsp_context: self.lsp_context.clone(),
            route_learning: self.route_learning.clone(),
        }
    }
}
