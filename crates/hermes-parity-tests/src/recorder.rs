//! Record comparison cases for `fixtures/` from the Python reference.
//!
//! The checkpoint-manager cases need no Python: the harness helpers mirror upstream's
//! `tools/checkpoint_manager.py`. When a Hermes Python checkout exists, the Anthropic
//! adapter cases come from upstream's own `agent.anthropic_adapter`, run through a short
//! Python shim: that module is the reference under test, so it stays Python.
//!
//! Run with `cargo run -p hermes-parity-tests --bin record_fixtures`; set `PARITY_PYTHON`
//! to choose the interpreter (default `python3`).

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::{json, Value};

use crate::harness::{
    checkpoint_parse_shortstat, checkpoint_shadow_dir_id, checkpoint_validate_commit_hash,
    CheckpointShortstat,
};

/// Calls upstream's adapter for each Anthropic case and prints the results as one JSON list
/// after [`RESULTS_MARKER`], so whatever the adapter prints first, newline or not, stays apart.
/// `-c` puts the working folder first on `sys.path` (unless `-P` or `PYTHONSAFEPATH` is in
/// force); the checkout takes that place before anything but the built-in `sys` is imported.
const ANTHROPIC_SHIM: &str = r#"
import sys
if getattr(sys.flags, "safe_path", False):
    sys.path.insert(0, sys.argv[1])
else:
    sys.path[0] = sys.argv[1]
import json
from agent import anthropic_adapter as aa
results = json.dumps([
    aa.normalize_model_name("anthropic/claude-opus-4.6", False),
    aa.normalize_model_name("anthropic/qwen3.5-plus", True),
    aa._sanitize_tool_id("abc.123"),
    aa._sanitize_tool_id(""),
    aa._is_oauth_token("sk-ant-api03-xxx"),
    aa._is_oauth_token("sk-ant-oat-xxx"),
    aa._common_betas_for_base_url(None),
    aa._common_betas_for_base_url("https://api.minimax.io/anthropic/v1"),
    list(aa._common_betas_for_base_url(None)) + list(aa._OAUTH_ONLY_BETAS),
])
sys.stdout.write("\x00hermes-record-results\x00" + results)
"#;

/// Separates the adapter's own stdout from the shim's results; see [`ANTHROPIC_SHIM`].
const RESULTS_MARKER: &str = "\0hermes-record-results\0";

