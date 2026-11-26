use std::time::Instant;

use super::*;

fn changes(pairs: &[(&str, Option<&str>)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.map_or(Value::Null, |v| json!(v))))
        .collect()
}

fn store(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join(PREFERENCES_FILE)
}

// ---- UI preferences -------------------------------------------------------------------------

#[test]
fn a_missing_store_reads_as_none_without_a_generation() {
    let dir = tempfile::tempdir().unwrap();
    let snap = read_preferences(&store(&dir)).unwrap();
    assert_eq!(
        snap.to_json(),
        json!({"exists": false, "revision": 0, "values": {}})
    );
}

#[test]
fn a_stored_document_keeps_only_string_values_and_reads_past_a_bom() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        store(&dir),
        "\u{feff}{\"version\":1,\"generation\":\"g1\",\"revision\":4,\"values\":{\"hermes.desktop.a\":\"x\",\"hermes.desktop.n\":3}}",
    )
    .unwrap();
    let snap = read_preferences(&store(&dir)).unwrap();
    assert_eq!(
        snap.to_json(),
        json!({"exists": true, "revision": 4, "values": {"hermes.desktop.a": "x"}, "generation": "g1"})
    );
}

#[test]
fn an_unparseable_store_reads_as_none_and_a_write_keeps_it_aside() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(store(&dir), "{not json").unwrap();
    let snap = read_preferences(&store(&dir)).unwrap();
    assert!(!snap.exists && snap.corrupt);
    let out = write_preferences(
        &store(&dir),
        &changes(&[("hermes.desktop.a", Some("1"))]),
        false,
    )
    .unwrap();
    assert_eq!(out["revision"], json!(1));
    let aside: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("ui-preferences.json.corrupt-")
        })
        .collect();
    assert_eq!(aside.len(), 1);
    assert_eq!(
        std::fs::read_to_string(aside[0].path()).unwrap(),
        "{not json"
    );
}

#[test]
fn a_write_merges_per_key_bumps_the_revision_and_keeps_the_generation() {
    let dir = tempfile::tempdir().unwrap();
    let first = write_preferences(
        &store(&dir),
        &changes(&[
            ("hermes.desktop.a", Some("1")),
            ("hermes-desktop-b", Some("2")),
        ]),
        false,
    )
    .unwrap();
    let generation = first["generation"].as_str().unwrap().to_owned();
    assert_eq!(generation.len(), 16);
    let second = write_preferences(
        &store(&dir),
        &changes(&[
            ("hermes.desktop.a", None),
            ("hermes-onboarding-c", Some("3")),
        ]),
        false,
    )
    .unwrap();
    assert_eq!(
        second,
        json!({"exists": true, "revision": 2, "generation": generation,
               "values": {"hermes-desktop-b": "2", "hermes-onboarding-c": "3"}})
    );
    let doc: Value = serde_json::from_slice(&std::fs::read(store(&dir)).unwrap()).unwrap();
    assert_eq!(doc["version"], json!(1));
    assert_eq!(doc["revision"], json!(2));
}

#[test]
fn a_seed_write_only_adds_missing_keys() {
    let dir = tempfile::tempdir().unwrap();
    write_preferences(
        &store(&dir),
        &changes(&[("hermes.desktop.a", Some("host"))]),
        false,
    )
    .unwrap();
    let out = write_preferences(
        &store(&dir),
        &changes(&[
            ("hermes.desktop.a", Some("browser")),
            ("hermes.desktop.b", Some("new")),
            ("hermes.desktop.c", None),
        ]),
        true,
    )
    .unwrap();
    assert_eq!(
        out["values"],
        json!({"hermes.desktop.a": "host", "hermes.desktop.b": "new"})
    );
}

#[test]
fn a_seed_or_no_op_write_creates_no_store_from_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let out =
        write_preferences(&store(&dir), &changes(&[("hermes.desktop.a", None)]), true).unwrap();
    assert_eq!(out, json!({"exists": false, "revision": 0, "values": {}}));
    assert!(!store(&dir).exists());
}

#[test]
fn keys_outside_the_desktop_prefixes_and_oversized_values_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    for bad in [
        changes(&[("token", Some("x"))]),
        changes(&[(
            &format!("hermes.desktop.{}", "k".repeat(MAX_KEY_CHARS)),
            Some("x"),
        )]),
        changes(&[("hermes.desktop.a", Some(&"v".repeat(MAX_VALUE_CHARS + 1)))]),
    ] {
        assert!(matches!(
            write_preferences(&store(&dir), &bad, false),
            Err(PrefsError::Refused(_))
        ));
    }
    let mut not_a_string = Map::new();
    not_a_string.insert("hermes.desktop.a".into(), json!(1));
    assert!(matches!(
        write_preferences(&store(&dir), &not_a_string, false),
        Err(PrefsError::Refused(_))
    ));
    assert!(!store(&dir).exists());
}

#[test]
fn a_store_past_its_key_limit_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let many: Map<String, Value> = (0..=MAX_KEYS)
        .map(|i| (format!("hermes.desktop.k{i}"), json!("v")))
        .collect();
    assert!(matches!(
        write_preferences(&store(&dir), &many, false),
        Err(PrefsError::Refused(detail)) if detail == "preferences store is full"
    ));
}

