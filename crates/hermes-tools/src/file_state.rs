//! What each agent session has seen of each local file, so `write_file` can
//! refuse to replace content the session never saw whole, or that changed on
//! disk since it did, and `patch` can warn when its target changed since the
//! session's last read.
//!
//! A read counts only once the model has been given its text unaltered, and
//! each read is settled by its own call, never by another read of the file:
//!
//! 1. `read_file` notes the bytes it read under the call's id ([`note_read`]),
//!    checking that its text is exactly that version's text or page.
//! 2. The registry records whether it returned that text unaltered (not
//!    truncated, masked or redacted) and what it returned ([`delivered`]).
//!    For a caller that shows the registry's result as it is, that settles
//!    the read.
//! 3. When the agent loop called (its call id is in [`CALL_PARAM`]), the read
//!    is settled only once the loop hands over the result as the model will
//!    see it, after its own budget cut ([`settle_view`]); the read counts if
//!    that text is exactly what the registry returned.
//!
//! A file too large for one view counts once pages taken at one version cover
//! every line. A read never settled counts for nothing.
//!
//! State lives in this process only: a new process starts with none, so an
//! existing file is read again before `write_file` replaces it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use hermes_core::file_edit::{CALL_PARAM, SESSION_PARAM};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Sessions kept, files kept per session, page ranges kept per file and
/// unsettled reads kept; past these the least recently used session, file or
/// read is forgotten, or the pages start again.
const MAX_SESSIONS: usize = 1024;
const MAX_FILES: usize = 4096;
const MAX_PAGES: usize = 256;
const MAX_PENDING: usize = 1024;

/// Turns the refusal off, as upstream's variable of the same name does.
const DISABLE_ENV: &str = "HERMES_DISABLE_FILE_STATE_GUARD";

/// A file's bytes: their length and SHA-256.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Version {
    len: usize,
    sha256: [u8; 32],
}

impl Version {
    fn of(bytes: &[u8]) -> Self {
        Self {
            len: bytes.len(),
            sha256: Sha256::digest(bytes).into(),
        }
    }
}

#[derive(Default)]
struct Seen {
    /// The version the session last read, in any view.
    last_read: Option<Version>,
    /// The version whose every line the session was shown unaltered, or
    /// wrote itself.
    full: Option<Version>,
    /// Lines (from 0, end exclusive) shown unaltered at `pages_of`, merged.
    pages: Vec<(usize, usize)>,
    pages_of: Option<Version>,
    used: u64,
}

/// A read `read_file` returned that is yet to be settled.
struct Pending {
    session: String,
    file: PathBuf,
    version: Version,
    lines: usize,
    start: usize,
    end: usize,
    /// What the registry returned, once it has, for the agent loop to match.
    delivered: Option<Version>,
    used: u64,
}

#[derive(Default)]
struct Session {
    files: HashMap<PathBuf, Seen>,
    used: u64,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, Session>,
    /// Keyed by call id.
    pending: HashMap<String, Pending>,
    clock: u64,
}

impl State {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn seen(&mut self, session: &str, file: PathBuf) -> &mut Seen {
        let now = self.tick();
        if !self.sessions.contains_key(session) && self.sessions.len() >= MAX_SESSIONS {
            forget_least_used(&mut self.sessions, |s| s.used);
        }
        let session = self.sessions.entry(session.to_string()).or_default();
        session.used = now;
        if !session.files.contains_key(&file) && session.files.len() >= MAX_FILES {
            forget_least_used(&mut session.files, |s| s.used);
        }
        let seen = session.files.entry(file).or_default();
        seen.used = now;
        seen
    }

    fn peek(&self, session: &str, file: &Path) -> (Option<Version>, Option<Version>) {
        self.sessions
            .get(session)
            .and_then(|s| s.files.get(file))
            .map_or((None, None), |seen| (seen.full, seen.last_read))
    }

