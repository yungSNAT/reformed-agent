use super::*;
use serde_json::json;
use std::fs;

fn assert_config_error<T: std::fmt::Debug>(result: Result<T, AgentError>, prefix: &str) {
    match result.expect_err("expected a configuration error") {
        AgentError::Config(message) => assert!(message.starts_with(prefix), "{message}"),
        error => panic!("expected a configuration error, got {error:?}"),
    }
}

fn assert_io_error<T: std::fmt::Debug>(result: Result<T, AgentError>, prefix: &str) {
    match result.expect_err("expected an I/O error") {
        AgentError::Io(message) => assert!(message.starts_with(prefix), "{message}"),
        error => panic!("expected an I/O error, got {error:?}"),
    }
}

fn index_entry(value: serde_json::Value) -> HermesSkillsIndexEntry {
    serde_json::from_value(value).expect("valid index fixture")
}

#[test]
fn masks_empty_short_and_multibyte_secrets() {
    if run_isolated("masks_empty_short_and_multibyte_secrets") {
        return;
    }

    assert_eq!(mask_secret_value(""), "(empty)");
    assert_eq!(mask_secret_value("a"), "*");
    assert_eq!(mask_secret_value("12345678"), "********");
    assert_eq!(mask_secret_value("é"), "**");
    assert_eq!(mask_secret_value("éé-middle-🦀"), "éé***🦀");
}

#[test]
fn parses_tap_paths_and_rejects_incomplete_repositories() {
    if run_isolated("parses_tap_paths_and_rejects_incomplete_repositories") {
        return;
    }

    for invalid in [" \t", "owner", "https://github.com/owner/"] {
        assert_eq!(parse_skill_tap_spec(invalid), None, "{invalid:?}");
    }

    for (input, path) in [
        (" owner/repo ", "skills"),
        ("owner//repo/library/agents/", "library/agents"),
        (
            "http://github.com/owner/repo/tree/dev/library/agents",
            "library/agents",
        ),
        ("owner/repo/ignored :: /custom/ ", "custom"),
        ("owner/repo::", ""),
    ] {
        assert_eq!(
            parse_skill_tap_spec(input),
            Some(SkillTapSpec {
                repo: "owner/repo".into(),
                path: path.into(),
            }),
            "{input}"
        );
    }
}

#[test]
fn splits_versions_only_when_both_parts_are_present_and_not_https() {
    if run_isolated("splits_versions_only_when_both_parts_are_present_and_not_https") {
        return;
    }

    assert_eq!(
        parse_skill_name_and_version(" group@helper@2.0 "),
        ("group@helper".into(), Some("2.0".into()))
    );

    for input in [
        "helper",
        "@2.0",
        "helper@",
        "https://example.invalid/helper@2.0",
    ] {
        assert_eq!(
            parse_skill_name_and_version(&format!(" {input} ")),
            (input.into(), None)
        );
    }
}

#[test]
fn distinguishes_explicit_github_skills_from_repos_and_registry_keys() {
    if run_isolated("distinguishes_explicit_github_skills_from_repos_and_registry_keys") {
        return;
    }

    for invalid in [
        " ",
        "owner/repo",
        "https://github.com/owner",
        "https://github.com/owner/repo/",
        "official/library/helper",
    ] {
        assert_eq!(parse_explicit_github_skill(invalid), None, "{invalid:?}");
    }

    for (input, branch) in [
        (
            "https://github.com/owner/repo/tree/dev/library/helper",
            Some("dev"),
        ),
        ("http://github.com/owner/repo/library/helper", None),
        (" owner/repo/library//helper/ ", None),
    ] {
        assert_eq!(
            parse_explicit_github_skill(input),
            Some((
                "owner/repo".into(),
                branch.map(str::to_owned),
                "library/helper".into()
            )),
            "{input}"
        );
    }

    assert_eq!(parse_registry_prefixed_skill(" Skills-Sh / \t"), None);
    assert_eq!(
        parse_registry_prefixed_skill(" Skills-Sh / owner/helper "),
        Some(("skills.sh".into(), "owner/helper".into()))
    );
}

