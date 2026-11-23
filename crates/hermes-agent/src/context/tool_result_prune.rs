//! Archive-before-stub pruning of old tool results.
//!
//! A long session carries every tool result it ever produced. Once the
//! provider's prompt cache has expired, rewriting old messages breaks no
//! cached prefix, so the turn that starts then shortens large old results
//! first. Compaction prunes the same way before it summarises.
//!
//! No result loses its text: each original is written to
//! `<hermes home>/cache/pruned/` and read back before its message is
//! shortened, and the stub that replaces it names the file for `read_file`.
//! A result whose write fails stays whole.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use hermes_core::{Message, MessageRole};
use sha2::{Digest, Sha256};

/// Separates a stub from the path of its archived original.
pub const ARCHIVE_POINTER_SEP: &str = " — full result: ";
/// Where archives go, under the Hermes home.
pub const PRUNED_SUBDIR: &str = "cache/pruned";
/// A result shorter than this many characters is left whole.
pub const MIN_RESULT_CHARS: usize = 8_000;
/// The newest this many messages are left whole.
pub const PROTECT_LAST_N: usize = 20;
/// On an expired cache a prune commits only when it frees at least this many
/// characters (16,000 tokens at 4 characters a token).
pub const COLD_MIN_RECLAIM_CHARS: usize = 64_000;
/// Heading of the archive list a compaction summary carries.
pub const ARCHIVE_LIST_HEADING: &str =
    "Archived tool output (exact originals; read_file a path to recover one):";

const STUB_ARGS_MAX: usize = 120;
const CLOCK_SESSIONS_MAX: usize = 1_000;
/// Claude's cache markers ask for the default 5-minute entry.
const ANTHROPIC_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// Other providers' automatic prefix caches, as the upstream clock declares them.
const AUTOMATIC_CACHE_TTL: Duration = Duration::from_secs(30 * 60);

/// What one prune changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PruneOutcome {
    /// Results shortened.
    pub pruned: usize,
    /// Characters removed from the history.
    pub reclaimed_chars: usize,
}

/// True for a stub that already names its archived original.
pub fn has_archive_pointer(content: &str) -> bool {
    content.len() < 2_000 && content.contains(ARCHIVE_POINTER_SEP)
}

/// The archive path a stub names, if it is one.
pub fn archive_pointer(content: &str) -> Option<&str> {
    if !has_archive_pointer(content) {
        return None;
    }
    content
        .rsplit_once(ARCHIVE_POINTER_SEP)
        .map(|(_, path)| path.trim())
        .filter(|path| !path.is_empty())
}

/// Index after the last message a prune may shorten: before the newest
/// [`PROTECT_LAST_N`] messages and the turn from `turn_start` on, and never
/// inside one assistant step's results.
fn prune_end(messages: &[Message], turn_start: usize) -> usize {
    let mut end = messages
        .len()
        .saturating_sub(PROTECT_LAST_N)
        .min(turn_start);
    while end > 0 && messages[end].role == MessageRole::Tool {
        end -= 1;
    }
    end
}

/// `" key=value"` pairs of a call's arguments, at most [`STUB_ARGS_MAX`] characters.
fn stub_arguments(arguments: &str) -> String {
    let Ok(serde_json::Value::Object(args)) = serde_json::from_str(arguments) else {
        return String::new();
    };
    let mut out = String::new();
    for (key, value) in args {
        let text = match value {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        let preview: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&format!(
            " {key}={}",
            preview.chars().take(40).collect::<String>()
        ));
        if out.chars().count() >= STUB_ARGS_MAX {
            return out.chars().take(STUB_ARGS_MAX).collect();
        }
    }
    out
}

fn safe_name_part(text: &str) -> String {
    let part: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    if part.is_empty() {
        "x".to_string()
    } else {
        part
    }
}

