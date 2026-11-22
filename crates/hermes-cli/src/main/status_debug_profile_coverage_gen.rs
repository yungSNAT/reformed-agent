use super::*;
use std::collections::BTreeMap;

fn cli_for(dir: &Path) -> Cli {
    use clap::Parser;
    let mut cli = Cli::try_parse_from(["hermes"]).expect("default cli parses");
    cli.config_dir = Some(dir.to_str().expect("utf8 temp path").into());
    cli
}

fn config_err(r: Result<(), AgentError>) -> String {
    match r {
        Err(AgentError::Config(m)) => m,
        Err(other) => panic!("expected config error, got {}", other),
        Ok(()) => panic!("expected config error, got Ok"),
    }
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

fn pdir(dir: &Path) -> PathBuf {
    dir.join("profiles")
}

fn write_profile(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = pdir(dir);
    std::fs::create_dir_all(&p).unwrap();
    let f = p.join(format!("{name}.yaml"));
    std::fs::write(&f, body).unwrap();
    f
}

fn read_aliases(dir: &Path) -> BTreeMap<String, String> {
    load_profile_aliases(&profile_aliases_path(&pdir(dir))).unwrap()
}

fn write_aliases(dir: &Path, pairs: &[(&str, &str)]) {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    save_profile_aliases(&profile_aliases_path(&pdir(dir)), &map).unwrap();
}

fn age_file(p: &Path) {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000))
        .unwrap();
}

#[derive(Default)]
struct ProfileArgs {
    name: Option<String>,
    secondary: Option<String>,
    output: Option<String>,
    import_name: Option<String>,
    alias_name: Option<String>,
    remove: bool,
    yes: bool,
    clone: bool,
    clone_all: bool,
    clone_from: Option<String>,
    no_alias: bool,
    no_skills: bool,
}

async fn profile(dir: &Path, action: &str, a: ProfileArgs) -> Result<(), AgentError> {
    run_profile(
        cli_for(dir),
        Some(action.to_string()),
        a.name,
        a.secondary,
        a.output,
        a.import_name,
        a.alias_name,
        a.remove,
        a.yes,
        a.clone,
        a.clone_all,
        a.clone_from,
        a.no_alias,
        a.no_skills,
    )
    .await
}

#[test]
fn prune_old_debug_reports_handles_missing_and_old_files() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        prune_old_debug_reports(&tmp.path().join("missing"), 1).unwrap(),
        0
    );
    let old = tmp.path().join("old.md");
    let new = tmp.path().join("new.md");
    std::fs::write(&old, "old").unwrap();
    std::fs::write(&new, "new").unwrap();
    std::fs::create_dir_all(tmp.path().join("subdir")).unwrap();
    age_file(&old);
    assert_eq!(prune_old_debug_reports(tmp.path(), 1).unwrap(), 1);
    assert!(!old.exists());
    assert!(new.exists());
    assert!(tmp.path().join("subdir").exists());
}

#[test]
fn sweep_pending_pastes_paths() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(sweep_expired_pending_pastes(tmp.path(), 100).unwrap(), 0);

    let store = debug_pending_pastes_path(tmp.path());
    let entries = vec![PendingPasteDelete {
        url: "https://paste.rs/a".into(),
        expires_at_unix: 200,
    }];
    std::fs::write(&store, serde_json::to_string(&entries).unwrap()).unwrap();
    assert_eq!(sweep_expired_pending_pastes(tmp.path(), 100).unwrap(), 0);
    let kept: Vec<PendingPasteDelete> =
        serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
    assert_eq!(kept, entries);

    assert_eq!(sweep_expired_pending_pastes(tmp.path(), 300).unwrap(), 1);
    assert!(!store.exists());
}

