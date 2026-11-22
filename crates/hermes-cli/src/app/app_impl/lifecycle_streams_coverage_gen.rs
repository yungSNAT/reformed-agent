use super::*;

const CHILD_MARKER: &str = "HERMES_LIFECYCLE_STREAM_COVERAGE_CHILD";
const OUTPUT_BEGIN: &str = "begin lifecycle stream coverage output";
const OUTPUT_END: &str = "end lifecycle stream coverage output";

fn exercise_detached_stream() {
    let shared = Arc::new(StdMutex::new(None));
    let mut callbacks = App::stream_callbacks(Arc::clone(&shared));

    println!("{OUTPUT_BEGIN}");
    println!(
        "stdout enabled: {}",
        App::oneshot_lifecycle_stdout_enabled(&shared)
    );

    let thinking = callbacks.on_thinking.as_mut().expect("thinking callback");
    thinking("");
    thinking("Considering the next step");

    let tool_start = callbacks
        .on_tool_start
        .as_mut()
        .expect("tool start callback");
    tool_start("test-tool", &serde_json::json!({"limit": 3}));

    let tool_complete = callbacks
        .on_tool_complete
        .as_mut()
        .expect("tool completion callback");
    tool_complete("test-tool", "Finished");

    let status = callbacks.status_callback.as_ref().expect("status callback");
    status("progress", "");
    status("progress", "Working");

    App::emit_lifecycle_event(&shared, "");
    App::emit_lifecycle_event(&shared, "Starting");

    App::emit_phase_event(&shared, " \t", "Missing phase", 20);
    App::emit_phase_event(&shared, "context", "", 20);
    App::emit_phase_event(&shared, " context ", "Preparing", 8);
    App::emit_phase_event(&shared, "context", "Ready", 150);

    App::emit_moa_reference_event(&shared, 1, 3, "", "Missing model");
    App::emit_moa_reference_event(&shared, 1, 3, "model-a", "");
    App::emit_moa_reference_event(&shared, 2, 3, "model-a", "Reference text");

    println!("{OUTPUT_END}");
    assert!(
        shared.lock().expect("stream slot lock").is_none(),
        "emitting events must leave a detached stream detached"
    );
}

#[test]
fn detached_stream_events_respect_lifecycle_stdout_opt_in() {
    if std::env::var_os(CHILD_MARKER).as_deref() == Some(std::ffi::OsStr::new("1")) {
        exercise_detached_stream();
        return;
    }

    // Isolate the stdout capture and environment from the other unit tests.
    let home = tempfile::tempdir().expect("temporary child home");
    let executable = std::env::current_exe().expect("test executable");
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let test_name = format!("{module}::detached_stream_events_respect_lifecycle_stdout_opt_in");

    for setting in [None, Some(" off "), Some(" \tYeS ")] {
        let mut command = std::process::Command::new(&executable);
        command
            .arg("--exact")
            .arg(&test_name)
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("HERMES_HOME", home.path());

        if let Some(value) = setting {
            command.env("HERMES_ONESHOT_LIFECYCLE_STDOUT", value);
        } else {
            command.env_remove("HERMES_ONESHOT_LIFECYCLE_STDOUT");
        }

        let output = command.output().expect("run detached stream test in child");
        assert!(
            output.status.success(),
            "child failed for {setting:?}:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8(output.stdout).expect("child stdout is UTF-8");
        let (_, tail) = stdout.split_once(OUTPUT_BEGIN).expect("child test ran");
        let (payload, _) = tail
            .split_once(OUTPUT_END)
            .expect("child finished emitting");
        let lines: Vec<_> = payload.lines().skip(1).collect();
        let expected: &[&str] = if setting == Some(" \tYeS ") {
            &[
                "stdout enabled: true",
                "[lifecycle] Starting",
                "[phase   8%] context: Preparing",
                "[phase 100%] context: Ready",
                "[moa reference 2/3] model-a: Reference text",
            ]
        } else {
            &["stdout enabled: false"]
        };

        assert_eq!(lines.as_slice(), expected, "setting: {setting:?}");
    }
}

#[test]
fn topic_sanitization_replaces_invalid_characters_and_trims_edge_dashes() {
    assert_eq!(
        App::sanitize_topic_path_segment("...Team_1/model v2:β!next---"),
        "Team_1/model-v2---next"
    );
    assert_eq!(App::sanitize_topic_path_segment("/A_b-9/"), "/A_b-9/");
    assert_eq!(App::sanitize_topic_path_segment("...é!?--"), "");
}
