#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::process::Command;

fn fixture() -> tempfile::TempDir {
    let built_wrapper = Path::new(env!("CARGO_BIN_EXE_hermes-ultra"))
        .canonicalize()
        .unwrap();
    let root = tempfile::Builder::new()
        .prefix("ultra-wrapper-")
        .tempdir_in(built_wrapper.parent().unwrap())
        .unwrap();
    std::fs::hard_link(built_wrapper, root.path().join("hermes-ultra")).unwrap();
    root
}

fn isolated_wrapper(root: &Path) -> Command {
    let wrapper = root.join("hermes-ultra");
    let mut command = Command::new(wrapper);
    command
        .current_dir(root)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PATH", root.join("empty-path"))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
}

fn install_target(path: &Path, script: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

async fn run_wrapper(command: &mut Command) -> Output {
    tokio::time::timeout(Duration::from_secs(8), command.output())
        .await
        .expect("wrapper process finished")
        .expect("wrapper process started")
}

#[tokio::test]
async fn ultra_wrapper_skips_itself_and_forwards_exact_arguments_to_sibling() {
    let fixture = fixture();
    let root = fixture.path();
    install_target(
        &root.join("hermes-agent-ultra"),
        "#!/bin/sh\nprintf '%s\\n' 'sibling' \"$@\"\nexit 23\n",
    );
    install_target(
        &root.join("home/.cargo/bin/hermes-agent-ultra"),
        "#!/bin/sh\nprintf '%s\\n' 'wrong target'\n",
    );

    let output = run_wrapper(
        isolated_wrapper(root)
            .env("HERMES_ULTRA_BIN", root.join("hermes-ultra"))
            .args(["chat", "two words", "--literal=*"]),
    )
    .await;

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stdout, b"sibling\nchat\ntwo words\n--literal=*\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("points back to this wrapper"));
}

#[tokio::test]
async fn ultra_wrapper_falls_back_to_the_isolated_cargo_bin_directory() {
    let fixture = fixture();
    let root = fixture.path();
    install_target(
        &root.join("home/.cargo/bin/hermes-agent-ultra"),
        "#!/bin/sh\nprintf '%s\\n' 'cargo fixture' \"$@\"\n",
    );

    let output = run_wrapper(
        isolated_wrapper(root)
            .env("HERMES_ULTRA_BIN", root.join("missing-override"))
            .arg("--help"),
    )
    .await;

    assert!(output.status.success());
    assert_eq!(output.stdout, b"cargo fixture\n--help\n");
}

#[tokio::test]
async fn ultra_wrapper_reports_unusable_targets_in_attempt_order() {
    let fixture = fixture();
    let root = fixture.path();
    let missing = root.join("missing-override");
    let sibling = root.join("hermes-agent-ultra");
    let cargo = root.join("home/.cargo/bin/hermes-agent-ultra");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::create_dir_all(&cargo).unwrap();

    for (explicit, ignored) in [(&missing, false), (&root.join("hermes-ultra"), true)] {
        let output = run_wrapper(isolated_wrapper(root).env("HERMES_ULTRA_BIN", explicit)).await;

        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.contains("points back to this wrapper"), ignored);
        let attempts = stderr
            .split_once("Failed to launch hermes-agent-ultra. Attempts: ")
            .expect("launch attempts")
            .1;
        let mut attempts = attempts.split(" ; ");
        if !ignored {
            assert!(attempts
                .next()
                .unwrap()
                .starts_with(&format!("{} (", explicit.display())));
        }
        assert!(attempts
            .next()
            .unwrap()
            .starts_with(&format!("{} (", sibling.display())));
        assert!(attempts
            .next()
            .unwrap()
            .starts_with(&format!("{} (", cargo.display())));
        assert!(attempts.next().unwrap().starts_with("hermes-agent-ultra ("));
        assert!(attempts.next().is_none());
    }
}
