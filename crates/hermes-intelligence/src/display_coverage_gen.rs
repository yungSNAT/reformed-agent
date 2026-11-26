use super::*;
use serde_json::json;

#[test]
fn tool_previews_handle_empty_objects_and_special_task_modes() {
    let cases = [
        ("web_search", json!({}), None),
        ("process", json!({"action": null, "session_id": 42}), None),
        ("todo", json!({"merge": true}), Some("reading task list")),
        (
            "todo",
            json!({"todos": [{"id": "a"}, {"id": "b"}]}),
            Some("planning 2 task(s)"),
        ),
    ];

    for (tool, args, expected) in cases {
        assert_eq!(
            build_tool_preview(tool, &args, 60).as_deref(),
            expected,
            "{tool}: {args}"
        );
    }
}

#[test]
fn memory_previews_show_the_action_and_relevant_text() {
    let old_text = "abcdefghijklmnopqrstuvwxyz";
    let cases = [
        (
            json!({"action": "add", "target": "notes", "content": "abcdefghijklmnopqrstuéxyz"}),
            "+notes: \"abcdefghijklmnopqrstu...\"",
        ),
        (
            json!({"action": "replace", "target": "notes", "old_text": old_text, "content": "new"}),
            "~notes: \"abcdefghijklmnopq...\"",
        ),
        (
            json!({"action": "remove", "target": "notes", "old_text": old_text}),
            "-notes: \"abcdefghijklmnopq...\"",
        ),
        (json!({"action": "list", "target": "notes"}), "list"),
    ];

    for (args, expected) in cases {
        assert_eq!(
            build_tool_preview("memory", &args, 60).as_deref(),
            Some(expected),
            "{args}"
        );
    }
}

#[test]
fn generic_previews_choose_a_fallback_key_or_the_first_array_entry() {
    let cases = [
        (
            "custom_lookup",
            json!({"name": "secondary", "query": "  alpha\n beta "}),
            Some("alpha beta"),
        ),
        ("custom_lookup", json!({"unrecognized": "ignored"}), None),
        (
            "web_extract",
            json!({"urls": ["  first  ", "second"]}),
            Some("first"),
        ),
        ("web_extract", json!({"urls": [" \n\t ", "second"]}), None),
        ("web_extract", json!({"urls": [42, "second"]}), None),
        ("web_extract", json!({"urls": 42}), None),
    ];

    for (tool, args, expected) in cases {
        assert_eq!(
            build_tool_preview(tool, &args, 60).as_deref(),
            expected,
            "{tool}: {args}"
        );
    }
}

#[test]
fn tool_labels_and_calls_omit_missing_previews() {
    let blank = json!({"query": " \n\t "});
    assert_eq!(
        build_tool_label("web_search", &blank, 60, true).as_deref(),
        Some("Searching the web")
    );
    assert_eq!(format_tool_call("web_search", &blank), "web_search");
    assert_eq!(
        format_tool_call("web_search", &json!({"query": "rust\n docs"})),
        "web_search(rust docs)"
    );
}

#[test]
fn tool_result_truncation_respects_utf8_boundaries() {
    let result = "aébc";
    assert_eq!(format_tool_result(result, 0), result);
    assert_eq!(format_tool_result(result, 5), result);
    assert_eq!(format_tool_result(result, 2), "a... (5 chars)");
}

#[test]
fn completion_messages_shorten_long_paths_from_the_left() {
    let tail = "abcdefghijklmnopqrstuvwxyz123.rs";
    let path = format!("/workspace/generated/{tail}");
    assert_eq!(
        get_cute_tool_message("write_file", &json!({"path": path}), 1.0, None),
        format!("┊ ✍️  write     ...{tail}  1.0s")
    );
    assert_eq!(
        get_cute_tool_message("patch", &json!({"path": "src/main.rs"}), 1.0, None),
        "┊ 🔧 patch     src/main.rs  1.0s"
    );
}