/// Operation and input of each Anthropic case, in the order the shim prints its results.
fn anthropic_inputs() -> [(&'static str, Value); 9] {
    let minimax = "https://api.minimax.io/anthropic/v1";
    [
        (
            "normalize_model_name",
            json!({"model": "anthropic/claude-opus-4.6", "preserve_dots": false}),
        ),
        (
            "normalize_model_name",
            json!({"model": "anthropic/qwen3.5-plus", "preserve_dots": true}),
        ),
        ("sanitize_tool_id", json!({"tool_id": "abc.123"})),
        ("sanitize_tool_id", json!({"tool_id": ""})),
        ("is_oauth_token", json!({"key": "sk-ant-api03-xxx"})),
        ("is_oauth_token", json!({"key": "sk-ant-oat-xxx"})),
        ("common_betas_for_base_url", json!({"base_url": null})),
        ("common_betas_for_base_url", json!({"base_url": minimax})),
        (
            "default_anthropic_beta_list",
            json!({"base_url": null, "is_oauth": true}),
        ),
    ]
}

/// A recorded value. Python keeps dictionary keys in insertion order, so a shortstat is
/// serialised from its struct, whose field order matches, not through `Value`, which sorts keys.
/// The adapter's results go through `Value`: every object they hold today is already in
/// alphabetical order, so a new case returning a dict must keep to that or get a struct too.
#[derive(Serialize)]
#[serde(untagged)]
enum Output {
    Json(Value),
    Shortstat(CheckpointShortstat),
}

#[derive(Serialize)]
struct Case {
    op: &'static str,
    input: Value,
    py: Output,
}

#[derive(Serialize)]
struct Recording {
    source: String,
    cases: Vec<Case>,
}

/// What a recording run prints, byte for byte as the script printed it on this platform:
/// stdout, stderr and the exit code. What Python wrote passes through untouched; text the
/// recorder writes itself ends its lines as Python's text streams do here.
#[derive(Debug, PartialEq, Eq)]
pub struct Recorded {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: i32,
}

/// `text` with the line ends Python's text streams write on this platform (CRLF on Windows).
fn native(text: &str) -> Vec<u8> {
    if cfg!(windows) {
        text.replace('\n', "\r\n").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}

fn checkpoint_cases() -> Vec<Case> {
    let hash = |commit_hash: &str| Case {
        op: "checkpoint_validate_commit_hash",
        input: json!({ "commit_hash": commit_hash }),
        py: Output::Json(json!(checkpoint_validate_commit_hash(commit_hash))),
    };
    let shortstat = |stat_line: &str| Case {
        op: "checkpoint_parse_shortstat",
        input: json!({ "stat_line": stat_line }),
        py: Output::Shortstat(checkpoint_parse_shortstat(stat_line)),
    };
    vec![
        Case {
            op: "checkpoint_shadow_dir_id",
            input: json!({"abs_path": "/workspace/demo"}),
            py: Output::Json(json!(checkpoint_shadow_dir_id("/workspace/demo"))),
        },
        hash("abc"),
        hash("-abc123"),
        hash("ABCDEF1234"),
        shortstat("1 file changed, 2 insertions(+), 3 deletions(-)"),
        shortstat("2 files changed, 10 insertions(+)"),
    ]
}

/// Python's `json.dumps(..., indent=2)`: the same layout as serde's pretty printer, with
/// every non-ASCII character escaped. Outside strings JSON is ASCII, so escaping the whole
/// text is safe.
fn python_json(recording: &Recording) -> String {
    let pretty = serde_json::to_string_pretty(recording).expect("recording serialises");
    let mut out = String::with_capacity(pretty.len());
    for ch in pretty.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out.push('\n');
    out
}

fn failure(message: String) -> Recorded {
    Recorded {
        stdout: Vec::new(),
        stderr: native(&message),
        code: 1,
    }
}

/// `PARITY_PYTHON`, or `python3` when it is unset.
pub fn python_from_env() -> String {
    std::env::var("PARITY_PYTHON").unwrap_or_else(|_| "python3".into())
}

/// The `hermes-agent` folder beside the repository at `repo_root`, resolved as the script's
/// `Path(__file__).resolve()` was: links followed, and on Windows without the `\\?\` prefix.
pub fn python_checkout_beside(repo_root: &Path) -> PathBuf {
    let root = std::fs::canonicalize(repo_root)
        .map_or_else(|_| repo_root.to_path_buf(), without_verbatim_prefix);
    root.parent().unwrap_or(&root).join("hermes-agent")
}

fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    text.strip_prefix(r"\\?\")
        .map_or_else(|| path.clone(), PathBuf::from)
}

fn shim_command(python: &str, py_root: &Path) -> Command {
    let mut command = Command::new(python);
    command.arg("-c").arg(ANTHROPIC_SHIM).arg(py_root);
    command
}

/// What the shim printed around its results, the results, its stderr and exit code.
struct Upstream {
    printed: Vec<u8>,
    results: Vec<Value>,
    after: Vec<u8>,
    stderr: Vec<u8>,
    code: i32,
}

/// Splits the shim's stdout into what the adapter printed before the results, the results,
/// and what it printed after them (at interpreter exit); `None` when no complete results.
fn split_results(stdout: &[u8]) -> Option<(&[u8], Vec<Value>, &[u8])> {
    let marker = RESULTS_MARKER.as_bytes();
    let at = stdout
        .windows(marker.len())
        .position(|window| window == marker)?;
    let (printed, rest) = (&stdout[..at], &stdout[at + marker.len()..]);
    let mut values = serde_json::Deserializer::from_slice(rest).into_iter::<Vec<Value>>();
    let results = values.next()?.ok()?;
    Some((printed, results, &rest[values.byte_offset()..]))
}

fn upstream_results(python: &str, py_root: &Path) -> Result<Upstream, Recorded> {
    let output = shim_command(python, py_root)
        .output()
        .map_err(|error| failure(format!("cannot run {python}: {error}\n")))?;
    let code = output.status.code().unwrap_or(1);
    let Some((printed, results, after)) = split_results(&output.stdout) else {
        if code != 0 {
            // As the script did, keep what the adapter printed before it failed, and its exit code.
            return Err(Recorded {
                stdout: output.stdout,
                stderr: output.stderr,
                code,
            });
        }
        return Err(failure(format!(
            "unreadable output from {python}: no results\n"
        )));
    };
    let expected = anthropic_inputs().len();
    if results.len() != expected {
        return Err(failure(format!(
            "{python} returned {} results, expected {expected}\n",
            results.len()
        )));
    }
    // A failure after the results (at interpreter exit) still printed the recording.
    Ok(Upstream {
        printed: printed.to_vec(),
        results,
        after: after.to_vec(),
        stderr: output.stderr,
        code,
    })
}

/// The bytes to write to a stream: Rust refuses non-UTF-8 bytes on a Windows console,
/// where Python's console writer replaced them with U+FFFD, so `windows_console` gets
/// them replaced the same way; pipes and files get the bytes unchanged.
pub fn for_stream(bytes: &[u8], windows_console: bool) -> Cow<'_, [u8]> {
    match String::from_utf8_lossy(bytes) {
        Cow::Owned(text) if windows_console => Cow::Owned(text.into_bytes()),
        _ => Cow::Borrowed(bytes),
    }
}

/// Record the cases against the Python checkout at `py_root`, running `python` for the
/// Anthropic adapter cases when `agent/anthropic_adapter.py` exists there.
pub fn record(py_root: &Path, python: &str) -> Recorded {
    let mut cases = checkpoint_cases();
    let source = py_root.display().to_string();
    let (printed, after, stderr, code) = if py_root
        .join("agent")
        .join("anthropic_adapter.py")
        .is_file()
    {
        let upstream = match upstream_results(python, py_root) {
            Ok(upstream) => upstream,
            Err(failed) => return failed,
        };
        cases.extend(anthropic_inputs().into_iter().zip(upstream.results).map(
            |((op, input), py)| Case {
                op,
                input,
                py: Output::Json(py),
            },
        ));
        (
            upstream.printed,
            upstream.after,
            upstream.stderr,
            upstream.code,
        )
    } else {
        let note = format!(
            "Note: Python Hermes not found at {source} \u{2014} only checkpoint hash cases emitted.\n"
        );
        (Vec::new(), Vec::new(), native(&note), 0)
    };
    let recording = native(&python_json(&Recording { source, cases }));
    Recorded {
        stdout: [printed, recording, after].concat(),
        stderr,
        code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `json.py` in the working folder must not shadow the standard library's: with `-c`,
    /// Python puts the working folder first on `sys.path`, where the script had its own folder.
    #[test]
    fn the_shim_ignores_modules_in_the_working_folder() {
        let scratch = std::env::temp_dir().join(format!("recorder-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let (cwd, py_root) = (scratch.join("cwd"), scratch.join("hermes-agent"));
        std::fs::create_dir_all(py_root.join("agent")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            cwd.join("json.py"),
            "raise SystemExit('json.py from the working folder')\n",
        )
        .unwrap();
        std::fs::write(py_root.join("agent").join("__init__.py"), "").unwrap();
        std::fs::write(
            py_root.join("agent").join("anthropic_adapter.py"),
            "raise ImportError('reached the adapter')\n",
        )
        .unwrap();
        let output = shim_command(&python_from_env(), &py_root)
            .current_dir(&cwd)
            .output()
            .expect("run Python 3 (set PARITY_PYTHON if `python3` is not it)");
        let _ = std::fs::remove_dir_all(&scratch);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("reached the adapter"), "{stderr}");
    }

    #[test]
    fn a_windows_console_gets_invalid_bytes_replaced_as_python_does() {
        assert_eq!(&*for_stream(b"ok\xff\n", true), "ok\u{fffd}\n".as_bytes());
        assert_eq!(&*for_stream(b"ok\xff\n", false), b"ok\xff\n");
        assert_eq!(&*for_stream(b"caf\xc3\xa9\r\n", true), b"caf\xc3\xa9\r\n");
    }

    #[test]
    fn verbatim_prefixes_are_stripped_as_python_resolve_does() {
        let strip = |path: &str| without_verbatim_prefix(PathBuf::from(path));
        assert_eq!(strip(r"\\?\D:\code\repo"), PathBuf::from(r"D:\code\repo"));
        assert_eq!(
            strip(r"\\?\UNC\host\share\repo"),
            PathBuf::from(r"\\host\share\repo")
        );
        assert_eq!(strip("/home/l/repo"), PathBuf::from("/home/l/repo"));
    }

    #[test]
    fn python_json_escapes_non_ascii_like_json_dumps() {
        let recording = Recording {
            source: "caf\u{e9} \u{1f600}".into(),
            cases: Vec::new(),
        };
        assert_eq!(
            python_json(&recording),
            "{\n  \"source\": \"caf\\u00e9 \\ud83d\\ude00\",\n  \"cases\": []\n}\n"
        );
    }
}
