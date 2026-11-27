//! `hermes [--profile P] gateway ensure --json`, the command upstream Hermes Desktop v0.23.2
//! runs to start or find its local backend (apps/desktop/electron/local-gateway.ts), with
//! upstream's verdict, exit codes and `gateway run` owner (hermes_cli/gateway_runtime_cli.py).
#![cfg(unix)]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;

/// A root under /tmp, whose short path leaves room for the socket wherever TMPDIR points.
fn private_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

fn hermes(home: &Path, args: &[&str]) -> Output {
    Command::new(cargo_bin("hermes"))
        .env("HERMES_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

fn verdict(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 1, "one line on stdout: {stdout:?}");
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout:?}"))
}

/// The pid `gateway.lock` names; none while it is empty (between owners).
fn lock_record_pid(home: &Path) -> Option<u32> {
    let raw = std::fs::read(home.join("gateway.lock")).ok()?;
    let lock: Value = serde_json::from_slice(&raw).ok()?;
    u32::try_from(lock["pid"].as_u64()?).ok()
}

fn lock_pid(home: &Path) -> u32 {
    lock_record_pid(home).expect("gateway.lock names an owner")
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .unwrap()
        .success()
}

/// Ends the owner a test started, whatever the test's outcome.
struct Owner(PathBuf);

impl Drop for Owner {
    fn drop(&mut self) {
        if let Ok(raw) = std::fs::read(self.0.join("gateway.lock")) {
            if let Some(pid) = serde_json::from_slice::<Value>(&raw)
                .ok()
                .and_then(|lock| lock["pid"].as_u64())
            {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
        }
    }
}

#[test]
fn ensure_starts_one_gateway_run_owner_and_then_attaches_to_it() {
    let (_dir, home) = private_root();
    let _owner = Owner(home.clone());
    let first = hermes(&home, &["gateway", "ensure", "--json"]);
    let stderr = String::from_utf8_lossy(&first.stderr);
    let ready = verdict(&first);
    assert_eq!(first.status.code(), Some(0), "{ready} {stderr}");
    assert_eq!(ready["state"], "ready");
    let endpoint = &ready["endpoint"];
    assert_eq!(endpoint["profile_id"], home.to_str().unwrap());
    assert_eq!(endpoint["runtime_protocol"], 1);
    assert_eq!(
        endpoint["capabilities"],
        serde_json::json!(["session-authority-v1"])
    );
    assert!(endpoint["api_origin"]
        .as_str()
        .unwrap()
        .starts_with("http://127.0.0.1:"));

    // The owner is a `gateway run` process (upstream's e2e census reads its command line and
    // gateway.lock), detached from the ensure client that started it.
    let pid = lock_pid(&home);
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|raw| String::from_utf8_lossy(&raw).replace('\0', " "))
        .unwrap_or_default();
    if cfg!(target_os = "linux") {
        assert!(cmdline.contains(" gateway run"), "{cmdline:?}");
    }
    let state: Value =
        serde_json::from_slice(&std::fs::read(home.join("gateway_state.json")).unwrap()).unwrap();
    assert_eq!(state["gateway_state"], "running");
    assert_eq!(state["pid"], pid);

    let second = hermes(&home, &["gateway", "ensure", "--json"]);
    let again = verdict(&second);
    assert_eq!(second.status.code(), Some(0), "{again}");
    assert_eq!(again["endpoint"]["instance_id"], endpoint["instance_id"]);
    assert_eq!(lock_pid(&home), pid, "no second owner");

    let stop = hermes(&home, &["gateway", "stop"]);
    assert!(stop.status.success(), "{stop:?}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!alive(pid), "gateway stop ends the owner");
    let deadline = Instant::now() + Duration::from_secs(10);
    while home.join("gateway.sock").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !home.join("gateway.sock").exists(),
        "the owner removed its socket"
    );
}

