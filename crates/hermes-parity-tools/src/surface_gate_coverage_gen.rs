use super::*;
use serde_json::{json, Value};
use std::fs;
use std::process::Command;

fn gate_args(repo_root: &Path) -> SurfaceGateArgs {
    SurfaceGateArgs {
        repo_root: py::path_str(repo_root),
        local_ref: "HEAD".to_owned(),
        local_mode: LocalMode::Ref,
        upstream_ref: "refs/tags/surface-upstream".to_owned(),
        prefix: Vec::new(),
        report_path: String::new(),
        intentional_divergence: DEFAULT_DIVERGENCE_FILE.to_owned(),
        allow_python_test_surfaces: false,
        json: false,
    }
}

#[test]
fn divergence_registry_checks_the_top_level_and_optional_items() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.json");

    fs::write(&path, "[]").unwrap();
    assert_eq!(
        load_divergence_items(dir.path(), "registry.json").unwrap_err(),
        format!(
            "invalid intentional divergence payload: {}",
            py::resolve(&path).display()
        )
    );

    fs::write(&path, r#"{"version":1}"#).unwrap();
    assert!(load_divergence_items(dir.path(), "registry.json")
        .unwrap()
        .is_empty());
}

#[test]
fn divergence_status_rejects_containers_and_skips_other_non_strings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.json");

    for status in ["[]", "{}"] {
        fs::write(&path, format!(r#"{{"items":[{{"status":{status}}}]}}"#)).unwrap();
        assert_eq!(
            load_divergence_items(dir.path(), "registry.json").unwrap_err(),
            format!("unhashable intentional divergence status: {status}")
        );
    }

    fs::write(
        &path,
        r#"{"items":[
            {"status":null},
            {"status":true},
            {"status":42},
            {"id":"no-status"},
            {"id":"kept","status":"temporary","path_prefixes":["docs"]}
        ]}"#,
    )
    .unwrap();

    let active = load_divergence_items(dir.path(), "registry.json").unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, "kept");
    assert_eq!(active[0].status, "temporary");
    assert_eq!(active[0].path_prefixes, vec!["docs".to_owned()]);
}

#[test]
fn gate_reports_missing_root_and_filesystem_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = py::resolve(dir.path());
    let missing = root.join("not-a-repository");
    assert_eq!(
        run(gate_args(&missing)).unwrap_err(),
        format!("repo root does not exist: {}", py::path_str(&missing))
    );

    let file = root.join("not-a-directory");
    fs::write(&file, "unchanged").unwrap();
    let mut args = gate_args(&root);
    args.report_path = py::path_str(&file.join("report.json"));
    let error = report_path(&args, &root).unwrap_err();
    assert!(error.starts_with(&format!("could not create {}: ", file.display())));
    assert_eq!(fs::read_to_string(&file).unwrap(), "unchanged");

    let error =
        write_and_print(&args, Py::dict([("ok", Py::Bool(true))]), &root, "PASSED").unwrap_err();
    assert!(error.starts_with(&format!("could not write {}: ", root.display())));
}

