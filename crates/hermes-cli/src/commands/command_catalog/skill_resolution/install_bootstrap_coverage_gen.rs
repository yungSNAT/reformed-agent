use super::*;

use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

const BOOTSTRAP_ENV_KEYS: [&str; 3] = [
    "HERMES_SKILL_AUTO_BOOTSTRAP",
    "HERMES_SKILL_BOOTSTRAP_ASSUME_YES",
    "HERMES_SKILL_BOOTSTRAP_FORCE",
];

#[allow(unused_unsafe)]
fn set_env(key: &str, value: Option<&str>) {
    match value {
        Some(v) => unsafe { std::env::set_var(key, v) },
        None => unsafe { std::env::remove_var(key) },
    }
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = BOOTSTRAP_ENV_KEYS
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        for k in BOOTSTRAP_ENV_KEYS {
            set_env(k, None);
        }
        EnvGuard { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            set_env(k, v.as_deref());
        }
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

fn skill_md(frontmatter_extra: &str) -> Bytes {
    Bytes::from(format!(
        "---\nname: demo\ndescription: A demo skill\n{}---\n# Demo\n\nHello world.\n",
        frontmatter_extra
    ))
}

#[test]
fn install_skill_files_writes_files_and_replaces_existing() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::install_skill_files_writes_files_and_replaces_existing");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let skills = tmp.path().join("skills");
    let stale_dir = skills.join("demo");
    std::fs::create_dir_all(&stale_dir).expect("create stale");
    std::fs::write(stale_dir.join("stale.txt"), "old").expect("write stale");

    let files = vec![
        ("SKILL.md".to_string(), skill_md("")),
        (
            "docs/notes.md".to_string(),
            Bytes::from_static(b"some notes"),
        ),
    ];
    let target = install_skill_files(&skills, "demo", &files).expect("install ok");
    assert_eq!(target, skills.join("demo"));
    assert!(target.join("SKILL.md").exists());
    assert_eq!(
        std::fs::read_to_string(target.join("docs/notes.md")).expect("read"),
        "some notes"
    );
    assert!(!target.join("stale.txt").exists());
}

#[test]
fn install_skill_files_requires_skill_md() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::install_skill_files_requires_skill_md");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let skills = tmp.path().join("skills");
    let files = vec![(
        "README.md".to_string(),
        Bytes::from_static(b"# readme\nplain text"),
    )];
    let result = install_skill_files(&skills, "demo", &files);
    assert!(result.is_err());
    assert!(!skills.join("demo").join("SKILL.md").exists());
}

#[test]
fn install_skill_files_rejects_unsafe_paths() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::install_skill_files_rejects_unsafe_paths");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let skills = tmp.path().join("skills");
    let files = vec![
        ("SKILL.md".to_string(), skill_md("")),
        ("../escape.md".to_string(), Bytes::from_static(b"x")),
    ];
    let result = install_skill_files(&skills, "demo", &files);
    assert!(result.is_err());
    assert!(!skills.join("escape.md").exists());
}

#[test]
fn auto_bootstrap_enabled_respects_env() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::auto_bootstrap_enabled_respects_env");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _g = EnvGuard::new();
    assert!(skill_auto_bootstrap_enabled());
    set_env("HERMES_SKILL_AUTO_BOOTSTRAP", Some("0"));
    assert!(!skill_auto_bootstrap_enabled());
    set_env("HERMES_SKILL_AUTO_BOOTSTRAP", Some(" OFF "));
    assert!(!skill_auto_bootstrap_enabled());
    set_env("HERMES_SKILL_AUTO_BOOTSTRAP", Some("False"));
    assert!(!skill_auto_bootstrap_enabled());
    set_env("HERMES_SKILL_AUTO_BOOTSTRAP", Some("yes"));
    assert!(skill_auto_bootstrap_enabled());
}

#[test]
fn bootstrap_force_confirmed_respects_env() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::bootstrap_force_confirmed_respects_env");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _g = EnvGuard::new();
    assert!(!skill_bootstrap_force_confirmed());
    set_env("HERMES_SKILL_BOOTSTRAP_ASSUME_YES", Some(" Yes "));
    assert!(skill_bootstrap_force_confirmed());
    set_env("HERMES_SKILL_BOOTSTRAP_ASSUME_YES", Some("nope"));
    assert!(!skill_bootstrap_force_confirmed());
    set_env("HERMES_SKILL_BOOTSTRAP_ASSUME_YES", None);
    set_env("HERMES_SKILL_BOOTSTRAP_FORCE", Some("on"));
    assert!(skill_bootstrap_force_confirmed());
    set_env("HERMES_SKILL_BOOTSTRAP_FORCE", Some("off"));
    assert!(!skill_bootstrap_force_confirmed());
}