#[test]
fn sanitizes_install_names_and_validates_relative_paths() {
    if run_isolated("sanitizes_install_names_and_validates_relative_paths") {
        return;
    }

    assert_eq!(
        sanitize_skill_install_name(" owner/repo/My!! Skill-2 "),
        "My_Skill-2"
    );
    assert_eq!(sanitize_skill_install_name("owner/repo/?!"), "skill");

    ensure_safe_relative_path("docs/example.md").unwrap();
    assert_config_error(ensure_safe_relative_path(""), "Empty path in skill bundle.");
    for path in ["/absolute.md", r"docs\example.md"] {
        assert_config_error(
            ensure_safe_relative_path(path),
            &format!("Unsafe path in skill bundle: {path}"),
        );
    }
    for path in [
        "docs/../example.md",
        "docs/./example.md",
        "docs//example.md",
    ] {
        assert_config_error(
            ensure_safe_relative_path(path),
            &format!("Unsafe path segment in skill bundle: {path}"),
        );
    }
}

#[test]
fn scores_registry_matches_by_the_strongest_matching_field() {
    if run_isolated("scores_registry_matches_by_the_strongest_matching_field") {
        return;
    }

    let entry = index_entry(json!({
        "name": "Build Helper",
        "identifier": "official/BuildHelper",
        "description": "Build Helper for COMPILATION and packaging",
        "tags": ["COMPILATION", "TOOLING"]
    }));

    for (query, expected) in [
        (" \t", 0),
        ("missing", 0),
        (" BUILD HELPER ", 1000),
        ("OFFICIAL/BUILDHELPER", 1000),
        ("build", 900),
        ("official/", 900),
        ("helper", 700),
        ("/build", 700),
        ("compilation", 550),
        ("tooling", 550),
        ("packaging", 450),
    ] {
        assert_eq!(score_registry_match(&entry, query), expected, "{query:?}");
    }
}

#[test]
fn assigns_source_priority_and_default_trust() {
    if run_isolated("assigns_source_priority_and_default_trust") {
        return;
    }

    for (source, priority, trust) in [
        ("official", 0, "builtin"),
        ("skills.sh", 1, "trusted"),
        ("github", 4, "trusted"),
        ("clawhub", 5, "community"),
        ("lobehub", 7, "community"),
        ("unknown", 99, "community"),
    ] {
        assert_eq!(skill_source_priority(source), priority, "{source}");
        assert_eq!(default_trust_level_for_source(source), trust, "{source}");
    }
}

#[test]
fn resolves_registry_specific_install_sources() {
    if run_isolated("resolves_registry_specific_install_sources") {
        return;
    }

    for identifier in ["lobehub/ calendar ", " calendar "] {
        let entry = index_entry(json!({"source": "LobeHub", "identifier": identifier}));
        assert!(matches!(
            resolved_source_from_index(&entry),
            Some(RegistryInstallSource::LobeRegistry { slug }) if slug == "calendar"
        ));
    }

    let claw = index_entry(json!({"source": "ClawHub", "identifier": " calendar "}));
    assert!(matches!(
        resolved_source_from_index(&claw),
        Some(RegistryInstallSource::ClawRegistry { slug, version: None }) if slug == "calendar"
    ));

    for value in [
        json!({"source": "lobehub", "identifier": "lobehub/ "}),
        json!({"source": "clawhub", "identifier": " "}),
        json!({"source": "official", "path": " /// "}),
        json!({"source": "github", "resolved_github_id": "owner/repo"}),
        json!({}),
    ] {
        assert!(resolved_source_from_index(&index_entry(value)).is_none());
    }
}

