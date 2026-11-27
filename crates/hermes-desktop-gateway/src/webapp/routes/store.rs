//! What the route groups that keep a machine-level file share: who may call a Webapp-only route,
//! the machine root those files sit in, blocking work off the event loop, and the `<store>.lock`
//! folder lock native Desktop's writers take too (upstream `hermes_cli/ui_preferences.py`
//! `_store_lock`, which `hermes_cli/gateway_connections.py` reuses).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::http::StatusCode;

use crate::webapp::api::{Api, ApiError, Caller, Ctx};

const LOCK_WAIT: Duration = Duration::from_secs(3);
const LOCK_STALE: Duration = Duration::from_secs(10);
const LOCK_POLL: Duration = Duration::from_millis(20);

pub(crate) fn not_found(detail: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, detail.to_owned())
}

/// Upstream mounts these routes only where the Webapp is served; elsewhere they do not exist.
pub(crate) fn webapp_only(ctx: &Ctx) -> Result<(), ApiError> {
    match ctx.caller {
        Caller::Browser { .. } => Ok(()),
        Caller::Native | Caller::Token => Err(not_found("Not Found")),
    }
}

/// The Hermes home this gateway serves, fixed when the routes are built. A route takes it from
/// the router, never from the request ([`Ctx`] says only who called), so no request value can
/// name the folder its files live in.
#[derive(Clone, Debug)]
pub struct Served(Arc<PathBuf>);

impl Served {
    pub(crate) fn new(home: PathBuf) -> Self {
        Served(Arc::new(home))
    }

    pub(crate) fn of(api: &Api) -> Self {
        Served::new(api.home.clone().unwrap_or_else(hermes_config::hermes_home))
    }

    /// The Hermes home this gateway serves.
    pub(crate) fn home(&self) -> &Path {
        &self.0
    }

    /// The install root of the served home, where machine-level stores live (UI preferences,
    /// saved gateways), resolved through links as upstream resolves it (`Path.resolve`, which
    /// keeps a missing path as given).
    pub(crate) fn install_root(&self) -> PathBuf {
        let root = install_root_of(self.home());
        std::fs::canonicalize(&root).unwrap_or(root)
    }
}

/// The home itself, or its grandparent when the home is a profile's folder `<root>/profiles/<name>`.
pub(crate) fn install_root_of(home: &Path) -> PathBuf {
    match home.parent() {
        Some(parent)
            if parent
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("profiles")) =>
        {
            parent.parent().unwrap_or(parent).to_path_buf()
        }
        _ => home.to_path_buf(),
    }
}

/// Runs file work on the blocking pool; a panic in it is a 500.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work).await.unwrap_or_else(|_| {
        Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error".into(),
        ))
    })
}

/// The `<store>.lock` folder; its `owner` file names the holder. Dropping it releases the lock
/// only whilst it is still ours.
pub(crate) struct StoreLock {
    dir: PathBuf,
    token: String,
}

impl StoreLock {
    /// Takes the lock for the file at `path`, waiting up to 3 s and breaking a lock older than
    /// 10 s. The error is the 'busy' text for a 503, naming `what` (say 'preferences store').
    pub(crate) fn take(path: &Path, what: &str) -> Result<Self, String> {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        let dir = path.with_file_name(name);
        let token = format!(
            "{}-{}",
            std::process::id(),
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        );
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match std::fs::create_dir(&dir) {
                Ok(()) => break,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    break_if_stale(&dir);
                    if Instant::now() > deadline {
                        return Err(format!("{what} is busy"));
                    }
                    std::thread::sleep(LOCK_POLL);
                }
                Err(_) => return Err(format!("{what} is unreadable")),
            }
        }
        let lock = StoreLock { dir, token };
        if std::fs::write(lock.dir.join("owner"), &lock.token).is_err() {
            return Err(format!("{what} is busy"));
        }
        Ok(lock)
    }

    /// Still ours: a lock broken as stale and taken by another writer is not.
    pub(crate) fn held(&self) -> bool {
        std::fs::read(self.dir.join("owner")).is_ok_and(|owner| owner == self.token.as_bytes())
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        if self.held() {
            let _ = std::fs::remove_file(self.dir.join("owner"));
            let _ = std::fs::remove_dir(&self.dir);
        }
    }
}

