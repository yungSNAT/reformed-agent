//! Local mode: the runtime `hermes gateway run` hosts for Hermes Desktop on this machine, and the
//! discovery `hermes gateway ensure` runs (upstream v0.23.2 gateway/control_socket.py,
//! hermes_cli/gateway_runtime.py and gateway_runtime_cli.py; the desktop's side is
//! apps/desktop/electron/local-gateway.ts).
//!
//! The runtime owns one Hermes home. It claims `<home>/gateway.lock`, serves the desktop routes
//! on a free 127.0.0.1 port with ticket authentication only, answers one JSON line a connection
//! on `<home>/gateway.sock` (mode 0600) and publishes `<home>/gateway_state.json`. The socket
//! mints tickets only for a peer with this process's uid, in a home no other user can write;
//! ensure refuses such a home too, so neither side trusts a socket another user could replace.
//! A ticket is then a one-use, 30-second bearer credential, as upstream's: the loopback routes
//! check the ticket, not who presents it.
//!
//! shortcut: one profile a process (no multiplexer); no long-path socket pointer (where the
//! socket path would pass 103 bytes, `gateway run` runs without local mode and ensure answers
//! `control_path_too_long`); tickets only for the socket and REST routes this gateway serves (not
//! upstream's `native-profile-create` and `native-profile-registry`: it serves no profile
//! routes); no restart drain or update fence; no code identity (`code_sha` null, which the
//! desktop attaches to as-is); Unix only (upstream's Windows named pipe with its SID checks is
//! not ported). Upgrade path: upstream's modules above.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use crate::tickets::{Purpose, Tickets, TICKET_TTL};
use crate::{router, Auth, Chat};

/// The capability the desktop requires (`assertLocalGatewayEndpoint`).
const CAPABILITY: &str = "session-authority-v1";
const SOCKET: &str = "gateway.sock";
const LOCK: &str = "gateway.lock";
const STATE: &str = "gateway_state.json";
/// Longest socket path every Unix accepts (macOS's 104-byte `sun_path`, with its NUL).
const MAX_SOCKET_PATH: usize = 103;
/// Longest request or reply line on the control socket.
const MAX_LINE: usize = 64 << 10;
/// How long one control connection may take to send its line or read the reply.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest `detail` an ensure verdict carries.
const MAX_DETAIL: usize = 500;

/// A local runtime: from [`claim`], a held home; after [`LocalRuntime::serve`], a served one.
/// Dropping it stops serving, removes its files and empties `gateway.lock`.
pub struct LocalRuntime {
    home: PathBuf,
    api_origin: String,
    instance_id: String,
    /// Files this runtime created, with the device and inode it created, so a file another
    /// owner wrote since is left alone.
    files: Vec<(PathBuf, (u64, u64))>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Shared with the control socket and the router, so connections accepted before a stop
    /// mint and redeem nothing after it.
    tickets: Arc<Tickets>,
    /// `gateway.lock`, whose `flock` is released after the files go.
    lock: std::fs::File,
}

