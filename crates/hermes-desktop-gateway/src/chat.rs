//! Chat for Hermes Desktop: `session.create`, `prompt.submit` and the reply streamed back as
//! `message.start`, `message.delta` and `message.complete` events, saved to the sessions
//! database the CLI uses.
//!
//! Turns run the configured model with no tools, so no request can run a command or touch a
//! file beyond what a CLI turn reads from its working directory (context references, which
//! stay confined to it). Tool calls and their approval prompts come with a later change.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hermes_agent::agent_loop::ToolRegistry;
use hermes_agent::{AgentLoop, SessionPersistence};
use hermes_config::GatewayConfig;
use hermes_core::{AgentResult, Message, MessageRole, StreamChunk};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// Frames for one connection's socket.
pub(crate) type Outbox = UnboundedSender<Outgoing>;

/// A frame for the connection to write and when it was queued. A turn's last event carries
/// the turn's slot, so the slot frees once that event is written or dropped with a closed
/// connection.
pub(crate) struct Outgoing {
    pub(crate) frame: Value,
    pub(crate) queued: Instant,
    _slot: Option<OwnedSemaphorePermit>,
}

impl From<Value> for Outgoing {
    fn from(frame: Value) -> Self {
        Self {
            frame,
            queued: Instant::now(),
            _slot: None,
        }
    }
}

/// A JSON-RPC error: code and message.
pub(crate) type RpcError = (i64, String);

pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
// Upstream's gateway codes (tui_gateway): 4000 invalid params, 4001 unknown session,
// 4009 a turn already running, 4012 no text, 4090 no free turn slot.
const INVALID_PARAMS: i64 = 4000;
const SESSION_NOT_FOUND: i64 = 4001;
const SESSION_BUSY: i64 = 4009;
const TEXT_REQUIRED: i64 = 4012;
const NO_TURN_SLOT: i64 = 4090;

/// Most turns running or with events not yet written, across the gateway; it bounds the
/// replies held in memory, as a turn keeps its slot until its last event is written.
const MAX_TURNS: usize = 8;

/// How long a frame may wait, from being queued until it is written, before the connection
/// closes. A turn's events are all queued by its end, so its slot frees within this time of
/// the turn's end however slowly the client reads.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest session id a request may name.
const MAX_SESSION_ID: usize = 128;

/// The model, the sessions database and the live sessions.
pub struct Chat {
    config: GatewayConfig,
    home: PathBuf,
    // shortcut: sessions live until the gateway exits (about 100 bytes each, created only by
    // a token holder); dropping idle ones comes with session close and resume.
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
    turns: Arc<Semaphore>,
    write_timeout: Duration,
}

/// The reply streamed so far and its length in UTF-16 units, the desktop's offsets.
#[derive(Default)]
struct Streamed {
    text: String,
    units: usize,
}

#[derive(Default)]
struct LiveSession {
    /// Last event number sent for this session; the desktop orders and deduplicates by it.
    seq: AtomicU64,
    busy: AtomicBool,
}

/// Clears a session's busy flag when its turn ends, panics included.
struct TurnGuard(Arc<LiveSession>);

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

impl Chat {
    /// Chat with `config`'s model; sessions are saved under `config`'s home.
    pub fn new(config: GatewayConfig) -> Self {
        let home = config
            .home_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(hermes_config::hermes_home);
        Self {
            config,
            home,
            sessions: Mutex::default(),
            turns: Arc::new(Semaphore::new(MAX_TURNS)),
            write_timeout: WRITE_TIMEOUT,
        }
    }

    pub(crate) fn write_timeout(&self) -> Duration {
        self.write_timeout
    }

    /// How long a frame may wait, from being queued until it is written, before the
    /// connection closes (30 seconds unless set).
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    fn model(&self) -> Option<&str> {
        self.config
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }

