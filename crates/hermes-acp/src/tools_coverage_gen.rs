use super::*;
use serde_json::json;

fn assert_result(tool: &str, input: &str, expected: &str) {
    assert_eq!(
        format_tool_result(tool, Some(input)).as_deref(),
        Some(expected),
        "unexpected result for {tool}: {input}"
    );
}

#[test]
fn titles_use_single_urls_and_handle_missing_arguments() {
    let args = json!({"urls": "https://example.invalid/article"});
    assert_eq!(
        tool_title("web_extract", Some(&args)),
        "extract: https://example.invalid/article"
    );
    assert_eq!(tool_title("web_extract", None), "web_extract");
    assert_eq!(tool_title("custom_tool", None), "custom_tool");

    let args = json!({"path": 42});
    assert_eq!(tool_title("read_file", Some(&args)), "read: 42");
}

#[test]
fn delegation_and_visual_titles_truncate_by_character_count() {
    let args = json!({"tasks": [], "goal": "é".repeat(61)});
    assert_eq!(
        tool_title("delegate_task", Some(&args)),
        format!("delegate: {}...", "é".repeat(57))
    );
    let args = json!({"tasks": [], "goal": " \n "});
    assert_eq!(tool_title("delegate_task", Some(&args)), "delegate task");

    for (tool, key, prefix) in [
        ("browser_vision", "question", "browser vision"),
        ("vision_analyze", "question", "analyze image"),
        ("image_generate", "description", "generate image"),
    ] {
        let args = json!({ (key): "界".repeat(51) });
        assert_eq!(
            tool_title(tool, Some(&args)),
            format!("{prefix}: {}...", "界".repeat(47))
        );
        assert_eq!(tool_title(tool, None), prefix);
    }

    let args = json!({"prompt": "  A garden  ", "description": "Unused"});
    assert_eq!(
        tool_title("image_generate", Some(&args)),
        "generate image: A garden"
    );
}

#[test]
fn skill_titles_omit_blank_file_paths() {
    let args = json!({"name": "notes", "action": "update", "file_path": " \t "});
    assert_eq!(tool_title("skill_view", Some(&args)), "skill view (notes)");
    assert_eq!(
        tool_title("skill_manage", Some(&args)),
        "skill update: notes"
    );
}

#[test]
fn failure_flags_inside_json_arrays_do_not_fail_the_tool() {
    assert_eq!(
        tool_completion_status(
            "read_file",
            r#" [{"success":false,"error":"missing"}] "#.into()
        ),
        "completed"
    );
}

#[test]
fn text_results_count_unicode_characters_when_truncated() {
    let payload = format!("  {} \n", "界".repeat(5_001));
    let expected = format!("{}\n... (5001 chars total, truncated)", "界".repeat(4_920));
    assert_result("custom_tool", &payload, &expected);
}

#[test]
fn read_result_fences_are_longer_than_embedded_backtick_runs() {
    assert_result(
        "read_file",
        r#"{"path":"notes.md","total_lines":2,"content":"one `` then ````\ntwo"}"#,
        "Read notes.md - 2 total lines\n\n`````\none `` then ````\ntwo\n`````",
    );
}

#[test]
fn value_summaries_distinguish_scalar_and_collection_shapes() {
    for (value, expected) in [
        (Value::Null, None),
        (json!(" \t "), None),
        (json!(" note "), Some("note")),
        (json!(false), Some("false")),
        (json!([]), Some("0 items")),
        (json!([1]), Some("1 item")),
        (json!([1, 2]), Some("2 items")),
        (json!({}), Some("0 fields")),
        (json!({"a": 1}), Some("1 field")),
        (json!({"a": 1, "b": 2}), Some("2 fields")),
    ] {
        assert_eq!(value_summary(&value).as_deref(), expected, "value: {value}");
    }
}

#[test]
fn todo_results_include_cancelled_and_pending_items() {
    assert_result(
        "todo",
        r#"{
            "todos": [
                {"content":"Done","status":"completed"},
                {"content":"Ongoing","status":"in_progress"},
                {"content":" Cancelled ","status":"cancelled"},
                {"id":"follow-up"},
                {"content":" "},
                null
            ],
            "summary":{"completed":1,"in_progress":1,"pending":1,"cancelled":1}
        }"#,
        concat!(
            "**Todo list**\n\n",
            "- [x] Done\n- [~] Ongoing\n- [-] Cancelled\n- [ ] follow-up\n\n",
            "**Progress:** 1 completed, 1 in progress, 1 pending, 1 cancelled"
        ),
    );
}

