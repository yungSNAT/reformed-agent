//! Replies a gateway restart would otherwise lose.
//!
//! Before a turn runs, the gateway writes one record per turn under
//! `<hermes home>/gateway/pending-replies/`; when the agent answers and the
//! gateway begins its reply send, the record saves the reply and reserves the
//! first send attempt; once sent, the record goes. A turn that fails deletes
//! its record: the user already gets the error notice. At start, before any
//! adapter routes a turn, the gateway takes over the records stopped processes
//! left by moving them into `recovering/`, serialised against record creation
//! so turns of this process never overwrite them. It then handles each: a
//! stored reply is sent, and a turn that never answered gets one notice asking
//! the user to send the message again. The turn is not run again, as that could
//! repeat its tool calls (commands, file writes, messages).
//!
//! Each send attempt is recorded under a cross-process lock before the send is
//! made (starting with the live turn's own send; crashes after recording an
//! attempt consume that attempt). Failed recovery sends are retried in the same
//! process, and a record is dropped once [`MAX_SEND_ATTEMPTS`] attempts have
//! been recorded. Delivery is at least once, never exactly once: a process that
//! stops after a send but before the record is deleted sends it again on the
//! next start within the cap. None is lost short of the cap.
//!
//! On Unix the records, lock file, and their folders are created private and
//! fail closed on permission errors (`0o700` dirs, `0o600` files). Elsewhere
//! they take the permissions the folder inherits (on Windows, the user
//! profile's by default).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Recorded send attempts for one record before it is dropped.
pub const MAX_SEND_ATTEMPTS: u32 = 3;

/// Result of reserving one recovery send attempt for a taken-over record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// Attempt reserved on disk; caller may send now.
    Reserved,
    /// Record already reached [`MAX_SEND_ATTEMPTS`] and was removed.
    Exhausted,
    /// Record was already removed, unreadable, or a symlink.
    Missing,
    /// Locking or persisting the attempt count failed; record stays on disk.
    WriteFailed,
}

/// Summary of one [`take_over_report`] pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TakeOverReport {
    pub moved: usize,
    pub unmovable_stopped: bool,
}

/// What a user gets for a turn the restart cut off before it answered.
pub const INTERRUPTED_NOTICE: &str = "The gateway restarted before it answered your last \
     message, so nothing was done with it after that point. Send it again to retry.";

/// Heads a reply sent after the restart.
pub const RECOVERED_REPLY_HEADING: &str = "Reply recovered after a gateway restart:";

static TURN_COUNTER: AtomicU64 = AtomicU64::new(0);
static TMP_NONCE: AtomicU64 = AtomicU64::new(0);

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
    /// When the writer started; a live process with the writer's pid and
    /// another start reused the pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_started: Option<ProcessStart>,
    /// The agent's reply, once it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    /// Recorded send attempts made from this record.
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

/// A writer's start and the Linux time namespace it was read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessStart {
    /// As [`process_started`] reads it.
    pub at: u64,
    /// The reader's [`time_namespace`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_namespace: Option<u64>,
}

/// This process's start, as [`process_started`] reads it.
pub fn this_process_started() -> Option<ProcessStart> {
    static STARTED: std::sync::OnceLock<Option<ProcessStart>> = std::sync::OnceLock::new();
    *STARTED.get_or_init(|| {
        Some(ProcessStart {
            at: process_started(std::process::id())?,
            time_namespace: time_namespace(),
        })
    })
}

/// This process's Linux time namespace: the inode `/proc/self/ns/time`
/// names. `None` elsewhere and before Linux 5.6, which has none.
pub fn time_namespace() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        namespace_inode(&std::fs::read_link("/proc/self/ns/time").ok()?.to_string_lossy())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The inode in a `time:[<inode>]` namespace link.
#[cfg(any(target_os = "linux", test))]
fn namespace_inode(link: &str) -> Option<u64> {
    link.strip_prefix("time:[")?.strip_suffix(']')?.parse().ok()
}