    /// Answers one JSON-RPC request whose `params` are absent (null) or an object; `outbox`
    /// receives the events a turn streams later.
    pub(crate) fn call(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        outbox: &Outbox,
    ) -> Result<Value, RpcError> {
        let empty = Map::new();
        let named = match params {
            Value::Object(map) => Ok(map),
            Value::Null => Ok(&empty),
            _ => Err((INVALID_PARAMS, "params must be an object".to_string())),
        };
        match method {
            "setup.runtime_check" => named.map(|_| self.runtime_check()),
            "session.create" => named.map(|_| self.create()),
            "prompt.submit" => self.submit(named?, outbox),
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
        }
    }

    /// Whether a turn can reach a model: one is configured and its provider has a key, or
    /// needs none (a local endpoint). The desktop keeps its set-up screen up until this is ok.
    fn runtime_check(&self) -> Value {
        let Some(model) = self.model() else {
            return json!({"ok": false, "error": "No model is configured: run `hermes model`."});
        };
        let route = hermes_provider_runtime::provider_runtime_diagnostic(&self.config, model);
        if route.api_key_present || route.local_no_key_allowed {
            json!({"ok": true, "provider": route.requested_provider, "model": route.model})
        } else {
            json!({
                "ok": false,
                "provider": route.requested_provider,
                "error": format!("No API key for provider '{}'.", route.requested_provider),
            })
        }
    }

    fn create(&self) -> Value {
        let id = uuid::Uuid::new_v4().to_string();
        self.lock_sessions()
            .insert(id.clone(), Arc::new(LiveSession::default()));
        let cwd = std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        json!({
            "session_id": id,
            "stored_session_id": id,
            "message_count": 0,
            "messages": [],
            "info": {"model": self.model().unwrap_or_default(), "tools": {}, "skills": {}, "cwd": cwd},
        })
    }

    fn submit(
        self: &Arc<Self>,
        params: &Map<String, Value>,
        outbox: &Outbox,
    ) -> Result<Value, RpcError> {
        // The desktop sends a submission id first and, on this refusal, the prompt without
        // one; upstream's `hermes serve` answers the same way, as it keeps no submission log.
        if params.contains_key("submission_id") {
            return Err((
                INVALID_PARAMS,
                "invalid params for prompt.submit: submission_id is not accepted by this backend"
                    .into(),
            ));
        }
        let Some(Value::String(id)) = params.get("session_id") else {
            return Err((INVALID_PARAMS, "session_id must be a string".into()));
        };
        let text = match params.get("text") {
            Some(Value::String(text)) if !text.trim().is_empty() => text.clone(),
            None | Some(Value::String(_)) => return Err((TEXT_REQUIRED, "text required".into())),
            Some(_) => return Err((INVALID_PARAMS, "text must be a string".into())),
        };
        let Some(session) = self.lock_sessions().get(id).cloned() else {
            return Err((SESSION_NOT_FOUND, "session not found".into()));
        };
        let Ok(slot) = Arc::clone(&self.turns).try_acquire_owned() else {
            return Err((
                NO_TURN_SLOT,
                format!("{MAX_TURNS} turns are running; send again when one ends"),
            ));
        };
        if session.busy.swap(true, Ordering::AcqRel) {
            return Err((SESSION_BUSY, "session busy".into()));
        }
        let guard = TurnGuard(session);
        let turn = Turn {
            chat: Arc::clone(self),
            id: id.clone(),
            turn_id: uuid::Uuid::new_v4().simple().to_string(),
            outbox: outbox.clone(),
            slot: Some(slot),
        };
        tokio::spawn(async move {
            turn.run(text, &guard.0).await;
        });
        Ok(json!({"status": "streaming"}))
    }