#[test]
fn file_search_lists_paths_up_to_its_page_limit() {
    assert_result(
        "search_files",
        r#"{"files":["notes.txt"]}"#,
        "File search results\nFound 1 file; showing 1.\n\n- notes.txt",
    );

    let files: Vec<_> = (0..21).map(|i| format!("file-{i:02}.txt")).collect();
    let input = json!({"files": files, "total_count": 42}).to_string();
    let mut expected = vec![
        "File search results".to_string(),
        "Found 42 files; showing 20.".to_string(),
        String::new(),
    ];
    expected.extend((0..20).map(|i| format!("- file-{i:02}.txt")));
    expected.extend([
        String::new(),
        "Results truncated. Narrow the search or use offset to page.".to_string(),
    ]);
    assert_result("search_files", &input, &expected.join("\n"));
}

#[test]
fn search_matches_summarize_non_objects_and_limit_the_page() {
    assert_result(
        "search_files",
        r#"{"matches":[null,"notes.txt",false,1,2,3,4,5,6,7,8,9,"hidden"]}"#,
        concat!(
            "Search results\nFound 13 matches; showing 12.\n\n",
            "- notes.txt\n- false\n- 1\n- 2\n- 3\n- 4\n- 5\n- 6\n- 7\n- 8\n- 9\n\n",
            "Results truncated. Narrow the search, add file_glob, or use offset to page."
        ),
    );
}

#[test]
fn execution_results_format_plain_text_and_structured_streams() {
    assert_eq!(json_loads_maybe(Some(" \t\n")), None);
    assert_eq!(format_execute_code_result(Some(" \t\n")), None);
    assert_eq!(
        format_execute_code_result(Some("  plain output \n")).as_deref(),
        Some("plain output")
    );
    assert_result(
        "execute_code",
        r#"{"returncode":3,"output":" report\nsecond line ","error":" warning "} trailing text"#,
        "Exit code: 3\n\nOutput:\nreport\nsecond line\n\nError:\nwarning",
    );
    assert_result(
        "execute_code",
        r#"{"output":" ","error":"\n"}"#,
        "Execution complete",
    );
}

#[test]
fn skill_view_shows_headings_without_exposing_the_body() {
    let content =
        "  # Intro\nPrivate body\n###\n## Café\n### Usage\n# D\n# E\n# F\n# G\n# H\n# Hidden";
    let input = json!({
        "name": "notes",
        "path": "guide.md",
        "description": "  A guide  ",
        "content": content
    });
    let expected = format!(
        concat!(
            "**Skill loaded**\n\n- **Name:** `notes`\n- **File:** `guide.md`\n",
            "- **Description:** A guide\n- **Content:** {} chars loaded into agent context\n\n",
            "**Sections**\n- Intro\n- Café\n- Usage\n- D\n- E\n- F\n- G\n- H\n\n",
            "_Full skill content is available to the agent but hidden here to keep ACP readable._"
        ),
        content.chars().count()
    );
    assert_result("skill_view", &input.to_string(), &expected);
    assert_result(
        "skill_view",
        r#"{"success":false,"error":"not installed"}"#,
        "Skill view failed: not installed",
    );
    assert_result(
        "skill_view",
        r#"{"success":false}"#,
        "Skill view failed: unknown error",
    );
}

#[test]
fn edit_results_report_paths_messages_replacements_and_failures() {
    assert_result(
        "patch",
        r#"{"file_path":"draft.txt","message":"  Updated  ","replacement_count":2}"#,
        "patch completed for `draft.txt`\nUpdated\nReplacements: 2",
    );
    assert_result(
        "write_file",
        r#"{"path":"draft.txt","error":"read only"}"#,
        "write_file failed for draft.txt: read only",
    );
    assert_result(
        "skill_manage",
        r#"{"success":false}"#,
        "skill_manage failed for file: unknown error",
    );
}

#[test]
fn browser_results_render_text_pages_and_failures() {
    assert_eq!(
        format_browser_result("browser_snapshot", Some("  plain snapshot \n")).as_deref(),
        Some("plain snapshot")
    );
    assert_result(
        "browser_navigate",
        r#"{"title":"Catalog","url":"https://example.invalid/","content":" Welcome "}"#,
        "Catalog\nhttps://example.invalid/\n\nWelcome",
    );
    assert_result(
        "browser_snapshot",
        r#"{"url":"https://example.invalid/","snapshot":" Main panel "}"#,
        "https://example.invalid/\n\nMain panel",
    );
    assert_result(
        "browser_vision",
        r#"{"analysis":" A blue square "}"#,
        "browser_vision\n\nA blue square",
    );
    assert_result(
        "browser_vision",
        r#"{"status":"ready","message":" No screenshot requested "}"#,
        "ready\n\nNo screenshot requested",
    );
    assert_result(
        "browser_navigate",
        r#"{"error":"navigation cancelled"}"#,
        "browser_navigate failed: navigation cancelled",
    );
    assert_result(
        "browser_snapshot",
        r#"{"success":false}"#,
        "browser_snapshot failed: unknown error",
    );
}