impl LocalRuntime {
    /// Empty until [`LocalRuntime::serve`].
    pub fn api_origin(&self) -> &str {
        &self.api_origin
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Withdraws local mode: closes the tickets, stops accepting and removes the socket and state
    /// file, so ensure cannot report a stopping host ready, whilst the home stays claimed until
    /// this drops. Connections accepted earlier still answer, as draining, and redeem nothing;
    /// a WebSocket session opened earlier runs until the process exits.
    pub fn stop_serving(&mut self) {
        self.tickets.close();
        for task in self.tasks.drain(..) {
            task.abort();
        }
        for (path, identity) in self.files.drain(..).rev() {
            if file_identity(&path).ok() == Some(identity) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

impl Drop for LocalRuntime {
    fn drop(&mut self) {
        self.stop_serving();
        // The lock file stays, emptied of its pid: unlinking the inode this still locks would let
        // a new claimant lock a fresh one at the path whilst cleanup still owns the home.
        let _ = self.lock.set_len(0);
        // A forked child may retain the descriptor until exec. Release the flock even then.
        let _ = self.lock.unlock();
    }
}

fn file_identity(path: &Path) -> std::io::Result<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

/// The uid of the process at the other end of `stream`.
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> std::io::Result<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: getsockopt writes at most `len` bytes into `cred`, which outlives the call.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if rc == 0 {
            Ok(cred.uid)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid writes the 2 ids it is given.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0 {
            Ok(uid)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Upstream's private-home rule for minting: a directory owned by this user that neither its
/// group nor other users can write (`home_mode_unsafe` admits a private group; this does not).
fn check_home(home: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(home)
        .map_err(|e| format!("cannot read the Hermes home {}: {e}", home.display()))?;
    if !meta.is_dir() {
        return Err(format!(
            "the Hermes home {} is not a directory",
            home.display()
        ));
    }
    if meta.uid() != own_uid() {
        return Err(format!(
            "the Hermes home {} is owned by uid {}, not this user (uid {})",
            home.display(),
            meta.uid(),
            own_uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "the Hermes home {} is writable by other users (mode {:o}): run chmod go-w on it",
            home.display(),
            meta.mode() & 0o777
        ));
    }
    Ok(())
}

/// Creates `path` with mode 0600 and `contents`; fails if it exists.
fn create_private(path: &Path, contents: &[u8]) -> std::io::Result<(u64, u64)> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok((file.metadata()?.dev(), file.metadata()?.ino()))
}

/// A non-blocking `flock` of `operation` (`LOCK_EX` or `LOCK_SH`) on `file`: true when taken,
/// false when another descriptor holds it.
fn try_flock(file: &std::fs::File, operation: libc::c_int) -> std::io::Result<bool> {
    // SAFETY: flock on a descriptor `file` keeps open; LOCK_NB makes it return at once.
    if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// Opens `<home>/gateway.lock` without following a link or blocking on a FIFO, and accepts only
/// a regular file this user owns, so a planted link cannot turn the record this writes on another
/// file. A second hard link counts only where other users can write the home: in a private home
/// only this user can have placed the file, and another user may raise its link count from
/// elsewhere (upstream refuses any second link).
fn open_lock(home: &Path, create: bool) -> std::io::Result<std::fs::File> {
    let path = &home.join(LOCK);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    let shared_home = || check_home(home).is_err();
    if !meta.is_file() || meta.uid() != own_uid() || (meta.nlink() != 1 && shared_home()) {
        return Err(std::io::Error::other(format!(
            "{} is not a regular file owned by this user, with one link in a shared home",
            path.display()
        )));
    }
    // A lock an older build created under the umask keeps its mode, as creation sets it only
    // once: narrow it here, through this descriptor (upstream `_private_lock`).
    if meta.mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// True while a runtime holds `<home>/gateway.lock`. A lock that cannot be tested reads as held,
/// so a caller waits rather than starting a second owner.
pub fn lock_held(home: &Path) -> bool {
    !matches!(lock_state(home), Ok(false))
}

/// Whether a runtime holds `<home>/gateway.lock` (false when there is none), or why the lock
/// cannot be used safely: a link, a FIFO, another user's file or no `flock`.
fn lock_state(home: &Path) -> Result<bool, String> {
    let path = home.join(LOCK);
    match open_lock(home, false) {
        // A shared lock taken here is released when `file` closes.
        Ok(file) => try_flock(&file, libc::LOCK_SH)
            .map(|taken| !taken)
            .map_err(|e| format!("cannot lock {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot use {}: {e}", path.display())),
    }
}

/// How many times a claim tries a lock held by another descriptor, 20 ms apart: an ensure probing
/// it, or a child forked elsewhere in this process that has not exec'd yet, holds it a moment.
const CLAIM_ATTEMPTS: u32 = 10;

/// Claims the home: an exclusive `flock` on `<home>/gateway.lock`, held for the runtime's life
/// and released by the kernel when the process ends, so neither a crash nor a reused pid leaves a
/// lock that reads as live. The file names the owner's pid, which upstream's e2e census reads.
/// None, with a warning, when the lock cannot be used safely: a refused file, or a file system
/// with no `flock` (some network mounts).
fn claim_lock(home: &Path) -> Result<Option<std::fs::File>, String> {
    let path = home.join(LOCK);
    for attempt in 1..=CLAIM_ATTEMPTS {
        let mut file = match open_lock(home, true) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!("cannot use {}: {e}", path.display());
                return Ok(None);
            }
        };
        match try_flock(&file, libc::LOCK_EX) {
            Ok(true) => {}
            Ok(false) if attempt < CLAIM_ATTEMPTS => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Ok(false) => {
                let mut record = String::new();
                let _ = file.read_to_string(&mut record);
                let pid = serde_json::from_str::<Value>(&record)
                    .ok()
                    .and_then(|r| r["pid"].as_u64());
                return Err(format!(
                    "a gateway is already running for {}{}",
                    home.display(),
                    pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
                ));
            }
            Err(e) => {
                tracing::warn!("cannot lock {}: {e}", path.display());
                return Ok(None);
            }
        }
        let meta = file
            .metadata()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        // Someone may have removed or replaced the file this opened: lock the one at the path.
        if file_identity(&path).ok() != Some((meta.dev(), meta.ino())) {
            continue;
        }
        let record =
            json!({"pid": std::process::id(), "kind": "hermes-gateway", "hermes_home": home});
        file.set_len(0)
            .and_then(|()| file.write_all(record.to_string().as_bytes()))
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        return Ok(Some(file));
    }
    Err(format!(
        "cannot claim {}: it was replaced while claiming",
        path.display()
    ))
}

/// Whether `home`'s control socket path fits a Unix socket address (see [`MAX_SOCKET_PATH`]).
pub fn control_socket_fits(home: &Path) -> bool {
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    staging_socket(&home, "000000").as_os_str().len() <= MAX_SOCKET_PATH
}

/// Where the control socket is bound before it moves into place: the longest path this binds.
fn staging_socket(home: &Path, tag: &str) -> PathBuf {
    home.join(format!(".g{tag}")).join(SOCKET)
}

/// Removes the staging folders a bind killed part-way left in `home` (`.g` and 6 lowercase hex
/// digits, owned by this user). Only the lock holder binds, so none is in use; a home other users
/// can write is left alone, as a folder there could be swapped for a link.
fn sweep_staging(home: &Path) {
    if check_home(home).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(home) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(tag) = name.to_str().and_then(|n| n.strip_prefix(".g")) else {
            continue;
        };
        if tag.len() != 6 || !tag.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            continue;
        }
        let dir = entry.path();
        // The socket is unlinked through the folder this opened without following a link, so a
        // folder swapped for a link after the check cannot point the unlink elsewhere; rmdir
        // follows no link either.
        let Ok(folder) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&dir)
        else {
            continue;
        };
        if !folder.metadata().is_ok_and(|m| m.uid() == own_uid()) {
            continue;
        }
        // SAFETY: unlinkat on a descriptor `folder` keeps open, with a NUL-terminated name.
        unsafe { libc::unlinkat(folder.as_raw_fd(), c"gateway.sock".as_ptr(), 0) };
        let _ = std::fs::remove_dir(&dir);
    }
}

/// Binds the control socket in a fresh private folder, makes it 0600 and moves it into place, so
/// it is never reachable with looser permissions, and a stale socket or link there is replaced.
fn bind_control(home: &Path) -> Result<(tokio::net::UnixListener, (u64, u64)), String> {
    let target = home.join(SOCKET);
    let staged = staging_socket(home, &uuid::Uuid::new_v4().simple().to_string()[..6]);
    let staging = staged.parent().unwrap_or(home).to_path_buf();
    if staged.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(format!(
            "the control socket path {} is longer than {MAX_SOCKET_PATH} bytes; use a shorter HERMES_HOME",
            staged.display()
        ));
    }
    sweep_staging(home);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
    let result = (|| {
        let listener = std::os::unix::net::UnixListener::bind(&staged)
            .map_err(|e| format!("cannot bind {}: {e}", staged.display()))?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot restrict {}: {e}", staged.display()))?;
        if let Ok(old) = std::fs::symlink_metadata(&target) {
            if old.uid() != own_uid() {
                return Err(format!("{} belongs to another user", target.display()));
            }
        }
        std::fs::rename(&staged, &target)
            .map_err(|e| format!("cannot publish {}: {e}", target.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("cannot use {}: {e}", target.display()))?;
        let identity =
            file_identity(&target).map_err(|e| format!("cannot read {}: {e}", target.display()))?;
        let listener = tokio::net::UnixListener::from_std(listener)
            .map_err(|e| format!("cannot use {}: {e}", target.display()))?;
        Ok((listener, identity))
    })();
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_dir(&staging);
    result
}

/// Starts the local runtime for `home`: lock, listener, control socket, then state file.
pub fn start(home: &Path, chat: Chat) -> Result<LocalRuntime, String> {
    let mut runtime = claim(home)?
        .ok_or_else(|| format!("{}/gateway.lock cannot be used safely", home.display()))?;
    runtime.serve(chat)?;
    Ok(runtime)
}

/// Claims `home` for one runtime without serving it yet. While claimed and unserved, ensure
/// waits, so a host can finish starting (say, write the pid file `gateway stop` reads) before
/// [`LocalRuntime::serve`] makes it ready.
/// None when `gateway.lock` cannot be used safely (see the warning logged): the host then runs
/// without local mode.
pub fn claim(home: &Path) -> Result<Option<LocalRuntime>, String> {
    let home = std::fs::canonicalize(home)
        .map_err(|e| format!("cannot resolve the Hermes home {}: {e}", home.display()))?;
    let Some(lock) = claim_lock(&home)? else {
        return Ok(None);
    };
    Ok(Some(LocalRuntime {
        home,
        api_origin: String::new(),
        instance_id: uuid::Uuid::new_v4().simple().to_string(),
        files: Vec::new(),
        tasks: Vec::new(),
        tickets: Arc::default(),
        lock,
    }))
}

impl LocalRuntime {
    /// Serves the claimed home: listener, control socket, then state file. Call it inside a Tokio
    /// runtime, which runs the servers.
    pub fn serve(&mut self, chat: Chat) -> Result<(), String> {
        let home = self.home.clone();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .and_then(|listener| {
                listener.set_nonblocking(true)?;
                tokio::net::TcpListener::from_std(listener)
            })
            .map_err(|e| format!("cannot bind 127.0.0.1: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("cannot read the bound address: {e}"))?
            .port();
        self.api_origin = format!("http://127.0.0.1:{port}");
        let (control, identity) = bind_control(&home)?;
        self.files.push((home.join(SOCKET), identity));

        let tickets = Arc::clone(&self.tickets);
        let app = router(Auth::Native(Arc::clone(&tickets)), port, chat);
        self.tasks.push(tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                tracing::error!("local desktop gateway stopped: {err}");
            }
        }));
        let control_state = Arc::new(Control {
            home: home.clone(),
            instance_id: self.instance_id.clone(),
            api_origin: self.api_origin.clone(),
            tickets,
        });
        self.tasks
            .push(tokio::spawn(serve_control(control, control_state)));

        let state_path = home.join(STATE);
        let _ = std::fs::remove_file(&state_path);
        let state = json!({"gateway_state": "running", "pid": std::process::id()}).to_string();
        let identity = create_private(&state_path, state.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", state_path.display()))?;
        self.files.push((state_path, identity));
        Ok(())
    }
}

struct Control {
    home: PathBuf,
    instance_id: String,
    api_origin: String,
    tickets: Arc<Tickets>,
}

async fn serve_control(listener: tokio::net::UnixListener, control: Arc<Control>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // Out of descriptors, say: try again shortly instead of spinning.
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let control = Arc::clone(&control);
        tokio::spawn(async move {
            let _ =
                tokio::time::timeout(CONTROL_TIMEOUT, answer_connection(stream, &control)).await;
        });
    }
}

async fn answer_connection(stream: tokio::net::UnixStream, control: &Control) {
    let peer_ok = stream.peer_cred().is_ok_and(|cred| cred.uid() == own_uid())
        && check_home(&control.home).is_ok();
    let (read, mut write) = stream.into_split();
    let mut line = Vec::new();
    let mut reader = tokio::io::BufReader::new(read).take(MAX_LINE as u64 + 1);
    if reader.read_until(b'\n', &mut line).await.is_err() || line.len() > MAX_LINE {
        return;
    }
    let mut reply = control.answer(&line, peer_ok).to_string();
    reply.push('\n');
    let _ = write.write_all(reply.as_bytes()).await;
}

impl Control {
    /// One request line to one reply, as upstream's `handle_request_line`.
    fn answer(&self, raw: &[u8], peer_ok: bool) -> Value {
        let request: Option<Map<String, Value>> = serde_json::from_slice(raw).ok();
        let id = request.as_ref().and_then(|r| r.get("id")).cloned();
        let verb = request
            .as_ref()
            .and_then(|r| r.get("verb"))
            .and_then(Value::as_str);
        let result = match (verb, &request) {
            (Some("identify"), _) => Ok(self.identify()),
            (Some("session-ticket"), Some(request)) => self.session_ticket(request, peer_ok),
            _ => Err(format!("unknown verb: {}", verb.unwrap_or("none"))),
        };
        let mut reply = match result {
            Ok(result) => json!({"ok": true, "protocol": 1, "result": result}),
            Err(error) => json!({"ok": false, "protocol": 1, "error": error}),
        };
        if let Some(id) = id.filter(|id| !id.is_null()) {
            reply["id"] = id;
        }
        reply
    }

