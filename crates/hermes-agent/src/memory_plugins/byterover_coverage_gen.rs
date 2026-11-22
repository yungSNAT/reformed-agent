use super::*;

#[test]
fn coerces_integer_values_and_uses_the_default_for_unrecognized_values() {
    assert!(!coerce_bool(Some(&json!(0)), true));
    assert!(coerce_bool(Some(&json!(-2)), false));
    assert!(coerce_bool(Some(&json!(4)), false));
    assert!(coerce_bool(Some(&json!(1.5)), true));
    assert!(!coerce_bool(Some(&json!(u64::MAX)), false));
    assert!(coerce_bool(Some(&json!("automatic")), true));
    assert!(!coerce_bool(Some(&json!("automatic")), false));
}

#[test]
fn default_plugin_tracks_turns_without_an_initialized_workspace() {
    let plugin = ByteRoverPlugin::default();
    assert!(plugin.working_dir().is_empty());
    assert!(plugin.session_id.lock().unwrap().is_empty());
    assert!(plugin.auto_extract_enabled());
    assert_eq!(*plugin.turn_count.lock().unwrap(), 0);

    let message = "A query long enough to look up";
    assert!(plugin.prefetch(message, "test").is_empty());

    plugin.on_turn_start(7, message);
    plugin.queue_prefetch(message, "test");
    assert_eq!(*plugin.turn_count.lock().unwrap(), 7);

    plugin.sync_turn(message, "An answer", "test");
    plugin.on_memory_write("add", "user", "A preference");
    assert!(plugin
        .on_pre_compress(&[json!({"role": "user", "content": message})])
        .is_empty());

    plugin.shutdown();
    assert_eq!(*plugin.turn_count.lock().unwrap(), 8);
    assert!(plugin.working_dir().is_empty());
    assert!(plugin.auto_extract_enabled());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::{Duration, Instant};

    const CHILD_CASE: &str = "HERMES_BYTEROVER_COVERAGE_CHILD";
    const CHILD_DONE: &str = "ByteRover fixture assertions completed";
    const MISSING_CLI: &str = "brv CLI not found. Install: npm install -g byterover-cli";

    // Use only shell builtins. PATH in the child contains only the fixture directory.
    const FAKE_BRV: &str = r#"#!/bin/sh
case "$1" in
    query)
        [ "$#" -eq 3 ] && [ "$2" = "--" ] || exit 90
        printf '%s' "$3" > last-query
        case "$3" in
            "query fails")
                printf 'not the error\n'
                printf '  query rejected  \n' >&2
                exit 7
                ;;
            "short result") printf '  too short  \n' ;;
            "threshold result") printf '  12345678901234567890  \n' ;;
            *) printf '  %s  \n' "$HERMES_BYTEROVER_COVERAGE_RESULT" ;;
        esac
        ;;
    curate)
        [ "$#" -eq 3 ] && [ "$2" = "--" ] || exit 91
        case "$3" in
            "manual memory") exit 0 ;;
            "reject curation") printf '  curation rejected  \n' >&2; exit 8 ;;
            "User: "*) capture=turn ;;
            "[User profile] "*) capture=user ;;
            "[Agent memory] "*) capture=agent ;;
            "[Pre-compression context]"*) capture=compress ;;
            *) capture=unexpected ;;
        esac
        printf '%s' "$3" > "$capture"
        printf 'done' > "$capture.done"
        case "$capture" in
            turn|user) exit 7 ;;
        esac
        ;;
    status)
        [ "$#" -eq 1 ] || exit 92
        if [ -f fail-status ]; then
            printf '  unavailable  \n'
            exit 8
        fi
        printf '  local fixture  \n'
        printf 'ignore this on success\n' >&2
        ;;
    silent-failure) exit 9 ;;
    *) exit 93 ;;
