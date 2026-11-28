//! `llm.oneshot`: one stateless model call outside any conversation, as the desktop asks for a
//! commit message or a project idea. Nothing is saved and no session changes.
//!
//! As upstream (`llm.oneshot`, tui_gateway/methods_session.py, and agent/oneshot.py), a
//! registered `template` with `variables`, or raw `instructions` and `input`, make the prompt,
//! and the reply's text comes back without inline reasoning blocks or a wrapping code fence
//! (a reply that is only reasoning falls back to `reasoning_content`). The call runs on this
//! gateway's configured model, the one its turns use; upstream's `task` routing to a separate
//! auxiliary model is not served, so `task` and `session_id` are accepted and unused.
//!
//! Limits upstream does not set (`llm.oneshot` at `ae2a81cc` has none beyond its template's
//! truncation and `run_oneshot`'s 60 seconds): at most [`MAX_RUNNING`] calls at once across the
//! gateway, instructions and input of at most [`MAX_TEXT_BYTES`] each, `max_tokens` at most
//! [`MAX_TOKENS`] and `temperature` within 0 to 2. A call whose connection closes is dropped
//! with the handler, which cancels the request to the model.

use std::time::Duration;

use hermes_agent::{render_oneshot_template, strip_wrapping_code_fence};
use hermes_core::Message;
use serde_json::{json, Map, Value};

use tokio::sync::Semaphore;

use super::{Handler, Reply, Request, RpcError, INVALID_PARAMS, NO_TURN_SLOT};

pub(super) const METHODS: &[(&str, Handler)] = &[("llm.oneshot", oneshot)];

/// Upstream's codes (tui_gateway/methods_session.py `llm.oneshot`).
const PROMPT_REQUIRED: i64 = 4030;
const UNKNOWN_TEMPLATE: i64 = 4031;
const FAILED: i64 = 5030;

/// As upstream's `run_oneshot` default.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Calls running at once across the gateway; past it a call is refused, as a turn is when
/// every turn slot is taken.
const MAX_RUNNING: usize = 4;
static RUNNING: Semaphore = Semaphore::const_new(MAX_RUNNING);

/// Largest `instructions` or `input`, in bytes, and largest `max_tokens`.
const MAX_TEXT_BYTES: usize = 64 << 10;
const MAX_TOKENS: u64 = 4096;

fn oneshot(req: Request) -> Reply {
    Box::pin(async move {
        let (instructions, input, temperature, max_tokens) = prompt(&req.params)?;
        let failed = |why: String| (FAILED, format!("one-shot generation failed: {why}"));
        let model = req
            .chat
            .model()
            .ok_or_else(|| failed("no model is configured".into()))?
            .to_string();
        let Ok(_running) = RUNNING.try_acquire() else {
            return Err((
                NO_TURN_SLOT,
                format!("{MAX_RUNNING} one-shot calls are running; send again when one ends"),
            ));
        };
        let mut messages = Vec::new();
        if !instructions.trim().is_empty() {
            messages.push(Message::system(instructions));
        }
        messages.push(Message::user(input));
        // The key a turn on this model would send: one saved, reloaded or disconnected since
        // start-up included.
        let config = req.chat.turn_config(&model).map_err(failed)?;
        let provider = hermes_provider_runtime::build_provider(&config, &model);
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
            .map(|text| without_reasoning(&text))
            .filter(|text| !text.is_empty())
            .or(message.reasoning_content)
            .unwrap_or_default();
        Ok(json!({"text": strip_wrapping_code_fence(&text)}))
    })
}

/// Upstream's reasoning tag names (agent/think_scrubber.py `THINK_TAG_NAMES` at `ae2a81cc`).
const THINK_TAGS: &[&str] = &[
    "think",
    "thinking",
    "reasoning",
    "thought",
    "REASONING_SCRATCHPAD",
    "思考",
    "反思",
    "推理",
    "推敲",
    "mm:think",
];

/// `text` without its closed reasoning blocks, trimmed, as upstream's
/// `extract_content_or_reasoning` cleans `content` (`_REASONING_BLOCK_PATTERNS`: each tag in
/// turn, case-insensitive, the shortest block from each opening tag). Like upstream's, it does
/// not tell Markdown code from prose, so a closed tag pair quoted in code goes too.
fn without_reasoning(text: &str) -> String {
    let mut text = text.trim().to_string();
    for tag in THINK_TAGS {
        let (open, close) = (
            format!("<{}>", tag.to_ascii_lowercase()),
            format!("</{}>", tag.to_ascii_lowercase()),
        );
        // ASCII lowercasing keeps every byte offset, so positions in `lower` index `text`.
        let lower = text.to_ascii_lowercase();
        let mut kept = String::with_capacity(text.len());
        let mut at = 0;
        while let Some(start) = lower[at..].find(&open).map(|i| at + i) {
            let body = start + open.len();
            let Some(len) = lower[body..].find(&close) else {
                break;
            };
            kept.push_str(&text[at..start]);
            at = body + len + close.len();
        }
        kept.push_str(&text[at..]);
        text = kept;
    }
    text.trim().to_string()
}