    fn identify(&self) -> Value {
        let home = self.home.to_string_lossy();
        json!({
            "protocol": 1,
            "pid": std::process::id(),
            "hermes_home": home,
            "supervisor": "none",
            "launch_supervisor": "manual",
            "runtime_protocol": 1,
            // A stopping owner's open connections still answer, as upstream's draining owner.
            "state": if self.tickets.is_closed() { "draining" } else { "ready" },
            "instance_id": self.instance_id,
            "authority_epoch": 1,
            "api_origin": self.api_origin,
            "served_profiles": [{"home": home, "profile_id": home}],
            "capabilities": [CAPABILITY],
        })
    }

    /// Upstream's `_session_ticket` checks, narrowed to what this runtime serves: one home, the
    /// `interactive` and `native-http` purposes, and no forwarded subject.
    fn session_ticket(&self, request: &Map<String, Value>, peer_ok: bool) -> Result<Value, String> {
        if !peer_ok {
            return Err("PermissionError: authenticated runtime bootstrap unavailable".into());
        }
        let envelope_ok = request
            .keys()
            .all(|k| ["protocol", "verb", "id", "params"].contains(&k.as_str()))
            && request.get("protocol").and_then(Value::as_u64) == Some(1);
        let params = request.get("params").and_then(Value::as_object);
        let (true, Some(params)) = (envelope_ok, params) else {
            return Err("PermissionError: invalid bootstrap envelope".into());
        };
        let text = |key: &str| params.get(key).and_then(Value::as_str);
        let known = [
            "profile_id",
            "instance_id",
            "purpose",
            "scope",
            "restart_wait",
        ];
        let purpose = match text("purpose") {
            Some("interactive") => Some(Purpose::Interactive),
            Some("native-http") => Some(Purpose::NativeHttp),
            _ => None,
        };
        let scope_ok = match params.get("scope") {
            None => true,
            Some(scope) => {
                *scope == "profile" || (*scope == "host" && purpose == Some(Purpose::Interactive))
            }
        };
        let (Some(profile_id), Some(instance_id), Some(purpose)) =
            (text("profile_id"), text("instance_id"), purpose)
        else {
            return Err("PermissionError: invalid bootstrap parameters".into());
        };
        if !params.keys().all(|k| known.contains(&k.as_str()))
            || !scope_ok
            || params.get("restart_wait").is_some_and(|w| *w != "refuse")
        {
            return Err("PermissionError: invalid bootstrap parameters".into());
        }
        if instance_id != self.instance_id {
            return Err("PermissionError: stale runtime instance".into());
        }
        if Path::new(profile_id) != self.home {
            return Err("PermissionError: profile not served by this gateway".into());
        }
        let ticket = self.tickets.mint(purpose)?;
        Ok(json!({
            "ticket": ticket,
            "expires_in_seconds": TICKET_TTL.as_secs(),
            "instance_id": self.instance_id,
            "profile_id": profile_id,
            "runtime_protocol": 1,
        }))
    }
}

/// `hermes gateway ensure`'s answer (upstream `GatewayDiscovery` and its JSON).
#[derive(Debug)]
pub struct Verdict {
    state: &'static str,
    reason_code: Option<&'static str>,
    detail: Option<String>,
    endpoint: Option<Value>,
}

impl Verdict {
    fn pending(state: &'static str) -> Self {
        Self {
            state,
            reason_code: None,
            detail: None,
            endpoint: None,
        }
    }

