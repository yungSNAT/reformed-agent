use super::*;
use std::ffi::OsString;
use std::path::Path;
use tempfile::tempdir;

static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

#[cfg(unix)]
fn pids_for_marker(marker: &str) -> Vec<u32> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .expect("ps output");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(marker))
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .collect()
}

#[cfg(unix)]
async fn wait_for_marker(marker: &str, present: bool, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let found = !pids_for_marker(marker).is_empty();
        if found == present {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let found = !pids_for_marker(marker).is_empty();
    found == present
}

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

#[cfg(unix)]
#[test]
fn test_with_login_profile_sources_prepends_profile_loads() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    for file in [".profile", ".bash_profile", ".bashrc", ".zshrc"] {
        std::fs::write(td.path().join(file), "export HERMES_TEST=1\n").unwrap();
    }
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
    let wrapped = with_login_profile_sources("echo hi", &[], true, &[], None);
    assert!(wrapped.contains("command -v bash"));
    assert!(wrapped.contains("exec bash -lc"));
    assert!(wrapped.contains(".bash_profile"));
    assert!(wrapped.contains(".bashrc"));
    assert!(wrapped.contains("command -v zsh"));
    assert!(wrapped.contains("exec zsh -lc"));
    assert!(wrapped.contains(".zshrc"));
    assert!(wrapped.contains("echo hi"));
}

#[cfg(unix)]
#[test]
fn test_with_login_profile_sources_prefers_user_shell_when_supported() {
    let _lock = lock_env();
    let _shell = EnvGuard::set("SHELL", "/bin/zsh");
    let wrapped = with_login_profile_sources("echo hi", &[], true, &[], None);
    let preferred = "if command -v zsh >/dev/null 2>&1; then exec zsh -lc";
    let fallback = "if command -v bash >/dev/null 2>&1; then exec bash -lc";
    let preferred_idx = wrapped.find(preferred).expect("preferred zsh branch");
    let fallback_idx = wrapped.find(fallback).expect("fallback bash branch");
    assert!(
        preferred_idx < fallback_idx,
        "preferred shell branch should come before fallback"
    );
}

// Shell init files are sourced only on Unix; see `with_login_profile_sources`.
#[cfg(unix)]
#[test]
fn test_resolve_shell_init_files_auto_profile_before_bashrc() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    for file in [".profile", ".bash_profile", ".bashrc"] {
        std::fs::write(td.path().join(file), "export HERMES_TEST=1\n").unwrap();
    }
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

    let resolved = resolve_shell_init_files_for_shell("bash", &[], true, None);
    let names = resolved
        .iter()
        .filter_map(|p| p.file_name().and_then(|name| name.to_str()))
        .collect::<Vec<_>>();
    assert_eq!(names, [".profile", ".bash_profile", ".bashrc"]);
}

// Shell init files are sourced only on Unix; see `with_login_profile_sources`.
#[cfg(unix)]
#[test]
fn test_resolve_shell_init_files_explicit_list_wins_over_auto() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    std::fs::write(td.path().join(".bashrc"), "export FROM_BASHRC=1\n").unwrap();
    let custom = td.path().join("custom.sh");
    std::fs::write(&custom, "export FROM_CUSTOM=1\n").unwrap();
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

    let resolved = resolve_shell_init_files_for_shell(
        "bash",
        &[custom.to_string_lossy().to_string()],
        true,
        None,
    );
    assert_eq!(resolved, [custom]);
}

// Shell init files are sourced only on Unix; see `with_login_profile_sources`.
#[cfg(unix)]
#[test]
fn test_resolve_shell_init_files_auto_source_off_suppresses_defaults() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    std::fs::write(td.path().join(".bashrc"), "export FROM_BASHRC=1\n").unwrap();
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

    let resolved = resolve_shell_init_files_for_shell("bash", &[], false, None);
    assert!(resolved.is_empty());
}

// Shell init files are sourced only on Unix; see `with_login_profile_sources`.
#[cfg(unix)]
#[test]
fn test_resolve_shell_init_files_expands_home_and_env_vars() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    let rc_dir = td.path().join("rc");
    std::fs::create_dir_all(&rc_dir).unwrap();
    let custom = rc_dir.join("custom.sh");
    std::fs::write(&custom, "export FROM_CUSTOM=1\n").unwrap();
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
    let _custom_dir = EnvGuard::set("CUSTOM_RC_DIR", rc_dir.to_string_lossy().as_ref());

    let home_resolved =
        resolve_shell_init_files_for_shell("bash", &["~/rc/custom.sh".to_string()], false, None);
    let env_resolved = resolve_shell_init_files_for_shell(
        "bash",
        &["${CUSTOM_RC_DIR}/custom.sh".to_string()],
        false,
        None,
    );
    assert_eq!(home_resolved.as_slice(), std::slice::from_ref(&custom));
    assert_eq!(env_resolved, [custom]);
}

// Shell init files are sourced only on Unix; see `with_login_profile_sources`.
#[cfg(unix)]
#[test]
fn test_resolve_shell_init_files_uses_subprocess_home_override() {
    let _lock = lock_env();
    let real = tempdir().unwrap();
    let profile = tempdir().unwrap();
    std::fs::write(real.path().join(".bashrc"), "export REAL_HOME_RC=1\n").unwrap();
    std::fs::write(profile.path().join(".bashrc"), "export PROFILE_HOME_RC=1\n").unwrap();
    let _home = EnvGuard::set("HOME", real.path().to_string_lossy().as_ref());

    let resolved = resolve_shell_init_files_for_shell("bash", &[], true, Some(profile.path()));

    assert!(resolved
        .iter()
        .any(|p| p == &profile.path().join(".bashrc")));
    assert!(!resolved.iter().any(|p| p == &real.path().join(".bashrc")));
}

#[test]
fn test_subprocess_env_passthrough_parses_configured_and_env_values() {
    let _lock = lock_env();
    let _passthrough = EnvGuard::set(
        SUBPROCESS_ENV_PASSTHROUGH_VAR,
        "OPENAI_API_KEY,ANTHROPIC_TOKEN:BAD-NAME VALID_NAME",
    );
    let parsed = subprocess_env_passthrough_set(&["GOOGLE_API_KEY".to_string()]);
    assert!(parsed.contains("OPENAI_API_KEY"));
    assert!(parsed.contains("ANTHROPIC_TOKEN"));
    assert!(parsed.contains("VALID_NAME"));
    assert!(parsed.contains("GOOGLE_API_KEY"));
    assert!(!parsed.contains("BAD-NAME"));
}

