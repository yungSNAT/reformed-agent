//! Shared helpers for ported operational gates, harnesses and report scripts.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use regex::Regex;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;

pub(crate) fn resolve_repo_root(raw: &str) -> Result<PathBuf, String> {
    resolve_argument(&py::expanduser(raw)?)
}

pub(crate) fn require_existing_dir(path: &Path, label: &str) -> Result<(), String> {
    if path.exists() {
        Ok(())
    } else {
        Err(format!("{label} does not exist: {}", py::path_str(path)))
    }
}

pub(crate) fn default_sync_report_path(repo_root: &Path, prefix: &str) -> PathBuf {
    repo_root
        .join(".sync-reports")
        .join(format!("{prefix}-{}.json", py::utc_stamp()))
}

pub(crate) fn resolve_report_path(
    repo_root: &Path,
    explicit: &str,
    prefix: &str,
) -> Result<PathBuf, String> {
    if explicit.is_empty() {
        Ok(default_sync_report_path(repo_root, prefix))
    } else {
        resolve_argument(&py::expanduser(explicit)?)
    }
}

pub(crate) fn write_json_pretty(path: &Path, value: &Py, sort_keys: bool) -> Result<(), String> {
    write_text_creating_parents(path, &(py::dumps(value, Some(2), sort_keys) + "\n"))
}

pub(crate) fn write_text_creating_parents(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn tail_chars(text: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }
    let start = text
        .char_indices()
        .rev()
        .nth(max_chars - 1)
        .map_or(0, |(index, _)| index);
    &text[start..]
}

pub(crate) fn py_splitlines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let stripped = text.strip_suffix('\n').unwrap_or(text);
    stripped.split('\n').collect()
}

pub(crate) fn tail_lines(text: &str, max_lines: usize) -> Vec<String> {
    let lines = py_splitlines(py::strip(text));
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].iter().map(|s| (*s).to_string()).collect()
}

pub(crate) fn shlex_split(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // Python shlex's whitespace; a form feed or vertical tab is part of a word.
            ' ' | '\t' | '\r' | '\n' => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err("No closing quotation".to_string()),
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        // Python shlex, unlike a POSIX shell, escapes only `"` and `\` here.
                        Some('\\') => match chars.next() {
                            Some(esc @ ('"' | '\\')) => current.push(esc),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err("No escaped character".to_string()),
                        },
                        Some(c) => current.push(c),
                        None => return Err("No closing quotation".to_string()),
                    }
                }
            }
            '\\' => {
                in_token = true;
                // No line continuation: Python keeps an escaped newline.
                match chars.next() {
                    Some(c) => current.push(c),
                    None => return Err("No escaped character".to_string()),
                }
            }
            c => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

#[cfg(test)]
#[path = "script_helpers_shlex_tests.rs"]
mod shlex_tests;

pub(crate) fn shlex_quote(token: &str) -> String {
    if token.is_empty() {
        return "''".to_string();
    }
    let safe = token
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b));
    if safe {
        return token.to_string();
    }
    format!("'{}'", token.replace('\'', "'\"'\"'"))
}

pub(crate) fn shlex_join(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| shlex_quote(t))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) struct CaptureResult {
    pub(crate) exit_code: i32,
    pub(crate) ok: bool,
    pub(crate) timed_out: bool,
    pub(crate) started: Timestamp,
    pub(crate) finished: Timestamp,
    pub(crate) elapsed_ms: i128,
    pub(crate) duration_seconds: f64,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

fn normalize_newlines(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes)
        .map(|s| s.replace("\r\n", "\n").replace('\r', "\n"))
        .map_err(|e| format!("command output is not valid UTF-8: {e}"))
}

#[cfg(unix)]
fn status_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| -s))
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn status_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