#[test]
fn browser_image_results_handle_missing_labels_and_urls() {
    assert_result(
        "browser_get_images",
        r#"{"data":[{"alt":"Logo","src":"https://example.invalid/logo.png"},null,{"alt":" "} ]}"#,
        "Images found: 3\n- Logo - https://example.invalid/logo.png\n- image",
    );
}

#[test]
fn web_extract_reports_global_and_per_url_failures() {
    assert_result(
        "web_extract",
        r#"{"success":false,"error":"request rejected"}"#,
        "Web extract failed: request rejected",
    );
    assert_result(
        "web_extract",
        r#"{"success":false}"#,
        "Web extract failed: unknown error",
    );
    assert_result(
        "web_extract",
        r#"{"results":[{"url":" https://example.invalid/a ","error":"missing"},{"error":"denied"},{"error":null}]}"#,
        "Web extract failed for 2 URLs\n- https://example.invalid/a\n  Error: missing\n- Untitled\n  Error: denied",
    );
}

#[test]
fn process_lists_prefer_status_metadata_and_limit_the_number_shown() {
    let mut processes = vec![
        json!({"id":"finished","exited":true,"pid":7,"returncode":2,"command":" print report "}),
        json!({"session_id":"active","exited":false}),
        json!("  detached  "),
        Value::Null,
    ];
    processes.extend((1..=17).map(|i| json!(i)));
    let input = json!({"processes": processes}).to_string();
    let mut expected = vec![
        "Processes: 21".to_string(),
        "- `finished` - exited, pid 7, exit 2 - print report".to_string(),
        "- `active` - running".to_string(),
        "- detached".to_string(),
    ];
    expected.extend((1..=16).map(|i| format!("- {i}")));
    expected.push("... 1 more process(es)".to_string());
    assert_result("process", &input, &expected.join("\n"));
}

#[test]
fn process_details_format_metadata_streams_and_errors() {
    assert_eq!(
        format_process_result(Some("  raw status \n")).as_deref(),
        Some("raw status")
    );
    assert_result(
        "process",
        r#"{
            "action":"poll","state":"exited","session_id":"p-1","command":" print report ",
            "pid":7,"exit_code":0,"lines":2,"stdout":" report ",
            "stderr":" warning ","message":" Done "
        }"#,
        concat!(
            "Process poll: exited (`p-1`)\n",
            "- **Command:** print report\n- **PID:** 7\n- **Exit code:** 0\n- **Lines:** 2\n\n",
            "Output:\nreport\n\nError:\nwarning\nDone"
        ),
    );
    assert_result("process", "{}", "Process process: process");
    assert_result(
        "process",
        r#"{"error":"session gone"}"#,
        "Process error: session gone",
    );
    assert_result(
        "process",
        r#"{"success":false}"#,
        "Process error: unknown error",
    );
}

#[test]
fn delegation_reports_roles_errors_and_omitted_trace_entries() {
    let trace: Vec<_> = (1..=13)
        .map(|i| json!({"tool": format!("check-{i:02}")}))
        .collect();
    let input = json!({
        "results": [
            "external result",
            null,
            {
                "task_index": 0, "status": "failed", "model": "fixture",
                "_child_role": "reviewer", "duration_seconds": 3,
                "error": " incomplete ", "tool_trace": trace
            },
            {"task_index": 1, "status": "completed", "tool_trace": [{}, null]}
        ]
    });
    assert_result(
        "delegate_task",
        &input.to_string(),
        concat!(
            "Delegation results: 4 tasks\n- external result\n\n",
            "Task 1: failed (fixture, role=reviewer, 3s)\nError: incomplete\n",
            "Tools: check-01, check-02, check-03, check-04, check-05, check-06, ",
            "check-07, check-08, check-09, check-10, check-11, check-12 (+1)\n\n",
            "Task 2: completed"
        ),
    );
    assert_result(
        "delegate_task",
        r#"{"results":[null]}"#,
        "Delegation results: 1 task",
    );
    assert_result(
        "delegate_task",
        r#"{"error":"worker unavailable"}"#,
        "Delegation failed: worker unavailable",
    );
    assert_result(
        "delegate_task",
        r#"{"error":null}"#,
        "Delegation failed: unknown error",
    );
}

