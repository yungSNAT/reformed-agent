impl App {
    /// Append a UI-only message anchored to the current conversation size.
    pub fn push_ui_message(&mut self, message: hermes_core::Message) {
        self.ui_messages.push(UiTranscriptMessage {
            insert_at: self.messages.len(),
            message,
        });
    }

    /// Append a UI-only user transcript line.
    pub fn push_ui_user(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::user(text.into()));
    }

    /// Append a UI-only assistant transcript line.
    pub fn push_ui_assistant(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::assistant(text.into()));
    }

    /// Build the merged transcript for TUI rendering.
    ///
    /// This includes durable conversation history and UI-only events in
    /// chronological order, while preserving model-facing context purity.
    pub fn transcript_messages(&self) -> Vec<hermes_core::Message> {
        let mut merged = Vec::with_capacity(self.messages.len() + self.ui_messages.len());
        for idx in 0..=self.messages.len() {
            for ui in self.ui_messages.iter().filter(|m| m.insert_at == idx) {
                merged.push(ui.message.clone());
            }
            if idx < self.messages.len() {
                merged.push(self.messages[idx].clone());
            }
        }
        merged
    }

    fn prune_ui_after_current_messages(&mut self) {
        let cap = self.messages.len();
        self.ui_messages.retain(|m| m.insert_at <= cap);
    }

    /// Apply the finalized messages returned by an agent run.
    pub fn apply_agent_result(&mut self, result: hermes_core::AgentResult) {
        let usage = result.usage.clone();
        let run_cost = result
            .session_cost_usd
            .or_else(|| usage.as_ref().and_then(|usage| usage.estimated_cost))
            .filter(|cost| cost.is_finite() && *cost >= 0.0);

        self.last_usage = usage.clone();
        if let Some(usage) = usage {
            self.session_usage = Some(merge_usage_stats(self.session_usage.take(), &usage));
        }
        if let Some(run_cost) = run_cost {
            self.session_cost_usd += run_cost;
        }
        self.messages = result.messages;
        self.prune_ui_after_current_messages();
    }

    /// Apply finalized messages and persist the session snapshot.
    pub fn apply_agent_result_and_persist(
        &mut self,
        result: hermes_core::AgentResult,
    ) -> Result<(), AgentError> {
        self.apply_agent_result(result);
        self.persist_session_snapshot(None).map(|_| ())
    }

    /// Count background jobs currently queued/running.
    pub fn running_background_job_count(&self) -> usize {
        let jobs_dir = hermes_config::hermes_home().join("background_jobs");
        let mut active = 0usize;
        let entries = match std::fs::read_dir(jobs_dir) {
            Ok(entries) => entries,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if matches!(status, "queued" | "running") {
                active += 1;
            }
        }
        active
    }

    /// Count sub-agent lineage files still marked as started.
    pub fn active_subagent_count(&self) -> usize {
        let subagents_dir = hermes_config::hermes_home().join("subagents");
        let mut active = 0usize;
        let entries = match std::fs::read_dir(subagents_dir) {
            Ok(entries) => entries,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if matches!(status, "started" | "running" | "background_pending") {
                active += 1;
            }
        }
        active
    }

    fn prune_session_snapshot_entry(
        entry: &SessionSnapshotEntry,
        total_bytes: &mut u64,
    ) -> Result<(), AgentError> {
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {
                *total_bytes = total_bytes.saturating_sub(entry.size_bytes);
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(AgentError::Io(format!(
                "Failed to prune session snapshot {}: {}",
                entry.path.display(),
                err
            ))),
        }
    }

    fn enforce_session_snapshot_guardrails(
        sessions_dir: &Path,
        preserve_path: &Path,
    ) -> Result<(), AgentError> {
        // An interactive save may prune any other snapshot, one-shot snapshots
        // first, so one-shot runs do not shorten interactive retention. A
        // one-shot save prunes only one-shot snapshots, itself last, so it never
        // evicts an interactive snapshot; when that cannot meet a limit, its own
        // snapshot is removed and the caller reports it. sessions.db is untouched.
        let preserve = preserve_path.to_path_buf();
        let oneshot_save = is_oneshot_snapshot_path(preserve_path);
        let prunable = |entry: &SessionSnapshotEntry| {
            if oneshot_save {
                is_oneshot_snapshot_path(&entry.path)
            } else {
                entry.path != preserve
            }
        };
        let mut entries = list_session_snapshot_entries(sessions_dir);
        // Oldest first within each group, from list_session_snapshot_entries;
        // the new snapshot goes last whatever the other files' dates.
        entries.sort_by_key(|entry| {
            (
                entry.path == preserve,
                !is_oneshot_snapshot_path(&entry.path),
            )
        });
        let mut total_bytes = entries.iter().map(|e| e.size_bytes).sum::<u64>();

        let max_files = snapshot_max_files();
        if max_files > 0 {
            while entries.len() > max_files {
                let Some(idx) = entries.iter().position(&prunable) else {
                    break;
                };
                let removed = entries.remove(idx);
                Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
            }
        }

        let max_total_bytes = snapshot_max_total_bytes();
        if max_total_bytes > 0 {
            while total_bytes > max_total_bytes {
                let Some(idx) = entries.iter().position(&prunable) else {
                    break;
                };
                let removed = entries.remove(idx);
                Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
            }
        }

        let min_free_bytes = snapshot_min_free_bytes();
        if min_free_bytes > 0 {
            if let Some(mut free_bytes) = available_disk_space_bytes(sessions_dir) {
                while free_bytes < min_free_bytes {
                    let Some(idx) = entries.iter().position(&prunable) else {
                        break;
                    };
                    let removed = entries.remove(idx);
                    Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
                    free_bytes = available_disk_space_bytes(sessions_dir).unwrap_or(free_bytes);
                }
                if free_bytes < min_free_bytes && !oneshot_save {
                    return Err(AgentError::Io(format!(
                        "Session snapshot write blocked by disk guardrail: free={} bytes, required_min={} bytes (dir={})",
                        free_bytes,
                        min_free_bytes,
                        sessions_dir.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Get a serializable snapshot of the current session info.
    pub fn session_info(&self) -> SessionInfo {
        SessionInfo {
            session_id: self.session_id.clone(),
            model: self.current_model.clone(),
            personality: self.current_personality.clone(),
            message_count: self.messages.len(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Persist a JSON session snapshot to `<state_root>/sessions`.
    ///
    /// When `name_override` is provided, that value is used as the file stem.
    /// Otherwise the active `session_id` is used, and a non-empty session is
    /// also written to `sessions.db` so `session_search` can find it.
    pub fn persist_session_snapshot(
        &self,
        name_override: Option<&str>,
    ) -> Result<PathBuf, AgentError> {
        let stem = name_override
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(self.session_id.as_str());
        if is_oneshot_session_id(stem) {
            return Err(AgentError::Config(format!(
                "Session names of the form '{ONESHOT_SESSION_ID_PREFIX}<uuid>' are reserved for one-shot runs."
            )));
        }
        let path = write_session_snapshot(
            &self.state_root,
            stem,
            &self.session_info(),
            None,
            &self.messages,
        )?;
        if name_override.is_none() && !self.messages.is_empty() {
            if let Err(err) = SessionPersistence::new(&self.state_root).persist_session(
                &self.session_id,
                &self.messages,
                Some(&self.current_model),
                None,
                None,
                None,
            ) {
                tracing::warn!(
                    "session {} not written to sessions.db: {}",
                    self.session_id,
                    err
                );
            }
        }
        Ok(path)
    }
}

const ONESHOT_SESSION_ID_PREFIX: &str = "oneshot-";

/// Whether `id` names a one-shot run (`hermes -z`, `chat "<q>"`): the
/// `oneshot-` prefix and a UUID, as `save_oneshot_session` assigns. Other
/// names with the prefix stay ordinary sessions, as they were before.
pub fn is_oneshot_session_id(id: &str) -> bool {
    id.strip_prefix(ONESHOT_SESSION_ID_PREFIX)
        .is_some_and(|rest| Uuid::parse_str(rest).is_ok())
}

/// Whether a snapshot file belongs to a one-shot run.
pub fn is_oneshot_snapshot_path(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(is_oneshot_session_id)
}

/// The session id to save under after resuming the snapshot of session `id`.
/// A one-shot run is finished, so resuming one starts a new session and its
/// own snapshot and `sessions.db` row stay as saved.
pub fn resumed_session_id(id: &str) -> String {
    if is_oneshot_session_id(id) {
        Uuid::new_v4().to_string()
    } else {
        id.to_string()
    }
}

/// Write `<state_root>/sessions/<stem>.json`, then apply the snapshot limits.
fn write_session_snapshot(
    state_root: &Path,
    stem: &str,
    session_info: &SessionInfo,
    source: Option<&str>,
    messages: &[hermes_core::Message],
) -> Result<PathBuf, AgentError> {
    let sessions_dir = state_root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create sessions dir {}: {}",
            sessions_dir.display(),
            e
        ))
    })?;
    let path = sessions_dir.join(format!("{stem}.json"));
    let mut payload = serde_json::json!({
        "session_info": session_info,
        "messages": messages.iter().map(|m| {
            serde_json::json!({
                "role": format!("{:?}", m.role),
                "content": m.content.as_deref().unwrap_or(""),
                "tool_call_id": m.tool_call_id,
                "tool_calls": m.tool_calls,
                "name": m.name,
                "reasoning_content": m.reasoning_content,
            })
        }).collect::<Vec<_>>(),
    });
    if let Some(source) = source {
        payload["source"] = Value::from(source);
    }
    let json = serde_json::to_string_pretty(&payload)
        .map_err(|e| AgentError::Config(format!("Failed to serialize session snapshot: {e}")))?;
    std::fs::write(&path, json).map_err(|e| {
        AgentError::Io(format!(
            "Failed to write session snapshot {}: {}",
            path.display(),
            e
        ))
    })?;
    App::enforce_session_snapshot_guardrails(&sessions_dir, &path)?;
    Ok(path)
}

/// Save a finished one-shot run as a session: a `sessions.db` row (platform
/// `oneshot`, no title) for `session_search`, then a snapshot for `sessions list`.
/// Returns whether the snapshot was kept: it is not when only interactive
/// snapshots could be pruned to meet the snapshot limits.
pub fn save_oneshot_session(
    state_root: &Path,
    model: &str,
    personality: Option<String>,
    messages: &[hermes_core::Message],
) -> Result<bool, AgentError> {
    let session_id = format!("{ONESHOT_SESSION_ID_PREFIX}{}", Uuid::new_v4());
    SessionPersistence::new(state_root).persist_session(
        &session_id,
        messages,
        Some(model),
        Some("oneshot"),
        None,
        None,
    )?;
    let session_info = SessionInfo {
        session_id: session_id.clone(),
        model: model.to_string(),
        personality,
        message_count: messages.len(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let path = write_session_snapshot(
        state_root,
        &session_id,
        &session_info,
        Some("oneshot"),
        messages,
    )?;
    Ok(path.exists())
}