#[cfg(unix)]
#[test]
fn test_shell_cleanup_keeps_configured_passthrough() {
    let wrapped = with_login_profile_sources(
        "echo hi",
        &[],
        true,
        &[
            "OPENAI_API_KEY".to_string(),
            "HERMES_GATEWAY_SECRET".to_string(),
        ],
        None,
    );
    assert!(wrapped.contains("HERMES_SUBPROCESS_ENV_PASSTHROUGH"));
    assert!(wrapped.contains("OPENAI_API_KEY"));
    assert!(wrapped.contains("HERMES_GATEWAY_SECRET"));
    assert!(wrapped
        .contains("${HERMES_SUBPROCESS_FORCE_TARGETS:-} ${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-}"));
}

#[test]
fn test_profile_home_mode_sets_home_and_real_home() {
    let _lock = lock_env();
    let real = tempdir().unwrap();
    let hermes = tempdir().unwrap();
    let _home = EnvGuard::set("HOME", real.path().to_string_lossy().as_ref());
    let _real_home = EnvGuard::set("HERMES_REAL_HOME", real.path().to_string_lossy().as_ref());
    let _hermes_home = EnvGuard::set("HERMES_HOME", hermes.path().to_string_lossy().as_ref());
    let backend = LocalBackend::new_with_shell_init_and_home_mode(
        10,
        1_048_576,
        Vec::new(),
        false,
        TerminalHomeMode::Profile,
        Vec::new(),
    );

    // An MSYS shell on Windows prints HOME in its POSIX form, so prove which
    // directory HOME names by writing a file through it.
    let output = block_on(backend.execute_command(
        "printf home-probe > \"$HOME/home-probe\" && printf '%s' \"$HERMES_REAL_HOME\"",
        None,
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0, "stderr={}", output.stderr);
    assert_eq!(output.stdout, real.path().display().to_string());
    assert_eq!(
        std::fs::read_to_string(hermes.path().join("home").join("home-probe")).unwrap(),
        "home-probe"
    );
}

#[test]
fn test_terminal_sanitizer_blocks_provider_env_by_default() {
    let _lock = lock_env();
    let _api = EnvGuard::set("OPENAI_API_KEY", "secret-value");
    let _passthrough = EnvGuard::remove(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    let backend = LocalBackend::new_with_shell_init(10, 1_048_576, Vec::new(), true, Vec::new());

    let output = block_on(backend.execute_command(
        "printf '%s' \"${OPENAI_API_KEY-unset}\"",
        Some(10),
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "unset");
}

#[test]
fn test_terminal_config_passthrough_allows_blocklisted_env() {
    let _lock = lock_env();
    let _api = EnvGuard::set("OPENAI_API_KEY", "secret-value");
    let _passthrough = EnvGuard::remove(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    let backend = LocalBackend::new_with_shell_init(
        10,
        1_048_576,
        Vec::new(),
        true,
        vec!["OPENAI_API_KEY".to_string()],
    );

    let output = block_on(backend.execute_command(
        "printf '%s' \"$OPENAI_API_KEY\"",
        Some(10),
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "secret-value");
}

#[test]
fn test_terminal_registry_env_passthrough_allows_prefix_blocked_env() {
    let _lock = lock_env();
    let _gateway = EnvGuard::set("HERMES_GATEWAY_SECRET", "gateway-secret");
    let _passthrough = EnvGuard::set(SUBPROCESS_ENV_PASSTHROUGH_VAR, "HERMES_GATEWAY_SECRET");
    let backend = LocalBackend::new_with_shell_init(10, 1_048_576, Vec::new(), true, Vec::new());

    let output = block_on(backend.execute_command(
        "printf '%s' \"$HERMES_GATEWAY_SECRET\"",
        Some(10),
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "gateway-secret");
}

#[test]
fn test_rewrite_compound_background_contract() {
    assert_eq!(rewrite_compound_background("A && B &"), "A && { B & }");
    assert_eq!(rewrite_compound_background("A || B &"), "A || { B & }");
    assert_eq!(
        rewrite_compound_background("A && B && C &"),
        "A && B && { C & }"
    );
    assert_eq!(
        rewrite_compound_background("cd /tmp && server &\nsleep 1"),
        "cd /tmp && { server & }\nsleep 1"
    );
    assert_eq!(rewrite_compound_background("sleep 5 &"), "sleep 5 &");
    assert_eq!(rewrite_compound_background("A && B | C &"), "A && B | C &");
    assert_eq!(
        rewrite_compound_background("A && B &>/dev/null &"),
        "A && { B &>/dev/null & }"
    );
    assert_eq!(
        rewrite_compound_background("echo 'A && B &'"),
        "echo 'A && B &'"
    );
    assert_eq!(
        rewrite_compound_background("   A && B &"),
        "   A && { B & }"
    );
    let once = rewrite_compound_background("A && B &");
    assert_eq!(rewrite_compound_background(&once), once);
}

#[test]
fn shell_init_lists_accept_json_and_delimited_paths() {
    for (value, expected) in [
        (" ", &[][..]),
        (
            r#"["path,with,comma.sh", "  second.sh  ", "", "  "]"#,
            &["path,with,comma.sh", "second.sh"][..],
        ),
        ("first.sh, , second.sh", &["first.sh", "second.sh"][..]),
        ("first.sh:: second.sh", &["first.sh", "second.sh"][..]),
        ("[literal.sh:second.sh", &["[literal.sh", "second.sh"][..]),
    ] {
        assert_eq!(parse_shell_init_list(value), expected, "{value}");
    }
}

#[cfg(unix)]
#[test]
fn shell_init_variable_expansion_preserves_non_reference_text() {
    let _lock = lock_env();
    let _directory = EnvGuard::set("HERMES_FIXTURE_INIT_ROOT", "/fixture/rc");
    let _missing = EnvGuard::remove("HERMES_FIXTURE_INIT_MISSING");
    assert_eq!(
        expand_env_refs("${HERMES_FIXTURE_INIT_ROOT}/setup${HERMES_FIXTURE_INIT_MISSING}.sh"),
        "/fixture/rc/setup.sh"
    );
    assert_eq!(
        expand_env_refs("${lowercase}/${}/${HERMES_FIXTURE_INIT_ROOT"),
        "${lowercase}/${}/${HERMES_FIXTURE_INIT_ROOT"
    );
}

#[test]
fn compound_background_rewrite_only_wraps_top_level_chain_tails() {
    for (input, expected) in [
        (
            "printf '%s' 'a && b' && printf done &",
            "printf '%s' 'a && b' && { printf done & }",
        ),
        (
            "printf '%s' \"a || b\" || printf done &",
            "printf '%s' \"a || b\" || { printf done & }",
        ),
        (
            "(printf a || printf b) && printf done &",
            "(printf a || printf b) && { printf done & }",
        ),
        (
            "printf '%s' $(printf a && printf b) && printf done &",
            "printf '%s' $(printf a && printf b) && { printf done & }",
        ),
    ] {
        assert_eq!(rewrite_compound_background(input), expected, "{input}");
    }
    for input in [
        "# printf a && printf done &\n\n",
        "false && printf skipped; printf done &",
        "printf a && printf '%s' escaped\\&",
        "printf a &&",
        "printf a && &",
        "printf '%s' 'a && b' &",
        "printf '%s' \"$(printf a && printf b)\" &",
    ] {
        assert_eq!(rewrite_compound_background(input), input);
    }
}

#[cfg(unix)]
#[test]
fn test_foreground_process_group_avoids_fork_hook() {
    let source = [
        include_str!("../local.rs"),
        include_str!("../local/env_shell.rs"),
        include_str!("../local/process_tree.rs"),
    ]
    .join("\n");
    let forbidden = ["pre", "_exec"].concat();
    assert!(
        !source.contains(&forbidden),
        "{forbidden} fork hook must not be used in local foreground process setup"
    );
    assert!(source.contains("process_group(0)"));
}

#[cfg(not(unix))]
#[test]
fn test_with_login_profile_sources_is_passthrough_off_unix() {
    let wrapped = with_login_profile_sources("echo hi", &[], true, &[], None);
    assert_eq!(wrapped, "echo hi");
}

#[tokio::test]
async fn test_execute_command_echo() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("echo hello", None, None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.trim().contains("hello"));
}

/// A foreground command must not read the agent's stdin, which under ACP or
/// `mcp serve` is the JSON-RPC stream. A child copy of this test binary gets
/// a stdin that holds a line and stays open, and runs `cat` in the foreground:
/// with the agent's stdin, `cat` echoes the line and then waits for more.
#[test]
fn a_foreground_command_cannot_read_the_agent_stdin() {
    const MARK: &str = "agent-stdin-line";
    if std::env::var_os("REFORMED_TEST_CHILD").is_some() {
        let output =
            block_on(LocalBackend::default().execute_command("cat", None, None, false, false))
                .expect("cat");
        assert!(!output.stdout.contains(MARK), "{}", output.stdout);
        return;
    }
    use std::io::Write;
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "local::tests::a_foreground_command_cannot_read_the_agent_stdin",
            "--test-threads=1",
        ])
        .env("REFORMED_TEST_CHILD", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(format!("{MARK}\n").as_bytes()).unwrap();
    stdin.flush().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the foreground command read the agent's stdin and waited on it");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}"
    );
}

#[tokio::test]
async fn test_execute_command_with_stdin_data() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command_with_stdin("cat", None, None, false, false, Some("hello stdin"))
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("hello stdin"));
}

#[tokio::test]
async fn test_execute_command_with_workdir() {
    let dir = tempdir().unwrap();
    // An MSYS shell prints `/c/...` for `C:\...`, so compare the last part.
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("pwd", None, dir.path().to_str(), false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.trim().ends_with(&name), "{}", output.stdout);
}

// Shell init files are sourced only on Unix; see `with_login_profile_sources`.
#[cfg(unix)]
#[tokio::test]
async fn test_execute_command_sources_explicit_shell_init_file() {
    let td = tempdir().unwrap();
    let init = td.path().join("custom-init.sh");
    std::fs::write(
        &init,
        "export HERMES_SHELL_INIT_PROBE=probe-ok\nexport PATH=\"/opt/shell-init-probe/bin:$PATH\"\n",
    )
    .unwrap();
    let backend = LocalBackend::new_with_shell_init(
        10,
        1_048_576,
        vec![init.to_string_lossy().to_string()],
        false,
        Vec::new(),
    );

    let output = backend
        .execute_command(
            "printf '%s|%s' \"$HERMES_SHELL_INIT_PROBE\" \"$PATH\"",
            Some(10),
            Some(td.path().to_string_lossy().as_ref()),
            false,
            false,
        )
        .await
        .unwrap();

    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("probe-ok"));
    assert!(output.stdout.contains("/opt/shell-init-probe/bin"));
}