    /// Runs `work` on the sessions database on the blocking pool. A turn's store calls block,
    /// and on the turn's thread they would hold its queued events past their write deadline.
    async fn with_store<T: Send + 'static>(
        &self,
        work: impl FnOnce(SessionPersistence) -> T + Send + 'static,
    ) -> Result<T, String> {
        let home = self.home.clone();
        tokio::task::spawn_blocking(move || work(SessionPersistence::new(home)))
            .await
            .map_err(|e| e.to_string())
    }

    fn lock_sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<LiveSession>>> {
        // A panic while the map is held cannot leave it half-updated (single inserts and
        // reads), so a poisoned lock is still safe to use.
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The user and assistant messages saved for `id`, oldest first, each with its position
    /// among the session's active rows as `id`. `None` when `id` is not a session id.
    pub(crate) fn history(&self, id: &str) -> Option<Result<Vec<Value>, String>> {
        if !valid_session_id(id) {
            return None;
        }
        let rows = match SessionPersistence::new(&self.home).load_session(id) {
            Ok(rows) => rows,
            Err(err) => return Some(Err(err.to_string())),
        };
        let messages = rows
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                let role = match message.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    _ => return None,
                };
                Some(json!({
                    "id": index + 1,
                    "session_id": id,
                    "role": role,
                    "content": message.content.as_deref().unwrap_or_default(),
                }))
            })
            .collect();
        Some(Ok(messages))
    }
}

/// Session ids are this gateway's UUIDs or the CLI's; anything else names no session.
fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SESSION_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

struct Turn {
    chat: Arc<Chat>,
    id: String,
    turn_id: String,
    outbox: Outbox,
    /// The turn's slot, sent with its last event.
    slot: Option<OwnedSemaphorePermit>,
}

impl Turn {
    fn emit(
        &self,
        session: &LiveSession,
        kind: &str,
        payload: Value,
        slot: Option<OwnedSemaphorePermit>,
    ) {
        let seq = session.seq.fetch_add(1, Ordering::AcqRel) + 1;
        // A closed socket drops the frame, and with it any slot; the turn still runs and is
        // saved.
        let _ = self.outbox.send(Outgoing {
            _slot: slot,
            ..Outgoing::from(json!({
                "jsonrpc": "2.0",
                "method": "event",
                "params": {"type": kind, "session_id": self.id, "payload": payload, "seq": seq},
            }))
        });
    }

    async fn run(mut self, text: String, session: &Arc<LiveSession>) {
        self.emit(
            session,
            "message.start",
            json!({"turn_id": self.turn_id}),
            None,
        );
        let streamed = Arc::new(Mutex::new(Streamed::default()));
        let payload = match self.ask(text, session, &streamed).await {
            Ok((reply, None)) => json!({"text": reply, "status": "complete"}),
            Ok((reply, Some(warning))) => {
                json!({"text": reply, "status": "complete", "warning": warning})
            }
            Err(error) => {
                let streamed = streamed_text(&streamed);
                json!({
                    "text": streamed,
                    "status": "error",
                    "error": error,
                    "partial": !streamed.is_empty(),
                })
            }
        };
        let slot = self.slot.take();
        self.emit(session, "message.complete", payload, slot);
    }

    /// Runs the turn; returns the reply and a warning when it could not be saved.
    async fn ask(
        &self,
        text: String,
        session: &Arc<LiveSession>,
        streamed: &Arc<Mutex<Streamed>>,
    ) -> Result<(String, Option<String>), String> {
        let model = self
            .chat
            .model()
            .ok_or("no model is configured: run `hermes model`")?
            .to_string();
        let id = self.id.clone();
        let mut transcript = self
            .chat
            .with_store(move |store| store.load_session(&id))
            .await?
            .map_err(|e| e.to_string())?;
        // The leading system rows hold the stored system prompt, which the agent adds itself.
        let prompt_rows = transcript
            .iter()
            .take_while(|m| m.role == MessageRole::System)
            .count();
        transcript.push(Message::user(text));
        let messages = transcript[prompt_rows..].to_vec();
        let result = match self.answer(&model, messages, session, streamed).await {
            Ok(result) => result,
            Err(error) => {
                // As upstream keeps the user's row once a prompt is admitted, a failed turn
                // keeps the prompt and what streamed, for the transcript and the next turn.
                let partial = streamed_text(streamed);
                if !partial.is_empty() {
                    transcript.push(Message::assistant(partial));
                }
                let (id, model) = (self.id.clone(), model.clone());
                let saved = self
                    .chat
                    .with_store(move |store| {
                        store.persist_session(
                            &id,
                            &transcript,
                            Some(&model),
                            Some("desktop"),
                            None,
                            None,
                        )
                    })
                    .await
                    .and_then(|saved| saved.map_err(|e| e.to_string()));
                return Err(match saved {
                    Ok(()) => error,
                    Err(e) => format!("{error} (the prompt was not saved: {e})"),
                });
            }
        };
        let reply = hermes_app_runtime::assistant_reply_from_result(&result);
        let id = self.id.clone();
        let warning = self
            .chat
            .with_store(move |store| {
                store
                    .persist_agent_result(&id, &result, Some(&model), Some("desktop"))
                    .map_err(|e| e.to_string())
            })
            .await
            .and_then(|saved| saved)
            .err()
            .map(|e| format!("The reply was not saved: {e}"));
        Ok((reply, warning))
    }