    /// Upstream's bare `inaccessible`: a state with no reason code.
    fn inaccessible(detail: String) -> Self {
        Self {
            state: "inaccessible",
            reason_code: None,
            detail: Some(detail.chars().take(MAX_DETAIL).collect()),
            endpoint: None,
        }
    }

    fn refused(state: &'static str, reason_code: &'static str, detail: Option<String>) -> Self {
        Self {
            state,
            reason_code: Some(reason_code),
            detail: detail.map(|d| d.chars().take(MAX_DETAIL).collect()),
            endpoint: None,
        }
    }

    /// One compact line with sorted keys, as `json.dumps(..., separators=(",", ":"),
    /// sort_keys=True)`. `client_code_sha` is null: this build names no code identity.
    pub fn to_json(&self) -> String {
        // Written in key order, so the output is sorted whatever serde_json's map type.
        json!({
            "client_code_sha": null,
            "detail": self.detail,
            "endpoint": self.endpoint,
            "reason_code": self.reason_code,
            "state": self.state,
        })
        .to_string()
    }

    /// Upstream's `ensure_exit_code`.
    pub fn exit_code(&self) -> i32 {
        match (self.state, self.reason_code) {
            (_, Some("deadline")) => 5,
            (_, Some("authorization" | "profile_mismatch")) => 4,
            ("ready", _) => 0,
            ("incompatible", _) => 3,
            ("draining", _) => 6,
            ("starting" | "absent", _) => 5,
            _ => 7,
        }
    }
}

enum Discovery {
    Found(Verdict),
    /// A live lock holder has not answered yet.
    Starting,
    Absent,
}

/// Finds the runtime for `home`, starts `command` once when there is none, and waits for it to
/// answer until `timeout` passes (upstream `ensure_gateway_runtime`). `command`'s output is
/// appended to `<home>/logs/gateway-stdio.log`; it runs in its own process group with no stdin,
/// so it outlives this call.
pub fn ensure(home: &Path, timeout: Duration, mut command: Command) -> Verdict {
    // A timeout past what an Instant holds waits a year instead.
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(365 * 86_400));
    let home = match std::fs::canonicalize(home) {
        Ok(home) => home,
        Err(e) => {
            return Verdict::refused("inaccessible", "invalid_home", Some(format!("{e}")));
        }
    };
    if !control_socket_fits(&home) {
        return Verdict::refused(
            "inaccessible",
            "control_path_too_long",
            Some(format!(
                "the control socket path under {} would be longer than {MAX_SOCKET_PATH} bytes; use a shorter HERMES_HOME",
                home.display()
            )),
        );
    }
    let mut started: Option<(std::process::Child, PathBuf, u64)> = None;
    let mut delay = Duration::from_millis(25);
    loop {
        // Polled before discovering: an exit seen first, then no owner, cannot be a child that
        // published its owner a moment later.
        let exited = started
            .as_mut()
            .and_then(|(child, _, _)| child.try_wait().ok().flatten());
        match discover(&home) {
            Discovery::Found(verdict) => return verdict,
            Discovery::Starting => {}
            Discovery::Absent if started.is_none() => match spawn(&home, &mut command) {
                Ok(child) => started = Some(child),
                Err(e) => {
                    return Verdict::refused(
                        "inaccessible",
                        "runtime_start_failed",
                        Some(e.to_string()),
                    )
                }
            },
            Discovery::Absent => {
                if let (Some((_, log, offset)), Some(status)) = (&started, exited) {
                    return Verdict::refused(
                        "inaccessible",
                        "runtime_exited",
                        Some(format!(
                            "the gateway exited ({status}): {}",
                            log_tail(log, *offset)
                        )),
                    );
                }
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Verdict::refused("starting", "deadline", None);
        }
        std::thread::sleep(delay.min(deadline - now));
        delay = (delay * 3 / 2).min(Duration::from_millis(50));
    }
}

/// What the gateway ensure starts must not inherit from the caller that asked for it (upstream
/// `gateway_runtime_start.py`: `DAEMON_STRIPPED_ENV`, its `SUPERVISOR_MARKER_ENV` and the
/// session variables of `gateway/session_context.py`). One launch's policy, session and pins
/// would otherwise hold for every later session of the shared runtime, and a supervisor's
/// markers would make it believe the supervisor started it.
const DAEMON_STRIPPED_ENV: &[&str] = &[
    "HERMES_PROFILE",
    "HERMES_YOLO_MODE",
    "HERMES_IGNORE_RULES",
    "HERMES_SAFE_MODE",
    "HERMES_IGNORE_USER_CONFIG",
    "HERMES_ACCEPT_HOOKS",
    "HERMES_SESSION_SOURCE",
    "TERMINAL_CWD",
    "HERMES_KANBAN_TASK",
    "HERMES_KANBAN_WORKSPACE",
    "HERMES_KANBAN_BRANCH",
    "HERMES_KANBAN_RUN_ID",
    "HERMES_KANBAN_CLAIM_LOCK",
    "HERMES_KANBAN_CLAIM_TTL_SECONDS",
    "HERMES_KANBAN_GOAL_MODE",
    "HERMES_KANBAN_GOAL_MAX_TURNS",
    "HERMES_KANBAN_DB",
    "HERMES_KANBAN_WORKSPACES_ROOT",
    "HERMES_KANBAN_BOARD",
    "HERMES_TENANT",
    "HERMES_SESSION_SOURCE_EXPLICIT",
    "HERMES_TURN_AUTHOR",
    "HERMES_INTERACTIVE",
    "HERMES_GATEWAY_SESSION",
    "HERMES_SINGLE_QUERY_SESSION",
    "HERMES_EPHEMERAL_SYSTEM_PROMPT",
    "HERMES_PREFILL_MESSAGES_FILE",
    "HERMES_INFERENCE_PROVIDER",
    "HERMES_ENABLE_PROJECT_PLUGINS",
    "HERMES_TUI_CLIENT_ENV",
    // Supervisor markers.
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "XPC_SERVICE_NAME",
    "LAUNCHD_SOCKET",
    "HERMES_LAUNCHD_LABEL",
    "HERMES_GATEWAY_EXTERNAL_SUPERVISOR",
    "HERMES_SUPERVISED_CHILD",
    "NOTIFY_SOCKET",
    "WATCHDOG_USEC",
    "WATCHDOG_PID",
    // The launching turn's session.
    "HERMES_SESSION_PLATFORM",
    "HERMES_SESSION_CHAT_ID",
    "HERMES_SESSION_CHAT_TYPE",
    "HERMES_SESSION_CHAT_NAME",
    "HERMES_SESSION_THREAD_ID",
    "HERMES_SESSION_USER_ID",
    "HERMES_SESSION_USER_ID_ALT",
    "HERMES_SESSION_USER_NAME",
    "HERMES_SESSION_SCOPE_ID",
    "HERMES_SESSION_KEY",
    "HERMES_SESSION_ID",
    "HERMES_UI_SESSION_ID",
    "HERMES_SESSION_MESSAGE_ID",
    "HERMES_SESSION_PROFILE",
    "HERMES_CRON_SESSION",
    "HERMES_SESSION_PARENT_CHAT_ID",
    "HERMES_SESSION_ACTOR_ID",
    "HERMES_SESSION_ACTOR_NAME",
    "HERMES_CRON_AUTO_DELIVER_PLATFORM",
    "HERMES_CRON_AUTO_DELIVER_CHAT_ID",
    "HERMES_CRON_AUTO_DELIVER_THREAD_ID",
];

fn spawn(
    home: &Path,
    command: &mut Command,
) -> std::io::Result<(std::process::Child, PathBuf, u64)> {
    let logs = home.join("logs");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&logs)?;
    let log_path = logs.join("gateway-stdio.log");
    // A planted link must not point the gateway's output at another file.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&log_path)?;
    let meta = log.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other(format!(
            "{} is not a regular file",
            log_path.display()
        )));
    }
    let offset = meta.len();
    for key in DAEMON_STRIPPED_ENV {
        command.env_remove(key);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .current_dir(home)
        .process_group(0)
        .spawn()?;
    Ok((child, log_path, offset))
}

