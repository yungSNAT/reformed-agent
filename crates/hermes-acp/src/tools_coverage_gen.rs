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

include!("tools_part2_coverage_gen.rs");
