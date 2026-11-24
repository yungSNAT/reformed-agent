//! Replies a gateway restart would otherwise lose.
//!
//! Before a turn runs, the gateway writes a record for its session under
//! `<hermes home>/gateway/pending-replies/`; when the agent answers, the
//! record keeps the reply; once the reply is sent, the record goes. A turn
//! that fails deletes its record: the user already gets the error notice.
//! After a restart the gateway handles each record another process left:
//! a stored reply is sent, and a turn that never answered gets one notice
//! asking the user to send the message again. The turn is not run again,
//! as that could repeat its tool calls (commands, file writes, messages).
//!
//! Each send is counted in the record before it is made, and a record is
//! dropped after [`MAX_SEND_ATTEMPTS`]. Delivery is at least once, never
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

/// Write `record` whole or not at all; owner-only on Unix.
pub fn write(dir: &Path, record: &PendingReply) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::debug!(error = %e, "pending reply dir create failed");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = record_path(dir, &record.session_key);
    let body = match serde_json::to_vec_pretty(record) {
        Ok(body) => body,
        Err(e) => {
            tracing::debug!(error = %e, "pending reply serialize failed");
            return;
        }
    };
    if let Err(e) = hermes_config::atomic_write_bytes(&path, &body) {
        tracing::warn!(error = %e, path = %path.display(), "pending reply write failed");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
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

fn read(path: &Path) -> Option<PendingReply> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Records other processes left, oldest name first.
pub fn left_by_others(dir: &Path) -> Vec<PendingReply> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let me = this_process();
    let mut records: Vec<PendingReply> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read(&e.path()))
        .filter(|r| r.process != me)
        .collect();
    records.sort_by(|a, b| a.session_key.cmp(&b.session_key));
    records
}

/// Count a send of `record` before it is made. Returns false (and drops the
/// record) once the cap is reached.
pub fn count_attempt(dir: &Path, record: &mut PendingReply) -> bool {
    if record.attempts >= MAX_SEND_ATTEMPTS {
        remove(dir, &record.session_key);
        return false;
    }
    record.attempts += 1;
    write(dir, record);
    true
}

/// Drop the record for `session_key`, whoever wrote it.
pub fn remove(dir: &Path, session_key: &str) {
    let _ = std::fs::remove_file(record_path(dir, session_key));
}

#[cfg(test)]
mod tests;
