//! `llm.oneshot`: one stateless model call outside any conversation, as the desktop asks for a
//! commit message or a project idea. Nothing is saved and no session changes.
//!
//! As upstream (`llm.oneshot`, tui_gateway/methods_session.py, and agent/oneshot.py), a
//! registered `template` with `variables`, or raw `instructions` and `input`, make the prompt,
//! and the reply's text comes back without a wrapping code fence. The call runs on this
//! gateway's configured model, the one its turns use; upstream's `task` routing to a separate
//! auxiliary model is not served, so `task` and `session_id` are accepted and unused.

use std::time::Duration;

use hermes_agent::{render_oneshot_template, strip_wrapping_code_fence, OneShotError};
use hermes_core::Message;
use serde_json::{json, Map, Value};

use super::{Handler, Reply, Request, RpcError};

pub(super) const METHODS: &[(&str, Handler)] = &[("llm.oneshot", oneshot)];

/// Upstream's codes (tui_gateway/methods_session.py `llm.oneshot`).
const PROMPT_REQUIRED: i64 = 4030;
const UNKNOWN_TEMPLATE: i64 = 4031;
const FAILED: i64 = 5030;

/// As upstream's `run_oneshot` default.
const TIMEOUT: Duration = Duration::from_secs(60);

fn oneshot(req: Request) -> Reply {
    Box::pin(async move {
        let (instructions, input, temperature, max_tokens) = prompt(&req.params)?;
        let failed = |why: String| (FAILED, format!("one-shot generation failed: {why}"));
        let model = req
            .chat
            .model()
            .ok_or_else(|| failed("no model is configured".into()))?
            .to_string();
        let mut messages = Vec::new();
        if !instructions.trim().is_empty() {
            messages.push(Message::system(instructions));
        }
        messages.push(Message::user(input));
        let provider = hermes_provider_runtime::build_provider(&req.chat.config, &model);
        let call = provider.chat_completion(
            &messages,
            &[],
            Some(max_tokens),
            Some(temperature),
            Some(&model),
            None,
        );
        let response = tokio::time::timeout(TIMEOUT, call)
            .await
            .map_err(|_| failed(format!("no reply within {} seconds", TIMEOUT.as_secs())))?
            .map_err(|e| failed(e.to_string()))?;
        let message = response.message;
        let text = message
            .content
            .filter(|text| !text.trim().is_empty())
            .or(message.reasoning_content)
            .unwrap_or_default();
        Ok(json!({"text": strip_wrapping_code_fence(&text)}))
    })
}

/// The system and user text, temperature and token limit `params` ask for, as upstream reads
/// them: a bad temperature or limit falls back to its default rather than refusing.
fn prompt(params: &Map<String, Value>) -> Result<(String, String, f64, u32), RpcError> {
    let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or_default();
    let template = text("template").trim();
    let (instructions, input) = if template.is_empty() {
        (text("instructions").to_string(), text("input").to_string())
    } else {
        let variables = match params.get("variables") {
            Some(Value::Object(variables)) => variables.clone(),
            _ => Map::new(),
        };
        render_oneshot_template(template, &variables).map_err(|e| match e {
            OneShotError::UnknownTemplate(name) => {
                (UNKNOWN_TEMPLATE, format!("unknown one-shot template: {name}"))
            }
            other => (PROMPT_REQUIRED, other.to_string()),
        })?
    };
    if instructions.trim().is_empty() && input.trim().is_empty() {
        return Err((
            PROMPT_REQUIRED,
            "llm.oneshot requires a template or instructions/input".into(),
        ));
    }
    let temperature = params
        .get("temperature")
        .and_then(Value::as_f64)
        .unwrap_or(0.3);
    let max_tokens = params
        .get("max_tokens")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .map_or(1024, |n| u32::try_from(n).unwrap_or(u32::MAX));
    Ok((instructions, input, temperature, max_tokens))
}