/// The last lines the start wrote, up to [`MAX_DETAIL`] characters.
fn log_tail(path: &Path, offset: u64) -> String {
    let mut text = String::new();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    if let Ok(mut file) = file {
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let from = offset.max(len.saturating_sub(4096));
        if file.seek(SeekFrom::Start(from)).is_ok() {
            let mut raw = Vec::new();
            let _ = file.read_to_end(&mut raw);
            text = String::from_utf8_lossy(&raw).into_owned();
        }
    }
    let text = text.trim();
    let skip = text.chars().count().saturating_sub(MAX_DETAIL);
    text.chars().skip(skip).collect()
}

fn discover(home: &Path) -> Discovery {
    if let Err(detail) = check_home(home) {
        return Discovery::Found(Verdict::refused(
            "inaccessible",
            "unsafe_control_permissions",
            Some(detail),
        ));
    }
    let path = home.join(SOCKET);
    // A lock no runtime can claim is named at once, as upstream's `missing_owner_state` does,
    // instead of read as a start in progress until the deadline.
    let waiting = || match lock_state(home) {
        Ok(true) => Discovery::Starting,
        Ok(false) => Discovery::Absent,
        Err(detail) => Discovery::Found(Verdict::inaccessible(detail)),
    };
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return waiting(),
        Err(e) => {
            return Discovery::Found(Verdict::refused(
                "inaccessible",
                "invalid_control_peer",
                Some(format!("cannot read {}: {e}", path.display())),
            ))
        }
    };
    use std::os::unix::fs::FileTypeExt;
    if !meta.file_type().is_socket() || meta.uid() != own_uid() || meta.mode() & 0o077 != 0 {
        return Discovery::Found(Verdict::refused(
            "inaccessible",
            "unsafe_control_permissions",
            Some(format!(
                "{} must be a socket owned by uid {} with mode 600 (found uid {}, mode {:o}): remove it, then run hermes gateway restart",
                path.display(),
                own_uid(),
                meta.uid(),
                meta.mode() & 0o7777
            )),
        ));
    }
    let mut stream = match std::os::unix::net::UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => return waiting(),
        Err(e) => {
            return Discovery::Found(Verdict::refused(
                "inaccessible",
                "invalid_control_peer",
                Some(e.to_string()),
            ))
        }
    };
    // connect() follows whatever is at the path by now, so authenticate the listener itself.
    match peer_uid(&stream) {
        Ok(uid) if uid == own_uid() => {}
        found => {
            return Discovery::Found(Verdict::refused(
                "inaccessible",
                "invalid_control_peer",
                Some(match found {
                    Ok(uid) => format!("{} is served by uid {uid}", path.display()),
                    Err(e) => format!("cannot read the peer of {}: {e}", path.display()),
                }),
            ))
        }
    }
    let reply = (|| -> std::io::Result<Vec<u8>> {
        stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
        stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
        stream.write_all(b"{\"protocol\":1,\"id\":1,\"verb\":\"identify\"}\n")?;
        let mut line = Vec::new();
        BufReader::new(stream)
            .take(MAX_LINE as u64)
            .read_until(b'\n', &mut line)?;
        Ok(line)
    })();
    match reply {
        Ok(line) => Discovery::Found(endpoint_verdict(&line, home)),
        // A starved owner: keep waiting until the deadline.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Discovery::Starting
        }
        Err(e) => Discovery::Found(Verdict::refused(
            "inaccessible",
            "invalid_control_peer",
            Some(e.to_string()),
        )),
    }
}

