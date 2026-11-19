//! `hermes [--profile P] gateway ensure --json`, the command upstream Hermes Desktop v0.23.2
//! runs to start or find its local backend (apps/desktop/electron/local-gateway.ts), with
//! upstream's verdict, exit codes and `gateway run` owner (hermes_cli/gateway_runtime_cli.py).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;

fn private_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
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

fn lock_pid(home: &Path) -> u32 {
    let lock: Value =
        serde_json::from_slice(&std::fs::read(home.join("gateway.lock")).unwrap()).unwrap();
    lock["pid"].as_u64().unwrap() as u32
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
    for timeout in ["soon", "-1", "nan"] {
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