#[tokio::test]
async fn test_execute_command_timeout() {
    let backend = LocalBackend::new(1, 1_048_576);
    let result = backend
        .execute_command("sleep 30", None, None, false, false)
        .await;
    assert!(result.is_err());
    match result {
        Err(AgentError::Timeout(_)) => {}
        _ => panic!("Expected timeout error"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn test_foreground_child_group_is_killed_when_future_is_aborted() {
    let backend = Arc::new(LocalBackend::new(60, 1_048_576));
    let marker = format!("hermes_abort_guard_{}", std::process::id());
    let command = format!("python3 -c 'import time; time.sleep(60)' {marker}");
    let task = {
        let backend = backend.clone();
        tokio::spawn(async move {
            backend
                .execute_command(&command, Some(60), None, false, false)
                .await
        })
    };

    assert!(
        wait_for_marker(&marker, true, std::time::Duration::from_secs(5)).await,
        "test setup failed to observe marker process"
    );
    task.abort();
    let _ = task.await;
    assert!(
        wait_for_marker(&marker, false, std::time::Duration::from_secs(5)).await,
        "foreground process marker survived future cancellation: {:?}",
        pids_for_marker(&marker)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_plain_shell_background_child_does_not_hang_foreground_collection() {
    let backend = LocalBackend::new(10, 1_048_576);
    let marker = "hermes_bg_nohang_marker";
    let probe = "hermes_bg_nohang_probe";
    let command = format!("python3 -c 'import time; time.sleep(60)' {probe} & echo {marker}");
    let started = std::time::Instant::now();
    let output = backend
        .execute_command(&command, Some(5), None, false, false)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let _ = tokio::process::Command::new("pkill")
        .args(["-f", probe])
        .status()
        .await;

    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "foreground collection hung for {elapsed:?}"
    );
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains(marker), "stdout={:?}", output.stdout);
}

#[tokio::test]
async fn test_foreground_collection_preserves_multibyte_utf8_boundaries() {
    let backend = LocalBackend::new(10, 100_000);
    let command = "python3 -c 'import sys; sys.stdout.buffer.write(chr(0x65e5).encode(\"utf-8\") * 10000); sys.stdout.buffer.write(b\"\\n\")'";
    let output = backend
        .execute_command(command, Some(10), None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.matches('\u{65e5}').count(), 10_000);
    assert!(!output.stdout.contains("binary output detected"));
}

#[tokio::test]
async fn test_foreground_collection_preserves_high_volume_line_output() {
    let backend = LocalBackend::new(10, 1_048_576);
    let output = backend
        .execute_command("seq 1 3000", Some(10), None, false, false)
        .await
        .unwrap();
    let lines = output.stdout.trim().split('\n').collect::<Vec<_>>();
    assert_eq!(output.exit_code, 0);
    assert_eq!(lines.len(), 3000);
    assert_eq!(lines.first().copied(), Some("1"));
    assert_eq!(lines.last().copied(), Some("3000"));
}

#[tokio::test]
async fn test_foreground_collection_replaces_invalid_utf8() {
    let backend = LocalBackend::new(10, 1_048_576);
    let command = r"printf 'before \377\376 after\n'";
    let output = backend
        .execute_command(command, Some(5), None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("before"));
    assert!(output.stdout.contains("after"));
    assert!(output.stdout.contains('\u{fffd}'));
    assert!(!output.stdout.contains("binary output detected"));
}

#[tokio::test]
async fn test_execute_command_failure() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("exit 42", None, None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 42);
}

#[tokio::test]
async fn test_write_and_read_file() {
    let dir = std::env::temp_dir().join("hermes_test_write_read");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_file.txt");
    let path_str = path.to_string_lossy().to_string();

    let backend = LocalBackend::default();

    backend
        .write_file(&path_str, "hello\nworld\nfoo\nbar")
        .await
        .unwrap();
    let content = backend.read_file(&path_str, None, None).await.unwrap();
    assert_eq!(content, "hello\nworld\nfoo\nbar");

    // Test with offset
    let content = backend.read_file(&path_str, Some(1), None).await.unwrap();
    assert_eq!(content, "world\nfoo\nbar");

    // Test with offset and limit
    let content = backend
        .read_file(&path_str, Some(1), Some(2))
        .await
        .unwrap();
    assert_eq!(content, "world\nfoo");

    // Cleanup
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[tokio::test]
async fn unpaged_local_read_preserves_mixed_line_endings_and_final_newline() {
    let directory = tempdir().unwrap();
    let target = directory.path().join("notes.txt");
    let original = "heading\r\nβeta\nfinish\r\n";
    tokio::fs::write(&target, original).await.unwrap();
    let path = target.to_str().unwrap();
    let backend = LocalBackend::default();

    assert_eq!(
        backend.read_file(path, Some(1), Some(1)).await.unwrap(),
        "βeta"
    );
    assert_eq!(backend.read_file(path, None, None).await.unwrap(), original);
}

#[tokio::test]
async fn local_read_error_identifies_the_missing_fixture() {
    let directory = tempdir().unwrap();
    let missing = directory.path().join("missing-notes.txt");
    let error = LocalBackend::default()
        .read_file(&missing.to_string_lossy(), None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AgentError::Io(message)
            if message.starts_with("Failed to read file") && message.contains("missing-notes.txt")
    ));
}

#[test]
fn test_relative_file_paths_use_terminal_cwd() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    let terminal_cwd = td.path().join("worktree");
    std::fs::create_dir_all(&terminal_cwd).unwrap();
    let _cwd_guard = EnvGuard::set("TERMINAL_CWD", terminal_cwd.to_string_lossy().as_ref());
    let backend = LocalBackend::default();

    block_on(backend.write_file("nested/file.txt", "from terminal cwd")).unwrap();

    let expected = terminal_cwd.join("nested/file.txt");
    assert_eq!(
        std::fs::read_to_string(&expected).unwrap(),
        "from terminal cwd"
    );
    assert_eq!(
        block_on(backend.read_file("nested/file.txt", None, None)).unwrap(),
        "from terminal cwd"
    );
}

#[tokio::test]
async fn test_file_exists() {
    let dir = tempdir().unwrap();
    let backend = LocalBackend::default();

    // A path that should exist
    assert!(backend
        .file_exists(dir.path().to_str().unwrap())
        .await
        .unwrap());

    // A path that should not exist
    let missing = dir.path().join("hermes_nonexistent_test_file_xyz");
    assert!(!backend
        .file_exists(missing.to_str().unwrap())
        .await
        .unwrap());
}

#[test]
fn test_resolve_path_rejects_tilde_injection() {
    let malicious = "~; echo PWNED > /tmp/hermes_local_backend_injection";
    let resolved = resolve_path(malicious).unwrap();
    assert_eq!(resolved, Path::new(malicious));
    assert!(!Path::new("/tmp/hermes_local_backend_injection").exists());
}

#[test]
fn test_resolve_path_expands_tilde_username_with_suffix() {
    let _lock = lock_env();
    let Some(user) = current_username() else {
        return;
    };
    let Some(home) = home_dir() else {
        return;
    };

    let resolved = resolve_path(&format!("~{user}/workspace/file.txt")).unwrap();
    assert!(resolved.starts_with(&home));
    assert!(resolved.ends_with("workspace/file.txt"));
}

#[test]
fn test_write_file_expands_tilde_home() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    let _home_guard = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
    let backend = LocalBackend::default();
    let file = "~/nested/path/test.txt";

    block_on(backend.write_file(file, "ok")).unwrap();
    let expanded = td.path().join("nested/path/test.txt");
    let content = std::fs::read_to_string(&expanded).unwrap();
    assert_eq!(content, "ok");
}

