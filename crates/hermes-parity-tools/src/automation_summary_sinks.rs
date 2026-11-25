//! Sink dispatch helpers for `publish-automation-summary`.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::automation_summary::{parse_sink_order, AutomationSummaryArgs};
use crate::py::{self, Py};

/// Most bytes kept from a child's stdout or stderr; the rest is drained unread.
const MAX_CHILD_OUTPUT: u64 = 64 * 1024;
/// urllib's `HTTPRedirectHandler.max_redirections` and `max_repeats`.
const MAX_REDIRECTS: usize = 10;
const MAX_REPEATS: usize = 4;
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

/// `urlopen(timeout=max(0.5, t))`: no upper bound; NaN behaves as 0.5 and an
/// infinite or unrepresentable timeout means none.
pub(crate) fn bounded_timeout_secs(raw: f64) -> Option<Duration> {
    let secs = if raw.is_nan() { 0.5 } else { raw.max(0.5) };
    Duration::try_from_secs_f64(secs).ok()
}

pub(crate) struct Captured {
    pub(crate) success: bool,
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

fn read_bounded<R: Read>(mut reader: R) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = reader.by_ref().take(MAX_CHILD_OUTPUT).read_to_end(&mut buf);
    let _ = io::copy(&mut reader, &mut io::sink());
    buf
}

