//! Named profiles served from child gateways, one process per profile, as upstream hot-serves
//! a profile from its own runtime.
//!
//! A profile cannot be served in this process: loading its configuration puts its `.env` into the
//! process environment and bridges its settings there (`hermes_config::load_config`), and
//! providers read their keys from that environment, so the default profile's keys would change.
//! A child is this binary in child mode, started on first use with the profile's home as
//! `HERMES_HOME` and working folder, an environment of [`ALLOWED_ENV`] only (taken before this
//! process loaded any configuration) and a private token. It loads its own `.env`; no profile's
//! secrets reach another process. Clients never see a child's token, and their own is never
//! forwarded.
//!
//! A connection reaches a child two ways, both through [`Forwards`]: a socket scoped with
//! `?profile=<name>`, and a request whose `params.profile` names a profile (the Webapp keeps the
//! default socket and names the tab's profile in each request). Each client connection gets its own
//! connection to each child it uses, so the child's events for that client's sessions reach only
//! it, and the child sees the client's origin, so a forwarded request keeps the client's
//! restrictions. Forwarded requests wait for their replies as in-process handlers do: as long
//! as the child's handler takes, or until the child connection closes.
//!
//! At most [`MAX_CHILDREN`] children live at once, counted from spawn until their exit has been
//! awaited. A client connection holds a lease on each child it uses; a child with no lease is
//! stopped after [`IDLE_STOP`], or sooner when another profile needs its slot. A child exits
//! when its standard input closes, so none outlives this process.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, protocol::WebSocketConfig};

use crate::chat::profiles::{install_root, named_profile_home};
use crate::chat::{ready, Chat, Conn, Origin, Outgoing, Reply, RpcError};
use crate::{ATTACH_MAX_MESSAGE_BYTES, CANONICAL_PROTOCOL, TOKEN_ENV};

/// Environment variables a child inherits; `LC_` and `XDG_` names too. Nothing else, so no
/// provider key or Hermes setting of this process reaches a child unless the child's own `.env`
/// sets it.
const ALLOWED_ENV: [&str; 25] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TZ",
    "TMPDIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "PATHEXT",
];
const ALLOWED_ENV_PREFIXES: [&str; 2] = ["LC_", "XDG_"];

/// The header a parent's dial names its client's [`Origin`] in; a child (`--child`) trusts it
/// on a dial with its private token, so a forwarded request keeps the client's restrictions.
pub(crate) const CHILD_ORIGIN_HEADER: &str = "x-hermes-child-origin";

/// Most children alive at once.
pub const MAX_CHILDREN: usize = 4;

/// How long a child with no lease lives on.
const IDLE_STOP: Duration = Duration::from_secs(600);

/// How long a child has to print its readiness line.
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// The refusal for a profile this gateway does not list (legacy wire; the canonical wire answers
/// its own `profile_mismatch`).
const INVALID_PARAMS: i64 = 4000;
/// The canonical wire's refusal for a profile it does not serve (its `profile_mismatch`).
const CANONICAL_REFUSED: i64 = 4001;
/// A child that could not be started, reached or that stopped answering.
const CHILD_FAILED: i64 = 5032;
/// Every child slot is leased.
const CHILDREN_BUSY: i64 = 4090;

/// How to start child gateways. Make it before loading any configuration: it keeps the
/// allowed part of the environment as it is then.
#[derive(Clone)]
pub struct ChildGateways {
    program: PathBuf,
    env: Vec<(OsString, OsString)>,
    max: usize,
    idle: Duration,
    start: Duration,
}

impl ChildGateways {
    /// Children run `program`, this crate's binary.
    pub fn new(program: PathBuf) -> Self {
        let env = std::env::vars_os()
            .filter(|(name, _)| {
                name.to_str().is_some_and(|name| {
                    ALLOWED_ENV.contains(&name)
                        || ALLOWED_ENV_PREFIXES.iter().any(|p| name.starts_with(p))
                })
            })
            .collect();
        Self {
            program,
            env,
            max: MAX_CHILDREN,
            idle: IDLE_STOP,
            start: START_TIMEOUT,
        }
    }

    /// At most `max` children at once ([`MAX_CHILDREN`] unless set).
    pub fn with_limit(mut self, max: usize) -> Self {
        self.max = max;
        self
    }