#[test]
fn test_execute_command_strips_gateway_env_vars() {
    let _lock = lock_env();
    let _token_guard = EnvGuard::set("TOOL_GATEWAY_USER_TOKEN", "should-not-leak");
    let _managed_guard = EnvGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
    let _http_guard = EnvGuard::set("HERMES_HTTP_API_KEY", "secret-http-key");
    let _safe_guard = EnvGuard::set("SAFE_PASSTHRU_TEST", "ok");
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
            "printf '%s|%s|%s|%s' \"${TOOL_GATEWAY_USER_TOKEN:-}\" \"${HERMES_ENABLE_NOUS_MANAGED_TOOLS:-}\" \"${HERMES_HTTP_API_KEY:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
            None,
            None,
            false,
            false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "|||ok");
}

#[test]
fn test_execute_command_strips_provider_tool_and_gateway_env_vars() {
    let _lock = lock_env();
    let _openai_key = EnvGuard::set("OPENAI_API_KEY", "sk-should-not-leak");
    let _openai_base = EnvGuard::set("OPENAI_BASE_URL", "http://localhost:8000/v1");
    let _bedrock_bearer = EnvGuard::set("AWS_BEARER_TOKEN_BEDROCK", "bedrock-secret");
    let _github = EnvGuard::set("GITHUB_TOKEN", "ghp-secret");
    let _modal = EnvGuard::set("MODAL_TOKEN_SECRET", "modal-secret");
    let _gateway = EnvGuard::set("GATEWAY_ALLOWED_USERS", "alice,bob");
    let _safe_guard = EnvGuard::set("SAFE_PASSTHRU_TEST", "ok");
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
            "printf '%s|%s|%s|%s|%s|%s|%s' \"${OPENAI_API_KEY:-}\" \"${OPENAI_BASE_URL:-}\" \"${AWS_BEARER_TOKEN_BEDROCK:-}\" \"${GITHUB_TOKEN:-}\" \"${MODAL_TOKEN_SECRET:-}\" \"${GATEWAY_ALLOWED_USERS:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
            None,
            None,
            false,
            false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "||||||ok");
}

