use super::*;
use tempfile::TempDir;

fn fixture() -> (TempDir, DiskCleanup) {
    let tmp = TempDir::new().unwrap();
    let home = fs::canonicalize(tmp.path()).unwrap().join("hermes-home");
    fs::create_dir(&home).unwrap();
    (tmp, DiskCleanup::new(home))
}

fn put_file(cleaner: &DiskCleanup, relative: &str, contents: &str) -> PathBuf {
    let path = cleaner.hermes_home().join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

fn old_item(path: &Path, category: &str, size: u64) -> TrackedItem {
    TrackedItem {
        path: path.to_str().unwrap().to_owned(),
        timestamp: "2000-01-01T00:00:00Z".to_owned(),
        category: category.to_owned(),
        size,
    }
}

#[test]
fn categorizes_paths_before_the_home_exists() {
    if run_isolated("categorizes_paths_before_the_home_exists") {
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("missing-home");
    let cleaner = DiskCleanup::new(&home);

    assert!(cleaner.is_safe_path(&home.join("cache/item")));
    for (relative, expected) in [
        ("logs/test_protected.js", None),
        ("cache/test_cached.js", Some("temp")),
        ("worker.test.ts", Some("test")),
        ("tmp_worker.log", Some("test")),
        ("notes.md", None),
        ("cronjobs/output/run.log", Some("cron-output")),
        ("cronjobs/test_config.js", None),
    ] {
        assert_eq!(
            cleaner.guess_category(&home.join(relative)),
            expected,
            "{relative}"
        );
    }

    for relative in ["cronjobs", "cronjobs/jobs.json", "cronjobs/.tick.lock"] {
        assert!(cleaner.is_protected_cron_path(&home.join(relative)));
    }
    assert!(!cleaner.is_protected_cron_path(&home.join("cronjobs/output/jobs.json")));
    assert!(!cleaner.is_protected_cron_path(&root.join("cronjobs/jobs.json")));

    let relative = Path::new("scratch/report.txt");
    assert_eq!(
        absolute_path(relative).unwrap(),
        std::env::current_dir().unwrap().join(relative)
    );
    assert_eq!(normalized_path(&home).unwrap(), home);
    assert!(!home.exists());
}

#[test]
fn temp_scope_matcher_requires_a_hermes_prefix_immediately_after_tmp() {
    if run_isolated("temp_scope_matcher_requires_a_hermes_prefix_immediately_after_tmp") {
        return;
    }

    // A leading slash adds a RootDir component before the names this helper matches.
    for path in ["tmp/hermes-run", "private/tmp/hermes-run/nested/file"] {
        assert!(is_tmp_hermes_path(Path::new(path)), "{path}");
    }
    for path in [
        "tmp",
        "tmp/hermes",
        "tmp/other/hermes-run",
        "private/tmp/ordinary",
        "var/tmp/hermes-run",
    ] {
        assert!(!is_tmp_hermes_path(Path::new(path)), "{path}");
    }
}

#[test]
fn loads_last_good_backup_or_starts_empty_when_both_copies_are_corrupt() {
    if run_isolated("loads_last_good_backup_or_starts_empty_when_both_copies_are_corrupt") {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let old = old_item(&cleaner.hermes_home().join("old.log"), "temp", 11);
    let new = old_item(&cleaner.hermes_home().join("new.log"), "research", 24);
    cleaner.save_tracked(std::slice::from_ref(&old)).unwrap();
    cleaner.save_tracked(std::slice::from_ref(&new)).unwrap();
    assert_eq!(cleaner.load_tracked(), [new]);

    let tracked = cleaner.tracked_file();
    fs::write(&tracked, "{broken").unwrap();
    assert_eq!(cleaner.load_tracked(), [old]);
    let log = fs::read_to_string(cleaner.log_file()).unwrap();
    assert!(log.contains("WARN: tracked.json corrupted - restored from .bak"));
    assert!(!log.contains("starting fresh"));

    fs::write(backup_path(&tracked), "{}").unwrap();
    assert!(cleaner.load_tracked().is_empty());
    let log = fs::read_to_string(cleaner.log_file()).unwrap();
    assert!(log.contains("WARN: tracked.json corrupted, no backup - starting fresh"));
}

#[test]
fn track_normalizes_unknown_categories_and_audits_rejections() {
    if run_isolated("track_normalizes_unknown_categories_and_audits_rejections") {
        return;
    }

    let (tmp, cleaner) = fixture();
    let local = put_file(&cleaner, "sample.dat", "hello");
    let missing = cleaner.hermes_home().join("missing.dat");
    let outside = tmp.path().join("outside.dat");
    fs::write(&outside, "outside").unwrap();

    assert!(cleaner.track(&local, "unrecognized").unwrap());
    assert!(!cleaner.track(&local, "test").unwrap());
    assert!(!cleaner.track(&missing, "temp").unwrap());
    assert!(!cleaner.track(&outside, "test").unwrap());

    let tracked = cleaner.load_tracked();
    assert_eq!(tracked.len(), 1);
    assert_eq!(tracked[0].path, local.to_str().unwrap());
    assert_eq!(tracked[0].category, "other");
    assert_eq!(tracked[0].size, 5);

    let log = fs::read_to_string(cleaner.log_file()).unwrap();
    assert!(log.contains("WARN: unknown category 'unrecognized', using 'other'"));
    assert!(log.contains(&format!("SKIP: {} (does not exist)", missing.display())));
    let outside = fs::canonicalize(outside).unwrap();
    assert!(log.contains(&format!(
        "REJECT: {} (outside HERMES_HOME)",
        outside.display()
    )));
}

#[test]
fn a_file_at_the_state_directory_blocks_saves_but_does_not_break_audit_callers() {
    if run_isolated("a_file_at_the_state_directory_blocks_saves_but_does_not_break_audit_callers") {
        return;
    }

    let (_tmp, cleaner) = fixture();
    fs::write(cleaner.state_dir(), "leave this alone").unwrap();

    let error = cleaner.save_tracked(&[]).unwrap_err();
    assert!(matches!(error, ToolError::ExecutionFailed(message) if !message.is_empty()));
    assert!(!cleaner
        .track(&cleaner.hermes_home().join("absent"), "temp")
        .unwrap());
    assert!(!cleaner.log_file().exists());
    assert_eq!(
        fs::read_to_string(cleaner.state_dir()).unwrap(),
        "leave this alone"
    );
}

#[test]
fn dry_run_separates_auto_deletions_prompts_and_exclusions() {
    if run_isolated("dry_run_separates_auto_deletions_prompts_and_exclusions") {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let mut invalid_timestamp = old_item(&put_file(&cleaner, "recent.tmp", "x"), "temp", 1);
    invalid_timestamp.timestamp = "not a timestamp".to_owned();

    let auto = vec![
        old_item(&put_file(&cleaner, "test_run.rs", "x"), "test", 1),
        old_item(&put_file(&cleaner, "old.tmp", "xx"), "temp", 2),
        old_item(
            &put_file(&cleaner, "cron/output/run.log", "xxx"),
            "cron-output",
            3,
        ),
    ];
    let prompt = vec![
        old_item(&put_file(&cleaner, "notes.md", "x"), "research", 1),
        old_item(&put_file(&cleaner, "profile", "x"), "chrome-profile", 1),
        old_item(
            &put_file(&cleaner, "archive.bin", "x"),
            "download",
            LARGE_FILE_BYTES + 1,
        ),
    ];
    let excluded = [
        old_item(&cleaner.hermes_home().join("gone.rs"), "test", 10),
        old_item(
            &put_file(&cleaner, "moved-output.log", "x"),
            "cron-output",
            1,
        ),
        old_item(&put_file(&cleaner, "cronjobs/jobs.json", "[]"), "test", 2),
        old_item(
            &put_file(&cleaner, "at-limit.bin", "x"),
            "download",
            LARGE_FILE_BYTES,
        ),
        invalid_timestamp,
    ];
    let tracked = [auto.as_slice(), prompt.as_slice(), &excluded].concat();
    cleaner.save_tracked(&tracked).unwrap();

    assert_eq!(cleaner.dry_run(), (auto, prompt));
    assert_eq!(cleaner.load_tracked(), tracked);
    assert!(Path::new(&tracked[0].path).is_file());
}

#[test]
fn deep_without_confirmation_removes_test_tree_and_stale_entry_but_keeps_research() {
    if run_isolated(
        "deep_without_confirmation_removes_test_tree_and_stale_entry_but_keeps_research",
    ) {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let nested = put_file(&cleaner, "bundle/child/result.log", "result");
    let bundle = cleaner.hermes_home().join("bundle");
    let missing = cleaner.hermes_home().join("gone.tmp");
    let research = put_file(&cleaner, "notes.md", "notes");
    let keep = old_item(&research, "research", 5);
    cleaner
        .save_tracked(&[
            old_item(&bundle, "test", 7),
            old_item(&missing, "temp", 100),
            keep.clone(),
        ])
        .unwrap();

    assert_eq!(
        cleaner.deep_without_confirm(),
        DeepSummary {
            quick: CleanupSummary {
                deleted: 1,
                empty_dirs: 0,
                freed: 7,
                errors: Vec::new(),
            },
            deep_deleted: 0,
            deep_freed: 0,
        }
    );
    assert!(!bundle.exists());
    assert!(!nested.exists());
    assert_eq!(fs::read_to_string(research).unwrap(), "notes");
    assert_eq!(cleaner.load_tracked(), [keep]);
    let log = fs::read_to_string(cleaner.log_file()).unwrap();
    assert!(log.contains(&format!(
        "STALE: {} (removed from tracking)",
        missing.display()
    )));
}

#[test]
fn status_counts_missing_items_but_ranks_only_existing_paths() {
    if run_isolated("status_counts_missing_items_but_ranks_only_existing_paths") {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let mut tracked: Vec<_> = (0..11)
        .rev()
        .map(|i| {
            let path = put_file(&cleaner, &format!("file-{i:02}.dat"), "x");
            old_item(&path, "temp", 1024)
        })
        .collect();
    let largest = put_file(&cleaner, "largest.dat", "x");
    tracked.push(old_item(&largest, "download", 4096));
    tracked.push(old_item(
        &cleaner.hermes_home().join("missing-big.dat"),
        "download",
        20 * 1024,
    ));
    tracked.push(old_item(
        &cleaner.hermes_home().join("missing-test.dat"),
        "test",
        11 * 1024,
    ));
    cleaner.save_tracked(&tracked).unwrap();

    let status = cleaner.status();
    assert_eq!(status.total_tracked, 14);
    assert_eq!(status.categories.len(), 3);
    assert_eq!(
        status.categories["download"],
        CategorySummary {
            count: 2,
            size: 24 * 1024,
        }
    );
    assert_eq!(
        status.categories["temp"],
        CategorySummary {
            count: 11,
            size: 11 * 1024,
        }
    );
    assert_eq!(status.top10.len(), 10);
    assert_eq!(
        status.top10[0],
        TopTrackedItem {
            path: largest.to_str().unwrap().to_owned(),
            size: 4096,
            category: "download".to_owned(),
        }
    );
    for (i, item) in status.top10[1..].iter().enumerate() {
        let expected = cleaner.hermes_home().join(format!("file-{i:02}.dat"));
        assert_eq!(item.path, expected.to_str().unwrap());
        assert_eq!(item.size, 1024);
        assert_eq!(item.category, "temp");
    }

    let formatted = handle_slash_args(&cleaner, &["status"]);
    let rows: Vec<_> = formatted.lines().collect();
    assert_eq!(
        rows[0].split_whitespace().collect::<Vec<_>>(),
        ["Category", "Files", "Size"]
    );
    assert_eq!(rows[1], "-".repeat(40));
    assert_eq!(
        rows[2].split_whitespace().collect::<Vec<_>>(),
        ["download", "2", "24.0", "KB"]
    );
    assert_eq!(
        rows[3].split_whitespace().collect::<Vec<_>>(),
        ["temp", "11", "11.0", "KB"]
    );
    assert_eq!(
        rows[4].split_whitespace().collect::<Vec<_>>(),
        ["test", "1", "11.0", "KB"]
    );
    assert_eq!(rows[6], "Top 10 largest tracked files:");
    assert_eq!(
        rows[7],
        format!("   1.   4.0 KB  [download]  {}", largest.display())
    );
    assert_eq!(rows.len(), 17);
    assert!(!formatted.contains("missing-"));
}

#[test]
fn empty_status_and_binary_size_units_are_rendered() {
    if run_isolated("empty_status_and_binary_size_units_are_rendered") {
        return;
    }

    let empty = format_status(&StatusSummary::default());
    assert!(empty.contains("\n(nothing tracked yet)\n"));
    assert!(empty.ends_with("\nTop 10 largest tracked files:\n  (none)"));

    for (bytes, expected) in [
        (0, "0.0 B"),
        (1023, "1023.0 B"),
        (1536, "1.5 KB"),
        (1024_u64.pow(2), "1.0 MB"),
        (1024_u64.pow(3), "1.0 GB"),
        (1024_u64.pow(4), "1.0 TB"),
        (1024_u64.pow(5), "1.0 PB"),
    ] {
        assert_eq!(fmt_size(bytes), expected);
    }
}

#[test]
fn slash_preview_lists_candidates_and_deep_only_deletes_automatic_items() {
    if run_isolated("slash_preview_lists_candidates_and_deep_only_deletes_automatic_items") {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let test_file = put_file(&cleaner, "test_scan.rs", "abc");
    let research = put_file(&cleaner, "draft.md", "draft");
    let prompt = old_item(&research, "research", 5);
    cleaner
        .save_tracked(&[old_item(&test_file, "test", 3), prompt.clone()])
        .unwrap();

    assert_eq!(
        handle_slash_args(&cleaner, &["dry-run"]),
        format!(
            "Dry-run preview (nothing deleted):\n  Auto-delete : 1 files (3.0 B)\n    [test] {}\n  Needs prompt: 1 files (5.0 B)\n    [research] {}\n\n  Total potential: 8.0 B",
            test_file.display(),
            research.display()
        )
    );
    assert!(test_file.is_file());
    assert!(research.is_file());

    let deep = handle_slash_args(&cleaner, &["deep"]);
    assert!(deep.starts_with("[disk-cleanup] Cleaned 1 files + 0 empty dirs, freed 3.0 B."));
    assert!(deep.contains(&format!(
        "\n1 item(s) need confirmation (5.0 B):\n  [research] {}",
        research.display()
    )));
    assert!(deep
        .ends_with("Run `/disk-cleanup forget <path>` to skip, or delete manually via terminal."));
    assert!(!test_file.exists());
    assert!(research.is_file());
    assert_eq!(cleaner.load_tracked(), [prompt]);
    assert_eq!(
        handle_slash_args(&cleaner, &["quick"]),
        "[disk-cleanup] Cleaned 0 files + 0 empty dirs, freed 0.0 B."
    );
}

#[test]
fn slash_validates_arguments_and_forgets_without_deleting() {
    if run_isolated("slash_validates_arguments_and_forgets_without_deleting") {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let file = put_file(&cleaner, "keep.dat", "keep");
    let path = file.to_str().unwrap();

    assert_eq!(handle_slash_args(&cleaner, &[]), HELP_TEXT);
    for arg in ["help", "-h", "--help"] {
        assert_eq!(handle_slash_args(&cleaner, &[arg]), HELP_TEXT);
    }
    assert_eq!(
        handle_slash_args(&cleaner, &["unknown"]),
        format!("Unknown subcommand: unknown\n\n{HELP_TEXT}")
    );
    assert_eq!(
        handle_slash_args(&cleaner, &["track", path]),
        "Usage: /disk-cleanup track <path> <category>"
    );
    assert_eq!(
        handle_slash_args(&cleaner, &["track", path, "invalid"]),
        format!("Unknown category 'invalid'. Allowed: {ALLOWED_CATEGORIES:?}")
    );
    assert!(cleaner.load_tracked().is_empty());

    assert_eq!(
        handle_slash_args(&cleaner, &["track", path, "temp"]),
        format!("Tracked {path} as 'temp'.")
    );
    assert_eq!(
        handle_slash_args(&cleaner, &["track", path, "temp"]),
        format!("Not tracked (already present, missing, or outside HERMES_HOME): {path}")
    );
    assert_eq!(
        handle_slash_args(&cleaner, &["forget"]),
        "Usage: /disk-cleanup forget <path>"
    );
    assert_eq!(
        handle_slash_args(&cleaner, &["forget", path]),
        format!("Removed 1 tracking entry for {path}.")
    );
    assert_eq!(
        handle_slash_args(&cleaner, &["forget", path]),
        format!("Not found in tracking: {path}")
    );

    let item = old_item(&file, "temp", 4);
    cleaner.save_tracked(&[item.clone(), item]).unwrap();
    assert_eq!(
        handle_slash_args(&cleaner, &["forget", path]),
        format!("Removed 2 tracking entries for {path}.")
    );
    assert!(cleaner.load_tracked().is_empty());
    assert_eq!(fs::read_to_string(&file).unwrap(), "keep");
}

#[test]
fn slash_reports_save_errors_without_replacing_the_previous_tracking_state() {
    if run_isolated("slash_reports_save_errors_without_replacing_the_previous_tracking_state") {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let kept = put_file(&cleaner, "kept.dat", "old");
    let added = put_file(&cleaner, "added.dat", "new");
    let item = old_item(&kept, "other", 3);
    cleaner.save_tracked(std::slice::from_ref(&item)).unwrap();
    fs::create_dir(backup_path(&cleaner.tracked_file())).unwrap();

    let added_path = added.to_str().unwrap();
    let kept_path = kept.to_str().unwrap();
    let track = handle_slash_args(&cleaner, &["track", added_path, "other"]);
    assert!(track.starts_with(&format!("Failed to track {added_path}: ")));
    let forget = handle_slash_args(&cleaner, &["forget", kept_path]);
    assert!(forget.starts_with(&format!("Failed to forget {kept_path}: ")));
    assert_eq!(
        handle_slash_args(&cleaner, &["quick"]),
        "[disk-cleanup] Cleaned 0 files + 0 empty dirs, freed 0.0 B.\n  1 error(s); see cleanup.log."
    );
    assert_eq!(cleaner.load_tracked(), [item]);
    assert!(kept.is_file());
    assert!(added.is_file());
}

#[test]
fn auto_tracker_does_not_start_session_cleanup_for_existing_tests_or_new_cache_files() {
    if run_isolated(
        "auto_tracker_does_not_start_session_cleanup_for_existing_tests_or_new_cache_files",
    ) {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let test_file = put_file(&cleaner, "existing.test.py", "test");
    let cache_file = put_file(&cleaner, "cache/artifact.bin", "cache");
    assert!(cleaner.track(&test_file, "test").unwrap());
    let tracker = DiskCleanupAutoTracker::new(cleaner.clone());

    tracker.post_tool_call_success("patch", &json!({"path": test_file}), "ok");
    tracker.post_tool_call_success("write_file", &json!({"path": cache_file}), "ok");

    let tracked = cleaner.load_tracked();
    assert_eq!(tracked.len(), 2);
    assert_eq!(tracked[0].path, test_file.to_str().unwrap());
    assert_eq!(tracked[0].category, "test");
    assert_eq!(tracked[1].path, cache_file.to_str().unwrap());
    assert_eq!(tracked[1].category, "temp");
    assert_eq!(tracker.on_session_end(), None);
    assert_eq!(cleaner.load_tracked(), tracked);
    assert!(test_file.is_file());
    assert!(cache_file.is_file());
}

#[tokio::test]
async fn handler_rejects_missing_or_non_string_parameters() {
    if run_isolated("handler_rejects_missing_or_non_string_parameters") {
        return;
    }

    let (_tmp, cleaner) = fixture();
    let handler = DiskCleanupHandler::new(cleaner.clone());
    let path = put_file(&cleaner, "sample.dat", "x");

    for (params, expected) in [
        (json!({}), "Missing 'action' parameter"),
        (json!({"action": false}), "Missing 'action' parameter"),
        (
            json!({"action": "track", "category": "test"}),
            "Missing 'path' parameter",
        ),
        (
            json!({"action": "track", "path": path, "category": 3}),
            "Missing 'category' parameter",
        ),
        (
            json!({"action": "forget", "path": null}),
            "Missing 'path' parameter",
        ),
    ] {
        let result = handler.execute(params).await;
        assert!(
            matches!(&result, Err(ToolError::InvalidParams(message)) if message == expected),
            "expected {expected:?}, got {result:?}"
        );
    }
    assert!(cleaner.load_tracked().is_empty());
    assert!(path.is_file());
}

#[test]
fn directory_walk_prunes_dependencies_and_visits_only_managed_dirs_in_postorder() {
    if run_isolated("directory_walk_prunes_dependencies_and_visits_only_managed_dirs_in_postorder")
    {
        return;
    }

    let (tmp, cleaner) = fixture();
    let home = cleaner.hermes_home();
    let empty = home.join("scratch/empty");
    let dependency = home.join("workspace/node_modules/pkg/empty");
    let protected = home.join("logs/empty");
    for dir in [&empty, &dependency, &protected] {
        fs::create_dir_all(dir).unwrap();
    }
    let file = put_file(&cleaner, "retained.txt", "x");

    #[cfg(unix)]
    let (link, target) = {
        let target = tmp.path().join("outside/empty");
        fs::create_dir_all(&target).unwrap();
        let link = home.join("linked-dir");
        std::os::unix::fs::symlink(target.parent().unwrap(), &link).unwrap();
        (link, target)
    };

    let mut dirs = Vec::new();
    collect_pruned_dirs_postorder(home, &home.join("missing"), &mut dirs);
    assert!(dirs.is_empty());
    collect_pruned_dirs_postorder(&tmp.path().join("unrelated"), home, &mut dirs);
    assert!(dirs.is_empty());
    collect_pruned_dirs_postorder(home, home, &mut dirs);
    dirs.retain(|dir| dir.starts_with(home.join("scratch")));
    assert_eq!(dirs, [empty, home.join("scratch")]);

    assert_eq!(cleaner.remove_empty_dirs(), 2);
    assert!(!home.join("scratch").exists());
    assert!(dependency.is_dir());
    assert!(protected.is_dir());
    assert!(file.is_file());
    #[cfg(unix)]
    {
        assert!(link.is_symlink());
        assert!(target.is_dir());
    }
}

#[test]
fn default_handler_and_path_fallbacks_use_isolated_process_state() {
    if run_isolated("default_handler_and_path_fallbacks_use_isolated_process_state") {
        return;
    }

    const CHILD_HOME: &str = "HERMES_DISK_CLEANUP_COVERAGE_CHILD_HOME";
    const CHECKED: &str = "disk-cleanup isolated checks passed";

    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let home = PathBuf::from(home);
        let handler = DiskCleanupHandler::default();
        assert_eq!(handler.cleaner.hermes_home(), home);
        assert_eq!(
            expand_user("~/scratch/test.log".to_owned()),
            home.join("scratch/test.log")
        );

        #[cfg(unix)]
        {
            // Only this child process uses the directory that is about to disappear.
            let tmp = TempDir::new_in(&home).unwrap();
            let working = tmp.path().join("working");
            fs::create_dir(&working).unwrap();
            std::env::set_current_dir(&working).unwrap();
            fs::remove_dir(&working).unwrap();

            let relative = Path::new("cron/jobs.json");
            assert!(absolute_path(relative).is_err());
            assert!(!handler.cleaner.is_safe_path(relative));
            assert!(!handler.cleaner.is_protected_cron_path(relative));

            std::env::set_current_dir(&home).unwrap();
        }
        println!("{CHECKED}");
        return;
    }

    let (_tmp, cleaner) = fixture();
    let home = cleaner.hermes_home();
    let module = module_path!().split_once("::").unwrap().1;
    let test = format!("{module}::default_handler_and_path_fallbacks_use_isolated_process_state");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test.as_str(), "--nocapture"])
        .env(CHILD_HOME, home)
        .env("HERMES_HOME", home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .current_dir(home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(CHECKED),
        "isolated test did not complete: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
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