    /// Stop a child after `idle` with no lease (10 minutes unless set).
    pub fn with_idle_stop(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// Give a child `start` to report ready (20 seconds unless set).
    pub fn with_start_timeout(mut self, start: Duration) -> Self {
        self.start = start;
        self
    }
}

/// Why a lease was refused.
#[derive(Debug)]
pub(crate) enum Refused {
    /// The name is not a listed named profile.
    Unknown,
    /// Every slot is leased.
    Busy,
    /// The child could not be started.
    Failed(String),
}

impl Refused {
    fn rpc(&self) -> RpcError {
        match self {
            Refused::Unknown => (INVALID_PARAMS, "profile_mismatch: no such profile".into()),
            Refused::Busy => (CHILDREN_BUSY, "every profile gateway is in use".into()),
            Refused::Failed(why) => (CHILD_FAILED, format!("profile gateway failed: {why}")),
        }
    }
}

struct Running {
    child: Child,
    port: u16,
    token: String,
    /// Dropping it closes the child's standard input, which stops the child.
    _stdin: ChildStdin,
}

struct Entry {
    /// The child, behind a lock that also makes starts one at a time.
    gate: Arc<tokio::sync::Mutex<Option<Running>>>,
    leases: usize,
    last_used: Instant,
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    /// Children removed from `entries` whose exit has not yet been awaited; they hold slots.
    reaping: usize,
}

/// The children of one gateway.
pub(crate) struct Children {
    config: ChildGateways,
    root: PathBuf,
    state: Mutex<State>,
}

/// A client connection's claim on one child: the child is not stopped while it lives.
pub(crate) struct Lease {
    children: Arc<Children>,
    name: String,
    port: u16,
    token: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.children.lock();
        if let Some(entry) = state.entries.get_mut(&self.name) {
            entry.leases -= 1;
            entry.last_used = Instant::now();
        }
    }
}

impl Children {
    /// Children for the install whose home `chat` uses; starts the idle sweep.
    pub(crate) fn start(config: ChildGateways, chat: &Chat) -> Arc<Self> {
        let children = Arc::new(Self {
            root: install_root(chat.home()),
            config,
            state: Mutex::default(),
        });
        tokio::spawn(sweep(Arc::downgrade(&children)));
        children
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Counters and a map only, so a poisoned lock is still whole.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The home of `name` when it is a listed named profile.
    pub(crate) fn home_of(&self, name: &str) -> Option<PathBuf> {
        named_profile_home(&self.root, name)
    }

    /// A lease on `name`'s child, started if it is not running.
    pub(crate) async fn lease(self: &Arc<Self>, name: &str) -> Result<Lease, Refused> {
        let home = self.home_of(name).ok_or(Refused::Unknown)?;
        let (gate, evicted) = {
            let mut state = self.lock();
            let state = &mut *state;
            if let Some(entry) = state.entries.get_mut(name) {
                entry.leases += 1;
                entry.last_used = Instant::now();
                (Arc::clone(&entry.gate), None)
            } else {
                let mut evicted = None;
                if state.entries.len() + state.reaping >= self.config.max {
                    let idle = state
                        .entries
                        .iter()
                        .filter(|(_, entry)| entry.leases == 0)
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(name, _)| name.clone());
                    let Some(idle) = idle else {
                        return Err(Refused::Busy);
                    };
                    let entry = state.entries.remove(&idle).expect("chosen from the map");
                    state.reaping += 1;
                    evicted = Some(entry.gate);
                }
                let gate = Arc::new(tokio::sync::Mutex::new(None));
                let entry = Entry {
                    gate: Arc::clone(&gate),
                    leases: 1,
                    last_used: Instant::now(),
                };
                state.entries.insert(name.to_string(), entry);
                (gate, evicted)
            }
        };
        // From here the lease is held, so every return below releases it.
        let mut lease = Lease {
            children: Arc::clone(self),
            name: name.to_string(),
            port: 0,
            token: String::new(),
        };
        if let Some(gate) = evicted {
            tokio::spawn(Arc::clone(self).reap(gate));
        }
        let (port, token) = self
            .running(gate, home, None)
            .await
            .map_err(Refused::Failed)?;
        lease.port = port;
        lease.token = token;
        Ok(lease)
    }

