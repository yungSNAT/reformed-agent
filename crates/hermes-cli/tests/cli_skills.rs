// The subprocess uses HOME to isolate OS-level discovery of profiles.
#![cfg(unix)]

use assert_cmd::{assert::Assert, cargo::cargo_bin, Command};
use serde_json::Value;
use std::{fs, path::Path, time::Duration};

fn skills(home: &Path, args: &[&str]) -> Command {
    let managed = home.join("managed");
    fs::create_dir_all(&managed).expect("create empty managed config directory");
    let mut cmd = Command::new(cargo_bin!("hermes"));
    cmd.env_clear()
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("HERMES_HOME", home)
        .env("HERMES_MANAGED_DIR", managed)
        .env("HERMES_BUNDLED_SKILLS", home.join("bundled"))
        .env("HERMES_OPTIONAL_SKILLS", home.join("optional"))
        .env("HERMES_SKILLS_EXECUTION_TIER", "open")
        .timeout(Duration::from_secs(15))
        .arg("skills")
        .args(args);
    for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd
}

fn success_text(assertion: Assert) -> String {
    let assertion = assertion.success();
    String::from_utf8(assertion.get_output().stdout.clone()).expect("UTF-8 stdout")
}

fn write_skill(path: &Path, markdown: &str) {
    fs::create_dir_all(path).expect("create fixture skill");
    fs::write(path.join("SKILL.md"), markdown).expect("write fixture markdown");
}

#[test]
fn skills_cli_lists_inspects_and_checks_local_skills() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let listed = success_text(skills(root, &[]).assert());
    assert!(listed.contains("No skills directory found"), "{listed}");

    let installed = root.join("skills");
    fs::create_dir(&installed).unwrap();
    let empty = success_text(skills(root, &["browse"]).assert());
    assert!(empty.contains("(no skills installed)"), "{empty}");

    let markdown = "---\nname: heron-notes\n---\n# Heron notes\n\nNest survey.\n";
    write_skill(&installed.join("heron-notes"), markdown);
    write_skill(&installed.join("blank"), " \n");
    fs::create_dir(installed.join("missing")).unwrap();
    fs::write(installed.join("loose.md"), "# Loose file").unwrap();

    let listed = success_text(skills(root, &[]).assert());
    assert!(listed.contains("heron-notes"), "{listed}");
    let browsed = success_text(skills(root, &["browse"]).assert());
    assert!(
        browsed.contains("heron-notes") && browsed.contains("Heron notes"),
        "{browsed}"
    );
    assert!(browsed.contains("(no description)"), "{browsed}");

    let inspected = success_text(skills(root, &["inspect", "heron-notes"]).assert());
    assert!(inspected.contains(markdown), "{inspected}");
    let absent = success_text(skills(root, &["inspect", "absent"]).assert());
    assert!(absent.contains("Skill 'absent' not found"), "{absent}");

    let checked = success_text(skills(root, &["check"]).assert());
    assert!(checked.contains("1 healthy, 2 with issues"), "{checked}");
    let named = success_text(skills(root, &["check", "heron-notes"]).assert());
    assert!(named.contains("Frontmatter: yes"), "{named}");
    let broken = success_text(skills(root, &["check", "missing"]).assert());
    assert!(broken.contains("MISSING SKILL.md"), "{broken}");
}

