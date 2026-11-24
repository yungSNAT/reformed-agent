use super::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn bundle_slug_discards_punctuation_and_collapses_separators() {
    assert_eq!(
        slugify_skill_bundle_name("  __Rust@Lang --- QUICK_Start!__  "),
        "rustlang-quick-start"
    );
    assert!(slugify_skill_bundle_name(" -_é!? ").is_empty());
}

#[test]
fn bundle_discovery_filters_invalid_entries_and_sorts_normalized_slugs() {
    let root = tempdir().unwrap();
    assert!(list_skill_bundles(root.path()).is_empty());

    let dir = root.path().join("skill-bundles");
    fs::create_dir(&dir).unwrap();
    fs::create_dir(dir.join("unreadable.yaml")).unwrap();

    for (name, contents) in [
        ("extensionless", "name: Extensionless\nskills: [one]\n"),
        ("ignored.txt", "name: Ignored\nskills: [one]\n"),
        ("malformed.yml", "skills: [\n"),
        ("empty-slug.yaml", "name: '!?'\nskills: [one]\n"),
        (
            "empty-skills.yaml",
            "name: Empty skills\nskills: [' ', '']\n",
        ),
        (
            "Zulu_Pack.yml",
            "name: '  '\ndescription: ' '\nskills: ['  inspect  ', '', 'report']\n",
        ),
        (
            "alpha.yaml",
            "name: ' Alpha Pack! '\ndescription: '  Starter tools  '\nskills: [inspect]\n",
        ),
        (
            "duplicate.yml",
            "name: alpha_pack\ndescription: Starter tools\nskills: [inspect]\n",
        ),
    ] {
        fs::write(dir.join(name), contents).unwrap();
    }

    let bundles = list_skill_bundles(root.path());
    let [alpha, zulu] = bundles.as_slice() else {
        panic!("expected two bundles, got {bundles:?}");
    };
    assert_eq!(alpha.slug, "alpha-pack");
    assert_eq!(alpha.description, "Starter tools");
    assert_eq!(alpha.skills, ["inspect"]);
    assert_eq!(zulu.slug, "zulu-pack");
    assert_eq!(zulu.description, "Load 2 skills as a bundle");
    assert_eq!(zulu.skills, ["inspect", "report"]);
}

#[cfg(unix)]
#[test]
fn bundle_discovery_falls_back_when_the_file_stem_is_not_utf8() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let root = tempdir().unwrap();
    let dir = root.path().join("skill-bundles");
    fs::create_dir(&dir).unwrap();
    fs::write(
        dir.join(OsStr::from_bytes(b"\xff.yaml")),
        "skills: [inspect]\n",
    )
    .unwrap();

    let bundles = list_skill_bundles(root.path());
    let [bundle] = bundles.as_slice() else {
        panic!("expected one bundle, got {bundles:?}");
    };
    assert_eq!(bundle.slug, "bundle");
    assert_eq!(bundle.skills, ["inspect"]);
}

#[test]
fn empty_mcp_status_distinguishes_missing_and_present_files() {
    let root = tempdir().unwrap();
    let path = root.path().join("mcp_servers.json");
    for status in ["missing", "present"] {
        if status == "present" {
            fs::write(&path, "{}").unwrap();
        }
        assert_eq!(
            render_mcp_runtime_status(&[], None, &path),
            format!(
                "No MCP servers configured.\n  config.yaml entries: 0\n  mcp_servers.json: {status} ({})\nAdd one with `hermes mcp add <name> --url <url>` or `hermes mcp add <name> --command <cmd>`.",
                path.display()
            )
        );
    }
}

#[test]
fn mcp_status_reports_parallel_yaml_server_and_keepalive() {
    let root = tempdir().unwrap();
    let entry: hermes_config::McpServerEntry = serde_yaml::from_str(
        "name: metrics\ncommand: metrics-client\nargs: []\nenv: {}\nsupports_parallel_tool_calls: true\nkeepalive_interval: 17\n",
    )
    .unwrap();
    let status = render_mcp_runtime_status(&[entry], None, &root.path().join("mcp_servers.json"));

    assert!(
        status.contains(
            "metrics-client  [source:config.yaml; parallel_tool_calls:on; keepalive:17s]"
        ),
        "{status}"
    );
    assert!(status.ends_with("Drift: config_only=[metrics] json_only=[]"));
}

