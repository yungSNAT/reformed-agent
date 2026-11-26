//! Methods that act on the host: `shell.exec`, `cli.exec` and `complete.path` (upstream
//! tui_gateway/methods_tools.py and methods_complete.py).
//!
//! Each is an access boundary, so each refuses before reading its parameters unless the
//! connection holds the host-input capability ([`Conn::host_input`]): only a local-mode ticket
//! connection does, as a token on the loopback listener cannot tell this machine's client from
//! one tunnelled in. No refusal echoes request text, and nothing typed, run or printed is
//! logged. Children lead their own session and process group with a cleared environment, and a
//! supervisor task, not the handler, owns each one: on exit, timeout or a dropped request (the
//! connection closed) it kills the group (on Linux, the session too), reaps the child and stops
//! reading output soon after, whatever a descendant still holds. Windows has no local mode yet
//! and these supervisors kill process groups only, so there all 3 refuse. `shell.exec`'s pattern
//! checks are a guard, not a sandbox: shell quoting can hide a command from them, and the
//! capability, not the patterns, is the boundary.

use serde_json::{Map, Value};

use super::{Conn, Handler, Origin, Request, RpcError, INVALID_PARAMS};

pub(super) const METHODS: &[(&str, Handler)] = &[
    ("shell.exec", |r| Box::pin(shell_exec(r))),
    ("cli.exec", |r| Box::pin(cli_exec(r))),
    ("complete.path", |r| Box::pin(complete_path(r))),
];

/// A connection without the host-input capability, or a host that cannot serve the method.
const NOT_AVAILABLE: i64 = 4030;
const NOT_AVAILABLE_TEXT: &str = "not available to this client";
const NOT_ON_THIS_HOST: &str = "not available on this host yet";

/// The capability check, first in every handler here, then whether this host serves these
/// methods at all (`host_serves`, `cfg!(unix)` outside tests).
fn admit(conn: &Conn, host_serves: bool) -> Result<(), RpcError> {
    if !(conn.host_input && conn.origin == Origin::Local) {
        Err((NOT_AVAILABLE, NOT_AVAILABLE_TEXT.into()))
    } else if !host_serves {
        Err((NOT_AVAILABLE, NOT_ON_THIS_HOST.into()))
    } else {
        Ok(())
    }
}

/// An optional string parameter of at most `max` bytes with no NUL: absent or null is empty.
#[cfg_attr(not(unix), allow(dead_code, reason = "read by the Unix bodies only"))]
fn text<'a>(params: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, RpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(text)) if text.len() <= max && !text.contains('\0') => Ok(text),
        Some(Value::String(_)) => Err((
            INVALID_PARAMS,
            format!("{key} is too long or holds a NUL byte"),
        )),
        Some(_) => Err((INVALID_PARAMS, format!("{key} must be a string"))),
    }
}

async fn shell_exec(req: Request) -> Result<Value, RpcError> {
    admit(&req.conn, cfg!(unix))?;
    #[cfg(unix)]
    {
        unix::shell_exec(req).await
    }
    #[cfg(not(unix))]
    {
        Err((NOT_AVAILABLE, NOT_ON_THIS_HOST.into()))
    }
}

async fn cli_exec(req: Request) -> Result<Value, RpcError> {
    admit(&req.conn, cfg!(unix))?;
    #[cfg(unix)]
    {
        unix::cli_exec(req).await
    }
    #[cfg(not(unix))]
    {
        Err((NOT_AVAILABLE, NOT_ON_THIS_HOST.into()))
    }
}

async fn complete_path(req: Request) -> Result<Value, RpcError> {
    admit(&req.conn, cfg!(unix))?;
    #[cfg(unix)]
    {
        unix::complete_path(req).await
    }
    #[cfg(not(unix))]
    {
        Err((NOT_AVAILABLE, NOT_ON_THIS_HOST.into()))
    }
}

#[cfg(unix)]
mod unix {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Mutex;
    use std::time::Duration;

    use hermes_config::TerminalBackendType;
    use hermes_intelligence::redact::redact_sensitive_text;
    use hermes_tools::approval::{
        check_all_command_guards_with_context, detect_dangerous_command, CommandGuardContext,
    };
    use rustix::fs::{openat, statat, AtFlags, Dir, FileType, Mode, OFlags, CWD};
    use serde_json::{json, Map, Value};
    use tokio::io::{AsyncRead, AsyncReadExt};
    use tokio::process::Command;
    use tokio::sync::{oneshot, watch};