    /// Replaces the child behind `lease` when it still holds the lease's port: nothing listens
    /// there, so it serves no one, even while its exit cannot be collected yet.
    async fn replace(self: &Arc<Self>, lease: &mut Lease) -> Result<(), String> {
        let home = self.home_of(&lease.name).ok_or("the profile is gone")?;
        // A live lease keeps its entry: only entries with no lease are evicted or swept.
        let gate = self
            .lock()
            .entries
            .get(&lease.name)
            .map(|entry| Arc::clone(&entry.gate))
            .ok_or("the profile gateway was stopped")?;
        let (port, token) = self.running(gate, home, Some(lease.port)).await?;
        lease.port = port;
        lease.token = token;
        Ok(())
    }

    /// The port and token of the child behind `gate`, started if it is not running or is on
    /// port `dead`.
    async fn running(
        &self,
        gate: Arc<tokio::sync::Mutex<Option<Running>>>,
        home: PathBuf,
        dead: Option<u16>,
    ) -> Result<(u16, String), String> {
        // The start runs in its own task, so a caller that goes away mid-start leaves a child
        // that is either running and owned by the entry, or killed and reaped.
        let program = self.config.clone();
        tokio::spawn(async move {
            let mut slot = gate.lock_owned().await;
            if let Some(running) = slot.as_mut() {
                if Some(running.port) != dead && matches!(running.child.try_wait(), Ok(None)) {
                    return Ok((running.port, running.token.clone()));
                }
                if let Some(mut gone) = slot.take() {
                    let _ = gone.child.start_kill();
                    let _ = gone.child.wait().await;
                }
            }
            let running = spawn(&program, &home).await?;
            let found = (running.port, running.token.clone());
            *slot = Some(running);
            Ok(found)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
    }

    /// Kills the child behind `gate` and awaits its exit, then frees its slot.
    async fn reap(self: Arc<Self>, gate: Arc<tokio::sync::Mutex<Option<Running>>>) {
        if let Some(mut running) = gate.lock().await.take() {
            let _ = running.child.start_kill();
            let _ = running.child.wait().await;
        }
        self.lock().reaping -= 1;
    }
}

/// Stops children with no lease for their idle time, until the gateway is gone.
async fn sweep(children: Weak<Children>) {
    loop {
        let every = match children.upgrade() {
            Some(children) => (children.config.idle / 4).max(Duration::from_millis(50)),
            None => return,
        };
        tokio::time::sleep(every).await;
        let Some(children) = children.upgrade() else {
            return;
        };
        let idle: Vec<_> = {
            let mut state = children.lock();
            let names: Vec<String> = state
                .entries
                .iter()
                .filter(|(_, e)| e.leases == 0 && e.last_used.elapsed() >= children.config.idle)
                .map(|(name, _)| name.clone())
                .collect();
            state.reaping += names.len();
            names
                .iter()
                .filter_map(|name| state.entries.remove(name))
                .map(|entry| entry.gate)
                .collect()
        };
        for gate in idle {
            tokio::spawn(Arc::clone(&children).reap(gate));
        }
    }
}

/// Starts one child for `home` and waits for its readiness line; a child that does not report
/// ready in time is killed and reaped.
async fn spawn(config: &ChildGateways, home: &Path) -> Result<Running, String> {
    let token = crate::new_token();
    let mut child = Command::new(&config.program)
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--exit-when-stdin-closes",
            "--child",
        ])
        .env_clear()
        .envs(config.env.iter().map(|(k, v)| (k, v)))
        .env("HERMES_HOME", home)
        .env(TOKEN_ENV, &token)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", config.program.display()))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        unreachable!("both are piped");
    };
    let mut lines = BufReader::new(stdout).lines();
    let port = tokio::time::timeout(config.start, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(port) = line.strip_prefix("HERMES_BACKEND_READY port=") {
                return port.trim().parse::<u16>().ok();
            }
        }
        None
    })
    .await;
    let Ok(Some(port)) = port else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err("it did not report ready".into());
    };
    // Nothing else is expected on standard output; read it so the child never blocks on it.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    Ok(Running {
        child,
        port,
        token,
        _stdin: stdin,
    })
}

type ChildSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// One client connection's link to one child.
struct Link {
    _lease: Lease,
    to_child: mpsc::UnboundedSender<String>,
    /// The requests sent and not yet answered, by id.
    pending: Arc<Mutex<Pending>>,
}