#[test]
fn memory_status_distinguishes_legacy_files_and_disabled_precedence() {
    let root = tempdir().unwrap();
    let home = root.path();
    let legacy = home.join("memory.db");
    fs::write(&legacy, [0_u8; 2560]).unwrap();
    assert_eq!(
        render_memory_backend_status(home),
        format!(
            "Memory provider: legacy sqlite artifact only\n  File: {}\n  Size: 2 KB\nRun `hermes memory setup` to initialize the current file backend.",
            legacy.display()
        )
    );

    let memories = home.join("memories");
    fs::create_dir(&memories).unwrap();
    let user = memories.join("USER.md");
    fs::write(&user, [b'x'; 1536]).unwrap();
    assert_eq!(
        render_memory_backend_status(home),
        format!(
            "Memory provider: files (MEMORY.md + USER.md)\n  Directory: {}\n  MEMORY.md: {} (0.0 KB)\n  USER.md:   {} (1.5 KB)\n  Legacy file detected (unused by current memory backend): {}",
            memories.display(),
            memories.join("MEMORY.md").display(),
            user.display(),
            legacy.display()
        )
    );

    let marker = home.join(".memory_disabled");
    fs::write(&marker, "").unwrap();
    assert_eq!(
        render_memory_backend_status(home),
        format!(
            "Memory provider: disabled\n  Marker: {}\nRun `hermes memory setup` to re-enable.",
            marker.display()
        )
    );
}

fn write_skill(root: &Path, relative_dir: &str, contents: &str) {
    let dir = root.join(relative_dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), contents).unwrap();
}

#[test]
fn local_skill_search_and_display_use_names_paths_and_headings() {
    let root = tempdir().unwrap();
    let skills = root.path().join("skills");
    assert!(collect_local_skill_summaries(&skills).is_empty());

    write_skill(&skills, "basic", "Plain text without a heading.\n");
    write_skill(
        &skills,
        "guides/reference",
        "---\nname: '  zeta-guide  '\ndescription: ''\n---\n##  Reference guide  \n",
    );
    write_skill(&skills, ".hidden", "# Should not be listed\n");
    write_skill(&skills, "basic/child", "# Not a separate skill\n");

    let basic_md = skills.join("basic").join("SKILL.md");
    let reference_md = skills.join("guides").join("reference").join("SKILL.md");
    let summaries = collect_local_skill_summaries(&skills);
    assert_eq!(
        summaries,
        [
            LocalSkillSummary {
                name: "basic".to_string(),
                title: "(no description)".to_string(),
                relative_dir: "basic".to_string(),
                skill_md: basic_md,
            },
            LocalSkillSummary {
                name: "zeta-guide".to_string(),
                title: "Reference guide".to_string(),
                relative_dir: "guides/reference".to_string(),
                skill_md: reference_md.clone(),
            },
        ]
    );
    assert_eq!(format_skill_display_name(&summaries[0]), "basic");
    assert_eq!(
        format_skill_display_name(&summaries[1]),
        "zeta-guide (guides/reference)"
    );
    for query in [
        "  /zeta-guide  ",
        "guides/reference",
        "reference",
        "ZETA-GUIDE",
        "/GUIDES/REFERENCE",
    ] {
        assert_eq!(
            find_local_skill_markdown(&skills, query).as_deref(),
            Some(reference_md.as_path()),
            "query: {query}"
        );
    }
    for query in ["  ///  ", "missing", ".hidden"] {
        assert_eq!(find_local_skill_markdown(&skills, query), None);
    }
}