esac
"#;

    fn text(path: &Path) -> &str {
        path.to_str().expect("UTF-8 fixture path")
    }

    fn install_fake_brv(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, FAKE_BRV).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn run_isolated(home: &Path, case: &str, primary: Option<&str>, legacy: &str) {
        let (_, module) = module_path!().split_once("::").unwrap();
        let worker = format!("{module}::isolated_worker");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", worker.as_str(), "--ignored", "--nocapture"])
            .current_dir(home)
            .env(CHILD_CASE, case)
            .env("HOME", home)
            .env("HERMES_HOME", home)
            .env("PATH", home.join("bin"))
            .env("HERMES_BYTEROVER_COVERAGE_RESULT", "界".repeat(8_001))
            .env("BYTEROVER_AUTO_EXTRACT", legacy);
        if let Some(primary) = primary {
            command.env("HERMES_BYTEROVER_AUTO_EXTRACT", primary);
        } else {
            command.env_remove("HERMES_BYTEROVER_AUTO_EXTRACT");
        }

        let output = command.output().expect("run isolated ByteRover tests");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(CHILD_DONE),
            "isolated case {case} failed ({}):\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn fake_cli_exercises_tools_prefetch_and_curation() {
        let home = tempfile::tempdir().unwrap();
        install_fake_brv(&home.path().join("bin/brv"));
        run_isolated(home.path(), "installed", Some("  OfF  "), "yes");
    }

    #[test]
    fn cli_resolves_from_the_isolated_home_when_path_lookup_fails() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("bin")).unwrap();
        install_fake_brv(&home.path().join(".brv-cli/bin/brv"));
        run_isolated(home.path(), "fallback", None, "no");
    }

    #[test]
    #[ignore = "invoked by tests that provide an isolated home and PATH"]
    fn isolated_worker() {
        let Ok(case) = std::env::var(CHILD_CASE) else {
            return;
        };
        let home = std::env::var_os("HERMES_HOME").expect("fixture home");
        let home = Path::new(&home);
        match case.as_str() {
            "installed" => {
                assert_eq!(
                    resolve_brv_path().as_deref(),
                    Some(text(&home.join("bin/brv")))
                );
                check_command_results(home);
                check_tools_and_prefetch(home);
                check_curation_hooks(home);
            }
            "fallback" => check_home_fallback(home),
            _ => panic!("unknown fixture case: {case}"),
        }
        println!("{CHILD_DONE}");
    }

    fn check_home_fallback(home: &Path) {
        let fake = home.join(".brv-cli/bin/brv");
        assert_eq!(resolve_brv_path().as_deref(), Some(text(&fake)));
        assert!(!load_auto_extract(text(home)));

        let cwd = home.join("fallback-workspace");
        assert_eq!(
            run_brv(&["status"], 15, text(&cwd)),
            (true, "local fixture".to_owned())
        );
        fs::remove_file(fake).unwrap();

        // Do not exercise the missing-CLI branch on hosts with a system fallback.
        if !Path::new("/usr/local/bin/brv").exists() {
            // Even if resolution unexpectedly finds a host executable, this
            // working directory cannot be entered and the executable cannot run.
            let blocked = home.join("not-a-directory");
            fs::write(&blocked, "sentinel").unwrap();
            assert_eq!(
                run_brv(&["status"], 15, text(&blocked.join("impossible"))),
                (false, MISSING_CLI.to_owned())
            );
            assert_eq!(fs::read_to_string(blocked).unwrap(), "sentinel");
        }
    }

    fn check_command_results(home: &Path) {
        let cwd = home.join("new").join("workspace");
        assert_eq!(
            run_brv(&["status"], 15, text(&cwd)),
            (true, "local fixture".to_owned())
        );
        assert!(cwd.is_dir());

        let (ok, error) = run_brv(&["silent-failure"], 15, text(&cwd));
        assert!(!ok);
        assert!(error.starts_with("brv exited "), "{error}");
        assert!(error.ends_with('9'), "{error}");

        let blocked = cwd.join("not-a-directory");
        fs::write(&blocked, "sentinel").unwrap();
        let (ok, error) = run_brv(&["status"], 15, text(&blocked));
        assert!(!ok);
        assert!(error.starts_with("brv execution failed: "), "{error}");
        assert_eq!(fs::read_to_string(blocked).unwrap(), "sentinel");
    }

    fn tool(plugin: &ByteRoverPlugin, name: &str, args: &Value) -> Value {
        serde_json::from_str(&plugin.handle_tool_call(name, args)).expect("tool JSON")
    }

    fn check_tools_and_prefetch(home: &Path) {
        let plugin = ByteRoverPlugin::new();
        assert_eq!(
            tool(&plugin, "brv_status", &json!({})),
            json!({"error": "ByteRover not initialized"})
        );

        fs::write(home.join("byterover.json"), r#"{"auto_extract":true}"#).unwrap();
        plugin.initialize("tools", text(home));
        assert!(!plugin.auto_extract_enabled());
        assert!(plugin.is_available());
        let prompt = plugin.system_prompt_block();
        assert!(prompt.starts_with("# ByteRover Memory\n"));
        for name in ["brv_query", "brv_curate", "brv_status"] {
            assert!(prompt.contains(name));
        }

        let cwd = home.join("byterover");
        let last_query = cwd.join("last-query");
        let truncated = "é".repeat(5_000);
        let long_query = format!("{truncated}discarded");
        assert_eq!(
            plugin.prefetch(&format!(" \n{long_query}\t "), "tools"),
            format!("## ByteRover Context\n{}", "界".repeat(8_001))
        );
        assert_eq!(fs::read_to_string(&last_query).unwrap(), truncated);
        assert!(plugin.prefetch("threshold result", "tools").is_empty());
        assert!(plugin.prefetch("query fails", "tools").is_empty());
        assert_eq!(fs::read_to_string(&last_query).unwrap(), "query fails");

        assert_eq!(
            tool(&plugin, "brv_query", &json!({"query": 42})),
            json!({"error": "query is required"})
        );
        assert_eq!(
            tool(&plugin, "brv_query", &json!({"query": "query fails"})),
            json!({"error": "query rejected"})
        );
        assert_eq!(
            tool(&plugin, "brv_query", &json!({"query": "short result"})),
            json!({"result": "No relevant memories found."})
        );
        assert_eq!(
            tool(&plugin, "brv_query", &json!({"query": "threshold result"})),
            json!({"result": "12345678901234567890"})
        );
        assert_eq!(
            tool(&plugin, "brv_query", &json!({"query": long_query})),
            json!({"result": "界".repeat(8_000)})
        );
        assert_eq!(fs::read_to_string(last_query).unwrap(), truncated);

        assert_eq!(
            tool(&plugin, "brv_curate", &json!({})),
            json!({"error": "content is required"})
        );
        assert_eq!(
            tool(&plugin, "brv_curate", &json!({"content": "manual memory"})),
            json!({"result": "Memory curated successfully."})
        );
        assert_eq!(
            tool(
                &plugin,
                "brv_curate",
                &json!({"content": "reject curation"})
            ),
            json!({"error": "curation rejected"})
        );
        assert_eq!(
            tool(&plugin, "brv_status", &json!({})),
            json!({"status": "local fixture"})
        );
        fs::write(cwd.join("fail-status"), "").unwrap();
        assert_eq!(
            tool(&plugin, "brv_status", &json!({})),
            json!({"error": "unavailable"})
        );
        assert_eq!(
            tool(&plugin, "brv_unknown", &json!({})),
            json!({"error": "Unknown tool: brv_unknown"})
        );
    }

    fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_capture(cwd: &Path, name: &str, expected: &str) {
        let done = cwd.join(format!("{name}.done"));
        wait_for(name, || done.is_file());
        assert_eq!(fs::read_to_string(cwd.join(name)).unwrap(), expected);
    }

    fn check_curation_hooks(home: &Path) {
        let log_path = home.join("hook-log");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(Mutex::new(fs::File::create(&log_path).unwrap()))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();

        let guard_home = home.join("guards");
        let plugin = ByteRoverPlugin::new();
        plugin.initialize("hooks", text(&guard_home));
        let skipped = guard_home.join("byterover");
        let message = "A message that would otherwise merit curation";
        let valid_messages = [json!({"role": "user", "content": message})];

        assert!(!plugin.auto_extract_enabled());
        plugin.queue_prefetch(message, "hooks");
        plugin.sync_turn(message, "An answer", "hooks");
        plugin.on_memory_write("add", "user", "Do not mirror this");
        assert!(plugin.on_pre_compress(&valid_messages).is_empty());
        assert_eq!(*plugin.turn_count.lock().unwrap(), 1);

        *plugin.auto_extract.lock().unwrap() = true;
        plugin.sync_turn("  short  ", "An answer", "hooks");
        plugin.on_memory_write("remove", "user", "Do not mirror a deletion");
        plugin.on_memory_write("replace", "agent", "");
        assert!(plugin.on_pre_compress(&[]).is_empty());
        assert!(plugin
            .on_pre_compress(&[json!({"role": "system", "content": "Ignore me"})])
            .is_empty());
        assert_eq!(*plugin.turn_count.lock().unwrap(), 2);

        let active = home.join("curation");
        *plugin.cwd.lock().unwrap() = text(&active).to_owned();

        let user = "u".repeat(1_999);
        let assistant = "a".repeat(1_998);
        plugin.sync_turn(&format!("{user}é!"), &format!("{assistant}界!"), "hooks");
        assert_eq!(*plugin.turn_count.lock().unwrap(), 3);
        assert_capture(
            &active,
            "turn",
            &format!("User: {user}\nAssistant: {assistant}"),
        );

        plugin.on_memory_write("add", "user", "Prefers concise answers");
        assert_capture(&active, "user", "[User profile] Prefers concise answers");
        plugin.on_memory_write("replace", "agent", "Keep decisions in the changelog");
        assert_capture(
            &active,
            "agent",
            "[Agent memory] Keep decisions in the changelog",
        );

        let kept = "界".repeat(500);
        let mut messages = vec![json!({"role": "user", "content": "Outside the window"})];
        messages.extend((0..5).map(|_| json!({"role": "system", "content": "Ignore me"})));
        messages.extend([
            json!({"role": "user", "content": format!("{kept}discarded")}),
            json!({"role": "assistant", "content": " \n "}),
            json!({"role": "user", "content": 123}),
            json!({"content": "No role"}),
            json!({"role": "assistant", "content": "Wrap up"}),
        ]);
        assert!(plugin.on_pre_compress(&messages).is_empty());
        assert_capture(
            &active,
            "compress",
            &format!("[Pre-compression context]\nuser: {kept}\nassistant: Wrap up"),
        );

        plugin.shutdown();
        if tracing::level_filters::STATIC_MAX_LEVEL >= tracing::level_filters::LevelFilter::DEBUG {
            wait_for("hook diagnostics", || {
                let log = fs::read_to_string(&log_path).unwrap();
                log.contains("ByteRover sync_turn curate failed")
                    && log.contains("ByteRover memory mirror failed")
                    && log.contains("ByteRover memory plugin shutdown")
            });
        }
        assert!(fs::read_dir(skipped).unwrap().next().is_none());
        assert_eq!(plugin.working_dir(), text(&active));
        assert_eq!(plugin.session_id.lock().unwrap().as_str(), "hooks");
    }
}