#[test]
fn collect_bootstrap_commands_handles_object_forms() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::collect_bootstrap_commands_handles_object_forms");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let value = serde_json::json!({
        "command": "  pip install a  ",
        "commands": ["npm ci", 5, ""],
        "script": "setup.py",
        "scripts": ["a.py", "   ", "b.sh", 7]
    });
    let mut out = Vec::new();
    collect_bootstrap_commands_from_value(&value, &mut out);
    assert_eq!(
        out,
        vec![
            "pip install a".to_string(),
            "npm ci".to_string(),
            "python3 setup.py".to_string(),
            "python3 a.py".to_string(),
            "bash b.sh".to_string(),
        ]
    );
}

#[test]
fn collect_bootstrap_commands_ignores_other_values() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::collect_bootstrap_commands_ignores_other_values");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let mut out = Vec::new();
    collect_bootstrap_commands_from_value(&serde_json::json!(42), &mut out);
    collect_bootstrap_commands_from_value(&serde_json::json!(null), &mut out);
    collect_bootstrap_commands_from_value(&serde_json::json!(true), &mut out);
    assert!(out.is_empty());

    collect_bootstrap_commands_from_value(&serde_json::json!({"script": "run.sh"}), &mut out);
    assert_eq!(out, vec!["bash run.sh".to_string()]);
}

#[test]
fn parse_skill_bootstrap_plan_dedups_commands() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::parse_skill_bootstrap_plan_dedups_commands");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let files = vec![
        ("README.md".to_string(), Bytes::from_static(b"readme")),
        (
            "SKILL.md".to_string(),
            skill_md("bootstrap: pip install foo\nsetup: pip install foo\ninstall: npm ci\n"),
        ),
    ];
    let plan = parse_skill_bootstrap_plan(&files)
        .expect("parse ok")
        .expect("plan present");
    assert_eq!(
        plan.commands,
        vec!["pip install foo".to_string(), "npm ci".to_string()]
    );
}

#[test]
fn parse_skill_bootstrap_plan_returns_none_without_commands() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::parse_skill_bootstrap_plan_returns_none_without_commands");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let files = vec![
        ("other.md".to_string(), Bytes::from_static(b"x")),
        ("SKILL.md".to_string(), skill_md("")),
    ];
    let plan = parse_skill_bootstrap_plan(&files).expect("parse ok");
    assert!(plan.is_none());
}

#[test]
fn parse_bootstrap_command_rejects_empty_and_long() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::parse_bootstrap_command_rejects_empty_and_long");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let err = parse_bootstrap_command("   ").unwrap_err();
    assert!(matches!(err, AgentError::Config(_)));
    assert!(err.to_string().contains("cannot be empty"));

    let long = format!("pip {}", "a".repeat(2100));
    let err = parse_bootstrap_command(&long).unwrap_err();
    assert!(err.to_string().contains("too long"));
}

#[test]
fn parse_bootstrap_command_rejects_unparseable_and_empty_tokens() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::parse_bootstrap_command_rejects_unparseable_and_empty_tokens");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let err = parse_bootstrap_command("pip install \"unterminated").unwrap_err();
    assert!(err.to_string().contains("Unable to parse"));

    let err = parse_bootstrap_command("# just a comment").unwrap_err();
    assert!(err.to_string().contains("no executable"));
}

#[cfg(unix)]
#[test]
fn parse_bootstrap_command_rejects_absolute_executable() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::parse_bootstrap_command_rejects_absolute_executable");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let err = parse_bootstrap_command("/usr/bin/pip install x").unwrap_err();
    assert!(err.to_string().contains("must be relative"));
}

#[test]
fn parse_bootstrap_command_handles_python_script_and_allowlist() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::parse_bootstrap_command_handles_python_script_and_allowlist");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let parsed = parse_bootstrap_command("scripts/setup.py --flag value").expect("ok");
    assert_eq!(parsed.executable, "python3");
    assert_eq!(
        parsed.args,
        vec![
            "scripts/setup.py".to_string(),
            "--flag".to_string(),
            "value".to_string()
        ]
    );
    assert_eq!(parsed.display, "scripts/setup.py --flag value");

    let err = parse_bootstrap_command("definitely-not-a-real-tool --x").unwrap_err();
    assert!(err.to_string().contains("not in the allowlist"));
}

#[cfg(unix)]
fn sh_command(script: &str) -> ParsedBootstrapCommand {
    ParsedBootstrapCommand {
        display: format!("sh -c {}", script),
        executable: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
    }
}

