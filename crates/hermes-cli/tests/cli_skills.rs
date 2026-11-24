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