#[test]
fn concurrent_ensures_converge_on_one_owner() {
    let (_dir, home) = private_root();
    let _owner = Owner(home.clone());
    let clients: Vec<_> = (0..2)
        .map(|_| {
            Command::new(cargo_bin("hermes"))
                .env("HERMES_HOME", &home)
                .args(["gateway", "ensure", "--json"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let verdicts: Vec<Value> = clients
        .into_iter()
        .map(|client| {
            let output = client.wait_with_output().unwrap();
            let ready = verdict(&output);
            assert_eq!(output.status.code(), Some(0), "{ready}");
            ready
        })
        .collect();
    assert_eq!(
        verdicts[0]["endpoint"]["instance_id"],
        verdicts[1]["endpoint"]["instance_id"]
    );
    assert!(alive(lock_pid(&home)));
}

/// A home whose socket path would not fit runs the messaging gateway as before, without local mode.
#[test]
fn gateway_run_in_a_home_too_deep_for_a_socket_runs_without_local_mode() {
    let (_dir, root) = private_root();
    let home = root.join("d".repeat(100));
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut run = Command::new(cargo_bin("hermes"))
        .env("HERMES_HOME", &home)
        .args(["gateway", "run"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid_file = home.join("gateway.pid");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !pid_file.exists() && run.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    // Read before the kill: a stopping gateway removes its pid file.
    let ready = run.try_wait().unwrap().is_none() && pid_file.exists();
    let _ = Command::new("kill").arg(run.id().to_string()).status();
    let output = run.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(ready, "{stderr}");
    assert!(stderr.contains("Desktop local mode is off"), "{stderr}");
    assert!(!home.join("gateway.lock").exists());
}

/// `status` and `restart` must leave the lock inode a live owner holds: a new inode would let a
/// second owner claim the home whilst the first still runs.
#[test]
fn status_and_restart_keep_the_owners_lock_and_one_owner() {
    let (_dir, home) = private_root();
    let _owner = Owner(home.clone());
    let first = hermes(&home, &["gateway", "ensure", "--json"]);
    let ready = verdict(&first);
    assert_eq!(first.status.code(), Some(0), "{ready}");
    let old = lock_pid(&home);
    let inode = || std::fs::metadata(home.join("gateway.lock")).unwrap().ino();
    let before = inode();

    assert!(hermes(&home, &["gateway", "status"]).status.success());
    assert_eq!(inode(), before, "status kept the lock");
    assert!(alive(old));

    let mut restart = Command::new(cargo_bin("hermes"))
        .env("HERMES_HOME", &home)
        .args(["gateway", "restart"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while lock_record_pid(&home) != Some(restart.id()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let owner = lock_record_pid(&home);
    let _ = restart.kill();
    let _ = restart.wait();
    assert_eq!(owner, Some(restart.id()), "the restarted run owns the home");
    assert_eq!(inode(), before, "restart kept the lock inode");
    assert!(!alive(old), "the old owner is gone");
}

/// A signalled run withdraws local mode before its slower shutdown, so ensure cannot report it
/// ready once its pid file is gone: by the time it says it is shutting down, the socket and state
/// file are gone.
#[test]
fn a_signalled_run_withdraws_local_mode_before_it_shuts_down() {
    use std::io::{BufRead, BufReader};
    let (_dir, home) = private_root();
    let _owner = Owner(home.clone());
    let mut run = Command::new(cargo_bin("hermes"))
        .env("HERMES_HOME", &home)
        .args(["gateway", "run"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(run.stdout.take().unwrap()).lines();
    let mut next_line_starting = |prefix: &str| {
        lines
            .by_ref()
            .map_while(Result::ok)
            .any(|line| line.starts_with(prefix))
    };
    assert!(next_line_starting("Gateway is ready"), "the run started");
    assert!(home.join("gateway.sock").exists());
    let signalled = Command::new("kill")
        .args(["-TERM", &run.id().to_string()])
        .status()
        .unwrap();
    assert!(signalled.success());
    assert!(next_line_starting("Shutting down gateway"));
    assert!(!home.join("gateway.sock").exists(), "socket still served");
    assert!(!home.join("gateway_state.json").exists());
    assert!(run.wait().unwrap().success());
}

/// `gateway stop` sends SIGTERM as soon as the pid file names the owner, before the owner has
/// reported ready: the owner must already be listening then, or it dies leaving its socket.
/// The window is short, so without listeners this fails only intermittently (often under load):
/// a green run is not proof.
#[test]
fn a_run_signalled_once_its_pid_file_appears_still_shuts_down() {
    let (_dir, home) = private_root();
    let _owner = Owner(home.clone());
    let mut run = Command::new(cargo_bin("hermes"))
        .env("HERMES_HOME", &home)
        .args(["gateway", "run"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid_file = home.join("gateway.pid");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !pid_file.exists() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(pid_file.exists(), "the run wrote its pid file");
    let signalled = Command::new("kill")
        .args(["-TERM", &run.id().to_string()])
        .status()
        .unwrap();
    assert!(signalled.success());
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = run.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the run ignored the signal");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "the run ended on the signal: {status:?}");
    assert!(!home.join("gateway.sock").exists(), "the socket was left");
    assert!(!home.join("gateway_state.json").exists());
}

/// The first `gateway stop` removes the pid file, so a run whose shutdown hangs is found only by
/// the gateway.lock it still holds: a second stop must say it is still stopping, not that nothing
/// runs.
#[test]
fn stop_reports_a_run_still_holding_the_lock_as_stopping() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let (_dir, home) = private_root();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(home.join("gateway.lock"))
        .unwrap();
    // SAFETY: flock on a descriptor `lock` keeps open; LOCK_NB makes it return at once.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let stop = hermes(&home, &["gateway", "stop"]);
    let stdout = String::from_utf8_lossy(&stop.stdout);
    assert!(stop.status.success(), "{stop:?}");
    assert!(stdout.contains("still stopping"), "{stdout}");

    drop(lock);
    let stop = hermes(&home, &["gateway", "stop"]);
    let stdout = String::from_utf8_lossy(&stop.stdout);
    assert!(stdout.contains("nothing to stop"), "{stdout}");
}

/// A run that cannot own the home must leave the owner's pid file alone, even when the file names
/// a dead pid: removing it would leave a ready owner that `gateway stop` cannot find.
#[test]
fn a_run_refused_the_lock_leaves_the_pid_file() {
    let (_dir, home) = private_root();
    let _owner = Owner(home.clone());
    let first = hermes(&home, &["gateway", "ensure", "--json"]);
    let ready = verdict(&first);
    assert_eq!(first.status.code(), Some(0), "{ready}");
    let pid_file = home.join("gateway.pid");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !pid_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    // As a second run sees it when it reads the pid file before the owner rewrites it.
    let mut gone = Command::new("true").spawn().unwrap();
    let dead = gone.id();
    gone.wait().unwrap();
    std::fs::write(&pid_file, format!("{dead}\n")).unwrap();

    let second = hermes(&home, &["gateway", "run"]);
    assert!(!second.status.success(), "the second run was refused");
    assert_eq!(
        std::fs::read_to_string(&pid_file).unwrap().trim(),
        dead.to_string(),
        "the refused run left the pid file"
    );
}

#[test]
fn profile_selects_the_named_profile_home_or_the_root_for_default() {
    let (_dir, root) = private_root();
    let work = root.join("profiles").join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::set_permissions(
        root.join("profiles"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _owners = (Owner(root.clone()), Owner(work.clone()));

    let named = hermes(&root, &["--profile", "work", "gateway", "ensure", "--json"]);
    let named_verdict = verdict(&named);
    assert_eq!(named.status.code(), Some(0), "{named_verdict}");
    assert_eq!(
        named_verdict["endpoint"]["profile_id"],
        work.to_str().unwrap()
    );
    assert!(work.join("gateway.lock").exists());
    assert!(!root.join("gateway.lock").exists());

    let default = hermes(
        &root,
        &["--profile", "default", "gateway", "ensure", "--json"],
    );
    let default_verdict = verdict(&default);
    assert_eq!(default.status.code(), Some(0), "{default_verdict}");
    assert_eq!(
        default_verdict["endpoint"]["profile_id"],
        root.to_str().unwrap()
    );
    assert_ne!(
        default_verdict["endpoint"]["instance_id"],
        named_verdict["endpoint"]["instance_id"]
    );
}

#[test]
fn a_missing_or_invalid_profile_is_refused_before_anything_runs() {
    let (_dir, root) = private_root();
    for (name, message) in [
        ("nope", "Profile 'nope' does not exist"),
        ("../escape", "Invalid profile name"),
    ] {
        let output = hermes(&root, &["--profile", name, "gateway", "ensure", "--json"]);
        assert_eq!(output.status.code(), Some(1), "{name}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{name}: {stderr}");
        assert!(output.stdout.is_empty(), "{name}");
    }
    assert!(!root.join("gateway.lock").exists());
}

#[test]
fn an_unusable_timeout_is_an_invalid_invocation() {
    let (_dir, home) = private_root();
    for timeout in ["soon", "-1", "nan", "1e20"] {
        let output = hermes(
            &home,
            &["gateway", "ensure", "--json", "--timeout", timeout],
        );
        assert_eq!(output.status.code(), Some(2), "{timeout}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            r#"{"endpoint":null,"reason_code":"invalid_invocation","state":"inaccessible"}"#,
            "{timeout}"
        );
    }
    assert!(!home.join("gateway.lock").exists());
}
