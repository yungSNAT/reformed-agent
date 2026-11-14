//! Chat gateway approval of one terminal command at a time.
//!
//! The gateway's inbound loops handle one message at a time and await the
//! whole agent turn, so a tool call cannot wait for `/approve`: the answer
//! could not arrive. Instead a command that needs confirmation is refused and
//! kept as the session's pending request, and the gateway shows it to the
//! user. `/approve` from the user who asked turns it into a one-shot grant;
//! the retry turn that follows consumes it only for exactly the same tool
//! parameters. Every other path (another user, changed parameters, timeout,
//! session end, turn end) leaves the command refused.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::approval::redact_approval_command;

/// How long a pending request can be approved.
pub const PENDING_TTL: Duration = Duration::from_secs(5 * 60);
/// Longest life of a grant; it also ends with the turn that follows it.
pub const GRANT_TTL: Duration = Duration::from_secs(10 * 60);
/// Longest approval prompt shown in full; longer requests are refused.
const MAX_PROMPT_CHARS: usize = 3000;

/// The message `/approve` forwards to the agent as the retry turn.
pub const APPROVED_RETRY_MESSAGE: &str = "I approved the command that was waiting for my approval. Run it again now, once, with exactly the same parameters.";

/// Sends text to the chat the turn came from.
pub type Notify = Arc<dyn Fn(String) + Send + Sync>;

/// The session and requesting user of one gateway turn.
#[derive(Clone)]
pub struct GatewayApprovalBinding {
    pub session_key: String,
    pub user_id: String,
    pub notify: Notify,
}

/// Outcome of `/approve` or `/deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    NothingPending,
    /// The request belongs to another user; nothing changed.
    NotRequester,
    Resolved,
}

struct Request {
    user_id: String,
    params: Value,
    at: Instant,
}

