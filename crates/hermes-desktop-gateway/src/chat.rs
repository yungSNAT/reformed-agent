//! Chat for Hermes Desktop: `session.create`, `file.attach`, `prompt.submit` and the reply
//! streamed back as `message.start`, `message.delta` and `message.complete` events, saved to
//! the sessions database the CLI uses.
//!
//! Turns run the configured model with no tools, so no request can run a command, and the only
//! files one writes are an attachment under `<working folder>/.hermes/attachments` and an image
//! (`image.attach_bytes`) under `<home>/images`, which the next prompt sends the model. The agent
//! expands a prompt's `@file:` references, confined to the working folder, so the model reads
//! an attachment's text. Tool calls and their approval prompts come with a later change.
//!
//! Every connection that creates, resumes or prompts a session gets its events (see
//! [`session`]); a prompt sent during a turn waits for it.
//!
//! Methods are looked up in [`AREAS`]: each area is a module with a `METHODS` table of names
//! and [`Handler`]s, and a canonical connection looks in `canonical::METHODS` first.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
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

use crate::attach::{ImageError, StagedImage, MAX_IMAGE_BYTES};

mod canonical;
mod images;
pub(crate) mod profiles;
mod relay;
mod session;
use canonical::Admission;
pub(crate) use session::LiveSession;
use session::{Admitted, Budget, Next, Queued, Refused, MAX_STAGED_BYTES, MAX_STAGED_IMAGES};

/// Marks a message whose content is a JSON array of text and image parts, which the agent's
/// providers send as such (`ACP_MULTIMODAL_PREFIX` in hermes-agent).
const PARTS_PREFIX: &str = "__hermes_acp_parts_json__:";

/// Frames for one connection's socket.
pub(crate) type Outbox = UnboundedSender<Outgoing>;

/// A frame for the connection to write and when it was queued. A turn's last event carries
/// the turn's slot on the copy for the connection that prompted the turn, so the slot frees
/// once that copy is written, or dropped with a closed connection. Other connections' copies
/// carry none: a viewer that stops reading holds no slot, and its write deadline closes it.
pub(crate) struct Outgoing {
    pub(crate) text: Arc<str>,
    pub(crate) queued: Instant,
    _slot: Option<Arc<OwnedSemaphorePermit>>,
}

impl Outgoing {
    pub(crate) fn shared(
        text: Arc<str>,
        queued: Instant,
        slot: Option<Arc<OwnedSemaphorePermit>>,
    ) -> Self {
        Self {
            text,
            queued,
            _slot: slot,
        }
    }
}

impl From<Value> for Outgoing {
    fn from(frame: Value) -> Self {
        Self::shared(frame.to_string().into(), Instant::now(), None)
    }
}

/// Where a connection's client is, for the methods that serve only clients on this machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Origin {
    /// Not known to be on this machine. A connection has it until its listener says otherwise,
    /// so a listener that does not say gets the most restricted.
    #[default]
    Remote,
    /// A browser session of the Webapp.
    Webapp,
    /// This machine's desktop or terminal UI: the loopback token socket or local mode's ticket
    /// socket (`upgrade` in lib.rs).
    Local,
}

/// One WebSocket connection.
#[derive(Clone)]
pub(crate) struct Conn {
    /// Unique within the process.
    pub(crate) id: u64,
    pub(crate) outbox: Outbox,
    /// Speaks the canonical wire ([`crate::CANONICAL_PROTOCOL`]).
    pub(crate) canonical: bool,
    /// Where its client is; check it before serving a client that must be on this machine.
    #[allow(
        dead_code,
        reason = "read by the methods that serve only local clients"
    )]
    pub(crate) origin: Origin,
}

impl Conn {
    /// A connection with [`Origin::Remote`] until its listener sets `origin`.
    pub(crate) fn new(id: u64, outbox: Outbox, canonical: bool) -> Self {
        Self {
            id,
            outbox,
            canonical,
            origin: Origin::default(),
        }
    }
}

/// One request as a handler gets it. Absent params arrive as an empty map.
pub(crate) struct Request {
    pub(crate) chat: Arc<Chat>,
    pub(crate) conn: Conn,
    pub(crate) params: Map<String, Value>,
}

/// A method's result, ready now or later.
pub(crate) type Reply = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;