#[test]
fn test_execute_command_preserves_general_aws_credentials() {
    let _lock = lock_env();
    let _access_key = EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE");
    let _secret_key = EnvGuard::set("AWS_SECRET_ACCESS_KEY", "aws-secret");
    let _session = EnvGuard::set("AWS_SESSION_TOKEN", "aws-session");
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
            "printf '%s|%s|%s' \"${AWS_ACCESS_KEY_ID:-}\" \"${AWS_SECRET_ACCESS_KEY:-}\" \"${AWS_SESSION_TOKEN:-}\"",
            None,
            None,
            false,
            false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "AKIAIOSFODNN7EXAMPLE|aws-secret|aws-session");
    for var in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_DEFAULT_REGION",
        "AWS_REGION",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_ROLE_ARN",
    ] {
        assert!(
            !should_strip_subprocess_env(var),
            "{var} must not be in the Hermes subprocess blocklist"
        );
    }
}

#[test]
fn test_execute_command_force_prefix_reinjects_blocked_var() {
    let _lock = lock_env();
    let _blocked = EnvGuard::set("OPENAI_API_KEY", "sk-should-not-leak");
    let _forced = EnvGuard::set("_HERMES_FORCE_OPENAI_API_KEY", "sk-explicit");
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
        "printf '%s|%s' \"${OPENAI_API_KEY:-}\" \"${_HERMES_FORCE_OPENAI_API_KEY:-}\"",
        None,
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "sk-explicit|");
}

// Shell init files are sourced only on Unix; see `with_login_profile_sources`.
#[cfg(unix)]
#[test]
fn test_execute_command_cleans_profile_reintroduced_blocked_vars() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    let profile = td.path().join(".profile");
    std::fs::write(
        &profile,
        "export OPENAI_API_KEY=from-profile\nexport SAFE_PASSTHRU_TEST=ok\n",
    )
    .unwrap();
    let backend = LocalBackend::new_with_shell_init(
        10,
        1_048_576,
        vec![profile.to_string_lossy().to_string()],
        false,
        Vec::new(),
    );

    let output = block_on(backend.execute_command(
        "printf '%s|%s' \"${OPENAI_API_KEY:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
        None,
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "|ok");
}

// PATH normalisation runs only on Unix; Windows keeps the inherited PATH.
#[cfg(unix)]
#[test]
fn test_subprocess_path_appends_homebrew_when_path_is_minimal() {
    let normalized = normalize_subprocess_path(Some("/some/custom/bin"));
    assert!(normalized.contains("/some/custom/bin"));
    assert!(normalized.contains("/usr/bin"));
    assert!(normalized.contains("/opt/homebrew/bin"));
    assert!(normalized.contains("/opt/homebrew/sbin"));
}

// PATH normalisation runs only on Unix; Windows keeps the inherited PATH.
#[cfg(unix)]
#[test]
fn test_subprocess_path_preserves_full_path() {
    assert_eq!(
        normalize_subprocess_path(Some("/usr/bin:/bin")),
        "/usr/bin:/bin"
    );
}

#[cfg(windows)]
#[test]
fn test_windows_command_finds_programs_on_the_inherited_path() {
    let _lock = lock_env();
    let bin = tempdir().unwrap();
    std::fs::write(
        bin.path().join("hermes-path-probe.bat"),
        "@echo path-probe-ok\r\n",
    )
    .unwrap();
    // Last, not first: an MSYS shell reads the first entry of a `:`-joined
    // Windows PATH correctly and loses the drive of every later one.
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::env::split_paths(&inherited).chain(std::iter::once(bin.path().to_path_buf())),
    )
    .unwrap();
    let _path = EnvGuard::set("PATH", path.to_string_lossy().as_ref());

    let output = block_on(LocalBackend::default().execute_command(
        "hermes-path-probe.bat",
        None,
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0, "stderr={}", output.stderr);
    assert!(output.stdout.contains("path-probe-ok"));
}

#[tokio::test]
async fn test_background_process_lifecycle_with_stdin() {
    let backend = LocalBackend::new(10, 1_048_576);
    let started = backend
        .execute_command("cat", None, None, true, false)
        .await
        .unwrap();
    assert_eq!(started.exit_code, 0);
    let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .expect("session_id")
        .to_string();

    let write = backend
        .write_process_stdin(&session_id, "hello from stdin\nsecond line\nthird line\n")
        .await
        .unwrap();
    assert_eq!(write["status"], "ok");

    let tail = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let log = backend
                .read_process_log(&session_id, None, Some(2))
                .await
                .unwrap();
            if log["total_lines"] == 3 && log["output"] == "second line\nthird line" {
                break log;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background process did not publish its log");
    assert_eq!(tail["status"], "running");
    assert_eq!(tail["output"], "second line\nthird line");
    assert_eq!(tail["showing"], "2 lines");
    let page = backend
        .read_process_log(&session_id, Some(1), Some(1))
        .await
        .unwrap();
    assert_eq!(page["output"], "second line");

    let list = backend.list_processes().await.unwrap();
    assert_eq!(list["count"], 1);
    let summary = &list["sessions"][0];
    assert_eq!(summary["session_id"], session_id);
    assert_eq!(summary["command"], "cat");
    assert_eq!(summary["status"], "running");
    assert!(summary["output_preview"].as_str().unwrap().lines().eq([
        "hello from stdin",
        "second line",
        "third line"
    ]));

    let before_close = backend.wait_process(&session_id, Some(0)).await.unwrap();
    assert_eq!(before_close["status"], "timeout");
    assert_eq!(
        before_close["timeout_note"],
        "Waited 0s, process still running"
    );

    let close = backend.close_process_stdin(&session_id).await.unwrap();
    assert_eq!(close["status"], "ok");

    let wait = backend.wait_process(&session_id, Some(20)).await.unwrap();
    if wait["status"] == "timeout" {
        let poll = backend.poll_process(&session_id).await.unwrap();
        let _ = backend.kill_process(&session_id).await;
        panic!("background process did not exit after closing stdin: wait={wait}, poll={poll}");
    }
    assert_eq!(wait["status"], "exited");
    assert_eq!(wait["exit_code"], 0);
    assert_eq!(
        wait["timeout_note"],
        "Requested wait of 20s was clamped to configured limit of 10s"
    );
    assert!(wait["output"]
        .as_str()
        .unwrap_or_default()
        .contains("hello from stdin"));
    let finished = backend.poll_process(&session_id).await.unwrap();
    assert_eq!(finished["status"], "exited");
    assert_eq!(finished["exit_code"], 0);
}

#[tokio::test]
async fn test_background_process_not_found_contract() {
    let backend = LocalBackend::default();
    let poll = backend.poll_process("proc_missing").await.unwrap();
    assert_eq!(poll["status"], "not_found");

    let log = backend
        .read_process_log("proc_missing", None, None)
        .await
        .unwrap();
    assert_eq!(log["status"], "not_found");

    let kill = backend.kill_process("proc_missing").await.unwrap();
    assert_eq!(kill["status"], "not_found");

    let submit = backend
        .submit_process_stdin("proc_missing", "synthetic input")
        .await
        .unwrap();
    assert_eq!(submit["status"], "not_found");
    let close = backend.close_process_stdin("proc_missing").await.unwrap();
    assert_eq!(close["status"], "not_found");
}

#[tokio::test]
async fn process_readers_keep_output_and_report_a_read_error() {
    struct FailingReader;

    impl AsyncRead for FailingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("fixture read failed")))
        }
    }

    let stream = || (&b"received data"[..]).chain(FailingReader);
    let text = Arc::new(Mutex::new(String::new()));
    let bytes = Arc::new(Mutex::new(Vec::new()));
    LocalBackend::read_stream_to_buffer(stream(), text.clone(), 128).await;
    LocalBackend::read_stream_to_bytes(stream(), bytes.clone(), 128).await;

    let expected = "received data\n[stream read error: fixture read failed]";
    assert_eq!(*text.lock().unwrap(), expected);
    assert_eq!(LocalBackend::decode_output(&bytes), expected);
}

