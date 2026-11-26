//! Record comparison cases for `fixtures/` from the Python reference.
//!
//! The checkpoint-manager cases need no Python: the harness helpers mirror upstream's
//! `tools/checkpoint_manager.py`. When a Hermes Python checkout exists, the Anthropic
//! adapter cases come from upstream's own `agent.anthropic_adapter`, run through a short
//! Python shim: that module is the reference under test, so it stays Python.
//!
//! Run with `cargo run -p hermes-parity-tests --bin record_fixtures`; set `PARITY_PYTHON`
//! to choose the interpreter (default `python3`).

use std::path::Path;
use std::process::Command;

use serde::Serialize;
use serde_json::{json, Value};

use crate::harness::{
    checkpoint_parse_shortstat, checkpoint_shadow_dir_id, checkpoint_validate_commit_hash,
    CheckpointShortstat,
};

/// Calls upstream's adapter for each Anthropic case and prints the results as one JSON list.
const ANTHROPIC_SHIM: &str = r#"
import json, sys
sys.path.insert(0, sys.argv[1])
from agent import anthropic_adapter as aa
print(json.dumps([
    aa.normalize_model_name("anthropic/claude-opus-4.6", False),
    aa.normalize_model_name("anthropic/qwen3.5-plus", True),
    aa._sanitize_tool_id("abc.123"),
    aa._sanitize_tool_id(""),
    aa._is_oauth_token("sk-ant-api03-xxx"),
    aa._is_oauth_token("sk-ant-oat-xxx"),
    aa._common_betas_for_base_url(None),
    aa._common_betas_for_base_url("https://api.minimax.io/anthropic/v1"),
    list(aa._common_betas_for_base_url(None)) + list(aa._OAUTH_ONLY_BETAS),
]))
"#;

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

/// What a recording run prints: stdout, stderr and the exit code.
#[derive(Debug, PartialEq, Eq)]
pub struct Recorded {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
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

fn failure(stderr: String) -> Recorded {
    Recorded {
        stdout: String::new(),
        stderr,
        code: 1,
    }
}

fn upstream_results(python: &str, py_root: &Path) -> Result<Vec<Value>, Recorded> {
    let output = Command::new(python)
        .arg("-c")
        .arg(ANTHROPIC_SHIM)
        .arg(py_root)
        .output()
        .map_err(|error| failure(format!("cannot run {python}: {error}\n")))?;
    if !output.status.success() {
        return Err(failure(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let results: Vec<Value> = serde_json::from_slice(&output.stdout)
        .map_err(|error| failure(format!("unreadable output from {python}: {error}\n")))?;
    let expected = anthropic_inputs().len();
    if results.len() != expected {
        return Err(failure(format!(
            "{python} returned {} results, expected {expected}\n",
            results.len()
        )));
    }
    Ok(results)
}

/// Record the cases against the Python checkout at `py_root`, running `python` for the
/// Anthropic adapter cases when `agent/anthropic_adapter.py` exists there.
pub fn record(py_root: &Path, python: &str) -> Recorded {
    let mut cases = checkpoint_cases();
    let source = py_root.display().to_string();
    let mut stderr = String::new();
    if py_root.join("agent").join("anthropic_adapter.py").is_file() {
        let results = match upstream_results(python, py_root) {
            Ok(results) => results,
            Err(failed) => return failed,
        };
        cases.extend(
            anthropic_inputs()
                .into_iter()
                .zip(results)
                .map(|((op, input), py)| Case {
                    op,
                    input,
                    py: Output::Json(py),
                }),
        );
    } else {
        stderr = format!(
            "Note: Python Hermes not found at {source} \u{2014} only checkpoint hash cases emitted.\n"
        );
    }
    Recorded {
        stdout: python_json(&Recording { source, cases }),
        stderr,
        code: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
