//! The profile's mandatory tool guard, `security.tool_guard` in `config.yaml`.
//!
//! When a profile names a guard, every local `terminal` command, `write_file`
//! and `patch` asks it first, where the command runs or the file is written.
//! The guard speaks Claude Code's PreToolUse hook format, as `guard.exe` does.
//! A command goes as a `Bash` event. A file edit goes as a Codex `apply_patch`
//! event (`--codex`) with one header for the path and one for where it lands
//! once links are followed, because that route also refuses secrets.
//!
//! Exit 0 with no output allows, and a deny carries its reason. Anything else
//! refuses the call: a missing or crashing guard, a timeout, an unreadable
//! answer, or a profile config that cannot be read. Cron scripts ask as commands.
//!
//! The config is read on each call until it names a guard, and that guard then
//! stays in force until the process exits. The model can write the config, so
//! a rewrite mid-run can add a guard but cannot remove or replace one.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use hermes_core::subprocess::CommandNoWindowExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::config::ToolGuardConfig;

/// Asks the profile's guard about a shell command that will run in `cwd`.
pub async fn check_command(command: &str, cwd: &Path) -> Result<(), String> {
    ask_command(profile_guard().await?, command, cwd).await
}

/// Asks the profile's guard about a write to `path`. A relative path is
/// judged against this process's folder, where the write would land.
pub async fn check_write(path: &Path) -> Result<(), String> {
    ask_write(profile_guard().await?, path).await
}

/// The profile folder, fixed at first use. Without `HERMES_HOME` the home
/// falls back to `~/.hermes` until `~/.hermes-agent-ultra` exists, so a
/// folder created later must not move the guard's config.
fn profile_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(crate::paths::hermes_home)
}

/// The first guard this process reads from the profile's config, kept until
/// it exits. "No guard" is never kept, so a torn read cannot pin it, and a
/// read that finds none still defers to a guard another call pinned meanwhile.
async fn profile_guard() -> Result<Option<&'static ToolGuardConfig>, String> {
    static GUARD: OnceLock<ToolGuardConfig> = OnceLock::new();
    if let Some(guard) = GUARD.get() {
        return Ok(Some(guard));
    }
    Ok(match configured_guard(profile_home()).await? {
        Some(read) => Some(GUARD.get_or_init(|| read)),
        None => GUARD.get(),
    })
}

async fn ask_command(
    guard: Option<&ToolGuardConfig>,
    command: &str,
    cwd: &Path,
) -> Result<(), String> {
    let Some(guard) = guard else {
        return Ok(());
    };
    check_form(cwd)?;
    let cwd = folder(&absolute(cwd)?)?;
    ask(guard, false, &event("Bash", command, &cwd)).await
}

async fn ask_write(guard: Option<&ToolGuardConfig>, path: &Path) -> Result<(), String> {
    let Some(guard) = guard else {
        return Ok(());
    };
    // shortcut: the path is judged, then written separately, so a link swapped
    // in between can redirect the write. Closing that needs the writers to open
    // the file first and judge the handle's final path.
    check_form(path)?;
    let target = plain(&absolute(path)?);
    let mut headers = format!("*** Update File: {target}\n");
    let lookup = Duration::from_secs(guard.timeout_secs);
    let landed = tokio::time::timeout(lookup, landing(Path::new(&target)))
        .await
        .map_err(|_| undecided("the path could not be followed in time"))??;
    if let Some(real) = landed {
        let real = plain(&real);
        if real != target {
            headers += &format!("*** Update File: {real}\n");
        }
    }
    let cwd = folder(&absolute(Path::new("."))?)?;
    ask(guard, true, &event("apply_patch", &headers, &cwd)).await
}

fn undecided(why: impl std::fmt::Display) -> String {
    format!("The tool guard refused the call: {why}.")
}

/// The guard `config.yaml` names, read without the rest of the config so an
/// unrelated setting cannot switch it off. A config that exists but cannot be
/// read refuses, as it may name a guard.
async fn configured_guard(home: &Path) -> Result<Option<ToolGuardConfig>, String> {
    let text = match tokio::fs::read_to_string(home.join("config.yaml")).await {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(undecided(format!(
                "the profile config could not be read ({e})"
            )))
        }
    };
    let config: serde_yaml::Value = serde_yaml::from_str(&text)
        .map_err(|e| undecided(format!("the profile config could not be parsed ({e})")))?;
    match config
        .get("security")
        .and_then(|security| security.get("tool_guard"))
    {
        None | Some(serde_yaml::Value::Null) => Ok(None),
        Some(guard) => serde_yaml::from_value(guard.clone())
            .map(Some)
            .map_err(|e| undecided(format!("security.tool_guard is invalid ({e})"))),
    }
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path)
        .map_err(|e| undecided(format!("the path could not be resolved ({e})")))
}

