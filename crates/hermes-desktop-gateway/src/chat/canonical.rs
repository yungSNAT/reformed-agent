//! The canonical session-authority wire, `hermes-gateway-v1`, that Hermes Desktop speaks on a
//! native dial (upstream gateway/session_controls.py `AuthorityConnection`): `session.create` and
//! `session.resume` answer with a session snapshot, `prompt.submit` with an admission receipt, and
//! a turn's events carry its admission, execution generation and the replay epoch, with a
//! `session.info` as the turn starts and ends. Every other method is answered as on the legacy
//! wire, so one this backend does not serve gets JSON-RPC 'method not found' (-32601).
//!
//! One profile is served, `default`, so a request naming any other is refused. A session's
//! metadata cannot be changed here, so its revision stays 0.
//!
//! A prompt sent whilst a turn runs is queued (its receipt says `queued`, and snapshots and
//! `session.info` list it in `pending` until it starts), a session resumed on another
//! connection streams there too, and a retried `session.create` with the same `request_id`
//! gets the same session while the gateway remembers it (its last [`super::CREATE_RECORDS`]
//! creates, of ids at least [`MIN_REMEMBERED_REQUEST_ID`] long).

use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::{ready, Chat, Conn, Handler, LiveSession, RpcError, Taken, CREATE_RECORDS};
use crate::CANONICAL_PROTOCOL;

/// Upstream's code for every canonical refusal (its `data.reason` names the cause).
const REFUSED: i64 = 4001;
/// Upstream's code when the sessions database cannot be read.
const STORAGE_UNAVAILABLE: i64 = 5001;
/// This process owns its sessions from the start, so their authority epoch never moves.
const AUTHORITY_EPOCH: u64 = 1;
/// The only profile served.
const PROFILE: &str = "default";
/// The `session.create` keys taken: the desktop's own (apps/desktop/src/api/canonical-protocol.ts
/// `prepareCreate`) that change nothing this backend does. Any other launch option (a model, a
/// working folder, toolsets) is refused rather than dropped.
const CREATE_KEYS: [&str; 4] = ["request_id", "source", "profile", "service_tier"];
/// Shortest `request_id` remembered. Any connection with the gateway's credential can create,
/// so a remembered id opens its session to whoever sends it again: a short one could be
/// guessed, where the desktop sends a UUID (36 characters).
const MIN_REMEMBERED_REQUEST_ID: usize = 16;

/// The methods a canonical connection answers its own way; the rest are the legacy wire's.
pub(super) const METHODS: &[(&str, Handler)] = &[
    ("session.create", |r| {
        ready(r.chat.canonical_create(&r.params, &r.conn))
    }),
    ("session.resume", |r| {
        ready(r.chat.canonical_resume(&r.params, &r.conn))
    }),
    ("prompt.submit", |r| {
        ready(r.chat.canonical_submit(&r.params, &r.conn))
    }),
];

/// Refuses a request that names a profile other than the one served.
pub(super) fn refuse_profile(params: &Map<String, Value>) -> Option<RpcError> {
    params
        .get("profile")
        .is_some_and(|p| p != PROFILE)
        .then(|| {
            (
                REFUSED,
                "profile_mismatch: this gateway serves the default profile only".into(),
            )
        })
}

/// A turn admitted on the canonical wire, stamped on each of its events.
#[derive(Clone)]
pub(super) struct Admission {
    pub(super) id: String,
    /// Prompts admitted in its session up to and including this one.
    pub(super) sequence: u64,
    /// Its turn's number among the session's turns, once it starts.
    pub(super) generation: u64,
    replay_epoch: String,
    input_id: Option<String>,
    text: String,
    profile_id: String,
}

impl Admission {
    /// Adds the admission's fields to an event's `params`, as upstream stamps them.
    pub(super) fn stamp(&self, params: &mut Value) {
        params["authority_epoch"] = AUTHORITY_EPOCH.into();
        params["execution_generation"] = self.generation.into();
        params["admission_id"] = self.id.clone().into();
        params["replay_epoch"] = self.replay_epoch.clone().into();
    }