/// Write `body` to a new file in `dir` and confirm it reads back the same.
fn write_archive(
    dir: &Path,
    session_id: &str,
    tool_name: &str,
    call_id: &str,
    body: &str,
) -> Option<PathBuf> {
    let stamp = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ");
    let digest = Sha256::digest(call_id.as_bytes());
    let short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    let stem = format!(
        "{}_{stamp}_{}_{short}",
        safe_name_part(session_id),
        safe_name_part(tool_name)
    );
    // A resumed history can repeat a call id: take the first free name.
    for n in 0..1_000 {
        let name = if n == 0 {
            format!("{stem}.txt")
        } else {
            format!("{stem}_{n}.txt")
        };
        let path = dir.join(name);
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        };
        let written = file
            .write_all(body.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        if written.is_ok() && fs::read(&path).is_ok_and(|back| back == body.as_bytes()) {
            return Some(path);
        }
        let _ = fs::remove_file(&path);
        return None;
    }
    None
}

/// Shorten tool results older than both the newest [`PROTECT_LAST_N`]
/// messages and the turn starting at `turn_start`, when together they free at
/// least `min_reclaim_chars`. Each original is archived in `archive_dir`
/// first; its stub names the file. Returns what changed (nothing when the
/// gate is not met).
pub fn prune_tool_results(
    messages: &mut [Message],
    turn_start: usize,
    archive_dir: &Path,
    session_id: &str,
    min_reclaim_chars: usize,
) -> PruneOutcome {
    let end = prune_end(messages, turn_start);
    let calls: HashMap<&str, (&str, &str)> = messages[..end]
        .iter()
        .flat_map(|m| m.tool_calls.iter().flatten())
        .map(|c| {
            (
                c.id.as_str(),
                (c.function.name.as_str(), c.function.arguments.as_str()),
            )
        })
        .collect();
    let candidates: Vec<(usize, String)> = messages[..end]
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let body = m.content.as_deref()?;
            if m.role != MessageRole::Tool
                || body.chars().count() < MIN_RESULT_CHARS
                || has_archive_pointer(body)
            {
                return None;
            }
            let call_id = m.tool_call_id.as_deref().unwrap_or("");
            let (name, args) = calls
                .get(call_id)
                .copied()
                .unwrap_or((m.name.as_deref().unwrap_or("tool"), ""));
            let stub = format!(
                "[{name}]{} ({} chars result)",
                stub_arguments(args),
                body.chars().count()
            );
            Some((i, stub))
        })
        .collect();
    let saving = |rows: &[(usize, String)]| -> usize {
        rows.iter()
            .map(|(i, stub)| {
                let body = messages[*i].content.as_deref().unwrap_or("");
                body.len().saturating_sub(stub.len())
            })
            .sum()
    };
    if candidates.is_empty() || saving(&candidates) < min_reclaim_chars {
        return PruneOutcome::default();
    }
    if fs::create_dir_all(archive_dir).is_err() {
        return PruneOutcome::default();
    }

    let mut done: Vec<(usize, String, PathBuf)> = Vec::new();
    for (i, stub) in candidates {
        let msg = &messages[i];
        let call_id = msg.tool_call_id.as_deref().unwrap_or("");
        let name = calls
            .get(call_id)
            .map(|(name, _)| *name)
            .or(msg.name.as_deref())
            .unwrap_or("tool");
        let body = msg.content.as_deref().unwrap_or("");
        if let Some(path) = write_archive(archive_dir, session_id, name, call_id, body) {
            let text = format!("{stub}{ARCHIVE_POINTER_SEP}{}", path.display());
            done.push((i, text, path));
        }
    }
    let reclaimed: usize = done
        .iter()
        .map(|(i, text, _)| {
            let body = messages[*i].content.as_deref().unwrap_or("");
            body.len().saturating_sub(text.len())
        })
        .sum();
    if done.is_empty() || reclaimed < min_reclaim_chars {
        for (_, _, path) in &done {
            let _ = fs::remove_file(path);
        }
        return PruneOutcome::default();
    }
    let pruned = done.len();
    for (i, text, _) in done {
        messages[i].content = Some(text);
    }
    PruneOutcome {
        pruned,
        reclaimed_chars: reclaimed,
    }
}

/// When each session last sent a request, newest kept; the prompt cache's clock.
static LAST_REQUEST: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record that `session_id` just used its prompt cache.
pub fn record_cache_touch(session_id: &str) {
    record_cache_touch_at(session_id, Instant::now());
}