/// When process `pid` started, as its kernel keeps it: fixed for the
/// process's life and unmoved by clock changes, so a later process given the
/// same pid reads another value. Linux: clock ticks after boot
/// (`/proc/<pid>/stat` field 22); macOS: microseconds after the epoch
/// (`proc_pidinfo`). `None` elsewhere or when unreadable. A Linux time
/// namespace shifts the reading, so a record's start is compared only in the
/// namespace that read it.
pub fn process_started(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        started_from_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }
    #[cfg(target_os = "macos")]
    {
        bsd_started(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn bsd_started(pid: u32) -> Option<u64> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: `info` is a writable, zeroed `proc_bsdinfo` of `size` bytes,
    // the buffer `PROC_PIDTBSDINFO` fills.
    let wrote = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if wrote != size {
        return None;
    }
    // SAFETY: zeroed, then filled in full by the kernel.
    let info = unsafe { info.assume_init() };
    info.pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)
}

/// Field 22 (`starttime`) of a `/proc/<pid>/stat` line. Field 2, the name,
/// is in parentheses and may itself hold spaces and parentheses, so the
/// count starts after its last `)`.
#[cfg(any(target_os = "linux", test))]
fn started_from_stat(stat: &str) -> Option<u64> {
    let after_name = stat.get(stat.rfind(')')? + 1..)?;
    after_name.split_ascii_whitespace().nth(19)?.parse().ok()
}

fn turn_record_path(dir: &Path, session_key: &str) -> PathBuf {
    let digest = Sha256::digest(session_key.as_bytes());
    let session: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let writer: String = this_process()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let seq = TURN_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{session}-{writer}-{seq}.json"))
}

/// Where a start moves the records earlier processes left, so turns of this
/// process never write over them.
pub fn recovering_dir(dir: &Path) -> PathBuf {
    dir.join("recovering")
}

fn set_unix_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

fn private_dir_with(dir: &Path, chmod: fn(&Path, u32) -> std::io::Result<()>) -> bool {
    if let Err(e) = std::fs::symlink_metadata(dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(error = %e, "pending reply dir stat failed");
            return false;
        }
        if let Some(parent) = dir.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::debug!(error = %e, "pending reply parent create failed");
                return false;
            }
        }
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                tracing::debug!(error = %e, "pending reply dir create failed");
                return false;
            }
        }
    }
    if !is_real_dir(dir) {
        tracing::debug!(path = %dir.display(), "pending reply dir is not a directory");
        return false;
    }
    #[cfg(unix)]
    if let Err(e) = chmod(dir, 0o700) {
        tracing::warn!(error = %e, path = %dir.display(), "pending reply dir chmod failed");
        return false;
    }
    #[cfg(not(unix))]
    let _ = chmod;
    true
}

fn lock_path(dir: &Path) -> PathBuf {
    dir.join(".lock")
}

fn lock_dir_with(
    dir: &Path,
    chmod: fn(&Path, u32) -> std::io::Result<()>,
) -> Option<std::fs::File> {
    if !private_dir_with(dir, chmod) {
        return None;
    }
    let path = lock_path(dir);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            tracing::warn!(path = %path.display(), "refusing non-regular pending reply lock");
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "pending reply lock stat failed");
            return None;
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = match options.open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "pending reply lock open failed");
            return None;
        }
    };
    #[cfg(unix)]
    if let Err(e) = chmod(&path, 0o600) {
        tracing::warn!(error = %e, path = %path.display(), "pending reply lock chmod failed");
        return None;
    }
    if let Err(e) = file.lock() {
        tracing::warn!(error = %e, path = %path.display(), "pending reply lock failed");
        return None;
    }
    Some(file)
}

fn lock_dir(dir: &Path) -> Option<std::fs::File> {
    lock_dir_with(dir, set_unix_mode)
}

/// Write `record` to `path` whole or not at all; owner-only and fail-closed on Unix.
fn write_at_with(
    path: &Path,
    record: &PendingReply,
    chmod: fn(&Path, u32) -> std::io::Result<()>,
) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if !private_dir_with(parent, chmod) {
        return false;
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_file() {
            tracing::warn!(path = %path.display(), "refusing to overwrite non-regular pending reply");
            return false;
        }
    }
    let body = match serde_json::to_vec_pretty(record) {
        Ok(body) => body,
        Err(e) => {
            tracing::debug!(error = %e, "pending reply serialize failed");
            return false;
        }
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("record");
    let mut opened = None;
    for _ in 0..16 {
        let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{name}.tmp.{}.{nonce}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => {
                opened = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                tracing::warn!(error = %e, path = %candidate.display(), "pending reply temp open failed");
                return false;
            }
        }
    }
    let Some((tmp_path, mut file)) = opened else {
        return false;
    };
    let result = (|| -> std::io::Result<()> {
        #[cfg(unix)]
        chmod(&tmp_path, 0o600)?;
        #[cfg(not(unix))]
        let _ = chmod;
        use std::io::Write;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    })();
    if let Err(e) = result {
        tracing::warn!(error = %e, path = %path.display(), "pending reply write failed");
        let _ = std::fs::remove_file(&tmp_path);
        return false;
    }
    true
}