pub(crate) fn run_process(
    argv: &[String],
    cwd: &Path,
    timeout_secs: Option<u64>,
) -> Result<CaptureResult, String> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| "cannot run empty command".to_string())?;
    let started = Timestamp::now();
    let timer = Instant::now();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run {program}: {e}"))?;

    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let deadline = timeout_secs.map(|s| timer + Duration::from_secs(s));

    let (wait_res, out_res, err_res) = std::thread::scope(|scope| {
        let out_reader = scope.spawn(move || -> std::io::Result<Vec<u8>> {
            let mut buf = Vec::new();
            out_pipe.read_to_end(&mut buf)?;
            Ok(buf)
        });
        let err_reader = scope.spawn(move || -> std::io::Result<Vec<u8>> {
            let mut buf = Vec::new();
            err_pipe.read_to_end(&mut buf)?;
            Ok(buf)
        });

        let wait_res = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(Some(status)),
                Ok(None) => {
                    if let Some(limit) = deadline {
                        if Instant::now() >= limit {
                            let _ = child.kill();
                            let _ = child.wait();
                            break Ok(None);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!("failed while waiting for {program}: {e}"));
                }
            }
        };

        (wait_res, out_reader.join(), err_reader.join())
    });

    let status = wait_res?;
    let timed_out = status.is_none();
    let out_bytes = out_res
        .map_err(|_| format!("stdout reader thread panicked for {program}"))?
        .map_err(|e| format!("failed reading stdout of {program}: {e}"))?;
    let err_bytes = err_res
        .map_err(|_| format!("stderr reader thread panicked for {program}"))?
        .map_err(|e| format!("failed reading stderr of {program}: {e}"))?;

    let finished = Timestamp::now();
    let elapsed_secs = timer.elapsed().as_secs_f64();
    let elapsed_ms = (elapsed_secs * 1000.0) as i128;
    let duration_seconds = (elapsed_secs * 1000.0).round_ties_even() / 1000.0;
    let exit_code = if timed_out {
        124
    } else {
        status.map_or(1, status_code)
    };
    let ok = !timed_out && exit_code == 0;
    Ok(CaptureResult {
        exit_code,
        ok,
        timed_out,
        started,
        finished,
        elapsed_ms,
        duration_seconds,
        stdout: normalize_newlines(out_bytes)?,
        stderr: normalize_newlines(err_bytes)?,
    })
}

pub(crate) fn run_bash_lc(command: &str, cwd: &Path) -> Result<CaptureResult, String> {
    let argv = vec!["bash".to_string(), "-lc".to_string(), command.to_string()];
    run_process(&argv, cwd, None)
}