/// A writer that died holding the lock must not wedge every later write.
///
/// Waiters of this gateway break one at a time: each holds an OS lock on `<store>.lock.break`,
/// which the OS frees if it dies, and judges the lock again under it. So a lock another waiter
/// broke and took since the first judgement is fresh by then and is left alone. Upstream's and
/// native Desktop's writers do not take that OS lock; a break by one of them is the limit
/// [`break_stale`] states. Only a turn another waiter holds stops a break: when the file cannot
/// be opened or locked (on Unix a link in its place; on Windows a folder or another process
/// holding the file unshared; a file system without locks), the waiter breaks as before, so a
/// dead lock never wedges writes. A link in its place is never followed: Unix refuses to open it
/// and Windows opens the link itself. A newly created file is owner-only on Unix and takes default
/// permissions on Windows; an existing one keeps its permissions.
fn break_if_stale(dir: &Path) {
    if !is_stale(dir) {
        return;
    }
    seam("judged");
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(".break");
    let mut open = std::fs::OpenOptions::new();
    open.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        open.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let turn = open.open(dir.with_file_name(name)).ok();
    // Another waiter is breaking it: poll again.
    let taken = |t: &std::fs::File| matches!(t.try_lock(), Err(std::fs::TryLockError::WouldBlock));
    if turn.as_ref().is_some_and(taken) {
        return;
    }
    if let Some(seen) = stale_owner(dir) {
        break_stale(dir, &seen);
    }
}

fn is_stale(dir: &Path) -> bool {
    std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > LOCK_STALE)
}

/// The owner token of a lock older than the stale limit (empty when it names none).
fn stale_owner(dir: &Path) -> Option<Vec<u8>> {
    is_stale(dir).then(|| std::fs::read(dir.join("owner")).unwrap_or_default())
}

/// Removes the lock judged stale with owner `seen`. It is first moved aside under a name of its
/// own, which only one waiter's rename can do, then removed only if it still names the stale owner;
/// a lock another writer re-took since the judgement goes back untouched.
///
/// The lock folder, owner file, wait and stale age are shared with upstream and native Desktop.
/// This break is stricter than upstream's, which removes a folder judged stale outright, and
/// [`break_if_stale`] runs it for one waiter of this gateway at a time on a fresh judgement. It
/// is still not airtight against other writers: one breaking as upstream does can remove a live
/// lock this gateway took since that writer judged it, and a holder stalled past the stale age
/// can be broken whilst it writes. The displaced holder answers busy at its next `held()` check,
/// but one already past it can overwrite the newer write.
fn break_stale(dir: &Path, seen: &[u8]) {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".stale-{}", uuid::Uuid::new_v4().simple()));
    let aside = dir.with_file_name(name);
    if std::fs::rename(dir, &aside).is_err() {
        return;
    }
    seam("aside");
    let owner = std::fs::read(aside.join("owner")).unwrap_or_default();
    // The lock is put back only when a writer outside this protocol broke and re-took it since
    // the judgement. On Unix that rename also replaces an empty folder, so a taker that made `dir`
    // but has not yet written `owner` writes into the moved-back folder and displaces the holder,
    // as a failed rename does.
    if owner == seen {
        let _ = std::fs::remove_file(aside.join("owner"));
        let _ = std::fs::remove_dir(&aside);
    } else if std::fs::rename(&aside, dir).is_err() {
        // Another writer took the free name meanwhile: the moved holder answers busy at its next
        // `held()` check; one already past it is the limit stated above.
        let _ = std::fs::remove_file(aside.join("owner"));
        let _ = std::fs::remove_dir(&aside);
    }
}

/// A step another writer takes at the named point.
#[cfg(test)]
type Seam = (&'static str, Box<dyn FnOnce()>);

#[cfg(test)]
thread_local! {
    /// Steps another writer takes at a named point of this thread's lock code, each once.
    pub(super) static SEAMS: std::cell::RefCell<Vec<Seam>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Runs the step a test armed for `at`; a no-op outside tests.
fn seam(at: &str) {
    #[cfg(test)]
    {
        let step = SEAMS.with(|seams| {
            let mut seams = seams.borrow_mut();
            let found = seams.iter().position(|(name, _)| *name == at);
            found.map(|i| seams.remove(i).1)
        });
        if let Some(step) = step {
            step();
        }
    }
    #[cfg(not(test))]
    let _ = at;
}

/// Replaces `path` whole. A new file is made owner-only before any bytes land; the atomic writer
/// keeps an existing file's mode.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path);
    }
    hermes_config::loader::atomic_write_bytes(path, bytes).map_err(|err| err.to_string())
}

#[cfg(test)]
pub(crate) const TEST_LOCK_WAIT: Duration = LOCK_WAIT;

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
