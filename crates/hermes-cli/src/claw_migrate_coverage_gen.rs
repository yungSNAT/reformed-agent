use super::*;
use std::ffi::OsString;
use std::fs;

fn migrate_in_home(source: &Path, home: &Path) -> MigrationResult {
    struct RestoreEnv([(&'static str, Option<OsString>); 3]);

    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    let _lock = crate::test_env_lock::lock();
    let keys = ["HOME", "HERMES_HOME", "HERMES_AGENT_ULTRA_HOME"];
    let _restore = RestoreEnv(keys.map(|key| (key, std::env::var_os(key))));
    for key in keys {
        std::env::set_var(key, home);
    }

    run_migration(&MigrateOptions {
        source: Some(source.to_path_buf()),
        ..MigrateOptions::default()
    })
}

fn assert_item(
    items: &[MigrationItem],
    kind: &str,
    status: MigrationStatus,
    source: &Path,
    destination: Option<&Path>,
    reason: &str,
) {
    let item = items
        .iter()
        .find(|item| item.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind} in {items:?}"));
    assert_eq!(item.status, status);
    assert_eq!(item.source.as_deref(), Some(source));
    assert_eq!(item.destination.as_deref(), destination);
    assert_eq!(item.reason.as_deref(), Some(reason));
}

#[test]
fn an_explicit_regular_file_is_not_a_source_directory() {
    if run_isolated("an_explicit_regular_file_is_not_a_source_directory") {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("not-a-directory");
    fs::write(&file, "fixture\n").unwrap();

    assert_eq!(find_openclaw_dir(Some(&file)), Ok(None));
}

#[test]
fn directories_and_existing_files_receive_distinct_migration_statuses() {
    if run_isolated("directories_and_existing_files_receive_distinct_migration_statuses") {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let home = temp.path().join("home");
    fs::create_dir_all(source.join("SOUL.md")).unwrap();
    fs::create_dir_all(source.join("sessions/nested")).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(source.join("sessions/nested/ignored.jsonl"), "nested\n").unwrap();
    fs::write(source.join("sessions/kept.jsonl"), "session\n").unwrap();
    fs::write(source.join("USER.md"), "incoming user\n").unwrap();
    fs::write(home.join("USER.md"), "local user\n").unwrap();

    let result = migrate_in_home(&source, &home);

    assert_eq!(result.errors.len(), 1, "{result:?}");
    assert_item(
        &result.errors,
        "personality:SOUL.md",
        MigrationStatus::Error,
        &source.join("SOUL.md"),
        Some(&home.join("SOUL.md")),
        "it is a folder",
    );
    assert_item(
        &result.skipped,
        "personality:USER.md",
        MigrationStatus::Conflict,
        &source.join("USER.md"),
        Some(&home.join("USER.md")),
        "Already exists",
    );
    assert_item(
        &result.skipped,
        "session:nested",
        MigrationStatus::Skipped,
        &source.join("sessions/nested"),
        None,
        "it is a folder; only session files are copied",
    );
    assert!(!home.join("SOUL.md").exists());
    assert!(!home.join("sessions/nested").exists());
    assert_eq!(
        fs::read_to_string(home.join("USER.md")).unwrap(),
        "local user\n"
    );
    assert_eq!(
        fs::read_to_string(home.join("sessions/kept.jsonl")).unwrap(),
        "session\n"
    );
}

#[test]
fn a_collection_that_is_not_a_directory_is_reported_without_visiting_entries() {
    if run_isolated("a_collection_that_is_not_a_directory_is_reported_without_visiting_entries") {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path();
    let skills = source.join("skills");
    fs::write(&skills, "not a directory\n").unwrap();
    let root = SourceDir::open(source).unwrap();
    let mut result = MigrationResult {
        migrated: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };

    for_each_entry(&root, source, "skills", "skill", &mut result, |_, _| {
        panic!("a collection file must not reach the visitor");
    });

    assert_eq!(result.errors.len(), 1, "{result:?}");
    assert_item(
        &result.errors,
        "skills",
        MigrationStatus::Error,
        &skills,
        None,
        "it is not a folder",
    );
    assert!(result.migrated.is_empty());
    assert!(result.skipped.is_empty());

    #[cfg(unix)]
    {
        let actual = source.join("actual");
        let link = source.join("sessions");
        fs::create_dir(&actual).unwrap();
        fs::write(actual.join("entry.jsonl"), "fixture\n").unwrap();
        std::os::unix::fs::symlink(&actual, &link).unwrap();

        for_each_entry(&root, source, "sessions", "session", &mut result, |_, _| {
            panic!("a linked collection must not reach the visitor")
        });

        assert_eq!(result.errors.len(), 1, "{result:?}");
        assert_eq!(result.skipped.len(), 1, "{result:?}");
        assert_item(
            &result.skipped,
            "sessions",
            MigrationStatus::Skipped,
            &link,
            None,
            "it is a link, which this migration does not follow",
        );
        assert!(result.migrated.is_empty());
    }
}

#[test]
fn an_entry_removed_after_listing_is_reported_as_an_error() {
    if run_isolated("an_entry_removed_after_listing_is_reported_as_an_error") {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path();
    let sessions = source.join("sessions");
    fs::create_dir(&sessions).unwrap();
    let names = ["first.jsonl", "second.jsonl"];
    for name in names {
        fs::write(sessions.join(name), "session\n").unwrap();
    }
    let root = SourceDir::open(source).unwrap();
    let mut result = MigrationResult {
        migrated: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };
    let mut removed = None;

    for_each_entry(
        &root,
        source,
        "sessions",
        "session",
        &mut result,
        |entry, name| {
            assert!(matches!(entry, Entry::File(_)));
            assert!(removed.is_none(), "only the first entry should be visited");
            // Both names have been listed. Remove whichever has not been visited.
            let other = names
                .into_iter()
                .find(|candidate| OsStr::new(candidate) != name)
                .unwrap();
            fs::remove_file(sessions.join(other)).unwrap();
            removed = Some(other);
            (None, Ok(None))
        },
    );

    let removed = removed.unwrap();
    assert_eq!(result.migrated.len(), 1, "{result:?}");
    assert!(result.skipped.is_empty());
    assert_eq!(result.errors.len(), 1, "{result:?}");
    assert_item(
        &result.errors,
        &format!("session:{removed}"),
        MigrationStatus::Error,
        &sessions.join(removed),
        None,
        "it went away while it was read",
    );
}

#[test]
fn a_skill_preview_creates_nothing_and_overwrite_cannot_replace_a_skill() {
    if run_isolated("a_skill_preview_creates_nothing_and_overwrite_cannot_replace_a_skill") {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("skill");
    let home = temp.path().join("home");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(source.join("SKILL.md"), "Incoming notes.\n").unwrap();
    let skill = SourceDir::open(&source).unwrap();
    let imports = home.join("imports");
    let dst = imports.join("notes");

    assert_eq!(
        migrate_skill(
            &skill,
            &home,
            &dst,
            &MigrateOptions {
                dry_run: true,
                ..MigrateOptions::default()
            },
        ),
        Ok(Some("(dry run)".to_owned()))
    );
    assert!(!imports.exists());

    fs::create_dir_all(&dst).unwrap();
    fs::write(dst.join("SKILL.md"), "Local notes.\n").unwrap();
    assert_eq!(
        migrate_skill(
            &skill,
            &home,
            &dst,
            &MigrateOptions {
                overwrite: true,
                ..MigrateOptions::default()
            },
        ),
        Err((MigrationStatus::Conflict, "Already exists".to_owned()))
    );
    assert_eq!(
        fs::read_to_string(dst.join("SKILL.md")).unwrap(),
        "Local notes.\n"
    );
}

#[test]
fn malformed_utf8_in_the_source_env_is_rejected_without_creating_a_destination() {
    if run_isolated("malformed_utf8_in_the_source_env_is_rejected_without_creating_a_destination") {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    fs::write(temp.path().join(".env"), b"TEST_KEY=\xff\n").unwrap();
    let root = SourceDir::open(temp.path()).unwrap();
    let entry = root.entry(".env").unwrap().unwrap();
    let dst = home.join(".env");

    let (status, reason) = migrate_env(entry, &home, &dst, &MigrateOptions::default()).unwrap_err();

    assert_eq!(status, MigrationStatus::Error);
    assert!(
        reason.starts_with("Failed to read source .env:"),
        "{reason}"
    );
    assert!(!dst.exists());
}

#[test]
fn an_existing_env_conflicts_by_default_and_an_overwrite_preview_leaves_it_unchanged() {
    if run_isolated(
        "an_existing_env_conflicts_by_default_and_an_overwrite_preview_leaves_it_unchanged",
    ) {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    fs::write(temp.path().join(".env"), "NEW_KEY=incoming\n").unwrap();
    let root = SourceDir::open(temp.path()).unwrap();
    let dst = home.join(".env");
    let existing = "LOCAL_KEY=local\n";
    fs::write(&dst, existing).unwrap();

    assert_eq!(
        migrate_env(
            root.entry(".env").unwrap().unwrap(),
            &home,
            &dst,
            &MigrateOptions::default(),
        ),
        Err((
            MigrationStatus::Conflict,
            "Already exists (use --overwrite)".to_owned(),
        ))
    );
    assert_eq!(fs::read_to_string(&dst).unwrap(), existing);

    assert_eq!(
        migrate_env(
            root.entry(".env").unwrap().unwrap(),
            &home,
            &dst,
            &MigrateOptions {
                dry_run: true,
                overwrite: true,
                ..MigrateOptions::default()
            },
        ),
        Ok(Some("(dry run)".to_owned()))
    );
    assert_eq!(fs::read_to_string(&dst).unwrap(), existing);
}

#[test]
fn env_merge_ignores_non_assignments_and_separates_imports_from_unterminated_content() {
    if run_isolated(
        "env_merge_ignores_non_assignments_and_separates_imports_from_unterminated_content",
    ) {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let dst = temp.path().join(".env");
    fs::write(&dst, "KEEP=local").unwrap();
    let ignored = "# COMMENTED=unused\nnot an assignment\n KEEP =incoming\n";

    assert_eq!(merge_env_files(ignored, &dst), Ok(0));
    assert_eq!(fs::read_to_string(&dst).unwrap(), "KEEP=local");

    let source = format!("{ignored}NEW=left=right\n");
    assert_eq!(merge_env_files(&source, &dst), Ok(1));
    assert_eq!(
        fs::read_to_string(&dst).unwrap(),
        "KEEP=local\n# Imported from OpenClaw\nNEW=left=right\n"
    );
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path
        .split_once("::")
        .map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
        .output()
        .expect("run the isolated test");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}