#[tokio::test]
async fn background_output_keeps_the_tail_on_a_utf8_boundary() {
    let output = Arc::new(Mutex::new(String::new()));
    LocalBackend::read_stream_to_buffer("dropé留".as_bytes(), output.clone(), 4).await;

    assert_eq!(*output.lock().unwrap(), "留");
}

/// A value no UTF-8 string can hold: a lone 0xFF byte on Unix, a lone
/// surrogate on Windows.
fn not_utf8(prefix: &str) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = prefix.as_bytes().to_vec();
        bytes.push(0xff);
        std::ffi::OsString::from_vec(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut wide: Vec<u16> = prefix.encode_utf16().collect();
        wide.push(0xD800);
        std::ffi::OsString::from_wide(&wide)
    }
}

fn child_test_command(
    test: &str,
    envs: &[(std::ffi::OsString, std::ffi::OsString)],
) -> std::process::Command {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--test-threads=1"])
        .env("REFORMED_TEST_CHILD", "1")
        .envs(envs.iter().map(|(k, v)| (k, v)));
    cmd
}

fn child_passed(mut cmd: std::process::Command) -> bool {
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let ok = output.status.success() && stdout.contains("1 passed");
    if !ok {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "child test failed ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status
        );
    }
    ok
}

/// Runs `test` (its full path) in a child copy of this test binary with
/// `envs` added, so a variable that is not UTF-8 never enters this process,
/// whose other tests read the environment. True when the child ran it and it
/// passed.
fn passes_in_child(test: &str, envs: &[(std::ffi::OsString, std::ffi::OsString)]) -> bool {
    child_passed(child_test_command(test, envs))
}

#[test]
fn scrubbing_survives_a_variable_that_is_not_utf8() {
    if std::env::var_os("REFORMED_TEST_CHILD").is_some() {
        let mut cmd = TokioCommand::new("true");
        scrub_subprocess_env(&mut cmd, &[]);
        let removed: Vec<String> = cmd
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        assert!(
            removed.iter().any(|key| key == "OPENAI_API_KEY"),
            "{removed:?}"
        );
        return;
    }
    assert!(passes_in_child(
        "local::tests::scrubbing_survives_a_variable_that_is_not_utf8",
        &[
            ("OPENAI_API_KEY".into(), "sk-test".into()),
            ("REFORMED_TEST_BAD_VALUE".into(), not_utf8("x")),
            (not_utf8("REFORMED_TEST_BAD_NAME"), "x".into()),
        ],
    ));
}

/// A command whose inner shell, a grandchild of the backend's shell,
/// ignores SIGTERM, writes `<marker>.ready` once running and `marker` after
/// 4 seconds. The trailing `echo` stops the outer shell from replacing
/// itself with the inner one.
fn survivor_command(marker: &Path) -> String {
    // Forward slashes, which both a native and an MSYS shell read.
    let marker = marker.to_string_lossy().replace('\\', "/");
    format!(
        "sh -c 'trap \"\" TERM; echo > \"{marker}.ready\"; sleep 4; echo survived > \"{marker}\"'; echo unreachable"
    )
}

async fn wait_until_ready(marker: &Path) {
    let ready = marker.with_extension("ready");
    for _ in 0..100 {
        if ready.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the inner shell never started: {}", ready.display());
}

async fn assert_no_survivor(marker: &Path) {
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    assert!(
        !marker.exists(),
        "a descendant of the ended shell kept running and wrote {}",
        marker.display()
    );
}

#[tokio::test]
async fn test_timeout_ends_the_shells_descendants() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("survivor");
    let backend = LocalBackend::new(60, 1_048_576);

    let result = backend
        .execute_command(&survivor_command(&marker), Some(2), None, false, false)
        .await;

    assert!(matches!(result, Err(AgentError::Timeout(_))), "{result:?}");
    assert!(
        marker.with_extension("ready").exists(),
        "the inner shell never started"
    );
    assert_no_survivor(&marker).await;
}

#[tokio::test]
async fn test_cancelled_command_ends_the_shells_descendants() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("survivor");
    let backend = Arc::new(LocalBackend::new(60, 1_048_576));
    let command = survivor_command(&marker);
    let task = {
        let backend = backend.clone();
        tokio::spawn(async move {
            backend
                .execute_command(&command, Some(60), None, false, false)
                .await
        })
    };

    wait_until_ready(&marker).await;
    task.abort();
    let _ = task.await;
    assert_no_survivor(&marker).await;
}

#[tokio::test]
async fn test_kill_process_ends_the_shells_descendants() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("survivor");
    let backend = LocalBackend::new(60, 1_048_576);
    let started = backend
        .execute_command(&survivor_command(&marker), None, None, true, false)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
    let session_id = payload["session_id"].as_str().expect("session_id");

    wait_until_ready(&marker).await;
    let killed = backend.kill_process(session_id).await.unwrap();

    assert_eq!(killed["status"], "killed", "{killed}");
    assert_no_survivor(&marker).await;
}

/// A caller that gives up on `kill_process` midway still ends the tree.
#[tokio::test]
async fn test_abandoned_kill_process_still_ends_the_shells_descendants() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("survivor");
    let backend = LocalBackend::new(60, 1_048_576);
    let started = backend
        .execute_command(&survivor_command(&marker), None, None, true, false)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
    let session_id = payload["session_id"].as_str().expect("session_id");

    wait_until_ready(&marker).await;
    let abandoned = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        backend.kill_process(session_id),
    )
    .await;

    // Windows ends the job at once; Unix is still in its grace period.
    if cfg!(unix) {
        assert!(abandoned.is_err(), "kill_process finished within 50 ms");
    }
    assert_no_survivor(&marker).await;
}