#[cfg(unix)]
#[test]
fn execute_bootstrap_command_success_and_failures() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::execute_bootstrap_command_success_and_failures");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    block_on(execute_bootstrap_command(dir, &sh_command("echo hello"))).expect("success");

    let err = block_on(execute_bootstrap_command(
        dir,
        &sh_command("echo out; echo err 1>&2; exit 3"),
    ))
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Bootstrap command failed"));
    assert!(msg.contains("stdout:"));
    assert!(msg.contains("stderr:"));
    assert!(msg.contains("out"));
    assert!(msg.contains("err"));

    let err = block_on(execute_bootstrap_command(
        dir,
        &sh_command("echo onlyerr 1>&2; exit 2"),
    ))
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("onlyerr"));
    assert!(!msg.contains("stdout:"));

    let err = block_on(execute_bootstrap_command(
        dir,
        &sh_command("echo onlyout; exit 1"),
    ))
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("stdout:"));
    assert!(msg.contains("onlyout"));
    assert!(!msg.contains("stderr:"));
}

#[test]
fn execute_bootstrap_command_missing_relative_executable() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::execute_bootstrap_command_missing_relative_executable");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let cmd = ParsedBootstrapCommand {
        display: "bin/does-not-exist".to_string(),
        executable: "bin/does-not-exist".to_string(),
        args: Vec::new(),
    };
    let err = block_on(execute_bootstrap_command(tmp.path(), &cmd)).unwrap_err();
    assert!(matches!(err, AgentError::Io(_)));
    assert!(err.to_string().contains("Failed to execute"));
}

#[test]
fn maybe_run_skill_bootstrap_skips_when_disabled() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::maybe_run_skill_bootstrap_skips_when_disabled");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _g = EnvGuard::new();
    set_env("HERMES_SKILL_AUTO_BOOTSTRAP", Some("off"));
    let tmp = tempfile::tempdir().expect("tempdir");
    // No SKILL.md: parsing would fail if bootstrap were not skipped first.
    let files = vec![("README.md".to_string(), Bytes::from_static(b"x"))];
    let result = block_on(maybe_run_skill_bootstrap("demo", tmp.path(), &files));
    assert!(result.is_ok());
}

#[test]
fn maybe_run_skill_bootstrap_errors_without_skill_md_when_enabled() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::maybe_run_skill_bootstrap_errors_without_skill_md_when_enabled");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _g = EnvGuard::new();
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = vec![("README.md".to_string(), Bytes::from_static(b"x"))];
    let result = block_on(maybe_run_skill_bootstrap("demo", tmp.path(), &files));
    assert!(result.is_err());
}

#[test]
fn maybe_run_skill_bootstrap_no_plan_is_ok() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::maybe_run_skill_bootstrap_no_plan_is_ok");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _g = EnvGuard::new();
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = vec![("SKILL.md".to_string(), skill_md(""))];
    let result = block_on(maybe_run_skill_bootstrap("demo", tmp.path(), &files));
    assert!(result.is_ok());
    assert_eq!(std::fs::read_dir(tmp.path()).expect("read").count(), 0);
}

#[test]
fn maybe_run_skill_bootstrap_only_blocked_commands() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::maybe_run_skill_bootstrap_only_blocked_commands");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _g = EnvGuard::new();
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = vec![(
        "SKILL.md".to_string(),
        skill_md("bootstrap: definitely-not-a-real-tool --flag\n"),
    )];
    let result = block_on(maybe_run_skill_bootstrap("demo", tmp.path(), &files));
    assert!(result.is_ok());
    assert_eq!(std::fs::read_dir(tmp.path()).expect("read").count(), 0);
}

#[cfg(unix)]
#[test]
fn maybe_run_skill_bootstrap_runs_forced_script() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::maybe_run_skill_bootstrap_runs_forced_script");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _g = EnvGuard::new();
    set_env("HERMES_SKILL_BOOTSTRAP_FORCE", Some("1"));
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("scripts")).expect("mkdir");
    std::fs::write(dir.join("scripts/setup.sh"), "echo ran > marker.txt\n").expect("write");
    let files = vec![(
        "SKILL.md".to_string(),
        skill_md("bootstrap: scripts/setup.sh\n"),
    )];
    let result = block_on(maybe_run_skill_bootstrap("demo", dir, &files));
    assert!(result.is_ok(), "{:?}", result);
    assert_eq!(
        std::fs::read_to_string(dir.join("marker.txt"))
            .expect("marker")
            .trim(),
        "ran"
    );
}