#[test]
fn record_pending_paste_writes_and_appends() {
    let tmp = tempfile::tempdir().unwrap();
    let store = debug_pending_pastes_path(tmp.path());
    record_pending_paste(tmp.path(), "   ", 1, 10).unwrap();
    assert!(!store.exists());

    record_pending_paste(tmp.path(), "  https://paste.rs/abc  ", 2, 1000).unwrap();
    record_pending_paste(tmp.path(), "https://paste.rs/def", 1, 1000).unwrap();
    let got: Vec<PendingPasteDelete> =
        serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
    assert_eq!(
        got,
        vec![
            PendingPasteDelete {
                url: "https://paste.rs/abc".into(),
                expires_at_unix: 1000 + 2 * 86_400,
            },
            PendingPasteDelete {
                url: "https://paste.rs/def".into(),
                expires_at_unix: 1000 + 86_400,
            },
        ]
    );

    std::fs::write(&store, "not json").unwrap();
    record_pending_paste(tmp.path(), "https://paste.rs/x", 0, 5).unwrap();
    let got: Vec<PendingPasteDelete> =
        serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].url, "https://paste.rs/x");
    assert_eq!(got[0].expires_at_unix, 5);
}

#[test]
fn capture_snapshot_small_file_reads_everything() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hermes.log");
    std::fs::write(&f, "a\nb\nc\n").unwrap();
    let snap = capture_debug_log_snapshot(&f, 2, 1024);
    assert_eq!(snap.tail_text, "b\nc");
    assert_eq!(snap.full_text.as_deref(), Some("a\nb\nc\n"));
}

#[test]
fn capture_snapshot_directory_reports_error() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("logdir");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("inner.txt"), "x").unwrap();
    let snap = capture_debug_log_snapshot(&d, 5, 1024 * 1024);
    assert!(
        snap.tail_text.starts_with("(error reading") || snap.tail_text == "(file empty)",
        "unexpected: {}",
        snap.tail_text
    );
    assert!(snap.full_text.is_none());
}

#[test]
fn capture_snapshot_truncated_drops_partial_line() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hermes.log");
    let mut body = String::new();
    for k in 1..=100 {
        body.push_str(&format!("line-{:05}\n", k));
    }
    std::fs::write(&f, &body).unwrap();
    let snap = capture_debug_log_snapshot(&f, 50, 100);
    assert!(snap.tail_text.ends_with("line-00100"));
    let full = snap.full_text.expect("full text");
    assert!(full.starts_with("[... truncated"));
    assert!(full.contains("line-00092"));
    assert!(!full.contains("line-00091"));
}

#[test]
fn profile_helper_functions() {
    assert!(matches!(
        validate_profile_name("   "),
        Err(AgentError::Config(m)) if m.contains("cannot be empty")
    ));
    assert_eq!(validate_profile_name(" ok-1 ").unwrap(), "ok-1");

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alt.yml"), "name: alt\n").unwrap();
    assert_eq!(
        resolve_profile_yaml_path(tmp.path(), "alt"),
        Some(tmp.path().join("alt.yml"))
    );
    assert_eq!(resolve_profile_yaml_path(tmp.path(), "nope"), None);

    let aliases: BTreeMap<String, String> = [("x".to_string(), "y".to_string())].into();
    assert_eq!(profile_alias_label(&aliases, "  "), None);
    assert_eq!(
        profile_alias_label(&aliases, "y").as_deref(),
        Some("alias: x")
    );
}

