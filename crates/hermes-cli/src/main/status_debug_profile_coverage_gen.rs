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

include!("status_debug_profile_part2_coverage_gen.rs");
include!("status_debug_profile_part3_coverage_gen.rs");
