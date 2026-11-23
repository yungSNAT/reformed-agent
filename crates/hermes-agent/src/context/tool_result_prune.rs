//! Archive-before-stub pruning of old tool results.
//!
//! A long session carries every tool result it ever produced. Once the
//! provider's prompt cache has expired, rewriting old messages breaks no
//! cached prefix, so the turn that starts then shortens large old results
//! first. Compaction prunes the same way before it summarises.
//!
//! No result loses its text: each original is written to
//! `<hermes home>/cache/pruned/` (owner-only on Unix) and read back before
//! its message is shortened, and the stub that replaces it names the file for
//! `read_file`. A result whose write fails stays whole. Archives are made only
//! when `read_file` runs on this machine, and go with their session when it is
//! deleted.

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
/// Past this size a stub says to read the archive in parts.
const PAGED_RESULT_CHARS: usize = 100_000;
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

/// The archive a stub names: only an existing file, not a symlink, directly
/// inside a real archive folder (see [`is_real_archive_dir`]), so tool text
/// cannot pass off another path as one.
pub fn archive_pointer(content: &str) -> Option<&str> {
    if !has_archive_pointer(content) {
        return None;
    }
    let path = content.rsplit_once(ARCHIVE_POINTER_SEP)?.1.trim();
    let file = Path::new(path);
    let in_archive = file.is_absolute()
        && file.parent().is_some_and(is_real_archive_dir)
        && fs::symlink_metadata(file).is_ok_and(|m| m.is_file());
    in_archive.then_some(path)
}

/// True when `dir` is `cache/pruned` and resolves to that folder inside its
/// Hermes home: neither `cache` nor `pruned` leads elsewhere. The home itself
/// may be a symlink.
fn is_real_archive_dir(dir: &Path) -> bool {
    let Some(home) = dir.parent().and_then(Path::parent) else {
        return false;
    };
    // One component at a time: a canonical Windows path (`\\?\`) takes no `/`.
    let expected = |home: PathBuf| PRUNED_SUBDIR.split('/').fold(home, |p, part| p.join(part));
    dir.ends_with(PRUNED_SUBDIR)
        && matches!(
            (fs::canonicalize(dir), fs::canonicalize(home)),
            (Ok(real), Ok(home)) if real == expected(home)
        )
}

/// True when `read_file` reads this machine's files: no other terminal
/// backend (`TERMINAL_ENV`, then `TERMINAL_BACKEND`) is chosen. Elsewhere it
/// runs inside that backend and could not open an archive.
pub fn file_tools_read_this_machine() -> bool {
    ["TERMINAL_ENV", "TERMINAL_BACKEND"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .is_none_or(|name| name.eq_ignore_ascii_case("local"))
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

/// Start of every archive name of `session_id`: its readable part plus the
/// SHA-256 of the whole id. The readable part never holds a `.`, so no
/// session's prefix starts another's.
fn session_prefix(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    let hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}.{hash}_", safe_name_part(session_id))
}

/// Delete the archives of `session_id` in `dir`. Returns how many went.
pub fn remove_session_archives(dir: &Path, session_id: &str) -> usize {
    let prefix = session_prefix(session_id);
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .filter(|e| fs::remove_file(e.path()).is_ok())
        .count()
}

/// Create `dir`, owner-only on Unix. Refuses a `dir` that leads outside its
/// Hermes home through a symlink, so archives stay where their stubs say.
fn create_archive_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    if !is_real_archive_dir(dir) {
        return Err(std::io::Error::other("the archive folder leads elsewhere"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
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
        "{}{stamp}_{}_{short}",
        session_prefix(session_id),
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
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        };
        let written = file
            .write_all(body.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        // Checked again after the write, in case the folder was swapped since.
        let in_place = is_real_archive_dir(dir)
            && fs::canonicalize(&path)
                .ok()
                .as_deref()
                .and_then(Path::parent)
                == fs::canonicalize(dir).ok().as_deref();
        if written.is_ok() && in_place && fs::read(&path).is_ok_and(|back| back == body.as_bytes())
        {
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
            let chars = body.chars().count();
            let paging = if chars > PAGED_RESULT_CHARS {
                "; read it in parts with offset and limit"
            } else {
                ""
            };
            let stub = format!(
                "[{name}]{} ({chars} chars result{paging})",
                stub_arguments(args)
            );
            Some((i, stub))
        })
        .collect();
    let saving = |rows: &[(usize, String)]| -> usize {
        rows.iter()
            .map(|(i, stub)| {
                let body = messages[*i].content.as_deref().unwrap_or("");
                body.chars().count().saturating_sub(stub.chars().count())
            })
            .sum()
    };
    if candidates.is_empty() || saving(&candidates) < min_reclaim_chars {
        return PruneOutcome::default();
    }
    if create_archive_dir(archive_dir).is_err() {
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
            body.chars().count().saturating_sub(text.chars().count())
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

/// Touches the session's clock when dropped, so a run that ends any way
/// (returned, failed, aborted) counts as a use of the cache.
pub struct CacheUse(Option<String>);

impl CacheUse {
    pub fn new(session_id: Option<&str>) -> Self {
        Self(session_id.map(str::to_string))
    }
}

impl Drop for CacheUse {
    fn drop(&mut self) {
        if let Some(session_id) = &self.0 {
            record_cache_touch(session_id);
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
mod tests;
