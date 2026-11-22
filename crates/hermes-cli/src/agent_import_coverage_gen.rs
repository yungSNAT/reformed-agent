use super::*;
use serde_json::json;
use std::fs;

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let home = root.path().join("home");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&home).unwrap();
        Self {
            _root: root,
            source,
            home,
        }
    }

    fn request(&self, agent: SourceAgent) -> AgentImportRequest<'_> {
        AgentImportRequest {
            agent,
            source: &self.source,
            home: &self.home,
            apply: true,
        }
    }

    fn claude_servers(&self, servers: Value) {
        fs::write(
            self.source.join("settings.json"),
            json!({"mcpServers": servers}).to_string(),
        )
        .unwrap();
    }
}

fn assert_line(report: &AgentImportReport, prefix: &str) {
    assert!(
        report.lines.iter().any(|line| line.starts_with(prefix)),
        "missing {prefix:?} in {:#?}",
        report.lines
    );
}

#[test]
fn default_folder_names_match_the_source_agents() {
    assert_eq!(SourceAgent::ClaudeCode.default_folder(), ".claude");
    assert_eq!(SourceAgent::Codex.default_folder(), ".codex");
}

#[cfg(target_os = "linux")]
#[test]
fn default_home_lookup_and_parentless_source_use_the_isolated_fixture() {
    const CHILD_HOME: &str = "HERMES_AGENT_IMPORT_COVERAGE_TEST_HOME";
    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let home = PathBuf::from(home);
        assert_eq!(
            default_source_dir(SourceAgent::ClaudeCode),
            Some(home.join(".claude"))
        );
        assert_eq!(
            default_source_dir(SourceAgent::Codex),
            Some(home.join(".codex"))
        );

        // The public entry point rejects an empty path before dispatching.
        let source = Path::new("");
        assert!(source.parent().is_none());
        let request = AgentImportRequest {
            agent: SourceAgent::ClaudeCode,
            source,
            home: &home,
            apply: false,
        };
        let mut importer = Importer {
            request: &request,
            report: AgentImportReport::default(),
            left_out: Vec::new(),
            save_config: |_, _| panic!("a plan must not save config"),
        };
        importer.claude_code();
        assert!(!importer.report.failed, "{:#?}", importer.report.lines);
        assert_line(&importer.report, "would import CLAUDE.md: 1 of 1 entries");
        assert_eq!(fs::read_dir(home).unwrap().count(), 0);
        return;
    }

    let fixture = Fixture::new();
    fs::write(fixture.source.join("CLAUDE.md"), "- Keep notes short.\n").unwrap();
    // If a parent is fabricated from the current directory, this causes a parse error.
    fs::write(fixture.source.join(".claude.json"), "{").unwrap();

    let qualified = concat!(
        module_path!(),
        "::default_home_lookup_and_parentless_source_use_the_isolated_fixture"
    );
    let (_, test_name) = qualified.split_once("::").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name])
        .current_dir(&fixture.source)
        .env("HOME", &fixture.home)
        .env(CHILD_HOME, &fixture.home)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn missing_source_fails_without_writing_to_the_home() {
    let fixture = Fixture::new();
    let missing = fixture.source.join("missing");
    let report = import_agent(&AgentImportRequest {
        source: &missing,
        ..fixture.request(SourceAgent::Codex)
    });

    assert!(report.failed);
    assert_line(&report, "error        source: the folder does not exist");
    assert_eq!(report.lines.len(), 2);
    assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), 0);
}

#[test]
fn invalid_claude_settings_are_reported_as_read_shape_or_parse_errors() {
    let fixture = Fixture::new();
    let settings = fixture.source.join("settings.json");
    fs::create_dir(&settings).unwrap();
    let request = fixture.request(SourceAgent::ClaudeCode);

    let report = import_agent(&request);
    assert!(report.failed);
    assert_line(&report, "error        settings.json: could not read it:");

    fs::remove_dir(&settings).unwrap();
    for (contents, expected) in [
        ("[]", "error        settings.json: it is not a JSON object"),
        (
            "{\"mcpServers\":",
            "error        settings.json: could not parse it:",
        ),
    ] {
        fs::write(&settings, contents).unwrap();
        let report = import_agent(&request);
        assert!(report.failed);
        assert_line(&report, expected);
        assert_line(&report, "skipped      MCP servers: none found");
    }

    assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), 0);
}

