//! A session for a second window or a reconnecting client, on the legacy wire: `session.resume`
//! (attach to a live or saved session) and `session.title` (read or set its title), as
//! upstream's tui_gateway/methods_session.py answers them.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::{Handler, LiveSession, Reply, Request, RpcError, INVALID_PARAMS, SESSION_NOT_FOUND};

// Upstream's codes: 4006 no session id, 4021 no title, 5000 the sessions database cannot be
// read on resume, 5007 a title cannot be read or stored.
const SESSION_ID_REQUIRED: i64 = 4006;
const TITLE_REQUIRED: i64 = 4021;
const RESUME_STORAGE: i64 = 5000;
const TITLE_STORAGE: i64 = 5007;

pub(super) const METHODS: &[(&str, Handler)] =
    &[("session.resume", resume), ("session.title", title)];

fn session_id(params: &Map<String, Value>) -> Result<String, RpcError> {
    match params.get("session_id") {
        Some(Value::String(id)) if !id.is_empty() => Ok(id.clone()),
        None | Some(Value::Null | Value::String(_)) => {
            Err((SESSION_ID_REQUIRED, "session_id required".into()))
        }
        Some(_) => Err((INVALID_PARAMS, "session_id must be a string".into())),
    }
}

/// Attaches the connection to a live session, or to one saved earlier, and answers with its
/// saved messages (each with `text`, which the terminal UI reads, and `content`).
fn resume(req: Request) -> Reply {
    Box::pin(async move {
        let id = session_id(&req.params)?;
        let chat = Arc::clone(&req.chat);
        let lookup = id.clone();
        let rows = match tokio::task::spawn_blocking(move || chat.history(&lookup)).await {
            Err(e) => return Err((RESUME_STORAGE, e.to_string())),
            Ok(None) => return Err((SESSION_NOT_FOUND, "session not found".into())),
            Ok(Some(Err(e))) => return Err((RESUME_STORAGE, e)),
            Ok(Some(Ok(rows))) => rows,
        };
        let session: Arc<LiveSession> = {
            let mut sessions = req.chat.lock_sessions();
            match sessions.get(&id) {
                Some(session) => Arc::clone(session),
                None if rows.is_empty() => {
                    return Err((SESSION_NOT_FOUND, "session not found".into()))
                }
                None => Arc::clone(
                    sessions
                        .entry(id.clone())
                        .or_insert_with(|| req.chat.new_session()),
                ),
            }
        };
        session.subscribe(req.conn.id, &req.conn.outbox);
        let messages: Vec<Value> = rows
            .into_iter()
            .map(|mut row| {
                row["text"] = row["content"].clone();
                row
            })
            .collect();
        Ok(json!({
            "session_id": id,
            "resumed": id,
            "message_count": messages.len(),
            "messages": messages,
            "info": req.chat.session_info(),
        }))
    })
}

/// Reads a live session's title (no `title`) or sets it. A session with no saved row yet gets
/// one now, holding the title, as upstream saves the row for an explicit title.
fn title(req: Request) -> Reply {
    Box::pin(async move {
        let id = session_id(&req.params)?;
        if !req.chat.lock_sessions().contains_key(&id) {
            return Err((SESSION_NOT_FOUND, "session not found".into()));
        }
        let Some(given) = req.params.get("title") else {
            let key = id.clone();
            let title = req
                .chat
                .with_store(move |store| store.get_session_title(&key))
                .await
                .and_then(|read| read.map_err(|e| e.to_string()))
                .map_err(|e| (TITLE_STORAGE, e))?;
            return Ok(json!({"title": title.unwrap_or_default(), "session_key": id}));
        };
        let title = match given {
            Value::String(title) => title.trim().to_string(),
            Value::Null => String::new(),
            _ => return Err((INVALID_PARAMS, "title must be a string".into())),
        };
        if title.is_empty() {
            return Err((TITLE_REQUIRED, "title required".into()));
        }
        let model = req.chat.model().map(str::to_string);
        let (key, value) = (id.clone(), title.clone());
        req.chat
            .write_store(move |store| {
                if store.update_session_title(&key, &value)? {
                    return Ok(());
                }
                store.persist_session(
                    &key,
                    &[],
                    model.as_deref(),
                    Some("desktop"),
                    Some(&value),
                    None,
                )
            })
            .await
            .and_then(|saved| saved.map_err(|e| e.to_string()))
            .map_err(|e| (TITLE_STORAGE, e))?;
        Ok(json!({"pending": false, "title": title, "session_key": id}))
    })
}
