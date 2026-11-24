impl Gateway {
    /// Create a new `Gateway` with the given session manager and config.
    pub fn new(
        session_manager: Arc<SessionManager>,
        dm_manager: DmManager,
        config: GatewayConfig,
    ) -> Self {
        let stream_manager = Arc::new(StreamManager::new(config.streaming.clone()));
        let default_model = config.model.clone();

        Self {
            adapters: RwLock::new(HashMap::new()),
            session_manager,
            browser_retirer: None,
            dm_manager: Arc::new(RwLock::new(dm_manager)),
            stream_manager,
            config,
            message_handler: RwLock::new(None),
            message_handler_with_context: RwLock::new(None),
            streaming_handler: RwLock::new(None),
            streaming_handler_with_context: RwLock::new(None),
            runtime_state: RwLock::new(HashMap::new()),
            default_model: RwLock::new(default_model),
            tool_progress_modes: RwLock::new(BTreeMap::new()),
            usage_stats: RwLock::new(HashMap::new()),
            background_tasks: Arc::new(BackgroundTaskManager::new(8)),
            mcp_reload_generation: RwLock::new(0),
            hook_registry: RwLock::new(None),
            platform_access_policies: RwLock::new(HashMap::new()),
            fail_closed_default_warnings: RwLock::new(HashSet::new()),
            message_deduplicator: RwLock::new(MessageDeduplicator::default()),
            busy_sessions: Arc::new(RwLock::new(BusySessionCoordinator::default())),
            reply_records: std::sync::OnceLock::new(),
        }
    }

    /// Create a Gateway with default DM manager (pair behavior).
    pub fn with_defaults(session_manager: Arc<SessionManager>, config: GatewayConfig) -> Self {
        Self::new(session_manager, DmManager::with_pair_behavior(), config)
    }

    /// Bind gateway browser retirement before any turns are admitted.
    pub fn with_browser_retirement(mut self, retirer: BrowserRetirer) -> Self {
        self.browser_retirer = Some(retirer);
        self
    }

    /// Merge per-request runtime hints (HTTP API, webhooks) for the composed session key.
    pub async fn merge_request_runtime_overrides(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
        model: Option<String>,
        provider: Option<String>,
        personality: Option<String>,
    ) {
        let session_key = self
            .session_manager
            .compose_session_key(platform, chat_id, user_id);
        let mut states = self.runtime_state.write().await;
        let s = states.entry(session_key).or_default();
        if let Some(m) = model {
            s.model = Some(m.clone());
            if m.contains(':') {
                s.provider = None;
            }
        }
        if let Some(p) = provider {
            s.provider = Some(p);
        }
        if let Some(pers) = personality {
            s.personality = Some(pers);
        }
    }

    /// Number of messages currently stored for the session (platform + chat + user).
    pub async fn session_transcript_len(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
    ) -> usize {
        let key = self
            .session_manager
            .compose_session_key(platform, chat_id, user_id);
        self.session_manager.get_messages(&key).await.len()
    }

    /// Effective model for a composed platform/chat/user session, including
    /// per-session overrides and process-wide `/model --global` state.
    pub async fn effective_model_for_session(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
    ) -> Option<String> {
        let key = self
            .session_manager
            .compose_session_key(platform, chat_id, user_id);
        self.effective_session_model(&key).await
    }

    fn reaction_lifecycle_plan(
        incoming: &IncomingMessage,
        access_policy: Option<&PlatformAccessPolicy>,
    ) -> Option<ReactionLifecyclePlan> {
        if incoming.text.trim_start().starts_with('/') {
            return None;
        }
        incoming
            .message_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        if matches!(
            access_policy.and_then(|policy| policy.reactions_enabled),
            Some(false)
        ) {
            return None;
        }
        if !(incoming.is_dm || incoming.text.contains("<@")) {
            return None;
        }

        if incoming.platform.eq_ignore_ascii_case("slack") {
            return Some(ReactionLifecyclePlan {
                start: "eyes",
                success: "white_check_mark",
                error: "x",
            });
        }
        if incoming.platform.eq_ignore_ascii_case("discord") {
            return Some(ReactionLifecyclePlan {
                start: "👀",
                success: "✅",
                error: "❌",
            });
        }
        if incoming.platform.eq_ignore_ascii_case("telegram")
            && matches!(
                access_policy.and_then(|policy| policy.reactions_enabled),
                Some(true)
            )
        {
            return Some(ReactionLifecyclePlan {
                start: "👀",
                success: "👍",
                error: "👎",
            });
        }
        None
    }

    /// Set the message handler for processing incoming messages.
    pub async fn set_message_handler(&self, handler: MessageHandler) {
        *self.message_handler.write().await = Some(handler);
        *self.message_handler_with_context.write().await = None;
    }

    /// Set a context-aware message handler for processing incoming messages.
    pub async fn set_message_handler_with_context(&self, handler: MessageHandlerWithContext) {
        *self.message_handler_with_context.write().await = Some(handler);
    }

    /// Set the streaming message handler.
    pub async fn set_streaming_handler(&self, handler: StreamingMessageHandler) {
        *self.streaming_handler.write().await = Some(handler);
        *self.streaming_handler_with_context.write().await = None;
    }

    /// Set a context-aware streaming message handler.
    pub async fn set_streaming_handler_with_context(
        &self,
        handler: StreamingMessageHandlerWithContext,
    ) {
        *self.streaming_handler_with_context.write().await = Some(handler);
    }

    /// Attach gateway hook registry for emitting lifecycle/progress events.
    pub async fn set_hook_registry(&self, registry: Arc<HookRegistry>) {
        *self.hook_registry.write().await = Some(registry);
    }

    /// Set per-platform access policies for non-DM and slash-command traffic.
    pub async fn set_platform_access_policies(
        &self,
        policies: HashMap<String, PlatformAccessPolicy>,
    ) {
        *self.platform_access_policies.write().await = policies
            .into_iter()
            .map(|(platform, policy)| (platform.to_ascii_lowercase(), policy))
            .collect();
    }

    async fn platform_access_policy(&self, platform: &str) -> Option<PlatformAccessPolicy> {
        let key = platform.trim().to_ascii_lowercase();
        self.platform_access_policies
            .read()
            .await
            .get(&key)
            .cloned()
    }

    /// Emit one hook event if a registry is configured.
    pub async fn emit_hook_event(&self, event_type: &str, context: serde_json::Value) {
        let registry = self.hook_registry.read().await.clone();
        if let Some(reg) = registry {
            reg.emit(&HookEvent::new(event_type, context)).await;
        }
    }

    fn session_lifecycle_context(
        session_key: &str,
        session: &Session,
        reason: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "platform": session.platform,
            "chat_id": session.chat_id,
            "user_id": session.user_id,
            "session_key": session_key,
            "session_id": session.id,
            "reason": reason,
        })
    }

    async fn notify_finalized_session(
        registry: &HookRegistry,
        session_key: &str,
        session: Arc<Session>,
        reason: &str,
    ) {
        let event = HookEvent::new(
            "on_session_finalize",
            Self::session_lifecycle_context(session_key, &session, reason),
        );
        registry.emit_finalized_session(&event, session).await;
    }

    async fn emit_session_finalize(&self, session_key: &str, session: Arc<Session>, reason: &str) {
        let registry = self.hook_registry.read().await.clone();
        if let Some(registry) = registry {
            Self::notify_finalized_session(&registry, session_key, session, reason).await;
        }
    }

    async fn run_admission_lifecycle(
        registry: &HookRegistry,
        incoming: &IncomingMessage,
        session_key: &str,
        transition: SessionTransition,
    ) {
        let reason = match transition {
            SessionTransition::Created => "new",
            SessionTransition::PolicyReset(previous) => {
                Self::notify_finalized_session(registry, session_key, Arc::from(previous), "auto_reset")
                    .await;
                "auto_reset"
            }
            SessionTransition::Existing => return,
        };
        registry
            .emit(&HookEvent::new(
                "session:start",
                serde_json::json!({
                    "platform": incoming.platform,
                    "chat_id": incoming.chat_id,
                    "user_id": incoming.user_id,
                    "session_id": session_key,
                    "reason": reason,
                }),
            ))
            .await;
    }

    async fn run_reset_lifecycle(
        registry: &HookRegistry,
        incoming: &IncomingMessage,
        session_key: &str,
        old_session: Option<Session>,
        reset_session: Session,
    ) {
        if let Some(old_session) = old_session {
            let ended = Arc::new(old_session);
            let event = HookEvent::new("session:end", serde_json::json!({
                "platform": incoming.platform,
                "chat_id": incoming.chat_id,
                "user_id": incoming.user_id,
                "session_id": session_key,
                "logical_session_id": ended.id,
            }));
            registry.emit_finalized_session(&event, ended.clone()).await;
            Self::notify_finalized_session(registry, session_key, ended, "reset").await;
        } else {
            registry.emit(&HookEvent::new("session:start", serde_json::json!({
                "platform": incoming.platform,
                "chat_id": incoming.chat_id,
                "user_id": incoming.user_id,
                "session_id": session_key,
                "reason": "new",
            }))).await;
        }
        registry.emit(&HookEvent::new("session:reset", serde_json::json!({
            "platform": incoming.platform,
            "chat_id": incoming.chat_id,
            "user_id": incoming.user_id,
            "session_id": session_key,
            "logical_session_id": reset_session.id,
        }))).await;
        registry.emit(&HookEvent::new(
            "on_session_reset",
            Self::session_lifecycle_context(session_key, &reset_session, "reset"),
        )).await;
    }

    async fn finalize_active_sessions(&self, reason: &str) -> usize {
        let sessions = self.session_manager.all_sessions().await;
        let count = sessions.len();
        for (session_key, session) in sessions {
            hermes_tools::gateway_approval::clear(&session_key);
            self.emit_session_finalize(&session_key, Arc::new(session), reason)
                .await;
        }
        count
    }

    fn busy_input_mode(&self) -> BusyInputMode {
        match self.config.display.normalized_busy_input_mode() {
            "queue" => BusyInputMode::Queue,
            "steer" => BusyInputMode::Steer,
            _ => BusyInputMode::Interrupt,
        }
    }

    fn incoming_to_busy_event(incoming: &IncomingMessage, text: impl Into<String>) -> MessageEvent {
        let mut source = SessionSource::new(
            &incoming.platform,
            &incoming.chat_id,
            if incoming.is_dm { "dm" } else { "group" },
        )
        .with_user(&incoming.user_id);
        if let Some(thread_id) = incoming
            .thread_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            source = source.with_thread(thread_id);
        }
        let mut event = MessageEvent::text(text, source);
        event.message_id = incoming.message_id.clone();
        event.message_type = MessageType::Text;
        event
    }

    fn busy_event_to_incoming(event: MessageEvent) -> IncomingMessage {
        IncomingMessage {
            platform: event.source.platform,
            chat_id: event.source.chat_id,
            user_id: event
                .source
                .user_id
                .unwrap_or_else(|| "unknown".to_string()),
            text: event.text,
            message_id: event.message_id,
            thread_id: event.source.thread_id,
            is_dm: event.source.chat_type.eq_ignore_ascii_case("dm"),
        }
    }

    /// Register a platform adapter under the given name.
    pub async fn register_adapter(
        &self,
        name: impl Into<String>,
        adapter: Arc<dyn PlatformAdapter>,
    ) {
        let name = name.into();
        info!("Registering platform adapter: {}", name);
        self.adapters.write().await.insert(name, adapter);
    }

    /// Retrieve a registered platform adapter by name.
    pub async fn get_adapter(&self, name: &str) -> Option<Arc<dyn PlatformAdapter>> {
        self.adapters.read().await.get(name).cloned()
    }

    /// Start all registered and enabled platform adapters.
    pub async fn start_all(&self) -> Result<(), GatewayError> {
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            info!("Starting platform adapter: {}", name);
            if let Err(e) = adapter.start().await {
                error!("Failed to start adapter '{}': {}", name, e);
                return Err(e);
            }
        }
        info!("All platform adapters started successfully");
        Ok(())
    }

    /// Stop all platform adapters gracefully.
    pub async fn stop_all(&self) -> Result<(), GatewayError> {
        let finalized = self.finalize_active_sessions("shutdown").await;
        if finalized > 0 {
            info!(
                finalized,
                "Finalized active gateway sessions before shutdown"
            );
        }
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            info!("Stopping platform adapter: {}", name);
            if let Err(e) = adapter.stop().await {
                warn!("Error stopping adapter '{}': {}", name, e);
            }
        }
        info!("All platform adapters stopped");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Message routing
    // -----------------------------------------------------------------------
}
