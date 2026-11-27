//! A session for a second window or a reconnecting client, on the legacy wire: `session.resume`
//! (attach to a live or saved session) and `session.title` (read or set its title), as
//! upstream's tui_gateway/methods_session.py answers them.
//!
//! Differences from upstream at `ae2a81cc`: resume serves a live session or its saved rows
//! only (no lookup by title, no compression tips, no lazy or deferred modes) and its reply has
//! no `started_at` and an `inflight` of null, and its `session_id` is the saved session's id, as
//! in `resumed` and `session_key` (this gateway mints no separate runtime id). A title is not
//! refused for being another session's (this store keeps no title index), one that cleans to
//! nothing is refused as missing rather than clearing the title, no `session.info` event
//! follows a title change, as this gateway's legacy wire sends none, and a title can be set
//! whilst a turn runs, as upstream allows (a turn's save keeps the title).
//!
//! A resume during a turn streams that turn's events from its subscription on. The events
//! sent before it are not replayed in the reply: each event carries its `seq`, so the client
//! asks `session.events.since` for them, and the turn's `message.complete` carries its whole
//! reply.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::{
    Chat, Handler, LiveSession, Reply, Request, RpcError, INVALID_PARAMS, SESSION_NOT_FOUND,
};

// Upstream's codes: 4006 no session id, 4007 no such session to resume, 4021 no title, 4022 a
// title too long, 5000 the sessions database cannot be read on resume, 5007 a title cannot be
// read or stored.
const SESSION_ID_REQUIRED: i64 = 4006;
const RESUME_NOT_FOUND: i64 = 4007;
const TITLE_REQUIRED: i64 = 4021;
const TITLE_INVALID: i64 = 4022;
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

/// Session `id`'s saved user and assistant messages, read on the blocking pool.
async fn saved_rows(chat: &Arc<Chat>, id: &str) -> Result<Vec<Value>, RpcError> {
    let (reader, lookup) = (Arc::clone(chat), id.to_string());
    // A read that panics is a storage failure, as a store error is.
    match chat.with_store(move |_| reader.history(&lookup)).await {
        Ok(None) => Err((RESUME_NOT_FOUND, "session not found".into())),
        Ok(Some(Ok(rows))) => Ok(rows),
        Ok(Some(Err(e))) | Err(e) => Err((RESUME_STORAGE, e)),
    }
}

/// Attaches the connection to a live session, or to one saved earlier, and answers with its
/// saved messages (each with `text`, which the terminal UI reads, and `content`).
fn resume(req: Request) -> Reply {
    Box::pin(async move {
        let id = session_id(&req.params)?;
        // Nothing changes before the first wait, so a handler dropped there (a busy
        // connection's) leaves no subscription.
        tokio::task::yield_now().await;
        let live = req.chat.lock_sessions().get(&id).cloned();
        let session: Arc<LiveSession> = match live {
            Some(session) => session,
            None => {
                // Not live: its saved rows decide whether it can be resumed.
                let rows = saved_rows(&req.chat, &id).await?;
                // One made live during the read is taken as it is.
                let mut sessions = req.chat.lock_sessions();
                if rows.is_empty() && !sessions.contains_key(&id) {
                    return Err((RESUME_NOT_FOUND, "session not found".into()));
                }
                Arc::clone(
                    sessions
                        .entry(id.clone())
                        .or_insert_with(|| req.chat.new_session()),
                )
            }
        };
        // The session streams to this connection before the rows that answer are read, so a
        // turn that ends meanwhile, on this or another connection, is not lost: its last
        // events follow this reply, and may repeat what the rows already hold. The
        // subscription stays on a failed read: the connection may have streamed it already.
        session.subscribe(req.conn.id, &req.conn.outbox);
        let rows = saved_rows(&req.chat, &id).await?;
        let ((running, _, _), _) = session.status(&id);
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
            "session_key": id,
            "running": running,
            "status": if running { "working" } else { "idle" },
            "inflight": null,
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
            Value::String(title) => clean_title(title)?,
            Value::Null => String::new(),
            _ => return Err((INVALID_PARAMS, "title must be a string".into())),
        };
        if title.is_empty() {
            return Err((TITLE_REQUIRED, "title required".into()));
        }
        // A request its connection answers busy is dropped at its first poll: wait once here,
        // so the store write below is never queued for a request answered busy.
        tokio::task::yield_now().await;
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

/// Upstream's longest title, in characters (hermes_state.py `MAX_TITLE_LENGTH`).
const MAX_TITLE_CHARS: usize = 100;

/// `title` as upstream's `sanitize_title` stores it: without ASCII controls (tab and line
/// breaks count as whitespace) or zero-width, bidi-override, object-replacement and annotation
/// characters, each run of whitespace one space, trimmed; longer than [`MAX_TITLE_CHARS`] is
/// refused with 4022.
fn clean_title(title: &str) -> Result<String, RpcError> {
    let invisible = |c: &char| {
        matches!(c,
            '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'
            | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2069}'
            | '\u{feff}' | '\u{fff9}'..='\u{fffc}')
    };
    let kept: String = title.chars().filter(|c| !invisible(c)).collect();
    let cleaned = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = cleaned.chars().count();
    if length > MAX_TITLE_CHARS {
        return Err((
            TITLE_INVALID,
            format!("Title too long ({length} chars, max {MAX_TITLE_CHARS})"),
        ));
    }
    Ok(cleaned)
}