/// Runs one method. A handler may wait (for a turn, a slot or an answer from the client):
/// meanwhile its connection goes on writing events and answering other requests, so replies
/// can come out of order, and up to 16 of a connection's frames wait at once (`serve_socket`
/// in lib.rs). Past that, a handler that would wait is dropped before its first wait and its
/// request answered busy, so a method that answers a waiting handler (an approval's answer)
/// must answer at once. A handler that changes state does so before it returns, or makes no
/// change before its first wait, so a request answered busy has changed nothing. A waiting
/// handler is dropped at its await if the connection closes, so it must leave shared state
/// whole at each await: change state under a lock rather than across an await, or hold a
/// guard that undoes the change when dropped.
///
/// Each method name is registered once across [`AREAS`]; `canonical::METHODS` overrides an
/// area's name only where its test lists it (`each_method_is_registered_once`).
pub(crate) type Handler = fn(Request) -> Reply;

/// A reply that is ready now.
pub(crate) fn ready(result: Result<Value, RpcError>) -> Reply {
    Box::pin(std::future::ready(result))
}

/// Every area's methods; a new area adds its table here.
const AREAS: &[&[(&str, Handler)]] = &[METHODS, images::METHODS, profiles::METHODS, relay::METHODS];

/// The methods served on both wires.
const METHODS: &[(&str, Handler)] = &[
    ("setup.runtime_check", |r| ready(Ok(r.chat.runtime_check()))),
    ("wake.status", |_| ready(Ok(wake_status()))),
    ("session.create", |r| ready(Ok(r.chat.create(&r.conn)))),
    ("file.attach", |r| ready(r.chat.attach(&r.params))),
    ("image.attach_bytes", |r| {
        ready(r.chat.attach_image(&r.params))
    }),
    ("prompt.submit", |r| {
        ready(r.chat.submit(&r.params, &r.conn))
    }),
    ("session.events.since", |r| ready(events_since(&r))),
];

/// `session.events.since`: a session's events after `last_seen`, from its log of recent events
/// (upstream tui_gateway/methods_session.py); `truncated` when some were dropped, so the client
/// refetches the transcript. An unknown session has none.
fn events_since(req: &Request) -> Result<Value, RpcError> {
    let id = match req.params.get("session_id") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(id)) => id.clone(),
        Some(_) => return Err((INVALID_PARAMS, "session_id must be a string".into())),
    };
    let last_seen = match req.params.get("last_seen") {
        None | Some(Value::Null) => 0,
        // A negative watermark asks for every event, as upstream's `seq > last_seen` does.
        Some(Value::Number(n)) if n.is_u64() || n.is_i64() => n.as_u64().unwrap_or(0),
        Some(_) => {
            return Err((
                BAD_LAST_SEEN,
                "invalid params: last_seen must be an integer".into(),
            ))
        }
    };
    let session = req.chat.lock_sessions().get(&id).cloned();
    let session::Replay {
        events,
        latest_seq,
        truncated,
    } = match session {
        Some(session) => session.events_since(last_seen),
        None => session::Replay {
            events: Vec::new(),
            latest_seq: 0,
            truncated: false,
        },
    };
    Ok(json!({
        "count": events.len(),
        "events": events,
        "latest_seq": latest_seq,
        "truncated": truncated,
        "epoch": req.chat.replay_epoch(),
        "open_requests": [],
    }))
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
/// JSON-RPC's invalid params, which upstream gives a `last_seen` that is not an integer.
const BAD_LAST_SEEN: i64 = -32602;
// 4015 no attachment given, 5028 the attachment could not be staged.
const ATTACHMENT_REQUIRED: i64 = 4015;
const ATTACH_FAILED: i64 = 5028;
// `image.attach_bytes` refusals, as upstream numbers them.
const IMAGE_UNSUPPORTED: i64 = 4016;
const IMAGE_INVALID: i64 = 4017;
const IMAGE_TOO_LARGE: i64 = 4018;
const IMAGE_WRITE_FAILED: i64 = 5027;

/// Most turns running or with their last event not yet written to the connection that
/// prompted them, across the gateway; it bounds the replies held for those connections.
const MAX_TURNS: usize = 8;

/// How long a frame may wait, from being queued until it is written, before the connection
/// closes. A turn's events are all queued by its end, so its slot frees within this time of
/// the turn's end however slowly the client reads.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest session id a request may name.
const MAX_SESSION_ID: usize = 128;

/// Most `session.create` request ids remembered, so a retried create gets its session back.
const CREATE_RECORDS: usize = 1024;