#[test]
fn skills_cli_resets_only_the_named_local_skill() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let installed = root.join("skills");
    let original = installed.join("heron-notes");
    let sibling = installed.join("wren-notes");
    write_skill(&original, "# Heron notes");
    fs::write(original.join("notes.md"), "Old fixture notes").unwrap();
    write_skill(&sibling, "# Wren notes");

    let missing = skills(root, &["reset"]).assert().failure();
    assert!(String::from_utf8_lossy(&missing.get_output().stderr)
        .contains("Missing skill name. Usage: hermes skills reset <name>"));
    assert!(original.join("notes.md").exists());

    for name in ["heron-notes", "plover-notes"] {
        let output = success_text(skills(root, &["reset", name]).assert());
        assert!(
            output.contains(&format!("Skill '{name}' reset at ")),
            "{output}"
        );
        let manifest = fs::read_to_string(installed.join(name).join("SKILL.md")).unwrap();
        assert_eq!(
            manifest,
            format!("# {name}\n\nReset by CLI. Replace with canonical skill contents.\n")
        );
    }
    assert!(!original.join("notes.md").exists());
    assert_eq!(
        fs::read_to_string(sibling.join("SKILL.md")).unwrap(),
        "# Wren notes"
    );
}

#[test]
fn skills_cli_sync_and_opt_out_preserve_local_edits() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let bundled = root.join("bundled");
    write_skill(&bundled.join("heron-notes"), "# Heron v1");
    write_skill(&bundled.join("wren-notes"), "# Wren v1");

    let copied = success_text(skills(root, &["sync"]).assert());
    assert!(copied.contains("2 new"), "{copied}");
    let heron = root.join("skills/heron-notes/SKILL.md");
    let wren = root.join("skills/wren-notes/SKILL.md");
    assert_eq!(fs::read_to_string(&heron).unwrap(), "# Heron v1");
    fs::write(&wren, "# Local wren").unwrap();
    write_skill(&bundled.join("heron-notes"), "# Heron v2");

    let updated = success_text(skills(root, &["sync"]).assert());
    assert!(updated.contains("Updated: heron-notes"), "{updated}");
    assert!(
        updated.contains("User-modified kept: wren-notes"),
        "{updated}"
    );
    assert_eq!(fs::read_to_string(&heron).unwrap(), "# Heron v2");

    skills(root, &["opt-out"]).assert().success();
    let marker = root.join(".no-bundled-skills");
    assert!(marker.exists() && heron.exists());
    let skipped = success_text(skills(root, &["sync"]).assert());
    assert!(skipped.contains("profile opted out"), "{skipped}");

    let declined = success_text(
        skills(root, &["opt-out", "--remove"])
            .write_stdin("no\n")
            .assert(),
    );
    assert!(
        declined.contains("Marker kept; no skills deleted"),
        "{declined}"
    );
    assert!(marker.exists() && heron.exists());

    let removed = success_text(skills(root, &["opt-out", "--remove", "--yes"]).assert());
    assert!(removed.contains("Removed: heron-notes"), "{removed}");
    assert!(!heron.exists());
    assert_eq!(fs::read_to_string(&wren).unwrap(), "# Local wren");

    skills(root, &["opt-in", "--sync"]).assert().success();
    assert!(!marker.exists());
    assert_eq!(fs::read_to_string(&heron).unwrap(), "# Heron v2");
    assert_eq!(fs::read_to_string(&wren).unwrap(), "# Local wren");
}

#[test]
fn skills_cli_subscriptions_are_idempotent_and_tier_gated() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let denied = skills(root, &["subscribe", "local-catalogue"])
        .env("HERMES_SKILLS_EXECUTION_TIER", "trusted")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&denied.get_output().stderr);
    assert!(
        stderr.contains("blocked by skills tier 'trusted'"),
        "{stderr}"
    );
    let registry = root.join("skills/subscriptions.json");
    assert!(!registry.exists());

    skills(root, &["subscribe", " local-catalogue "])
        .assert()
        .success();
    let first = fs::read_to_string(&registry).unwrap();
    let entries: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert_eq!(entries[0]["source"], "local-catalogue");
    let duplicate = success_text(skills(root, &["subscribe", "local-catalogue"]).assert());
    assert!(duplicate.contains("already exists"), "{duplicate}");
    skills(root, &["subscribe", " "]).assert().failure();
    assert_eq!(fs::read_to_string(&registry).unwrap(), first);
}

