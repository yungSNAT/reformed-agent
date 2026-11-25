#[tokio::test]
async fn profile_import_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let m = config_err(profile(tmp.path(), "import", ProfileArgs::default()).await);
    assert!(m.contains("profile import usage"));
    let missing = tmp.path().join("nope.yaml");
    let m = config_err(
        profile(
            tmp.path(),
            "import",
            ProfileArgs {
                name: Some(missing.to_str().unwrap().to_string()),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("source not found"));

    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("team.yaml");
    std::fs::write(&src, "name: orig\nmodel: m\n").unwrap();
    let src_str = src.to_str().unwrap().to_string();

    profile(
        tmp.path(),
        "import",
        ProfileArgs {
            name: Some(src_str.clone()),
            alias_name: s(" tm "),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let v = load_profile_yaml(&pdir(tmp.path()).join("team.yaml")).unwrap();
    assert_eq!(v["name"].as_str(), Some("team"));
    assert_eq!(v["model"].as_str(), Some("m"));
    assert_eq!(
        read_aliases(tmp.path()).get("tm").map(String::as_str),
        Some("team")
    );

    let m = config_err(
        profile(
            tmp.path(),
            "import",
            ProfileArgs {
                name: Some(src_str.clone()),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("re-run with -y"));

    profile(
        tmp.path(),
        "import",
        ProfileArgs {
            name: Some(src_str.clone()),
            import_name: s("team"),
            yes: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    profile(
        tmp.path(),
        "import",
        ProfileArgs {
            name: Some(src_str),
            import_name: s("other"),
            secondary: s("zz"),
            no_alias: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(pdir(tmp.path()).join("other.yaml").exists());
    assert!(!read_aliases(tmp.path()).contains_key("zz"));
}

#[tokio::test]
async fn profile_unknown_action_is_ok_and_noop() {
    let tmp = tempfile::tempdir().unwrap();
    profile(tmp.path(), "frobnicate", ProfileArgs::default())
        .await
        .unwrap();
    assert!(!pdir(tmp.path()).exists());
}

#[tokio::test]
async fn logs_missing_and_present() {
    let tmp = tempfile::tempdir().unwrap();
    run_logs(cli_for(tmp.path()), 10, false).await.unwrap();

    let logs = tmp.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("hermes.log"), "one\ntwo\nthree\n").unwrap();
    run_logs(cli_for(tmp.path()), 2, false).await.unwrap();
    run_logs(cli_for(tmp.path()), 100, false).await.unwrap();
}

#[tokio::test]
async fn dashboard_refuses_non_local_without_insecure() {
    let tmp = tempfile::tempdir().unwrap();
    let m =
        config_err(run_dashboard(cli_for(tmp.path()), " 0.0.0.0 ".into(), 8080, true, false).await);
    assert!(m.contains("--insecure"));
    assert!(!tmp.path().join("config.yaml").exists());
}

#[tokio::test]
async fn debug_actions() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = cli_for(tmp.path());
    let reports = debug_reports_dir_for_cli(&cli);
    if !reports.starts_with(tmp.path()) {
        return;
    }

    run_debug(cli_for(tmp.path()), Some("list".into()), None, 5, 30, true)
        .await
        .unwrap();
    assert!(reports.exists());

    let keep = reports.join("keep.md");
    std::fs::write(&keep, "x").unwrap();
    run_debug(cli_for(tmp.path()), Some("list".into()), None, 5, 30, true)
        .await
        .unwrap();
    assert!(keep.exists());

    let m = config_err(
        run_debug(
            cli_for(tmp.path()),
            Some("delete".into()),
            None,
            5,
            30,
            true,
        )
        .await,
    );
    assert!(m.contains("debug delete requires"));

    let target = tmp.path().join("report.md");
    std::fs::write(&target, "r").unwrap();
    run_debug(
        cli_for(tmp.path()),
        Some("delete".into()),
        Some(format!("file://{}", target.display())),
        5,
        30,
        true,
    )
    .await
    .unwrap();
    assert!(!target.exists());
    run_debug(
        cli_for(tmp.path()),
        Some("delete".into()),
        Some(target.to_str().unwrap().to_string()),
        5,
        30,
        true,
    )
    .await
    .unwrap();

    let m =
        config_err(run_debug(cli_for(tmp.path()), Some("bogus".into()), None, 5, 30, true).await);
    assert!(m.contains("Unknown debug action 'bogus'"));
}

#[tokio::test]
async fn debug_share_local_prunes_and_writes_report() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = cli_for(tmp.path());
    let reports = debug_reports_dir_for_cli(&cli);
    if !reports.starts_with(tmp.path()) {
        return;
    }
    let pid_path = gateway_pid_path_for_cli(&cli);
    if pid_path.starts_with(tmp.path()) {
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();
    }
    std::fs::create_dir_all(&reports).unwrap();
    let old = reports.join("old-debug-report.md");
    std::fs::write(&old, "old").unwrap();
    age_file(&old);
    let store = debug_pending_pastes_path(&reports);
    std::fs::write(
        &store,
        serde_json::to_string(&vec![PendingPasteDelete {
            url: "https://paste.rs/old".into(),
            expires_at_unix: 1,
        }])
        .unwrap(),
    )
    .unwrap();

    run_debug(cli, Some("share".into()), None, 5, 1, true)
        .await
        .unwrap();
    assert!(!old.exists());
    assert!(!store.exists());
    let reports_written: Vec<PathBuf> = std::fs::read_dir(&reports)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().ends_with("-debug-report.md"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(reports_written.len(), 1);
    let body = std::fs::read_to_string(&reports_written[0]).unwrap();
    assert!(body.starts_with("# Hermes Debug Report"));
    assert!(body.contains("- gateway_pid:"));
}

#[test]
fn status_runs_with_route_health_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = cli_for(tmp.path());
    let rh = route_health_state_path_for_cli(&cli);
    if rh.starts_with(tmp.path()) {
        if let Some(parent) = rh.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&rh, "{ not json").unwrap();
        run_status(cli_for(tmp.path())).unwrap();
        std::fs::write(
            &rh,
            r#"{"summary":{"overall":"healthy","entries":3,"average_score":0.75}}"#,
        )
        .unwrap();
        run_status(cli_for(tmp.path())).unwrap();
        assert!(rh.exists());
    } else {
        run_status(cli).unwrap();
    }
}