    /// Counts `read` as shown unaltered.
    fn credit(&mut self, read: Pending) {
        let seen = self.seen(&read.session, read.file);
        if seen.pages_of != Some(read.version) || seen.pages.len() >= MAX_PAGES {
            seen.pages_of = Some(read.version);
            seen.pages.clear();
        }
        seen.pages.push((read.start, read.end));
        seen.pages.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(seen.pages.len());
        for &(start, end) in &seen.pages {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        if read.lines == 0 || merged.first() == Some(&(0, read.lines)) {
            seen.full = Some(read.version);
        }
        seen.pages = merged;
    }
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Mutex::default);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn forget_least_used<V>(
    map: &mut HashMap<impl Clone + Eq + std::hash::Hash, V>,
    used: fn(&V) -> u64,
) {
    if let Some(key) = map
        .iter()
        .min_by_key(|(_, v)| used(v))
        .map(|(k, _)| k.clone())
    {
        map.remove(&key);
    }
}

fn session(params: &Value) -> &str {
    params
        .get(SESSION_PARAM)
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .unwrap_or("default")
}

fn call(params: &Value) -> Option<&str> {
    params
        .get(CALL_PARAM)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

async fn current(file: &Path) -> Option<Version> {
    tokio::fs::read(file).await.ok().map(|b| Version::of(&b))
}

/// Gives a `read_file` call its own id when the agent loop gave it none, so
/// the read is settled by this call alone. Returns whether the agent loop
/// settles it ([`settle_view`]) rather than the registry.
pub fn prepare_call(tool: &str, params: &mut Value) -> bool {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if tool != "read_file" {
        return false;
    }
    if call(params).is_some() {
        return true;
    }
    if let Some(args) = params.as_object_mut() {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        args.insert(CALL_PARAM.to_string(), Value::from(format!("registry:{n}")));
    }
    false
}

/// The file a local file tool's `path` names, links resolved, or None when the
/// terminal backend is not local (its files are not on this machine) or the
/// path cannot be resolved here.
pub async fn tracked_file(path: &str) -> Option<PathBuf> {
    if crate::terminal_requirements::non_local_terminal_backend().is_some() {
        return None;
    }
    let path = crate::credential_guard::local_write_path(Path::new(path))?;
    Some(hermes_core::file_edit::edited_file(&path).await)
}

/// Notes that `read_file` call `params` returned `content` from `file`, read
/// from line `offset` (from 0) for `limit` lines. It counts only once settled,
/// and only if `content` is exactly the file's current text, or those lines.
pub async fn note_read(
    params: &Value,
    file: &Path,
    offset: Option<u64>,
    limit: Option<u64>,
    content: &str,
) {
    hermes_core::file_edit::set_view_settler(settle_view);
    let Some(call) = call(params) else {
        return;
    };
    let Ok(bytes) = tokio::fs::read(file).await else {
        return;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    // The local backend's slicing: whole text, or lines joined by '\n'.
    let (start, end, matches) = if offset.is_none() && limit.is_none() {
        (0, lines.len(), content == text)
    } else {
        let start =
            usize::try_from(offset.unwrap_or(0)).map_or(lines.len(), |s| s.min(lines.len()));
        let end = limit.map_or(lines.len(), |limit| {
            usize::try_from(limit).map_or(lines.len(), |l| start.saturating_add(l).min(lines.len()))
        });
        (start, end, content == lines[start..end].join("\n"))
    };
    let mut state = state();
    // Otherwise the file changed between the backend's read and this one: the
    // version the model saw is unknown, so the read counts for nothing.
    state.pending.remove(call);
    if matches {
        if state.pending.len() >= MAX_PENDING {
            forget_least_used(&mut state.pending, |p| p.used);
        }
        let used = state.tick();
        let read = Pending {
            session: session(params).to_string(),
            file: file.to_path_buf(),
            version: Version::of(&bytes),
            lines: lines.len(),
            start,
            end,
            delivered: None,
            used,
        };
        state.pending.insert(call.to_string(), read);
    }
}

/// Records what dispatch returned for the `tool` call `params`: `intact` when
/// it is the handler's text unaltered, `returned` the exact string returned.
/// With `deferred` (see [`prepare_call`]) the agent loop settles the read;
/// otherwise this does. Any tool but `read_file` is ignored.
pub fn delivered(tool: &str, params: &Value, intact: bool, returned: &str, deferred: bool) {
    if tool != "read_file" {
        return;
    }
    let Some(call) = call(params) else {
        return;
    };
    let mut state = state();
    let Some(mut read) = state.pending.remove(call) else {
        return;
    };
    let file = read.file.clone();
    state.seen(&read.session, file).last_read = Some(read.version);
    if !intact {
        return;
    }
    if deferred {
        read.delivered = Some(Version::of(returned.as_bytes()));
        state.pending.insert(call.to_string(), read);
    } else {
        state.credit(read);
    }
}

/// Settles the agent loop's call `call_id` with `shown`, its result as the
/// model will see it: the read counts only if `shown` is exactly what the
/// registry returned.
pub fn settle_view(call_id: &str, shown: &str) {
    let mut state = state();
    let Some(read) = state.pending.remove(call_id) else {
        return;
    };
    if read.delivered == Some(Version::of(shown.as_bytes())) {
        state.credit(read);
    }
}

/// Why `write_file` must not replace `file` (named `path` by the model), or
/// None. Call it holding the file's edit lock. A file that does not exist, or
/// is not a regular file, is left to the backend.
pub async fn write_refusal(params: &Value, path: &str, file: &Path) -> Option<String> {
    if std::env::var(DISABLE_ENV).is_ok_and(|v| v.trim() == "1") {
        return None;
    }
    match tokio::fs::metadata(file).await {
        Ok(meta) if meta.is_file() => {}
        _ => return None,
    }
    let bytes = tokio::fs::read(file).await.ok();
    let current = bytes.as_deref().map(Version::of);
    let (full, last_read) = state().peek(session(params), file);
    if current.is_some() && full == current {
        return None;
    }
    if bytes
        .as_deref()
        .is_some_and(|b| std::str::from_utf8(b).is_err())
    {
        return Some(format!(
            "Refusing to overwrite {path}: it is not UTF-8 text, so read_file cannot show it and \
             this session cannot have seen it. The file was NOT modified. Replace it with the \
             terminal only if the user means its current content to be discarded."
        ));
    }
    let reason = if last_read.is_some() && last_read != current {
        format!(
            "{path} was modified since you last read it (external edit or concurrent agent). \
             Re-read the file before writing."
        )
    } else {
        format!(
            "{path} exists but this session has not seen its full current content (never read, \
             patched without a later full read, or only a partial, truncated or redacted view). \
             Read the file (every page of it, if it needs offset/limit) or use patch for a \
             targeted edit; a stale conversation copy must not overwrite the current disk \
             content. If read_file cannot show it whole (a secret is redacted from it, or a \
             line is too long), edit it with patch."
        )
    };
    Some(format!(
        "Refusing to overwrite {path}: {reason} The file was NOT modified. Reload the current \
         contents with read_file (every page, for a file that needs offset/limit), merge the \
         requested change, then call write_file again. For small edits, prefer patch so existing \
         unrelated changes are preserved."
    ))
}

/// Notes the session's own successful `write_file` of `content` to `file`:
/// the session knows the whole file while the disk holds exactly that.
pub async fn note_write(params: &Value, file: &Path, content: &str) {
    let on_disk = current(file).await;
    let written = Version::of(content.as_bytes());
    let mut state = state();
    let seen = state.seen(session(params), file.to_path_buf());
    if on_disk == Some(written) {
        seen.full = Some(written);
        seen.last_read = Some(written);
    } else {
        seen.full = None;
    }
}

/// The warning for a `patch` of `file` (named `path`) that changed since the
/// session last read it, or None. A file never read gets none.
pub async fn patch_warning(params: &Value, path: &str, file: &Path) -> Option<String> {
    let (_, last_read) = state().peek(session(params), file);
    let last_read = last_read?;
    (current(file).await != Some(last_read)).then(|| {
        format!(
            "Warning: {path} was modified since you last read it (external edit or concurrent \
             agent). The content you read may be stale. Consider re-reading the file to verify \
             before writing."
        )
    })
}

/// Notes the session's own successful `patch` of `file`. The session saw only
/// what it replaced, so a later `write_file` needs a fresh full read.
pub async fn note_patch(params: &Value, file: &Path) {
    let now = current(file).await;
    let mut state = state();
    let seen = state.seen(session(params), file.to_path_buf());
    seen.full = None;
    seen.last_read = now;
}
