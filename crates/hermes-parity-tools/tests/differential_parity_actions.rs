#![cfg(unix)]

use std::{env, fs, path::Path, process::Command};

use serde_json::{json, Value};

fn fixture_command(program: &str, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(env::var_os("LLVM_PROFILE_FILE").map(|path| ("LLVM_PROFILE_FILE", path)));
    command
}

fn git(root: &Path, args: &[&str]) {
    let output = fixture_command("git", root)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn parity_report(root: &Path, upstream: &str, report: &str) -> (Option<i32>, Value) {
    let output = fixture_command(env!("CARGO_BIN_EXE_hermes-parity-tools"), root)
        .args([
            "run-differential-parity-gate",
            "--local-ref",
            "local",
            "--upstream-ref",
            upstream,
            "--report-path",
            report,
            "--fallback-cmd",
            "printf invoked > .fallback-invoked; exit 7",
        ])
        .output()
        .unwrap();
    let saved = fs::read(root.join(report))
        .unwrap_or_else(|error| panic!("could not read {report}: {error}; {output:?}"));
    (
        output.status.code(),
        serde_json::from_slice(&saved).unwrap(),
    )
}

#[test]
fn parity_reports_missing_gateway_actions_and_missing_refs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-b", "upstream"]);

    let source = root.join("crates/hermes-cli/src");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("cli.rs"), "enum Commands {\n    Gateway,\n}\n").unwrap();
    fs::write(
        source.join("commands.rs"),
        r#"pub const SLASH_COMMANDS: &[(&str, &str)] = &[("/help", "help")];"#,
    )
    .unwrap();
    let gateway = r#"fn run_gateway(action: Option<&str>) {
    match action.unwrap_or("status") {
        "status" | "show" => (),
        "restart" => (),
        _ => (),
    }
}
"#;
    let main = source.join("main.rs");
    fs::write(&main, gateway).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "Add gateway actions"]);

    git(root, &["switch", "-c", "local"]);
    fs::write(&main, gateway.replace("        \"restart\" => (),\n", "")).unwrap();
    git(root, &["commit", "-am", "Drop the restart action"]);

    let (status, compared) = parity_report(root, "upstream", "actions.json");
    assert_eq!(status, Some(1));
    assert_eq!(compared["summary"]["missing_top_level"], 0);
    assert_eq!(compared["summary"]["missing_actions"], 1);
    assert_eq!(
        compared["drift"]["actions"]["gateway"]["missing_in_local"],
        json!(["restart"])
    );
    assert_eq!(
        compared["local_surface"]["actions"]["gateway"],
        json!(["show", "status"])
    );

    let (status, missing) = parity_report(root, "missing-upstream", "missing-ref.json");
    assert_eq!(status, Some(1));
    assert_eq!(missing["ok"], false);
    assert_eq!(missing["reason"], "missing_git_ref");
    assert_eq!(missing["local_ref_exists"], true);
    assert_eq!(missing["upstream_ref_exists"], false);
    assert!(missing.get("fallback").is_none());
    assert!(!root.join(".fallback-invoked").exists());
}
