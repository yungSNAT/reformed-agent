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
    let output = std::process::Command::new(std::env::current_exe().unwrap()) // nosemgrep: rust.lang.security.current-exe.current-exe
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

include!("agent_import_part2_coverage_gen.rs");