/// The model, the sessions database and the live sessions.
pub struct Chat {
    config: GatewayConfig,
    home: PathBuf,
    // shortcut: sessions live until the gateway exits (a few hundred bytes each plus their
    // event logs, which the budget bounds; created only by a token holder); dropping idle ones
    // comes with session close.
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
    /// Canonical `session.create` request ids and the sessions they made, oldest first.
    created: Mutex<VecDeque<(String, String)>>,
    budget: Arc<Budget>,
    /// Held for each write to the sessions database (see [`Chat::write_store`]).
    writes: Arc<Mutex<()>>,
    turns: Arc<Semaphore>,
    write_timeout: Duration,
    replay_epoch: Arc<str>,
}

/// The reply streamed so far and its length in UTF-16 units, the desktop's offsets.
#[derive(Default)]
struct Streamed {
    text: String,
    units: usize,
}

/// A prompt taken for a session: its turn started, or it waits behind the running turn.
enum Taken {
    Started { generation: u64, sequence: u64 },
    Queued { sequence: u64 },
}

/// Leaves a session idle, dropping its waiting prompts, if its turn task ends before it has
/// run them all (a panic, or the runtime shutting down).
struct DrainGuard(Option<Arc<LiveSession>>);

impl Drop for DrainGuard {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.abandon();
        }
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
            created: Mutex::default(),
            budget: Arc::default(),
            writes: Arc::default(),
            turns: Arc::new(Semaphore::new(MAX_TURNS)),
            write_timeout: WRITE_TIMEOUT,
            replay_epoch: uuid::Uuid::new_v4().simple().to_string().into(),
        }
    }

    /// Identifies this process's event numbering (`gateway.ready`'s `replay_epoch`).
    pub(crate) fn replay_epoch(&self) -> &str {
        &self.replay_epoch
    }

    fn new_session(&self) -> Arc<LiveSession> {
        LiveSession::new(Arc::clone(&self.budget))
    }

    /// Stops streaming to connection `id`, which has closed.
    pub(crate) fn disconnect(&self, id: u64) {
        let sessions: Vec<Arc<LiveSession>> = self.lock_sessions().values().cloned().collect();
        for session in sessions {
            session.unsubscribe(id);
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

    /// Answers one JSON-RPC request on `conn`, whose `params` must be absent (null) or an
    /// object. A canonical connection checks the params and profile first, as upstream's
    /// session authority does; the legacy wire names an unknown method before bad params.
    pub(crate) fn call(self: &Arc<Self>, conn: &Conn, method: &str, params: Value) -> Reply {
        let params = match params {
            Value::Object(map) => Ok(map),
            Value::Null => Ok(Map::new()),
            _ => Err((INVALID_PARAMS, "params must be an object".to_string())),
        };
        if conn.canonical {
            let refused = match &params {
                Err(e) => Some(e.clone()),
                Ok(params) => canonical::refuse_profile(params),
            };
            if let Some(refused) = refused {
                return ready(Err(refused));
            }
        }
        let canonical = conn.canonical.then_some(canonical::METHODS);
        let handler = canonical
            .into_iter()
            .chain(AREAS.iter().copied())
            .flatten()
            .find(|(name, _)| *name == method);
        let Some((_, handler)) = handler else {
            return ready(Err((
                METHOD_NOT_FOUND,
                // Upstream's text (tui_gateway/contracts/registry.py), which the desktop
                // matches to tell a method this backend lacks from one that failed.
                format!("unknown method: {method} \u{2014} not available on this Hermes backend"),
            )));
        };
        match params {
            Ok(params) => handler(Request {
                chat: Arc::clone(self),
                conn: conn.clone(),
                params,
            }),
            Err(e) => ready(Err(e)),
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

    fn create(&self, conn: &Conn) -> Value {
        let id = uuid::Uuid::new_v4().to_string();
        let session = self.new_session();
        session.subscribe(conn.id, &conn.outbox);
        self.lock_sessions().insert(id.clone(), session);
        json!({
            "session_id": id,
            "stored_session_id": id,
            "message_count": 0,
            "messages": [],
            "info": self.session_info(),
        })
    }

    /// A session's `info`: the model, no tools or skills, and the working folder.
    fn session_info(&self) -> Value {
        let cwd = std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        json!({"model": self.model().unwrap_or_default(), "tools": {}, "skills": {}, "cwd": cwd})
    }

    /// Stages a file the desktop sends as a `data_url` (upstream's reply shape). A `path`
    /// alone is refused: this backend reads no host path a client names.
    fn attach(&self, params: &Map<String, Value>) -> Result<Value, RpcError> {
        let Some(Value::String(id)) = params.get("session_id") else {
            return Err((INVALID_PARAMS, "session_id must be a string".into()));
        };
        let data_url = match params.get("data_url") {
            Some(Value::String(data_url)) if !data_url.trim().is_empty() => data_url,
            None | Some(Value::String(_)) => {
                return Err((ATTACHMENT_REQUIRED, "data_url required".into()));
            }
            Some(_) => return Err((INVALID_PARAMS, "data_url must be a string".into())),
        };
        let name = match params.get("name") {
            Some(Value::String(name)) => name.as_str(),
            None => "",
            Some(_) => return Err((INVALID_PARAMS, "name must be a string".into())),
        };
        if !self.lock_sessions().contains_key(id) {
            return Err((SESSION_NOT_FOUND, "session not found".into()));
        }
        // The agent resolves `@file:` against the process's working folder.
        let workspace = std::env::current_dir()
            .map_err(|e| (ATTACH_FAILED, format!("no working folder: {e}")))?;
        // Writing up to 256 MiB blocks; on the multi-threaded runtime `hermes serve` uses, the
        // other tasks on this thread, other connections' writes among them, move meanwhile.
        let stage = || crate::attach::stage(&workspace, name, data_url);
        let staged = match tokio::runtime::Handle::current().runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(stage),
            _ => stage(),
        }
        .map_err(|e| (ATTACH_FAILED, e))?;
        Ok(json!({
            "attached": true,
            "name": staged.name,
            "path": staged.path.display().to_string(),
            "ref_path": staged.ref_path,
            "ref_text": format!("@file:{}", staged.ref_path),
            "uploaded": true,
        }))
    }

    /// Stages an image the desktop sends as base64 (`content_base64`, or upstream's `data`
    /// alias) under `<home>/images` for the session's next prompt. The bytes decide the type;
    /// `filename` and `ext` are ignored, so no client name reaches the disk.
    fn attach_image(&self, params: &Map<String, Value>) -> Result<Value, RpcError> {
        let Some(Value::String(id)) = params.get("session_id") else {
            return Err((INVALID_PARAMS, "session_id must be a string".into()));
        };
        // As upstream, an absent, null or empty `content_base64` gives way to `data`.
        let mut payload = None;
        for key in ["content_base64", "data"] {
            match params.get(key) {
                Some(Value::String(text)) if !text.is_empty() => {
                    payload = Some(text);
                    break;
                }
                None | Some(Value::Null | Value::String(_)) => {}
                Some(_) => return Err((INVALID_PARAMS, format!("{key} must be a string"))),
            }
        }
        let Some(payload) = payload.filter(|text| !text.trim().is_empty()) else {
            return Err((ATTACHMENT_REQUIRED, "content_base64 required".into()));
        };
        let Some(session) = self.lock_sessions().get(id).cloned() else {
            return Err((SESSION_NOT_FOUND, "session not found".into()));
        };
        let full = || {
            (
                IMAGE_TOO_LARGE,
                format!(
                    "a session holds at most {MAX_STAGED_IMAGES} images ({MAX_STAGED_BYTES} bytes)"
                ),
            )
        };
        if !session.has_room_for_image(0) {
            return Err(full());
        }
        let images = self.home.join("images");
        let stage = || crate::attach::stage_image(&images, payload);
        let staged = match tokio::runtime::Handle::current().runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(stage),
            _ => stage(),
        }
        .map_err(|e| match e {
            ImageError::Invalid(message) => (IMAGE_INVALID, message),
            ImageError::TooLarge => (
                IMAGE_TOO_LARGE,
                format!("image larger than {MAX_IMAGE_BYTES} bytes"),
            ),
            ImageError::Unsupported => (
                IMAGE_UNSUPPORTED,
                "unsupported image: send PNG, JPEG, GIF, WebP or BMP".into(),
            ),
            ImageError::Write(message) => (IMAGE_WRITE_FAILED, message),
        })?;
        let name = staged
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut reply = json!({
            "attached": true,
            "name": name,
            "path": staged.path.display().to_string(),
            "bytes": staged.bytes,
            "remainder": "",
            "text": format!("[User attached image: {name}]"),
        });
        match session.stage_image(staged) {
            Ok(count) => reply["count"] = count.into(),
            Err(refused) => {
                let _ = std::fs::remove_file(&refused.path);
                return Err(full());
            }
        }
        Ok(reply)
    }

    fn submit(
        self: &Arc<Self>,
        params: &Map<String, Value>,
        conn: &Conn,
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
        let (id, text) = turn_request(params)?;
        Ok(match self.start_turn(id, text, conn, None)? {
            Taken::Started { .. } => json!({"status": "streaming"}),
            Taken::Queued { .. } => json!({"status": "queued"}),
        })
    }

    /// Takes a prompt for session `id` and streams its turn's events to every connection on
    /// the session, `conn` included, stamped with `admission` on the canonical wire. An idle
    /// session starts the turn now; a busy one runs it after the turns before it (upstream's
    /// `queue` busy mode: our turns cannot be redirected).
    // shortcut: a waiting prompt is held in memory only, so it is lost if the gateway exits
    // before it runs (upstream saves its user row when it is accepted).
    fn start_turn(
        self: &Arc<Self>,
        id: &str,
        text: String,
        conn: &Conn,
        admission: Option<Admission>,
    ) -> Result<Taken, RpcError> {
        let Some(session) = self.lock_sessions().get(id).cloned() else {
            return Err((SESSION_NOT_FOUND, "session not found".into()));
        };
        session.subscribe(conn.id, &conn.outbox);
        // The prompt takes every image staged so far; one staged after it waits for the next
        // prompt.
        let images = std::mem::take(&mut *session.images());
        let prompt = Queued {
            owner: conn.id,
            text,
            images,
            admission: admission.map(Box::new),
        };
        let slot = || Arc::clone(&self.turns).try_acquire_owned().ok();
        let (refused, prompt) = match session.admit(slot, prompt) {
            Ok(Admitted::Start {
                generation,
                sequence,
                slot,
                prompt,
            }) => {
                self.drain(id, session, prompt, slot);
                return Ok(Taken::Started {
                    generation,
                    sequence,
                });
            }
            Ok(Admitted::Queued { sequence }) => return Ok(Taken::Queued { sequence }),
            Err(refusal) => refusal,
        };
        // Refused: the images wait for the next prompt again.
        session.images().splice(0..0, prompt.images);
        Err(match refused {
            Refused::NoSlot => (
                NO_TURN_SLOT,
                format!("{MAX_TURNS} turns are running; send again when one ends"),
            ),
            Refused::QueueFull => (
                SESSION_BUSY,
                "session busy: its queue of prompts is full; send again when a turn ends".into(),
            ),
        })
    }

    /// Runs `prompt` as session `id`'s turn holding `slot`, then each prompt that waits for
    /// the session, in order, each with a turn slot as one frees. This task is the session's
    /// only runner until its queue is empty.
    fn drain(
        self: &Arc<Self>,
        id: &str,
        session: Arc<LiveSession>,
        prompt: Queued,
        slot: OwnedSemaphorePermit,
    ) {
        let chat = Arc::clone(self);
        let id = id.to_string();
        // Made before the task, so a task dropped before it first runs leaves the session idle.
        let guard = DrainGuard(Some(Arc::clone(&session)));
        tokio::spawn(async move {
            // Moves the whole guard in; a closure would capture only the field it assigns.
            let mut guard = guard;
            let mut next = Some((prompt, slot));
            while let Some((prompt, slot)) = next {
                let turn = Turn {
                    chat: Arc::clone(&chat),
                    id: id.clone(),
                    turn_id: uuid::Uuid::new_v4().simple().to_string(),
                    slot: Some(Arc::new(slot)),
                    owner: prompt.owner,
                    admission: prompt.admission.map(|admission| *admission),
                };
                turn.run(prompt.text, prompt.images, &session).await;
                let mut slot = None;
                next = loop {
                    match session.next(&mut slot) {
                        Next::Run(prompt, slot) => break Some((prompt, slot)),
                        Next::Idle => {
                            guard.0 = None;
                            break None;
                        }
                        // The prompt was accepted, so it waits for a slot rather than failing.
                        Next::Wait => match Arc::clone(&chat.turns).acquire_owned().await {
                            Ok(permit) => slot = Some(permit),
                            Err(_) => break None,
                        },
                    }
                };
            }
        });
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

    /// As [`Chat::with_store`], one write at a time. The gateway's turns share one sessions
    /// database, and SQLite fails a writer that waits past its busy timeout (5 seconds), which
    /// would lose a reply when several long turns end together.
    async fn write_store<T: Send + 'static>(
        &self,
        work: impl FnOnce(SessionPersistence) -> T + Send + 'static,
    ) -> Result<T, String> {
        let writes = Arc::clone(&self.writes);
        self.with_store(move |store| {
            // Held only around a write, which leaves nothing half-done on a panic.
            let _one = writes.lock().unwrap_or_else(|e| e.into_inner());
            work(store)
        })
        .await
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

/// The words a session keeps for a prompt `text` with `images`, as upstream's
/// `build_native_content_parts`: the text, then an `[Image attached at: <path>]` marker per
/// staged file.
fn with_markers(text: String, images: &[StagedImage]) -> String {
    let markers: Vec<String> = images
        .iter()
        .map(|image| format!("[Image attached at: {}]", image.path.display()))
        .collect();
    format!("{text}\n\n{}", markers.join("\n"))
}

/// The content the model gets for a prompt's `words` and `images`: the words as a text part,
/// then each image as a `data:` URL part.
fn with_images(words: String, images: &[StagedImage]) -> Result<String, String> {
    use base64::Engine as _;
    let mut parts = vec![json!({"type": "text", "text": words})];
    for image in images {
        let data = std::fs::read(&image.path)
            .map_err(|e| format!("cannot read the attached image: {e}"))?;
        let url = format!(
            "data:{};base64,{}",
            image.mime,
            base64::engine::general_purpose::STANDARD.encode(data)
        );
        parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
    }
    Ok(format!("{PARTS_PREFIX}{}", Value::Array(parts)))
}

/// Gives the newest user row in `rows` that holds image parts its text parts' text instead,
/// so the session keeps the prompt's words and markers, not the image data, and later turns
/// do not send the images again.
fn keep_words(rows: &mut [Message]) {
    for row in rows
        .iter_mut()
        .rev()
        .filter(|row| row.role == MessageRole::User)
    {
        let parts = row
            .content
            .as_deref()
            .and_then(|content| content.trim().strip_prefix(PARTS_PREFIX))
            .and_then(|json| serde_json::from_str::<Vec<Value>>(json).ok());
        if let Some(parts) = parts {
            let text: Vec<&str> = parts
                .iter()
                .filter(|part| part["type"] == "text")
                .filter_map(|part| part["text"].as_str())
                .collect();
            row.content = Some(text.join("\n"));
            return;
        }
    }
}

/// A prompt's session id and text, refused as upstream refuses them.
fn turn_request(params: &Map<String, Value>) -> Result<(&str, String), RpcError> {
    let Some(Value::String(id)) = params.get("session_id") else {
        return Err((INVALID_PARAMS, "session_id must be a string".into()));
    };
    match params.get("text") {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok((id, text.clone())),
        None | Some(Value::String(_)) => Err((TEXT_REQUIRED, "text required".into())),
        Some(_) => Err((INVALID_PARAMS, "text must be a string".into())),
    }
}

/// No wake word is served: upstream's `WakeStatusResult` (tui_gateway/contracts/prompt_voice.py)
/// for a detector that is unavailable and off, with upstream's default phrase, provider and frame
/// format, so the desktop shows the wake word as off.
fn wake_status() -> Value {
    json!({
        "listening": false,
        "owned_by_caller": false,
        "owner_surface": null,
        "phrase": "hey hermes",
        "provider": "openwakeword",
        "configured_surface": "auto",
        "input_device": {"selector": null, "error": null},
        "available": false,
        "hint": "The wake word is not served by this gateway.",
        "enabled": false,
        "audio_silent": false,
        "capture": "client",
        "local_input_available": false,
        "sample_rate": 16000,
        "frame_length": 1280,
    })
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
    /// The turn's slot, sent with its last event.
    slot: Option<Arc<OwnedSemaphorePermit>>,
    /// The connection that prompted the turn.
    owner: u64,
    /// Set on the canonical wire.
    admission: Option<Admission>,
}

impl Turn {
    fn emit(
        &self,
        session: &LiveSession,
        kind: &str,
        payload: Value,
        slot: Option<Arc<OwnedSemaphorePermit>>,
    ) {
        let mut params = json!({"type": kind, "session_id": self.id, "payload": payload});
        if let Some(admission) = &self.admission {
            admission.stamp(&mut params);
        }
        // With no connection left the turn still runs and is saved.
        session.publish(params, slot.map(|slot| (self.owner, slot)));
    }

    async fn run(mut self, text: String, images: Vec<StagedImage>, session: &Arc<LiveSession>) {
        self.emit(
            session,
            "message.start",
            json!({"turn_id": self.turn_id}),
            None,
        );
        if let Some(admission) = &self.admission {
            self.emit(
                session,
                "session.info",
                admission.info(&self.id, true, session.pending(&self.id, true)),
                None,
            );
        }
        let streamed = Arc::new(Mutex::new(Streamed::default()));
        let mut payload = match self.ask(text, images, session, &streamed).await {
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
        match &self.admission {
            None => self.emit(session, "message.complete", payload, slot),
            Some(admission) => {
                admission.complete(&mut payload);
                self.emit(session, "message.complete", payload, None);
                self.emit(
                    session,
                    "session.info",
                    admission.info(&self.id, false, session.pending(&self.id, false)),
                    slot,
                );
            }
        }
    }

    /// Runs the turn; returns the reply and a warning when it could not be saved.
    async fn ask(
        &self,
        text: String,
        images: Vec<StagedImage>,
        session: &Arc<LiveSession>,
        streamed: &Arc<Mutex<Streamed>>,
    ) -> Result<(String, Option<String>), String> {
        let model = self
            .chat
            .model()
            .ok_or("no model is configured: run `hermes model`")?
            .to_string();
        let sent_images = !images.is_empty();
        let text = if sent_images {
            with_markers(text, &images)
        } else {
            text
        };
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
        let mut messages = transcript[prompt_rows..].to_vec();
        transcript.push(Message::user(text.clone()));
        // The model gets the images; the session keeps the prompt's words, also when an image
        // can no longer be read.
        let content = if sent_images {
            tokio::task::spawn_blocking(move || with_images(text, &images))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
        } else {
            Ok(text)
        };
        let answered = match content {
            Ok(content) => {
                messages.push(Message::user(content));
                self.answer(&model, messages, session, streamed).await
            }
            Err(error) => Err(error),
        };
        let mut result = match answered {
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
                    .write_store(move |store| {
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
        if sent_images {
            keep_words(&mut result.messages);
            for replaced in &mut result.compressed_transcripts {
                keep_words(replaced);
            }
        }
        let reply = hermes_app_runtime::assistant_reply_from_result(&result);
        let id = self.id.clone();
        let warning = self
            .chat
            .write_store(move |store| {
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
            id: self.id.clone(),
            turn_id: self.turn_id.clone(),
            session: Arc::clone(session),
            streamed: Arc::clone(streamed),
            admission: self.admission.clone(),
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
    id: String,
    turn_id: String,
    session: Arc<LiveSession>,
    streamed: Arc<Mutex<Streamed>>,
    admission: Option<Admission>,
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
        let mut params = json!({
            "type": "message.delta",
            "session_id": self.id,
            "payload": {"text": text, "turn_id": self.turn_id, "segment": 0, "offset": offset},
        });
        if let Some(admission) = &self.admission {
            admission.stamp(&mut params);
        }
        self.session.publish(params, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The methods the canonical wire answers in place of an area's handler.
    const CANONICAL_OVERRIDES: &[&str] = &["session.create", "prompt.submit"];

    /// `Chat::call` takes the first handler that names a method, so a second registration
    /// would never run: each name is registered once across the areas, and the canonical
    /// table repeats an area's name only where it is meant to override it.
    #[test]
    fn each_method_is_registered_once() {
        let mut served = std::collections::HashSet::new();
        for (name, _) in AREAS.iter().flat_map(|area| area.iter()) {
            assert!(served.insert(*name), "{name} is registered twice in AREAS");
        }
        let mut canonical_names = std::collections::HashSet::new();
        for (name, _) in canonical::METHODS {
            assert!(
                canonical_names.insert(*name),
                "{name} is registered twice in canonical::METHODS"
            );
            assert_eq!(
                served.contains(name),
                CANONICAL_OVERRIDES.contains(name),
                "{name}: served by an area but not a listed canonical override, or the reverse"
            );
        }
    }

    #[test]
    fn a_connection_whose_listener_sets_no_origin_is_remote() {
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(Conn::new(1, outbox, false).origin, Origin::Remote);
    }

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