    fn reference(&self, session_id: &str) -> Value {
        json!({"profile_id": self.profile_id, "session_id": session_id})
    }

    /// Its row in a snapshot's or `session.info`'s `pending`: `started` with its generation,
    /// or `queued` with none, as upstream's `CanonicalPendingAdmission`.
    pub(super) fn row(&self, session_id: &str, started: bool) -> Value {
        json!({
            "admission_id": self.id,
            "ref": self.reference(session_id),
            "sequence": self.sequence,
            "status": if started { "started" } else { "queued" },
            "outcome": null,
            "authority_epoch": AUTHORITY_EPOCH,
            "execution_generation": if started { json!(self.generation) } else { Value::Null },
            "input_id": self.input_id,
            "text": self.text,
            "controls": [],
            "steering": false,
        })
    }

    /// Bytes it holds besides its prompt's text, which a queue counts against its limits.
    pub(super) fn held_bytes(&self) -> usize {
        self.text.len()
    }

    /// The `session.info` payload as the turn starts (`running`) and once it has ended, with
    /// the session's `pending` admissions.
    pub(super) fn info(&self, session_id: &str, running: bool, pending: Vec<Value>) -> Value {
        json!({
            "stored_session_id": session_id,
            "pending": pending,
            "authority_epoch": AUTHORITY_EPOCH,
            "desktop_protocol": CANONICAL_PROTOCOL,
            "running": running,
            "execution_generation": self.generation,
            "revision": 0,
        })
    }

    /// Adds upstream's canonical fields to a `message.complete` payload.
    pub(super) fn complete(&self, payload: &mut Value) {
        payload["content"] = payload["text"].clone();
        payload["admission_id"] = self.id.clone().into();
        if payload["status"] == "complete" {
            payload["outcome"] = "completed".into();
        }
    }
}

