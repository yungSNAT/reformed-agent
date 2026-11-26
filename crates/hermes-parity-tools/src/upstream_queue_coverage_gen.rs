use super::*;
use serde_json::{json, Value};
use std::fs;

#[test]
fn parses_commit_boundaries_subjects_and_sorted_unique_paths() {
    let commits = commits_from_log(
        "path-before-first-header\r\n\
         __H__ abc \t\u{2003}subject\twith a tab \r\n\
         \r\n\
         z.txt \r\n\
         a.txt\r\n\
         z.txt\r\n\
         __H__def\r\n\
         \u{2003}\r\n\
         last.txt",
    );

    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].sha, "abc");
    assert_eq!(commits[0].subject, "subject\twith a tab");
    assert_eq!(
        commits[0]
            .files
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["a.txt", "z.txt"]
    );
    assert_eq!(commits[1].sha, "def");
    assert_eq!(commits[1].subject, "");
    assert_eq!(
        commits[1]
            .files
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["last.txt"]
    );
    assert!(commits_from_log("loose.txt\n__H__ \tmissing SHA\nignored.txt").is_empty());
}

fn report_row(sha: String, subject: String, ticket: usize, status: &str) -> Row {
    Row {
        sha,
        subject,
        files: Vec::new(),
        ticket,
        disposition: Disposition {
            status: status.to_owned(),
            ..Disposition::default()
        },
        rule: "fixture_rule",
        python_only: false,
        guarded: false,
        rust_primary: false,
    }
}

#[test]
fn reports_full_file_count_but_only_twenty_sample_paths() {
    let mut row = report_row("abc123".to_owned(), "Add tests".to_owned(), 1, "superseded");
    row.files = (0..24)
        .map(|index| format!("tests/test_{index:02}.py"))
        .collect();
    row.disposition.owner = "parity-team".to_owned();
    row.disposition.notes = "Use Rust tests".to_owned();
    row.python_only = true;
    row.guarded = true;

    let report: Value =
        serde_json::from_str(&py::dumps(&row.report(false), Some(2), true)).unwrap();
    let sample: Vec<_> = (0..20)
        .map(|index| format!("tests/test_{index:02}.py"))
        .collect();
    assert_eq!(
        report,
        json!({
            "sha": "abc123",
            "subject": "Add tests",
            "target_ticket": 1,
            "target_ticket_name": ticket_name(1),
            "files_touched": 24,
            "files_sample": sample,
            "disposition": "superseded",
            "owner": "parity-team",
            "notes": "Use Rust tests",
            "rust_only_guard": {
                "python_test_only_commit": true,
                "superseded_by_guard": true,
                "allow_python_test_surfaces": false,
                "rust_primary_surface_only_commit": false
            },
            "classification_rule": "fixture_rule"
        })
    );
}

#[test]
fn markdown_counts_all_rows_but_displays_only_the_first_hundred_pending() {
    let mut rows = vec![report_row(
        "finished".to_owned(),
        "Completed work".to_owned(),
        2,
        "ported",
    )];
    rows.extend((0..101).map(|index| {
        report_row(
            format!("é{index:011}suffix"),
            format!("Pending | {index}"),
            1,
            "pending",
        )
    }));

    let counts = Counts::summarise(&rows);
    assert_eq!(counts.tickets, BTreeMap::from([(1, 101), (2, 1)]));
    assert_eq!(
        counts.dispositions,
        BTreeMap::from([("pending", 101), ("ported", 1)])
    );

    let text = markdown(
        "2025-01-01T00:00:00+00:00",
        "main..upstream/main",
        &rows,
        &counts,
    );
    assert!(text.contains("Generated: `2025-01-01T00:00:00+00:00`"));
    assert!(text.contains("Range: `main..upstream/main`; total commits tracked: `102`."));
    assert!(text.contains(&format!("| #1 | {} | 101 |", ticket_name(1))));
    assert!(text.contains("| pending | 101 |\n| ported | 1 |"));
    assert_eq!(
        text.lines().filter(|line| line.starts_with("| `")).count(),
        100
    );
    assert!(text.contains("| `é00000000000` | #1 | Pending \\| 0 |"));
    assert!(text.contains("| `é00000000099` | #1 | Pending \\| 99 |"));
    assert!(!text.contains("Pending \\| 100"));
    assert!(!text.contains("Completed work"));
    assert!(!text.contains("_(none)_"));

    let done = &rows[..1];
    let text = markdown("now", "main..upstream/main", done, &Counts::summarise(done));
    assert!(text.ends_with("| _(none)_ | - | - |\n"));
}