#[derive(Default)]
struct Pending {
    /// The id of the last request sent.
    last: u64,
    waiting: HashMap<u64, oneshot::Sender<Value>>,
    /// The child connection is gone: every request waiting and every later one fails at once.
    closed: bool,
}

impl Pending {
    fn lock(pending: &Mutex<Pending>) -> std::sync::MutexGuard<'_, Pending> {
        // A map and a flag, each update whole: a poisoned lock is still safe to use.
        pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Marks the link closed, which drops every waiting request unanswered.
    fn close(pending: &Mutex<Pending>) {
        let mut pending = Self::lock(pending);
        pending.closed = true;
        pending.waiting.clear();
    }
}

/// Forgets a request whose caller went away (its connection closed) before the reply came.
struct Forget<'a> {
    pending: &'a Mutex<Pending>,
    id: u64,
}

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        Pending::lock(self.pending).waiting.remove(&self.id);
    }
}

/// The value of [`CHILD_ORIGIN_HEADER`] for `origin`.
fn origin_name(origin: Origin) -> &'static str {
    match origin {
        Origin::Remote => "remote",
        Origin::Webapp => "webapp",
        Origin::Local => "local",
    }
}

/// The origin a parent named for its client in [`CHILD_ORIGIN_HEADER`]; anything else is
/// [`Origin::Remote`], the most restricted.
pub(crate) fn child_origin(headers: &axum::http::HeaderMap) -> Origin {
    match headers
        .get(CHILD_ORIGIN_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some("local") => Origin::Local,
        Some("webapp") => Origin::Webapp,
        _ => Origin::Remote,
    }
}

impl Link {
    /// Dials the leased child on the client's wire, as the client's origin, and starts copying
    /// its events to `conn`. A child whose port refuses the connection is replaced once.
    async fn dial(mut lease: Lease, conn: &Conn) -> Result<Self, String> {
        let socket = match Self::connect(&lease, conn).await {
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                Arc::clone(&lease.children).replace(&mut lease).await?;
                Self::connect(&lease, conn).await
            }
            other => other,
        }
        .map_err(|e| e.to_string())?;
        Ok(Self::attach(lease, socket, conn))
    }

    /// Opens the WebSocket to the leased child.
    async fn connect(lease: &Lease, conn: &Conn) -> Result<ChildSocket, tungstenite::Error> {
        let url = format!("ws://127.0.0.1:{}/api/ws?token={}", lease.port, lease.token);
        let mut request = url.into_client_request()?;
        let headers = request.headers_mut();
        if conn.canonical {
            let protocol = tungstenite::http::HeaderValue::from_static(CANONICAL_PROTOCOL);
            headers.insert(tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL, protocol);
        }
        headers.insert(
            CHILD_ORIGIN_HEADER,
            tungstenite::http::HeaderValue::from_static(origin_name(conn.origin)),
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(ATTACH_MAX_MESSAGE_BYTES))
            .max_frame_size(Some(ATTACH_MAX_MESSAGE_BYTES));
        let (socket, _) =
            tokio_tungstenite::connect_async_with_config(request, Some(config), false).await?;
        Ok(socket)
    }

    /// Starts copying the child's events on `socket` to `conn`.
    fn attach(lease: Lease, socket: ChildSocket, conn: &Conn) -> Self {
        let (mut sink, mut stream) = socket.split();
        let (to_child, mut outgoing) = mpsc::unbounded_channel::<String>();
        let pending: Arc<Mutex<Pending>> = Arc::default();
        let sent = Arc::clone(&pending);
        tokio::spawn(async move {
            while let Some(text) = outgoing.recv().await {
                if sink.send(tungstenite::Message::text(text)).await.is_err() {
                    break;
                }
            }
            Pending::close(&sent);
            let _ = sink.close().await;
        });
        let replies = Arc::clone(&pending);
        let outbox = conn.outbox.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let tungstenite::Message::Text(text) = message else {
                    continue;
                };
                let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if frame.get("method").is_none() {
                    // A reply goes to the request with its id, if that one still waits.
                    let waiter = frame["id"]
                        .as_u64()
                        .and_then(|id| Pending::lock(&replies).waiting.remove(&id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(frame);
                    }
                } else if frame["params"]["type"] != "gateway.ready" {
                    let text: &str = &text;
                    let queued = Outgoing::shared(text.into(), Instant::now(), None);
                    if outbox.send(queued).is_err() {
                        break;
                    }
                }
            }
            // The child connection is gone: every request waiting, and every later one, fails.
            Pending::close(&replies);
        });
        Self {
            _lease: lease,
            to_child,
            pending,
        }
    }

    /// Sends one request and waits for its reply for as long as the child's handler takes (a
    /// handler may wait for a turn or an approval), or until the child connection closes.
    async fn call(&self, method: &str, params: Map<String, Value>) -> Result<Value, RpcError> {
        let gone = || {
            (
                CHILD_FAILED,
                "profile gateway connection closed".to_string(),
            )
        };
        let (reply, answer) = oneshot::channel();
        let id = {
            let mut pending = Pending::lock(&self.pending);
            if pending.closed {
                return Err(gone());
            }
            pending.last += 1;
            let id = pending.last;
            pending.waiting.insert(id, reply);
            id
        };
        let _forget = Forget {
            pending: &self.pending,
            id,
        };
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.to_child.send(frame.to_string()).map_err(|_| gone())?;
        let frame = answer.await.map_err(|_| gone())?;
        match (frame.get("result"), frame.get("error")) {
            (Some(result), _) => Ok(result.clone()),
            (None, Some(error)) => Err((
                error["code"].as_i64().unwrap_or(CHILD_FAILED),
                error["message"].as_str().unwrap_or_default().to_string(),
            )),
            (None, None) => Err((CHILD_FAILED, "profile gateway sent no reply".into())),
        }
    }
}