/// When the exit cannot be seen unreaped, the shell is still reaped only
/// whilst nobody holds the signal lock.
#[cfg(unix)]
#[tokio::test]
async fn test_an_unseen_exit_is_reaped_only_outside_a_held_signal_lock() {
    let mut child = TokioCommand::new("sh")
        .arg("-c")
        .arg("sleep 0.2")
        .spawn()
        .unwrap();
    let pid = child.id().expect("pid") as libc::pid_t;
    let lock = Arc::new(AsyncMutex::new(()));
    let held = lock.clone().lock_owned().await;
    let reaper = tokio::spawn(async move {
        let guard = reap_under_lock(&lock, || child.try_wait()).await;
        (guard.is_some(), child)
    });

    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    // SAFETY: signal 0 only checks that the process id exists.
    let exists = unsafe { libc::kill(pid, 0) } == 0;
    drop(held);
    let (guarded, mut child) = reaper.await.unwrap();

    assert!(
        exists,
        "the child was reaped whilst the signal lock was held"
    );
    assert!(guarded, "the reap returned without the lock");
    assert!(child.try_wait().unwrap().is_some());
}

/// A failed check gives the lock back: the child's state is unknown, and the
/// waiter's blocking reap must not hold up `kill_process`.
#[cfg(unix)]
#[tokio::test]
async fn test_a_failed_reap_check_releases_the_signal_lock() {
    let lock = Arc::new(AsyncMutex::new(()));

    let guard = reap_under_lock(&lock, || {
        Err(std::io::Error::from_raw_os_error(libc::ECHILD))
    })
    .await;

    assert!(
        guard.is_none(),
        "the reap kept the lock after a failed check"
    );
    assert!(lock.try_lock().is_ok(), "the signal lock is still held");
}