#[test]
fn resolves_index_git_sources_in_precedence_order() {
    if run_isolated("resolves_index_git_sources_in_precedence_order") {
        return;
    }

    for (value, expected_repo, expected_dir) in [
        (
            json!({
                "source": "OFFICIAL",
                "path": " /skills/calendar/ ",
                "repo": "other/repo",
                "resolved_github_id": "other/repo/other-skill"
            }),
            "nousresearch/hermes-agent",
            "skills/calendar",
        ),
        (
            json!({
                "source": "skills.sh",
                "resolved_github_id": "https://github.com/owner/preferred/tree/dev/library/helper",
                "repo": "other/repo",
                "path": "ignored"
            }),
            "owner/preferred",
            "library/helper",
        ),
        (
            json!({
                "source": "github",
                "resolved_github_id": "owner/incomplete",
                "repo": " owner/fallback ",
                "path": "/library/helper/"
            }),
            "owner/fallback",
            "library/helper",
        ),
        (
            json!({"source": "claude-marketplace", "repo": "owner/collection", "path": " "}),
            "owner/collection",
            "skills",
        ),
    ] {
        let source = resolved_source_from_index(&index_entry(value));
        assert!(matches!(
            source,
            Some(RegistryInstallSource::GitRepo(repo))
                if repo.repo == expected_repo && repo.branch == "main" && repo.skill_dir == expected_dir
        ));
    }
}

#[test]
fn renders_lobehub_metadata_and_multiline_instructions() {
    if run_isolated("renders_lobehub_metadata_and_multiline_instructions") {
        return;
    }

    let response: LobeHubAgentResponse = serde_json::from_value(json!({
        "author": " Example Author ",
        "homepage": " https://example.invalid/calendar ",
        "summary": "Not used when a description is provided",
        "meta": {"title": " Calendar Helper ", "description": " First line\nSecond line "},
        "config": {"systemRole": " Check the date.\nList the events. "}
    }))
    .unwrap();

    assert_eq!(
        build_lobehub_skill_markdown(&response, "calendar"),
        concat!(
            "---\nname: calendar\ndescription: First line Second line\ncategory: lobehub\n---\n\n",
            "# Calendar Helper\n\nFirst line\nSecond line\n\n",
            "## Source\n- Registry: lobehub\n- Identifier: calendar\n",
            "- Author: Example Author\n- Homepage: https://example.invalid/calendar\n",
            "\n## Instructions\nCheck the date.\nList the events.\n",
        )
    );
}

#[test]
fn renders_lobehub_fallbacks_and_omits_blank_metadata() {
    if run_isolated("renders_lobehub_fallbacks_and_omits_blank_metadata") {
        return;
    }

    let response: LobeHubAgentResponse =
        serde_json::from_value(json!({"summary": " Summary text ", "meta": {"title": " "}}))
            .unwrap();

    assert_eq!(
        build_lobehub_skill_markdown(&response, "calendar"),
        concat!(
            "---\nname: calendar\ndescription: Summary text\ncategory: lobehub\n---\n\n",
            "# calendar\n\nSummary text\n\n",
            "## Source\n- Registry: lobehub\n- Identifier: calendar\n",
            "\n## Instructions\nNo system role provided by source registry.\n",
        )
    );

    let empty: LobeHubAgentResponse = serde_json::from_value(json!({})).unwrap();
    let markdown = build_lobehub_skill_markdown(&empty, "calendar");
    assert!(markdown.starts_with("---\nname: calendar\ncategory: lobehub\n---\n"));
    assert!(!markdown.contains("description:"));
}

#[test]
fn reads_hub_lock_defaults_for_missing_legacy_and_malformed_files() {
    if run_isolated("reads_hub_lock_defaults_for_missing_legacy_and_malformed_files") {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let skills = temp.path().join("skills");
    let state = skills.join(".hub");
    assert_eq!(skills_hub_state_dir(&skills), state);
    assert_eq!(skills_hub_lock_path(&skills), state.join("lock.json"));
    assert_eq!(skills_hub_audit_path(&skills), state.join("audit.log"));

    let missing = read_skills_hub_lock(&skills);
    assert_eq!(missing.version, 1);
    assert!(missing.installed.is_empty());
    assert!(!state.exists());

    fs::create_dir_all(&state).unwrap();
    for (body, version) in [("{}", 1), ("{", 1), (r#"{"version":42}"#, 42)] {
        fs::write(state.join("lock.json"), body).unwrap();
        let lock = read_skills_hub_lock(&skills);
        assert_eq!(lock.version, version, "{body}");
        assert!(lock.installed.is_empty());
    }
}

include!("command_catalog_part2_coverage_gen.rs");