/// One client connection's requests, routed by profile: the profile this process serves here,
/// another listed named profile to its child, anything else refused.
pub(crate) struct Forwards {
    chat: Arc<Chat>,
    conn: Conn,
    /// The profile the socket was opened for (`?profile=`), when it names a child.
    scope: Option<String>,
    children: Option<Arc<Children>>,
    links: Arc<tokio::sync::Mutex<HashMap<String, Arc<Link>>>>,
    /// The scoped profile's lease, taken before the socket was accepted, until its link is made.
    scope_lease: Arc<Mutex<Option<Lease>>>,
}

impl Forwards {
    /// `scope` is the socket's profile with the lease [`Children::lease`] gave for it.
    pub(crate) fn new(
        chat: Arc<Chat>,
        conn: Conn,
        scope: Option<Lease>,
        children: Option<Arc<Children>>,
    ) -> Self {
        Self {
            chat,
            conn,
            scope: scope.as_ref().map(|lease| lease.name.clone()),
            children,
            links: Arc::default(),
            scope_lease: Arc::new(Mutex::new(scope)),
        }
    }

    /// Runs one request.
    pub(crate) fn call(&self, method: &str, mut params: Value) -> Reply {
        // Fail closed: a socket opened for a profile runs only there, and a profile that is
        // not a name is refused; a null one is no profile.
        let named = match (params.get("profile"), &self.scope) {
            (None | Some(Value::Null), scope) => scope.clone(),
            (Some(Value::String(name)), None) => Some(name.clone()),
            (Some(Value::String(name)), Some(scope)) if name == scope => Some(name.clone()),
            _ => return self.refuse(),
        };
        let Some(name) = named else {
            return self.chat.call(&self.conn, method, params);
        };
        if let Value::Object(map) = &mut params {
            map.remove("profile");
        }
        if self.chat.serves(&name) {
            // The profile this process runs in, as the socket and history routes decide.
            return self.chat.call(&self.conn, method, params);
        }
        // Only a listed named profile has a child: `default` runs only in a gateway at the root.
        let Some(children) = self.children.clone() else {
            return self.refuse();
        };
        if children.home_of(&name).is_none() {
            return self.refuse();
        }
        let params = match params {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            _ => return ready(Err((INVALID_PARAMS, "params must be an object".into()))),
        };
        let links = Arc::clone(&self.links);
        let scope_lease = Arc::clone(&self.scope_lease);
        let conn = self.conn.clone();
        let method = method.to_string();
        Box::pin(async move {
            let mut held = links.lock().await;
            let link = match held.get(&name) {
                Some(link) => Arc::clone(link),
                None => {
                    let taken = scope_lease.lock().unwrap_or_else(|e| e.into_inner()).take();
                    let lease = match taken {
                        Some(lease) if lease.name == name => lease,
                        other => {
                            *scope_lease.lock().unwrap_or_else(|e| e.into_inner()) = other;
                            children.lease(&name).await.map_err(|r| r.rpc())?
                        }
                    };
                    let link = Link::dial(lease, &conn)
                        .await
                        .map_err(|e| Refused::Failed(e).rpc())?;
                    let link = Arc::new(link);
                    held.insert(name.clone(), Arc::clone(&link));
                    link
                }
            };
            drop(held);
            let result = link.call(&method, params).await;
            if matches!(&result, Err((CHILD_FAILED, _))) {
                // Drop a link that failed; the next request dials again.
                links.lock().await.remove(&name);
            }
            result
        })
    }

