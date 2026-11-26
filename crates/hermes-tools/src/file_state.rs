//! What each agent session has seen of each local file, so `write_file` can
//! refuse to replace content the session never saw whole, or that changed on
//! disk since it did, and `patch` can warn when its target changed since the
//! session's last read.
//!
//! A read counts in 2 steps. `read_file` notes the bytes it read
//! ([`note_read`]), checking that its text is exactly that version's view;
//! then the registry, which knows whether the model was shown that text
//! unaltered (not truncated, masked or redacted), settles it
//! ([`settle_read`]). Only unaltered views count, and a file too large for one
//! view counts once pages taken at one version cover every line. A read the
//! registry never settles counts for nothing.
//!
//! State lives in this process only: a new process starts with none, so an
//! existing file is read again before `write_file` replaces it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use hermes_core::file_edit::SESSION_PARAM;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Sessions kept, files kept per session and page ranges kept per file; past
/// these the least recently used session or file is forgotten, or the pages
/// start again.
const MAX_SESSIONS: usize = 1024;
const MAX_FILES: usize = 4096;
const MAX_PAGES: usize = 256;

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

/// A read `read_file` returned that the registry has yet to settle.
struct Pending {
    file: PathBuf,
    version: Version,
    lines: usize,
    start: usize,
    end: usize,
}

#[derive(Default)]
struct Session {
    files: HashMap<PathBuf, Seen>,
    /// Keyed by the `path` argument as the model wrote it.
    pending: HashMap<String, Pending>,
    used: u64,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, Session>,
    clock: u64,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Mutex::default);

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

fn with_session<R>(params: &Value, f: impl FnOnce(&mut Session, u64) -> R) -> R {
    let key = params
        .get(SESSION_PARAM)
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .unwrap_or("default");
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    state.clock += 1;
    let now = state.clock;
    if !state.sessions.contains_key(key) && state.sessions.len() >= MAX_SESSIONS {
        forget_least_used(&mut state.sessions, |s| s.used);
    }
    let session = state.sessions.entry(key.to_string()).or_default();
    session.used = now;
    f(session, now)
}

fn seen(session: &mut Session, file: PathBuf, now: u64) -> &mut Seen {
    if !session.files.contains_key(&file) && session.files.len() >= MAX_FILES {
        forget_least_used(&mut session.files, |s| s.used);
    }
    let seen = session.files.entry(file).or_default();
    seen.used = now;
    seen
}

fn peek(params: &Value, file: &Path) -> (Option<Version>, Option<Version>) {
    with_session(params, |session, _| {
        session
            .files
            .get(file)
            .map_or((None, None), |seen| (seen.full, seen.last_read))
    })
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

/// Notes that `read_file` returned `content` for `path` (`file` on disk), read
/// from line `offset` (from 0) for `limit` lines. It counts only once
/// [`settle_read`] confirms the model was shown it unaltered, and only if
/// `content` is exactly the file's current text, or those lines of it.
pub async fn note_read(
    params: &Value,
    path: &str,
    file: &Path,
    offset: Option<u64>,
    limit: Option<u64>,
    content: &str,
) {
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
    with_session(params, |session, _| {
        if matches {
            let pending = Pending {
                file: file.to_path_buf(),
                version: Version::of(&bytes),
                lines: lines.len(),
                start,
                end,
            };
            session.pending.insert(path.to_string(), pending);
        } else {
            // The file changed between the backend's read and this one: the
            // version the model saw is unknown, so the read counts for nothing.
            session.pending.remove(path);
        }
    });
}

/// Settles the `read_file` call `params` that dispatch just finished:
/// `intact` when the model was shown the handler's text unaltered. Any other
/// tool is ignored.
pub fn settle_read(tool: &str, params: &Value, intact: bool) {
    if tool != "read_file" {
        return;
    }
    let Some(path) = params.get("path").and_then(Value::as_str) else {
        return;
    };
    with_session(params, |session, now| {
        let Some(read) = session.pending.remove(path) else {
            return;
        };
        let seen = seen(session, read.file, now);
        seen.last_read = Some(read.version);
        if !intact {
            return;
        }
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
    });
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
    let current = tokio::fs::read(file).await.ok().map(|b| Version::of(&b));
    let (full, last_read) = peek(params, file);
    if current.is_some() && full == current {
        return None;
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
             targeted edit; a stale conversation copy must not overwrite the current disk content."
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
    let on_disk = tokio::fs::read(file).await.ok().map(|b| Version::of(&b));
    let written = Version::of(content.as_bytes());
    with_session(params, |session, now| {
        let seen = seen(session, file.to_path_buf(), now);
        if on_disk == Some(written) {
            seen.full = Some(written);
            seen.last_read = Some(written);
        } else {
            seen.full = None;
        }
    });
}

/// The warning for a `patch` of `file` (named `path`) that changed since the
/// session last read it, or None. A file never read gets none.
pub async fn patch_warning(params: &Value, path: &str, file: &Path) -> Option<String> {
    let (_, last_read) = peek(params, file);
    let last_read = last_read?;
    let current = tokio::fs::read(file).await.ok().map(|b| Version::of(&b));
    (current != Some(last_read)).then(|| {
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
    let current = tokio::fs::read(file).await.ok().map(|b| Version::of(&b));
    with_session(params, |session, now| {
        let seen = seen(session, file.to_path_buf(), now);
        seen.full = None;
        seen.last_read = current;
    });
}
