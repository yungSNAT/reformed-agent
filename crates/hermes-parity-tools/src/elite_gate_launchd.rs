//! Install, inspect and remove the launchd user agent that runs the nightly
//! elite gate on macOS. Ports of `scripts/install-elite-gate-launchd.sh`,
//! `scripts/status-elite-gate-launchd.sh` and
//! `scripts/uninstall-elite-gate-launchd.sh`.

use std::fs;
use std::io::{self, Write};
use std::process::{Command, Stdio};

use clap::Args;
use regex::Regex;

use crate::nightly_elite_gate::{exit_code, home_dir, repo_root_arg};

const DEFAULT_PREFIX: &str = "com.hermes_agent_ultra";

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Repository root (default: the current directory)
    #[arg(long)]
    repo_root: Option<String>,
    /// launchd label prefix
    #[arg(long, default_value = DEFAULT_PREFIX)]
    label_prefix: String,
    /// LaunchAgents directory (default: ~/Library/LaunchAgents)
    #[arg(long)]
    agents_dir: Option<String>,
    /// Log directory (default: ~/.hermes-agent-ultra/logs)
    #[arg(long)]
    log_dir: Option<String>,
    /// Run hour, 0 to 23
    #[arg(long, default_value = "3", allow_hyphen_values = true)]
    hour: String,
    /// Run minute, 0 to 59
    #[arg(long, default_value = "30", allow_hyphen_values = true)]
    minute: String,
    /// Only write the plist, do not load it
    #[arg(long)]
    no_load: bool,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// launchd label prefix
    #[arg(long, default_value = DEFAULT_PREFIX)]
    label_prefix: String,
    /// Log directory (default: ~/.hermes-agent-ultra/logs)
    #[arg(long)]
    log_dir: Option<String>,
    /// Number of log lines to show
    #[arg(long = "tail", default_value_t = 30)]
    tail_n: usize,
    /// Print the full launchctl record (first 140 lines)
    #[arg(long)]
    verbose: bool,
}

#[derive(Args, Debug)]
pub struct UninstallArgs {
    /// launchd label prefix
    #[arg(long, default_value = DEFAULT_PREFIX)]
    label_prefix: String,
    /// LaunchAgents directory (default: ~/Library/LaunchAgents)
    #[arg(long)]
    agents_dir: Option<String>,
}