/// The shell stays unreaped whilst `kill_process` signals its group, so the
/// group id cannot pass to an unrelated process meanwhile.
#[cfg(unix)]
#[tokio::test]
async fn test_kill_process_keeps_the_shell_unreaped_until_it_has_signalled() {
    let backend = std::sync::Arc::new(LocalBackend::new(60, 1_048_576));
    let started = backend
        .execute_command("sleep 30; echo unreachable", None, None, true, false)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
    let session_id = payload["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();
    let pid = payload["pid"].as_i64().expect("pid") as libc::pid_t;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let killer = {
        let backend = backend.clone();
        tokio::spawn(async move { backend.kill_process(&session_id).await })
    };
    // Well inside the grace period; the shell ends on SIGTERM.
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    // SAFETY: signal 0 only checks that the process id exists.
    let exists = unsafe { libc::kill(pid, 0) } == 0;

    let killed = killer.await.unwrap().unwrap();
    assert_eq!(killed["status"], "killed", "{killed}");
    assert!(exists, "the shell was reaped before the group was killed");
}

/// A group whose only member is the exited, still unreaped shell can be
/// signalled, as `kill_process` does after the shell ends on SIGTERM.
#[cfg(unix)]
#[tokio::test]
async fn test_a_group_left_with_only_its_unreaped_shell_can_be_signalled() {
    let (mut child, tree) =
        process_tree::spawn(TokioCommand::new("sh").arg("-c").arg("exit 0")).unwrap();
    let pid = child.id().expect("pid");
    assert!(
        process_tree::wait_unreaped(pid).await,
        "the shell's exit was not seen"
    );

    let terminated = tree.terminate();
    let killed = tree.kill();
    child.wait().await.unwrap();

    assert!(terminated.is_ok(), "terminate: {terminated:?}");
    assert!(killed.is_ok(), "kill: {killed:?}");
}

/// macOS answers EPERM both for a group of zombies and for one whose live
/// members refused the signal; only the first counts as signalled.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_a_live_member_is_not_taken_for_a_zombie() {
    let (mut child, tree) =
        process_tree::spawn(TokioCommand::new("sh").arg("-c").arg("sleep 30 & exit 0")).unwrap();
    let pid = child.id().expect("pid");
    assert!(
        process_tree::wait_unreaped(pid).await,
        "the shell's exit was not seen"
    );

    let with_live_member = process_tree::group_has_exited(pid as libc::pid_t);
    tree.kill().unwrap();
    let mut after_kill = false;
    for _ in 0..50 {
        after_kill = process_tree::group_has_exited(pid as libc::pid_t);
        if after_kill {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    child.wait().await.unwrap();

    assert!(
        !with_live_member,
        "a live member was taken for one that has ended"
    );
    assert!(after_kill, "the group was not seen as ended");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct NobodySleepGuard {
    pattern: String,
    active: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl NobodySleepGuard {
    fn disarm(&mut self) {
        self.active = false;
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for NobodySleepGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = std::process::Command::new("sudo")
                .args([
                    "-n",
                    "-u",
                    "nobody",
                    "pkill",
                    "-u",
                    "nobody",
                    "-f",
                    &self.pattern,
                ])
                .status();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn kill_process_with_live_refusing_member() {
    let required = std::env::var_os("REFORMED_TEST_REQUIRE_LIVE_REFUSING_MEMBER").is_some();
    let skip_or_fail = |reason: &str| {
        if required {
            panic!("REFORMED_TEST_REQUIRE_LIVE_REFUSING_MEMBER set: {reason}");
        }
        eprintln!("skipped: {reason}");
    };
    let probe = tokio::process::Command::new("sudo")
        .args([
            "-n",
            "-u",
            "nobody",
            "sh",
            "-c",
            "sleep 5 </dev/null >/dev/null 2>&1 & printf '%s\\n' \"$!\"",
        ])
        .output()
        .await;
    let Some(probe) = probe.ok().filter(|output| output.status.success()) else {
        skip_or_fail("passwordless sudo to nobody is not available");
        return;
    };
    let Ok(probe_pid) = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse::<libc::pid_t>()
    else {
        skip_or_fail("sudo nobody probe did not report a pid");
        return;
    };
    // SAFETY: signal 0 checks permission without delivering a signal.
    let can_signal_nobody = unsafe { libc::kill(probe_pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM);
    let _ = tokio::process::Command::new("sudo")
        .args(["-n", "-u", "nobody", "kill", "-9", &probe_pid.to_string()])
        .status()
        .await;
    if can_signal_nobody {
        skip_or_fail("caller can signal processes owned by nobody");
        return;
    }
    let dir = tempdir().unwrap();
    let marker = dir.path().join("member");
    let shell_marker = marker.to_string_lossy().replace('\\', "/");
    let sleep_secs = format!("31.{:06}", 100_000 + (std::process::id() % 900_000));
    let pattern = format!("sleep {sleep_secs}");
    // Fork `sleep` from a short-lived `nobody` subshell so `sudo` (whose real
    // uid is the caller's and would forward SIGTERM) exits before the kill,
    // leaving an unkillable `nobody` child in the shell's process group.
    let command = format!(
        "sudo -n -u nobody sh -c '{pattern} </dev/null >/dev/null 2>&1 &' && {{ echo > \"{shell_marker}.ready\"; sleep 30 & wait; }}"
    );
    let backend = LocalBackend::new(60, 1_048_576);
    let mut guard = NobodySleepGuard {
        pattern: pattern.clone(),
        active: true,
    };
    let started = backend
        .execute_command(&command, None, None, true, false)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
    let session_id = payload["session_id"].as_str().expect("session_id");
    wait_until_ready(&marker).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let killed = backend.kill_process(session_id).await;
    let cleanup = tokio::process::Command::new("sudo")
        .args([
            "-n", "-u", "nobody", "pkill", "-u", "nobody", "-f", &pattern,
        ])
        .status()
        .await;
    if cleanup.as_ref().is_ok_and(|status| status.success()) {
        guard.disarm();
    }

    assert!(
        !matches!(&killed, Ok(value) if value["status"] == "killed"),
        "a group with a live member was reported killed: {killed:?}"
    );
    assert!(
        cleanup.as_ref().is_ok_and(|status| status.success()),
        "cleanup failed while sleeper was live: {cleanup:?}"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_passed_in_setsid_child(test_name: &str) {
    use std::os::unix::process::CommandExt;

    let mut cmd = child_test_command(test_name, &[]);
    // SAFETY: `setsid` is async-signal-safe and detaches `/dev/tty` before
    // `exec` so `sudo` `use_pty` never moves the `nobody` sleeper to a new
    // session and process group.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    assert!(child_passed(cmd));
}

/// A group member that refuses the signal (here one running as another
/// user) leaves the group alive, so `kill_process` must not report it
/// killed. Runs in a `setsid()` child so `/dev/tty` is detached even when
/// `cargo test` runs from a terminal (avoiding `sudo` `use_pty`). Needs
/// passwordless `sudo -u nobody` that refuses signals from this caller;
/// skipped without it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_kill_process_does_not_report_a_group_with_a_live_refusing_member_killed() {
    if std::env::var_os("REFORMED_TEST_CHILD").is_some() {
        block_on(kill_process_with_live_refusing_member());
        return;
    }
    assert_passed_in_setsid_child(
        "local::tests::test_kill_process_does_not_report_a_group_with_a_live_refusing_member_killed",
    );
}

#[cfg(target_os = "linux")]
struct ChildKillGuard(Option<std::process::Child>);

#[cfg(target_os = "linux")]
impl Drop for ChildKillGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// An unrelated Linux process whose `/proc/<pid>/stat` `comm` holds
/// non-UTF-8 bytes must not fail closed (blocking clean exits) or abort
/// `/proc` scanning before detecting a later live refusing member.
#[cfg(target_os = "linux")]
#[test]
fn test_kill_process_ignores_unrelated_process_with_non_utf8_comm() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    if std::env::var_os("REFORMED_TEST_NON_UTF8_COMM_CHILD").is_some() {
        if let Some(ready) = std::env::var_os("REFORMED_TEST_READY") {
            let _ = std::fs::write(ready, b"ready");
        }
        std::thread::sleep(std::time::Duration::from_secs(15));
        return;
    }
    if std::env::var_os("REFORMED_TEST_CHILD").is_none() {
        assert_passed_in_setsid_child(
            "local::tests::test_kill_process_ignores_unrelated_process_with_non_utf8_comm",
        );
        return;
    }

    let dir = tempdir().unwrap();
    let ready = dir.path().join("comm.ready");
    let bad_link = dir.path().join(std::ffi::OsStr::from_bytes(b"bad\xffcomm"));
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &bad_link).unwrap();
    let mut cmd = std::process::Command::new(&bad_link);
    cmd.arg0("rust-test")
        .args([
            "--exact",
            "local::tests::test_kill_process_ignores_unrelated_process_with_non_utf8_comm",
            "--nocapture",
        ])
        .env("REFORMED_TEST_NON_UTF8_COMM_CHILD", "1")
        .env("REFORMED_TEST_READY", &ready)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _child = ChildKillGuard(Some(cmd.spawn().unwrap()));
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(ready.exists(), "non-utf8 comm child did not become ready");

    let killed = block_on(async {
        let backend = LocalBackend::new(60, 1_048_576);
        let started = backend
            .execute_command("sleep 30", None, None, true, false)
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
        let session_id = payload["session_id"].as_str().expect("session_id");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        backend.kill_process(session_id).await
    });
    assert!(
        matches!(&killed, Ok(value) if value["status"] == "killed"),
        "unrelated non-utf8 comm caused kill_process failure: {killed:?}"
    );

    // Spawned after `_child`, so `/proc` visits `bad\xffcomm` before the
    // `nobody` sleeper when `sudo -u nobody` is available.
    block_on(kill_process_with_live_refusing_member());
}

#[tokio::test]
async fn test_finished_command_leaves_its_background_child_running() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("alive");
    let shell_marker = marker.to_string_lossy().replace('\\', "/");
    let command = format!("sh -c 'sleep 1; echo alive > \"{shell_marker}\"' & echo started");
    let backend = LocalBackend::new(30, 1_048_576);

    let output = backend
        .execute_command(&command, None, None, false, false)
        .await
        .unwrap();

    assert_eq!(output.exit_code, 0, "stderr={}", output.stderr);
    for _ in 0..100 {
        if marker.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the background child of a finished command was ended");
}

/// CreateProcess refuses a command line over 32,767 UTF-16 units. Linux
/// caps one argument at 128 KiB, which the Unix wrapper reaches sooner: it
/// repeats the command for each login shell it may run.
#[cfg(windows)]
#[tokio::test]
async fn test_commands_longer_than_a_windows_command_line_run() {
    let command = format!("x={}; read -r line; echo ${{#x}} $line", "a".repeat(40_000));
    let backend = LocalBackend::new(30, 1_048_576);

    let output = backend
        .execute_command_with_stdin(&command, None, None, false, false, Some("piped\n"))
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0, "stderr={}", output.stderr);
    assert_eq!(output.stdout.trim(), "40000 piped");

    let output = backend
        .execute_command(&command, None, None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0, "stderr={}", output.stderr);
    assert_eq!(output.stdout.trim(), "40000");

    let started = backend
        .execute_command(&command, None, None, true, false)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
    let session_id = payload["session_id"].as_str().expect("session_id");
    backend.close_process_stdin(session_id).await.unwrap();
    let wait = backend.wait_process(session_id, Some(20)).await.unwrap();
    assert_eq!(wait["status"], "exited", "{wait}");
    assert_eq!(wait["exit_code"], 0, "{wait}");
    assert_eq!(
        wait["output"].as_str().map(str::trim),
        Some("40000"),
        "{wait}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_file_refuses_leaf_symlink() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.txt");
    let link = dir.path().join("alias.txt");
    std::fs::write(&target, "untouched").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let backend = LocalBackend::default();
    let link_str = link.to_str().unwrap();
    let err = backend.write_file(link_str, "overwrite").await.unwrap_err();
    assert!(err.to_string().contains("symlink"), "{err}");
    let err_tool = backend
        .write_file_for_tool(link_str, "overwrite")
        .await
        .unwrap_err();
    assert!(err_tool.to_string().contains("symlink"), "{err_tool}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
}
