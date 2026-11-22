use super::*;
use ratatui::style::Color;

#[test]
fn inline_backticks_split_spans_without_rendering_the_delimiters() {
    let base = Style::default().fg(Color::White);
    let code = Style::default().fg(Color::Yellow);

    assert_eq!(
        render_inline_with_code("> ", "before `a_b` after", base, code),
        Line::from(vec![
            Span::styled("> ", base),
            Span::styled("before ", base),
            Span::styled("a_b", code),
            Span::styled(" after", base),
        ])
    );
    assert_eq!(
        render_inline_with_code("", "`unfinished", base, code),
        Line::from(vec![Span::styled("unfinished", code)])
    );
    assert_eq!(
        render_inline_with_code("", "``", base, code),
        Line::from(vec![Span::styled("", base)])
    );
}

#[test]
fn numbered_markers_preserve_the_delimiter_and_body() {
    assert_eq!(
        parse_markdown_numbered_marker("012. café"),
        Some(("012.", "café"))
    );
    assert_eq!(
        parse_markdown_numbered_marker("7)  nested"),
        Some(("7)", " nested"))
    );
    for invalid in ["12.no space", "12)", " 1. indented", "１. non-ASCII digit"] {
        assert_eq!(parse_markdown_numbered_marker(invalid), None, "{invalid:?}");
    }
}

#[test]
fn highlighting_selects_keywords_by_normalized_language_alias() {
    for (lang, keyword) in [
        (" Rust ", "fn"),
        ("rS", "where"),
        (" Python ", "def"),
        ("pY", "elif"),
        ("JavaScript", "const"),
        (" JS ", "function"),
        ("TypeScript", "export"),
        ("tS", "switch"),
        (" BASH ", "then"),
        ("sh", "esac"),
        ("ZSH", "echo"),
    ] {
        let keywords = keyword_set_for_lang(lang);
        assert!(
            keywords.contains(&keyword),
            "{lang:?} should include {keyword}"
        );
        assert!(!keywords.contains(&"not_a_keyword"), "{lang:?}");
    }
    assert!(keyword_set_for_lang(" JSON ").is_empty());
    assert!(keyword_set_for_lang("unrecognized").is_empty());
}

#[test]
fn table_parser_trims_cells_and_rejects_single_cell_rows() {
    assert_eq!(
        parse_table_cells(" \t| left ||  middle  | right | "),
        Some(vec![
            "left".to_owned(),
            "middle".to_owned(),
            "right".to_owned(),
        ])
    );
    assert_eq!(
        parse_table_cells("left|right"),
        Some(vec!["left".to_owned(), "right".to_owned()])
    );
    for invalid in ["no separator", "| only |", " || \t| "] {
        assert_eq!(parse_table_cells(invalid), None, "{invalid:?}");
    }
}

#[test]
fn table_width_has_a_minimum_and_saturates_on_overflow() {
    assert_eq!(content_width_for_table_row(0, 5), 8);
    assert_eq!(content_width_for_table_row(2, 3), 8);
    assert_eq!(content_width_for_table_row(3, 5), 15);
    assert_eq!(content_width_for_table_row(usize::MAX, 2), usize::MAX);
}

#[test]
fn message_fingerprint_changes_when_only_the_role_changes() {
    let mut message = hermes_core::Message::user("same text");
    let user = message_fingerprint(&message);

    message.role = hermes_core::MessageRole::System;
    assert_ne!(message_fingerprint(&message), user);

    message.role = hermes_core::MessageRole::User;
    assert_eq!(message_fingerprint(&message), user);
}

#[test]
fn message_fingerprint_tracks_each_tool_call_field() {
    let mut message = hermes_core::Message::user("same text");
    message.role = hermes_core::MessageRole::Assistant;

    let calls = |id: &str, name: &str, arguments: &str| {
        serde_json::from_value(serde_json::json!([{
            "id": id,
            "type": "function",
            "name": name,
            "arguments": arguments,
        }]))
        .expect("valid tool call fixture")
    };
    let without_calls = message_fingerprint(&message);
    message.tool_calls = Some(calls("call-1", "example", "{}"));
    let original = message_fingerprint(&message);
    assert_ne!(original, without_calls);

    for (id, name, arguments) in [
        ("call-2", "example", "{}"),
        ("call-1", "other", "{}"),
        ("call-1", "example", r#"{"key":1}"#),
    ] {
        message.tool_calls = Some(calls(id, name, arguments));
        assert_ne!(
            message_fingerprint(&message),
            original,
            "changed call: {id}, {name}, {arguments}"
        );
    }
}

#[test]
fn strict_language_setting_accepts_trimmed_case_insensitive_opt_outs() {
    const SETTING: &str = "HERMES_TUI_STRICT_DEFAULT_LANGUAGE";
    const CHILD_EXPECTED: &str = "HERMES_INLINE_MARKDOWN_COVERAGE_EXPECTED_STRICT";

    if let Ok(expected) = std::env::var(CHILD_EXPECTED) {
        let expected = expected.parse::<bool>().expect("valid child expectation");
        assert_eq!(strict_default_language_output_enabled(), expected);
        return;
    }

    // Set the variable on the child only so parallel tests keep their environment.
    let qualified_name = concat!(
        module_path!(),
        "::strict_language_setting_accepts_trimmed_case_insensitive_opt_outs"
    );
    let (_, test_name) = qualified_name
        .split_once("::")
        .expect("test module path includes the crate name");
    let executable = std::env::current_exe().expect("locate the current test binary");

    for (raw, expected) in [
        (None, true),
        (Some(" 0 "), false),
        (Some(" FaLsE\t"), false),
        (Some(" NO "), false),
        (Some("oFf"), false),
        (Some(" true "), true),
    ] {
        let mut command = std::process::Command::new(&executable);
        command
            .args(["--exact", test_name])
            .env(CHILD_EXPECTED, expected.to_string());
        if let Some(value) = raw {
            command.env(SETTING, value);
        } else {
            command.env_remove(SETTING);
        }

        let output = command.output().expect("run the isolated environment test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("running 1 test"),
            "unexpected child result for {raw:?}:\n{stdout}\n{stderr}"
        );
    }
}

#[test]
fn ascii_sanitizer_drops_empty_bodies_and_tool_tags() {
    for compact in [false, true] {
        for rejected in [
            " \t ",
            "\t東京🙂",
            "prefix <TOOL_CALL id=x> suffix",
            "prefix </TOOL_CALL> suffix",
            "prefix <Tool_Use> suffix",
            "prefix </Tool_Use> suffix",
        ] {
            assert_eq!(
                sanitize_line_to_default_language_ascii(rejected, compact),
                None,
                "{rejected:?}, compact={compact}"
            );
        }
    }

    let mixed = " \talpha  β\tbeta ";
    assert_eq!(
        sanitize_line_to_default_language_ascii(mixed, true).as_deref(),
        Some(" \talpha beta")
    );
    assert_eq!(
        sanitize_line_to_default_language_ascii(mixed, false).as_deref(),
        Some(" \talpha   beta")
    );
}