fn fixture_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture_commit(repo: &Path, message: &str) {
    fixture_git(
        repo,
        &[
            "-c",
            "user.name=Surface Gate Fixture",
            "-c",
            "user.email=surface-gate@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

fn read_report(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.ends_with('\n'));
    let report: Value = serde_json::from_str(&text).unwrap();
    assert!(report.get("report_path").is_none());
    report
}

fn exercise_gate_reports(sandbox: &Path) {
    let repo = sandbox.join("repo");
    fs::create_dir(&repo).unwrap();

    // The policy guard must write a report even before Git is initialized.
    let blocked_path = sandbox.join("reports/blocked.json");
    let mut args = gate_args(&repo);
    args.prefix = vec!["./tests//fixture.py".to_owned(), "test".to_owned()];
    args.report_path = py::path_str(&blocked_path);
    args.json = true;
    assert_eq!(run(args).unwrap(), 1);

    let blocked = read_report(&blocked_path);
    assert_eq!(blocked["ok"], false);
    assert_eq!(blocked["reason"], "rust_only_parity_guard_blocked_prefix");
    assert_eq!(blocked["local_ref"], "HEAD");
    assert_eq!(blocked["upstream_ref"], "refs/tags/surface-upstream");
    assert_eq!(
        blocked["policy"],
        json!({
            "rust_only": true,
            "allow_python_test_surfaces": false,
            "blocked_prefixes": ["./tests//fixture.py", "test"]
        })
    );
    assert!(blocked.get("summary").is_none());

    fixture_git(&repo, &["init", "--quiet", "--template="]);
    for name in [
        "docs/kept.md",
        "docs/missing.md",
        "docs/exempt.md",
        "docs-extra/unrelated.md",
        "tests/fixture.py",
    ] {
        let path = repo.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fixture\n").unwrap();
    }
    fixture_git(&repo, &["add", "--", "."]);
    fixture_commit(&repo, "upstream surface");
    fixture_git(&repo, &["update-ref", "refs/tags/surface-upstream", "HEAD"]);
    fixture_git(
        &repo,
        &[
            "rm",
            "--quiet",
            "--",
            "docs/missing.md",
            "docs/exempt.md",
            "tests/fixture.py",
        ],
    );
    fixture_commit(&repo, "local surface");

    // These files are visible in the worktree but absent from HEAD.
    fs::write(repo.join("docs/missing.md"), "restored locally\n").unwrap();
    fs::create_dir_all(repo.join("tests")).unwrap();
    fs::write(repo.join("tests/fixture.py"), "# worktree fixture\n").unwrap();

    let registry = repo.join(DEFAULT_DIVERGENCE_FILE);
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::write(
        registry,
        r#"{"items":[{"id":"known-gap","status":"approved","workstream":"docs",
                      "path_prefixes":["./docs/exempt.md"]}]}"#,
    )
    .unwrap();

    assert_eq!(run(gate_args(&repo)).unwrap(), 1);
    let mut reports = fs::read_dir(repo.join(".sync-reports")).unwrap();
    let default_path = reports.next().unwrap().unwrap().path();
    assert!(reports.next().is_none());
    let name = default_path.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("upstream-surface-coverage-gate-"));
    assert!(name.ends_with(".json"));

    let failed = read_report(&default_path);
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["local_mode"], "ref");
    assert_eq!(failed["missing_paths"], json!(["docs/missing.md"]));
    assert_eq!(
        failed["intentional_divergence_by_id"],
        json!({"known-gap": 1})
    );
    assert_eq!(
        failed["summary"],
        json!({
            "prefixes_checked": DEFAULT_PREFIXES,
            "upstream_file_count": 3,
            "present_locally": 1,
            "raw_missing_total": 2,
            "intentional_divergence_total": 1,
            "missing_total": 1
        })
    );

    let mut args = gate_args(&repo);
    args.local_mode = LocalMode::Worktree;
    args.prefix = vec!["docs".to_owned(), "tests".to_owned()];
    args.allow_python_test_surfaces = true;
    let worktree_path = sandbox.join("reports/worktree.json");
    args.report_path = py::path_str(&worktree_path);
    assert_eq!(run(args).unwrap(), 0);

    let passed = read_report(&worktree_path);
    assert_eq!(passed["ok"], true);
    assert_eq!(passed["local_mode"], "worktree");
    assert_eq!(passed["missing_paths"], json!([]));
    assert_eq!(passed["by_prefix"]["tests"]["present_locally"], 1);
    assert_eq!(passed["summary"]["raw_missing_total"], 1);
    assert_eq!(
        passed["policy"],
        json!({
            "rust_only": true,
            "allow_python_test_surfaces": true,
            "blocked_prefixes": [],
            "intentional_divergence": DEFAULT_DIVERGENCE_FILE,
            "active_divergence_items": 1
        })
    );
}

#[test]
fn gate_writes_and_prints_guard_ref_and_worktree_reports() {
    const SANDBOX: &str = "HERMES_PARITY_SURFACE_GATE_COVERAGE_SANDBOX";
    if let Some(sandbox) = std::env::var_os(SANDBOX) {
        exercise_gate_reports(Path::new(&sandbox));
        return;
    }

    // Isolate the Git subprocesses' home and config, and capture both output modes.
    let dir = tempfile::tempdir().unwrap();
    let sandbox = py::resolve(dir.path());
    let home = sandbox.join("home");
    fs::create_dir(&home).unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            child.env_remove(key);
        }
    }
    let output = child
        .arg("--exact")
        .arg(format!(
            "{module}::gate_writes_and_prints_guard_ref_and_worktree_reports"
        ))
        .arg("--nocapture")
        .env(SANDBOX, &sandbox)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "fixture test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("[upstream-surface-coverage] FAILED (missing=1 checked=3)"));
    assert!(stdout.contains("[upstream-surface-coverage] PASSED (missing=0 checked=4)"));
    let worktree_path = sandbox.join("reports/worktree.json");
    assert!(stdout.contains(&format!(
        "[upstream-surface-coverage] Report: {}",
        py::path_str(&worktree_path)
    )));

    let start = stdout
        .find('{')
        .expect("the guard should print a JSON report");
    let printed = serde_json::Deserializer::from_str(&stdout[start..])
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap();
    let blocked_path = sandbox.join("reports/blocked.json");
    let mut expected = read_report(&blocked_path);
    expected["report_path"] = json!(py::path_str(&blocked_path));
    assert_eq!(printed, expected);
}