pub(crate) fn isoformat_secs(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

pub(crate) fn extract_report_path(output: &str, patterns: &[&str]) -> Option<String> {
    let compiled: Vec<Regex> = patterns
        .iter()
        .map(|p| Regex::new(p).expect("valid report pattern"))
        .collect();
    for line in py_splitlines(output) {
        for re in &compiled {
            if let Some(cap) = re.captures(line) {
                if let Some(m) = cap.get(1) {
                    return Some(py::strip(m.as_str()).to_string());
                }
            }
        }
    }
    None
}

pub(crate) fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

pub(crate) fn read_rust_surface(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    if path.is_file() {
        parts.push(read_utf8(path)?);
    }
    if path.is_dir() {
        let mut files = Vec::new();
        collect_rs_files(path, &mut files)?;
        for file in files {
            parts.push(read_utf8(&file)?);
        }
    } else if path.extension().is_some_and(|e| e == "rs") {
        let split_dir = path.with_extension("");
        if split_dir.is_dir() {
            let mut files = Vec::new();
            collect_rs_files(&split_dir, &mut files)?;
            for file in files {
                parts.push(read_utf8(&file)?);
            }
        }
    }
    if parts.is_empty() {
        return read_utf8(path);
    }
    Ok(parts.join("\n"))
}

pub(crate) fn read_utf8(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map(|s| s.replace("\r\n", "\n").replace('\r', "\n"))
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn py_int(value: Option<&Py>, default: i128) -> i128 {
    match value {
        Some(Py::Int(i)) => *i,
        Some(Py::Bool(b)) => i128::from(*b),
        Some(Py::Float(f)) => *f as i128,
        Some(Py::Str(s)) => py::strip(s).parse::<i128>().unwrap_or(default),
        _ => default,
    }
}

pub(crate) fn py_float(value: Option<&Py>, default: f64) -> f64 {
    match value {
        Some(Py::Float(f)) => *f,
        Some(Py::Int(i)) => *i as f64,
        Some(Py::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Py::Str(s)) => py::strip(s).parse::<f64>().unwrap_or(default),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_rs_files, extract_report_path, fs, py_float, py_int, read_rust_surface,
        run_process, shlex_join, shlex_split, Py,
    };
    use std::path::Path;

    #[test]
    fn quoted_command_arguments_round_trip_without_losing_empty_values() {
        let args = [
            "report",
            "weekly summary",
            "",
            "O'Brien",
            r"C:\reports\archive",
        ]
        .map(String::from);
        assert_eq!(shlex_split(&shlex_join(&args)).unwrap(), args);
        let input = r#"report "weekly \"status\"" 'a''b' "kept\z""#;
        assert_eq!(
            shlex_split(input).unwrap(),
            ["report", "weekly \"status\"", "ab", "kept\\z"]
        );

        for unfinished in ["'weekly", "\"weekly"] {
            assert_eq!(shlex_split(unfinished).unwrap_err(), "No closing quotation");
        }
        assert_eq!(shlex_split("weekly\\").unwrap_err(), "No escaped character");
    }

    #[cfg(unix)]
    #[test]
    fn process_timeout_returns_exit_124_with_no_output() {
        let cwd = tempfile::tempdir().unwrap();
        let args = ["sh", "-c", "exec sleep 30"].map(String::from);
        let result = run_process(&args, cwd.path(), Some(0)).unwrap();

        assert!(result.timed_out);
        assert!(!result.ok);
        assert_eq!(result.exit_code, 124);
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn source_surface_includes_sorted_rust_siblings_and_reports_missing_files() {
        let root = tempfile::tempdir().unwrap();
        let module = root.path().join("report.rs");
        let parts = module.with_extension("");
        fs::create_dir_all(parts.join("a")).unwrap();
        fs::write(&module, "pub fn summary() {}\r\n").unwrap();
        fs::write(parts.join("z.rs"), "fn z() {}\r\n").unwrap();
        fs::write(parts.join("a").join("first.rs"), "fn first() {}\r\n").unwrap();
        fs::write(parts.join("notes.txt"), "fixture notes").unwrap();
        assert_eq!(
            read_rust_surface(&module).unwrap(),
            "pub fn summary() {}\n\nfn first() {}\n\nfn z() {}\n"
        );
        assert_eq!(
            read_rust_surface(&parts).unwrap(),
            "fn first() {}\n\nfn z() {}\n"
        );

        let last = parts.join("zz.rs");
        let mut files = vec![last.clone()];
        collect_rs_files(&parts, &mut files).unwrap();
        assert_eq!(
            files,
            [parts.join("a").join("first.rs"), parts.join("z.rs"), last]
        );

        let missing = root.path().join("missing.rs");
        assert!(read_rust_surface(&missing)
            .unwrap_err()
            .contains("missing.rs"));
        assert!(collect_rs_files(&missing, &mut Vec::new())
            .unwrap_err()
            .contains("missing.rs"));
    }

    #[test]
    fn numeric_coercion_and_report_path_match_the_python_contract() {
        assert_eq!(py_int(Some(&Py::Bool(true)), 7), 1);
        assert_eq!(py_int(Some(&Py::Float(-4.5)), 7), -4);
        assert_eq!(py_int(Some(&Py::Str(" -8 ".into())), 7), -8);
        assert_eq!(py_float(Some(&Py::Int(-4)), 7.0), -4.0);
        assert_eq!(py_float(Some(&Py::Bool(false)), 7.0), 0.0);
        assert_eq!(py_float(Some(&Py::Bool(true)), 7.0), 1.0);
        assert_eq!(py_float(Some(&Py::Str(" 3.5 ".into())), 7.0), 3.5);
        let hints = [r"^Export complete$", r"^Report: (.*)$"];
        assert_eq!(
            extract_report_path(
                "Export complete\r\nReport:  reports/weekly summary.json  \r\n",
                &hints
            )
            .as_deref(),
            Some("reports/weekly summary.json")
        );
        assert_eq!(extract_report_path("Export complete\r\n", &hints), None);
    }

    #[test]
    fn run_process_captures_output_and_reports_missing_binary() {
        let missing = run_process(
            &["definitely-not-a-real-binary-xyz-123".to_string()],
            Path::new("."),
            Some(5),
        );
        assert!(missing.is_err());

        #[cfg(unix)]
        let res = run_process(
            &[
                "sh".to_string(),
                "-c".to_string(),
                "printf hi; exit 0".to_string(),
            ],
            Path::new("."),
            Some(5),
        )
        .unwrap();
        #[cfg(windows)]
        let res = run_process(
            &["cmd".to_string(), "/C".to_string(), "echo hi".to_string()],
            Path::new("."),
            Some(5),
        )
        .unwrap();
        assert!(res.ok);
        assert!(!res.timed_out);
        assert!(res.stdout.contains("hi"));
    }
}