#[test]
fn completion_messages_distinguish_filename_search_from_content_search() {
    for (target, expected) in [
        ("files", "┊ 🔎 find      needle  1.0s"),
        ("content", "┊ 🔎 grep      needle  1.0s"),
    ] {
        assert_eq!(
            get_cute_tool_message(
                "search_files",
                &json!({"pattern": "needle", "target": target}),
                1.0,
                None,
            ),
            expected
        );
    }
}

#[test]
fn navigation_completion_shows_the_domain_without_the_scheme_or_path() {
    for url in [
        "https://docs.example.test/guide?page=1",
        "http://docs.example.test/index.html",
    ] {
        assert_eq!(
            get_cute_tool_message("browser_navigate", &json!({"url": url}), 1.0, None),
            "┊ 🌐 navigate  docs.example.test  1.0s"
        );
    }
}

#[test]
fn execution_completion_handles_a_shell_summary_and_blank_code() {
    assert_eq!(
        get_cute_tool_message(
            "execute_code",
            &json!({"code": "cd /tmp && python demo.py | head -n 1"}),
            1.0,
            None,
        ),
        "┊ 🐍 exec      python demo.py  1.0s"
    );
    assert_eq!(
        get_cute_tool_message("execute_code", &json!({"code": " \n\t "}), 1.0, None),
        "┊ 🐍 exec        1.0s"
    );
}