/// Write `record` as its turn in flight. Returns its path, or `None` when it
/// could not be written.
pub fn write(dir: &Path, record: &PendingReply) -> Option<PathBuf> {
    let _lock = lock_dir(dir)?;
    let path = turn_record_path(dir, &record.session_key);
    write_at_with(&path, record, set_unix_mode).then_some(path)
}

/// Keep `reply` and reserve the live turn's first send attempt on this
/// process's record at `path`. Returns true if the attempt was recorded; on
/// failure the existing record is left on disk.
pub fn begin_send(dir: &Path, path: &Path, reply: &str) -> bool {
    begin_send_with(dir, path, reply, set_unix_mode)
}

fn begin_send_with(
    dir: &Path,
    path: &Path,
    reply: &str,
    chmod: fn(&Path, u32) -> std::io::Result<()>,
) -> bool {
    let Some(_lock) = lock_dir_with(dir, chmod) else {
        return false;
    };
    let Some(mut record) = read(path) else {
        return false;
    };
    if record.process != this_process() || record.attempts >= MAX_SEND_ATTEMPTS {
        return false;
    }
    record.reply = Some(reply.to_string());
    record.attempts += 1;
    write_at_with(path, &record, chmod)
}

/// The turn's reply was sent, or the turn failed: drop this process's record.
pub fn finish(dir: &Path, path: &Path) {
    let Some(_lock) = lock_dir(dir) else {
        return;
    };
    if read(path).is_some_and(|r| r.process == this_process()) {
        let _ = std::fs::remove_file(path);
    }
}

/// Drop a taken-over record after its reply has been delivered.
pub fn mark_delivered(dir: &Path, path: &Path) {
    let Some(_lock) = lock_dir(dir) else {
        return;
    };
    let _ = std::fs::remove_file(path);
}

/// The record at `path`. One that does not parse is deleted, so its text
/// does not stay on disk. Symlinks and non-regular files are refused.
fn read(path: &Path) -> Option<PendingReply> {
    if !is_regular_file(path) {
        return None;
    }
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
    if !is_real_dir(dir) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_regular_file(p) && p.extension().is_some_and(|x| x == "json"))
        .collect()
}

/// The process id a record's writer had.
fn writer_pid(record: &PendingReply) -> Option<u32> {
    record.process.split('-').next()?.parse().ok()
}

/// Whether the live process `pid` started at another time than the record's
/// writer, so it is not the writer. Unknown starts (older records, Windows,
/// an unreadable process) and starts read in another time namespace count as
/// the writer.
fn pid_reused(record: &PendingReply, pid: u32) -> bool {
    record.process_started.is_some_and(|writer| {
        writer.time_namespace == time_namespace()
            && process_started(pid).is_some_and(|live| live != writer.at)
    })
}

/// When the record's writer started.
fn writer_started(record: &PendingReply) -> Option<std::time::SystemTime> {
    let nanos: u64 = record.process.split_once('-')?.1.parse().ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(nanos))
}

/// When this host booted (Linux: `btime` in `/proc/stat`; macOS:
/// `kern.boottime`); `None` elsewhere.
#[cfg(target_os = "macos")]
pub fn boot_time() -> Option<std::time::SystemTime> {
    let mut booted = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<libc::timeval>();
    let mut name = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    // SAFETY: `name` is a 2-level sysctl name, and `booted` with `len` is a
    // writable buffer of the size the kernel is told; no new value is set.
    let rc = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            2,
            std::ptr::addr_of_mut!(booted).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<libc::timeval>() {
        return None;
    }
    let secs = u64::try_from(booted.tv_sec).ok().filter(|s| *s > 0)?;
    let micros = u64::try_from(booted.tv_usec).ok()?;
    Some(
        std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(secs)
            + std::time::Duration::from_micros(micros),
    )
}

/// When this host booted (Linux: `btime` in `/proc/stat`; macOS:
/// `kern.boottime`); `None` elsewhere.
#[cfg(not(target_os = "macos"))]
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
/// host booted has stopped, whatever process its pid now names, as has one
/// whose pid now names a process with another start. Returns the number
/// moved.
pub fn take_over(dir: &Path, process_alive: fn(u32) -> bool) -> usize {
    take_over_report(dir, process_alive).moved
}