#[cfg(windows)]
fn is_drive_path(text: &str) -> bool {
    matches!(text.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}

/// `\\?\C:\x` names `C:\x`. `canonicalize` gives this form, and the cron
/// runner passes it on as `TERMINAL_CWD`.
#[cfg(windows)]
fn without_verbatim_drive(text: &str) -> &str {
    text.strip_prefix(r"\\?\")
        .filter(|rest| matches!(rest.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic()))
        .unwrap_or(text)
}

/// A path as the guard is told it: on Windows, `\\?\C:\x` becomes `C:\x`.
fn plain(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    let text = without_verbatim_drive(&text);
    text.to_string()
}

/// The event's working folder. On Windows `guard.exe` judges relative paths
/// only against a drive folder, and allows without one.
fn folder(cwd: &Path) -> Result<String, String> {
    let text = cwd
        .to_str()
        .ok_or_else(|| undecided("the working folder is not valid Unicode"))?;
    #[cfg(windows)]
    let text = without_verbatim_drive(text);
    #[cfg(windows)]
    if !is_drive_path(text) {
        return Err(undecided("the working folder has no drive letter"));
    }
    Ok(text.to_string())
}

/// Forms the guard would pass but cannot place, or that it is unsafe to look
/// up. Checked before `absolute`, which turns `\??\C:\x` into `C:\??\C:\x`.
fn check_form(path: &Path) -> Result<(), String> {
    let text = path
        .to_str()
        .ok_or_else(|| undecided("the path is not valid Unicode"))?;
    if text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(undecided("the path has a control character"));
    }
    #[cfg(windows)]
    {
        let text = without_verbatim_drive(text);
        let slashes = text.replace('\\', "/");
        if ["//?/", "//./", "/??/"]
            .iter()
            .any(|prefix| slashes.starts_with(prefix))
        {
            return Err(undecided("the path is a device path"));
        }
        if matches!(text.as_bytes(), [drive, b':', rest @ ..]
            if drive.is_ascii_alphabetic() && !matches!(rest.first(), Some(b'\\' | b'/')))
        {
            return Err(undecided(
                "the path is relative to a drive's current folder",
            ));
        }
        // `.env::$DATA` writes `.env`, and no file name can hold a colon.
        let after_drive = if is_drive_path(text) {
            &text[2..]
        } else {
            text
        };
        if after_drive.contains(':') {
            return Err(undecided("the path names a file stream"));
        }
    }
    Ok(())
}

/// Where a write to `path` lands: its deepest existing part with links and
/// 8.3 names followed, plus the rest. A path written as UNC is not looked up,
/// as that would send this account's credentials to the server; a mapped
/// drive or a link to a share still is. A link to nothing is refused, as the
/// write would go through it to wherever it points.
async fn landing(path: &Path) -> Result<Option<PathBuf>, String> {
    #[cfg(windows)]
    if plain(path).replace('\\', "/").starts_with("//") {
        return Ok(None);
    }
    for part in path.ancestors() {
        match tokio::fs::canonicalize(part).await {
            Ok(real) => {
                let rest = path.strip_prefix(part).map_err(undecided)?;
                return Ok(Some(if rest.as_os_str().is_empty() {
                    real
                } else {
                    real.join(rest)
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if tokio::fs::symlink_metadata(part).await.is_ok() {
                    return Err(undecided("the path has a link that leads nowhere"));
                }
            }
            Err(e) => return Err(undecided(format!("the path could not be followed ({e})"))),
        }
    }
    Err(undecided("no part of the path exists"))
}

fn event(tool: &str, command: &str, cwd: &str) -> String {
    json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": { "command": command },
        "cwd": cwd,
    })
    .to_string()
}

async fn ask(guard: &ToolGuardConfig, codex: bool, event: &str) -> Result<(), String> {
    let mut command = tokio::process::Command::new(&guard.command);
    command.args(&guard.args);
    if codex {
        command.arg("--codex");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .suppress_windows_console();
    let run = async {
        let mut child = command.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(event.as_bytes()).await?;
        }
        child.wait_with_output().await
    };
    match tokio::time::timeout(Duration::from_secs(guard.timeout_secs), run).await {
        Err(_) => Err(undecided("the guard did not answer in time")),
        Ok(Err(e)) => Err(undecided(format!("the guard could not be run ({e})"))),
        Ok(Ok(output)) => read_answer(output.status.code(), &output.stdout),
    }
}

/// Exit 0 with no output allows and a deny carries its reason. Everything
/// else, from another format to a crash, is undecided.
fn read_answer(exit: Option<i32>, stdout: &[u8]) -> Result<(), String> {
    if exit != Some(0) {
        return Err(undecided("the guard failed"));
    }
    if stdout.is_empty() {
        return Ok(());
    }
    let answer: Value = serde_json::from_slice(stdout).unwrap_or(Value::Null);
    let output = &answer["hookSpecificOutput"];
    match (
        output["permissionDecision"].as_str(),
        output["permissionDecisionReason"].as_str(),
    ) {
        (Some("deny"), Some(reason)) if !reason.is_empty() => Err(reason.to_string()),
        _ => Err(undecided("the guard's answer could not be read")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const DENY: &str =
        r#"{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"no"}}"#;

    /// A profile whose guard is `sh -c <script>`, with `$0` naming `record`.
    fn profile(script: &str, timeout_secs: u64) -> (TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let record = home.path().join("event.json");
        let config = json!({ "security": { "tool_guard": {
            "command": "sh",
            "args": ["-c", script, record.to_str().unwrap()],
            "timeout_secs": timeout_secs,
        }}});
        std::fs::write(
            home.path().join("config.yaml"),
            serde_yaml::to_string(&config).unwrap(),
        )
        .unwrap();
        (home, record)
    }

    fn deny_script() -> String {
        format!("cat >/dev/null; printf '%s' '{DENY}'")
    }

    /// `check_command` for the profile in `home`, without this process's pin.
    async fn check_command_in(home: &Path, command: &str, cwd: &Path) -> Result<(), String> {
        ask_command(configured_guard(home).await?.as_ref(), command, cwd).await
    }

    /// `check_write` for the profile in `home`, without this process's pin.
    async fn check_write_in(home: &Path, path: &Path) -> Result<(), String> {
        ask_write(configured_guard(home).await?.as_ref(), path).await
    }

    #[tokio::test]
    async fn no_config_or_no_guard_allows_without_asking() {
        let home = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            check_command_in(home.path(), "rm -rf x", &cwd).await,
            Ok(())
        );
        std::fs::write(
            home.path().join("config.yaml"),
            "security:\n  allow_private_urls: true\n",
        )
        .unwrap();
        assert_eq!(
            check_command_in(home.path(), "rm -rf x", &cwd).await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn command_event_carries_command_and_absolute_folder() {
        let (home, record) = profile("cat > \"$0\"", 15);
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            check_command_in(home.path(), "echo hi", Path::new(".")).await,
            Ok(())
        );
        let event: Value = serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
        assert_eq!(event["hook_event_name"], "PreToolUse");
        assert_eq!(event["tool_name"], "Bash");
        assert_eq!(event["tool_input"]["command"], "echo hi");
        assert_eq!(event["cwd"], cwd.to_str().unwrap());
    }

    #[tokio::test]
    async fn write_event_names_target_and_landing_and_asks_codex_route() {
        let (home, record) = profile("printf '%s' \"$1\" > \"$0.args\"; cat > \"$0\"", 15);
        let target = home.path().join("new").join("file.txt");
        assert_eq!(check_write_in(home.path(), &target).await, Ok(()));
        let event: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        assert_eq!(event["tool_name"], "apply_patch");
        let headers = event["tool_input"]["command"].as_str().unwrap();
        assert!(headers.starts_with(&format!("*** Update File: {}\n", target.display())));
        let real = std::fs::canonicalize(home.path())
            .unwrap()
            .join("new")
            .join("file.txt");
        assert!(headers.contains(&format!("*** Update File: {}\n", plain(&real))));
        let args = std::fs::read_to_string(record.with_extension("json.args")).unwrap();
        assert_eq!(args, "--codex");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_through_a_link_is_judged_where_it_lands_and_dangling_links_refuse() {
        let (home, record) = profile("cat > \"$0\"", 15);
        let outside = tempfile::tempdir().unwrap();
        let link = home.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert_eq!(
            check_write_in(home.path(), &link.join(".env")).await,
            Ok(())
        );
        let event: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        let real = std::fs::canonicalize(outside.path()).unwrap().join(".env");
        let headers = event["tool_input"]["command"].as_str().unwrap();
        assert!(headers.contains(&format!("*** Update File: {}\n", real.display())));

        let dangling = home.path().join("dangling");
        std::os::unix::fs::symlink(home.path().join("missing"), &dangling).unwrap();
        let refused = check_write_in(home.path(), &dangling).await.unwrap_err();
        assert!(refused.contains("leads nowhere"), "{refused}");
    }

    #[tokio::test]
    async fn deny_returns_the_guards_reason() {
        let (home, _) = profile(&deny_script(), 15);
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            check_command_in(home.path(), "echo hi", &cwd).await,
            Err("no".to_string())
        );
        assert_eq!(
            check_write_in(home.path(), &home.path().join("x")).await,
            Err("no".to_string())
        );
    }

    #[tokio::test]
    async fn anything_but_a_clean_allow_or_deny_refuses() {
        let cwd = std::env::current_dir().unwrap();
        for (script, why) in [
            ("cat >/dev/null; exit 3", "the guard failed"),
            ("cat >/dev/null; echo allow", "could not be read"),
            ("sleep 5", "did not answer in time"),
        ] {
            let (home, _) = profile(script, 1);
            let refused = check_command_in(home.path(), "echo hi", &cwd)
                .await
                .unwrap_err();
            assert!(refused.contains(why), "{script}: {refused}");
        }

        let home = tempfile::tempdir().unwrap();
        let missing = home.path().join("no-such-guard");
        let config = format!(
            "security:\n  tool_guard:\n    command: '{}'\n",
            missing.display()
        );
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        let refused = check_command_in(home.path(), "echo hi", &cwd)
            .await
            .unwrap_err();
        assert!(refused.contains("could not be run"), "{refused}");
    }

    #[tokio::test]
    async fn unreadable_or_invalid_guard_config_refuses() {
        let cwd = std::env::current_dir().unwrap();
        for config in [
            "security: [unclosed",
            "security:\n  tool_guard:\n    args: []\n",
            "security:\n  tool_guard:\n    command: guard.exe\n    timeout: 5\n",
        ] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("config.yaml"), config).unwrap();
            let refused = check_command_in(home.path(), "echo hi", &cwd).await;
            assert!(refused.is_err(), "{config}");
        }
    }

    #[tokio::test]
    async fn config_rewrites_keep_the_guard() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.yaml");
        std::fs::write(
            &path,
            "security:\n  tool_guard:\n    command: guard.exe\n    timeout_secs: 9\n",
        )
        .unwrap();
        let config = crate::load_user_config_file(&path).unwrap();
        crate::save_config_yaml(&path, &config).unwrap();
        assert_eq!(
            configured_guard(home.path()).await.unwrap(),
            Some(ToolGuardConfig {
                command: "guard.exe".into(),
                args: Vec::new(),
                timeout_secs: 9,
            })
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_forms_the_guard_cannot_place_refuse_before_asking() {
        let (home, record) = profile("cat > \"$0\"", 15);
        for (path, why) in [
            (r"\\.\PhysicalDrive0", "device path"),
            (r"\\?\UNC\server\share\x", "device path"),
            (r"\??\C:\x", "device path"),
            (r"\??\UNC\server\share\x", "device path"),
            (r"C:\x\.env::$DATA", "file stream"),
            (r"\\?\C:\x\.env::$DATA", "file stream"),
            (r"C:x", "drive's current folder"),
            (r"C:", "drive's current folder"),
        ] {
            let refused = check_write_in(home.path(), Path::new(path))
                .await
                .unwrap_err();
            assert!(refused.contains(why), "{path}: {refused}");
        }
        for (cwd, why) in [
            (r"\\server\share", "no drive letter"),
            (r"\??\C:\Windows", "device path"),
        ] {
            let refused = check_command_in(home.path(), "echo hi", Path::new(cwd))
                .await
                .unwrap_err();
            assert!(refused.contains(why), "{cwd}: {refused}");
        }
        assert!(!record.exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_verbatim_drive_paths_are_judged_as_the_drive_path() {
        let (home, record) = profile("cat > \"$0\"", 15);
        let verbatim = std::fs::canonicalize(home.path()).unwrap();
        assert!(verbatim.to_str().unwrap().starts_with(r"\\?\"));
        assert_eq!(
            check_command_in(home.path(), "echo hi", &verbatim).await,
            Ok(())
        );
        let event: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        let drive_path = &verbatim.to_str().unwrap()[4..];
        assert_eq!(event["cwd"], drive_path);
        assert_eq!(
            check_write_in(home.path(), &verbatim.join("x.txt")).await,
            Ok(())
        );
        // One header: the landing differs from the target only in its prefix.
        let event: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        assert_eq!(
            event["tool_input"]["command"],
            format!("*** Update File: {drive_path}\\x.txt\n")
        );
    }
}