impl Chat {
    /// Creates a session, or answers again for the one an earlier request with the same
    /// `request_id` made, and streams it to `conn`.
    fn canonical_create(
        &self,
        params: &Map<String, Value>,
        conn: &Conn,
    ) -> Result<Value, RpcError> {
        if let Some(key) = params.keys().find(|k| !CREATE_KEYS.contains(&k.as_str())) {
            return Err((
                REFUSED,
                format!("invalid_params: {key} is not a session option this gateway serves"),
            ));
        }
        let request_id: &String = match params.get("request_id") {
            Some(Value::String(r)) if !r.is_empty() => r,
            _ => {
                return Err((
                    REFUSED,
                    "invalid_params: request_id must be a non-empty string".into(),
                ))
            }
        };
        if params.get("source").is_some_and(|s| !s.is_string()) {
            return Err((REFUSED, "invalid_params: source must be a string".into()));
        }
        // `normal` is the desktop's default speed; another tier would not be honoured.
        if params.get("service_tier").is_some_and(|t| t != "normal") {
            return Err((
                REFUSED,
                "invalid_params: only the normal service tier is served".into(),
            ));
        }
        // One lock over the lookup, the creation and the record, so 2 requests with one id
        // make one session.
        let remembered = request_id.len() >= MIN_REMEMBERED_REQUEST_ID;
        let mut created = self.created.lock().unwrap_or_else(|e| e.into_inner());
        let earlier = created
            .iter()
            .filter(|_| remembered)
            .find(|(request, _)| request == request_id)
            .and_then(|(_, id)| Some((id.clone(), self.lock_sessions().get(id).cloned()?)));
        let repeat = earlier.is_some();
        let (id, session) = match earlier {
            Some(found) => found,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let session = self.new_session();
                self.lock_sessions()
                    .insert(id.clone(), Arc::clone(&session));
                if remembered {
                    created.retain(|(request, _)| request != request_id);
                    created.push_back((request_id.clone(), id.clone()));
                    if created.len() > CREATE_RECORDS {
                        created.pop_front();
                    }
                }
                (id, session)
            }
        };
        drop(created);
        session.subscribe(conn.id, &conn.outbox);
        // A repeated create answers with what the session holds by now.
        let rows = match repeat.then(|| self.history(&id)).flatten() {
            Some(Ok(rows)) => rows,
            _ => Vec::new(),
        };
        Ok(self.snapshot(&id, &session, rows, false))
    }

    /// Attaches `conn` to a live session, or to one saved earlier (a restarted gateway's).
    fn canonical_resume(
        &self,
        params: &Map<String, Value>,
        conn: &Conn,
    ) -> Result<Value, RpcError> {
        let Some(Value::String(id)) = params.get("session_id") else {
            return Err((
                REFUSED,
                "invalid_params: session_id must be a string".into(),
            ));
        };
        let not_found = || (REFUSED, "session_not_found".to_string());
        let rows = match self.history(id) {
            None => return Err(not_found()),
            Some(Err(e)) => return Err((STORAGE_UNAVAILABLE, format!("storage_unavailable: {e}"))),
            Some(Ok(rows)) => rows,
        };
        let session = {
            let mut sessions = self.lock_sessions();
            match sessions.get(id) {
                Some(session) => Arc::clone(session),
                None if rows.is_empty() => return Err(not_found()),
                None => Arc::clone(
                    sessions
                        .entry(id.clone())
                        .or_insert_with(|| self.new_session()),
                ),
            }
        };
        session.subscribe(conn.id, &conn.outbox);
        let omit = params.get("omit_messages") == Some(&Value::Bool(true));
        Ok(self.snapshot(id, &session, rows, omit))
    }

    fn canonical_submit(
        self: &Arc<Self>,
        params: &Map<String, Value>,
        conn: &Conn,
    ) -> Result<Value, RpcError> {
        let (id, text) = super::turn_request(params)?;
        // The desktop names its input `submission_id`; upstream's wire calls it `input_id`.
        let input_id = match params.get("submission_id").or(params.get("input_id")) {
            None => None,
            Some(Value::String(input)) => Some(input.clone()),
            Some(_) => {
                return Err((
                    REFUSED,
                    "invalid_params: submission_id must be a string".into(),
                ))
            }
        };
        let admission = Admission {
            id: uuid::Uuid::new_v4().simple().to_string(),
            sequence: 0,
            generation: 0,
            replay_epoch: self.replay_epoch().to_string(),
            input_id,
            text: text.clone(),
            profile_id: self.home.display().to_string(),
        };
        let reference = admission.reference(id);
        let admission_id = admission.id.clone();
        let (status, sequence, generation) =
            match self.start_turn(id, text, conn, Some(admission))? {
                Taken::Started {
                    generation,
                    sequence,
                } => ("started", sequence, Some(generation)),
                Taken::Queued { sequence } => ("queued", sequence, None),
            };
        Ok(json!({
            "admission_id": admission_id,
            "ref": reference,
            "sequence": sequence,
            "status": status,
            "outcome": null,
            "authority_epoch": AUTHORITY_EPOCH,
            "execution_generation": generation,
        }))
    }

    /// A session's snapshot, as upstream's `session.create` and `session.resume` answer: its saved
    /// user and assistant `rows`, or only their count when `omit` is set.
    fn snapshot(&self, id: &str, session: &LiveSession, rows: Vec<Value>, omit: bool) -> Value {
        let (running, last_sequence, generation) = session.state();
        let mut info = self.session_info();
        info["desktop_protocol"] = CANONICAL_PROTOCOL.into();
        info["profile_name"] = PROFILE.into();
        let count = rows.len();
        let mut snapshot = json!({
            "session_id": id,
            "stored_session_id": id,
            "messages": if omit { Vec::new() } else { rows },
            "message_count": count,
            "running": running,
            "authority_epoch": AUTHORITY_EPOCH,
            "replay_epoch": self.replay_epoch(),
            "last_sequence": last_sequence,
            "revision": 0,
            "execution_generation": generation,
            "pending": session.pending(id, true),
            "prompts": [],
            "info": info,
        });
        if omit {
            snapshot["messages_omitted"] = true.into();
        }
        snapshot
    }
}
