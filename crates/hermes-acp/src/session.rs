//! ACP session state management.
//!
//! Maps ACP sessions to Hermes agent instances with persistence support.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// SessionPhase
// ---------------------------------------------------------------------------

/// Lifecycle phase of an ACP session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// Session created, awaiting first prompt.
    #[default]
    Created,
    /// Session is actively processing a prompt.
    Active,
    /// Session is idle, waiting for the next prompt.
    Idle,
    /// Session completed normally.
    Completed,
    /// Session was cancelled by the client.
    Cancelled,
    /// Session encountered an unrecoverable error.
    Failed,
}

tokio::task_local! {
    /// The arrival number of the prompt a server task is running (from
    /// [`SessionManager::prompt_received`]), so the handler's turn knows
    /// which cancels came after it. Unset when a handler is called directly.
    pub static PROMPT_SEQ: Option<u64>;
}

/// What [`SessionManager::begin_turn`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStart {
    /// The turn runs under this number; finish it with
    /// [`SessionManager::finish_turn`].
    Started(u64),
    /// Another turn of the session is running.
    Busy,
    /// A cancel came after the prompt arrived: the turn did not start.
    Cancelled,
    /// No such session.
    Missing,
}

/// What [`SessionManager::next_queued_or_finish`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueuedOrFinished {
    /// Run this queued prompt in the same turn.
    Queued(String),
    /// The turn is finished; `cancelled` when a cancel covers it.
    Finished { cancelled: bool },
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

/// Tracks per-session state for an ACP-managed Hermes agent.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub cwd: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub api_mode: Option<String>,
    pub base_url: Option<String>,
    pub profile: Option<String>,
    pub home: Option<String>,
    pub title: Option<String>,
    pub phase: SessionPhase,
    pub history: Vec<Value>,
    pub mode: Option<String>,
    pub config_options: HashMap<String, String>,
    /// Prompts queued by `/queue` to run after the current prompt completes.
    pub queued_prompts: Vec<String>,
    /// Last interrupted prompt text that `/steer` can replay with guidance.
    pub interrupted_prompt_text: Option<String>,
    /// Prompts received for this session so far; each prompt's number.
    pub prompts_received: u64,
    /// A `session/cancel` covers every prompt numbered up to this.
    pub cancel_through: u64,
    /// The number of the running turn's prompt. Kept apart from `phase`,
    /// which a cancel sets whilst the turn still runs.
    pub turn_seq: Option<u64>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Total prompt tokens across all turns.
    pub total_prompt_tokens: u64,
    /// Total completion tokens across all turns.
    pub total_completion_tokens: u64,
}

/// Public metadata update for ACP sessions.
///
/// Optional fields intentionally preserve existing values when omitted. This
/// mirrors upstream's `update_session_meta(..., model=None)` COALESCE behavior
/// without exposing storage internals to callers.
#[derive(Debug, Clone, Default)]
pub struct SessionMetaUpdate {
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub api_mode: Option<String>,
    pub base_url: Option<String>,
    pub profile: Option<String>,
    pub home: Option<String>,
    pub title: Option<String>,
    pub config_options: HashMap<String, String>,
}

impl SessionState {
    pub fn new(session_id: String, cwd: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            session_id,
            cwd,
            model: None,
            provider: None,
            api_mode: None,
            base_url: None,
            profile: None,
            home: None,
            title: None,
            phase: SessionPhase::Created,
            history: Vec::new(),
            mode: None,
            config_options: HashMap::new(),
            queued_prompts: Vec::new(),
            interrupted_prompt_text: None,
            prompts_received: 0,
            cancel_through: 0,
            turn_seq: None,
            created_at: now,
            updated_at: now,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }

    /// Finish the turn numbered `seq`, if it is the running one; whether a
    /// cancel covers it.
    fn finish_turn(&mut self, seq: u64, phase: SessionPhase) -> bool {
        if self.turn_seq == Some(seq) {
            self.turn_seq = None;
            self.phase = phase;
            self.touch();
        }
        seq <= self.cancel_through
    }
}

// ---------------------------------------------------------------------------
// SessionInfo (lightweight view for listing)
// ---------------------------------------------------------------------------

/// Lightweight session info returned by `list_sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub cwd: String,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    pub phase: SessionPhase,
    pub history_len: usize,
    pub created_at: u64,
    pub updated_at: u64,
}

