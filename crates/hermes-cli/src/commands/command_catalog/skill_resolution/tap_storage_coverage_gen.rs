use super::{
    read_skill_subscriptions, read_skill_taps, subscription_source_to_tap, write_skill_taps,
};
use serde_json::{json, Value};
use std::fs;

#[test]
fn storage_readers_return_no_entries_for_missing_or_malformed_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("skills.json");

    assert!(read_skill_taps(&path).is_empty());
    assert!(read_skill_subscriptions(&path).is_empty());

    fs::write(
        &path,
        r#"{"taps":["owner/tap"],"subscriptions":["owner/sub"]"#,
    )
    .unwrap();
    assert!(read_skill_taps(&path).is_empty());
    assert!(read_skill_subscriptions(&path).is_empty());
}

#[test]
fn storage_readers_return_no_entries_for_non_array_collections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("skills.json");

    for payload in [
        json!(42),
        json!({"taps": "owner/tap", "subscriptions": {"source": "owner/sub"}}),
    ] {
        fs::write(&path, payload.to_string()).unwrap();
        assert!(read_skill_taps(&path).is_empty(), "payload: {payload}");
        assert!(
            read_skill_subscriptions(&path).is_empty(),
            "payload: {payload}"
        );
    }
}

#[test]
fn legacy_tap_arrays_keep_only_strings_in_their_original_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("taps.json");
    fs::write(
        &path,
        json!([" owner/first ", {"repo": "owner/ignored"}, 12, "owner/last"]).to_string(),
    )
    .unwrap();

    assert_eq!(read_skill_taps(&path), [" owner/first ", "owner/last"]);
}

#[test]
fn wrapped_taps_accept_strings_and_normalize_supported_objects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("taps.json");
    fs::write(
        &path,
        json!({
            "taps": [
                "legacy/skills",
                {"url": "  https://github.com/example/from-url  ", "repo": "ignored/repo"},
                {"tap": "  https://github.com/example/from-alias  "},
                {"repo": " /// "},
                false,
                {"url": " \t ", "repo": " /example/root/ ", "path": " /// "}
            ]
        })
        .to_string(),
    )
    .unwrap();

    assert_eq!(
        read_skill_taps(&path),
        [
            "legacy/skills",
            "https://github.com/example/from-url",
            "https://github.com/example/from-alias",
            "https://github.com/example/root::",
        ]
    );
}

#[test]
fn unparseable_taps_are_stored_as_url_only_objects_and_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("taps.json");
    let taps = ["not-a-tap".to_owned()];

    write_skill_taps(&path, &taps).unwrap();

    let stored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(stored, json!({"taps": [{"url": "not-a-tap"}]}));
    assert_eq!(read_skill_taps(&path), taps);
}

#[test]
fn subscriptions_trim_sources_and_ignore_unsupported_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("subscriptions.json");
    fs::write(
        &path,
        json!({
            "subscriptions": [
                "  example/first  ",
                true,
                {"source": "  example/second  ", "tap": "ignored/repo"},
                null,
                {"url": " \t "},
                {"url": "  https://github.com/example/third  "}
            ]
        })
        .to_string(),
    )
    .unwrap();

    assert_eq!(
        read_skill_subscriptions(&path),
        [
            "example/first",
            "example/second",
            "https://github.com/example/third",
        ]
    );
}

#[test]
fn subscription_taps_accept_repo_shorthand_but_reject_blank_and_registry_sources() {
    assert_eq!(subscription_source_to_tap(" \t\n "), None);
    assert_eq!(subscription_source_to_tap(" Official/skills "), None);
    assert_eq!(subscription_source_to_tap("not-a-tap"), None);
    assert_eq!(
        subscription_source_to_tap("  example/skills  ").as_deref(),
        Some("example/skills")
    );
}
