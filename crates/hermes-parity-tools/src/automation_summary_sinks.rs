//! Sink dispatch helpers for `publish-automation-summary`.

use std::collections::hash_map::RandomState;
use std::fs::{self, File, OpenOptions};
use std::hash::BuildHasher;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::automation_summary::{parse_sink_order, AutomationSummaryArgs};
use crate::py::{self, Py};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct HeaderConfigGuard(PathBuf);

impl Drop for HeaderConfigGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) fn bounded_timeout_secs(raw: f64) -> Duration {
    let clamped = if raw.is_finite() {
        raw.clamp(0.5, 3600.0)
    } else {
        8.0
    };
    Duration::from_secs_f64(clamped)
}

fn validate_endpoint(raw: &str) -> Result<&str, String> {
    let url = raw.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("error=ValueError:invalid ContextLattice URL scheme".to_string());
    }
    if url
        .bytes()
        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err("error=ValueError:invalid whitespace or control char in URL".to_string());
    }
    Ok(url)
}

fn open_auth_config(opts: &OpenOptions, pid: u32, seq: u64) -> Result<(PathBuf, File), String> {
    let hasher = RandomState::new();
    let dir = std::env::temp_dir();
    for attempt in 0..16u32 {
        let rand_id = hasher.hash_one((pid, seq, attempt));
        let path = dir.join(format!("hermes-summary-curl-{pid}-{rand_id:016x}.cfg"));
        match opts.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("error=OSError:failed creating curl config: {e}")),
        }
    }
    Err("error=OSError:failed creating unique curl config after retries".to_string())
}

fn write_auth_config(api_key: &str) -> Result<HeaderConfigGuard, String> {
    let trimmed = api_key.trim();
    if trimmed.bytes().any(|b| b.is_ascii_control()) {
        return Err("error=ValueError:invalid control character in API key".to_string());
    }
    let escaped = trimmed.replace('\\', "\\\\").replace('"', "\\\"");
    let pid = std::process::id();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let (path, mut file) = open_auth_config(&opts, pid, seq)?;
    let guard = HeaderConfigGuard(path);
    writeln!(file, "header = \"Authorization: Bearer {escaped}\"")
        .map_err(|e| format!("error=OSError:failed writing curl config: {e}"))?;
    Ok(guard)
}

fn spawn_curl_post(
    url: &str,
    auth_guard: Option<&HeaderConfigGuard>,
    timeout: Duration,
    body: &Py,
) -> Result<std::process::Output, String> {
    let max_time = format!("{:.3}", timeout.as_secs_f64());
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sS",
        "--max-time",
        &max_time,
        "-X",
        "POST",
        "-H",
        "Content-Type: application/json",
        "-H",
        "Accept: application/json",
    ]);
    if let Some(guard) = auth_guard {
        cmd.arg("-K").arg(&guard.0);
    }
    cmd.args(["--data-binary", "@-", "-w", "\n%{http_code}", "--", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("error=CurlError:{e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let json_body = py::dumps(body, None, false);
        let _ = stdin.write_all(json_body.as_bytes());
    }
    child
        .wait_with_output()
        .map_err(|e| format!("error=CurlError:{e}"))
}

fn parse_curl_status(out: std::process::Output) -> (bool, String) {
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if err.is_empty() {
            format!("curl_exit_{}", out.status.code().unwrap_or(1))
        } else {
            err
        };
        return (false, format!("error=RequestError:{detail}"));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let code = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.trim().parse::<u16>().ok());
    match code {
        Some(c) => ((200..300).contains(&c), format!("http_status={c}")),
        None => (false, "error=CurlError:missing_http_status".to_string()),
    }
}

fn post_contextlattice_json(
    endpoint: &str,
    api_key: &str,
    timeout: Duration,
    body: &Py,
) -> (bool, String) {
    let url = match validate_endpoint(endpoint) {
        Ok(u) => u,
        Err(err) => return (false, err),
    };
    let _auth_guard = if api_key.trim().is_empty() {
        None
    } else {
        match write_auth_config(api_key) {
            Ok(g) => Some(g),
            Err(err) => return (false, err),
        }
    };
    match spawn_curl_post(url, _auth_guard.as_ref(), timeout, body) {
        Ok(out) => parse_curl_status(out),
        Err(err) => (false, err),
    }
}

fn write_contextlattice(
    args: &AutomationSummaryArgs,
    api_key: &str,
    project: &str,
    content: &str,
) -> (bool, String) {
    let endpoint = format!(
        "{}/memory/write",
        args.contextlattice_url.trim_end_matches('/')
    );
    let mut body = Py::dict([
        ("projectName", Py::from(project)),
        ("fileName", Py::from(args.context_file_name.as_str())),
        ("content", Py::from(content)),
    ]);
    if !args.context_topic_path.is_empty() {
        body.set("topicPath", Py::from(args.context_topic_path.as_str()));
    }
    let timeout = bounded_timeout_secs(args.context_timeout_secs);
    post_contextlattice_json(&endpoint, api_key, timeout, &body)
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
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
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
    let api_key = args.resolved_api_key();
    let mut attempts = Vec::new();
    for sink in parse_sink_order(&args.sink_order) {
        let (ok, detail) = match sink.as_str() {
            "contextlattice" => write_contextlattice(args, &api_key, project, markdown),
            "github" => comment_github_issue(repo_root, args.github_issue, markdown),
            "local" => append_local_log(local_path, markdown),
            "stdout" => {
                // Keep JSON stdout uncorrupted when --json is active.
                if args.json {
                    eprintln!("{markdown}");
                } else {
                    println!("{markdown}");
                }
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