#[tokio::test]
async fn profile_show_named() {
    let tmp = tempfile::tempdir().unwrap();
    let m = config_err(
        profile(
            tmp.path(),
            "show",
            ProfileArgs {
                name: s("ghost"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("not found"));

    write_profile(tmp.path(), "work", "name: work\n");
    write_aliases(tmp.path(), &[("w", "work")]);
    profile(
        tmp.path(),
        "show",
        ProfileArgs {
            name: s("w"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn profile_list_variants() {
    let tmp = tempfile::tempdir().unwrap();
    profile(tmp.path(), "list", ProfileArgs::default())
        .await
        .unwrap();
    assert!(!pdir(tmp.path()).exists());

    std::fs::create_dir_all(pdir(tmp.path())).unwrap();
    profile(tmp.path(), "list", ProfileArgs::default())
        .await
        .unwrap();

    write_profile(tmp.path(), "a", "name: a\n");
    std::fs::write(pdir(tmp.path()).join("b.yml"), "name: b\n").unwrap();
    std::fs::write(pdir(tmp.path()).join("notes.txt"), "x").unwrap();
    write_active_profile_name(&pdir(tmp.path()), "a").unwrap();
    write_aliases(tmp.path(), &[("x", "a")]);
    profile(tmp.path(), "list", ProfileArgs::default())
        .await
        .unwrap();
    assert_eq!(
        read_active_profile_name(&pdir(tmp.path())).as_deref(),
        Some("a")
    );
}

#[tokio::test]
async fn profile_create_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let m = config_err(profile(tmp.path(), "create", ProfileArgs::default()).await);
    assert!(m.contains("Missing profile name"));

    write_profile(tmp.path(), "dup", "name: dup\n");
    let m = config_err(
        profile(
            tmp.path(),
            "create",
            ProfileArgs {
                name: s("dup"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("already exists"));

    let m = config_err(
        profile(
            tmp.path(),
            "create",
            ProfileArgs {
                name: s("fresh"),
                clone: true,
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("requires a source profile"));

    let m = config_err(
        profile(
            tmp.path(),
            "create",
            ProfileArgs {
                name: s("fresh"),
                clone_from: s("missing-src"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("clone source profile 'missing-src' not found"));
    assert!(!pdir(tmp.path()).join("fresh.yaml").exists());
}

#[tokio::test]
async fn profile_create_with_alias_and_clones() {
    let tmp = tempfile::tempdir().unwrap();
    profile(
        tmp.path(),
        "create",
        ProfileArgs {
            name: s("plain"),
            secondary: s(" pl "),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let v = load_profile_yaml(&pdir(tmp.path()).join("plain.yaml")).unwrap();
    assert_eq!(v["model"].as_str(), Some("openai:gpt-5.5"));
    assert_eq!(v["max_turns"].as_u64(), Some(50));
    assert_eq!(
        read_aliases(tmp.path()).get("pl").map(String::as_str),
        Some("plain")
    );

    write_profile(
        tmp.path(),
        "src",
        "name: src\nmodel: src-model\npersonality: p\nmax_turns: 9\ncustom: yes\nskills: [a]\nskill_overrides: {}\n",
    );
    write_active_profile_name(&pdir(tmp.path()), "src").unwrap();
    profile(
        tmp.path(),
        "create",
        ProfileArgs {
            name: s("partial"),
            clone: true,
            alias_name: s("ignored"),
            no_alias: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let v = load_profile_yaml(&pdir(tmp.path()).join("partial.yaml")).unwrap();
    assert_eq!(v["model"].as_str(), Some("src-model"));
    assert_eq!(v["max_turns"].as_u64(), Some(9));
    assert!(v.get("custom").is_none());
    assert!(!read_aliases(tmp.path()).contains_key("ignored"));

    profile(
        tmp.path(),
        "create",
        ProfileArgs {
            name: s("full"),
            clone_all: true,
            no_skills: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let v = load_profile_yaml(&pdir(tmp.path()).join("full.yaml")).unwrap();
    assert_eq!(v["name"].as_str(), Some("full"));
    assert!(v.get("custom").is_some());
    assert!(v.get("skills").is_none());
    assert!(v.get("skill_overrides").is_none());

    write_profile(tmp.path(), "seq", "- a\n- b\n");
    profile(
        tmp.path(),
        "create",
        ProfileArgs {
            name: s("fromseq"),
            clone_from: s("seq"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let v = load_profile_yaml(&pdir(tmp.path()).join("fromseq.yaml")).unwrap();
    assert_eq!(v["personality"].as_str(), Some("default"));
}

#[tokio::test]
async fn profile_use_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let m = config_err(profile(tmp.path(), "use", ProfileArgs::default()).await);
    assert!(m.contains("Missing profile name"));
    let m = config_err(
        profile(
            tmp.path(),
            "switch",
            ProfileArgs {
                name: s("nope"),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(m.contains("not found"));

    write_profile(
        tmp.path(),
        "work",
        "name: work\nmodel: anthropic:claude-x\npersonality: pirate\nmax_turns: 7\n",
    );
    profile(
        tmp.path(),
        "use",
        ProfileArgs {
            name: s("work"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        read_active_profile_name(&pdir(tmp.path())).as_deref(),
        Some("work")
    );
    let Ok(disk) = load_user_config_file(&tmp.path().join("config.yaml")) else {
        panic!("config.yaml should load");
    };
    assert_eq!(disk.personality.as_deref(), Some("pirate"));
    assert_eq!(disk.model.as_deref(), Some("anthropic:claude-x"));
    assert_eq!(disk.max_turns, 7);
}

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