#[test]
fn skills_cli_snapshot_round_trip_restores_uninstalled_content() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let installed = root.join("skills/heron-notes");
    let markdown = "# Heron notes\n\nNest survey.";
    write_skill(&installed, markdown);
    fs::write(installed.join("survey.txt"), "two nests").unwrap();

    skills(root, &["snapshot", "export", "--extra", "survey.tar.gz"])
        .assert()
        .success();
    assert!(root.join("survey.tar.gz").is_file());
    skills(root, &["uninstall", "heron-notes"])
        .assert()
        .success();
    assert!(!installed.exists());

    skills(root, &["snapshot", "import", "--extra", "survey.tar.gz"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(installed.join("SKILL.md")).unwrap(),
        markdown
    );
    assert_eq!(
        fs::read_to_string(installed.join("survey.txt")).unwrap(),
        "two nests"
    );
}

#[test]
fn skills_cli_uninstall_updates_only_the_matching_lock_and_audit_records() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let installed = root.join("skills");
    let state = installed.join(".hub");
    let lock = state.join("lock.json");
    fs::create_dir_all(&state).unwrap();
    write_skill(&installed.join("heron-notes"), "# Heron notes");
    write_skill(&installed.join("wren-notes"), "# Wren notes");

    let records = ["heron-notes", "stale-robin", "wren-notes"].map(|name| {
        serde_json::json!({
            "name": name,
            "source": "tap",
            "identifier": format!("field/notes/{name}"),
            "trust_level": "trusted",
            "scan_verdict": "clean",
            "content_hash": format!("fixture-{name}"),
            "install_path": installed.join(name),
            "installed_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        })
    });
    fs::write(
        &lock,
        serde_json::json!({"version": 1, "installed": records}).to_string(),
    )
    .unwrap();
    let read_lock = || serde_json::from_str::<Value>(&fs::read_to_string(&lock).unwrap()).unwrap();

    let removed = success_text(skills(root, &["uninstall", "heron-notes"]).assert());
    assert!(removed
        .contains("Skill 'heron-notes' uninstalled (source=tap, id=field/notes/heron-notes)"));
    assert!(!installed.join("heron-notes").exists());
    assert_eq!(read_lock()["installed"][0]["name"], "stale-robin");

    let stale = success_text(skills(root, &["uninstall", "stale-robin"]).assert());
    assert!(stale.contains("Skill 'stale-robin' not found locally, but removed stale lock entry"));
    let absent = success_text(skills(root, &["uninstall", "absent"]).assert());
    assert!(absent.contains("Skill 'absent' not found."));
    let check = success_text(skills(root, &["check", "heron-notes"]).assert());
    assert!(check.contains("Skill 'heron-notes' not found."));

    let kept = read_lock();
    assert_eq!(kept["installed"].as_array().unwrap().len(), 1);
    assert_eq!(kept["installed"][0]["name"], "wren-notes");
    assert_eq!(kept["installed"][0]["content_hash"], "fixture-wren-notes");
    assert_eq!(
        fs::read_to_string(installed.join("wren-notes/SKILL.md")).unwrap(),
        "# Wren notes"
    );

    let log = fs::read_to_string(state.join("audit.log")).unwrap();
    let entries: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 2);
    for (entry, name) in entries.iter().zip(["heron-notes", "stale-robin"]) {
        assert_eq!(entry["action"], "UNINSTALL");
        assert_eq!(entry["name"], name);
        assert_eq!(entry["identifier"], format!("field/notes/{name}"));
    }
}

