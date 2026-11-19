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
//!
//! shortcut: one profile a process (no multiplexer), no long-path socket pointer (a home whose
//! socket path is over 103 bytes refuses to start), no restart drain or update fence, no code
//! identity (`code_sha` null, which the desktop attaches to as-is), and Unix only (upstream's
//! Windows named pipe with its SID checks is not ported). Upgrade path: upstream's modules above.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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

/// A running local runtime. Dropping it stops serving and removes its files.
pub struct LocalRuntime {
    api_origin: String,
    instance_id: String,
    /// Files this runtime created, with the device and inode it created, so a file another
    /// owner wrote since is left alone.
    files: Vec<(PathBuf, (u64, u64))>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl LocalRuntime {
    pub fn api_origin(&self) -> &str {
        &self.api_origin
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
}

impl Drop for LocalRuntime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        // The lock goes last: while it is held, a concurrent ensure waits instead of starting.
        for (path, identity) in self.files.iter().rev() {
            if file_identity(path).ok() == Some(*identity) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn file_identity(path: &Path) -> std::io::Result<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path)?;
    Ok((meta.dev(), meta.ino()))
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

/// The process start time Linux records (field 22 of `/proc/<pid>/stat`), which tells a live
/// owner from a later process that reused its pid. None elsewhere or when unreadable.
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may hold spaces and parentheses; the fields follow its last ')'.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// The pid of the live process `<home>/gateway.lock` names: alive, and with the start time the
/// lock recorded when it recorded one.
fn lock_owner(home: &Path) -> Option<u32> {
    let raw = std::fs::read(home.join(LOCK)).ok()?;
    let lock: Value = serde_json::from_slice(&raw).ok()?;
    let pid = u32::try_from(lock.get("pid")?.as_u64()?).ok()?;
    let recorded = lock.get("start_time").and_then(Value::as_u64);
    let same_process = match (recorded, start_time(pid)) {
        (Some(recorded), Some(live)) => recorded == live,
        _ => true,
    };
    (pid_alive(pid) && same_process).then_some(pid)
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

/// Claims the home's lock; a lock whose owner is gone is replaced once.
fn claim_lock(home: &Path) -> Result<(PathBuf, (u64, u64)), String> {
    let path = home.join(LOCK);
    let pid = std::process::id();
    let record = json!({
        "pid": pid,
        "start_time": start_time(pid),
        "kind": "hermes-gateway",
        "hermes_home": home,
    })
    .to_string();
    for _ in 0..2 {
        match create_private(&path, record.as_bytes()) {
            Ok(identity) => return Ok((path, identity)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Some(owner) = lock_owner(home) {
                    return Err(format!(
                        "a gateway is already running for {} (pid {owner}, {})",
                        home.display(),
                        path.display()
                    ));
                }
                let _ = std::fs::remove_file(&path);
            }
            Err(err) => return Err(format!("cannot create {}: {err}", path.display())),
        }
    }
    Err(format!(
        "cannot claim {}: another gateway is starting",
        path.display()
    ))
}

/// Binds the control socket in a fresh private folder, makes it 0600 and moves it into place, so
/// it is never reachable with looser permissions, and a stale socket or link there is replaced.
fn bind_control(home: &Path) -> Result<(tokio::net::UnixListener, (u64, u64)), String> {
    let target = home.join(SOCKET);
    // Short: the staged path is the longest this binds.
    let staging = home.join(format!(
        ".g{}",
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    ));
    let staged = staging.join(SOCKET);
    if staged.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(format!(
            "the control socket path {} is longer than {MAX_SOCKET_PATH} bytes; use a shorter HERMES_HOME",
            staged.display()
        ));
    }
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
pub async fn start(home: &Path, chat: Chat) -> Result<LocalRuntime, String> {
    let home = std::fs::canonicalize(home)
        .map_err(|e| format!("cannot resolve the Hermes home {}: {e}", home.display()))?;
    let mut runtime = LocalRuntime {
        api_origin: String::new(),
        instance_id: uuid::Uuid::new_v4().simple().to_string(),
        files: vec![claim_lock(&home)?],
        tasks: Vec::new(),
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("cannot bind 127.0.0.1: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("cannot read the bound address: {e}"))?
        .port();
    runtime.api_origin = format!("http://127.0.0.1:{port}");
    let (control, identity) = bind_control(&home)?;
    runtime.files.push((home.join(SOCKET), identity));

    let tickets = Arc::new(Tickets::default());
    let app = router(Auth::Native(Arc::clone(&tickets)), port, chat);
    runtime.tasks.push(tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!("local desktop gateway stopped: {err}");
        }
    }));
    let control_state = Arc::new(Control {
        home: home.clone(),
        instance_id: runtime.instance_id.clone(),
        api_origin: runtime.api_origin.clone(),
        tickets,
    });
    runtime
        .tasks
        .push(tokio::spawn(serve_control(control, control_state)));

    let state_path = home.join(STATE);
    let _ = std::fs::remove_file(&state_path);
    let state = json!({"gateway_state": "running", "pid": std::process::id()}).to_string();
    let identity = create_private(&state_path, state.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", state_path.display()))?;
    runtime.files.push((state_path, identity));
    Ok(runtime)
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
            "state": "ready",
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
    let deadline = Instant::now() + timeout;
    let home = match std::fs::canonicalize(home) {
        Ok(home) => home,
        Err(e) => {
            return Verdict::refused("inaccessible", "invalid_home", Some(format!("{e}")));
        }
    };
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
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)?;
    let offset = log.metadata()?.len();
    let child = command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    Ok((child, log_path, offset))
}

/// The last lines the start wrote, up to [`MAX_DETAIL`] characters.
fn log_tail(path: &Path, offset: u64) -> String {
    let mut text = String::new();
    if let Ok(mut file) = std::fs::File::open(path) {
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
    let waiting = || {
        if lock_owner(home).is_some() {
            Discovery::Starting
        } else {
            Discovery::Absent
        }
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
    if reply["ok"] != true || reply["protocol"] != 1 {
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
