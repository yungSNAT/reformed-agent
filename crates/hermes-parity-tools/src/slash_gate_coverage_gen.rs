use std::path::Path;
use std::process::Command;

use super::*;
use crate::py::Py;

fn run_git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn make_repo(with_upstream: bool) -> Option<tempfile::TempDir> {
    let tmp = tempfile::tempdir().ok()?;
    let root = tmp.path();
    if with_upstream {
        std::fs::create_dir_all(root.join("hermes_cli")).ok()?;
        std::fs::write(
            root.join("hermes_cli/commands.py"),
            "COMMANDS = [\n    CommandDef(\"help\", \"h\"),\n    CommandDef(\"quit\", \"q\"),\n    CommandDef(\"model\", \"m\"),\n]\n",
        )
        .ok()?;
    }
    std::fs::create_dir_all(root.join("crates/hermes-cli/src/commands")).ok()?;
    std::fs::write(
        root.join("crates/hermes-cli/src/commands.rs"),
        "pub const SLASH_COMMANDS: &[&str] = &[\"/help\", \"/extra\"];\n",
    )
    .ok()?;
    std::fs::write(
        root.join("crates/hermes-cli/src/commands/sub.rs"),
        "// sub module without command table\n",
    )
    .ok()?;
    std::fs::write(root.join("README.md"), "readme\n").ok()?;
    if !run_git(root, &["init", "-q"]) {
        return None;
    }
    if !run_git(root, &["add", "-A"]) {
        return None;
    }
    if !run_git(
        root,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ) {
        return None;
    }
    Some(tmp)
}

