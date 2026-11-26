#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::path::absolute(std::env::temp_dir().join(format!(
            "hermes-entrypoint-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
        .expect("absolute entrypoint test directory");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("entrypoint test directory");
        let fixture = Self(root);
        fs::create_dir(fixture.0.join("bin")).expect("test executable directory");
        fixture
    }

    fn executable(&self, name: &str, body: &str) {
        let path = self.0.join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).expect("test executable");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("stub permissions");
    }

    fn entrypoint(&self) -> Command {
        let path = std::env::join_paths([self.0.join("bin")]).expect("path to test executables");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hermes-entrypoint"));
        cmd.env_clear()
            .env("PATH", path)
            .env("HERMES_HOME", self.0.join("home"))
            .current_dir(&self.0);
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_and_capture_pid(command: &mut Command) -> (u32, Output) {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start entrypoint");
    (
        child.id(),
        child.wait_with_output().expect("wait for entrypoint"),
    )
}

fn fixture_root_identity() -> Fixture {
    let fixture = Fixture::new();
    fixture.executable(
        "id",
        r#"
case "$*" in
  "-u") printf '0\n' ;;
  "-u hermes") [ -f uid_ready ] && printf '1201\n' ;;
  "-g hermes") [ -f gid_ready ] && printf '1301\n' ;;
  *) exit 1 ;;
esac
"#,
    );
    fixture.executable("getent", "exit 2\n");
    fixture.executable(
        "groupadd",
        r#"
name=${0##*/}
printf '%s %s\n' "$name" "$*" >> calls
case "$name" in
  groupmod) : > gid_ready ;;
  usermod) : > uid_ready ;;
esac
"#,
    );
    for name in ["useradd", "groupmod", "usermod", "chown"] {
        symlink("groupadd", fixture.0.join("bin").join(name)).expect("alias for test executable");
    }
    fixture
}

#[test]
fn root_entrypoint_reconciles_identity_and_replaces_itself_with_gosu() {
    let fixture = fixture_root_identity();
    fixture.executable("gosu", "printf '<%s>\\n' \"$$\" \"$@\"\n");
    let (pid, output) = run_and_capture_pid(
        fixture
            .entrypoint()
            .env("HERMES_UID", "1201")
            .env("HERMES_GID", "1301")
            .args(["fixture-program", "one value with spaces"]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        format!("<{pid}>\n<1201:1301>\n<fixture-program>\n<one value with spaces>\n").as_bytes()
    );
    let home = fixture.0.join("home");
    assert!(home.is_dir());
    let calls = fs::read_to_string(fixture.0.join("calls")).expect("provisioning calls");
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            "groupadd -o -g 1301 hermes",
            "useradd -m -o -u 1201 -g 1301 -s /bin/sh hermes",
            "groupmod -o -g 1301 hermes",
            "usermod -o -u 1201 -g 1301 hermes",
            &format!("chown -RP -- 1201:1301 {}", home.display()),
        ]
    );
}

#[test]
fn root_entrypoint_reports_missing_gosu() {
    let fixture = fixture_root_identity();
    let output = fixture
        .entrypoint()
        .env("HERMES_UID", "1201")
        .env("HERMES_GID", "1301")
        .arg("fixture-program")
        .output()
        .expect("run entrypoint");
    assert_eq!(output.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&output.stderr).contains("exec: gosu:"));
}

#[test]
fn non_root_entrypoint_handles_empty_command_and_preserves_exit_status() {
    let fixture = Fixture::new();
    fixture.executable("id", "printf '1201\\n'\n");
    fixture.executable(
        "fixture-program",
        "printf '<%s>\\n' \"$$\" \"$@\"\nexit 7\n",
    );
    let empty = fixture.entrypoint().output().expect("empty invocation");
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    assert!(fixture.0.join("home").is_dir());

    let (pid, output) = run_and_capture_pid(
        fixture
            .entrypoint()
            .args(["fixture-program", "one value with spaces"]),
    );
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        output.stdout,
        format!("<{pid}>\n<one value with spaces>\n").as_bytes()
    );

    let missing = fixture
        .entrypoint()
        .arg("missing-fixture-program")
        .output()
        .expect("run missing program");
    assert_eq!(missing.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("exec: missing-fixture-program:"));
}

#[test]
fn entrypoint_reports_home_creation_failure() {
    let fixture = Fixture::new();
    let home = fixture.0.join("home");
    fs::write(&home, b"existing file").expect("blocked home");
    let output = fixture.entrypoint().output().expect("start entrypoint");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to create"));
    assert_eq!(
        fs::read(home).expect("preserved home file"),
        b"existing file"
    );
}
