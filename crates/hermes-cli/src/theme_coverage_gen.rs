use super::*;
use std::fs;

fn load_parse_error(path: &Path) -> String {
    match Theme::load_theme(path).expect_err("invalid theme should not load") {
        ThemeError::Parse(message) => message,
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn invalid_colors_fall_back_to_white() {
    for input in ["#ff00zz", "256", "not-a-color"] {
        assert_eq!(ThemeColors::parse_color(input), Color::White, "{input:?}");
    }
}

#[test]
fn hex_parser_rejects_non_ascii_and_unsupported_lengths() {
    assert_eq!(parse_hex("aé123"), Err(()));
    assert_eq!(parse_hex("1234"), Err(()));
}

#[test]
fn underlined_style_preserves_colors_and_other_modifiers() {
    let definition = StyleDef {
        fg: Some("cyan".into()),
        bg: Some("black".into()),
        bold: true,
        italic: false,
        underline: true,
    };

    assert_eq!(
        definition.to_style(),
        Style::default()
            .fg(Color::Cyan)
            .bg(Color::Black)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    );
}

#[test]
fn loads_json_themes_with_or_without_a_json_extension() {
    let dir = tempfile::tempdir().expect("create theme directory");
    let expected = Theme {
        name: "Custom theme".into(),
        ..light_theme()
    };
    let json = serde_json::to_vec(&expected).expect("serialize custom theme");

    for filename in ["custom.JsOn", "custom.theme", "custom"] {
        let path = dir.path().join(filename);
        fs::write(&path, &json).expect("write custom theme");
        assert_eq!(
            Theme::load_theme(&path).expect("load custom theme"),
            expected,
            "{filename}"
        );
    }
}

#[test]
fn invalid_json_theme_reports_the_json_parser_error() {
    let dir = tempfile::tempdir().expect("create theme directory");
    let path = dir.path().join("broken.json");
    let contents = r#"{"name":"unfinished""#;
    fs::write(&path, contents).expect("write malformed JSON");

    let expected = serde_json::from_str::<Theme>(contents)
        .expect_err("fixture should be malformed")
        .to_string();
    assert_eq!(load_parse_error(&path), expected);
}

#[test]
fn toml_theme_error_recommends_json() {
    let dir = tempfile::tempdir().expect("create theme directory");
    let path = dir.path().join("custom.ToMl");
    fs::write(&path, "name = \"Custom theme\"\n").expect("write TOML fixture");

    assert_eq!(
        load_parse_error(&path),
        "TOML theme loading requires the `toml` crate. Use JSON format."
    );
}

#[test]
fn non_json_theme_with_unknown_extension_reports_unsupported_format() {
    let dir = tempfile::tempdir().expect("create theme directory");
    let path = dir.path().join("custom.ThEmE");
    fs::write(&path, "name: Custom theme\n").expect("write non-JSON fixture");

    assert_eq!(
        load_parse_error(&path),
        "Unsupported theme file format: theme. Use .json or .toml"
    );
}