#[cfg(unix)]
#[test]
fn local_skill_discovery_excludes_broken_and_escaping_links() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let skills = root.path().join("skills");
    let targets = root.path().join("targets");
    fs::create_dir(&skills).unwrap();
    write_skill(&targets, "contained", "# Contained skill\n");
    write_skill(&targets, "outside", "# Outside skill\n");
    fs::create_dir(targets.join("escaped")).unwrap();
    symlink(
        targets.join("outside").join("SKILL.md"),
        targets.join("escaped").join("SKILL.md"),
    )
    .unwrap();
    symlink(targets.join("contained"), skills.join("linked")).unwrap();
    symlink(targets.join("escaped"), skills.join("escaping")).unwrap();
    symlink(targets.join("missing"), skills.join("broken")).unwrap();

    let summaries = collect_local_skill_summaries(&skills);
    assert_eq!(
        summaries,
        [LocalSkillSummary {
            name: "linked".to_string(),
            title: "Contained skill".to_string(),
            relative_dir: "linked".to_string(),
            skill_md: skills.join("linked").join("SKILL.md"),
        }]
    );
}

#[test]
fn frontmatter_ignores_empty_values_and_values_outside_the_header() {
    assert_eq!(frontmatter_value("name: body only", "name"), None);
    assert_eq!(
        frontmatter_value("---\nname: '  '\n---\nname: body only\n", "name"),
        None
    );
    assert_eq!(
        frontmatter_value("---\nname: ''\nname: \"  actual name  \"\n---\n", "name").as_deref(),
        Some("actual name")
    );
}

#[test]
fn forward_slash_path_omits_non_normal_components() {
    assert_eq!(
        path_to_forward_slashes(Path::new("./group/../reference")),
        "group/reference"
    );
}

#[test]
fn blueprint_delivery_defaults_to_origin_and_normalizes_destination() {
    let default_delivery = blueprint_deliver_config(" \t ").unwrap();
    assert!(matches!(default_delivery.target, DeliverTarget::Origin));
    assert_eq!(default_delivery.platform, None);

    let platform_delivery = blueprint_deliver_config(" Home_Assistant :  kitchen  ").unwrap();
    assert!(matches!(
        platform_delivery.target,
        DeliverTarget::HomeAssistant
    ));
    assert_eq!(platform_delivery.platform.as_deref(), Some("kitchen"));
    assert_eq!(blueprint_deliver_config("local:  ").unwrap().platform, None);
    assert!(blueprint_deliver_config("unsupported: room").is_none());
}

#[test]
fn suggestion_job_copies_skills_and_rejects_unsupported_delivery() {
    let mut spec: SuggestionJobSpec = serde_json::from_value(serde_json::json!({
        "name": "Release digest",
        "schedule": "15 9 * * 1-5",
        "prompt": "Summarize release notes.",
        "skills": ["release-notes", "summarize"],
        "deliver": "Slack: releases"
    }))
    .unwrap();

    let job = cron_job_from_suggestion_spec(&spec).unwrap();
    assert_eq!(job.name.as_deref(), Some("Release digest"));
    assert_eq!(job.skills.unwrap(), ["release-notes", "summarize"]);
    let delivery = job.deliver.unwrap();
    assert!(matches!(delivery.target, DeliverTarget::Slack));
    assert_eq!(delivery.platform.as_deref(), Some("releases"));

    spec.deliver = "  unsupported: releases  ".to_string();
    assert_eq!(
        cron_job_from_suggestion_spec(&spec).err().as_deref(),
        Some("unsupported deliver target `unsupported: releases`")
    );
}

#[test]
fn suggestions_usage_documents_the_available_actions() {
    assert_eq!(
        render_suggestions_usage(),
        concat!(
            "Usage:\n",
            "  /suggestions              list pending\n",
            "  /suggestions accept N     schedule suggestion N\n",
            "  /suggestions dismiss N    dismiss suggestion N\n",
            "  /suggestions catalog      add curated starter automations\n",
            "  /suggestions clear        housekeeping"
        )
    );
}

