//! The recorder prints what the Python recorder it replaced (`scripts/record_fixtures.py`)
//! printed on the same inputs, byte for byte; the goldens under
//! `tests/fixtures/record_fixtures/` were captured from that script.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_parity_tests::recorder::{python_checkout_beside, python_from_env, record, Recorded};

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

/// The interpreter the recorder would use; these tests need it to be a working Python 3.
fn python() -> String {
    let python = python_from_env();
    let works = Command::new(&python)
        .args(["-c", "import sys; sys.exit(sys.version_info[0] != 3)"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        works,
        "these tests run Python 3 as `{python}`: install it, or set PARITY_PYTHON to a \
         Python 3 interpreter (on Windows often `python` or `py`)"
    );
    python
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

/// An empty scratch folder for one test, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("record-fixtures-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn checkout(test: &str, adapter: &str) -> Scratch {
    let dir = Scratch::new(test);
    let agent = dir.path().join("hermes-agent/agent");
    fs::create_dir_all(&agent).unwrap();
    fs::write(agent.join("__init__.py"), "").unwrap();
    fs::write(agent.join("anthropic_adapter.py"), adapter).unwrap();
    dir
}

#[test]
fn without_a_checkout_prints_the_checkpoint_cases_and_a_note() {
    let dir = Scratch::new("no-checkout");
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
    let dir = checkout("fake", FAKE_ADAPTER);
    let py_root = dir.path().join("hermes-agent");
    let recorded = record(&py_root, &python());
    assert_eq!(recorded.stderr, "");
    assert_eq!(recorded.code, 0);
    assert_eq!(recorded.stdout, golden("fake_checkout.json", &py_root));
}

#[test]
fn what_the_adapter_writes_to_stderr_passes_through_on_success() {
    let adapter = format!("import sys\nsys.stderr.write('adapter warning\\n')\n{FAKE_ADAPTER}");
    let dir = checkout("warns", &adapter);
    let py_root = dir.path().join("hermes-agent");
    let recorded = record(&py_root, &python());
    assert_eq!(recorded.stderr, "adapter warning\n");
    assert_eq!(recorded.code, 0);
    assert_eq!(recorded.stdout, golden("fake_checkout.json", &py_root));
}

#[test]
fn what_the_adapter_prints_comes_before_the_json() {
    let adapter = format!("print('loading adapter')\n{FAKE_ADAPTER}");
    let dir = checkout("prints", &adapter);
    let py_root = dir.path().join("hermes-agent");
    let recorded = record(&py_root, &python());
    assert_eq!(recorded.stderr, "");
    assert_eq!(recorded.code, 0);
    assert_eq!(
        recorded.stdout,
        format!(
            "loading adapter\n{}",
            golden("fake_checkout.json", &py_root)
        )
    );
}

#[test]
fn what_the_adapter_prints_without_a_newline_comes_before_the_json() {
    let adapter = format!("import sys\nsys.stdout.write('loading')\n{FAKE_ADAPTER}");
    let dir = checkout("unterminated", &adapter);
    let py_root = dir.path().join("hermes-agent");
    let recorded = record(&py_root, &python());
    assert_eq!(recorded.stderr, "");
    assert_eq!(recorded.code, 0);
    assert_eq!(
        recorded.stdout,
        format!("loading{}", golden("fake_checkout.json", &py_root))
    );
}

#[test]
fn the_checkout_beside_is_found_from_the_resolved_repository_root() {
    let dir = Scratch::new("resolve");
    let repo = dir.path().join("reformed-agent");
    fs::create_dir_all(&repo).unwrap();
    let unresolved = repo.join("..").join("reformed-agent");
    let found = python_checkout_beside(&unresolved);
    let real = fs::canonicalize(dir.path()).unwrap();
    let real = real.to_str().unwrap();
    let real = real.strip_prefix(r"\\?\").unwrap_or(real);
    assert_eq!(found, Path::new(real).join("hermes-agent"));
}

#[test]
fn a_failing_adapter_import_fails_the_run_with_python_s_error() {
    let dir = checkout(
        "failing",
        "raise ImportError('adapter needs its dependencies')\n",
    );
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
    let dir = checkout("no-interpreter", FAKE_ADAPTER);
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