/// Runs `cmd` with `input` on stdin, keeping at most `MAX_CHILD_OUTPUT` bytes of
/// each output stream; threads keep every pipe moving so the child never blocks.
pub(crate) fn run_bounded(mut cmd: Command, input: &[u8]) -> io::Result<Captured> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdin = child.stdin.take();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        if let Some(mut pipe) = stdin {
            let _ = pipe.write_all(&input);
        }
    });
    let err_pipe = child.stderr.take();
    let err_reader = std::thread::spawn(move || err_pipe.map(read_bounded).unwrap_or_default());
    let stdout = child.stdout.take().map(read_bounded).unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    let _ = writer.join();
    let status = child.wait()?;
    Ok(Captured {
        success: status.success(),
        code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
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

/// A curl config-file string value: quoted, with backslash and quote escaped.
fn config_value(raw: &str) -> String {
    format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Curl config read from stdin (`-K -`), so the API key never reaches argv or
/// a file. `body` is `Some` for the first POST and `None` for a redirected GET.
pub(crate) fn curl_config(api_key: &str, body: Option<&str>) -> Result<String, String> {
    let key = api_key.trim();
    if key.bytes().any(|b| b.is_ascii_control()) {
        return Err("error=ValueError:invalid control character in API key".to_string());
    }
    let mut lines = Vec::new();
    if let Some(data) = body {
        lines.push(format!(
            "header = {}",
            config_value("Content-Type: application/json")
        ));
        lines.push(format!("data-binary = {}", config_value(data)));
    }
    lines.push(format!(
        "header = {}",
        config_value("Accept: application/json")
    ));
    if !key.is_empty() {
        lines.push(format!(
            "header = {}",
            config_value(&format!("Authorization: Bearer {key}"))
        ));
    }
    lines.push(String::new());
    Ok(lines.join("\n"))
}

/// One curl request; returns the status code and the redirect target.
fn curl_once(url: &str, config: &str, timeout: Option<Duration>) -> Result<(u16, String), String> {
    let mut cmd = Command::new("curl");
    // `-q` first: ~/.curlrc must not add --fail, -L or --insecure.
    cmd.args(["-q", "-sS", "--proto", "=http,https", "-o", NULL_DEVICE]);
    if let Some(t) = timeout {
        cmd.args(["--max-time", &format!("{:.3}", t.as_secs_f64())]);
    }
    cmd.args(["-w", "%{http_code}\\n%{redirect_url}", "-K", "-", "--", url]);
    let out = run_bounded(cmd, config.as_bytes()).map_err(|e| format!("error=CurlError:{e}"))?;
    if !out.success {
        let err = out.stderr.trim();
        return Err(if err.is_empty() {
            format!("error=RequestError:curl_exit_{}", out.code.unwrap_or(1))
        } else {
            format!("error=RequestError:{err}")
        });
    }
    let mut lines = out.stdout.lines();
    let code = lines
        .next()
        .and_then(|line| line.trim().parse::<u16>().ok())
        .ok_or_else(|| "error=CurlError:missing_http_status".to_string())?;
    Ok((code, lines.next().unwrap_or("").trim().to_string()))
}

/// urllib's redirect rules: a POST follows 301, 302 and 303 as a GET without
/// the body; a GET also follows 307 and 308; other schemes, more than
/// `MAX_REDIRECTS` distinct targets or a target seen `MAX_REPEATS` times end
/// on the redirect status.
pub(crate) fn follows_redirect(code: u16, is_post: bool) -> bool {
    matches!(code, 301..=303) || (!is_post && matches!(code, 307 | 308))
}

fn post_contextlattice_json(
    endpoint: &str,
    api_key: &str,
    timeout: Option<Duration>,
    body: &Py,
) -> (bool, String) {
    let mut url = match validate_endpoint(endpoint) {
        Ok(u) => u.to_string(),
        Err(err) => return (false, err),
    };
    let json_body = py::dumps(body, None, false);
    let mut is_post = true;
    let mut visited: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    loop {
        let config = match curl_config(api_key, is_post.then_some(json_body.as_str())) {
            Ok(c) => c,
            Err(err) => return (false, err),
        };
        let (code, location) = match curl_once(&url, &config, timeout) {
            Ok(r) => r,
            Err(err) => return (false, err),
        };
        let can_follow = location.starts_with("http://") || location.starts_with("https://");
        let seen = visited.get(&location).copied().unwrap_or(0);
        if !follows_redirect(code, is_post)
            || !can_follow
            || seen >= MAX_REPEATS
            || visited.len() >= MAX_REDIRECTS
        {
            return ((200..300).contains(&code), format!("http_status={code}"));
        }
        visited.insert(location.clone(), seen + 1);
        url = location;
        is_post = false;
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
    let mut cmd = Command::new("gh");
    cmd.args(["issue", "comment", &issue.to_string(), "--body-file", "-"])
        .current_dir(repo_root);
    match run_bounded(cmd, content.as_bytes()) {
        Ok(out) if out.success => (true, "commented".to_string()),
        Ok(out) => {
            let err = out.stderr.trim().to_string();
            let code = out.code.unwrap_or(1);
            (
                false,
                if err.is_empty() {
                    format!("gh_exit_{code}")
                } else {
                    err
                },
            )
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (false, "gh_cli_missing".to_string()),
        Err(e) => (false, format!("gh_spawn_error:{e}")),
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

#[cfg(test)]
mod tests {
    use super::{bounded_timeout_secs, curl_config, follows_redirect, run_bounded};
    use std::time::Duration;

    #[test]
    fn api_key_travels_only_in_the_stdin_config() {
        let cfg = curl_config(" k\"e\\y ", Some("{\"a\": \"b\\\\c\"}")).expect("config");
        assert_eq!(
            cfg,
            "header = \"Content-Type: application/json\"\n\
             data-binary = \"{\\\"a\\\": \\\"b\\\\\\\\c\\\"}\"\n\
             header = \"Accept: application/json\"\n\
             header = \"Authorization: Bearer k\\\"e\\\\y\"\n"
        );
        let get = curl_config("", None).expect("get config");
        assert_eq!(get, "header = \"Accept: application/json\"\n");
        assert!(curl_config("bad\nkey", None).is_err());
    }

    #[test]
    fn redirects_follow_urllib_rules() {
        for code in [301, 302, 303] {
            assert!(follows_redirect(code, true));
            assert!(follows_redirect(code, false));
        }
        assert!(!follows_redirect(307, true));
        assert!(!follows_redirect(308, true));
        assert!(follows_redirect(307, false));
        assert!(!follows_redirect(200, false));
    }

    #[test]
    fn timeout_matches_urlopen_floor() {
        assert_eq!(bounded_timeout_secs(0.1), Some(Duration::from_millis(500)));
        assert_eq!(
            bounded_timeout_secs(f64::NAN),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            bounded_timeout_secs(7200.0),
            Some(Duration::from_secs(7200))
        );
        assert_eq!(bounded_timeout_secs(f64::INFINITY), None);
    }

    #[cfg(unix)]
    #[test]
    fn child_output_is_capped_and_drained() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args([
            "-c",
            "head -c 300000 /dev/zero; head -c 200000 /dev/zero >&2; cat >/dev/null",
        ]);
        let out = run_bounded(cmd, &vec![b'x'; 200_000]).expect("run");
        assert!(out.success);
        assert_eq!(out.stdout.len(), 64 * 1024);
        assert_eq!(out.stderr.len(), 64 * 1024);
    }
}