impl From<&SessionState> for SessionInfo {
    fn from(s: &SessionState) -> Self {
        Self {
            session_id: s.session_id.clone(),
            cwd: s.cwd.clone(),
            model: s.model.clone(),
            profile: s.profile.clone(),
            home: s.home.clone(),
            phase: s.phase,
            history_len: s.history.len(),
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// SessionManager
// ---------------------------------------------------------------------------

/// Thread-safe manager for ACP sessions.
///
/// Sessions are held in-memory for fast access. A persistence callback can be
/// provided to sync state to a database or disk.
type PersistCallback = dyn Fn(&SessionState) + Send + Sync;

pub struct SessionManager {
    sessions: Mutex<HashMap<String, SessionState>>,
    on_persist: Option<Box<PersistCallback>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            on_persist: None,
        }
    }

    /// Set a callback invoked whenever a session is persisted.
    pub fn with_persist_callback(
        mut self,
        cb: impl Fn(&SessionState) + Send + Sync + 'static,
    ) -> Self {
        self.on_persist = Some(Box::new(cb));
        self
    }

    /// Create a new session with a unique ID.
    pub fn create_session(&self, cwd: &str) -> SessionState {
        self.create_session_with_meta(cwd, SessionMetaUpdate::default())
    }

    /// Create a new session with initial metadata.
    pub fn create_session_with_meta(&self, cwd: &str, update: SessionMetaUpdate) -> SessionState {
        let session_id = uuid::Uuid::new_v4().to_string();
        let mut state = SessionState::new(session_id.clone(), cwd.to_string());
        apply_session_meta(&mut state, update);
        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.insert(session_id.clone(), state.clone());
        }
        self.persist(&state);
        tracing::info!("Created ACP session {} (cwd={})", session_id, cwd);
        state
    }