    /// One tool-less agent run over `messages`, streaming the reply as it comes.
    async fn answer(
        &self,
        model: &str,
        messages: Vec<Message>,
        session: &Arc<LiveSession>,
        streamed: &Arc<Mutex<Streamed>>,
    ) -> Result<AgentResult, String> {
        let chat = &self.chat;
        let mut agent_config = hermes_app_runtime::build_agent_config(&chat.config, model);
        agent_config.session_id = Some(self.id.clone());
        let home = chat.home.clone();
        let agent_config = tokio::task::spawn_blocking(move || {
            AgentLoop::hydrate_stored_system_prompt_from_hermes_home(&mut agent_config, &home)
                .map(|()| agent_config)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        let provider = hermes_provider_runtime::build_provider(&chat.config, model);
        let agent = AgentLoop::new(agent_config, Arc::new(ToolRegistry::new()), provider);

        let delta = DeltaSink {
            outbox: self.outbox.clone(),
            id: self.id.clone(),
            turn_id: self.turn_id.clone(),
            session: Arc::clone(session),
            streamed: Arc::clone(streamed),
        };
        let on_chunk: Box<dyn Fn(StreamChunk) + Send + Sync> =
            Box::new(move |chunk| delta.send(chunk));
        agent
            .run_stream(messages, Some(Vec::new()), Some(on_chunk))
            .await
            .map_err(|e| e.to_string())
    }
}

fn streamed_text(streamed: &Mutex<Streamed>) -> String {
    streamed
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .text
        .clone()
}

/// Sends each streamed piece of the reply as `message.delta`, stamped with the turn, segment
/// and UTF-16 offset the desktop uses to drop a piece it already has.
struct DeltaSink {
    outbox: Outbox,
    id: String,
    turn_id: String,
    session: Arc<LiveSession>,
    streamed: Arc<Mutex<Streamed>>,
}

impl DeltaSink {
    fn send(&self, chunk: StreamChunk) {
        let Some(text) = chunk
            .delta
            .and_then(|d| d.content)
            .filter(|t| !t.is_empty())
        else {
            return;
        };
        let offset = {
            let mut streamed = self.streamed.lock().unwrap_or_else(|e| e.into_inner());
            let offset = streamed.units;
            streamed.units += text.encode_utf16().count();
            streamed.text.push_str(&text);
            offset
        };
        let seq = self.session.seq.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.outbox.send(Outgoing::from(json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {
                "type": "message.delta",
                "session_id": self.id,
                "payload": {"text": text, "turn_id": self.turn_id, "segment": 0, "offset": offset},
                "seq": seq,
            },
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids() {
        assert!(valid_session_id("7f0c6a1e-2b4d-4c4e-9a37-1d2f3e4a5b6c"));
        assert!(valid_session_id("20261117_130257_ddafe3"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("../state"));
        assert!(!valid_session_id("a b"));
        assert!(!valid_session_id(&"a".repeat(MAX_SESSION_ID + 1)));
    }
}
