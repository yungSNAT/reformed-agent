//! Sync upstream changes into this fork. Port of `scripts/sync-upstream.sh`,
//! which stays as a shim that runs this subcommand.
//!
//! Options are read as the script read them (a value may start with `--`, the
//! last occurrence wins, an empty value is refused), so the shim passes its
//! own `--repo-root` first and the caller's options after it. Every line the
//! script printed is printed the same way; `scripts/upstream_webhook_sync.py`
//! reads the final `[sync-upstream] Report: <path>` line. Gate commands still
//! run through `bash -lc`, so a configured command string keeps its meaning.

use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use clap::Args;
use serde_json::Value;

#[derive(Args, Debug)]
#[command(disable_help_flag = true)]
pub struct SyncUpstreamArgs {
    /// Options as `scripts/sync-upstream.sh` takes them (see `--help`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

const USAGE: &str = "Usage: scripts/sync-upstream.sh [options]

Syncs upstream changes into this fork in a reproducible way.

Options:
  --repo-root <path>        Repository root (default: script parent)
  --origin <name>           Origin remote name (default: origin)
  --upstream <name>         Upstream remote name (default: upstream)
  --base-branch <name>      Branch to sync (default: main)
  --mode <mode>             branch-pr (default) | direct-main
  --strategy <strategy>     merge (default) | cherry-pick
  --report-dir <path>       Report directory (default: <repo>/.sync-reports)
  --conflict-label <label>  Label for conflict issues (default: upstream-sync-conflict)
  --no-conflict-issue       Disable auto issue creation on conflicts
  --strict-risk-gate        Block sync when high-risk file paths changed upstream
  --no-strict-risk-gate     Disable strict high-risk path blocking (default)
  --allow-risk-paths        Override strict risk gate for this run
  --risk-paths-file <path>  Glob pattern file for high-risk path checks
  --no-tests                Skip the verification command and fork carry tests
  --test-cmd <command>      Verification command (default: cargo test -p hermes-gateway)
  --redteam-gate            Run adversarial red-team gate after verification (default)
  --no-redteam-gate         Skip adversarial red-team gate
  --redteam-cmd <command>   Red-team gate command (default: cargo run --locked -q -p hermes-parity-tools -- run-redteam-gate)
  --elite-gate              Run consolidated elite sync gate (optional)
  --no-elite-gate           Skip consolidated elite sync gate (default)
  --elite-cmd <command>     Elite gate command (default: python3 scripts/run-elite-sync-gate.py)
  --elite-rollback-cmd <c>  Optional rollback command executed by elite gate on failure
  --no-doc-refresh          Skip generated parity/readme doc refresh
  --doc-refresh-cmd <cmd>   Override generated docs refresh command
  --no-pr                   Do not open a PR in branch-pr mode
  --draft-pr                Open PR as draft in branch-pr mode
  --pr-labels <csv>         Labels to apply on created PR (default: upstream-sync,parity-sync)
  --dry-run                 Show what would happen, emit report, and exit
  -h, --help                Show help
";

const FORK_CARRY: [&str; 7] = [
    "run",
    "--locked",
    "-q",
    "-p",
    "hermes-parity-tools",
    "--",
    "check-fork-carry",
];

/// An exit code: the run stops and the process exits with it.
type Step<T> = Result<T, i32>;

pub fn run(args: SyncUpstreamArgs) -> Result<i32, String> {
    Ok(sync(args.args))
}

/// `${NAME:-default}`: an unset or empty variable takes the default.
pub(crate) fn env_or(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => value,
        _ => default.to_string(),
    }
}

/// `command -v <name>` for a program on `PATH`.
pub(crate) fn on_path(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| {
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        fs::metadata(dir.join(name)).is_ok_and(|m| m.is_file() && executable(&m))
    })
}

#[cfg(unix)]
fn executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &fs::Metadata) -> bool {
    true
}

/// The exit code a shell reports for a finished child.
#[cfg(unix)]
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

#[cfg(not(unix))]
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn log(message: &str) {
    println!("[sync-upstream] {message}");
}

fn die(message: &str) -> i32 {
    eprintln!("[sync-upstream] ERROR: {message}");
    1
}

/// Run a program with inherited output; a failure, or a program that cannot
/// start (exit 127, as the shell reports it), is its exit code.
fn status_of(program: &str, args: &[&str]) -> Step<()> {
    match Command::new(program).args(args).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(exit_code(status)),
        Err(e) => {
            eprintln!("{program}: {e}");
            Err(127)
        }
    }
}

/// `git ...` under `set -e`: a failure ends the run with git's exit code.
fn git(args: &[&str]) -> Step<()> {
    status_of("git", args)
}

