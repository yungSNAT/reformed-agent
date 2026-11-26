use super::{handle_cli_memory, handle_cli_plugins, MemorySetupCliOptions};
use hermes_core::AgentError;
use std::{env, fs, path::Path, process::Command};

const CASE_ENV: &str = "HERMES_PLUGINS_MEMORY_COVERAGE_CASE";
const MEMORY_TEMPLATE: &str = "# Hermes MEMORY\n\nStore durable assistant memory entries here.\n";
const USER_TEMPLATE: &str = "# Hermes USER\n\nStore durable user profile entries here.\n";
const PLUGIN_MANIFEST: &str =
    "name: coverage-toggle\nversion: '1.2.3'\nkind: tool\ndescription: Coverage fixture\n";

fn run_isolated(case: &str) -> String {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let user_home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir(&user_home).unwrap();
    fs::create_dir(&bin).unwrap();

    let (_, module) = module_path!().split_once("::").unwrap();
    let mut command = Command::new(env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
    command
        .args([
            "--exact",
            &format!("{module}::isolated_worker"),
            "--nocapture",
        ])
        .current_dir(root)
        .env_clear()
        .env(CASE_ENV, case)
        .env("HOME", &user_home)
        .env("USERPROFILE", &user_home)
        .env("HERMES_HOME", user_home.join(".hermes"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("APPDATA", root.join("appdata"))
        .env("LOCALAPPDATA", root.join("localappdata"))
        .env("PATH", bin)
        .env("HERMES_COVERAGE_GIT_LOG", root.join("git.log"));

    // Keep subprocess coverage in the same collection as the parent test.
    if let Some(profile) = env::var_os("LLVM_PROFILE_FILE") {
        command.env(
            "LLVM_PROFILE_FILE",
            env::current_dir().unwrap().join(profile),
        );
    }
    for key in ["SystemRoot", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }

    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{case}: {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains(&format!("coverage worker completed: {case}")));
    stdout
}

fn assert_output(stdout: &str, expected: &[&str]) {
    for fragment in expected {
        assert!(
            stdout.contains(fragment),
            "missing {fragment:?} in output:\n{stdout}"
        );
    }
}

async fn plugins(action: &str, name: Option<&str>) -> Result<(), AgentError> {
    handle_cli_plugins(Some(action.into()), name.map(str::to_owned), None, false).await
}

async fn memory(
    action: &str,
    target: Option<&str>,
    options: MemorySetupCliOptions,
) -> Result<(), AgentError> {
    handle_cli_memory(Some(action.into()), target.map(str::to_owned), options).await
}

fn confirmed() -> MemorySetupCliOptions {
    MemorySetupCliOptions {
        yes: true,
        ..MemorySetupCliOptions::default()
    }
}

fn assert_config_error(result: Result<(), AgentError>, expected: &str) {
    match result {
        Err(AgentError::Config(message)) => assert!(message.contains(expected), "{message}"),
        other => panic!("expected configuration error containing {expected:?}, got {other:?}"),
    }
}

fn assert_io_error(result: Result<(), AgentError>, expected: &str) {
    match result {
        Err(AgentError::Io(message)) => assert!(message.contains(expected), "{message}"),
        other => panic!("expected I/O error containing {expected:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn isolated_worker() {
    let Ok(case) = env::var(CASE_ENV) else {
        return;
    };
    let root = env::current_dir().unwrap();
    let home = hermes_config::hermes_home();
    assert_eq!(home.canonicalize().ok(), None);
    assert_eq!(
        home,
        std::path::PathBuf::from(env::var_os("HERMES_HOME").unwrap())
    );

    match case.as_str() {
        "plugins-local" => exercise_local_plugins(&home).await,
        "plugins-update-local" => exercise_local_updates(&home).await,
        "memory-state" => exercise_memory_state(&home).await,
        "memory-errors" => exercise_memory_errors(&home).await,
        #[cfg(unix)]
        "plugins-install-git" => exercise_git_installs(&root, &home).await,
        #[cfg(unix)]
        "plugins-update-git" => exercise_git_updates(&root, &home).await,
        other => panic!("unexpected coverage case: {other}"),
    }
    assert_eq!(env::current_dir().unwrap(), root);
    println!("coverage worker completed: {case}");
}

#[test]
fn local_plugin_actions_report_results_and_change_markers() {
    let stdout = run_isolated("plugins-local");
    assert_output(
        &stdout,
        &[
            "Plugin surface (",
            "Plugin 'coverage-toggle' disabled.",
            "Status: disabled",
            "--- plugin.yaml ---",
            PLUGIN_MANIFEST.trim_end(),
            "Plugin 'coverage-toggle' enabled.",
            "Status: enabled",
            "Plugin 'coverage-toggle' is already enabled (or not installed).",
            "Plugin 'coverage-toggle' removed.",
            "Plugin 'coverage-missing' not found.",
            "Plugin 'coverage-bare' has no plugin.yaml manifest.",
            "Plugins action 'coverage-unknown' is not recognized.",
        ],
    );
}

async fn exercise_local_plugins(home: &Path) {
    for (action, usage) in [
        ("enable", "enable <name>"),
        ("disable", "disable <name>"),
        ("install", "install <name|url>"),
        ("remove", "remove <name>"),
        ("uninstall", "remove <name>"),
        ("inspect", "inspect <name>"),
        ("info", "inspect <name>"),
    ] {
        assert_config_error(plugins(action, None).await, usage);
    }

    let plugin_root = home.join("plugins");
    let plugin = plugin_root.join("coverage-toggle");
    let marker = plugin.join(".disabled");
    let manifest = plugin.join("plugin.yaml");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(&manifest, PLUGIN_MANIFEST).unwrap();

    plugins("list", None).await.unwrap();
    plugins("disable", Some("coverage-missing")).await.unwrap();
    assert!(!plugin_root.join("coverage-missing").exists());

    plugins("disable", Some("coverage-toggle")).await.unwrap();
    assert_eq!(fs::read(&marker).unwrap(), b"");
    plugins("inspect", Some("coverage-toggle")).await.unwrap();

    plugins("enable", Some("coverage-toggle")).await.unwrap();
    assert!(!marker.exists());
    plugins("enable", Some("coverage-toggle")).await.unwrap();
    plugins("info", Some("coverage-toggle")).await.unwrap();
    assert_eq!(fs::read_to_string(&manifest).unwrap(), PLUGIN_MANIFEST);

    fs::create_dir(&marker).unwrap();
    assert_io_error(
        plugins("enable", Some("coverage-toggle")).await,
        "Failed to enable plugin:",
    );
    assert_io_error(
        plugins("disable", Some("coverage-toggle")).await,
        "Failed to disable plugin:",
    );
    assert!(marker.is_dir());

    fs::write(&manifest, [0xff]).unwrap();
    assert_io_error(plugins("inspect", Some("coverage-toggle")).await, "");
    plugins("uninstall", Some("coverage-toggle")).await.unwrap();
    assert!(!plugin.exists());

    let bare = plugin_root.join("coverage-bare");
    fs::create_dir(&bare).unwrap();
    plugins("inspect", Some("coverage-bare")).await.unwrap();
    plugins("info", Some("coverage-missing")).await.unwrap();
    plugins("remove", Some("coverage-missing")).await.unwrap();

    let file = plugin_root.join("coverage-file");
    fs::write(&file, "not a directory").unwrap();
    assert_io_error(
        plugins("remove", Some("coverage-file")).await,
        "Failed to remove plugin:",
    );
    plugins("coverage-unknown", None).await.unwrap();
    assert_eq!(fs::read_to_string(file).unwrap(), "not a directory");
}

#[test]
fn local_update_skips_files_and_non_git_plugins() {
    let stdout = run_isolated("plugins-update-local");
    assert_output(
        &stdout,
        &[
            "No plugins installed.",
            "No plugins to update.",
            "Plugin 'coverage-missing' not found.",
            "Checking updates for 'coverage-local'...",
            "Skipped: plugin is not a git checkout.",
            "Checked 1 plugin(s); updated 0.",
        ],
    );
    assert!(!stdout.contains("Checking updates for 'coverage-unselected'"));
}

async fn exercise_local_updates(home: &Path) {
    let plugin_root = home.join("plugins");
    plugins("update", None).await.unwrap();
    assert!(!plugin_root.exists());

    fs::create_dir_all(plugin_root.join("coverage-no-manifest")).unwrap();
    fs::write(plugin_root.join("coverage-file"), "not a plugin").unwrap();
    plugins("update", None).await.unwrap();

    let local = plugin_root.join("coverage-local");
    let unselected = plugin_root.join("coverage-unselected");
    for path in [&local, &unselected] {
        fs::create_dir(path).unwrap();
        fs::write(path.join("plugin.yaml"), PLUGIN_MANIFEST).unwrap();
    }
    plugins("update", Some("coverage-missing")).await.unwrap();
    plugins("update", Some("coverage-local")).await.unwrap();
    assert_eq!(
        fs::read_to_string(local.join("plugin.yaml")).unwrap(),
        PLUGIN_MANIFEST
    );
}

#[test]
fn memory_setup_off_and_scoped_resets_preserve_the_right_files() {
    let stdout = run_isolated("memory-state");
    assert_output(
        &stdout,
        &[
            "Memory provider disabled.",
            "Initialized file memory backend.",
            "Memory reset complete (target=memory). MEMORY.md=present USER.md=absent",
            "Memory reset complete (target=user). MEMORY.md=absent USER.md=present",
            "Memory reset complete (target=all). MEMORY.md=present USER.md=present",
            "Unknown memory action 'coverage-unknown'.",
        ],
    );
}

async fn exercise_memory_state(home: &Path) {
    let memories = home.join("memories");
    let memory_md = memories.join("MEMORY.md");
    let user_md = memories.join("USER.md");
    let marker = home.join(".memory_disabled");
    let legacy = home.join("memory.db");

    memory("off", None, Default::default()).await.unwrap();
    let marker_text = fs::read_to_string(&marker).unwrap();
    let mut lines = marker_text.lines();
    let timestamp = lines.next().unwrap().strip_prefix("disabled_at=").unwrap();
    assert!(!timestamp.is_empty());
    assert_eq!(lines.next(), Some("reason=hermes memory off"));
    assert_eq!(lines.next(), None);
    fs::write(&legacy, "legacy data").unwrap();

    assert_config_error(
        memory("reset", None, Default::default()).await,
        "requires confirmation flag",
    );
    assert_config_error(
        memory("reset", Some("  INVALID  "), confirmed()).await,
        "Unknown memory reset target 'invalid'",
    );
    assert!(!memories.exists());
    assert_eq!(fs::read_to_string(&marker).unwrap(), marker_text);
    assert_eq!(fs::read_to_string(&legacy).unwrap(), "legacy data");

    memory("reset", Some("  MeMoRy  "), confirmed())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&memory_md).unwrap(), MEMORY_TEMPLATE);
    assert!(!user_md.exists());
    assert!(!marker.exists());
    assert!(legacy.exists());

    fs::remove_file(&memory_md).unwrap();
    memory("reset", Some(" UsEr "), confirmed()).await.unwrap();
    assert_eq!(fs::read_to_string(&user_md).unwrap(), USER_TEMPLATE);
    assert!(!memory_md.exists());
    assert!(legacy.exists());

    fs::write(&user_md, "custom user").unwrap();
    memory("off", None, Default::default()).await.unwrap();
    memory("setup", Some(" \t "), Default::default())
        .await
        .unwrap();
    assert!(!marker.exists());
    assert_eq!(fs::read_to_string(&memory_md).unwrap(), MEMORY_TEMPLATE);
    assert_eq!(fs::read_to_string(&user_md).unwrap(), "custom user");

    fs::write(&memory_md, "custom memory").unwrap();
    memory("setup", None, Default::default()).await.unwrap();
    assert_eq!(fs::read_to_string(&memory_md).unwrap(), "custom memory");
    assert_eq!(fs::read_to_string(&user_md).unwrap(), "custom user");

    memory("off", None, Default::default()).await.unwrap();
    memory("reset", Some(" "), confirmed()).await.unwrap();
    assert!(!legacy.exists());
    assert!(!marker.exists());
    memory("coverage-unknown", None, Default::default())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&memory_md).unwrap(), MEMORY_TEMPLATE);
    assert_eq!(fs::read_to_string(&user_md).unwrap(), USER_TEMPLATE);

    fs::remove_file(&user_md).unwrap();
    memory("setup", None, Default::default()).await.unwrap();
    assert_eq!(fs::read_to_string(user_md).unwrap(), USER_TEMPLATE);
}

include!("command_cli_plugins_memory_mcp_part2_coverage_gen.rs");
