use super::*;

#[test]
fn upstream_commands_come_from_command_defs() {
    let source = r#"
COMMANDS = [
    CommandDef("help", "Show help"),
    CommandDef("model", "Switch model"),
    CommandDef("help", "duplicate"),
    CommandDef("Bad", "uppercase is not a command name"),
]
"#;
    let found: Vec<String> = extract_upstream_slash_commands(source)
        .into_iter()
        .collect();
    assert_eq!(found, ["/help", "/model"]);
}

#[test]
fn local_commands_come_from_the_slash_commands_block_only() {
    let source = r#"
const OTHER: &[&str] = &["/outside"];
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/new", "Start a new session"),
    ("/model", "Switch model"),
];
const AFTER: &[&str] = &["/after"];
"#;
    let found: Vec<String> = extract_local_slash_commands(source).into_iter().collect();
    assert_eq!(found, ["/model", "/new"]);
    assert!(extract_local_slash_commands("no table here").is_empty());
    assert!(extract_local_slash_commands("pub const SLASH_COMMANDS = [\"/x\"").is_empty());
}

#[test]
fn allowlist_reads_an_object_a_list_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("allow.json");
    assert!(load_allowlist(&path).unwrap().is_empty());

    std::fs::write(
        &path,
        r#"{"allowed_missing_upstream_commands": ["/a", " ", "/b"], "reasons": {"/a": "x"}}"#,
    )
    .unwrap();
    let found: Vec<String> = load_allowlist(&path).unwrap().into_iter().collect();
    assert_eq!(found, ["/a", "/b"]);

    std::fs::write(&path, r#"["/c"]"#).unwrap();
    let found: Vec<String> = load_allowlist(&path).unwrap().into_iter().collect();
    assert_eq!(found, ["/c"]);

    std::fs::write(&path, r#""text""#).unwrap();
    assert!(load_allowlist(&path).unwrap().is_empty());

    std::fs::write(&path, "not json").unwrap();
    assert!(load_allowlist(&path).is_err());
}