#[test]
fn delegation_env_truthy_requires_a_recognized_whole_value() {
    for raw in ["1", " TRUE \t", "YeS", "\nOn"] {
        assert!(env_truthy(raw), "expected {raw:?} to be true");
    }
    for raw in ["", "0", "off", "false", "true-ish", "1 1"] {
        assert!(!env_truthy(raw), "expected {raw:?} to be false");
    }
}

// Run tests of home-dependent functions in a separate test process so other
// tests in this binary cannot observe a changed environment.
fn with_isolated_home(test: &str, home_is_file: bool, check: impl FnOnce(&Path)) {
    const CHILD_TEST: &str = "HERMES_SKILLS_MEMORY_CRON_COVERAGE_CHILD";
    const COMPLETED: &str = "isolated Hermes home assertions completed";

    if std::env::var(CHILD_TEST).ok().as_deref() == Some(test) {
        let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
        assert_eq!(hermes_config::hermes_home(), home);
        check(&home);
        println!("{COMPLETED}");
        return;
    }

    let sandbox = tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    if home_is_file {
        fs::write(&home, "not a directory").unwrap();
    } else {
        fs::create_dir(&home).unwrap();
    }
    let (_, module) = module_path!().split_once("::").unwrap();
    let filter = format!("{module}::{test}");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
    command
        .args(["--exact", filter.as_str(), "--nocapture"])
        .env_clear()
        .env(CHILD_TEST, test)
        .env("HERMES_HOME", &home)
        .env("HOME", sandbox.path())
        .env("USERPROFILE", sandbox.path())
        .env("APPDATA", sandbox.path())
        .env("LOCALAPPDATA", sandbox.path())
        .current_dir(sandbox.path());
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    // Resolve relative profile paths against the parent working directory.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env(
            "LLVM_PROFILE_FILE",
            std::env::current_dir().unwrap().join(profile),
        );
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(COMPLETED),
        "isolated test {filter} did not complete ({})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
}

#[test]
fn background_status_and_audit_handle_mixed_manifests() {
    with_isolated_home(
        "background_status_and_audit_handle_mixed_manifests",
        false,
        |home| {
            assert!(background_status_rows().is_empty());
            let jobs = home.join("background_jobs");
            fs::create_dir(&jobs).unwrap();
            fs::create_dir(jobs.join("unreadable.json")).unwrap();

            for (name, contents) in [
                (
                    "a.json",
                    r#"{"id":"zulu","status":"completed","task":"done"}"#,
                ),
                (
                    "m.json",
                    r#"{"id":"alpha","status":"running","task":"fresh"}"#,
                ),
                (
                    "z.json",
                    r#"{"id":"alpha","status":"RUNNING","task":"first\nsecond"}"#,
                ),
                ("defaults.json", r#"{"id":4,"status":null,"task":false}"#),
                ("malformed.json", "{"),
                ("ignored.txt", r#"{"id":"ignored","status":"running"}"#),
            ] {
                fs::write(jobs.join(name), contents).unwrap();
            }
            let old = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
            fs::OpenOptions::new()
                .write(true)
                .open(jobs.join("z.json"))
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();

            assert_eq!(
                background_status_rows(),
                [
                    "alpha  [RUNNING]  first second",
                    "alpha  [running]  fresh",
                    "unknown  [unknown]  ",
                    "zulu  [completed]  done",
                ]
            );
            assert_eq!(
            render_background_queue_audit(&audit_background_queue_manifests()),
            format!(
                "Queue manifest audit (native)\n  dir: {}\n  json=6 valid=4 malformed=2 running=2 stale_running=1 duplicate_ids=1",
                jobs.display()
            )
        );
        },
    );
}

#[test]
fn suggestion_store_error_keeps_its_context() {
    with_isolated_home("suggestion_store_error_keeps_its_context", true, |_| {
        let error = hermes_cron::SuggestionStore::default()
            .seed_catalog_suggestions()
            .expect_err("the store cannot write under a regular file");
        let expected = format!("suggestions: {error}");
        assert!(matches!(
            suggestion_error(error),
            AgentError::Config(message) if message == expected
        ));
    });
}