#[test]
fn a_held_lock_makes_the_write_busy_and_leaves_the_holders_lock() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("ui-preferences.json.lock");
    std::fs::create_dir(&lock).unwrap();
    std::fs::write(lock.join("owner"), "other").unwrap();
    let started = Instant::now();
    let out = write_preferences(
        &store(&dir),
        &changes(&[("hermes.desktop.a", Some("1"))]),
        false,
    );
    assert!(matches!(out, Err(PrefsError::Busy(_))));
    assert!(started.elapsed() >= super::super::store::TEST_LOCK_WAIT);
    assert_eq!(
        std::fs::read_to_string(lock.join("owner")).unwrap(),
        "other"
    );
    assert!(!store(&dir).exists());
}

#[test]
fn a_write_releases_its_lock() {
    let dir = tempfile::tempdir().unwrap();
    write_preferences(
        &store(&dir),
        &changes(&[("hermes.desktop.a", Some("1"))]),
        false,
    )
    .unwrap();
    assert!(!dir.path().join("ui-preferences.json.lock").exists());
}

#[cfg(unix)]
#[test]
fn a_new_store_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    write_preferences(
        &store(&dir),
        &changes(&[("hermes.desktop.a", Some("1"))]),
        false,
    )
    .unwrap();
    let mode = std::fs::metadata(store(&dir)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[test]
fn an_unreadable_store_is_busy_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    // A folder where the file should be: reading it fails with something other than 'not found'.
    std::fs::create_dir(store(&dir)).unwrap();
    assert!(matches!(
        read_preferences(&store(&dir)),
        Err(PrefsError::Busy(_))
    ));
}

#[test]
fn the_machine_root_climbs_out_of_a_profile_home_only() {
    use crate::webapp::routes::store::install_root;
    let root = Path::new("/r");
    assert_eq!(install_root(root), root);
    assert_eq!(install_root(&root.join("profiles").join("x")), root);
    assert_eq!(
        install_root(&root.join("other").join("x")),
        root.join("other").join("x")
    );
}

// ---- Desktop plugins ------------------------------------------------------------------------

fn plugins(dir: &tempfile::TempDir) -> PathBuf {
    let root = plugins_root(dir.path()).unwrap();
    std::fs::create_dir(root.join("beta")).unwrap();
    std::fs::create_dir(root.join("alpha")).unwrap();
    std::fs::write(root.join("alpha").join("plugin.js"), "x").unwrap();
    std::fs::write(root.join("notes.txt"), "x").unwrap();
    root
}

#[test]
fn the_plugins_root_is_made_on_demand_under_the_machine_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = plugins_root(dir.path()).unwrap();
    assert!(root.is_dir());
    assert_eq!(
        root,
        std::fs::canonicalize(dir.path().join("desktop-plugins")).unwrap()
    );
}

#[test]
fn the_root_and_a_plugin_folder_list_their_entries_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let root = plugins(&dir);
    let listed = list_plugins(&root, &root.to_string_lossy()).unwrap();
    let names: Vec<_> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_owned(),
                e["isDirectory"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("alpha".into(), true),
            ("beta".into(), true),
            ("notes.txt".into(), false)
        ]
    );
    let alpha = root.join("alpha");
    let inner = list_plugins(&root, &alpha.to_string_lossy()).unwrap();
    assert_eq!(inner["entries"][0]["name"], json!("plugin.js"));
    assert_eq!(
        inner["entries"][0]["path"],
        json!(alpha.join("plugin.js").to_string_lossy())
    );
}

#[test]
fn paths_outside_or_below_a_plugin_folder_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = plugins(&dir);
    let refused = [
        dir.path().to_string_lossy().into_owned(),
        root.join("..").to_string_lossy().into_owned(),
        root.join("alpha")
            .join("..")
            .join("..")
            .to_string_lossy()
            .into_owned(),
        root.join("alpha")
            .join("plugin.js")
            .to_string_lossy()
            .into_owned(),
        root.join("alpha")
            .join("x")
            .join("y")
            .to_string_lossy()
            .into_owned(),
        root.join("missing").to_string_lossy().into_owned(),
        root.join("notes.txt").to_string_lossy().into_owned(),
        "relative/path".to_owned(),
    ];
    for raw in refused {
        let err = list_plugins(&root, &raw).unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND, "{raw}");
    }
}

#[cfg(unix)]
#[test]
fn a_linked_plugin_folder_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = plugins(&dir);
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("plugin.js"), "x").unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), root.join("linked")).unwrap();
    let listed = list_plugins(&root, &root.join("linked").to_string_lossy()).unwrap();
    assert_eq!(listed["entries"][0]["name"], json!("plugin.js"));
}

// ---- Default working folder -----------------------------------------------------------------

#[test]
fn the_configured_terminal_folder_is_the_default_cwd() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        format!("terminal:\n  cwd: '{}'\n", work.path().display()),
    )
    .unwrap();
    assert_eq!(
        default_cwd_for(home.path()),
        std::fs::canonicalize(work.path()).unwrap()
    );
}

#[test]
fn a_placeholder_or_missing_terminal_folder_falls_through() {
    let home = tempfile::tempdir().unwrap();
    for cwd in [".", "auto", "cwd", "/no/such/folder/anywhere"] {
        std::fs::write(
            home.path().join("config.yaml"),
            format!("terminal:\n  cwd: '{cwd}'\n"),
        )
        .unwrap();
        let got = default_cwd_for(home.path());
        assert_ne!(got, PathBuf::from(cwd));
        assert!(
            got.is_dir() || got == dirs_home(),
            "{cwd} gave {}",
            got.display()
        );
    }
}

#[test]
fn a_folder_outside_any_repository_has_no_branch() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(branch_of(dir.path()), "");
}