    /// Get a session by ID, or `None` if not found.
    pub fn get_session(&self, session_id: &str) -> Option<SessionState> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session_id).cloned()
    }

    /// Update a session's working directory.
    pub fn update_cwd(&self, session_id: &str, cwd: &str) -> Option<SessionState> {
        self.update_session_meta(
            session_id,
            SessionMetaUpdate {
                cwd: Some(cwd.to_string()),
                ..SessionMetaUpdate::default()
            },
        )
    }

    /// Update a session's model identifier.
    pub fn update_model(&self, session_id: &str, model: &str) -> Option<SessionState> {
        self.update_session_meta(
            session_id,
            SessionMetaUpdate {
                model: Some(model.to_string()),
                ..SessionMetaUpdate::default()
            },
        )
    }

    /// Update session metadata through the public, lock-protected manager path.
    pub fn update_session_meta(
        &self,
        session_id: &str,
        update: SessionMetaUpdate,
    ) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            apply_session_meta(state, update);
            state.touch();
            let cloned = state.clone();
            drop(sessions);
            self.persist(&cloned);
            Some(cloned)
        } else {
            None
        }
    }

    /// Update a session's mode identifier.
    pub fn update_mode(&self, session_id: &str, mode: &str) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.mode = Some(mode.to_string());
            state.touch();
            let cloned = state.clone();
            drop(sessions);
            self.persist(&cloned);
            Some(cloned)
        } else {
            None
        }
    }

    /// Set or replace a session config option.
    pub fn set_config_option(
        &self,
        session_id: &str,
        key: &str,
        value: &str,
    ) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state
                .config_options
                .insert(key.to_string(), value.to_string());
            state.touch();
            let cloned = state.clone();
            drop(sessions);
            self.persist(&cloned);
            Some(cloned)
        } else {
            None
        }
    }

    /// Update a session's phase.
    pub fn set_phase(&self, session_id: &str, phase: SessionPhase) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.phase = phase;
            state.touch();
        }
    }

    /// Number the next prompt for the session, in arrival order (`None` for
    /// a session that does not exist).
    pub fn prompt_received(&self, session_id: &str) -> Option<u64> {
        let mut sessions = self.sessions.lock().unwrap();
        let state = sessions.get_mut(session_id)?;
        state.prompts_received += 1;
        Some(state.prompts_received)
    }

    /// Start a turn for the prompt numbered `seq` unless a turn is running,
    /// checked and set under one lock. A prompt with no number (a handler
    /// called directly) is numbered now. A prompt that a later cancel covers
    /// does not start or queue, whether or not another turn runs.
    pub fn begin_turn(&self, session_id: &str, seq: Option<u64>) -> TurnStart {
        self.begin_turn_inner(session_id, seq, None)
    }

    /// [`Self::begin_turn`], queueing `prompt` for the running turn when the
    /// session is busy, under the same lock: queued separately, it could land
    /// after the running turn's last [`Self::next_queued_or_finish`] and wait
    /// behind a later prompt.
    pub fn begin_turn_or_queue(
        &self,
        session_id: &str,
        seq: Option<u64>,
        prompt: &str,
    ) -> TurnStart {
        self.begin_turn_inner(session_id, seq, Some(prompt))
    }

    fn begin_turn_inner(
        &self,
        session_id: &str,
        seq: Option<u64>,
        queue: Option<&str>,
    ) -> TurnStart {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get_mut(session_id) else {
            return TurnStart::Missing;
        };
        let seq = seq.unwrap_or_else(|| {
            state.prompts_received += 1;
            state.prompts_received
        });
        state.touch();
        // Before the busy check, so a cancelled prompt is never queued into
        // a later prompt's turn.
        if seq <= state.cancel_through {
            if state.turn_seq.is_none() {
                state.phase = SessionPhase::Idle;
            }
            return TurnStart::Cancelled;
        }
        if state.turn_seq.is_some() {
            if let Some(prompt) = queue.map(str::trim).filter(|p| !p.is_empty()) {
                state.queued_prompts.push(prompt.to_string());
            }
            return TurnStart::Busy;
        }
        state.turn_seq = Some(seq);
        state.phase = SessionPhase::Active;
        TurnStart::Started(seq)
    }

    /// Finish the turn numbered `seq`, if it is the running one, leaving the
    /// session in `phase`. Returns whether a cancel covers it, read under
    /// the same lock, so a cancel cannot fall between the two.
    pub fn finish_turn(&self, session_id: &str, seq: u64, phase: SessionPhase) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get_mut(session_id) else {
            return false;
        };
        state.finish_turn(seq, phase)
    }

    /// The next queued prompt for the running turn `seq`, or, when none is
    /// queued or a cancel covers the turn, the turn finished in `phase`. One
    /// lock covers both, so a prompt queued after the last pop is still run in
    /// this turn instead of waiting behind the next prompt.
    pub fn next_queued_or_finish(
        &self,
        session_id: &str,
        seq: u64,
        phase: SessionPhase,
    ) -> QueuedOrFinished {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get_mut(session_id) else {
            return QueuedOrFinished::Finished { cancelled: false };
        };
        // A cancelled turn leaves queued prompts for the next turn.
        if seq > state.cancel_through && !state.queued_prompts.is_empty() {
            state.touch();
            return QueuedOrFinished::Queued(state.queued_prompts.remove(0));
        }
        QueuedOrFinished::Finished {
            cancelled: state.finish_turn(seq, phase),
        }
    }

    /// Mark the session cancelled, covering every prompt received so far.
    /// Returns the session as it was before.
    pub fn request_cancel(&self, session_id: &str) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        let state = sessions.get_mut(session_id)?;
        let before = state.clone();
        state.phase = SessionPhase::Cancelled;
        state.cancel_through = state.prompts_received;
        state.touch();
        Some(before)
    }

    /// Whether a cancel covers the running turn.
    pub fn cancel_pending(&self, session_id: &str) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session_id).is_some_and(|state| {
            state
                .turn_seq
                .is_some_and(|seq| seq <= state.cancel_through)
        })
    }

    /// Update session history.
    pub fn set_history(&self, session_id: &str, history: Vec<Value>) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.history = history;
            state.touch();
        }
    }

    /// Queue a prompt to execute after the current turn.
    pub fn push_queued_prompt(&self, session_id: &str, prompt: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            let prompt = prompt.trim();
            if !prompt.is_empty() {
                state.queued_prompts.push(prompt.to_string());
                state.touch();
            }
            return true;
        }
        false
    }

    /// Queue a prompt to execute before any already queued.
    pub fn push_queued_prompt_front(&self, session_id: &str, prompt: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            let prompt = prompt.trim();
            if !prompt.is_empty() {
                state.queued_prompts.insert(0, prompt.to_string());
                state.touch();
            }
            return true;
        }
        false
    }

    /// Drain queued prompts in FIFO order.
    pub fn take_queued_prompts(&self, session_id: &str) -> Vec<String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.touch();
            return std::mem::take(&mut state.queued_prompts);
        }
        Vec::new()
    }

    /// Store an interrupted prompt for subsequent `/steer` replay.
    pub fn set_interrupted_prompt_text(&self, session_id: &str, prompt: Option<String>) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.interrupted_prompt_text = prompt
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            state.touch();
            return true;
        }
        false
    }

    /// Take and clear the interrupted prompt text.
    pub fn take_interrupted_prompt_text(&self, session_id: &str) -> Option<String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.touch();
            return state.interrupted_prompt_text.take();
        }
        None
    }

    /// Fork a session — deep-copy history into a new session.
    pub fn fork_session(&self, session_id: &str, cwd: &str) -> Option<SessionState> {
        self.fork_session_with_meta(session_id, cwd, SessionMetaUpdate::default())
    }

    /// Fork a session with optional metadata overrides.
    pub fn fork_session_with_meta(
        &self,
        session_id: &str,
        cwd: &str,
        update: SessionMetaUpdate,
    ) -> Option<SessionState> {
        let original = self.get_session(session_id)?;
        let new_id = uuid::Uuid::new_v4().to_string();
        let mut new_state = SessionState::new(new_id.clone(), cwd.to_string());
        new_state.model = original.model.clone();
        new_state.provider = original.provider.clone();
        new_state.api_mode = original.api_mode.clone();
        new_state.base_url = original.base_url.clone();
        new_state.profile = original.profile.clone();
        new_state.home = original.home.clone();
        new_state.title = original.title.clone();
        new_state.mode = original.mode.clone();
        new_state.config_options = original.config_options.clone();
        new_state.history = original.history.clone();
        apply_session_meta(&mut new_state, update);
        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.insert(new_id.clone(), new_state.clone());
        }
        self.persist(&new_state);
        tracing::info!("Forked ACP session {} -> {}", session_id, new_id);
        Some(new_state)
    }

    /// Remove a session.
    pub fn remove_session(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.remove(session_id).is_some()
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.lock().unwrap();
        sessions.values().map(SessionInfo::from).collect()
    }

    /// List all session states.
    pub fn list_session_states(&self) -> Vec<SessionState> {
        let sessions = self.sessions.lock().unwrap();
        sessions.values().cloned().collect()
    }

    /// Persist a session state via the registered callback.
    pub fn save_session(&self, session_id: &str) {
        if let Some(state) = self.get_session(session_id) {
            self.persist(&state);
        }
    }

    /// Accumulate token usage for a session.
    pub fn add_usage(&self, session_id: &str, prompt_tokens: u64, completion_tokens: u64) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.total_prompt_tokens += prompt_tokens;
            state.total_completion_tokens += completion_tokens;
            state.touch();
        }
    }

    fn persist(&self, state: &SessionState) {
        if let Some(ref cb) = self.on_persist {
            cb(state);
        }
    }
}