/// `git ... >/dev/null 2>&1`, for its status only.
fn git_quiet(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `$(git ...)`: stdout without its trailing newlines, stderr inherited.
fn git_capture(args: &[&str]) -> (bool, String) {
    match Command::new("git")
        .args(args)
        .stderr(Stdio::inherit())
        .output()
    {
        Ok(out) => (
            out.status.success(),
            strip_newlines(&String::from_utf8_lossy(&out.stdout)),
        ),
        Err(e) => {
            eprintln!("git: {e}");
            (false, String::new())
        }
    }
}

/// `VAR="$(git ...)"` under `set -e`.
fn git_value(args: &[&str]) -> Step<String> {
    match Command::new("git")
        .args(args)
        .stderr(Stdio::inherit())
        .output()
    {
        Ok(out) if out.status.success() => {
            Ok(strip_newlines(&String::from_utf8_lossy(&out.stdout)))
        }
        Ok(out) => Err(exit_code(out.status)),
        Err(e) => {
            eprintln!("git: {e}");
            Err(127)
        }
    }
}

fn strip_newlines(text: &str) -> String {
    text.trim_end_matches('\n').to_string()
}

/// `OUT="$(bash -lc "<command>" 2>&1)"`: stdout and stderr through one pipe,
/// so their order is kept, without trailing newlines.
fn bash_capture(command: &str) -> (bool, String) {
    let run = || -> std::io::Result<(ExitStatus, Vec<u8>)> {
        let (mut reader, writer) = std::io::pipe()?;
        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(command)
            .stdout(writer.try_clone()?)
            .stderr(writer);
        let mut child = cmd.spawn()?;
        drop(cmd);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok((child.wait()?, bytes))
    };
    match run() {
        Ok((status, bytes)) => (
            status.success(),
            strip_newlines(&String::from_utf8_lossy(&bytes)),
        ),
        Err(e) => (false, format!("bash: {e}")),
    }
}

/// `printf '%q'`, as bash quotes a word for reuse as shell input.
pub(crate) fn shell_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".to_string();
    }
    if text.chars().any(|c| c.is_ascii_control()) {
        let mut out = String::from("$'");
        for c in text.chars() {
            match c {
                '\x07' => out.push_str("\\a"),
                '\x08' => out.push_str("\\b"),
                '\x1b' => out.push_str("\\E"),
                '\x0c' => out.push_str("\\f"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\x0b' => out.push_str("\\v"),
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                c if c.is_ascii_control() => out.push_str(&format!("\\{:03o}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('\'');
        return out;
    }
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for c in text.chars() {
        let escape = match c {
            ' ' | '\'' | '"' | '\\' | '|' | '&' | ';' | '(' | ')' | '<' | '>' | '!' | '{' | '}'
            | '*' | '[' | '?' | ']' | '^' | '$' | '`' | ',' => true,
            '#' => prev.is_none(),
            '~' => matches!(prev, None | Some('=') | Some(':')),
            _ => false,
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

/// Whether `path` matches a bash pattern as `[[ "$path" == $pattern ]]`
/// tests it: `*` and `?` also match `/`, `[...]` sets (with `!` or `^`,
/// ranges and `[:class:]`), backslash escapes, and the extglob forms
/// `?(...)`, `*(...)`, `+(...)` and `@(...)`. `!(...)` is not supported and
/// matches literally.
pub(crate) fn pattern_matches(pattern: &str, path: &str) -> bool {
    let regex = format!(
        "^(?s:{})$",
        glob_regex(&pattern.chars().collect::<Vec<_>>())
    );
    regex::Regex::new(&regex).is_ok_and(|re| re.is_match(path))
}

fn glob_regex(chars: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if matches!(c, '?' | '*' | '+' | '@') && next == Some('(') {
            if let Some(close) = group_end(chars, i + 1) {
                let alternatives = split_alternatives(&chars[i + 2..close])
                    .iter()
                    .map(|alt| glob_regex(alt))
                    .collect::<Vec<_>>()
                    .join("|");
                let suffix = match c {
                    '?' => "?",
                    '*' => "*",
                    '+' => "+",
                    _ => "",
                };
                out.push_str(&format!("(?:{alternatives}){suffix}"));
                i = close + 1;
                continue;
            }
        }
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '\\' => {
                if let Some(n) = next {
                    out.push_str(&regex::escape(&n.to_string()));
                    i += 1;
                } else {
                    out.push_str(&regex::escape("\\"));
                }
            }
            '[' => match bracket(chars, i) {
                Some((class, end)) => {
                    out.push_str(&class);
                    i = end;
                }
                None => out.push_str(&regex::escape("[")),
            },
            c => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    out
}

/// Index of the `)` closing the group whose `(` is at `open`.
fn group_end(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_alternatives(chars: &[char]) -> Vec<Vec<char>> {
    let mut parts = vec![Vec::new()];
    let mut depth = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                parts.last_mut().expect("part").push(c);
                if let Some(&n) = chars.get(i + 1) {
                    parts.last_mut().expect("part").push(n);
                }
                i += 2;
                continue;
            }
            '(' => depth += 1,
            ')' => depth -= 1,
            '|' if depth == 0 => {
                parts.push(Vec::new());
                i += 1;
                continue;
            }
            _ => {}
        }
        parts.last_mut().expect("part").push(c);
        i += 1;
    }
    parts
}

/// A `[...]` set starting at `start`, as a regex class and the index of its
/// closing `]`; `None` when it does not close, so `[` is literal.
fn bracket(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start + 1;
    let mut class = String::from("[");
    if matches!(chars.get(i), Some('!') | Some('^')) {
        class.push('^');
        i += 1;
    }
    let first = i;
    while i < chars.len() {
        let c = chars[i];
        if c == ']' && i > first {
            class.push(']');
            return Some((class, i));
        }
        if c == '[' && chars.get(i + 1) == Some(&':') {
            if let Some(len) = chars[i + 2..].windows(2).position(|w| w == [':', ']']) {
                let name: String = chars[i + 2..i + 2 + len].iter().collect();
                class.push_str(&format!("[:{name}:]"));
                i += len + 4;
                continue;
            }
        }
        if c == '\\' {
            if let Some(&n) = chars.get(i + 1) {
                class.push_str(&class_char(n));
                i += 2;
                continue;
            }
        }
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|&e| e != ']') {
            class.push_str(&format!("{}-{}", class_char(c), class_char(chars[i + 2])));
            i += 3;
            continue;
        }
        class.push_str(&class_char(c));
        i += 1;
    }
    None
}

fn class_char(c: char) -> String {
    if c.is_ascii_alphanumeric() {
        c.to_string()
    } else {
        format!("\\x{{{:x}}}", c as u32)
    }
}

/// The parity queue summary the script's inline Python printed.
pub(crate) fn queue_summary(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return "unavailable".to_string();
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return "unavailable".to_string();
    };
    let items = match &data {
        Value::Array(_) => data.clone(),
        Value::Object(map) => [map.get("items"), map.get("entries")]
            .into_iter()
            .flatten()
            .find(|v| truthy(v))
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
        _ => Value::Array(Vec::new()),
    };
    let entries: Vec<Value> = match items {
        Value::Array(list) => list,
        // Python counted a dict's keys and a string's characters, none of
        // them dicts.
        Value::Object(map) => vec![Value::Null; map.len()],
        Value::String(s) => vec![Value::Null; s.chars().count()],
        // Python's len() fails on a number or a boolean.
        _ => return "unavailable".to_string(),
    };
    let field = |item: &serde_json::Map<String, Value>, keys: [&str; 2]| -> String {
        match keys.iter().filter_map(|k| item.get(*k)).find(|v| truthy(v)) {
            Some(Value::String(s)) => s.to_lowercase(),
            _ => String::new(),
        }
    };
    let (mut queued, mut actionable) = (0, 0);
    for item in &entries {
        let Value::Object(item) = item else { continue };
        if ["queued", "todo", "pending", "open", "in_progress"]
            .contains(&field(item, ["state", "status"]).as_str())
        {
            queued += 1;
        }
        if ["actionable", "functional", "required"]
            .contains(&field(item, ["classification", "class"]).as_str())
        {
            actionable += 1;
        }
    }
    format!(
        "total={}, queued={queued}, actionable={actionable}",
        entries.len()
    )
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Labels from `--pr-labels`, split on commas and trimmed as `xargs` did.
pub(crate) fn pr_labels(csv: &str) -> Vec<String> {
    csv.lines()
        .next()
        .unwrap_or("")
        .split(',')
        .map(|raw| raw.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|label| !label.is_empty())
        .collect()
}

struct Config {
    repo_root: String,
    origin: String,
    upstream: String,
    base_branch: String,
    mode: String,
    strategy: String,
    run_tests: bool,
    test_cmd: String,
    run_redteam_gate: String,
    redteam_cmd: String,
    run_elite_gate: String,
    elite_cmd: String,
    elite_rollback_cmd: String,
    run_doc_refresh: String,
    doc_refresh_cmd: String,
    create_pr: bool,
    pr_draft: bool,
    pr_labels: String,
    create_conflict_issue: bool,
    conflict_label: String,
    strict_risk_gate: String,
    allow_risk_paths: bool,
    risk_paths_file: String,
    dry_run: bool,
    report_dir: String,
}

enum Parsed {
    Run(Box<Config>),
    Exit(i32),
}

fn parse(args: &[String]) -> Parsed {
    let default_root = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let mut c = Config {
        repo_root: default_root,
        origin: "origin".into(),
        upstream: "upstream".into(),
        base_branch: "main".into(),
        mode: "branch-pr".into(),
        strategy: "merge".into(),
        run_tests: true,
        test_cmd: "cargo test -p hermes-gateway".into(),
        run_redteam_gate: env_or("RUN_REDTEAM_GATE", "1"),
        redteam_cmd: env_or(
            "REDTEAM_CMD",
            "cargo run --locked -q -p hermes-parity-tools -- run-redteam-gate",
        ),
        run_elite_gate: env_or("RUN_ELITE_GATE", "0"),
        elite_cmd: env_or("ELITE_CMD", "python3 scripts/run-elite-sync-gate.py"),
        elite_rollback_cmd: env_or("ELITE_ROLLBACK_CMD", ""),
        run_doc_refresh: env_or("RUN_DOC_REFRESH", "1"),
        doc_refresh_cmd: env_or(
            "DOC_REFRESH_CMD",
            "cargo run --locked -q -p hermes-parity-tools -- generate-parity-dashboard --repo-root . && cargo run --locked -q -p hermes-parity-tools -- generate-readme-sync-status --repo-root .",
        ),
        create_pr: true,
        pr_draft: false,
        pr_labels: env_or("PR_LABELS", "upstream-sync,parity-sync"),
        create_conflict_issue: true,
        conflict_label: "upstream-sync-conflict".into(),
        strict_risk_gate: env_or("STRICT_RISK_GATE", "0"),
        allow_risk_paths: false,
        risk_paths_file: String::new(),
        dry_run: false,
        report_dir: String::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let slot: Option<&mut String> = match flag {
            "--repo-root" => Some(&mut c.repo_root),
            "--origin" => Some(&mut c.origin),
            "--upstream" => Some(&mut c.upstream),
            "--base-branch" => Some(&mut c.base_branch),
            "--mode" => Some(&mut c.mode),
            "--strategy" => Some(&mut c.strategy),
            "--report-dir" => Some(&mut c.report_dir),
            "--conflict-label" => Some(&mut c.conflict_label),
            "--risk-paths-file" => Some(&mut c.risk_paths_file),
            "--test-cmd" => Some(&mut c.test_cmd),
            "--redteam-cmd" => Some(&mut c.redteam_cmd),
            "--elite-cmd" => Some(&mut c.elite_cmd),
            "--elite-rollback-cmd" => Some(&mut c.elite_rollback_cmd),
            "--doc-refresh-cmd" => Some(&mut c.doc_refresh_cmd),
            "--pr-labels" => Some(&mut c.pr_labels),
            _ => None,
        };
        if let Some(slot) = slot {
            match args.get(i + 1) {
                Some(value) if !value.is_empty() => *slot = value.clone(),
                _ => return Parsed::Exit(die(&format!("missing value for {flag}"))),
            }
            i += 2;
            continue;
        }
        match flag {
            "--no-conflict-issue" => c.create_conflict_issue = false,
            "--strict-risk-gate" => c.strict_risk_gate = "1".into(),
            "--no-strict-risk-gate" => c.strict_risk_gate = "0".into(),
            "--allow-risk-paths" => c.allow_risk_paths = true,
            "--no-tests" => c.run_tests = false,
            "--redteam-gate" => c.run_redteam_gate = "1".into(),
            "--no-redteam-gate" => c.run_redteam_gate = "0".into(),
            "--elite-gate" => c.run_elite_gate = "1".into(),
            "--no-elite-gate" => c.run_elite_gate = "0".into(),
            "--no-doc-refresh" => c.run_doc_refresh = "0".into(),
            "--no-pr" => c.create_pr = false,
            "--draft-pr" => c.pr_draft = true,
            "--dry-run" => c.dry_run = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Parsed::Exit(0);
            }
            other => return Parsed::Exit(die(&format!("Unknown argument: {other}"))),
        }
        i += 1;
    }
    Parsed::Run(Box::new(c))
}

fn flag(on: bool) -> &'static str {
    if on {
        "1"
    } else {
        "0"
    }
}

/// The run report file, appended to line by line.
struct Report {
    path: String,
}

impl Report {
    fn write(&self, text: &str, append: bool) -> Step<()> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&self.path);
        match file.and_then(|mut f| f.write_all(text.as_bytes())) {
            Ok(()) => Ok(()),
            Err(e) => {
                eprintln!("{}: {e}", self.path);
                Err(1)
            }
        }
    }

    fn line(&self, text: &str) -> Step<()> {
        self.write(&format!("{text}\n"), true)
    }
}