#[test]
fn session_search_formats_errors_empty_results_and_entries_without_metadata() {
    assert_result(
        "session_search",
        r#"{"success":false,"error":"index unavailable"}"#,
        "Session search failed: index unavailable",
    );
    assert_result(
        "session_search",
        r#"{"success":false}"#,
        "Session search failed: unknown error",
    );
    assert_result(
        "session_search",
        r#"{"query":" notes ","results":[],"message":" Nothing newer "}"#,
        "Session search results for `notes`\nNothing newer",
    );
    assert_result(
        "session_search",
        r#"{"query":" ","results":[]}"#,
        "Session search results\nNo matching sessions found.",
    );
    assert_result(
        "session_search",
        r#"{"results":[{"session_id":"s1","title":"Notes","preview":" One\n  two "}]}"#,
        "Session search results\n- **Notes** (`s1`)\n  One two",
    );
}

#[test]
fn memory_failures_show_at_most_the_first_five_match_candidates() {
    assert_result(
        "memory",
        r#"{
            "success":false,"action":"remove","target":"notes","error":"ambiguous",
            "matches":[" First ",null,true,["a","b"],{"key":1},"hidden"]
        }"#,
        "Memory remove failed (notes)\nambiguous\nMatches:\n- First\n- true\n- 2 items\n- 1 field",
    );
    assert_result(
        "memory",
        r#"{"success":false}"#,
        "Memory memory failed (memory)\nunknown error",
    );
}

#[test]
fn media_and_cron_results_handle_plain_text_and_failures() {
    assert_eq!(
        format_media_or_cron_result("text_to_speech", Some("  audio ready \n")).as_deref(),
        Some("audio ready")
    );
    assert_result(
        "image_generate",
        r#"{"error":"invalid prompt"}"#,
        "image_generate failed: invalid prompt",
    );
    assert_result(
        "cronjob",
        r#"{"success":false}"#,
        "cronjob failed: unknown error",
    );
}

#[test]
fn generic_array_results_render_nested_values_and_cap_both_lists() {
    assert_result(
        "custom_tool",
        r#"[
            {"a_null":null,"b_blank":" ","c_nested":{"leaf":[1,2]}},
            [{"payload":" data "},false,null,3,4,5,6],
            " note ",null,true,0,1,2,3,4,5,6,"hidden"
        ]"#,
        concat!(
            "custom_tool: 13 items\n",
            "- 3 fields\n  - **c_nested:**\n    - **leaf:** 2 items\n",
            "- 7 items\n  1.\n    - **payload:** data\n",
            "  2. false\n  3. null\n  4. 3\n  5. 4\n  6. 5\n  ... 1 more items\n",
            "- note\n- true\n- 0\n- 1\n- 2\n- 3\n- 4\n- 5\n- 6\n... 1 more items"
        ),
    );
    assert_result("custom_tool", "[1]", "custom_tool: 1 item\n- 1");
}

#[test]
fn structured_values_respect_depth_field_limits_and_indentation() {
    let value = json!({"alpha":{"one":true},"beta":[1,2],"delta":true,"gamma":true});
    assert_eq!(
        format_structured_value("details", &value, 1, 1, 2),
        [
            "  - **details:**",
            "    - **alpha:** 1 field",
            "    - **beta:** 2 items",
            "    - ... 2 more fields",
        ]
    );
    assert_eq!(
        format_structured_value("", &json!([1]), 1, 0, 6),
        ["  - 1 item"]
    );
    assert_eq!(
        format_structured_value("", &json!("  note  "), 1, 1, 6),
        ["  - note"]
    );
    assert!(format_structured_value("empty", &Value::Null, 0, 1, 6).is_empty());
}

#[test]
fn generic_scalar_fallback_is_opt_in() {
    let input = Some(" \t\"hello\" \n");
    assert_eq!(
        format_generic_structured_result("custom_tool", input, true).as_deref(),
        Some("\"hello\"")
    );
    assert_eq!(
        format_generic_structured_result("custom_tool", input, false),
        None
    );
}

#[test]
fn generic_objects_skip_blank_and_internal_fields_and_cap_output_lines() {
    let mut fields = serde_json::Map::new();
    fields.insert("blank".to_string(), json!(" \t "));
    fields.insert("entries".to_string(), json!(["internal entry"]));
    fields.insert("raw".to_string(), json!("internal response"));
    fields.insert("status".to_string(), json!("ready"));
    for i in 0..45 {
        fields.insert(format!("field_{i:02}"), json!(i));
    }

    let mut expected = vec![
        "custom_tool result".to_string(),
        "- **status:** ready".to_string(),
    ];
    expected.extend((0..38).map(|i| format!("- **field_{i:02}:** {i}")));
    expected.push("- ... more fields truncated".to_string());
    assert_result(
        "custom_tool",
        &Value::Object(fields).to_string(),
        &expected.join("\n"),
    );
}
