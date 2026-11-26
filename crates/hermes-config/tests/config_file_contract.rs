use std::fs;

use hermes_config::loader::{load_from_json, load_prefill_messages_file, ConfigError};
use hermes_core::MessageRole;

#[test]
fn prefill_requires_an_array_of_valid_messages() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("prefill.json");
    assert!(load_prefill_messages_file(&path).is_empty());
    assert!(load_prefill_messages_file(home.path()).is_empty());

    for invalid in [
        "[",
        r#"{"role":"system","content":"plain object"}"#,
        r#"[{"role":"system","content":"keep only complete arrays"},{"role":"unknown"}]"#,
    ] {
        fs::write(&path, invalid).unwrap();
        assert!(
            load_prefill_messages_file(&path).is_empty(),
            "unexpected messages from {invalid:?}"
        );
    }

    fs::write(
        &path,
        r#"[{"role":"system","content":"Catalogue the field notes"},
            {"role":"assistant","content":"Ready for notes"}]"#,
    )
    .unwrap();
    let messages = load_prefill_messages_file(&path);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::System);
    assert_eq!(
        messages[0].content.as_deref(),
        Some("Catalogue the field notes")
    );
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].content.as_deref(), Some("Ready for notes"));
}

#[test]
fn json_config_loads_legacy_discord_allowlist_and_preserves_canonical_users() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.json");
    assert!(matches!(
        load_from_json(&path),
        Err(ConfigError::ParseError(_))
    ));
    fs::write(&path, "{").unwrap();
    assert!(matches!(
        load_from_json(&path),
        Err(ConfigError::ParseError(_))
    ));

    fs::write(
        &path,
        r#"{"platforms":{"discord":{"allow_from":" field-user, survey-user, "}}}"#,
    )
    .unwrap();
    let config = load_from_json(&path).unwrap();
    assert_eq!(
        config.platforms["discord"].allowed_users,
        ["field-user", "survey-user"]
    );

    fs::write(
        &path,
        r#"{"platforms":{"discord":{"allowed_users":["observer"],"allow_from":"field-user"}}}"#,
    )
    .unwrap();
    let config = load_from_json(&path).unwrap();
    assert_eq!(config.platforms["discord"].allowed_users, ["observer"]);
}