/// A temporary file under `TMPDIR`, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(text: &str) -> Step<Self> {
        let dir = std::env::temp_dir();
        for n in 0..1000u32 {
            let path = dir.join(format!("sync-upstream.{}.{n}", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    return match f.write_all(text.as_bytes()) {
                        Ok(()) => Ok(Self(path)),
                        Err(e) => {
                            let _ = fs::remove_file(&path);
                            eprintln!("mktemp: {e}");
                            Err(1)
                        }
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    eprintln!("mktemp: {e}");
                    return Err(1);
                }
            }
        }
        eprintln!("mktemp: no free name in {}", dir.display());
        Err(1)
    }

    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn gh_quiet(args: &[&str]) -> bool {
    Command::new("gh")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run the sync with script-style options and return the exit code.
pub(crate) fn sync(args: Vec<String>) -> i32 {
    match parse(&args) {
        Parsed::Exit(code) => code,
        Parsed::Run(config) => match Sync::start(*config) {
            Ok(code) | Err(code) => code,
        },
    }
}

struct Sync {
    c: Config,
    upstream_url: String,
    expected_upstream: String,
    allow_non_official: String,
    origin_ref: String,
    upstream_ref: String,
    origin_sha: String,
    upstream_sha: String,
    timestamp: String,
    sync_branch: String,
    rollback_tag: String,
    report: Report,
}

impl Sync {
    fn start(mut c: Config) -> Step<i32> {
        if c.mode != "branch-pr" && c.mode != "direct-main" {
            return Err(die("--mode must be branch-pr or direct-main"));
        }
        if c.strategy != "merge" && c.strategy != "cherry-pick" {
            return Err(die("--strategy must be merge or cherry-pick"));
        }
        if c.mode == "direct-main" {
            c.create_pr = false;
        }
        if !on_path("git") {
            return Err(die("git is required"));
        }
        if c.report_dir.is_empty() {
            c.report_dir = format!("{}/.sync-reports", c.repo_root);
        }
        if c.risk_paths_file.is_empty() {
            c.risk_paths_file = format!("{}/scripts/upstream-risk-paths.txt", c.repo_root);
        }
        if let Err(e) = fs::create_dir_all(&c.report_dir) {
            eprintln!("mkdir: cannot create directory '{}': {e}", c.report_dir);
            return Err(1);
        }
        if c.create_pr && !on_path("gh") {
            log("gh CLI not found; disabling PR creation.");
            c.create_pr = false;
        }
        if c.create_conflict_issue && !on_path("gh") {
            log("gh CLI not found; disabling conflict issue creation.");
            c.create_conflict_issue = false;
        }
        if let Err(e) = std::env::set_current_dir(&c.repo_root) {
            eprintln!("cd: {}: {e}", c.repo_root);
            return Err(1);
        }
        if !git_quiet(&["rev-parse", "--is-inside-work-tree"]) {
            return Err(die(&format!("Not a git repository: {}", c.repo_root)));
        }
        if !git_quiet(&["remote", "get-url", &c.origin]) {
            return Err(die(&format!(
                "Origin remote '{}' is not configured",
                c.origin
            )));
        }
        if !git_quiet(&["remote", "get-url", &c.upstream]) {
            return Err(die(&format!(
                "Upstream remote '{}' is not configured",
                c.upstream
            )));
        }
        let upstream_url = git_value(&["remote", "get-url", &c.upstream])?;
        let expected_upstream = env_or("EXPECTED_UPSTREAM_REPO", "NousResearch/hermes-agent");
        let allow_non_official = env_or("ALLOW_NON_OFFICIAL_UPSTREAM", "0");
        if allow_non_official != "1"
            && !upstream_url
                .to_lowercase()
                .contains(&expected_upstream.to_lowercase())
        {
            return Err(die(&format!(
                "Upstream remote '{}' URL '{upstream_url}' is not '{expected_upstream}'. Set ALLOW_NON_OFFICIAL_UPSTREAM=1 to bypass.",
                c.upstream
            )));
        }
        if !git_value(&["status", "--porcelain"])?.is_empty() {
            return Err(die(
                "Working tree is not clean. Commit/stash changes before syncing.",
            ));
        }
        log(&format!(
            "Fetching {}/{} and {}/{}...",
            c.origin, c.base_branch, c.upstream, c.base_branch
        ));
        git(&["fetch", &c.origin, &c.base_branch, "--prune"])?;
        git(&["fetch", &c.upstream, &c.base_branch, "--prune"])?;
        let origin_ref = format!("refs/remotes/{}/{}", c.origin, c.base_branch);
        let upstream_ref = format!("refs/remotes/{}/{}", c.upstream, c.base_branch);
        let origin_sha = git_value(&["rev-parse", &origin_ref])?;
        let upstream_sha = git_value(&["rev-parse", &upstream_ref])?;
        let timestamp = jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S").to_string();
        let report = Report {
            path: format!("{}/upstream-sync-{timestamp}.txt", c.report_dir),
        };
        let sync = Sync {
            upstream_url,
            expected_upstream,
            allow_non_official,
            origin_ref,
            upstream_ref,
            origin_sha,
            upstream_sha,
            sync_branch: format!("chore/upstream-sync-{timestamp}"),
            rollback_tag: format!("rollback/upstream-sync-{timestamp}"),
            timestamp,
            report,
            c,
        };
        sync.go()
    }

    fn go(&self) -> Step<i32> {
        let c = &self.c;
        self.header()?;
        if git_quiet(&[
            "merge-base",
            "--is-ancestor",
            &self.upstream_ref,
            &self.origin_ref,
        ]) {
            self.report.line("status: no-updates")?;
            log(&format!(
                "No upstream updates to apply. {} already contains {}.",
                self.origin_ref, self.upstream_ref
            ));
            log(&format!("Report: {}", self.report.path));
            return Ok(0);
        }
        let range = format!("{}..{}", self.origin_ref, self.upstream_ref);
        let mut commits = git_capture(&["log", "--oneline", "--no-decorate", &range]).1;
        if commits.is_empty() {
            commits = "(origin and upstream diverged; merge strategy required)".to_string();
        }
        let diff_stat = git_capture(&["diff", "--stat", &range]).1;
        let diff_names = git_capture(&["diff", "--name-status", &range]).1;
        let diff_files = git_capture(&["diff", "--name-only", &range]).1;
        for (title, body) in [
            ("Pending Upstream Commits", &commits),
            ("Diff Stat", &diff_stat),
            ("Files", &diff_names),
        ] {
            self.report
                .write(&format!("## {title}\n```\n{body}\n```\n\n"), true)?;
        }
        let risk_matches = self.risk_gate(&diff_files)?;
        if c.dry_run {
            self.report.line("status: dry-run")?;
            log("Dry run complete.");
            log(&format!("Report: {}", self.report.path));
            return Ok(0);
        }

        log(&format!(
            "Checking out {} and updating from {}...",
            c.base_branch, c.origin
        ));
        git(&["checkout", &c.base_branch])?;
        git(&["pull", "--ff-only", &c.origin, &c.base_branch])?;
        log(&format!("Creating sync branch {}...", self.sync_branch));
        git(&["checkout", "-b", &self.sync_branch])?;
        if c.strategy == "merge" {
            log(&format!(
                "Merging {} into {}...",
                self.upstream_ref, self.sync_branch
            ));
            if git(&["merge", "--no-edit", &self.upstream_ref]).is_err() {
                return Err(self.conflict("merge conflict", "")?);
            }
        } else {
            let shas = git_capture(&["rev-list", "--reverse", &range]).1;
            let shas: Vec<&str> = shas.split('\n').filter(|s| !s.is_empty()).collect();
            if shas.is_empty() {
                return Err(die(
                    "No linear commits to cherry-pick; rerun with --strategy merge.",
                ));
            }
            for sha in shas {
                log(&format!("Cherry-picking {sha}..."));
                if git(&["cherry-pick", sha]).is_err() {
                    return Err(self.conflict("cherry-pick conflict", sha)?);
                }
            }
        }
        self.gates()?;

        log(&format!("Pushing {} to {}...", self.sync_branch, c.origin));
        git(&["push", "-u", &c.origin, &self.sync_branch])?;
        if c.mode == "direct-main" {
            log(&format!(
                "Fast-forwarding {} to {} and pushing...",
                c.base_branch, self.sync_branch
            ));
            git(&["checkout", &c.base_branch])?;
            git(&["merge", "--ff-only", &self.sync_branch])?;
            git(&["push", &c.origin, &c.base_branch])?;
            self.report.line("status: synced-direct-main")?;
            log("Direct-main sync complete.");
            log(&format!("Report: {}", self.report.path));
            return Ok(0);
        }
        if c.create_pr {
            self.open_pr(&commits, risk_matches)?;
        }
        self.report.line("status: synced-branch-pr")?;
        self.report
            .line(&format!("sync_branch: {}", self.sync_branch))?;
        log(&format!("Sync branch prepared: {}", self.sync_branch));
        log(&format!("Report: {}", self.report.path));
        Ok(0)
    }

    fn header(&self) -> Step<()> {
        let c = &self.c;
        let fields = [
            ("timestamp_utc", self.timestamp.as_str()),
            ("repo_root", &c.repo_root),
            ("mode", &c.mode),
            ("strategy", &c.strategy),
            ("strict_risk_gate", &c.strict_risk_gate),
            ("run_redteam_gate", &c.run_redteam_gate),
            ("redteam_cmd", &c.redteam_cmd),
            ("run_elite_gate", &c.run_elite_gate),
            ("elite_cmd", &c.elite_cmd),
            ("elite_rollback_cmd", &c.elite_rollback_cmd),
            ("draft_pr", flag(c.pr_draft)),
            ("allow_risk_paths", flag(c.allow_risk_paths)),
            ("pr_labels", &c.pr_labels),
            ("risk_paths_file", &c.risk_paths_file),
            ("upstream_url", &self.upstream_url),
            ("expected_upstream_repo", &self.expected_upstream),
            ("allow_non_official_upstream", &self.allow_non_official),
            ("origin_ref", &self.origin_ref),
            ("upstream_ref", &self.upstream_ref),
            ("origin_sha", &self.origin_sha),
            ("upstream_sha", &self.upstream_sha),
        ];
        let mut text = String::from("# Upstream Sync Report\n\n");
        for (key, value) in fields {
            text.push_str(&format!("{key}: {value}\n"));
        }
        text.push('\n');
        self.report.write(&text, false)
    }

    /// The strict risk gate: the number of pattern matches when the run may
    /// go on, or the exit code when it stops.
    fn risk_gate(&self, diff_files: &str) -> Step<usize> {
        let c = &self.c;
        if c.strict_risk_gate != "1" {
            return Ok(0);
        }
        if !Path::new(&c.risk_paths_file).is_file() {
            return Err(die(&format!(
                "Strict risk gate enabled but pattern file not found: {}",
                c.risk_paths_file
            )));
        }
        let changed: Vec<&str> = diff_files
            .split('\n')
            .filter(|l| !l.trim().is_empty())
            .collect();
        if changed.is_empty() {
            return Ok(0);
        }
        let patterns = match fs::read(&c.risk_paths_file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                eprintln!("{}: {e}", c.risk_paths_file);
                return Err(1);
            }
        };
        let mut matches = Vec::new();
        for pattern in patterns.lines() {
            if pattern.is_empty() || pattern.trim_start().starts_with('#') {
                continue;
            }
            for path in &changed {
                if pattern_matches(pattern, path) {
                    matches.push(format!("{pattern} -> {path}"));
                }
            }
        }
        if matches.is_empty() {
            self.report.line("risk_gate_status: pass")?;
            return Ok(0);
        }
        self.report.write(
            &format!(
                "## Strict Risk Gate Matches\n```\n{}\n```\n\n",
                matches.join("\n")
            ),
            true,
        )?;
        if c.allow_risk_paths {
            self.report.line("risk_gate_status: bypassed")?;
            self.report.line("risk_gate_bypass: --allow-risk-paths")?;
            log("Strict risk gate matched paths, but bypass is enabled for this run.");
            return Ok(matches.len());
        }
        self.report.line("risk_gate_status: blocked")?;
        if c.dry_run {
            self.report
                .line("risk_gate_status_detail: blocked_in_dry_run")?;
            log("Strict risk gate would block sync (dry-run).");
            return Err(1);
        }
        if c.create_conflict_issue {
            let body = format!(
                "Automated upstream sync was blocked by strict risk gating.\n\n- mode: `{}`\n- strategy: `{}`\n- strict risk gate: `{}`\n- allow risk paths: `{}`\n- risk file: `{}`\n- report: `{}`\n\nReview the matched files and rerun with explicit approval if intended.\n",
                c.mode,
                c.strategy,
                c.strict_risk_gate,
                flag(c.allow_risk_paths),
                c.risk_paths_file,
                self.report.path
            );
            self.issue(
                "EAB308",
                "Automated upstream sync risk gate",
                &format!(
                    "upstream sync blocked by strict risk gate ({}, {})",
                    c.base_branch, self.timestamp
                ),
                &body,
                &format!(
                    "Strict-risk issue created with label '{}'.",
                    c.conflict_label
                ),
                &format!(
                    "Failed to create strict-risk issue automatically; review report {}.",
                    self.report.path
                ),
            )?;
        }
        Err(die(&format!(
            "Strict risk gate blocked sync due to high-risk path changes. Review {}.",
            self.report.path
        )))
    }

    fn issue(
        &self,
        color: &str,
        description: &str,
        title: &str,
        body: &str,
        created: &str,
        failed: &str,
    ) -> Step<()> {
        let label = &self.c.conflict_label;
        gh_quiet(&[
            "label",
            "create",
            label,
            "--color",
            color,
            "--description",
            description,
        ]);
        let body = TempFile::new(body)?;
        let ok = gh_quiet(&[
            "issue",
            "create",
            "--title",
            title,
            "--label",
            label,
            "--body-file",
            &body.path(),
        ]);
        log(if ok { created } else { failed });
        Ok(())
    }

    /// Abort the merge or cherry-pick, write the conflict report, open the
    /// conflict issue and return the exit code.
    fn conflict(&self, reason: &str, failed_commit: &str) -> Step<i32> {
        let c = &self.c;
        let path = format!(
            "{}/upstream-sync-{}-conflict.txt",
            c.report_dir, self.timestamp
        );
        let conflicted = git_capture(&["diff", "--name-only", "--diff-filter=U"]).1;
        let cherry = c.strategy == "cherry-pick";
        if cherry {
            git_quiet(&["tag", "-f", &self.rollback_tag, "HEAD"]);
            git_quiet(&["cherry-pick", "--abort"]);
        } else {
            git_quiet(&["merge", "--abort"]);
        }
        let mut text = format!(
            "# Upstream Sync Conflict\n\ntimestamp_utc: {}\nmode: {}\nstrategy: {}\nreason: {reason}\n",
            self.timestamp, c.mode, c.strategy
        );
        if !failed_commit.is_empty() {
            text.push_str(&format!("failed_commit: {failed_commit}\n"));
        }
        text.push_str(&format!(
            "origin_sha: {}\nupstream_sha: {}\n",
            self.origin_sha, self.upstream_sha
        ));
        if cherry {
            text.push_str(&format!(
                "rollback_tag: {tag}\nrollback_hint: git checkout {} && git reset --hard {tag}\n",
                self.sync_branch,
                tag = self.rollback_tag
            ));
        }
        text.push_str(&format!("\n## Conflicted files\n```\n{conflicted}\n```\n"));
        Report { path: path.clone() }.write(&text, false)?;
        self.report.line("status: conflict")?;
        self.report.line(&format!("conflict_reason: {reason}"))?;
        self.report.line(&format!("conflict_report: {path}"))?;
        if cherry {
            self.report
                .line(&format!("rollback_tag: {}", self.rollback_tag))?;
        }
        if c.create_conflict_issue {
            let mut body = format!(
                "Automated upstream sync hit a conflict.\n\n- mode: `{}`\n- strategy: `{}`\n- reason: `{reason}`\n",
                c.mode, c.strategy
            );
            if !failed_commit.is_empty() {
                body.push_str(&format!("- failed commit: `{failed_commit}`\n"));
            }
            if cherry {
                body.push_str(&format!("- rollback tag: `{}`\n", self.rollback_tag));
            }
            body.push_str(&format!(
                "- report: `{path}`\n\nSee local report for conflicted files and recovery commands.\n"
            ));
            self.issue(
                "E11D48",
                "Automated upstream sync conflict",
                &format!(
                    "upstream sync conflict ({}, {})",
                    c.base_branch, self.timestamp
                ),
                &body,
                &format!("Conflict issue created with label '{}'.", c.conflict_label),
                &format!("Failed to create conflict issue automatically; review report {path}."),
            )?;
        }
        Ok(die(&format!("{reason}. See {path}")))
    }

    /// Verification, fork carry contracts, red-team and elite gates, then
    /// the generated doc refresh.
    fn gates(&self) -> Step<()> {
        let c = &self.c;
        if c.run_tests {
            log(&format!("Running verification: {}", c.test_cmd));
            status_of("bash", &["-lc", &c.test_cmd])?;
            log("Verifying fork carry contracts");
            if status_of("cargo", &FORK_CARRY).is_err() {
                self.report.line("status: fork-carry-failed")?;
                return Err(die("Required fork carry contracts did not pass"));
            }
            self.report.line("fork_carry: required-contracts-passed")?;
        } else {
            self.report.line("fork_carry: skipped")?;
        }
        if c.run_redteam_gate == "1" {
            log(&format!("Running red-team gate: {}", c.redteam_cmd));
            let output = self.gate(
                &c.redteam_cmd,
                "redteam-gate-failed",
                "redteam_output",
                "Red-team gate failed",
            )?;
            if let Some(path) = report_path(&output, "[redteam-gate] Report: ") {
                self.report.line(&format!("redteam_report: {path}"))?;
            }
        }
        if c.run_elite_gate == "1" {
            let mut command = c.elite_cmd.clone();
            if !c.elite_rollback_cmd.is_empty() {
                command = format!(
                    "{command} --rollback-cmd {}",
                    shell_quote(&c.elite_rollback_cmd)
                );
            }
            log(&format!("Running elite gate: {command}"));
            let output = self.gate(
                &command,
                "elite-gate-failed",
                "elite_output",
                "Elite gate failed",
            )?;
            if let Some(path) = report_path(&output, "[elite-sync-gate] Report: ") {
                self.report.line(&format!("elite_report: {path}"))?;
            }
        }
        if c.run_doc_refresh == "1" {
            log("Refreshing generated parity/readme docs...");
            self.gate(
                &c.doc_refresh_cmd,
                "doc-refresh-failed",
                "doc_refresh_output",
                "Generated doc refresh failed",
            )?;
            let docs = ["README.md", "docs/parity/PARITY_DASHBOARD.md"];
            let changed = git_capture(&["status", "--porcelain", "--", docs[0], docs[1]]).1;
            if !changed.is_empty() {
                git(&["add", docs[0], docs[1]])?;
                git(&[
                    "commit",
                    "-m",
                    "chore(sync): refresh parity dashboard and README sync status",
                ])?;
                let head = git_capture(&["rev-parse", "HEAD"]).1;
                self.report.line(&format!("doc_refresh_commit: {head}"))?;
            }
        }
        Ok(())
    }

    /// Run one gate command, print its joined output and return it; a
    /// failure records the output in the report and stops the run.
    fn gate(&self, command: &str, status: &str, key: &str, failure: &str) -> Step<String> {
        let (ok, output) = bash_capture(command);
        if !ok {
            self.report.line(&format!("status: {status}"))?;
            self.report
                .line(&format!("{key}: {}", output.replace('\n', "\\n")))?;
            println!("{output}");
            return Err(die(failure));
        }
        println!("{output}");
        Ok(output)
    }

    fn open_pr(&self, commits: &str, risk_matches: usize) -> Step<()> {
        let c = &self.c;
        let summary = queue_summary(Path::new("docs/parity/upstream-missing-queue.json"));
        let drift = [
            "docs/parity/upstream-missing-queue.json",
            "docs/parity/upstream-missing-queue.md",
            "docs/parity/global-parity-proof.json",
            &self.report.path,
        ]
        .iter()
        .map(|p| format!("- `{p}`"))
        .collect::<Vec<_>>()
        .join("\n");
        let body = format!(
            "Automated upstream sync.

- strategy: `{strategy}`
- upstream ref: `{upstream_ref}`
- upstream sha: `{upstream_sha}`
- origin sha before sync: `{origin_sha}`
- verification: `{test_cmd}`
- report: `{report}`
- parity queue summary: `{summary}`

Drift artifacts:
{drift}

Test guidance:
- Run `cargo test -p hermes-gateway` (or the configured `--test-cmd`) at minimum.
- Run `cargo run --locked -q -p hermes-parity-tools -- check-fork-carry` to verify the required fork behaviour contracts.
- If parity drift is detected, regenerate queue/proof artifacts before merge.

Commits pending from upstream at sync start:
```
{commits}
```
",
            strategy = c.strategy,
            upstream_ref = self.upstream_ref,
            upstream_sha = self.upstream_sha,
            origin_sha = self.origin_sha,
            test_cmd = c.test_cmd,
            report = self.report.path,
        );
        let body = TempFile::new(&body)?;
        let body_path = body.path();
        let title = format!(
            "chore: sync upstream {} ({})",
            c.base_branch, self.timestamp
        );
        let mut args = vec![
            "pr",
            "create",
            "--base",
            &c.base_branch,
            "--head",
            &self.sync_branch,
            "--title",
            &title,
            "--body-file",
            &body_path,
        ];
        if c.pr_draft {
            args.push("--draft");
        }
        let created = Command::new("gh")
            .args(&args)
            .stderr(Stdio::inherit())
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| strip_newlines(&String::from_utf8_lossy(&out.stdout)));
        let Some(url) = created else {
            log("PR creation failed (branch was pushed). Create PR manually if needed.");
            return Ok(());
        };
        log(&format!("PR created successfully: {url}"));
        let mut labels = pr_labels(&c.pr_labels);
        if risk_matches > 0 {
            labels.push("risk-paths-reviewed".to_string());
        }
        for label in &labels {
            gh_quiet(&[
                "label",
                "create",
                label,
                "--color",
                "1D76DB",
                "--description",
                "Automated upstream sync metadata",
            ]);
            gh_quiet(&["pr", "edit", &url, "--add-label", label]);
        }
        Ok(())
    }
}

