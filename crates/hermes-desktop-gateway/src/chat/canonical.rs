//! The canonical session-authority wire, `hermes-gateway-v1`, that Hermes Desktop speaks on a
//! native dial (upstream gateway/session_controls.py `AuthorityConnection`): `session.create` and
//! `session.resume` answer with a session snapshot, `prompt.submit` with an admission receipt, and
//! a turn's events carry its admission, execution generation and the replay epoch, with a
//! `session.info` as the turn starts and ends. Every other method is answered as on the legacy
//! wire, so one this backend does not serve gets JSON-RPC 'method not found'.
//!
//! One profile is served, `default`, so a request naming any other is refused. A session's
//! metadata cannot be changed here, so its revision stays 0.
// shortcut: a prompt sent whilst a turn runs is refused (session busy), not queued; a session
// resumed on a second connection gets only the turns started there; `session.create` keeps no
// record of its `request_id`, so a retry after a lost reply makes a second empty session.
// Upgrade path: a per-session queue and event fan-out, as upstream's session authority keeps.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::{Chat, LiveSession, Outbox, RpcError, INVALID_PARAMS};
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

/// A turn admitted on the canonical wire, stamped on each of its events.
#[derive(Clone)]
pub(super) struct Admission {
    pub(super) id: String,
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

    /// The `session.info` payload as the turn starts (`running`) and once it has ended.
    pub(super) fn info(&self, session_id: &str, running: bool) -> Value {
        let pending = if running {
            json!([{
                "admission_id": self.id,
                "ref": self.reference(session_id),
                "sequence": self.generation,
                "status": "started",
                "outcome": null,
                "authority_epoch": AUTHORITY_EPOCH,
                "execution_generation": self.generation,
                "input_id": self.input_id,
                "text": self.text,
                "controls": [],
                "steering": false,
            }])
        } else {
            json!([])
        };
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
    /// Answers one request on a canonical connection; `replay_epoch` is the gateway's.
    pub(crate) fn call_canonical(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        outbox: &Outbox,
        replay_epoch: &str,
    ) -> Result<Value, RpcError> {
        let empty = Map::new();
        let named = match params {
            Value::Object(map) => map,
            Value::Null => &empty,
            _ => return Err((INVALID_PARAMS, "params must be an object".into())),
        };
        if named.get("profile").is_some_and(|p| p != PROFILE) {
            return Err((
                REFUSED,
                "profile_mismatch: this gateway serves the default profile only".into(),
            ));
        }
        match method {
            "session.create" => self.canonical_create(named, replay_epoch),
            "session.resume" => self.canonical_resume(named, replay_epoch),
            "prompt.submit" => self.canonical_submit(named, outbox, replay_epoch),
            _ => self.call(method, params, outbox),
        }
    }

    fn canonical_create(
        &self,
        params: &Map<String, Value>,
        replay_epoch: &str,
    ) -> Result<Value, RpcError> {
        if let Some(key) = params.keys().find(|k| !CREATE_KEYS.contains(&k.as_str())) {
            return Err((
                REFUSED,
                format!("invalid_params: {key} is not a session option this gateway serves"),
            ));
        }
        if !matches!(params.get("request_id"), Some(Value::String(r)) if !r.is_empty()) {
            return Err((
                REFUSED,
                "invalid_params: request_id must be a non-empty string".into(),
            ));
        }
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
        let id = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(LiveSession::default());
        self.lock_sessions()
            .insert(id.clone(), Arc::clone(&session));
        Ok(self.snapshot(&id, &session, replay_epoch, Vec::new(), false))
    }

    /// Attaches to a live session, or to one saved earlier (a restarted gateway's).
    fn canonical_resume(
        &self,
        params: &Map<String, Value>,
        replay_epoch: &str,
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
                None => Arc::clone(sessions.entry(id.clone()).or_default()),
            }
        };
        let omit = params.get("omit_messages") == Some(&Value::Bool(true));
        Ok(self.snapshot(id, &session, replay_epoch, rows, omit))
    }

    fn canonical_submit(
        self: &Arc<Self>,
        params: &Map<String, Value>,
        outbox: &Outbox,
        replay_epoch: &str,
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
            generation: 0,
            replay_epoch: replay_epoch.to_string(),
            input_id,
            text: text.clone(),
            profile_id: self.home.display().to_string(),
        };
        let reference = admission.reference(id);
        let admission_id = admission.id.clone();
        let generation = self.start_turn(id, text, outbox, Some(admission))?;
        Ok(json!({
            "admission_id": admission_id,
            "ref": reference,
            "sequence": generation,
            "status": "started",
            "outcome": null,
            "authority_epoch": AUTHORITY_EPOCH,
            "execution_generation": generation,
        }))
    }

    /// A session's snapshot, as upstream's `session.create` and `session.resume` answer: its saved
    /// user and assistant `rows`, or only their count when `omit` is set.
    fn snapshot(
        &self,
        id: &str,
        session: &LiveSession,
        replay_epoch: &str,
        rows: Vec<Value>,
        omit: bool,
    ) -> Value {
        let mut info = self.session_info();
        info["desktop_protocol"] = CANONICAL_PROTOCOL.into();
        info["profile_name"] = PROFILE.into();
        let count = rows.len();
        let mut snapshot = json!({
            "session_id": id,
            "stored_session_id": id,
            "messages": if omit { Vec::new() } else { rows },
            "message_count": count,
            "running": session.busy.load(Ordering::Acquire),
            "authority_epoch": AUTHORITY_EPOCH,
            "replay_epoch": replay_epoch,
            "last_sequence": session.seq.load(Ordering::Acquire),
            "revision": 0,
            "execution_generation": session.generation.load(Ordering::Acquire),
            "pending": [],
            "prompts": [],
            "info": info,
        });
        if omit {
            snapshot["messages_omitted"] = true.into();
        }
        snapshot
    }
}