#[test]
fn terminal_completion_marks_only_numeric_nonzero_exit_codes() {
    let args = json!({"command": "echo ready"});
    let base = "┊ 💻 $         echo ready  1.0s";
    for (result, suffix) in [
        (r#"{"exit_code":7}"#, " [exit 7]"),
        (r#"{"exit_code":-9}"#, " [exit -9]"),
        (r#"{"exit_code":0,"error":"stderr text"}"#, ""),
        (r#"{"exit_code":"7"}"#, ""),
        ("Error: not a JSON result", ""),
    ] {
        assert_eq!(
            get_cute_tool_message("terminal", &args, 1.0, Some(result)),
            format!("{base}{suffix}"),
            "{result}"
        );
    }
}

#[test]
fn other_tool_completions_mark_error_results() {
    let args = json!({"query": "rust"});
    let base = "┊ 🔍 search    rust  1.0s";
    for (result, suffix) in [
        (r#"{"ERROR":"unavailable"}"#, " [error]"),
        (r#"{"status":"FaIlEd"}"#, " [error]"),
        ("Error: unavailable", " [error]"),
        (r#"{"status":"ok"}"#, ""),
    ] {
        assert_eq!(
            get_cute_tool_message("web_search", &args, 1.0, Some(result)),
            format!("{base}{suffix}"),
            "{result}"
        );
    }
}

#[test]
fn compact_stats_format_billions_and_long_durations() {
    assert_eq!(format_token_count(1_000_000_000), "1.0B");
    assert_eq!(format_token_count(1_500_000_000), "1.5B");
    assert_eq!(format_duration_compact(7_620.0), "2h 7m");
    assert_eq!(format_duration_compact(129_600.0), "1.5d");
}

#[test]
fn context_pressure_reports_thresholds_and_clamps_overfull_bars() {
    let partial = format!("{}{}", "▰".repeat(7), "▱".repeat(13));
    let full = "▰".repeat(20);

    assert_eq!(
        format_context_pressure(0.375, 12_345, 0.875, true),
        format!(
            "  \x1b[1m\x1b[33m⚠ context {partial} 37% to compaction\x1b[0m  \x1b[2m\
             12k threshold (87%) · compaction approaching\x1b[0m"
        )
    );
    assert_eq!(
        format_context_pressure(1.25, 999, 0.5, false),
        format!(
            "  \x1b[1m\x1b[33m⚠ context {full} 100% to compaction\x1b[0m  \x1b[2m\
             999 threshold (50%) · no auto-compaction\x1b[0m"
        )
    );
    assert_eq!(
        format_context_pressure_gateway(0.375, 0.875, true),
        format!(
            "⚠️ Context: {partial} 37% to compaction\n\
             Context compaction approaching (threshold: 87% of window)."
        )
    );
    assert_eq!(
        format_context_pressure_gateway(1.25, 0.5, false),
        format!(
            "⚠️ Context: {full} 100% to compaction\n\
             Auto-compaction is disabled — context may be truncated."
        )
    );
}

#[test]
fn zero_total_progress_has_no_percentage() {
    assert_eq!(format_progress_bar(5, 0, 4), "[    ]");
}

#[test]
fn inline_diff_styles_headers_hunks_and_changes() {
    let diff = concat!(
        "diff --git a/demo.txt b/demo.txt\n",
        "---   a/demo.txt  \n",
        "+++  b/demo.txt  \n",
        "@@ -1,2 +1,2 @@\n",
        " same\n",
        "-old\n",
        "+new\n",
        "\\ No newline at end of file\n",
        "\n",
        "+++   \n",
        "+++ b/orphan.txt\n",
    );

    assert_eq!(
        render_inline_unified_diff(diff),
        [
            "diff --git a/demo.txt b/demo.txt",
            "\x1b[38;2;180;160;255ma/demo.txt → b/demo.txt\x1b[0m",
            "\x1b[38;2;120;120;140m@@ -1,2 +1,2 @@\x1b[0m",
            "\x1b[38;2;150;150;150m same\x1b[0m",
            "\x1b[38;2;255;255;255;48;2;120;20;20m-old\x1b[0m",
            "\x1b[38;2;255;255;255;48;2;20;90;20m+new\x1b[0m",
            "\\ No newline at end of file",
            "\x1b[38;2;180;160;255ma/? → b/orphan.txt\x1b[0m",
        ]
    );
}

#[test]
fn model_response_renders_reasoning_only_when_present_and_nonempty() {
    let content = "Answer\n";
    assert_eq!(
        format_model_response(content, Some("First step\nSecond step")),
        concat!(
            "\x1b[2m💭 Reasoning:\x1b[0m\n",
            "\x1b[2m  First step\x1b[0m\n",
            "\x1b[2m  Second step\x1b[0m\n",
            "\n",
            "Answer\n",
        )
    );
    assert_eq!(format_model_response(content, Some("")), content);
    assert_eq!(format_model_response(content, None), content);
}

#[test]
fn terminal_previews_coerce_scalars_and_follow_first_array_entries() {
    let cases = [
        (json!(-42), Some("-42")),
        (json!(u64::MAX), Some("18446744073709551615")),
        (json!(true), Some("true")),
        (
            json!([["echo first", "echo unused"], "echo second"]),
            Some("echo first"),
        ),
        (json!([]), None),
        (json!({"text": "echo ignored"}), None),
    ];

    for (value, expected) in cases {
        let args = json!({"command": value});
        assert_eq!(
            build_tool_preview("terminal", &args, 0).as_deref(),
            expected,
            "{args}"
        );
    }
}

#[test]
fn shell_summary_preserves_quoted_separators_and_hides_shell_plumbing() {
    let command = concat!(
        "_MODE2=fast cd /tmp/demo || printf '%s' 'left || right' > out.txt 2>> err.txt; ",
        "echo '$?'; echo '${PIPESTATUS[0]}'; echo PIPESTATUS; pwd",
    );
    assert_eq!(
        summarize_shell_command(command),
        "printf '%s' 'left || right' + 1 command"
    );
    assert_eq!(
        summarize_shell_command("BAD-NAME=fast cd /tmp/demo; pwd"),
        "BAD-NAME=fast cd /tmp/demo + 1 command"
    );
    assert_eq!(summarize_shell_command("> output.txt"), "> output.txt");
}

#[test]
fn read_file_preview_handles_single_line_nonpositive_offsets_and_blank_paths() {
    let cases = [
        (
            json!({"path": r"C:\workspace\src\lib.rs", "offset": 0, "limit": 3}),
            Some("lib.rs"),
        ),
        (
            json!({"file": "src/lib.rs", "offset": " 7 ", "limit": 1}),
            Some("lib.rs L7"),
        ),
        (json!({"filepath": " \t ", "offset": 7, "limit": 1}), None),
    ];

    for (args, expected) in cases {
        assert_eq!(
            build_tool_preview("read_file", &args, 60).as_deref(),
            expected,
            "{args}"
        );
    }
}