pub(crate) fn record_cache_touch_at(session_id: &str, at: Instant) {
    if session_id.is_empty() {
        return;
    }
    let Ok(mut clock) = LAST_REQUEST.lock() else {
        return;
    };
    clock.insert(session_id.to_string(), at);
    if clock.len() > CLOCK_SESSIONS_MAX {
        if let Some(oldest) = clock
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(id, _)| id.clone())
        {
            clock.remove(&oldest);
        }
    }
}

/// How long `model`'s provider keeps a cached prefix after its last use.
pub fn cache_ttl_for_model(model: &str) -> Duration {
    if model.to_ascii_lowercase().contains("claude") {
        ANTHROPIC_CACHE_TTL
    } else {
        AUTOMATIC_CACHE_TTL
    }
}

/// True when this process saw `session_id` use its cache more than `ttl` ago.
/// An unknown clock is never cold: nothing acts on a guess.
pub fn cache_expired(session_id: &str, ttl: Duration) -> bool {
    LAST_REQUEST
        .lock()
        .ok()
        .and_then(|clock| clock.get(session_id).copied())
        .is_some_and(|at| at.elapsed() > ttl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{FunctionCall, ToolCall};

    fn call(id: &str, name: &str, args: &str) -> Message {
        let mut msg = Message::assistant("");
        msg.tool_calls = Some(vec![ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
            extra_content: None,
        }]);
        msg
    }

    /// One user turn with one large tool result, then `filler` short messages.
    fn history(results: &[(&str, usize)], filler: usize) -> Vec<Message> {
        let mut out = vec![Message::system("sys")];
        for (id, size) in results {
            out.push(Message::user(format!("do {id}")));
            out.push(call(id, "terminal", r#"{"command":"cat big.log"}"#));
            out.push(Message::tool_result(*id, "x".repeat(*size)));
            out.push(Message::assistant("done"));
        }
        for i in 0..filler {
            out.push(Message::user(format!("q{i}")));
            out.push(Message::assistant(format!("a{i}")));
        }
        out
    }

    #[test]
    fn archives_old_results_and_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut msgs = history(&[("c1", 70_000)], 12);
        let original = msgs[3].content.clone().unwrap();
        let turn_start = msgs.len() - 2;

        let out = prune_tool_results(&mut msgs, turn_start, dir.path(), "s1", 0);

        assert_eq!(out.pruned, 1);
        let stub = msgs[3].content.as_deref().unwrap();
        assert!(stub.starts_with("[terminal] command=cat big.log (70000 chars result)"));
        let path = archive_pointer(stub).expect("stub names its archive");
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert!(Path::new(path).starts_with(dir.path()));
        assert_eq!(out.reclaimed_chars, original.len() - stub.len());
    }

    #[test]
    fn leaves_the_newest_messages_and_the_running_turn_whole() {
        let dir = tempfile::tempdir().unwrap();
        // The result sits inside the newest 20 messages.
        let mut recent = history(&[("c1", 70_000)], 5);
        let len = recent.len();
        let out = prune_tool_results(&mut recent, len, dir.path(), "s", 0);
        assert_eq!(out, PruneOutcome::default());

        // Old enough by count, but inside the turn that is running.
        let mut running = history(&[("c1", 70_000)], 12);
        let out = prune_tool_results(&mut running, 1, dir.path(), "s", 0);
        assert_eq!(out, PruneOutcome::default());
        assert_eq!(running[3].content.as_deref().unwrap().len(), 70_000);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn never_splits_one_steps_results() {
        let dir = tempfile::tempdir().unwrap();
        let mut msgs = vec![Message::system("sys"), Message::user("go")];
        let mut step = call("a", "read_file", "{}");
        step.tool_calls
            .as_mut()
            .unwrap()
            .push(call("b", "read_file", "{}").tool_calls.unwrap().remove(0));
        msgs.push(step);
        msgs.push(Message::tool_result("a", "y".repeat(9_000)));
        msgs.push(Message::tool_result("b", "z".repeat(9_000)));
        for i in 0..19 {
            msgs.push(Message::assistant(format!("a{i}")));
        }
        // len - 20 lands on result "b": both results of the step stay whole.
        assert_eq!(
            msgs[msgs.len() - PROTECT_LAST_N].tool_call_id.as_deref(),
            Some("b")
        );
        let len = msgs.len();
        let out = prune_tool_results(&mut msgs, len, dir.path(), "s", 0);
        assert_eq!(out, PruneOutcome::default());
    }

    #[test]
    fn commits_nothing_below_the_reclaim_gate() {
        let dir = tempfile::tempdir().unwrap();
        let mut msgs = history(&[("c1", 20_000)], 12);
        let before = msgs.clone();
        let len = msgs.len();
        let out = prune_tool_results(&mut msgs, len, dir.path(), "s", COLD_MIN_RECLAIM_CHARS);
        assert_eq!(out, PruneOutcome::default());
        assert_eq!(msgs, before);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn keeps_results_whole_when_the_archive_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "not a folder").unwrap();
        let mut msgs = history(&[("c1", 70_000)], 12);
        let before = msgs.clone();
        let len = msgs.len();
        let out = prune_tool_results(&mut msgs, len, &blocker.join("pruned"), "s", 0);
        assert_eq!(out, PruneOutcome::default());
        assert_eq!(msgs, before);
    }

    #[test]
    fn skips_small_results_and_existing_stubs() {
        let dir = tempfile::tempdir().unwrap();
        let mut msgs = history(&[("c1", 70_000), ("c2", 500)], 12);
        let len = msgs.len();
        assert_eq!(
            prune_tool_results(&mut msgs, len, dir.path(), "s", 0).pruned,
            1
        );
        let stubbed = msgs.clone();
        assert_eq!(
            prune_tool_results(&mut msgs, len, dir.path(), "s", 0),
            PruneOutcome::default()
        );
        assert_eq!(msgs, stubbed);
        assert_eq!(msgs[7].content.as_deref().unwrap().len(), 500);
    }

    #[test]
    fn a_repeated_call_id_gets_its_own_archive() {
        let dir = tempfile::tempdir().unwrap();
        let mut msgs = history(&[("same", 9_000), ("same", 9_500)], 12);
        let len = msgs.len();
        assert_eq!(
            prune_tool_results(&mut msgs, len, dir.path(), "s", 0).pruned,
            2
        );
        let first = archive_pointer(msgs[3].content.as_deref().unwrap())
            .unwrap()
            .to_string();
        let second = archive_pointer(msgs[7].content.as_deref().unwrap())
            .unwrap()
            .to_string();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(second).unwrap().len(), 9_500);
    }

    #[test]
    fn a_compaction_summary_lists_the_archives_it_summarised() {
        let dir = tempfile::tempdir().unwrap();
        let mut msgs = history(&[("c1", 70_000)], 12);
        let len = msgs.len();
        prune_tool_results(&mut msgs, len, dir.path(), "s", 0);
        let path = archive_pointer(msgs[3].content.as_deref().unwrap())
            .unwrap()
            .to_string();
        let out = crate::context::ContextCompressor::new(4).compress(&msgs);
        let summary = out[1].content.as_deref().unwrap();
        let listed = format!("{ARCHIVE_LIST_HEADING}\n- {path}");
        assert!(summary.contains(&listed), "{summary}");
    }

    #[test]
    fn the_clock_is_cold_only_after_a_known_touch_ages_past_its_ttl() {
        let ttl = Duration::from_secs(300);
        assert!(!cache_expired("never-seen", ttl));
        record_cache_touch("warm");
        assert!(!cache_expired("warm", ttl));
        let past = Instant::now()
            .checked_sub(Duration::from_secs(301))
            .unwrap();
        record_cache_touch_at("cold", past);
        assert!(cache_expired("cold", ttl));
        record_cache_touch_at("", past);
        assert!(!cache_expired("", ttl));
        assert_eq!(
            cache_ttl_for_model("anthropic/claude-sonnet-5"),
            ANTHROPIC_CACHE_TTL
        );
        assert_eq!(cache_ttl_for_model("gpt-6.2"), AUTOMATIC_CACHE_TTL);
    }
}