/// The system and user text, temperature and token limit `params` ask for, as upstream reads
/// them: a temperature or limit that is not a number falls back to its default rather than
/// refusing, and one out of range is brought within it. A decimal number sent as a string
/// (`"0.8"`, `"50"`) is read as upstream's `float()` and `int()` read it; a boolean, digits with
/// underscores and an integer string past `i64` take the default, where upstream reads them.
fn prompt(params: &Map<String, Value>) -> Result<(String, String, f64, u32), RpcError> {
    let text = |key: &str| match params.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(text)) => Ok(text.as_str()),
        Some(_) => Err((INVALID_PARAMS, format!("{key} must be a string"))),
    };
    let template = text("template")?.trim();
    let (instructions, input) = if template.is_empty() {
        (
            text("instructions")?.to_string(),
            text("input")?.to_string(),
        )
    } else {
        let none = Map::new();
        let variables = match params.get("variables") {
            Some(Value::Object(variables)) => variables,
            _ => &none,
        };
        // Its only error is an unknown template, whose message names it.
        render_oneshot_template(template, variables)
            .map_err(|e| (UNKNOWN_TEMPLATE, e.to_string()))?
    };
    for (key, text) in [("instructions", &instructions), ("input", &input)] {
        if text.len() > MAX_TEXT_BYTES {
            return Err((
                INVALID_PARAMS,
                format!("{key} longer than {MAX_TEXT_BYTES} bytes"),
            ));
        }
    }
    if instructions.trim().is_empty() && input.trim().is_empty() {
        return Err((
            PROMPT_REQUIRED,
            "llm.oneshot requires a template or instructions/input".into(),
        ));
    }
    let temperature = match params.get("temperature") {
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        Some(value) => value.as_f64(),
        None => None,
    }
    .filter(|t| t.is_finite())
    .unwrap_or(0.3)
    .clamp(0.0, 2.0);
    let max_tokens = match params.get("max_tokens") {
        Some(Value::String(text)) => text.trim().parse::<i64>().ok(),
        Some(value) => value.as_f64().map(|n| n.trunc() as i64),
        None => None,
    }
    .filter(|n| *n > 0)
    .map_or(1024, |n| n.min(MAX_TOKENS as i64) as u32);
    Ok((instructions, input, temperature, max_tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed blocks go, in any case; an opening tag with no close is kept with what follows,
    /// as upstream's patterns match only closed blocks.
    #[test]
    fn reasoning_blocks() {
        for (reply, kept) in [
            ("<think>plan</think> answer", "answer"),
            ("<THINK>plan</Think>answer<thought>x</thought>!", "answer!"),
            ("answer <think>never closed", "answer <think>never closed"),
            ("<think>a</think>b <think>open", "b <think>open"),
        ] {
            assert_eq!(without_reasoning(reply), kept, "{reply}");
        }
    }

    /// A temperature or token limit sent as a string is read as upstream's `float()` and
    /// `int()` read it, a fractional limit is truncated, and one that does not parse (or a
    /// temperature that is not finite) takes its default.
    #[test]
    fn numbers_sent_as_strings() {
        for (temperature, max_tokens, want) in [
            (json!("0.8"), json!("50"), (0.8, 50)),
            (json!(" 1.5 "), json!(" 7 "), (1.5, 7)),
            (json!("warm"), json!("5.5"), (0.3, 1024)),
            (json!("nan"), json!("0"), (0.3, 1024)),
            (json!(0.5), json!(20.9), (0.5, 20)),
        ] {
            let params =
                json!({"input": "x", "temperature": temperature, "max_tokens": max_tokens});
            let (_, _, t, n) = prompt(params.as_object().unwrap()).unwrap();
            assert_eq!((t, n), want, "{params}");
        }
    }

    /// An unknown template is refused with 4031 and its name.
    #[test]
    fn an_unknown_template_is_4031() {
        let params = json!({"template": "limerick"});
        let refused = prompt(params.as_object().unwrap()).unwrap_err();
        assert_eq!(
            refused,
            (
                UNKNOWN_TEMPLATE,
                "unknown one-shot template: limerick".into()
            )
        );
    }

    /// A model that never answers is given up on after 60 seconds (5030); the call holds a
    /// permit whilst it waits and returns it. The clock is paused, so the 60 seconds pass at once.
    #[tokio::test(start_paused = true)]
    async fn a_model_that_never_answers_times_out() {
        // The kernel completes connections to it; nothing reads them or answers.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.yaml"),
            format!(
                "model:\n  default: mock-model\n  provider: mock\nproviders:\n  mock:\n    \
                 api: http://{}/v1\n    name: Mock\n    api_mode: chat_completions\n    \
                 models:\n      mock-model: {{}}\n",
                silent.local_addr().unwrap()
            ),
        )
        .unwrap();
        let config = hermes_config::load_config(home.path().to_str()).unwrap();
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let request = Request {
            chat: std::sync::Arc::new(super::super::Chat::new(config)),
            conn: super::super::Conn::new(1, outbox, false),
            params: json!({"input": "hello"}).as_object().unwrap().clone(),
        };
        let mut call = oneshot(request);
        assert!(futures::poll!(&mut call).is_pending());
        assert_eq!(RUNNING.available_permits(), MAX_RUNNING - 1);
        let refused = call.await.unwrap_err();
        assert_eq!(
            refused,
            (
                FAILED,
                "one-shot generation failed: no reply within 60 seconds".into()
            )
        );
        assert_eq!(RUNNING.available_permits(), MAX_RUNNING);
    }
}
