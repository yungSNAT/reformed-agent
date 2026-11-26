//! The recorder prints what the Python recorder it replaced (`scripts/record_fixtures.py`)
//! printed on the same inputs, byte for byte; the goldens under
//! `tests/fixtures/record_fixtures/` were captured from that script.

use std::fs;
use std::path::Path;

use hermes_parity_tests::recorder::{record, Recorded};

/// A stand-in for upstream's `agent/anthropic_adapter.py`; its non-ASCII beta checks that
/// the output escapes like Python's `json.dumps`.
const FAKE_ADAPTER: &str = r#"_OAUTH_ONLY_BETAS = ("oauth-2025-04-20", "fake-oauth-beta")


def normalize_model_name(model, preserve_dots=False):
    name = model.split("/", 1)[-1]
    return name if preserve_dots else name.replace(".", "-")


def _sanitize_tool_id(tool_id):
    return tool_id.replace(".", "_") if tool_id else "tool_0"


def _is_oauth_token(key):
    return "oat" in key


def _common_betas_for_base_url(base_url):
    if base_url and "minimax" in base_url:
        return ["fine-grained-tool-streaming-2025-05-14"]
    return ["interleaved-thinking-2025-05-14", "café-beta"]
"#;

fn python() -> String {
    std::env::var("PARITY_PYTHON").unwrap_or_else(|_| "python3".into())
}

fn golden(name: &str, py_root: &Path) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/record_fixtures")
        .join(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .replace("\r\n", "\n");
    let source = serde_json::to_string(&py_root.display().to_string()).unwrap();
    text.replace("\"@SOURCE@\"", &source)
}

fn checkout(adapter: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("hermes-agent/agent");
    fs::create_dir_all(&agent).unwrap();
    fs::write(agent.join("__init__.py"), "").unwrap();
    fs::write(agent.join("anthropic_adapter.py"), adapter).unwrap();
    dir
}

#[test]
fn without_a_checkout_prints_the_checkpoint_cases_and_a_note() {
    let dir = tempfile::tempdir().unwrap();
    let py_root = dir.path().join("hermes-agent");
    let recorded = record(&py_root, "python-that-must-not-run");
    assert_eq!(recorded.code, 0);
    assert_eq!(recorded.stdout, golden("no_checkout.json", &py_root));
    assert_eq!(
        recorded.stderr,
        format!(
            "Note: Python Hermes not found at {} \u{2014} only checkpoint hash cases emitted.\n",
            py_root.display()
        )
    );
}

#[test]
fn with_a_checkout_records_the_adapter_cases_from_python() {
    let dir = checkout(FAKE_ADAPTER);
    let py_root = dir.path().join("hermes-agent");
    let recorded = record(&py_root, &python());
    assert_eq!(recorded.stderr, "");
    assert_eq!(recorded.code, 0);
    assert_eq!(recorded.stdout, golden("fake_checkout.json", &py_root));
}

#[test]
fn a_failing_adapter_import_fails_the_run_with_python_s_error() {
    let dir = checkout("raise ImportError('adapter needs its dependencies')\n");
    let py_root = dir.path().join("hermes-agent");
    let Recorded {
        stdout,
        stderr,
        code,
    } = record(&py_root, &python());
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("ImportError: adapter needs its dependencies"),
        "{stderr}"
    );
}

#[test]
fn a_missing_interpreter_fails_the_run() {
    let dir = checkout(FAKE_ADAPTER);
    let recorded = record(
        &dir.path().join("hermes-agent"),
        "no-such-python-interpreter",
    );
    assert_eq!(recorded.code, 1);
    assert_eq!(recorded.stdout, "");
    assert!(
        recorded
            .stderr
            .starts_with("cannot run no-such-python-interpreter: "),
        "{}",
        recorded.stderr
    );
}