fn uid() -> Result<String, String> {
    let out = Command::new("id")
        .arg("-u")
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("id -u: {e}"))?;
    if !out.status.success() {
        return Err("id -u failed".to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn agents_dir(raw: Option<&str>) -> Result<String, String> {
    match raw {
        Some(dir) => Ok(dir.to_string()),
        None => Ok(format!("{}/Library/LaunchAgents", home_dir()?)),
    }
}

fn log_dir(raw: Option<&str>) -> Result<String, String> {
    match raw {
        Some(dir) => Ok(dir.to_string()),
        None => Ok(format!("{}/.hermes-agent-ultra/logs", home_dir()?)),
    }
}

/// `launchctl bootout gui/<uid> <plist>` with its output and failure ignored.
fn bootout(domain: &str, plist: &str) {
    let _ = Command::new("launchctl")
        .args(["bootout", domain, plist])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A value the script's `[[ =~ ^[0-9]+$ ]]` and range test accept.
fn schedule_value(raw: &str, max: u32) -> Option<u32> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u32>().ok().filter(|v| *v <= max)
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `${path##*/}`: the text after the last slash.
fn last_component(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn plist(label: &str, repo_root: &str, log_dir: &str, hour: &str, minute: &str) -> String {
    let program: String = [
        "/usr/bin/env".to_string(),
        "cargo".to_string(),
        "run".to_string(),
        "--locked".to_string(),
        "-q".to_string(),
        "--manifest-path".to_string(),
        format!("{repo_root}/Cargo.toml"),
        "-p".to_string(),
        "hermes-parity-tools".to_string(),
        "--".to_string(),
        "run-nightly-elite-gate".to_string(),
        "--repo-root".to_string(),
        repo_root.to_string(),
        "--log-dir".to_string(),
        format!("{log_dir}/elite-gate"),
    ]
    .iter()
    .map(|arg| format!("    <string>{}</string>\n", xml(arg)))
    .collect();
    let env = [
        ("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
        ("AUTOMATION_SUMMARY_SINK_ORDER", "contextlattice,github,local".to_string()),
        ("AUTOMATION_SUMMARY_CONTEXT_PROJECT", last_component(repo_root).to_string()),
        ("AUTOMATION_SUMMARY_CONTEXT_TOPIC_PATH", "ops/nightly-elite-gate".to_string()),
        ("AUTOMATION_SUMMARY_CONTEXT_FILE_NAME", "ops/nightly-elite-gate.md".to_string()),
        ("AUTOMATION_SUMMARY_GITHUB_ISSUE", "0".to_string()),
        (
            "AUTOMATION_SUMMARY_LOCAL_PATH",
            format!("{log_dir}/elite-gate/nightly-elite-gate-fallback.log"),
        ),
    ]
    .iter()
    .map(|(k, v)| format!("    <key>{k}</key>\n    <string>{}</string>\n", xml(v)))
    .collect::<String>();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{program}  </array>
  <key>WorkingDirectory</key>
  <string>{root}</string>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>{hour}</integer>
    <key>Minute</key>
    <integer>{minute}</integer>
  </dict>
  <key>StandardOutPath</key>
  <string>{logs}/elite-gate-launchd.out.log</string>
  <key>StandardErrorPath</key>
  <string>{logs}/elite-gate-launchd.err.log</string>
</dict>
</plist>
"#,
        label = xml(label),
        root = xml(repo_root),
        logs = xml(log_dir),
    )
}

pub fn run_install(args: InstallArgs) -> Result<i32, String> {
    let repo_root = repo_root_arg(args.repo_root.as_deref())?;
    let agents = agents_dir(args.agents_dir.as_deref())?;
    let logs = log_dir(args.log_dir.as_deref())?;
    let Some(hour) = schedule_value(&args.hour, 23) else {
        eprintln!("Invalid --hour: {}", args.hour);
        return Ok(2);
    };
    let Some(minute) = schedule_value(&args.minute, 59) else {
        eprintln!("Invalid --minute: {}", args.minute);
        return Ok(2);
    };

    let label = format!("{}.nightly_elite_gate", args.label_prefix);
    let plist_path = format!("{agents}/{label}.plist");
    for dir in [agents.clone(), logs.clone(), format!("{logs}/elite-gate")] {
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {dir}: {e}"))?;
    }
    fs::write(
        &plist_path,
        plist(&label, &repo_root, &logs, &args.hour, &args.minute),
    )
    .map_err(|e| format!("{plist_path}: {e}"))?;

    if !args.no_load {
        let domain = format!("gui/{}", uid()?);
        bootout(&domain, &plist_path);
        let status = Command::new("launchctl")
            .args(["bootstrap", &domain, &plist_path])
            .status()
            .map_err(|e| format!("launchctl: {e}"))?;
        if !status.success() {
            return Ok(exit_code(status));
        }
    }
    println!("Installed: {plist_path}");
    println!("Label: {label}");
    println!("Schedule: daily at {hour:02}:{minute:02} local time");
    if args.no_load {
        println!("Service not loaded (--no-load).");
    } else {
        println!("Service loaded.");
    }
    Ok(0)
}

/// First `pid = <digits>` value of a `launchctl print` record, as the script's
/// grep and sed found it (a line without digits comes back whole).
fn pid_of(record: &str) -> Option<String> {
    let line = Regex::new(r"^\s*pid = ").expect("regex");
    let digits = Regex::new(r"^.*pid = ([0-9]+).*$").expect("regex");
    let found = record.lines().find(|l| line.is_match(l))?;
    Some(match digits.captures(found) {
        Some(c) => c[1].to_string(),
        None => found.to_string(),
    })
}

/// The first `n` lines, as `sed -n '1,<n>p'` prints them.
fn head_lines(data: &[u8], n: usize) -> &[u8] {
    let mut seen = 0;
    for (i, b) in data.iter().enumerate() {
        if *b == b'\n' {
            seen += 1;
            if seen == n {
                return &data[..=i];
            }
        }
    }
    data
}

/// The last `n` lines, as `tail -n <n>` prints them.
fn tail_lines(data: &[u8], n: usize) -> &[u8] {
    if n == 0 || data.is_empty() {
        return &[];
    }
    let body = data.strip_suffix(b"\n").unwrap_or(data);
    let mut seen = 0;
    for (i, b) in body.iter().enumerate().rev() {
        if *b == b'\n' {
            seen += 1;
            if seen == n {
                return &data[i + 1..];
            }
        }
    }
    data
}

pub fn run_status(args: StatusArgs) -> Result<i32, String> {
    let logs = log_dir(args.log_dir.as_deref())?;
    let label = format!("{}.nightly_elite_gate", args.label_prefix);
    let reference = format!("gui/{}/{label}", uid()?);
    let mut out = io::stdout().lock();
    let printed = Command::new("launchctl")
        .args(["print", &reference])
        .stderr(Stdio::null())
        .output();
    match printed {
        Ok(record) if record.status.success() => {
            let _ = writeln!(out, "[loaded] {label}");
            if let Some(pid) = pid_of(&String::from_utf8_lossy(&record.stdout)) {
                if !pid.is_empty() {
                    let _ = writeln!(out, "  pid: {pid}");
                }
            }
            if args.verbose {
                let _ = out.write_all(head_lines(&record.stdout, 140));
            }
        }
        _ => {
            let _ = writeln!(out, "[not loaded] {label}");
        }
    }
    for file in [
        format!("{logs}/elite-gate-launchd.out.log"),
        format!("{logs}/elite-gate-launchd.err.log"),
        format!("{logs}/elite-gate/nightly-elite-gate-latest.json"),
        format!("{logs}/elite-gate/nightly-elite-gate-fallback.log"),
    ] {
        let _ = writeln!(out, "\n=== {file} ===");
        if std::path::Path::new(&file).is_file() {
            let data = fs::read(&file).map_err(|e| format!("{file}: {e}"))?;
            let shown = if file.ends_with(".json") {
                &data[..]
            } else {
                tail_lines(&data, args.tail_n)
            };
            let _ = out.write_all(shown);
        } else {
            let _ = writeln!(out, "(missing)");
        }
    }
    let _ = out.flush();
    Ok(0)
}

pub fn run_uninstall(args: UninstallArgs) -> Result<i32, String> {
    let agents = agents_dir(args.agents_dir.as_deref())?;
    let label = format!("{}.nightly_elite_gate", args.label_prefix);
    let plist_path = format!("{agents}/{label}.plist");
    bootout(&format!("gui/{}", uid()?), &plist_path);
    match fs::remove_file(&plist_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("rm: {plist_path}: {e}")),
    }
    println!("Removed: {plist_path}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_values_follow_the_script_checks() {
        assert_eq!(schedule_value("3", 23), Some(3));
        assert_eq!(schedule_value("23", 23), Some(23));
        assert_eq!(schedule_value("24", 23), None);
        assert_eq!(schedule_value("-1", 23), None);
        assert_eq!(schedule_value("", 23), None);
        assert_eq!(schedule_value("1a", 59), None);
    }

    #[test]
    fn pid_comes_from_the_first_pid_line() {
        let record = "gui/501/x = {\n\tstate = running\n\tpid = 4242\n\tpid = 7\n}\n";
        assert_eq!(pid_of(record).as_deref(), Some("4242"));
        assert_eq!(pid_of("\tpid = none\n").as_deref(), Some("\tpid = none"));
        assert_eq!(pid_of("state = waiting\n"), None);
    }

    #[test]
    fn head_and_tail_count_lines_like_sed_and_tail() {
        assert_eq!(head_lines(b"a\nb\nc\n", 2), b"a\nb\n");
        assert_eq!(head_lines(b"a\nb", 5), b"a\nb");
        assert_eq!(tail_lines(b"a\nb\nc\n", 2), b"b\nc\n");
        assert_eq!(tail_lines(b"a\nb\nc", 2), b"b\nc");
        assert_eq!(tail_lines(b"a\nb\n", 5), b"a\nb\n");
        assert_eq!(tail_lines(b"a\n", 0), b"");
    }

    #[test]
    fn plist_escapes_xml_and_runs_the_rust_gate() {
        let text = plist("p.nightly_elite_gate", "/r/a&b", "/l", "3", "30");
        assert!(text.contains("<string>/r/a&amp;b/Cargo.toml</string>"));
        assert!(text.contains("<string>run-nightly-elite-gate</string>"));
        assert!(text.contains("<string>a&amp;b</string>"));
        assert!(text.contains("<integer>30</integer>"));
    }
}
