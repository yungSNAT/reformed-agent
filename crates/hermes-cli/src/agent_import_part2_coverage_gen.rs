#[cfg(target_os = "linux")]
#[test]
fn memory_backup_failure_preserves_the_original() {
    let fixture = Fixture::new();
    let suffix = "memories/MEMORY.md";
    let destination = fixture.home.join(suffix);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, "Keep this note.").unwrap();
    fs::write(fixture.source.join("AGENTS.md"), "- Add a new note.\n").unwrap();
    let home = pad_home_to_path_limit(&fixture.home, suffix);
    assert_eq!(
        fs::read_to_string(home.join(suffix)).unwrap(),
        "Keep this note."
    );

    let report = import_agent(&AgentImportRequest {
        home: &home,
        ..fixture.request(SourceAgent::Codex)
    });
    assert!(report.failed);
    assert_line(
        &report,
        "error        AGENTS.md: could not write memories/MEMORY.md: could not back it up first:",
    );
    assert_eq!(fs::read_to_string(destination).unwrap(), "Keep this note.");
}

#[test]
fn a_skills_directory_without_skill_markers_is_not_imported() {
    let fixture = Fixture::new();
    let skills = fixture.source.join("skills");
    fs::create_dir_all(skills.join("notes")).unwrap();
    fs::write(skills.join("notes/README.md"), "No marker.\n").unwrap();
    fs::write(skills.join("README.md"), "Not a skill.\n").unwrap();

    let report = import_agent(&fixture.request(SourceAgent::Codex));
    assert!(!report.failed, "{:#?}", report.lines);
    assert_line(&report, "skipped      skills: no folder with a SKILL.md");
    assert!(!fixture.home.join("skills").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn skill_copy_failure_is_reported_without_an_imported_skill() {
    let fixture = Fixture::new();
    let skill = fixture.source.join("skills/tidy");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "Tidy up.\n").unwrap();
    let home = pad_home_to_path_limit(&fixture.home, "skills");

    let report = import_agent(&AgentImportRequest {
        home: &home,
        ..fixture.request(SourceAgent::Codex)
    });
    assert!(report.failed);
    assert_line(&report, "error        skill tidy: could not copy it:");
    assert!(!report
        .lines
        .iter()
        .any(|line| line.starts_with("imported     skill tidy:")));
    assert!(!fixture.home.join("skills/codex-imports/tidy").exists());
}

#[cfg(unix)]
#[test]
fn copying_a_skill_reports_the_links_it_omits() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let skill = fixture.source.join("skills/tidy");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "Tidy up.\n").unwrap();
    symlink("SKILL.md", skill.join("shortcut.md")).unwrap();

    let report = import_agent(&fixture.request(SourceAgent::Codex));
    assert!(!report.failed, "{:#?}", report.lines);
    let destination = fixture.home.join("skills/codex-imports/tidy");
    assert_line(
        &report,
        &format!(
            "imported     skill tidy: to {} (1 links inside it are not copied)",
            destination.display()
        ),
    );
    assert_eq!(
        fs::read_to_string(destination.join("SKILL.md")).unwrap(),
        "Tidy up.\n"
    );
    assert!(fs::symlink_metadata(destination.join("shortcut.md")).is_err());
}

#[cfg(unix)]
#[test]
fn mcp_import_rejects_a_destination_beneath_a_regular_file() {
    let fixture = Fixture::new();
    fixture.claude_servers(json!({"docs": {"command": "docs-server"}}));
    let blocked_home = fixture.home.join("not-a-directory");
    fs::write(&blocked_home, "Keep this file.\n").unwrap();

    let report = import_agent(&AgentImportRequest {
        home: &blocked_home,
        ..fixture.request(SourceAgent::ClaudeCode)
    });
    assert!(report.failed);
    assert_line(
        &report,
        &format!(
            "error        MCP servers: could not check {}:",
            blocked_home.join("config.yaml").display()
        ),
    );
    assert_eq!(
        fs::read_to_string(blocked_home).unwrap(),
        "Keep this file.\n"
    );
}

#[test]
fn unreadable_mcp_records_stop_import_without_changing_either_file() {
    let fixture = Fixture::new();
    fixture.claude_servers(json!({"docs": {"command": "docs-server"}}));
    let records = fixture.home.join("mcp_servers.json");
    let config = fixture.home.join("config.yaml");
    fs::write(&records, [0xff]).unwrap();
    fs::write(&config, "mcp_servers: []\n").unwrap();

    let report = import_agent(&fixture.request(SourceAgent::ClaudeCode));
    assert!(report.failed);
    assert_line(
        &report,
        "error        MCP servers: mcp_servers.json is left unchanged: could not read it:",
    );
    assert_eq!(fs::read(records).unwrap(), [0xff]);
    assert_eq!(fs::read_to_string(config).unwrap(), "mcp_servers: []\n");
}

#[test]
fn malformed_mcp_entries_and_credential_commands_are_skipped() {
    let fixture = Fixture::new();
    let credential = concat!("ghp", "_fixtureAgents01");
    fixture.claude_servers(json!({
        "scalar": 42,
        "secret-command": {"command": credential},
        "mixed-args": {"command": "srv", "args": ["--stdio", 3]},
        "scalar-args": {"command": "srv", "args": "--stdio"}
    }));

    let report = import_agent(&fixture.request(SourceAgent::ClaudeCode));
    assert!(!report.failed, "{:#?}", report.lines);
    for expected in [
        "skipped      MCP server scalar: the entry is not an object",
        "skipped      MCP server secret-command: its command looks like a credential",
        "skipped      MCP server mixed-args: its args are not all strings",
        "skipped      MCP server scalar-args: its args are not a list",
    ] {
        assert_line(&report, expected);
    }
    assert!(!report.lines.join("\n").contains(credential));
    assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), 0);
}