    use super::{text, RpcError, NOT_AVAILABLE};
    use crate::chat::{Chat, Request};

    const EMPTY: i64 = 4004;
    const BLOCKED: i64 = 4005;
    const BAD_ARGV: i64 = 4003;
    const BUSY: i64 = 4090;
    const TIMED_OUT: i64 = 5002;
    const NOT_STARTED: i64 = 5003;
    const NO_FOLDER: i64 = 5004;
    const CLI_TIMED_OUT: i64 = 5016;
    const CLI_FAILED: i64 = 5017;

    const SHELL_TIMEOUT: Duration = Duration::from_secs(30);
    /// Bytes read from each of a child's streams; the rest is drained and dropped.
    const READ_CAP: usize = 1 << 20;
    const PER_CONNECTION: usize = 2;
    const PER_PROCESS: usize = 4;

    /// Environment names a child keeps; every other name the gateway holds is dropped.
    const KEPT_ENV: &[&str] = &[
        "PATH", "HOME", "USER", "LOGNAME", "LANG", "TERM", "SHELL", "TMPDIR",
    ];

    /// The `!cmd` grammar needs a shell, so the request's text is the shell program, passed as
    /// one argument.
    pub(super) async fn shell_exec(req: Request) -> Result<Value, RpcError> {
        let command = text(&req.params, "command", 8192)?;
        if command.trim().is_empty() {
            return Err((EMPTY, "empty command".into()));
        }
        if req.chat.config.terminal.backend != TerminalBackendType::Local {
            return Err((
                NOT_AVAILABLE,
                "shell.exec runs only with the local terminal backend".into(),
            ));
        }
        if let Some(reason) = hardline(command) {
            return Err((
                BLOCKED,
                format!("blocked (hardline): {reason}. Use the agent for dangerous commands."),
            ));
        }
        if let Some(finding) = detect_dangerous_command(command) {
            return Err((
                BLOCKED,
                format!(
                    "blocked: {}. Use the agent for dangerous commands.",
                    finding.description
                ),
            ));
        }
        let slot = Slot::take(req.conn.id)?;
        let mut child = Command::new("/bin/sh");
        child.arg("-c").arg(command);
        match supervise(child, folder()?, SHELL_TIMEOUT, slot).await {
            Ended::Exited {
                stdout,
                stderr,
                code,
            } => Ok(json!({
                // Redacted before tailing, so a credential across the cut cannot survive in parts.
                "stdout": tail(&redact_sensitive_text(stdout), 4000),
                "stderr": tail(&redact_sensitive_text(stderr), 2000),
                "code": code,
            })),
            Ended::TimedOut => Err((TIMED_OUT, "command timed out (30s)".into())),
            Ended::NotStarted(kind) => {
                Err((NOT_STARTED, format!("could not start the command: {kind}")))
            }
        }
    }

    /// The hardline policy alone: with approval off the guard stops after it.
    fn hardline(command: &str) -> Option<String> {
        let context = CommandGuardContext {
            approval_mode_off: true,
            ..CommandGuardContext::from_env()
        };
        match check_all_command_guards_with_context(command, "local", context, None) {
            Ok(result) if result.approved => None,
            Ok(result) => Some(result.description.unwrap_or_default()),
            Err(_) => Some("the safety check could not run".into()),
        }
    }