/// Move stopped-process records into [`recovering_dir`], reporting both the
/// moved count and whether any stopped record or lock error prevented recovery.
pub fn take_over_report(dir: &Path, process_alive: fn(u32) -> bool) -> TakeOverReport {
    take_over_since_with_hook(dir, process_alive, boot_time(), || {})
}

fn take_over_since_with_hook(
    dir: &Path,
    process_alive: fn(u32) -> bool,
    booted_at: Option<std::time::SystemTime>,
    mut before_move: impl FnMut(),
) -> TakeOverReport {
    let Some(_lock) = lock_dir(dir) else {
        return TakeOverReport {
            moved: 0,
            unmovable_stopped: true,
        };
    };
    let me = this_process();
    let to = recovering_dir(dir);
    let mut moved = 0;
    let mut unmovable_stopped = false;
    let files = json_files(dir);
    before_move();
    for path in files {
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
            && writer_pid(&record).is_some_and(|pid| {
                pid != std::process::id() && process_alive(pid) && !pid_reused(&record, pid)
            })
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
        if !private_dir_with(&to, set_unix_mode) {
            return TakeOverReport {
                moved,
                unmovable_stopped: true,
            };
        }
        match std::fs::rename(&path, to.join(format!("{stem}-{writer}.json"))) {
            Ok(()) => moved += 1,
            Err(e) => {
                tracing::warn!(error = %e, "pending reply not taken over");
                unmovable_stopped = true;
            }
        }
    }
    TakeOverReport {
        moved,
        unmovable_stopped,
    }
}

fn recovering_unlocked(dir: &Path) -> Vec<(PathBuf, PendingReply)> {
    let mut records: Vec<(PathBuf, PendingReply)> = json_files(&recovering_dir(dir))
        .into_iter()
        .filter_map(|path| read(&path).map(|record| (path, record)))
        .collect();
    records.sort_by(|a, b| {
        a.1.session_key
            .cmp(&b.1.session_key)
            .then_with(|| a.0.cmp(&b.0))
    });
    records
}

/// Records taken over and not yet sent, by session key and path.
pub fn recovering(dir: &Path) -> Vec<(PathBuf, PendingReply)> {
    let Some(_lock) = lock_dir(dir) else {
        return Vec::new();
    };
    recovering_unlocked(dir)
}

fn held_for_others_unlocked(dir: &Path) -> usize {
    let me = this_process();
    json_files(dir)
        .iter()
        .filter_map(|path| read(path))
        .filter(|record| record.process != me)
        .count()
}

/// Number of records waiting in `recovering/` plus records held for other
/// writers when `recovering/` can be used.
pub fn pending_count(dir: &Path) -> usize {
    let Some(_lock) = lock_dir(dir) else {
        return 0;
    };
    let in_recovering = recovering_unlocked(dir).len();
    let held = held_for_others_unlocked(dir);
    let can_take_over = if held > 0 {
        private_dir_with(&recovering_dir(dir), set_unix_mode)
    } else {
        true
    };
    in_recovering + if can_take_over { held } else { 0 }
}

fn count_attempt_with(
    dir: &Path,
    path: &Path,
    record: &mut PendingReply,
    chmod: fn(&Path, u32) -> std::io::Result<()>,
) -> AttemptOutcome {
    let Some(_lock) = lock_dir_with(dir, chmod) else {
        return AttemptOutcome::WriteFailed;
    };
    let Some(mut current) = read(path) else {
        return AttemptOutcome::Missing;
    };
    if current.attempts >= MAX_SEND_ATTEMPTS {
        let _ = std::fs::remove_file(path);
        *record = current;
        return AttemptOutcome::Exhausted;
    }
    current.attempts += 1;
    if !write_at_with(path, &current, chmod) {
        return AttemptOutcome::WriteFailed;
    }
    *record = current;
    AttemptOutcome::Reserved
}

/// Count a send attempt of the taken-over `record` at `path` under the
/// cross-process lock before the send is made. Re-reads from disk so stale
/// snapshots never overshoot [`MAX_SEND_ATTEMPTS`].
pub fn count_attempt(dir: &Path, path: &Path, record: &mut PendingReply) -> AttemptOutcome {
    count_attempt_with(dir, path, record, set_unix_mode)
}

#[cfg(test)]
mod tests;
