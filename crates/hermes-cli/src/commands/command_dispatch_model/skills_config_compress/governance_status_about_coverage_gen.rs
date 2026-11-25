use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const GOVERNANCE_ENV: &str = "HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE";
const REPO_ROOT_ENV: &str = "HERMES_REPO_ROOT";
const CHILD_CASE_ENV: &str = "HERMES_COVERAGE_GEN_GOVERNANCE_STATUS_ABOUT_CASE";

fn run_isolated_case(test: &str, case: &str, cwd: &Path, key: &str, value: Option<&str>) {
    let (_, module) = module_path!()
        .split_once("::")
        .expect("coverage tests are in a child module");
    let mut command = Command::new(std::env::current_exe().expect("locate the test binary")); // nosemgrep: rust.lang.security.current-exe.current-exe
    command
        .arg("--exact")
        .arg(format!("{module}::{test}"))
        .arg("--nocapture")
        .current_dir(cwd)
        .env(CHILD_CASE_ENV, case);
    if let Some(value) = value {
        command.env(key, value);
    } else {
        command.env_remove(key);
    }

    let output = command.output().expect("run the isolated unit test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "{test} ({case}) exited with {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn governance_mode_parses_aliases_and_renders_canonical_names() {
    use CompactionGovernanceMode::{Advisory, Enforce, Off};

    for (raw, expected) in [
        ("OFF", Off),
        ("disable", Off),
        ("  Disabled\t", Off),
        ("0", Off),
        ("on", Advisory),
        (" ADVISORY ", Advisory),
        ("warn", Advisory),
        ("1", Advisory),
        ("enforce", Enforce),
        ("\tStRiCt\n", Enforce),
    ] {
        assert_eq!(
            CompactionGovernanceMode::parse(raw),
            Some(expected),
            "{raw:?}"
        );
    }
    assert_eq!(CompactionGovernanceMode::parse(" \t "), None);
    assert_eq!(CompactionGovernanceMode::parse("enforce now"), None);

    assert_eq!(Off.as_str(), "off");
    assert_eq!(Advisory.as_str(), "advisory");
    assert_eq!(Enforce.as_str(), "enforce");
}

#[test]
fn governance_mode_reads_environment_and_defaults_to_advisory() {
    if let Ok(case) = std::env::var(CHILD_CASE_ENV) {
        let expected = match case.as_str() {
            "unset" | "invalid" => CompactionGovernanceMode::Advisory,
            "disabled" => CompactionGovernanceMode::Off,
            "strict" => CompactionGovernanceMode::Enforce,
            _ => panic!("unexpected governance case: {case}"),
        };
        assert_eq!(compaction_governance_mode(), expected);
        return;
    }

    let temp = tempfile::tempdir().expect("create isolated working directory");
    for (case, value) in [
        ("unset", None),
        ("invalid", Some("unsupported")),
        ("disabled", Some(" Disabled ")),
        ("strict", Some(" StRiCt ")),
    ] {
        run_isolated_case(
            "governance_mode_reads_environment_and_defaults_to_advisory",
            case,
            temp.path(),
            GOVERNANCE_ENV,
            value,
        );
    }
}

#[test]
fn repo_discovery_uses_override_then_checkout_probes() {
    if let Ok(case) = std::env::var(CHILD_CASE_ENV) {
        let expected = match case.as_str() {
            "explicit" => Some(PathBuf::from("explicit-root")),
            "cwd" => {
                let cwd = std::env::current_dir().expect("read fixture working directory");
                Some(cwd.parent().expect("fixture checkout root").to_path_buf())
            }
            "manifest" => source_checkout_root(Path::new(env!("CARGO_MANIFEST_DIR"))),
            _ => panic!("unexpected repository discovery case: {case}"),
        };
        assert_eq!(discover_repo_root_for_about(), expected);
        return;
    }

    let temp = tempfile::tempdir().expect("create checkout fixture");
    let root = temp.path().join("checkout");
    fs::create_dir_all(root.join("docs/parity")).expect("create parity directory");
    fs::write(root.join("README.md"), "# Fixture checkout\n").expect("write checkout marker");

    let cwd = root.join("nested");
    fs::create_dir_all(cwd.join("explicit-root")).expect("create explicit root");
    let test = "repo_discovery_uses_override_then_checkout_probes";
    run_isolated_case(
        test,
        "explicit",
        &cwd,
        REPO_ROOT_ENV,
        Some("  explicit-root  "),
    );
    run_isolated_case(test, "cwd", &cwd, REPO_ROOT_ENV, Some("missing-root"));

    fs::write(root.join(".cargo-ok"), "").expect("mark checkout as a Cargo cache copy");
    run_isolated_case(test, "manifest", &cwd, REPO_ROOT_ENV, None);
}

#[test]
fn json_paths_traverse_nested_objects_without_coercing_types() {
    let value = serde_json::json!({
        "upstream_ref": "refs/heads/main",
        "summary": {
            "by_disposition": {"pending": 7, "ported": "8", "superseded": -1}
        },
        "release_gate": {"pass": false}
    });

    assert_eq!(json_value_at_path(&value, &[]), Some(&value));
    assert_eq!(
        json_str_at_path(&value, &["upstream_ref"]).as_deref(),
        Some("refs/heads/main")
    );
    assert_eq!(
        json_u64_at_path(&value, &["summary", "by_disposition", "pending"]),
        Some(7)
    );
    assert_eq!(
        json_value_at_path(&value, &["release_gate", "pass"]).and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(json_str_at_path(&value, &["release_gate", "pass"]), None);
    assert_eq!(json_str_at_path(&value, &["missing"]), None);
    assert_eq!(json_value_at_path(&value, &["upstream_ref", "child"]), None);
    for key in ["ported", "superseded", "missing"] {
        assert_eq!(
            json_u64_at_path(&value, &["summary", "by_disposition", key]),
            None
        );
    }
}

#[test]
fn latest_sync_report_filters_names_and_selects_lexically_last() {
    let temp = tempfile::tempdir().expect("create report fixture");
    let dir = temp.path();
    assert_eq!(latest_upstream_sync_report(&dir.join("missing")), None);

    fs::write(dir.join("other-sync-9999.txt"), "").expect("write unrelated report");
    fs::write(dir.join("upstream-sync-9999.txt.bak"), "").expect("write report backup");
    assert_eq!(latest_upstream_sync_report(dir), None);

    let newest = dir.join("upstream-sync-2026-02-01.txt");
    fs::write(&newest, "newest by name").expect("write newest report first");
    fs::write(dir.join("upstream-sync-2026-01-31.txt"), "older by name")
        .expect("write older report second");

    assert_eq!(latest_upstream_sync_report(dir), Some(newest));
}

#[test]
fn sync_report_metadata_stops_at_the_end_of_the_first_pending_block() {
    let temp = tempfile::tempdir().expect("create metadata fixture");
    let path = temp.path().join("upstream-sync-fixture.txt");
    fs::write(
        &path,
        concat!(
            "# Upstream sync report\n",
            " origin_sha : obsolete\n",
            " origin_sha : abc123 \n",
            "upstream_sha: def456\n",
            "timestamp_utc: 2026-02-01T12:30:00Z\n",
            "report-kind: fixture\n",
            "not a key: ignored\n",
            "clé: ignored\n",
            " : ignored\n",
            "  ## Pending Upstream Commits (2)\n",
            "origin_sha: outside the fence\n",
            "  ```  \n",
            "  aaa First commit\n",
            "   \n",
            "  bbb Second commit: follow-up\n",
            "  ```  \n",
            "upstream_sha: after the fence\n",
            "```\n",
            "ccc A commit in a later block\n",
            "```\n",
        ),
    )
    .expect("write report fixture");

    let (metadata, pending) = parse_sync_report_metadata(&path);
    assert_eq!(
        metadata,
        std::collections::HashMap::from([
            ("origin_sha".to_owned(), "abc123".to_owned()),
            ("upstream_sha".to_owned(), "def456".to_owned()),
            (
                "timestamp_utc".to_owned(),
                "2026-02-01T12:30:00Z".to_owned()
            ),
            ("report-kind".to_owned(), "fixture".to_owned()),
        ])
    );
    assert_eq!(pending, 2);

    let (missing_metadata, missing_pending) =
        parse_sync_report_metadata(&temp.path().join("missing.txt"));
    assert!(missing_metadata.is_empty());
    assert_eq!(missing_pending, 0);
}
