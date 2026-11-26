#[tokio::test]
async fn profile_delete_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let m = config_err(profile(tmp.path(), "delete", ProfileArgs::default()).await);
    assert!(m.contains("Missing profile name"));
    let m = config_err(
        profile(
            tmp.path(),
            "delete",
            ProfileArgs {
                name: s("nope"),
                yes: true,
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("not found"));

    let path = write_profile(tmp.path(), "gone", "name: gone\n");
    write_profile(tmp.path(), "keep", "name: keep\n");
    write_aliases(tmp.path(), &[("a", "gone"), ("b", "keep")]);
    write_active_profile_name(&pdir(tmp.path()), "gone").unwrap();
    profile(
        tmp.path(),
        "delete",
        ProfileArgs {
            name: s("a"),
            yes: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!path.exists());
    let aliases = read_aliases(tmp.path());
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases.get("b").map(String::as_str), Some("keep"));
    assert_eq!(read_active_profile_name(&pdir(tmp.path())), None);
}

#[tokio::test]
async fn profile_alias_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let m = config_err(
        profile(
            tmp.path(),
            "alias",
            ProfileArgs {
                remove: true,
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("--remove"));

    write_aliases(tmp.path(), &[("x", "t")]);
    profile(
        tmp.path(),
        "alias",
        ProfileArgs {
            remove: true,
            name: s("missing"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(read_aliases(tmp.path()).contains_key("x"));
    profile(
        tmp.path(),
        "alias",
        ProfileArgs {
            remove: true,
            alias_name: s(" x "),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(read_aliases(tmp.path()).is_empty());

    let m = config_err(profile(tmp.path(), "alias", ProfileArgs::default()).await);
    assert!(m.contains("profile alias usage"));
    let m = config_err(
        profile(
            tmp.path(),
            "alias",
            ProfileArgs {
                name: s("t"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("profile alias usage"));
    let m = config_err(
        profile(
            tmp.path(),
            "alias",
            ProfileArgs {
                name: s("t"),
                secondary: s("tt"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("Alias target profile 't' not found"));

    write_profile(tmp.path(), "t", "name: t\n");
    profile(
        tmp.path(),
        "alias",
        ProfileArgs {
            name: s("t"),
            alias_name: s(" tt "),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        read_aliases(tmp.path()).get("tt").map(String::as_str),
        Some("t")
    );
}

#[tokio::test]
async fn profile_rename_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let m = config_err(profile(tmp.path(), "rename", ProfileArgs::default()).await);
    assert!(m.contains("rename usage"));
    let m = config_err(
        profile(
            tmp.path(),
            "rename",
            ProfileArgs {
                name: s("old"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("rename usage"));
    let m = config_err(
        profile(
            tmp.path(),
            "rename",
            ProfileArgs {
                name: s("old"),
                secondary: s("new"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("Profile 'old' not found"));

    write_profile(tmp.path(), "old", "name: old\nmodel: m\n");
    write_profile(tmp.path(), "taken", "name: taken\n");
    let m = config_err(
        profile(
            tmp.path(),
            "rename",
            ProfileArgs {
                name: s("old"),
                secondary: s("taken"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("already exists"));

    write_aliases(tmp.path(), &[("old", "old"), ("x", "old")]);
    write_active_profile_name(&pdir(tmp.path()), "old").unwrap();
    profile(
        tmp.path(),
        "rename",
        ProfileArgs {
            name: s("old"),
            secondary: s("new"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!pdir(tmp.path()).join("old.yaml").exists());
    let v = load_profile_yaml(&pdir(tmp.path()).join("new.yaml")).unwrap();
    assert_eq!(v["name"].as_str(), Some("new"));
    assert_eq!(v["model"].as_str(), Some("m"));
    let aliases = read_aliases(tmp.path());
    assert!(!aliases.contains_key("old"));
    assert_eq!(aliases.get("new").map(String::as_str), Some("new"));
    assert_eq!(aliases.get("x").map(String::as_str), Some("new"));
    assert_eq!(
        read_active_profile_name(&pdir(tmp.path())).as_deref(),
        Some("new")
    );
}

#[tokio::test]
async fn profile_export_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let m = config_err(profile(tmp.path(), "export", ProfileArgs::default()).await);
    assert!(m.contains("no active profile"));
    let m = config_err(
        profile(
            tmp.path(),
            "export",
            ProfileArgs {
                name: s("nope"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("Profile 'nope' not found"));

    write_profile(tmp.path(), "work", "name: work\nmodel: m\n");
    let out = tmp.path().join("out.yaml");
    profile(
        tmp.path(),
        "export",
        ProfileArgs {
            name: s("work"),
            output: Some(out.to_str().unwrap().to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "name: work\nmodel: m\n"
    );

    write_active_profile_name(&pdir(tmp.path()), "work").unwrap();
    let out2 = tmp.path().join("out2.yaml");
    profile(
        tmp.path(),
        "export",
        ProfileArgs {
            output: Some(out2.to_str().unwrap().to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(out2.exists());
}