    /// `cli.exec {argv, timeout?}`: the `hermes` CLI beside this executable, with no shell, for
    /// the exact argument sequences [`allowed`] lists; anything else is answered as blocked.
    pub(super) async fn cli_exec(req: Request) -> Result<Value, RpcError> {
        let argv = argv(&req.params)?;
        let timeout = match req.params.get("timeout") {
            None | Some(Value::Null) => 240,
            Some(value) => value
                .as_u64()
                .filter(|seconds| *seconds > 0)
                .ok_or((
                    BAD_ARGV,
                    "timeout must be a positive number of seconds".into(),
                ))?
                .min(600),
        };
        if !allowed(&req.chat.home, &argv) {
            return Ok(json!({"blocked": true, "hint": BLOCKED_HINT, "code": -1, "output": ""}));
        }
        let program = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("hermes")))
            .filter(|path| path.is_file())
            .ok_or((CLI_FAILED, "hermes CLI not found".into()))?;
        let slot = Slot::take(req.conn.id)?;
        run_cli(
            &program,
            &req.chat,
            &argv,
            Duration::from_secs(timeout),
            slot,
        )
        .await
    }

    const BLOCKED_HINT: &str =
        "this command does not run from the desktop here; run `hermes` in a terminal";

    /// `argv`: 1 to 64 strings, each 1 to 4,096 bytes with no NUL.
    fn argv(params: &Map<String, Value>) -> Result<Vec<String>, RpcError> {
        let bad = || (BAD_ARGV, "argv must be list[str]".to_string());
        let Some(Value::Array(items)) = params.get("argv") else {
            return Err(bad());
        };
        let argv = items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(bad)?;
        if argv.is_empty()
            || argv.len() > 64
            || argv
                .iter()
                .any(|arg| arg.is_empty() || arg.len() > 4096 || arg.contains('\0'))
        {
            return Err((
                BAD_ARGV,
                "argv must hold 1 to 64 strings of 1 to 4096 bytes".into(),
            ));
        }
        Ok(argv)
    }

    /// The sequences the desktop sends that the Rust CLI serves: `profile delete <name> --yes`
    /// for a profile this home lists. Anything else, an empty argv included, is not run.
    pub(super) fn allowed(home: &Path, argv: &[String]) -> bool {
        match argv {
            [profile, delete, name, yes]
                if profile == "profile" && delete == "delete" && yes == "--yes" =>
            {
                listed_profile(home, name)
            }
            _ => false,
        }
    }

    /// A plain name with its own `profiles/<name>.yaml` (or `.yml`) file, not a link.
    fn listed_profile(home: &Path, name: &str) -> bool {
        let plain = !name.is_empty()
            && name.len() <= 64
            && !name.starts_with('-')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        plain
            && ["yaml", "yml"].iter().any(|ext| {
                std::fs::symlink_metadata(home.join("profiles").join(format!("{name}.{ext}")))
                    .is_ok_and(|meta| meta.file_type().is_file())
            })
    }

    pub(super) async fn run_cli(
        program: &Path,
        chat: &Chat,
        argv: &[String],
        limit: Duration,
        slot: Slot,
    ) -> Result<Value, RpcError> {
        let mut child = Command::new(program);
        child.args(argv);
        child.env("HERMES_HOME", &chat.home);
        match supervise(child, folder()?, limit, slot).await {
            Ended::Exited {
                stdout,
                stderr,
                code,
            } => {
                let joined = [stdout.trim_end(), stderr.trim_end()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                let output = redact_sensitive_text(joined);
                let output = if output.is_empty() {
                    "(no output)".to_string()
                } else {
                    output.chars().take(48_000).collect()
                };
                Ok(json!({"blocked": false, "code": code, "output": output}))
            }
            Ended::TimedOut => Err((CLI_TIMED_OUT, "cli.exec: timeout".into())),
            Ended::NotStarted(kind) => Err((CLI_FAILED, format!("could not start hermes: {kind}"))),
        }
    }

    /// The session's folder: today the gateway's own working folder for every session. Never a
    /// fallback when it is gone.
    fn folder() -> Result<PathBuf, RpcError> {
        std::env::current_dir()
            .ok()
            .filter(|dir| dir.is_dir())
            .ok_or((NO_FOLDER, "working folder unavailable".into()))
    }

    /// The last `count` characters of `text`.
    fn tail(text: &str, count: usize) -> String {
        let skip = text.chars().count().saturating_sub(count);
        text.chars().skip(skip).collect()
    }

    /// The kept names of `vars`, without any value that looks like a credential.
    pub(super) fn kept_env(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
        vars.filter(|(name, value)| {
            (KEPT_ENV.contains(&name.as_str()) || name.starts_with("LC_"))
                && redact_sensitive_text(value) == *value
        })
        .collect()
    }

    /// Running children, per connection and in all.
    static RUNNING: Mutex<Option<HashMap<u64, usize>>> = Mutex::new(None);

    /// One running child's place in [`RUNNING`], given back when dropped.
    pub(super) struct Slot(u64);

    impl Slot {
        pub(super) fn take(conn: u64) -> Result<Self, RpcError> {
            let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
            let running = running.get_or_insert_with(HashMap::new);
            let mine = running.get(&conn).copied().unwrap_or(0);
            if mine >= PER_CONNECTION || running.values().sum::<usize>() >= PER_PROCESS {
                return Err((BUSY, "busy".into()));
            }
            running.insert(conn, mine + 1);
            Ok(Self(conn))
        }
    }

    impl Drop for Slot {
        fn drop(&mut self) {
            let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(running) = running.as_mut() {
                if let Some(count) = running.get_mut(&self.0) {
                    *count -= 1;
                    if *count == 0 {
                        running.remove(&self.0);
                    }
                }
            }
        }
    }

    pub(super) enum Ended {
        Exited {
            stdout: String,
            stderr: String,
            code: Option<i32>,
        },
        TimedOut,
        NotStarted(std::io::ErrorKind),
    }

    /// How long output is still read after the child exits: a descendant that escaped the kill
    /// can hold the pipes open, and the answer does not wait for it.
    const DRAIN: Duration = Duration::from_secs(2);

    /// Starts `command` as the leader of its own session and process group, with stdin closed,
    /// a cleared environment and `folder` as its working folder, under a supervisor task that
    /// owns it. The supervisor ends the run when the child exits, `limit` passes or this future
    /// is dropped. It then kills the group and, on Linux, every process left in the session
    /// before reaping the child, so neither id can be reused under the kill; reads output for at
    /// most [`DRAIN`] more; and gives the slot back. A descendant that starts its own session
    /// (`setsid`), or on macOS its own process group, survives the run.
    pub(super) async fn supervise(
        mut command: Command,
        folder: PathBuf,
        limit: Duration,
        slot: Slot,
    ) -> Ended {
        let env = kept_env(std::env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        }));
        command
            .env_clear()
            .envs(env)
            .current_dir(folder)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // SAFETY: setsid is async-signal-safe and touches no memory of ours.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => return Ended::NotStarted(err.kind()),
        };
        let Some(leader) = child.id().and_then(|pid| i32::try_from(pid).ok()) else {
            return Ended::NotStarted(std::io::ErrorKind::Other);
        };
        let (stop, stopped) = watch::channel(());
        let stdout = tokio::spawn(read_capped(child.stdout.take(), stopped.clone()));
        let stderr = tokio::spawn(read_capped(child.stderr.take(), stopped));
        let (mut answer, answered) = oneshot::channel();
        tokio::spawn(async move {
            let exited = tokio::select! {
                _ = tokio::task::spawn_blocking(move || wait_unreaped(leader)) => true,
                () = tokio::time::sleep(limit) => false,
                () = answer.closed() => false,
            };
            // The leader is not reaped yet, so its pid, group and session ids are still taken.
            kill_run(leader);
            let status = child.wait().await;
            let ended = if exited {
                let stopper = tokio::spawn(async move {
                    tokio::time::sleep(DRAIN).await;
                    drop(stop);
                });
                let ended = Ended::Exited {
                    stdout: stdout.await.unwrap_or_default(),
                    stderr: stderr.await.unwrap_or_default(),
                    code: status.ok().and_then(|s| s.code()),
                };
                stopper.abort();
                ended
            } else {
                stdout.abort();
                stderr.abort();
                Ended::TimedOut
            };
            drop(slot);
            let _ = answer.send(ended);
        });
        answered
            .await
            .unwrap_or(Ended::NotStarted(std::io::ErrorKind::Other))
    }

    /// Blocks until `pid` has exited, leaving it unreaped.
    fn wait_unreaped(pid: i32) {
        loop {
            // SAFETY: waitid writes only into `info`, which lives on this stack.
            let done = unsafe {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                libc::waitid(
                    libc::P_PID,
                    pid.unsigned_abs(),
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if done == 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                return;
            }
        }
    }

    /// Kills the process group led by `leader` and, on Linux, every process in its session.
    fn kill_run(leader: i32) {
        if leader <= 1 {
            return;
        }
        // SAFETY: killpg takes plain integers and touches no memory of ours.
        unsafe {
            libc::killpg(leader, libc::SIGKILL);
        }
        #[cfg(target_os = "linux")]
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i32>().ok())
            else {
                continue;
            };
            // SAFETY: getsid and kill take plain integers and touch no memory of ours.
            unsafe {
                if pid != leader && libc::getsid(pid) == leader {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }

    /// Up to [`READ_CAP`] bytes of `stream`, read until its end or until `stop`'s sender is
    /// dropped, as lossy UTF-8.
    async fn read_capped(
        stream: Option<impl AsyncRead + Unpin>,
        mut stop: watch::Receiver<()>,
    ) -> String {
        let Some(mut stream) = stream else {
            return String::new();
        };
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            tokio::select! {
                read = stream.read(&mut buf) => match read {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let room = READ_CAP.saturating_sub(kept.len()).min(read);
                        kept.extend_from_slice(&buf[..room]);
                    }
                },
                _ = stop.changed() => break,
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    }

    /// `complete.path {word, session_id?, cwd?}`: completions under the session's folder. The
    /// client's `cwd` is not used: the folder is the session's.
    pub(super) async fn complete_path(req: Request) -> Result<Value, RpcError> {
        let word = text(&req.params, "word", 1024)?.to_string();
        if word.is_empty() {
            return Ok(json!({"items": []}));
        }
        let root = folder()?;
        let items = tokio::task::spawn_blocking(move || items(&root, &word))
            .await
            .unwrap_or_default();
        Ok(json!({"items": items}))
    }

    const AT_HINTS: &[(&str, &str)] = &[
        ("@diff", "git diff"),
        ("@staged", "staged diff"),
        ("@file:", "attach file"),
        ("@folder:", "attach folder"),
        ("@url:", "fetch url"),
        ("@git:", "git log"),
    ];
    const SKIPPED_IN_CONTEXT: &[&str] = &[".git", "node_modules", "target", "__pycache__", ".venv"];
    const MAX_ITEMS: usize = 30;
    const MAX_ENTRIES: usize = 5000;

    fn item(text: String, meta: &str, display: String) -> Value {
        json!({"text": text, "display": display, "meta": meta})
    }

    /// Upstream's listing (`_dir_listing_items`) confined to `root`: a word naming an absolute
    /// path, `~`, a drive or a `..` component lists nothing, and each folder on the way is
    /// opened from its parent's handle without following a link.
    pub(super) fn items(root: &Path, word: &str) -> Vec<Value> {
        let context = word.starts_with('@');
        let query = if context { &word[1..] } else { word };
        if context && query.is_empty() {
            return AT_HINTS
                .iter()
                .map(|(text, meta)| item((*text).into(), meta, (*text).into()))
                .collect();
        }
        let (tag, path) = match query.split_once(':') {
            Some((tag @ ("file" | "folder"), path)) if context => (tag, path),
            Some(_) if context => return Vec::new(),
            _ if context && matches!(query, "file" | "folder") => (query, ""),
            _ => ("", query),
        };
        // `@/foo` means foo from here, as upstream reads it when no such absolute path exists.
        let path = if context {
            path.trim_start_matches('/')
        } else {
            path
        };
        if path.starts_with(['/', '~', '\\'])
            || path.as_bytes().get(1) == Some(&b':')
            || path.contains('\\')
        {
            return Vec::new();
        }
        let (folder, typed) = match path.rsplit_once('/') {
            Some((folder, typed)) => (folder, typed),
            None => ("", path),
        };
        let mut parts = Vec::new();
        for part in folder.split('/') {
            match part {
                "" | "." => {}
                ".." => return Vec::new(),
                part => parts.push(part),
            }
        }
        let Some(entries) = list(root, &parts) else {
            return Vec::new();
        };
        let typed_lower = typed.to_lowercase();
        let mut items = Vec::new();
        for (name, is_dir) in entries {
            if !name.to_lowercase().starts_with(&typed_lower)
                || (name.starts_with('.') && !typed.starts_with('.'))
                || (context && SKIPPED_IN_CONTEXT.contains(&name.as_str()))
                || (!tag.is_empty() && (tag == "folder") != is_dir)
            {
                continue;
            }
            let suffix = if is_dir { "/" } else { "" };
            let rel = parts
                .iter()
                .copied()
                .chain([name.as_str()])
                .collect::<Vec<_>>()
                .join("/");
            let text = if context {
                let tag = match (tag, is_dir) {
                    ("", true) => "folder",
                    ("", false) => "file",
                    (tag, _) => tag,
                };
                format!("@{tag}:{rel}{suffix}")
            } else {
                let dot = if word.starts_with("./") { "./" } else { "" };
                format!("{dot}{rel}{suffix}")
            };
            items.push(item(
                text,
                if is_dir { "dir" } else { "" },
                format!("{name}{suffix}"),
            ));
            if items.len() >= MAX_ITEMS {
                break;
            }
        }
        items
    }

    /// The sorted `(name, is_dir)` entries of `root/parts…`, each part opened relative to the
    /// one before with `O_NOFOLLOW`, so a link anywhere on the way lists nothing. A link inside
    /// the folder is listed as a plain entry and not followed.
    fn list(root: &Path, parts: &[&str]) -> Option<Vec<(String, bool)>> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let mut dir = openat(CWD, root, flags, Mode::empty()).ok()?;
        for part in parts {
            dir = openat(&dir, *part, flags | OFlags::NOFOLLOW, Mode::empty()).ok()?;
        }
        let mut entries = Vec::new();
        let reader = Dir::read_from(&dir).ok()?;
        for entry in reader.take(MAX_ENTRIES) {
            let Ok(entry) = entry else { break };
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            #[allow(clippy::unnecessary_cast, reason = "st_mode is u16 on macOS")]
            let is_dir = statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|stat| {
                FileType::from_raw_mode(stat.st_mode as rustix::fs::RawMode) == FileType::Directory
            });
            entries.push((name.to_string(), is_dir));
        }
        entries.sort();
        Some(entries)
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::{symlink, PermissionsExt};

        use super::*;

        fn texts(items: &[Value]) -> Vec<&str> {
            items
                .iter()
                .map(|item| item["text"].as_str().unwrap())
                .collect()
        }

        #[test]
        fn a_listing_stays_under_its_root_and_follows_no_link() {
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
            let root = tempfile::tempdir().unwrap();
            let root = root.path();
            std::fs::create_dir_all(root.join("src/chat")).unwrap();
            std::fs::write(root.join("src/chat.rs"), "").unwrap();
            std::fs::write(root.join(".hidden"), "").unwrap();
            symlink(outside.path(), root.join("out")).unwrap();
            symlink(outside.path(), root.join("src/away")).unwrap();

            assert_eq!(texts(&items(root, "src/ch")), ["src/chat/", "src/chat.rs"]);
            assert_eq!(
                texts(&items(root, "./src/c")),
                ["./src/chat/", "./src/chat.rs"]
            );
            // A link is listed as an entry, never as a folder, and never walked into.
            assert_eq!(texts(&items(root, "src/a")), ["src/away"]);
            assert!(items(root, "out/").is_empty());
            assert!(items(root, "out/se").is_empty());
            assert!(items(root, "src/away/").is_empty());
            assert!(items(root, "@file:out/").is_empty());
            // Nothing outside the root, however it is named.
            let absolute = format!("{}/", outside.path().display());
            assert!(items(root, &absolute).is_empty());
            assert!(items(root, "~/").is_empty());
            assert!(items(root, "../").is_empty());
            assert!(items(root, "src/../../").is_empty());
            assert!(items(root, "C:/").is_empty());
            // Hidden names only when the typed part starts with a dot.
            assert!(!texts(&items(root, "")).contains(&".hidden"));
            assert!(texts(&items(root, ".h")).contains(&".hidden"));
        }

        #[test]
        fn context_words_tag_files_and_folders_and_a_bare_at_gives_the_hints() {
            let root = tempfile::tempdir().unwrap();
            let root = root.path();
            std::fs::create_dir_all(root.join("docs")).unwrap();
            std::fs::create_dir_all(root.join("target")).unwrap();
            std::fs::write(root.join("dev.md"), "").unwrap();
            assert_eq!(texts(&items(root, "@"))[0], "@diff");
            assert_eq!(texts(&items(root, "@d")), ["@file:dev.md", "@folder:docs/"]);
            assert_eq!(texts(&items(root, "@folder:")), ["@folder:docs/"]);
            assert_eq!(
                texts(&items(root, "@/d")),
                ["@file:dev.md", "@folder:docs/"]
            );
            assert!(items(root, "@t").is_empty());
            assert!(items(root, "@url:x").is_empty());
        }

        #[test]
        fn only_a_listed_profile_delete_is_allowed() {
            let home = tempfile::tempdir().unwrap();
            let home = home.path();
            std::fs::create_dir_all(home.join("profiles")).unwrap();
            std::fs::write(home.join("profiles/work.yaml"), "").unwrap();
            symlink(
                home.join("profiles/work.yaml"),
                home.join("profiles/linked.yaml"),
            )
            .unwrap();
            let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
            assert!(allowed(
                home,
                &argv(&["profile", "delete", "work", "--yes"])
            ));
            for refused in [
                &["profile", "delete", "other", "--yes"][..],
                &["profile", "delete", "linked", "--yes"],
                &["profile", "delete", "work"],
                &["profile", "delete", "work", "--yes", "--force"],
                &["profile", "delete", "--yes", "work"],
                &["profile", "delete", "../work", "--yes"],
                &["profile", "describe", "work", "--text", "x"],
                &["--profile", "work", "config", "get", "model"],
                &["gateway"],
                &[],
            ] {
                assert!(!allowed(home, &argv(refused)), "{refused:?}");
            }
        }

        #[test]
        fn a_child_keeps_only_listed_names_and_no_credential_looking_value() {
            let vars = [
                ("PATH", "/usr/bin:/bin"),
                ("LC_ALL", "C"),
                ("OPENAI_API_KEY", "plain"),
                ("HERMES_DASHBOARD_SESSION_TOKEN", "plain"),
                ("TERM", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
            ]
            .map(|(n, v)| (n.to_string(), v.to_string()));
            let kept = kept_env(vars.into_iter());
            let names: Vec<_> = kept.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, ["PATH", "LC_ALL"]);
        }

        fn script(dir: &Path, body: &str) -> PathBuf {
            let path = dir.join("hermes");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        #[tokio::test]
        async fn the_cli_gets_each_argument_whole_and_no_shell() {
            let dir = tempfile::tempdir().unwrap();
            let program = script(
                dir.path(),
                r#"for a in "$@"; do printf '<%s>\n' "$a"; done"#,
            );
            let chat = Chat::new(hermes_config::GatewayConfig::default());
            let argv = ["a b", "$(touch pwned)", ";", "'q'"].map(String::from);
            let reply = run_cli(&program, &chat, &argv, Duration::from_secs(10), Slot(9001))
                .await
                .unwrap();
            assert_eq!(reply["output"], "<a b>\n<$(touch pwned)>\n<;>\n<'q'>");
            assert_eq!(reply["code"], 0);
            assert!(!Path::new("pwned").exists());
        }

        fn alive(pid: i32) -> bool {
            // SAFETY: signal 0 only checks that the process exists.
            unsafe { libc::kill(pid, 0) == 0 }
        }

        async fn gone(pid: i32) -> bool {
            for _ in 0..100 {
                if !alive(pid) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            false
        }

        fn background_pid(path: &Path) -> i32 {
            std::fs::read_to_string(path)
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        }

        async fn written(path: &Path) {
            for _ in 0..100 {
                if std::fs::read_to_string(path).is_ok_and(|text| text.ends_with('\n')) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("the child never started");
        }

        fn shell(pidfile: &Path) -> Command {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(format!(
                "sleep 300 & echo $! > '{}'; wait",
                pidfile.display()
            ));
            command
        }

        #[tokio::test]
        async fn a_timeout_kills_the_whole_group_and_reaps_it() {
            let dir = tempfile::tempdir().unwrap();
            let pidfile = dir.path().join("pid");
            let ended = supervise(
                shell(&pidfile),
                dir.path().to_path_buf(),
                Duration::from_secs(2),
                Slot(9002),
            )
            .await;
            assert!(matches!(ended, Ended::TimedOut));
            assert!(
                gone(background_pid(&pidfile)).await,
                "a descendant outlived the timeout"
            );
        }

        #[tokio::test]
        async fn a_dropped_request_kills_the_whole_group_and_frees_its_slot() {
            let dir = tempfile::tempdir().unwrap();
            let pidfile = dir.path().join("pid");
            let run = tokio::spawn(supervise(
                shell(&pidfile),
                dir.path().to_path_buf(),
                Duration::from_secs(300),
                Slot::take(9003).unwrap(),
            ));
            written(&pidfile).await;
            run.abort();
            assert!(
                gone(background_pid(&pidfile)).await,
                "a descendant outlived the request"
            );
            // The supervisor gave the slot back once the child was reaped.
            for _ in 0..100 {
                if Slot::take(9003).is_ok() && Slot::take(9003).is_ok() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("the slot was never given back");
        }

        /// A shell that starts a background `perl` running `escape`, waits until it has escaped
        /// and written its pid to `pid`, then exits. The `perl` keeps both output pipes open.
        fn escapee(escape: &str) -> Command {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(format!(
                "perl -MPOSIX -e '{escape}; open my $f, \">\", \"pid.tmp\"; print $f \"$$\\n\"; \
                 close $f; rename \"pid.tmp\", \"pid\"; sleep 300' & \
                 while [ ! -e pid ]; do sleep 0.05; done"
            ));
            command
        }

        fn held(conn: u64) -> usize {
            let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
            running
                .as_ref()
                .and_then(|running| running.get(&conn).copied())
                .unwrap_or(0)
        }

        /// A slot for `conn`, waiting while other tests hold the process's 4.
        async fn slot(conn: u64) -> Slot {
            for _ in 0..100 {
                if let Ok(slot) = Slot::take(conn) {
                    return slot;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("no slot came free");
        }

        fn kill(pid: i32) {
            // SAFETY: kill takes plain integers and touches no memory of ours.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }

        /// Escaped processes, killed when the test ends however it ends.
        #[derive(Default)]
        struct Escaped(Vec<i32>);

        impl Drop for Escaped {
            fn drop(&mut self) {
                self.0.iter().copied().for_each(kill);
            }
        }

        #[tokio::test]
        async fn a_child_that_leaves_its_session_holds_neither_the_answer_nor_its_slot() {
            let dir = tempfile::tempdir().unwrap();
            let mut escaped = Escaped::default();
            // 5 in a row: a leaked slot fails the third (2 a connection), and `held` at once.
            for run in 0..5 {
                let folder = dir.path().join(run.to_string());
                std::fs::create_dir(&folder).unwrap();
                let ended = tokio::time::timeout(
                    Duration::from_secs(15),
                    supervise(
                        escapee("POSIX::setsid()"),
                        folder.clone(),
                        Duration::from_secs(30),
                        slot(9005).await,
                    ),
                )
                .await;
                if folder.join("pid").exists() {
                    escaped.0.push(background_pid(&folder.join("pid")));
                }
                let ended = ended.expect("the answer waited on a descendant that left the session");
                assert!(matches!(ended, Ended::Exited { code: Some(0), .. }));
                assert_eq!(held(9005), 0, "the slot was not given back");
            }
        }

        #[tokio::test]
        async fn a_run_polled_once_and_dropped_starts_no_child() {
            use futures::FutureExt;
            let dir = tempfile::tempdir().unwrap();
            let runs = dir.path().join("runs");
            let counting = || {
                let mut command = Command::new("/bin/sh");
                command.arg("-c").arg("echo run >> runs");
                command
            };
            let once = supervise(
                counting(),
                dir.path().to_path_buf(),
                Duration::from_secs(30),
                Slot(9007),
            )
            .now_or_never();
            assert!(once.is_none(), "a child run answers only after a wait");
            // Block this runtime's only thread, so no supervisor can kill what the poll started.
            std::thread::sleep(Duration::from_secs(1));
            let count = || {
                std::fs::read_to_string(&runs)
                    .map(|text| text.lines().count())
                    .unwrap_or(0)
            };
            assert_eq!(count(), 0, "the dropped request started its command");
            let ended = supervise(
                counting(),
                dir.path().to_path_buf(),
                Duration::from_secs(30),
                Slot(9007),
            )
            .await;
            assert!(matches!(ended, Ended::Exited { code: Some(0), .. }));
            assert_eq!(count(), 1);
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn a_child_that_leaves_its_group_but_not_its_session_is_killed() {
            let dir = tempfile::tempdir().unwrap();
            let ended = tokio::time::timeout(
                Duration::from_secs(15),
                supervise(
                    escapee("POSIX::setpgid(0, 0)"),
                    dir.path().to_path_buf(),
                    Duration::from_secs(30),
                    slot(9006).await,
                ),
            )
            .await;
            let pid = background_pid(&dir.path().join("pid"));
            let _escaped = Escaped(vec![pid]);
            let survived = !gone(pid).await;
            assert!(ended.is_ok(), "the answer waited on a descendant");
            assert!(!survived, "a descendant in the run's session outlived it");
        }

        // Other tests hold slots too, so the first 2 and the last wait for the process's 4.
        #[tokio::test]
        async fn a_connection_runs_at_most_2_children() {
            let first = slot(9004).await;
            let _second = slot(9004).await;
            assert_eq!(Slot::take(9004).err().unwrap().0, BUSY);
            drop(first);
            slot(9004).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_without_the_capability_is_refused_whatever_its_origin() {
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut conn = Conn::new(1, outbox, false);
        for origin in [Origin::Remote, Origin::Webapp, Origin::Local] {
            conn.origin = origin;
            assert_eq!(admit(&conn, true).unwrap_err().0, NOT_AVAILABLE);
        }
        conn.host_input = true;
        for origin in [Origin::Remote, Origin::Webapp] {
            conn.origin = origin;
            assert_eq!(admit(&conn, true).unwrap_err().0, NOT_AVAILABLE);
        }
        conn.origin = Origin::Local;
        assert!(admit(&conn, true).is_ok());
    }

    /// Every handler passes this flag, so serving off Unix fails here on that host.
    #[test]
    fn only_a_unix_host_serves_the_host_methods() {
        assert_eq!(HOST_SERVES, cfg!(unix));
    }

    /// Run on every platform, so weakening the Windows refusal fails here, not only on Windows.
    #[test]
    fn a_host_that_cannot_serve_refuses_even_the_capability() {
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut conn = Conn::new(1, outbox, false);
        conn.host_input = true;
        conn.origin = Origin::Local;
        assert_eq!(
            admit(&conn, false).unwrap_err(),
            (NOT_AVAILABLE, NOT_ON_THIS_HOST.to_string())
        );
    }
}
