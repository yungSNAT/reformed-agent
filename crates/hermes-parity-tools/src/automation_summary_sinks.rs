//! Sink dispatch helpers for `publish-automation-summary`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::automation_summary::{parse_sink_order, AutomationSummaryArgs};
use crate::py::{self, Py};

pub(crate) fn bounded_timeout_secs(raw: f64) -> Duration {
    let clamped = if raw.is_finite() {
        raw.clamp(0.5, 3600.0)
    } else {
        8.0
    };
    Duration::from_secs_f64(clamped)
}

fn post_contextlattice_json(
    endpoint: &str,
    api_key: &str,
    timeout: Duration,
    body: &Map<String, Value>,
) -> (bool, String) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return (
            false,
            "error=RuntimeError:failed to initialize tokio runtime".to_string(),
        );
    };
    rt.block_on(async {
        let client = match reqwest::Client::builder().timeout(timeout).build() {
            Ok(c) => c,
            Err(e) => return (false, format!("error=ClientError:{e}")),
        };
        let mut req = client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(body);
        if !api_key.trim().is_empty() {
            req = req.header("Authorization", format!("Bearer {}", api_key.trim()));
        }
        match req.send().await {
            Ok(resp) => {
                let code = resp.status().as_u16();
                (code < 300, format!("http_status={code}"))
            }
            Err(e) => (false, format!("error=RequestError:{e}")),
        }
    })
}

fn write_contextlattice(
    args: &AutomationSummaryArgs,
    project: &str,
    content: &str,
) -> (bool, String) {
    let endpoint = format!(
        "{}/memory/write",
        args.contextlattice_url.trim_end_matches('/')
    );
    let mut body = Map::new();
    body.insert("projectName".into(), Value::String(project.to_string()));
    body.insert(
        "fileName".into(),
        Value::String(args.context_file_name.clone()),
    );
    body.insert("content".into(), Value::String(content.to_string()));
    if !args.context_topic_path.is_empty() {
        body.insert(
            "topicPath".into(),
            Value::String(args.context_topic_path.clone()),
        );
    }
    let timeout = bounded_timeout_secs(args.context_timeout_secs);
    post_contextlattice_json(&endpoint, &args.context_api_key, timeout, &body)
}

fn comment_github_issue(repo_root: &Path, issue: i64, content: &str) -> (bool, String) {
    if issue <= 0 {
        return (false, "issue_not_configured".to_string());
    }
    let mut child = match Command::new("gh")
        .args(["issue", "comment", &issue.to_string(), "--body-file", "-"])
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (false, "gh_cli_missing".to_string());
        }
        Err(e) => return (false, format!("gh_spawn_error:{e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(content.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => (true, "commented".to_string()),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let code = out.status.code().unwrap_or(1);
            (
                false,
                if err.is_empty() {
                    format!("gh_exit_{code}")
                } else {
                    err
                },
            )
        }
        Err(e) => (false, format!("gh_wait_error:{e}")),
    }
}

fn append_local_log(path: &Path, content: &str) -> (bool, String) {
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return (false, format!("error=OSError:{e}"));
        }
    }
    let stamp = py::utc_isoformat();
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => {
            let suffix = if content.ends_with('\n') { "" } else { "\n" };
            let block = format!("\n\n===== {stamp} =====\n{content}{suffix}");
            match f.write_all(block.as_bytes()) {
                Ok(()) => (true, py::path_str(path)),
                Err(e) => (false, format!("error=OSError:{e}")),
            }
        }
        Err(e) => (false, format!("error=OSError:{e}")),
    }
}

pub(crate) fn try_sinks(
    args: &AutomationSummaryArgs,
    repo_root: &Path,
    project: &str,
    local_path: &Path,
    markdown: &str,
) -> (bool, String, Vec<Py>) {
    let mut attempts = Vec::new();
    for sink in parse_sink_order(&args.sink_order) {
        let (ok, detail) = match sink.as_str() {
            "contextlattice" => write_contextlattice(args, project, markdown),
            "github" => comment_github_issue(repo_root, args.github_issue, markdown),
            "local" => append_local_log(local_path, markdown),
            "stdout" => {
                println!("{markdown}");
                (true, "printed_stdout".to_string())
            }
            _ => (false, "unsupported_sink".to_string()),
        };
        attempts.push(Py::dict([
            ("sink", Py::from(sink.as_str())),
            ("ok", Py::from(if ok { "true" } else { "false" })),
            ("detail", Py::from(detail.as_str())),
        ]));
        if ok {
            return (true, sink, attempts);
        }
    }
    (false, String::new(), attempts)
}