#[test]
fn memory_folder_skips_directories_and_non_markdown_but_reports_invalid_utf8() {
    let fixture = Fixture::new();
    let memories = fixture.source.join("memories");
    fs::create_dir_all(memories.join("folder.md")).unwrap();
    fs::write(memories.join("ignored.txt"), "Do not import this.\n").unwrap();
    fs::write(memories.join("invalid.md"), [0xff]).unwrap();
    fs::write(memories.join("valid.md"), "- Keep this note.\n").unwrap();

    let report = import_agent(&fixture.request(SourceAgent::Codex));
    assert!(report.failed);
    assert_line(&report, "skipped      memories/folder.md: it is a folder");
    assert_line(
        &report,
        &format!(
            "error        memories: could not read {}:",
            memories.join("invalid.md").display()
        ),
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("memories/MEMORY.md")).unwrap(),
        "Keep this note."
    );
}

#[cfg(unix)]
#[test]
fn vanished_memory_and_skill_entries_are_reported_after_listing() {
    use crate::source_tree::tests::BETWEEN;

    struct ResetHook;

    impl Drop for ResetHook {
        fn drop(&mut self) {
            BETWEEN.with(|hook| *hook.borrow_mut() = None);
        }
    }

    let fixture = Fixture::new();
    let memory = fixture.source.join("memories/vanishing.md");
    let skill = fixture.source.join("skills/vanishing-skill");
    fs::create_dir_all(memory.parent().unwrap()).unwrap();
    fs::create_dir_all(&skill).unwrap();
    fs::write(&memory, "A note.\n").unwrap();
    fs::write(skill.join("SKILL.md"), "A skill.\n").unwrap();

    let _reset = ResetHook;
    BETWEEN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |name: &std::ffi::OsStr| {
            if name == "vanishing.md" {
                fs::remove_file(&memory).unwrap();
            } else if name == "vanishing-skill" {
                fs::remove_dir_all(&skill).unwrap();
            }
        }));
    });

    let report = import_agent(&fixture.request(SourceAgent::Codex));
    assert!(report.failed);
    // The lookup error stops collection and is reported against the source folder.
    assert_line(
        &report,
        "error        memories: could not read it: it went away while it was read",
    );
    assert_line(
        &report,
        "error        skills: could not read it: it went away while it was read",
    );
    assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), 0);
}

#[test]
fn unreadable_destination_memory_is_not_replaced() {
    let fixture = Fixture::new();
    let destination = fixture.home.join("memories/MEMORY.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, [0xff]).unwrap();
    fs::write(fixture.source.join("AGENTS.md"), "- A new note.\n").unwrap();

    let report = import_agent(&fixture.request(SourceAgent::Codex));
    assert!(report.failed);
    assert_line(
        &report,
        "error        AGENTS.md: could not read memories/MEMORY.md:",
    );
    assert_eq!(fs::read(destination).unwrap(), [0xff]);
}

#[test]
fn multiple_credentials_are_counted_but_not_printed_or_saved_in_memory() {
    let fixture = Fixture::new();
    let first = concat!("ghp", "_fixtureAgents01");
    let second = concat!("ghp", "_fixtureMemory02");
    fs::write(
        fixture.source.join("AGENTS.md"),
        format!("- CI uses {first}.\n- Deploy with {second}.\n- Run tests first.\n"),
    )
    .unwrap();

    let report = import_agent(&fixture.request(SourceAgent::Codex));
    assert!(!report.failed, "{:#?}", report.lines);
    let text = report.lines.join("\n");
    assert!(
        text.contains("2 left out as they look like a credential"),
        "{text}"
    );
    assert!(!text.contains(first));
    assert!(!text.contains(second));
    assert_eq!(
        fs::read_to_string(fixture.home.join("memories/MEMORY.md")).unwrap(),
        "Run tests first."
    );
}

#[cfg(target_os = "linux")]
fn pad_home_to_path_limit(home: &Path, suffix: &str) -> PathBuf {
    // Redundant components count toward the kernel's pathname limit without
    // needing a deep directory tree or changing permissions.
    let available = libc::PATH_MAX as usize - 1 - home.join(suffix).as_os_str().len();
    let mut padded = home.as_os_str().to_os_string();
    padded.push("/");
    padded.push("./".repeat(available / 2));
    PathBuf::from(padded)
}

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