fn normalize_meta_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn apply_session_meta(state: &mut SessionState, update: SessionMetaUpdate) {
    if let Some(cwd) = update.cwd.and_then(normalize_meta_string) {
        state.cwd = cwd;
    }
    if let Some(model) = update.model.and_then(normalize_meta_string) {
        state.model = Some(model);
    }
    if let Some(provider) = update.provider.and_then(normalize_meta_string) {
        state.provider = Some(provider);
    }
    if let Some(api_mode) = update.api_mode.and_then(normalize_meta_string) {
        state.api_mode = Some(api_mode);
    }
    if let Some(base_url) = update.base_url.and_then(normalize_meta_string) {
        state.base_url = Some(base_url);
    }
    if let Some(profile) = update.profile.and_then(normalize_meta_string) {
        state.profile = Some(profile);
    }
    if let Some(home) = update.home.and_then(normalize_meta_string) {
        state.home = Some(home);
    }
    if let Some(title) = update.title.and_then(normalize_meta_string) {
        state.title = Some(title);
    }
    for (key, value) in update.config_options {
        let key = key.trim();
        if !key.is_empty() {
            state.config_options.insert(key.to_string(), value);
        }
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_turn_of_a_session_starts_at_a_time() {
        let sessions = SessionManager::new();
        let id = sessions.create_session(".").session_id;
        let first = sessions.prompt_received(&id).unwrap();
        assert_eq!(
            sessions.begin_turn(&id, Some(first)),
            TurnStart::Started(first)
        );
        let second = sessions.prompt_received(&id).unwrap();
        assert_eq!(sessions.begin_turn(&id, Some(second)), TurnStart::Busy);
        // A cancel marks the phase, but the turn still owns the session,
        // and only its own task can finish it.
        sessions.request_cancel(&id);
        assert_eq!(sessions.begin_turn(&id, None), TurnStart::Busy);
        assert!(sessions.cancel_pending(&id));
        sessions.finish_turn(&id, second, SessionPhase::Failed);
        assert_eq!(sessions.begin_turn(&id, None), TurnStart::Busy);
        assert!(sessions.finish_turn(&id, first, SessionPhase::Idle));
        assert!(!sessions.cancel_pending(&id));
        assert!(matches!(
            sessions.begin_turn(&id, None),
            TurnStart::Started(_)
        ));
        assert_eq!(
            sessions.begin_turn("no-such-session", None),
            TurnStart::Missing
        );
    }

    #[test]
    fn a_cancel_covers_the_prompts_received_before_it() {
        let sessions = SessionManager::new();
        let id = sessions.create_session(".").session_id;
        // A, a cancel, then B; B's task starts first.
        let a = sessions.prompt_received(&id).unwrap();
        sessions.request_cancel(&id);
        let b = sessions.prompt_received(&id).unwrap();
        assert_eq!(sessions.begin_turn(&id, Some(b)), TurnStart::Started(b));
        assert!(!sessions.cancel_pending(&id));
        // A is refused whilst B still runs, and B keeps its turn.
        assert_eq!(sessions.begin_turn(&id, Some(a)), TurnStart::Cancelled);
        assert_eq!(
            sessions.get_session(&id).unwrap().phase,
            SessionPhase::Active
        );
        assert_eq!(sessions.begin_turn(&id, None), TurnStart::Busy);
        assert!(!sessions.finish_turn(&id, b, SessionPhase::Idle));
        assert_eq!(sessions.begin_turn(&id, Some(a)), TurnStart::Cancelled);

        // A cancel during a turn covers it, read with the finish.
        let c = sessions.prompt_received(&id).unwrap();
        assert_eq!(sessions.begin_turn(&id, Some(c)), TurnStart::Started(c));
        sessions.request_cancel(&id);
        assert!(sessions.finish_turn(&id, c, SessionPhase::Idle));
        // A cancel with nothing received covers nothing later.
        sessions.request_cancel(&id);
        let d = sessions.prompt_received(&id).unwrap();
        assert_eq!(sessions.begin_turn(&id, Some(d)), TurnStart::Started(d));
    }

    #[test]
    fn a_prompt_queued_whilst_a_turn_drains_runs_in_that_turn() {
        let sessions = SessionManager::new();
        let id = sessions.create_session(".").session_id;
        let a = sessions.prompt_received(&id).unwrap();
        assert_eq!(sessions.begin_turn(&id, Some(a)), TurnStart::Started(a));
        // Queued under the lock that saw the turn busy.
        assert_eq!(
            sessions.begin_turn_or_queue(&id, None, " b "),
            TurnStart::Busy
        );
        assert_eq!(
            sessions.next_queued_or_finish(&id, a, SessionPhase::Idle),
            QueuedOrFinished::Queued("b".into())
        );
        // The turn stays A's until its queue is empty, so C still queues
        // into it rather than starting a turn of its own.
        assert_eq!(
            sessions.begin_turn_or_queue(&id, None, "c"),
            TurnStart::Busy
        );
        assert_eq!(
            sessions.next_queued_or_finish(&id, a, SessionPhase::Idle),
            QueuedOrFinished::Queued("c".into())
        );
        assert_eq!(
            sessions.next_queued_or_finish(&id, a, SessionPhase::Idle),
            QueuedOrFinished::Finished { cancelled: false }
        );
        assert_eq!(sessions.get_session(&id).unwrap().phase, SessionPhase::Idle);
        // Once the turn has finished, a prompt starts its own turn and is
        // not queued.
        let d = sessions.prompt_received(&id).unwrap();
        assert_eq!(
            sessions.begin_turn_or_queue(&id, Some(d), "d"),
            TurnStart::Started(d)
        );
        assert!(sessions.take_queued_prompts(&id).is_empty());
    }

    #[test]
    fn a_cancelled_turn_finishes_and_leaves_its_queue_for_the_next() {
        let sessions = SessionManager::new();
        let id = sessions.create_session(".").session_id;
        let a = sessions.prompt_received(&id).unwrap();
        assert_eq!(sessions.begin_turn(&id, Some(a)), TurnStart::Started(a));
        sessions.push_queued_prompt(&id, "b");
        sessions.request_cancel(&id);
        assert_eq!(
            sessions.next_queued_or_finish(&id, a, SessionPhase::Idle),
            QueuedOrFinished::Finished { cancelled: true }
        );
        assert_eq!(sessions.take_queued_prompts(&id), vec!["b".to_string()]);
    }

    #[test]
    fn test_create_and_get_session() {
        let mgr = SessionManager::new();
        let state = mgr.create_session("/tmp");
        assert_eq!(state.cwd, "/tmp");
        assert_eq!(state.phase, SessionPhase::Created);

        let got = mgr.get_session(&state.session_id).unwrap();
        assert_eq!(got.session_id, state.session_id);
    }

    #[test]
    fn test_fork_session() {
        let mgr = SessionManager::new();
        let original = mgr.create_session("/project");
        mgr.set_history(
            &original.session_id,
            vec![serde_json::json!({"role": "user", "content": "hello"})],
        );

        let forked = mgr.fork_session(&original.session_id, "/other").unwrap();
        assert_ne!(forked.session_id, original.session_id);
        assert_eq!(forked.cwd, "/other");
        assert_eq!(forked.history.len(), 1);
    }

    #[test]
    fn test_list_sessions() {
        let mgr = SessionManager::new();
        mgr.create_session("/a");
        mgr.create_session("/b");
        assert_eq!(mgr.list_sessions().len(), 2);
    }

    #[test]
    fn test_remove_session() {
        let mgr = SessionManager::new();
        let state = mgr.create_session("/tmp");
        assert!(mgr.remove_session(&state.session_id));
        assert!(mgr.get_session(&state.session_id).is_none());
    }

    #[test]
    fn test_update_model_mode_and_config_option() {
        let mgr = SessionManager::new();
        let state = mgr.create_session("/tmp");
        let sid = state.session_id;

        mgr.update_model(&sid, "openai:gpt-4o");
        mgr.update_mode(&sid, "code");
        mgr.set_config_option(&sid, "temperature", "0.2");

        let updated = mgr.get_session(&sid).expect("session should exist");
        assert_eq!(updated.model.as_deref(), Some("openai:gpt-4o"));
        assert_eq!(updated.mode.as_deref(), Some("code"));
        assert_eq!(
            updated
                .config_options
                .get("temperature")
                .map(String::as_str),
            Some("0.2")
        );
    }

    #[test]
    fn update_session_meta_preserves_model_when_omitted() {
        let mgr = SessionManager::new();
        let state = mgr.create_session("/tmp");
        let sid = state.session_id;

        mgr.update_model(&sid, "nous:gpt-5.4");
        let updated = mgr
            .update_session_meta(
                &sid,
                SessionMetaUpdate {
                    cwd: Some("/workspace".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("session exists");

        assert_eq!(updated.cwd, "/workspace");
        assert_eq!(updated.model.as_deref(), Some("nous:gpt-5.4"));
        let stored = mgr.get_session(&sid).expect("session exists");
        assert_eq!(stored.model.as_deref(), Some("nous:gpt-5.4"));
    }

    #[test]
    fn update_session_meta_merges_fields_and_persists_once() {
        use std::sync::{Arc, Mutex};

        let persisted = Arc::new(Mutex::new(Vec::<SessionState>::new()));
        let persisted_for_cb = persisted.clone();
        let mgr = SessionManager::new().with_persist_callback(move |state| {
            persisted_for_cb
                .lock()
                .expect("persisted lock")
                .push(state.clone());
        });
        let state = mgr.create_session("/tmp");
        let sid = state.session_id;
        persisted.lock().expect("persisted lock").clear();

        let mut config_options = HashMap::new();
        config_options.insert("temperature".to_string(), "0.2".to_string());
        config_options.insert("".to_string(), "ignored".to_string());
        let updated = mgr
            .update_session_meta(
                &sid,
                SessionMetaUpdate {
                    cwd: Some("/repo".to_string()),
                    model: Some("openai:gpt-4o".to_string()),
                    provider: Some("openai".to_string()),
                    api_mode: Some("responses".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    config_options,
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("session exists");

        assert_eq!(updated.cwd, "/repo");
        assert_eq!(updated.model.as_deref(), Some("openai:gpt-4o"));
        assert_eq!(updated.provider.as_deref(), Some("openai"));
        assert_eq!(updated.api_mode.as_deref(), Some("responses"));
        assert_eq!(
            updated.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            updated
                .config_options
                .get("temperature")
                .map(String::as_str),
            Some("0.2")
        );
        assert!(!updated.config_options.contains_key(""));

        let persisted = persisted.lock().expect("persisted lock");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].session_id, sid);
        assert_eq!(persisted[0].cwd, "/repo");
    }

    #[test]
    fn update_session_meta_sets_title_and_persists_once() {
        use std::sync::{Arc, Mutex};

        let persisted = Arc::new(Mutex::new(Vec::<SessionState>::new()));
        let persisted_for_cb = persisted.clone();
        let mgr = SessionManager::new().with_persist_callback(move |state| {
            persisted_for_cb
                .lock()
                .expect("persisted lock")
                .push(state.clone());
        });
        let state = mgr.create_session("/tmp");
        let sid = state.session_id;
        persisted.lock().expect("persisted lock").clear();

        let updated = mgr
            .update_session_meta(
                &sid,
                SessionMetaUpdate {
                    title: Some("  My branch  ".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("session exists");

        assert_eq!(updated.title.as_deref(), Some("My branch"));
        let stored = mgr.get_session(&sid).expect("session exists");
        assert_eq!(stored.title.as_deref(), Some("My branch"));

        let persisted = persisted.lock().expect("persisted lock");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].title.as_deref(), Some("My branch"));
    }

    #[test]
    fn profile_home_metadata_flows_through_create_update_and_fork() {
        let mgr = SessionManager::new();
        let state = mgr.create_session_with_meta(
            "/workspace",
            SessionMetaUpdate {
                profile: Some("work".to_string()),
                home: Some("/profiles/work".to_string()),
                title: Some("Original title".to_string()),
                provider: Some("openrouter".to_string()),
                ..SessionMetaUpdate::default()
            },
        );
        assert_eq!(state.profile.as_deref(), Some("work"));
        assert_eq!(state.home.as_deref(), Some("/profiles/work"));
        assert_eq!(state.title.as_deref(), Some("Original title"));

        let updated = mgr
            .update_session_meta(
                &state.session_id,
                SessionMetaUpdate {
                    cwd: Some("/workspace/repo".to_string()),
                    profile: Some("research".to_string()),
                    home: Some("/profiles/research".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("session exists");
        assert_eq!(updated.cwd, "/workspace/repo");
        assert_eq!(updated.profile.as_deref(), Some("research"));
        assert_eq!(updated.home.as_deref(), Some("/profiles/research"));
        assert_eq!(updated.provider.as_deref(), Some("openrouter"));

        let forked = mgr
            .fork_session_with_meta(
                &state.session_id,
                "/fork",
                SessionMetaUpdate {
                    profile: Some("scratch".to_string()),
                    home: Some("/profiles/scratch".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("forked");
        assert_eq!(forked.cwd, "/fork");
        assert_eq!(forked.profile.as_deref(), Some("scratch"));
        assert_eq!(forked.home.as_deref(), Some("/profiles/scratch"));
        assert_eq!(forked.provider.as_deref(), Some("openrouter"));
        assert_eq!(forked.title.as_deref(), Some("Original title"));
    }

    #[test]
    fn update_session_meta_missing_session_is_noop() {
        use std::sync::{Arc, Mutex};

        let persisted = Arc::new(Mutex::new(Vec::<SessionState>::new()));
        let persisted_for_cb = persisted.clone();
        let mgr = SessionManager::new().with_persist_callback(move |state| {
            persisted_for_cb
                .lock()
                .expect("persisted lock")
                .push(state.clone());
        });
        assert!(mgr
            .update_session_meta(
                "missing",
                SessionMetaUpdate {
                    cwd: Some("/repo".to_string()),
                    model: Some("openai:gpt-4o".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .is_none());
        assert!(persisted.lock().expect("persisted lock").is_empty());
    }
}