fn git_ok(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn init_repo(root: &Path) {
    fs::create_dir_all(root).unwrap();
    git_ok(
        root,
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git_ok(root, &["config", "user.name", "Queue Coverage"]);
    git_ok(
        root,
        &["config", "user.email", "queue-coverage@example.invalid"],
    );
    git_ok(root, &["config", "commit.gpgsign", "false"]);
}

fn commit_file(root: &Path, path: &str, contents: &[u8], subject: &str) -> String {
    let full_path = root.join(path);
    fs::create_dir_all(full_path.parent().unwrap()).unwrap();
    fs::write(full_path, contents).unwrap();
    git_ok(root, &["add", "--", path]);
    git_ok(root, &["commit", "--quiet", "-m", subject]);
    git_ok(root, &["rev-parse", "HEAD"])
}

fn queue_commit(sha: &str, subject: &str, paths: &[&str]) -> Commit {
    Commit {
        sha: sha.to_owned(),
        subject: subject.to_owned(),
        files: paths.iter().map(|path| (*path).to_owned()).collect(),
    }
}

fn check_blobs_and_row_precedence(root: &Path) {
    let binary_path = "docs/shared.bin";
    let binary = b"shared\0\xff\n";
    let shared_test = "tests/test_queue_shared.py";
    let incoming_test = "tests/test_queue_incoming.py";
    let incoming_doc = "docs/queue-incoming.txt";

    init_repo(root);
    let base = commit_file(root, binary_path, binary, "Common base");
    commit_file(
        root,
        shared_test,
        b"def test_shared(): pass\n",
        "Local patch",
    );
    git_ok(root, &["checkout", "--quiet", "-b", "incoming", &base]);
    let cherry = commit_file(
        root,
        shared_test,
        b"def test_shared(): pass\n",
        "Upstream patch",
    );
    let changed = commit_file(root, incoming_doc, b"upstream only\n", "Incoming docs");
    let test_only = commit_file(
        root,
        incoming_test,
        b"def test_incoming(): pass\n",
        "Incoming test",
    );

    assert_eq!(
        blob(root, "main", binary_path).unwrap().as_deref(),
        Some(binary.as_slice())
    );
    assert_eq!(blob(root, "main", incoming_doc).unwrap(), None);
    assert_eq!(
        blob(root, "incoming", incoming_doc).unwrap(),
        Some(b"upstream only\n".to_vec())
    );
    let error = blob(&root.join("missing-directory"), "main", binary_path).unwrap_err();
    assert!(error.starts_with("git show main:docs/shared.bin:"));

    let patches = equivalent_patches(root, "main", "incoming").unwrap();
    assert_eq!(patches, HashSet::from([cherry.clone()]));
    assert!(equivalent_patches(root, "absent-ref", "incoming")
        .unwrap()
        .is_empty());

    let mut prior = Prior::load(root, "main", &root.join("no-prior.json")).unwrap();
    let saved = prior.by_sha.entry("saved".to_owned()).or_default();
    saved.status = "deferred".to_owned();
    saved.owner = "maintainer".to_owned();
    saved.notes = "Keep this disposition".to_owned();
    let carried = prior.by_sha.entry(cherry.clone()).or_default();
    carried.status.clear();
    carried.notes = "Existing note".to_owned();

    let overrides = Overrides::load(&root.join("no-overrides.json"));
    let queue = Queue {
        root,
        local: "main",
        upstream: "incoming",
        prior: &prior,
        overrides: &overrides,
        patch_equivalent: &patches,
        allow_python_tests: false,
    };

    assert!(!queue.mirrored(&[]).unwrap());
    assert!(queue.mirrored(&[binary_path.to_owned()]).unwrap());
    assert!(!queue
        .mirrored(&[binary_path.to_owned(), incoming_doc.to_owned()])
        .unwrap());

    let row = queue
        .row(queue_commit("saved", "Prior decision", &[incoming_test]))
        .unwrap();
    assert_eq!(row.disposition.status, "deferred");
    assert_eq!(row.disposition.owner, "maintainer");
    assert_eq!(row.disposition.notes, "Keep this disposition");
    assert_eq!(row.rule, "prior_state");
    assert!(row.python_only && row.guarded);

    let row = queue
        .row(queue_commit(&cherry, "Upstream patch", &[shared_test]))
        .unwrap();
    assert!(row.guarded);
    assert_eq!(row.disposition.status, "ported");
    assert_eq!(row.rule, "patch_equivalent_cherry");
    assert!(row.disposition.notes.starts_with("Existing note"));
    assert!(row
        .disposition
        .notes
        .ends_with("patch-equivalent commit already present on main"));

    let row = queue
        .row(queue_commit(&test_only, "Incoming test", &[incoming_test]))
        .unwrap();
    assert_eq!(row.disposition.status, "superseded");
    assert_eq!(row.rule, "rust_only_python_test_guard");
    assert!(row.disposition.notes.contains("Python test-only"));

    let runtime_path = [
        "run_agent.py",
        "hermes_cli/main.py",
        "hermes_cli/cli.py",
        "agent/model_tools.py",
        "agent/agent.py",
        "gateway/run.py",
        "tools/web_tools.py",
        "cron/scheduler.py",
        "cli.py",
    ]
    .into_iter()
    .find(|path| rust_primary_surface_only(&[(*path).to_owned()]))
    .expect("at least one Python runtime path should be a Rust-primary surface");
    let row = queue
        .row(queue_commit("runtime", "Python runtime", &[runtime_path]))
        .unwrap();
    assert!(row.rust_primary);
    assert_eq!(row.disposition.status, "superseded");
    assert_eq!(row.rule, "rust_primary_surface_guard");
    assert!(row.disposition.notes.contains("Rust-native architecture"));

    let row = queue
        .row(queue_commit("mirror", "Same binary", &[binary_path]))
        .unwrap();
    assert_eq!(row.disposition.status, "mirrored");
    assert_eq!(row.rule, "mirrored_file_state");
    assert_eq!(
        row.disposition.notes,
        "all touched files in incoming already mirror main"
    );

    let row = queue
        .row(queue_commit(&changed, "Incoming docs", &[incoming_doc]))
        .unwrap();
    assert_eq!(row.disposition.status, "pending");
    assert_eq!(row.rule, "default_pending");
    assert!(row.disposition.notes.is_empty());

    let permissive = Queue {
        allow_python_tests: true,
        ..queue
    };
    let row = permissive
        .row(queue_commit(&test_only, "Incoming test", &[incoming_test]))
        .unwrap();
    assert!(row.python_only);
    assert!(!row.guarded);
    // Allowing Python tests disables only the dedicated test guard.
    let rust_primary = rust_primary_surface_only(&[incoming_test.to_owned()]);
    assert_eq!(row.rust_primary, rust_primary);
    let (status, rule) = if rust_primary {
        ("superseded", "rust_primary_surface_guard")
    } else {
        ("pending", "default_pending")
    };
    assert_eq!(row.disposition.status, status);
    assert_eq!(row.rule, rule);
}

fn check_run_with_local_remote(sandbox: &Path) {
    let remote = sandbox.join("remote");
    let local = sandbox.join("local");
    let first_path = "docs/first.txt";
    let second_path = "docs/second.txt";
    let third_path = "docs/third.txt";
    let json_path = "reports/queue.json";
    let md_path = "reports/queue.md";

    init_repo(&remote);
    let first = commit_file(&remote, first_path, b"first\n", "First upstream change");
    let second = commit_file(&remote, second_path, b"second\n", "Second upstream change");
    let third = commit_file(&remote, third_path, b"third\n", "Third upstream change");
    init_repo(&local);
    let prior = json!({
        "commits": [{
            "sha": second.as_str(),
            "disposition": "ported",
            "owner": "parity-team",
            "notes": "Recorded in the prior queue"
        }]
    });
    commit_file(
        &local,
        json_path,
        prior.to_string().as_bytes(),
        "Seed prior queue",
    );
    git_ok(&local, &["tag", "prior-snapshot"]);

    let make_args = || UpstreamQueueArgs {
        repo_root: local.to_str().unwrap().to_owned(),
        local_ref: "main".to_owned(),
        upstream_remote: "upstream".to_owned(),
        upstream_url: remote.to_str().unwrap().to_owned(),
        upstream_branch: "main".to_owned(),
        no_fetch: false,
        max_commits: decimal_limit::parse("1").unwrap(),
        out_json: json_path.to_owned(),
        out_md: md_path.to_owned(),
        prior_ref: Some(String::new()),
        no_prior_baseline: false,
        allow_python_test_surfaces: false,
        overrides: "queue-overrides.json".to_owned(),
    };

    let read_json =
        || -> Value { serde_json::from_slice(&fs::read(local.join(json_path)).unwrap()).unwrap() };

    assert_eq!(run(make_args()).unwrap(), 0);
    let report = read_json();
    let refs = &report["refs"];
    assert_eq!(refs["local_ref"], "main");
    assert_eq!(refs["upstream_ref"], "upstream/main");
    assert_eq!(refs["range"], "main..upstream/main");
    assert_eq!(refs["prior_ref"], "main");
    assert_eq!(refs["baseline_start_sha"], second.as_str());
    assert_eq!(refs["skipped_before_baseline"], 1);

    let summary = &report["summary"];
    assert_eq!(summary["total_commits"], 1);
    assert_eq!(summary["patch_equivalent_count"], 0);
    assert_eq!(summary["overrides_path"], "queue-overrides.json");
    assert_eq!(summary["by_disposition"], json!({"ported": 1}));
    let rows = report["commits"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["sha"], second.as_str());
    assert_eq!(row["subject"], "Second upstream change");
    assert_eq!(row["files_sample"], json!([second_path]));
    assert_eq!(row["owner"], "parity-team");
    assert_eq!(row["notes"], "Recorded in the prior queue");
    assert_eq!(row["classification_rule"], "prior_state");
    let ticket = classify_ticket(&[second_path.to_owned()]).to_string();
    assert_eq!(summary["by_target_ticket"][ticket.as_str()], 1);

    let generated = report["generated_at_utc"].as_str().unwrap();
    assert!(!generated.is_empty());
    let md = fs::read_to_string(local.join(md_path)).unwrap();
    assert!(md.contains(&format!("Generated: `{generated}`")));
    assert!(md.contains("total commits tracked: `1`"));
    assert!(md.ends_with("| _(none)_ | - | - |\n"));
    assert!(fs::read(local.join(json_path)).unwrap().ends_with(b"\n"));

    // Advance the remote to verify that --no-fetch keeps the existing tracking ref.
    commit_file(
        &remote,
        "docs/fourth.txt",
        b"fourth\n",
        "Unfetched upstream change",
    );
    let args = UpstreamQueueArgs {
        no_fetch: true,
        max_commits: decimal_limit::parse("0").unwrap(),
        prior_ref: Some("prior-snapshot".to_owned()),
        no_prior_baseline: true,
        allow_python_test_surfaces: true,
        ..make_args()
    };
    assert_eq!(run(args).unwrap(), 0);
    let report = read_json();
    assert_eq!(report["refs"]["prior_ref"], "prior-snapshot");
    assert_eq!(report["refs"]["baseline_start_sha"], "");
    assert_eq!(report["refs"]["skipped_before_baseline"], 0);
    assert_eq!(report["summary"]["total_commits"], 3);
    assert_eq!(
        report["summary"]["by_disposition"],
        json!({"pending": 2, "ported": 1})
    );
    let rows = report["commits"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row["sha"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [first.as_str(), second.as_str(), third.as_str()]
    );
    for row in rows {
        assert_eq!(row["rust_only_guard"]["allow_python_test_surfaces"], true);
    }
    let md = fs::read_to_string(local.join(md_path)).unwrap();
    assert!(md.contains("First upstream change"));
    assert!(md.contains("Third upstream change"));
    assert!(!md.contains("Second upstream change"));
    assert!(!md.contains("_(none)_"));
}

#[test]
fn local_git_queue_behaviour() {
    // Isolate Git even for production commands that inherit the process environment.
    const SANDBOX: &str = "HERMES_PARITY_QUEUE_COVERAGE_SANDBOX";
    if let Some(root) = std::env::var_os(SANDBOX) {
        let root = std::path::PathBuf::from(root);
        check_blobs_and_row_precedence(&root.join("diverged"));
        check_run_with_local_remote(&root);
        return;
    }

    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(format!("{module}::local_git_queue_behaviour"))
        .arg("--nocapture")
        .current_dir(root)
        .env(SANDBOX, root)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("running 1 test"),
        "sandboxed Git test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