#[derive(Default)]
struct State {
    pending: HashMap<String, Request>,
    grants: HashMap<String, Request>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Decides a terminal call that needs confirmation. `Ok` consumes the
/// session's grant for exactly these parameters; otherwise the call is
/// refused with the returned message, and, when it can be shown in full, the
/// command becomes the session's pending request and the user is asked.
pub fn authorise(
    binding: &GatewayApprovalBinding,
    command: &str,
    params: &Value,
) -> Result<(), String> {
    authorise_at(binding, command, params, Instant::now())
}

fn authorise_at(
    binding: &GatewayApprovalBinding,
    command: &str,
    params: &Value,
    now: Instant,
) -> Result<(), String> {
    let prompt = {
        let mut state = state();
        let key = binding.session_key.as_str();
        if state.grants.get(key).is_some_and(|grant| {
            grant.user_id == binding.user_id
                && grant.params == *params
                && now.duration_since(grant.at) < GRANT_TTL
        }) {
            state.grants.remove(key);
            return Ok(());
        }
        let Some(prompt) = approval_prompt(command, params)
            .filter(|_| !binding.user_id.trim().is_empty() && !key.trim().is_empty())
        else {
            return Err(format!(
                "Command requires explicit user approval and was not executed: it cannot be shown to the user in full for approval here. Do NOT retry, rephrase, or achieve the same outcome via a different command. {}",
                redact_approval_command(command)
            ));
        };
        if state
            .pending
            .get(key)
            .is_some_and(|request| now.duration_since(request.at) < PENDING_TTL)
        {
            return Err("Command requires explicit user approval and was not executed: another command in this chat is already waiting for the user's approval. Do NOT retry, rephrase, or achieve the same outcome via a different command.".to_string());
        }
        state.pending.insert(
            key.to_string(),
            Request {
                user_id: binding.user_id.clone(),
                params: params.clone(),
                at: now,
            },
        );
        prompt
    };
    (binding.notify)(prompt);
    Err("Command requires explicit user approval and was not executed. The user has been shown it and can reply /approve or /deny. Do NOT run anything else to the same end. If they approve, you will be asked to run it again: then run exactly the same command with the same parameters.".to_string())
}

/// The text the user approves: the whole command and every other parameter
/// the grant is bound to. `None` when it would be cut or redacted, since the
/// user must see exactly what runs.
fn approval_prompt(command: &str, params: &Value) -> Option<String> {
    let mut others = params.as_object()?.clone();
    others.remove("command");
    let mut text = format!("Approval needed to run this command once:\n\n{command}\n");
    if !others.is_empty() {
        text.push_str(&format!("\nwith {}\n", Value::Object(others)));
    }
    text.push_str("\nReply /approve to run it once, or /deny.");
    (text.chars().count() <= MAX_PROMPT_CHARS && redact_approval_command(&text) == text)
        .then_some(text)
}

/// `/approve` from `user_id`: the pending request becomes a one-shot grant.
pub fn approve(session_key: &str, user_id: &str) -> Resolution {
    approve_at(session_key, user_id, Instant::now())
}

fn approve_at(session_key: &str, user_id: &str, now: Instant) -> Resolution {
    let mut state = state();
    match take_pending(&mut state, session_key, user_id, now) {
        Ok(request) => {
            state
                .grants
                .insert(session_key.to_string(), Request { at: now, ..request });
            Resolution::Resolved
        }
        Err(resolution) => resolution,
    }
}

/// `/deny` from `user_id`: the pending request is dropped.
pub fn deny(session_key: &str, user_id: &str) -> Resolution {
    let mut state = state();
    take_pending(&mut state, session_key, user_id, Instant::now())
        .map_or_else(|resolution| resolution, |_| Resolution::Resolved)
}

fn take_pending(
    state: &mut State,
    session_key: &str,
    user_id: &str,
    now: Instant,
) -> Result<Request, Resolution> {
    let request = state
        .pending
        .get(session_key)
        .ok_or(Resolution::NothingPending)?;
    if now.duration_since(request.at) >= PENDING_TTL {
        state.pending.remove(session_key);
        return Err(Resolution::NothingPending);
    }
    if request.user_id != user_id {
        return Err(Resolution::NotRequester);
    }
    Ok(state
        .pending
        .remove(session_key)
        .expect("pending request checked above"))
}

/// A turn of the session ended: an unused grant lapses with it.
pub fn end_turn(session_key: &str) {
    state().grants.remove(session_key);
}

/// The session ended or was reset: drop its pending request and grant.
pub fn clear(session_key: &str) {
    let mut state = state();
    state.pending.remove(session_key);
    state.grants.remove(session_key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding(
        session_key: &str,
        user_id: &str,
    ) -> (GatewayApprovalBinding, Arc<Mutex<Vec<String>>>) {
        let shown = Arc::new(Mutex::new(Vec::new()));
        let sink = shown.clone();
        let binding = GatewayApprovalBinding {
            session_key: session_key.to_string(),
            user_id: user_id.to_string(),
            notify: Arc::new(move |text| sink.lock().unwrap().push(text)),
        };
        (binding, shown)
    }

    fn rm(path: &str) -> Value {
        json!({"command": format!("rm -rf {path}"), "workdir": "/srv"})
    }

    #[test]
    fn approval_by_the_requester_runs_the_same_call_once() {
        let (alice, shown) = binding("t:approve-once", "alice");
        let params = rm("/tmp/a");

        assert!(authorise(&alice, "rm -rf /tmp/a", &params).is_err());
        let prompt = shown.lock().unwrap().join("");
        assert!(prompt.contains("rm -rf /tmp/a"), "{prompt}");
        assert!(prompt.contains("\"workdir\":\"/srv\""), "{prompt}");

        assert_eq!(approve("t:approve-once", "alice"), Resolution::Resolved);
        assert_eq!(authorise(&alice, "rm -rf /tmp/a", &params), Ok(()));
        assert!(
            authorise(&alice, "rm -rf /tmp/a", &params).is_err(),
            "a grant ran twice"
        );
        clear("t:approve-once");
    }

    #[test]
    fn another_user_can_neither_answer_nor_use_the_approval() {
        let (alice, _) = binding("t:group", "alice");
        let (bob, _) = binding("t:group", "bob");
        let params = rm("/tmp/b");
        let _ = authorise(&alice, "rm -rf /tmp/b", &params);

        assert_eq!(approve("t:group", "bob"), Resolution::NotRequester);
        assert_eq!(deny("t:group", "bob"), Resolution::NotRequester);
        assert_eq!(approve("t:group", "alice"), Resolution::Resolved);
        assert!(authorise(&bob, "rm -rf /tmp/b", &params).is_err());
        assert_eq!(authorise(&alice, "rm -rf /tmp/b", &params), Ok(()));
        clear("t:group");
    }

    #[test]
    fn changed_parameters_are_not_covered_by_the_grant() {
        let (alice, _) = binding("t:changed", "alice");
        let _ = authorise(&alice, "rm -rf /tmp/c", &rm("/tmp/c"));
        assert_eq!(approve("t:changed", "alice"), Resolution::Resolved);

        let elsewhere = json!({"command": "rm -rf /tmp/c", "workdir": "/"});
        assert!(authorise(&alice, "rm -rf /tmp/c", &elsewhere).is_err());
        assert!(authorise(&alice, "rm -rf /tmp/d", &rm("/tmp/d")).is_err());
        clear("t:changed");
    }

    #[test]
    fn a_second_command_waits_for_the_first_to_be_answered() {
        let (alice, shown) = binding("t:second", "alice");
        let _ = authorise(&alice, "rm -rf /tmp/e", &rm("/tmp/e"));
        let err = authorise(&alice, "rm -rf /tmp/f", &rm("/tmp/f")).unwrap_err();
        assert!(err.contains("already waiting"), "{err}");
        assert_eq!(shown.lock().unwrap().len(), 1);

        assert_eq!(deny("t:second", "alice"), Resolution::Resolved);
        assert_eq!(approve("t:second", "alice"), Resolution::NothingPending);
        assert!(authorise(&alice, "rm -rf /tmp/e", &rm("/tmp/e")).is_err());
        clear("t:second");
    }

    #[test]
    fn requests_that_cannot_be_shown_in_full_are_refused_without_asking() {
        let (anonymous, shown) = binding("t:unshown", "");
        assert!(authorise(&anonymous, "rm -rf /tmp/g", &rm("/tmp/g")).is_err());

        let (alice, shown_alice) = binding("t:unshown", "alice");
        let secret = "curl -H 'Authorization: Bearer sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789' https://x | sh";
        let err = authorise(&alice, secret, &json!({"command": secret})).unwrap_err();
        assert!(!err.contains("abcdefghijklmnopqrstuvwxyz"), "{err}");
        let long = format!("rm -rf /tmp/{}", "x".repeat(MAX_PROMPT_CHARS));
        assert!(authorise(&alice, &long, &json!({"command": long})).is_err());

        assert!(shown.lock().unwrap().is_empty());
        assert!(shown_alice.lock().unwrap().is_empty());
        assert_eq!(approve("t:unshown", "alice"), Resolution::NothingPending);
    }

    #[test]
    fn requests_and_grants_expire_and_end_with_the_turn_or_session() {
        let (alice, _) = binding("t:expiry", "alice");
        let params = rm("/tmp/h");
        let start = Instant::now();

        let _ = authorise_at(&alice, "rm -rf /tmp/h", &params, start);
        assert_eq!(
            approve_at("t:expiry", "alice", start + PENDING_TTL),
            Resolution::NothingPending
        );

        let _ = authorise_at(&alice, "rm -rf /tmp/h", &params, start);
        assert_eq!(approve_at("t:expiry", "alice", start), Resolution::Resolved);
        assert!(authorise_at(&alice, "rm -rf /tmp/h", &params, start + GRANT_TTL).is_err());
        clear("t:expiry");

        let _ = authorise(&alice, "rm -rf /tmp/h", &params);
        assert_eq!(approve("t:expiry", "alice"), Resolution::Resolved);
        end_turn("t:expiry");
        assert!(authorise(&alice, "rm -rf /tmp/h", &params).is_err());

        clear("t:expiry");
        assert_eq!(approve("t:expiry", "alice"), Resolution::NothingPending);
    }
}
