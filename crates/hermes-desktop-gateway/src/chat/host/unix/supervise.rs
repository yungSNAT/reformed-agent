use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use hermes_intelligence::redact::redact_sensitive_text;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{oneshot, watch};

use super::super::RpcError;
use super::BUSY;

/// Bytes read from each of a child's streams; the rest is drained and dropped.
const READ_CAP: usize = 1 << 20;
const PER_CONNECTION: usize = 2;
const PER_PROCESS: usize = 4;

/// Environment names a child keeps; every other name the gateway holds is dropped.
const KEPT_ENV: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "LANG", "TERM", "SHELL", "TMPDIR",
];

/// The kept names of `vars`, without any value that looks like a credential.
pub(super) fn kept_env(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    vars.filter(|(name, value)| {
        (KEPT_ENV.contains(&name.as_str()) || name.starts_with("LC_"))
            && redact_sensitive_text(value) == *value
    })
    .collect()
}

/// Running children, per connection and in all.
pub(super) static RUNNING: Mutex<BTreeMap<u64, usize>> = Mutex::new(BTreeMap::new());

/// One running child's place in [`RUNNING`], given back when dropped.
pub(super) struct Slot(pub(super) u64);

impl Slot {
    pub(super) fn take(conn: u64) -> Result<Self, RpcError> {
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
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
        if let Some(count) = running.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                running.remove(&self.0);
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
/// an environment of the kept names plus those `command` sets (which skip [`kept_env`]'s
/// filter, so a caller sets none that carries a credential), and `folder` as its working
/// folder, under a supervisor task that owns it. The supervisor ends the run when the child
/// exits, `limit` passes or this future is dropped. It then kills the group and, on Linux,
/// every process left in the session before reaping the child, so neither id can be reused
/// under the kill; reads output for at most [`DRAIN`] more; and gives the slot back. A
/// descendant that starts its own session (`setsid`), or on macOS its own process group,
/// survives the run.
/// Applies the filtered environment, working folder, pipes and session setup to `command`.
fn prepare_command(command: &mut Command, folder: PathBuf) {
    let env =
        kept_env(std::env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        }));
    // Clearing drops the names the caller set as well, so they are put back after.
    let set: Vec<_> = command
        .as_std()
        .get_envs()
        .filter_map(|(name, value)| Some((name.to_owned(), value?.to_owned())))
        .collect();
    command
        .env_clear()
        .envs(env)
        .envs(set)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: `new_session` calls only setsid, which is async-signal-safe.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        command.pre_exec(new_session);
    }
}

pub(super) async fn supervise(
    mut command: Command,
    folder: PathBuf,
    limit: Duration,
    slot: Slot,
) -> Ended {
    // A request past the connection's waiting frames is polled once and, if pending,
    // dropped and answered 'send again'. Waiting here first means that poll starts nothing,
    // so a command that ran is never answered that way.
    tokio::task::yield_now().await;
    prepare_command(&mut command, folder);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return Ended::NotStarted(err.kind()),
    };
    // A child not yet waited on has its id, and a pid fits in `pid_t`.
    let leader = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .expect("a child just started has a pid");
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
            collect_exited(stdout, stderr, status, stop).await
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

async fn collect_exited(
    stdout: tokio::task::JoinHandle<String>,
    stderr: tokio::task::JoinHandle<String>,
    status: std::io::Result<std::process::ExitStatus>,
    stop: watch::Sender<()>,
) -> Ended {
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
}

/// Makes the calling process the leader of a new session and process group. Runs in the
/// forked child before exec, so it calls only setsid.
pub(super) fn new_session() -> std::io::Result<()> {
    // SAFETY: setsid takes no arguments and touches no memory of ours.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    if unsafe { libc::setsid() } == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Blocks until `pid` has exited, leaving it unreaped; returns at once if `pid` is not a
/// child of this process.
pub(super) fn wait_unreaped(pid: i32) {
    // SAFETY: waitid writes only into `info`, which lives on this stack.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let wait = || unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        libc::waitid(
            libc::P_PID,
            pid.unsigned_abs(),
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    let interrupted = || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
    while wait() != 0 && interrupted() {}
}

/// Kills the process group led by `leader` and, on Linux, every process in its session.
pub(super) fn kill_run(leader: i32) {
    if leader <= 1 {
        return;
    }
    // SAFETY: killpg takes plain integers and touches no memory of ours.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        libc::killpg(leader, libc::SIGKILL);
    }
    // A process that forks can add one behind a pass, so passes repeat until one kills
    // nothing, at most 10.
    #[cfg(target_os = "linux")]
    for _ in 0..10 {
        if sweep_session(leader) == 0 {
            break;
        }
    }
}

/// Kills every live process in `leader`'s session but the leader; returns how many.
#[cfg(target_os = "linux")]
fn sweep_session(leader: i32) -> usize {
    let session = leader.to_string();
    let mut killed = 0;
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
            .filter(|pid| *pid != leader)
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // After the command's closing parenthesis: state, parent, group, session.
        let fields: Vec<&str> = stat
            .rsplit_once(')')
            .map(|(_, rest)| rest.split_whitespace().take(4).collect())
            .unwrap_or_default();
        if fields.len() == 4 && fields[0] != "Z" && fields[3] == session {
            // SAFETY: kill takes plain integers and touches no memory of ours.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            killed += 1;
        }
    }
    killed
}

/// Up to [`READ_CAP`] bytes of `stream`, read until its end or until `stop`'s sender is
/// dropped, as lossy UTF-8.
pub(super) async fn read_capped(
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
