//! Replies a gateway restart would otherwise lose.
//!
//! Before a turn runs, the gateway writes a record for its session under
//! `<hermes home>/gateway/pending-replies/`; when the agent answers, the
//! record keeps the reply; once the reply is sent, the record goes. A turn
//! that fails deletes its record: the user already gets the error notice.
//! At start, before any adapter routes a turn, the gateway moves the
//! records a stopped process left into `recovering/`, so this process's
//! turns never write over them. It then handles each: a stored reply is
//! sent, and a turn that never answered gets one notice asking the user
//! to send the message again. The turn is not run again,
//! as that could repeat its tool calls (commands, file writes, messages).
//!
//! Each send is counted in the record before it is made, failed sends are
//! retried in the same process, and a record is dropped after
//! [`MAX_SEND_ATTEMPTS`]. Delivery is at least once, never
//! exactly once: a process that stops after a send but before the record is
//! deleted sends it again on the next start. None is lost short of the cap.
//!
//! On Unix the records and their folder are owner-only. Elsewhere they take
//! the permissions the folder inherits (on Windows, the user profile's by
//! default).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Sends of one record before it is dropped.
pub const MAX_SEND_ATTEMPTS: u32 = 3;

/// What a user gets for a turn the restart cut off before it answered.
pub const INTERRUPTED_NOTICE: &str = "The gateway restarted before it answered your last \
     message, so nothing was done with it after that point. Send it again to retry.";

/// Heads a reply sent after the restart.
pub const RECOVERED_REPLY_HEADING: &str = "Reply recovered after a gateway restart:";

/// The folder records live in, under a Hermes home.
pub fn records_dir(hermes_home: &Path) -> PathBuf {
    hermes_home.join("gateway").join("pending-replies")
}

/// One turn in flight, or answered and not yet sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingReply {
    pub session_key: String,
    pub platform: String,
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// The process that wrote the record; a running gateway leaves its own
    /// records to the turns that own them.
    pub process: String,
    /// The agent's reply, once it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    /// Sends made from this record after a restart.
    #[serde(default)]
    pub attempts: u32,
}

impl PendingReply {
    /// The text a restart sends for this record.
    pub fn recovery_text(&self) -> String {
        match &self.reply {
            Some(reply) => format!("{RECOVERED_REPLY_HEADING}\n\n{reply}"),
            None => INTERRUPTED_NOTICE.to_string(),
        }
    }
}

/// This process, as records name it.
pub fn this_process() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!("{}-{nanos}", std::process::id())
    })
    .clone()
}

fn record_path(dir: &Path, session_key: &str) -> PathBuf {
    let digest = Sha256::digest(session_key.as_bytes());
    let name: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{name}.json"))
}

/// Where a start moves the records earlier processes left, so turns of this
/// process never write over them.
pub fn recovering_dir(dir: &Path) -> PathBuf {
    dir.join("recovering")
}

fn private_dir(dir: &Path) -> bool {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::debug!(error = %e, "pending reply dir create failed");
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    true
}

/// Write `record` to `path` whole or not at all; owner-only on Unix.
fn write_at(path: &Path, record: &PendingReply) -> bool {
    if !path.parent().is_some_and(private_dir) {
        return false;
    }
    let body = match serde_json::to_vec_pretty(record) {
        Ok(body) => body,
        Err(e) => {
            tracing::debug!(error = %e, "pending reply serialize failed");
            return false;
        }
    };
    if let Err(e) = hermes_config::atomic_write_bytes(path, &body) {
        tracing::warn!(error = %e, path = %path.display(), "pending reply write failed");
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    true
}

/// Write `record` as its session's turn in flight. False when it could not be
/// written.
pub fn write(dir: &Path, record: &PendingReply) -> bool {
    write_at(&record_path(dir, &record.session_key), record)
}

/// Keep `reply` in this process's record for `session_key`.
pub fn answered(dir: &Path, session_key: &str, reply: &str) {
    let Some(mut record) = read(&record_path(dir, session_key)) else {
        return;
    };
    if record.process != this_process() {
        return;
    }
    record.reply = Some(reply.to_string());
    write(dir, &record);
}

/// The turn's reply was sent, or the turn failed: drop this process's record.
pub fn finish(dir: &Path, session_key: &str) {
    let path = record_path(dir, session_key);
    if read(&path).is_some_and(|r| r.process == this_process()) {
        let _ = std::fs::remove_file(path);
    }
}

/// The record at `path`. One that does not parse is deleted, so its text
/// does not stay on disk.
fn read(path: &Path) -> Option<PendingReply> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(record) => Some(record),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "deleting an unreadable pending reply");
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
        .collect()
}

/// The process id a record's writer had.
fn writer_pid(record: &PendingReply) -> Option<u32> {
    record.process.split('-').next()?.parse().ok()
}

/// When the record's writer started.
fn writer_started(record: &PendingReply) -> Option<std::time::SystemTime> {
    let nanos: u64 = record.process.split_once('-')?.1.parse().ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(nanos))
}

/// When this host booted (Linux: `btime` in `/proc/stat`); `None` elsewhere.
pub fn boot_time() -> Option<std::time::SystemTime> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let secs: u64 = stat
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

/// At start, before any turn runs: move each record a stopped process left
/// into [`recovering_dir`]. Records this process wrote, and records whose
/// writer `process_alive` says is still running (another gateway, or an old
/// one still finishing a turn), stay. A writer that started before this
/// host booted has stopped, whatever process its pid now names. Returns
/// the number moved.
pub fn take_over(dir: &Path, process_alive: fn(u32) -> bool) -> usize {
    take_over_since(dir, process_alive, boot_time())
}

fn take_over_since(
    dir: &Path,
    process_alive: fn(u32) -> bool,
    booted_at: Option<std::time::SystemTime>,
) -> usize {
    let me = this_process();
    let to = recovering_dir(dir);
    let mut moved = 0;
    for path in json_files(dir) {
        let Some(record) = read(&path) else {
            continue;
        };
        if record.process == me {
            continue;
        }
        // A writer with this process's id ran before it (a container's pid 1).
        let before_boot = booted_at
            .zip(writer_started(&record))
            .is_some_and(|(boot, started)| started < boot);
        if !before_boot
            && writer_pid(&record)
                .is_some_and(|pid| pid != std::process::id() && process_alive(pid))
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let writer: String = record
            .process
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        if !private_dir(&to) {
            return moved;
        }
        match std::fs::rename(&path, to.join(format!("{stem}-{writer}.json"))) {
            Ok(()) => moved += 1,
            Err(e) => tracing::warn!(error = %e, "pending reply not taken over"),
        }
    }
    moved
}

/// Records taken over at start and not yet sent, by session key.
pub fn recovering(dir: &Path) -> Vec<(PathBuf, PendingReply)> {
    let mut records: Vec<(PathBuf, PendingReply)> = json_files(&recovering_dir(dir))
        .into_iter()
        .filter_map(|path| read(&path).map(|record| (path, record)))
        .collect();
    records.sort_by(|a, b| a.1.session_key.cmp(&b.1.session_key));
    records
}

/// Count a send of the taken-over `record` at `path` before it is made.
/// Returns false, and the send must not be made, once the cap is reached (the
/// record is then dropped) or when the count could not be written.
pub fn count_attempt(path: &Path, record: &mut PendingReply) -> bool {
    if record.attempts >= MAX_SEND_ATTEMPTS {
        let _ = std::fs::remove_file(path);
        return false;
    }
    record.attempts += 1;
    write_at(path, record)
}

#[cfg(test)]
mod tests;