/// The path from the last line holding `marker`, as the script's `awk` and
/// `${line#*Report: }` took it.
fn report_path(output: &str, marker: &str) -> Option<String> {
    let line = output.lines().rfind(|l| l.contains(marker))?;
    Some(
        line.split_once("Report: ")
            .map_or(line, |(_, rest)| rest)
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_words_as_printf_q() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("plain-word_1.2/x"), "plain-word_1.2/x");
        assert_eq!(
            shell_quote("git reset --hard 'HEAD~1' && echo \"$x\" #~a,b"),
            "git\\ reset\\ --hard\\ \\'HEAD~1\\'\\ \\&\\&\\ echo\\ \\\"\\$x\\\"\\ #~a\\,b"
        );
        assert_eq!(shell_quote("#a=~b:~c"), "\\#a=\\~b:\\~c");
        assert_eq!(shell_quote("a\nb'\\\x01"), "$'a\\nb\\'\\\\\\001'");
    }

    #[test]
    fn matches_bash_patterns() {
        for (pattern, path, want) in [
            (
                "crates/hermes-auth/**",
                "crates/hermes-auth/src/a/b.rs",
                true,
            ),
            (
                "crates/hermes-agent/src/memory_plugins/*.rs",
                "crates/hermes-agent/src/memory_plugins/x/y.rs",
                true,
            ),
            ("Cargo.toml", "crates/Cargo.toml", false),
            ("scripts/setup-*.sh", "scripts/setup-x.sh", true),
            ("docs/[nN]otes.md", "docs/Notes.md", true),
            ("docs/[!n]otes.md", "docs/notes.md", false),
            ("docs/[^n]otes.md", "docs/Notes.md", true),
            ("a[]]b", "a]b", true),
            ("a[x-z]", "ay", true),
            ("a[[:digit:]]", "a7", true),
            ("a[", "a[", true),
            ("a\\*", "a*", true),
            ("a\\*", "ab", false),
            ("a?c", "a/c", true),
            ("a.c", "abc", false),
            ("@(x|y).rs", "y.rs", true),
            ("+(ab).rs", "abab.rs", true),
            ("?(ab)c", "c", true),
            ("*(a|b)c", "abbac", true),
            ("a(b", "a(b", true),
        ] {
            assert_eq!(pattern_matches(pattern, path), want, "{pattern} vs {path}");
        }
    }

    #[test]
    fn summarises_the_queue_as_python_did() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("q.json");
        let summary = |text: &str| {
            fs::write(&path, text).expect("write");
            queue_summary(&path)
        };
        assert_eq!(
            summary(
                r#"[{"state":"queued","classification":"Actionable"},{"status":"OPEN"},{"state":0,"status":"todo"},{"state":1,"status":"todo"},"x",{"class":"required"}]"#
            ),
            "total=6, queued=3, actionable=2"
        );
        assert_eq!(
            summary(r#"{"items":[],"entries":[{"state":"in_progress"}]}"#),
            "total=1, queued=1, actionable=0"
        );
        assert_eq!(
            summary(r#"{"items":{"a":1,"b":2}}"#),
            "total=2, queued=0, actionable=0"
        );
        assert_eq!(
            summary(r#"{"items":"abc"}"#),
            "total=3, queued=0, actionable=0"
        );
        assert_eq!(summary(r#"{"items":5}"#), "unavailable");
        assert_eq!(summary("7"), "total=0, queued=0, actionable=0");
        assert_eq!(summary("{bad"), "unavailable");
        assert_eq!(
            queue_summary(&dir.path().join("missing.json")),
            "unavailable"
        );
    }

    #[test]
    fn splits_labels_as_read_and_xargs_did() {
        assert_eq!(pr_labels(" a , ,b  c,"), ["a", "b c"]);
        assert_eq!(pr_labels("x\ny"), ["x"]);
        assert!(pr_labels("").is_empty());
    }

    #[test]
    fn takes_the_last_report_line() {
        let out = "x\n[elite-sync-gate] Report: /a\n[elite-sync-gate] Report: /b Report: c\n";
        assert_eq!(
            report_path(out, "[elite-sync-gate] Report: ").as_deref(),
            Some("/b Report: c")
        );
        assert_eq!(report_path("none", "[redteam-gate] Report: "), None);
    }
}
