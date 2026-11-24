use std::{env, process::Command};

const CHILD_TEST: &str = "HERMES_ISOLATED_COMMAND_TEST";

/// Runs the named test in a child with a disposable profile.
/// Returns true in that child so it can execute the command assertions.
pub(crate) fn enter(test: &str) -> bool {
    if env::var(CHILD_TEST).is_ok_and(|child| child == test) {
        return true;
    }

    let root = tempfile::tempdir().expect("isolated profile");
    let managed = root.path().join("managed");
    std::fs::create_dir(&managed).expect("empty managed configuration");
    let executable = env::current_dir()
        .expect("test working directory")
        .join(env::args_os().next().expect("test executable"));
    let mut command = Command::new(executable);
    command
        .current_dir(root.path())
        .env_clear()
        .env("HOME", root.path())
        .env("HERMES_HOME", root.path())
        .env("HERMES_MANAGED_DIR", managed)
        .env(CHILD_TEST, test)
        .args(["--exact", test, "--color", "never"]);
    if let Some(profile) = env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let result = command
        .output()
        .expect("run command test in isolated profile");
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success() && output.contains(&format!("test {test} ... ok")),
        "isolated command test exited {}\nstdout: {output}\nstderr: {}",
        result.status,
        String::from_utf8_lossy(&result.stderr),
    );
    false
}
