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

use hermes_agent::agent_loop::ToolRegistry;
use hermes_agent::{AgentLoop, SessionPersistence};
use hermes_config::GatewayConfig;
use hermes_core::{Message, MessageRole, StreamChunk};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc::UnboundedSender;

/// Frames for one connection's socket.
pub(crate) type Outbox = UnboundedSender<Value>;

/// A JSON-RPC error: code and message.
pub(crate) type RpcError = (i64, String);

pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
// Upstream's gateway codes (tui_gateway): 4000 invalid params, 4001 unknown session,
// 4009 a turn already running, 4012 no text.
const INVALID_PARAMS: i64 = 4000;
const SESSION_NOT_FOUND: i64 = 4001;
const SESSION_BUSY: i64 = 4009;
const TEXT_REQUIRED: i64 = 4012;

/// Longest session id a request may name.
const MAX_SESSION_ID: usize = 128;

/// The model, the sessions database and the live sessions.
pub struct Chat {
    config: GatewayConfig,
    home: PathBuf,
    // shortcut: sessions live until the gateway exits (about 100 bytes each, created only by
    // a token holder); dropping idle ones comes with session close and resume.
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
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
        }
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
        if session.busy.swap(true, Ordering::AcqRel) {
            return Err((SESSION_BUSY, "session busy".into()));
        }
        let guard = TurnGuard(session);
        let turn = Turn {
            chat: Arc::clone(self),
            id: id.clone(),
            turn_id: uuid::Uuid::new_v4().simple().to_string(),
            outbox: outbox.clone(),
        };
        tokio::spawn(async move {
            turn.run(text, &guard.0).await;
        });
        Ok(json!({"status": "streaming"}))
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
}

impl Turn {
    fn emit(&self, session: &LiveSession, kind: &str, payload: Value) {
        let seq = session.seq.fetch_add(1, Ordering::AcqRel) + 1;
        // A closed socket drops the frame; the turn still runs and is saved.
        let _ = self.outbox.send(json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {"type": kind, "session_id": self.id, "payload": payload, "seq": seq},
        }));
    }

    async fn run(self, text: String, session: &Arc<LiveSession>) {
        self.emit(session, "message.start", json!({"turn_id": self.turn_id}));
        let streamed = Arc::new(Mutex::new(Streamed::default()));
        let result = self.ask(text, session, &streamed).await;
        let streamed = streamed.lock().map(|s| s.text.clone()).unwrap_or_default();
        let payload = match result {
            Ok((reply, None)) => json!({"text": reply, "status": "complete"}),
            Ok((reply, Some(warning))) => {
                json!({"text": reply, "status": "complete", "warning": warning})
            }
            Err(error) => json!({
                "text": streamed,
                "status": "error",
                "error": error,
                "partial": !streamed.is_empty(),
            }),
        };
        self.emit(session, "message.complete", payload);
    }

    /// Runs the turn; returns the reply and a warning when it could not be saved.
    async fn ask(
        &self,
        text: String,
        session: &Arc<LiveSession>,
        streamed: &Arc<Mutex<Streamed>>,
    ) -> Result<(String, Option<String>), String> {
        let chat = &self.chat;
        let model = chat
            .model()
            .ok_or("no model is configured: run `hermes model`")?
            .to_string();
        let store = SessionPersistence::new(&chat.home);
        let mut messages: Vec<Message> = store
            .load_session(&self.id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|m| m.role != MessageRole::System)
            .collect();
        messages.push(Message::user(text));

        let mut agent_config = hermes_app_runtime::build_agent_config(&chat.config, &model);
        agent_config.session_id = Some(self.id.clone());
        AgentLoop::hydrate_stored_system_prompt_from_hermes_home(&mut agent_config, &chat.home)
            .map_err(|e| e.to_string())?;
        let provider = hermes_provider_runtime::build_provider(&chat.config, &model);
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
        let result = agent
            .run_stream(messages, Some(Vec::new()), Some(on_chunk))
            .await
            .map_err(|e| e.to_string())?;
        let reply = hermes_app_runtime::assistant_reply_from_result(&result);
        let warning = store
            .persist_agent_result(&self.id, &result, Some(&model), Some("desktop"))
            .err()
            .map(|e| format!("The reply was not saved: {e}"));
        Ok((reply, warning))
    }
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
        let _ = self.outbox.send(json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {
                "type": "message.delta",
                "session_id": self.id,
                "payload": {"text": text, "turn_id": self.turn_id, "segment": 0, "offset": offset},
                "seq": seq,
            },
        }));
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