#[test]
fn skills_cli_reports_local_configuration_quality_and_audit_findings() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let before = success_text(skills(root, &["update"]).assert());
    assert!(before.contains("No skills installed."), "{before}");

    let complete = root.join("skills/heron-notes");
    let draft = root.join("skills/wren-draft");
    write_skill(
        &complete,
        &format!(
            "---\nname: heron-notes\nversion: '1.0'\ndescription: Survey notes\ncategory: research\n---\n{}",
            "Record the date of the field survey.\n".repeat(84)
        ),
    );
    for directory in ["scripts", "examples", "templates", "tests"] {
        fs::create_dir(complete.join(directory)).unwrap();
    }
    write_skill(&draft, "# Wren draft\n\nTODO write field notes.\n");

    let missing =
        success_text(skills(root, &["config", "heron-notes", "--extra", "region"]).assert());
    assert!(
        missing.contains("No config for skill 'heron-notes'."),
        "{missing}"
    );
    for entry in ["colour=blue", "region=outer=coast"] {
        skills(root, &["config", "heron-notes", "--extra", entry])
            .assert()
            .success();
    }
    let config: Value =
        serde_json::from_str(&fs::read_to_string(complete.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        config,
        serde_json::json!({"colour": "blue", "region": "outer=coast"})
    );
    let region =
        success_text(skills(root, &["config", "heron-notes", "--extra", "region"]).assert());
    assert!(region.contains("region = \"outer=coast\""), "{region}");
    let absent =
        success_text(skills(root, &["config", "heron-notes", "--extra", "unknown"]).assert());
    assert!(absent.contains("Key 'unknown' not found"), "{absent}");
    let listing = success_text(skills(root, &["config", "heron-notes"]).assert());
    assert!(
        listing.contains("Config for skill 'heron-notes':"),
        "{listing}"
    );

    let quality = success_text(skills(root, &["quality"]).assert());
    let best = quality
        .lines()
        .find(|line| line.starts_with("heron-notes"))
        .unwrap();
    let weakest = quality
        .lines()
        .find(|line| line.starts_with("wren-draft"))
        .unwrap();
    assert!(best.contains("100") && best.contains("excellent"), "{best}");
    assert!(weakest.contains("fallback") && weakest.contains("contains_placeholder_markers"));
    assert!(quality.contains("- wren-draft: run"), "{quality}");

    let clean = success_text(skills(root, &["audit"]).assert());
    assert!(
        clean.contains("Audited 2 skill(s)") && clean.contains("All clear"),
        "{clean}"
    );
    fs::create_dir(draft.join("docs")).unwrap();
    fs::write(
        draft.join("docs/service.md"),
        "Fixture service http://127.0.0.1:8080.",
    )
    .unwrap();
    let audited = success_text(skills(root, &["audit"]).assert());
    assert!(audited.contains("heron-notes — clean"), "{audited}");
    assert!(
        audited.contains("[WARN] docs/service.md — Internal network URL"),
        "{audited}"
    );
    assert!(
        audited.contains("Critical: 0") && audited.contains("Warnings: 1"),
        "{audited}"
    );

    let updates = success_text(skills(root, &["update"]).assert());
    assert!(
        updates.contains("No hub-installed skills tracked"),
        "{updates}"
    );
}

#[test]
fn skills_cli_tap_add_and_remove_preserve_the_default_catalogue() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let url = "https://catalogue.example.invalid/skills";
    let list = || success_text(skills(root, &["tap", "list"]).assert());
    let original = list();
    assert!(original.contains("Skill taps:"), "{original}");
    skills(root, &["tap", "add"]).assert().failure();
    let added = success_text(skills(root, &["tap", "add", "--extra", url]).assert());
    assert!(added.contains("Added tap:"), "{added}");
    assert_eq!(list().matches(url).count(), 1);
    let duplicate = success_text(skills(root, &["tap", "add", "--extra", url]).assert());
    assert!(duplicate.contains("Tap already exists"), "{duplicate}");
    assert_eq!(list().matches(url).count(), 1);
    let removed = success_text(skills(root, &["tap", "remove", "--extra", url]).assert());
    assert!(removed.contains("Removed tap:"), "{removed}");
    let missing = success_text(skills(root, &["tap", "remove", "--extra", url]).assert());
    assert!(missing.contains("Tap not found"), "{missing}");
    assert_eq!(list(), original);
}