#[test]
fn mcp_import_respects_existing_names_in_list_form() {
    let fixture = Fixture::new();
    fixture.claude_servers(json!({
        "kept": {"command": "replacement"},
        "new": {"command": "new-server", "args": ["--stdio"]}
    }));
    let records_path = fixture.home.join("mcp_servers.json");
    let kept = json!({"name": "kept", "command": "original", "args": []});
    fs::write(
        &records_path,
        json!({"servers": [kept.clone()]}).to_string(),
    )
    .unwrap();

    let report = import_agent(&fixture.request(SourceAgent::ClaudeCode));
    assert!(!report.failed, "{:#?}", report.lines);
    assert_line(&report, "conflict     MCP server kept:");
    assert_line(
        &report,
        "imported     MCP server new: new-server (1 argument)",
    );
    let records = read_records(&records_path).unwrap();
    assert_eq!(
        records,
        json!({"servers": [
            kept,
            {"name": "new", "command": "new-server", "args": ["--stdio"], "env": {}}
        ]})
    );
}

#[test]
fn merged_mcp_records_must_still_load_with_the_runtime_reader() {
    let fixture = Fixture::new();
    fixture.claude_servers(json!({"servers": {"command": "new-server"}}));
    let records_path = fixture.home.join("mcp_servers.json");
    let original = r#"{"kept":{"command":"original"}}"#;
    fs::write(&records_path, original).unwrap();

    // The name "servers" also denotes the list wrapper. Honor whichever
    // interpretation the runtime reader gives the resulting object.
    let merged = json!({
        "kept": {"command": "original"},
        "servers": {"command": "new-server", "args": [], "env": {}}
    });
    let report = import_agent(&fixture.request(SourceAgent::ClaudeCode));
    match crate::mcp_config::parse_mcp_config_json(&merged.to_string()) {
        Ok(_) => {
            assert!(!report.failed, "{:#?}", report.lines);
            assert_line(&report, "imported     MCP server servers: new-server");
            assert_eq!(read_records(&records_path).unwrap(), merged);
        }
        Err(_) => {
            assert!(report.failed);
            assert_line(
                &report,
                "error        MCP servers: the imported servers would not load:",
            );
            assert_eq!(fs::read_to_string(records_path).unwrap(), original);
            assert!(!fixture.home.join("config.yaml").exists());
        }
    }
}

#[test]
fn null_mcp_records_are_normalized_if_accepted_by_the_runtime_reader() {
    let fixture = Fixture::new();
    let path = fixture.home.join("mcp_servers.json");
    fs::write(&path, "null").unwrap();

    match crate::mcp_config::parse_mcp_config_json("null") {
        Ok(_) => assert_eq!(read_records(&path).unwrap(), json!({})),
        Err(err) => assert_eq!(read_records(&path).unwrap_err(), err),
    }
}

#[test]
fn mcp_records_that_cannot_be_represented_as_json_are_not_rewritten() {
    let fixture = Fixture::new();
    let path = fixture.home.join("mcp_servers.json");
    for text in [
        " \n",
        r#"{"servers":[{"name":"kept","command":"original","extra":1e400}]}"#,
    ] {
        fs::write(&path, text).unwrap();
        assert!(read_records(&path).is_err(), "accepted {text:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn write_servers_reports_an_unreadable_original_before_saving() {
    let fixture = Fixture::new();
    let records = fixture.home.join("mcp_servers.json");
    let config = fixture.home.join("config.yaml");
    fs::create_dir(&records).unwrap();

    let error = write_servers(
        &records,
        "{}",
        &config,
        &hermes_config::GatewayConfig::default(),
        |_, _| panic!("must not save when the original cannot be read"),
    )
    .unwrap_err();
    assert!(
        error.starts_with("could not read mcp_servers.json:"),
        "{error}"
    );
    assert!(records.is_dir());
    assert!(!config.exists());
}

#[test]
fn write_servers_reports_when_rollback_cannot_remove_a_directory() {
    let fixture = Fixture::new();
    let records = fixture.home.join("mcp_servers.json");
    let config = fixture.home.join("config.yaml");

    let error = write_servers(
        &records,
        "{}",
        &config,
        &hermes_config::GatewayConfig::default(),
        |path, _| {
            fs::create_dir(path).unwrap();
            Err("injected save failure".to_string())
        },
    )
    .unwrap_err();
    assert!(
        error.starts_with("injected save failure; the files could not be restored:"),
        "{error}"
    );
    assert!(config.is_dir());
}

#[test]
fn merge_ignores_whitespace_only_entries_without_counting_them_as_rejections() {
    let incoming = [" \t\n".to_string(), "Keep this note.".to_string()];
    let merge = merge_entries(&[], &incoming, 100);

    assert_eq!(merge.entries, vec!["Keep this note."]);
    assert_eq!(merge.added, 1);
    assert_eq!(
        (merge.duplicates, merge.over_limit, merge.refused),
        (0, 0, 0)
    );
}