/// Upstream's `_endpoint`: the checks a client makes of an `identify` answer.
fn endpoint_verdict(line: &[u8], home: &Path) -> Verdict {
    let peer = |detail: &str| {
        Verdict::refused(
            "inaccessible",
            "invalid_control_peer",
            Some(detail.to_string()),
        )
    };
    let Ok(reply) = serde_json::from_slice::<Value>(line) else {
        return peer("the control socket's reply is not JSON");
    };
    if reply["ok"] != true || reply["protocol"] != 1 || reply["id"] != 1 {
        return peer("the control socket refused identify");
    }
    let p = &reply["result"];
    if p["runtime_protocol"] != 1 {
        return Verdict::refused("incompatible", "runtime_protocol", None);
    }
    let Some(served) = p["served_profiles"].as_array() else {
        return Verdict::refused("inaccessible", "profile_mismatch", None);
    };
    let matches: Vec<&Value> = served
        .iter()
        .filter(|s| {
            s["home"]
                .as_str()
                .is_some_and(|h| std::fs::canonicalize(h).is_ok_and(|h| h == home))
        })
        .collect();
    let profile_id = match matches.as_slice() {
        [only] => only["profile_id"].as_str().filter(|id| !id.is_empty()),
        _ => None,
    };
    let Some(profile_id) = profile_id else {
        return Verdict::refused("inaccessible", "profile_mismatch", None);
    };
    match p["state"].as_str() {
        Some("ready") => {}
        Some("starting") => return Verdict::pending("starting"),
        Some("draining") => return Verdict::pending("draining"),
        Some("conflict") => return Verdict::pending("conflict"),
        _ => return Verdict::refused("inaccessible", "invalid_runtime_state", None),
    }
    let Some(capabilities) = p["capabilities"]
        .as_array()
        .and_then(|c| c.iter().map(Value::as_str).collect::<Option<Vec<&str>>>())
        .filter(|c| c.contains(&CAPABILITY))
    else {
        return Verdict::refused("incompatible", "session_authority_unavailable", None);
    };
    let (Some(epoch), Some(instance)) = (
        p["authority_epoch"].as_u64().filter(|e| *e > 0),
        p["instance_id"].as_str().filter(|i| !i.is_empty()),
    ) else {
        return Verdict::refused("inaccessible", "invalid_runtime_identity", None);
    };
    let Some(origin) = p["api_origin"].as_str().filter(|o| loopback_origin(o)) else {
        return Verdict::refused("inaccessible", "invalid_api_origin", None);
    };
    let Some(supervisor) = p["supervisor"]
        .as_str()
        .filter(|s| ["none", "systemd", "launchd", "windows", "external"].contains(s))
    else {
        return Verdict::refused("inaccessible", "invalid_supervisor", None);
    };
    let mut capabilities = capabilities;
    capabilities.sort_unstable();
    Verdict {
        state: "ready",
        reason_code: None,
        detail: None,
        endpoint: Some(json!({
            "api_origin": origin,
            "authority_epoch": epoch,
            "capabilities": capabilities,
            "code_sha": p["code_sha"].as_str().filter(|s| !s.is_empty()),
            "control_home": null,
            "instance_id": instance,
            "profile_id": profile_id,
            "runtime_protocol": 1,
            "supervisor": supervisor,
        })),
    }
}

/// `http(s)://<loopback ip>:<port>` with nothing else, as upstream's `_endpoint` requires.
fn loopback_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let Some((host, port)) = rest.rsplit_once(':') else {
        return false;
    };
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    port.parse::<u16>().is_ok_and(|p| p > 0)
        && host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::{claim, lock_held};

    #[test]
    fn dropping_runtime_unlocks_with_a_duplicated_descriptor() {
        let home = tempfile::tempdir().unwrap();
        let mut runtime = claim(home.path()).unwrap().unwrap();
        let duplicate = runtime.lock.try_clone().unwrap();

        runtime.stop_serving();
        assert!(lock_held(home.path()));

        drop(runtime);
        assert!(!lock_held(home.path()));
        assert_eq!(duplicate.metadata().unwrap().len(), 0);
    }
}