    /// A profile this gateway does not serve: 4001 `profile_mismatch` on the canonical wire, as
    /// its own check answers; 4000 on the legacy wire.
    fn refuse(&self) -> Reply {
        if self.conn.canonical {
            ready(Err((
                CANONICAL_REFUSED,
                "profile_mismatch: this gateway does not serve that profile".into(),
            )))
        } else {
            ready(Err(Refused::Unknown.rpc()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_takes_the_origin_its_parent_names_and_otherwise_the_most_restricted() {
        for origin in [Origin::Remote, Origin::Webapp, Origin::Local] {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                CHILD_ORIGIN_HEADER,
                axum::http::HeaderValue::from_static(origin_name(origin)),
            );
            assert_eq!(child_origin(&headers), origin);
        }
        assert_eq!(child_origin(&axum::http::HeaderMap::new()), Origin::Remote);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            CHILD_ORIGIN_HEADER,
            axum::http::HeaderValue::from_static("Local"),
        );
        assert_eq!(child_origin(&headers), Origin::Remote);
    }

    /// A link to a scripted child that answers the first 2 requests in reverse order, then
    /// echoes each method name at once.
    async fn scripted_link() -> (Link, Arc<Mutex<Pending>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut held: Vec<Value> = Vec::new();
            // After the first pair, each request is answered at once.
            let mut paired = false;
            while let Some(Ok(tungstenite::Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                held.push(request);
                if held.len() == 1 && !paired {
                    continue;
                }
                paired = true;
                for request in held.drain(..).rev() {
                    let reply = json!({"jsonrpc": "2.0", "id": request["id"],
                                       "result": {"method": request["method"]}});
                    socket
                        .send(tungstenite::Message::text(reply.to_string()))
                        .await
                        .unwrap();
                }
            }
        });
        let config = ChildGateways::new(PathBuf::from("unused"));
        let children = Arc::new(Children {
            config,
            root: PathBuf::from("unused"),
            state: Mutex::default(),
        });
        let lease = Lease {
            children,
            name: "scripted".into(),
            port,
            token: "t".into(),
        };
        let (outbox, _events) = mpsc::unbounded_channel();
        let conn = Conn::new(1, outbox, false);
        let link = Link::dial(lease, &conn).await.unwrap();
        let pending = Arc::clone(&link.pending);
        (link, pending)
    }

    #[tokio::test]
    async fn concurrent_requests_on_one_link_each_get_their_own_reply() {
        let (link, pending) = scripted_link().await;
        // Bounded, so a reply lost without dropping its sender fails the test, not hangs it.
        let (first, second) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                link.call("first.method", Map::new()),
                link.call("second.method", Map::new()),
            )
        })
        .await
        .expect("both replies within 10 s");
        assert_eq!(first.unwrap(), json!({"method": "first.method"}));
        assert_eq!(second.unwrap(), json!({"method": "second.method"}));
        assert!(Pending::lock(&pending).waiting.is_empty());
    }

    #[tokio::test]
    async fn a_request_whose_caller_leaves_is_forgotten() {
        let (link, pending) = scripted_link().await;
        // The child holds the first request until a second comes, so this one is still
        // waiting when its caller gives up.
        let gave_up =
            tokio::time::timeout(Duration::from_millis(200), link.call("held", Map::new())).await;
        assert!(gave_up.is_err());
        assert!(Pending::lock(&pending).waiting.is_empty());
    }
}