fn make_args(
    repo_root: &Path,
    local_ref: &str,
    report_path: &str,
    allowlist: &str,
    json: bool,
) -> SlashGateArgs {
    SlashGateArgs {
        repo_root: repo_root.to_str().unwrap().to_string(),
        local_ref: local_ref.to_string(),
        upstream_ref: "HEAD".to_string(),
        allowlist: allowlist.to_string(),
        report_path: report_path.to_string(),
        json,
    }
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn write_allowlist(dir: &Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("allow.json");
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn allowlist_string_value_yields_characters() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_allowlist(
        tmp.path(),
        r#"{"allowed_missing_upstream_commands": "a b"}"#,
    );
    let got = load_allowlist(&p).unwrap();
    let expected: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn allowlist_dict_value_yields_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_allowlist(
        tmp.path(),
        r#"{"allowed_missing_upstream_commands": {"/x": 1, "  ": 2}}"#,
    );
    let got = load_allowlist(&p).unwrap();
    let expected: BTreeSet<String> = ["/x"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn allowlist_non_iterable_values_error() {
    let tmp = tempfile::tempdir().unwrap();
    for (raw, name) in [
        ("null", "NoneType"),
        ("true", "bool"),
        ("3", "int"),
        ("1.5", "float"),
    ] {
        let p = write_allowlist(
            tmp.path(),
            &format!(r#"{{"allowed_missing_upstream_commands": {raw}}}"#),
        );
        let err = load_allowlist(&p).unwrap_err();
        assert_eq!(err, format!("'{name}' object is not iterable"));
    }
}

#[test]
fn default_report_path_is_under_sync_reports() {
    let root = Path::new("/some/repo");
    let p = default_report_path(root);
    assert_eq!(p.parent().unwrap(), root.join(".sync-reports"));
    let name = p.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("upstream-slash-parity-gate-"));
    assert!(name.ends_with(".json"));
}

#[test]
fn write_report_writes_json_and_reports_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let report = Py::dict([("ok", Py::from(false)), ("reason", Py::from("x"))]);
    let p = tmp.path().join("r.json");
    write_report(&p, &report).unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.ends_with('\n'));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(v["reason"], serde_json::Value::String("x".into()));

    let bad = tmp.path().join("missing-dir").join("r.json");
    let err = write_report(&bad, &report).unwrap_err();
    assert!(err.contains("could not write"));
}

#[test]
fn fail_writes_report_and_returns_one() {
    let tmp = tempfile::tempdir().unwrap();
    let report = Py::dict([("ok", Py::from(false))]);
    for json in [false, true] {
        let p = tmp.path().join(format!("fail-{json}.json"));
        let args = make_args(tmp.path(), "HEAD", "", "allow.json", json);
        assert_eq!(fail(&args, &report, &p, "testing").unwrap(), 1);
        assert_eq!(read_json(&p)["ok"], serde_json::Value::Bool(false));
    }
    let bad = tmp.path().join("nope").join("r.json");
    let args = make_args(tmp.path(), "HEAD", "", "allow.json", false);
    assert!(fail(&args, &report, &bad, "testing").is_err());
}

#[test]
fn git_helpers_on_real_repo() {
    let Some(repo) = make_repo(true) else {
        return;
    };
    let root = repo.path();
    assert!(git_ref_exists(root, "HEAD").unwrap());
    assert!(!git_ref_exists(root, "no-such-ref-xyz").unwrap());

    let shown = git_show_file(root, "HEAD", "hermes_cli/commands.py").unwrap();
    assert!(shown.unwrap().contains("CommandDef(\"help\""));
    assert!(git_show_file(root, "HEAD", "nope.py").unwrap().is_none());

    let surface = git_show_text_surface(
        root,
        "HEAD",
        &[
            "crates/hermes-cli/src/commands.rs",
            "crates/hermes-cli/src/commands",
        ],
    )
    .unwrap()
    .unwrap();
    assert!(surface.contains("SLASH_COMMANDS"));
    assert!(surface.contains("sub module without command table"));

    assert!(
        git_show_text_surface(root, "HEAD", &["missing.rs", "missing_dir"])
            .unwrap()
            .is_none()
    );
}

#[test]
fn run_missing_git_ref_fails() {
    let Some(repo) = make_repo(true) else {
        return;
    };
    let out = tempfile::tempdir().unwrap();
    let rp = out.path().join("nested").join("r.json");
    let args = make_args(
        repo.path(),
        "no-such-ref-xyz",
        rp.to_str().unwrap(),
        "allow.json",
        false,
    );
    assert_eq!(run(args).unwrap(), 1);
    let v = read_json(&rp);
    assert_eq!(
        v["reason"],
        serde_json::Value::String("missing_git_ref".into())
    );
    assert_eq!(
        v["local_ref"],
        serde_json::Value::String("no-such-ref-xyz".into())
    );
}

#[test]
fn run_missing_surface_file_fails() {
    let Some(repo) = make_repo(false) else {
        return;
    };
    let out = tempfile::tempdir().unwrap();
    let rp = out.path().join("r.json");
    let args = make_args(
        repo.path(),
        "HEAD",
        rp.to_str().unwrap(),
        "allow.json",
        true,
    );
    assert_eq!(run(args).unwrap(), 1);
    let v = read_json(&rp);
    assert_eq!(
        v["reason"],
        serde_json::Value::String("missing_surface_file".into())
    );
    assert_eq!(v["missing_upstream_source"], serde_json::Value::Bool(true));
    assert_eq!(v["missing_local_source"], serde_json::Value::Bool(false));
}

#[test]
fn run_reports_unallowlisted_missing_commands() {
    let Some(repo) = make_repo(true) else {
        return;
    };
    write_allowlist(
        repo.path(),
        r#"{"allowed_missing_upstream_commands": ["/model", "/help"]}"#,
    );
    let out = tempfile::tempdir().unwrap();
    let rp = out.path().join("r.json");
    let args = make_args(
        repo.path(),
        "HEAD",
        rp.to_str().unwrap(),
        "allow.json",
        false,
    );
    assert_eq!(run(args).unwrap(), 1);
    let v = read_json(&rp);
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(v["counts"]["upstream"], serde_json::json!(3));
    assert_eq!(v["counts"]["local"], serde_json::json!(2));
    assert_eq!(v["counts"]["missing_total"], serde_json::json!(2));
    assert_eq!(v["counts"]["missing_unallowlisted"], serde_json::json!(1));
    assert_eq!(v["counts"]["local_extra"], serde_json::json!(1));
    assert_eq!(v["counts"]["stale_allowlist_entries"], serde_json::json!(1));
    assert_eq!(v["missing_unallowlisted"], serde_json::json!(["/quit"]));
    assert_eq!(v["allowlisted_missing"], serde_json::json!(["/model"]));
    assert_eq!(v["local_extra"], serde_json::json!(["/extra"]));
    assert_eq!(v["stale_allowlist_entries"], serde_json::json!(["/help"]));
    assert_eq!(v["missing_total"], serde_json::json!(["/model", "/quit"]));
}

#[test]
fn run_passes_with_full_allowlist_and_default_report_path() {
    let Some(repo) = make_repo(true) else {
        return;
    };
    write_allowlist(repo.path(), r#"["/model", "/quit"]"#);
    let args = make_args(repo.path(), "HEAD", "", "allow.json", true);
    assert_eq!(run(args).unwrap(), 0);
    let reports: Vec<_> = std::fs::read_dir(repo.path().join(".sync-reports"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(reports.len(), 1);
    let name = reports[0]
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(name.starts_with("upstream-slash-parity-gate-"));
    let v = read_json(&reports[0]);
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert_eq!(v["counts"]["missing_unallowlisted"], serde_json::json!(0));
}
