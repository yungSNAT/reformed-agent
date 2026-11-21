//! Check tracked files for release credentials without retaining matched text.

use std::borrow::Cow;
use std::fs;
use std::process::Command;
use std::sync::LazyLock;

use clap::Args;
use fancy_regex::Regex;

use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_report};

const SKIP_PREFIXES: &[&str] = &[".git/", "target/", "node_modules/", ".sync-reports/"];

const SKIP_LINE_HINTS: &[&str] = &[
    "example",
    "redacted",
    "dummy",
    "placeholder",
    "test-key",
    "fake",
    "xxxx",
    "changeme",
    "detect_secrets(",
    "sk-abc123def456ghi789jkl012mno345",
    "akia1234567890abcdef",
    "abcdefghijklmnopqrstuvwxyzabcd1234",
    "miievg",
    "1234567890:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
];

const SAFE_FIXTURE_SUBSTRINGS: &[&str] = &[
    "tok_super_secret_123456",
    "sk-ant-oat01-abcdef1234567890",
    "sk-ant-api03-abcdef1234567890",
    "sk-ant-oat01-expired-env-token",
    "ghp_abc123def456ghi789jkl",
    "7123456789:aahdqtcvch1vgwjxfseofsas0k5paldsaw",
    "sk-ant-oat-expiredtoken00000",
    "sk-ant-api03-regularpaypertokenkey",
    "sk-ant-oat01-from-claude-setup",
    "ghp_abcdefghijklmnop1234",
    "gho_abcdefghijklmnop1234",
    "ghu_abcdefghijklmnop1234",
    "sk-ant-api-abcdef1234567890",
    "sk-ant-api03-test1234567890",
    "ghp_1234567890abcdef1234",
    "xoxb-1234567890-abcdef",
    "sk-abcdefghijklmnopqrstuvwxyz1234567890",
    "7123456789:aah1bgcioijsuzi1niisinr5cci6ikp",
    "sk-ant-api03-your-second-key",
    "xoxb-your-bot-token-here",
    "xoxb-workspace1-token",
    "xoxb-workspace2-token",
    "xoxb-workspace3-token",
    "xoxb-workspace-token-here",
    "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
];

const PATTERNS: &[(&str, &str)] = &[
    ("openai_key", r"\bsk-[A-Za-z0-9]{20,}\b"),
    ("anthropic_key", r"\bsk-ant-[A-Za-z0-9\-_]{20,}\b"),
    ("github_pat", r"\bgh[pousr]_[A-Za-z0-9]{20,}\b"),
    ("telegram_bot_token", r"\b\d{8,10}:[A-Za-z0-9_-]{30,}\b"),
    ("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b"),
    ("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{16,}\b"),
    (
        "private_key_block",
        r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
    ),
];

const REDACTED: &str = "[redacted]";

// Python's word character set is alphanumeric characters and underscore.
// The regex crate's default Unicode word class also includes combining marks.
const PYTHON_WORD: &str = r"[\p{L}\p{N}_]";

static DETECTORS: LazyLock<Result<Vec<(&'static str, Regex)>, String>> = LazyLock::new(|| {
    let boundary =
        format!(r"(?:(?<!{PYTHON_WORD})(?={PYTHON_WORD})|(?<={PYTHON_WORD})(?!{PYTHON_WORD}))");
    PATTERNS
        .iter()
        .map(|&(name, pattern)| {
            Regex::new(&pattern.replace(r"\b", &boundary))
                .map(|regex| (name, regex))
                .map_err(|error| format!("cannot compile release scanner detector {name}: {error}"))
        })
        .collect()
});

#[derive(Args, Debug)]
#[command(args_override_self = true, infer_long_args = true)]
pub struct ScanArgs {
    /// Repository root.
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Optional JSON report output path.
    #[arg(long, default_value = "")]
    report: String,
}

#[derive(Debug, PartialEq, Eq)]
struct Finding<'a> {
    file: &'a str,
    line: usize,
    detector: &'a str,
}

impl Finding<'_> {
    fn report_entry(&self) -> Py {
        Py::dict([
            ("file", Py::Str(self.file.to_string())),
            ("line", Py::Int(self.line as i128)),
            ("pattern", Py::Str(self.detector.to_string())),
            ("snippet", Py::Str(REDACTED.to_string())),
        ])
    }
}

fn utf8_ignoring_errors(bytes: &[u8]) -> Cow<'_, str> {
    if let Ok(valid) = std::str::from_utf8(bytes) {
        Cow::Borrowed(valid)
    } else {
        Cow::Owned(bytes.utf8_chunks().map(|chunk| chunk.valid()).collect())
    }
}

fn should_skip_file(relative: &str) -> bool {
    let normalized = relative.replace('\\', "/");
    SKIP_PREFIXES
        .iter()
        .any(|&prefix| normalized.starts_with(prefix))
        || [".png", ".jpg", ".jpeg", ".gif", ".pdf", ".ico", ".wasm"]
            .iter()
            .any(|&suffix| normalized.ends_with(suffix))
}

fn file_findings<'a>(
    relative: &'a str,
    contents: &str,
    detectors: &[(&'a str, Regex)],
    findings: &mut Vec<Finding<'a>>,
) -> Result<(), String> {
    let contents = if contents.contains("\r\n") {
        Cow::Owned(contents.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(contents)
    };
    for (line, text) in contents
        .split_terminator([
            '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
        ])
        .enumerate()
    {
        let folded = text.to_lowercase();
        if SKIP_LINE_HINTS
            .iter()
            .chain(SAFE_FIXTURE_SUBSTRINGS)
            .any(|&hint| folded.contains(hint))
        {
            continue;
        }
        for (name, pattern) in detectors {
            if pattern
                .is_match(text)
                .map_err(|_| format!("release scanner detector {name} failed"))?
            {
                findings.push(Finding {
                    file: relative,
                    line: line + 1,
                    detector: name,
                });
            }
        }
    }
    Ok(())
}

pub fn run(args: ScanArgs) -> Result<i32, String> {
    let root = resolve_argument(&py::expanduser(&args.repo_root)?)?;
    let listing = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(&root)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !listing.status.success() {
        return Err("git ls-files failed; run from a git repository root".to_string());
    }
    let paths = utf8_ignoring_errors(&listing.stdout);
    let detectors = DETECTORS.as_ref().map_err(Clone::clone)?;
    let mut findings = Vec::new();
    for relative in paths.split('\0').filter(|path| !path.is_empty()) {
        let file = root.join(relative);
        if should_skip_file(relative) || !file.is_file() {
            continue;
        }
        let Ok(bytes) = fs::read(file) else {
            continue;
        };
        if !bytes.contains(&0) {
            file_findings(
                relative,
                &utf8_ignoring_errors(&bytes),
                detectors,
                &mut findings,
            )?;
        }
    }

    if !args.report.is_empty() {
        let path = py::join(&root, &args.report);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let report = Py::dict([
            ("repo_root", Py::Str(py::path_str(&root))),
            ("total_findings", Py::Int(findings.len() as i128)),
            (
                "findings",
                Py::List(findings.iter().map(Finding::report_entry).collect()),
            ),
        ]);
        write_report(&path, py::dumps(&report, Some(2), false) + "\n")?;
    }
    if findings.is_empty() {
        println!("[release-secret-scan] OK: no tracked secret patterns detected");
        return Ok(0);
    }
    println!("[release-secret-scan] FAILED: potential secrets detected");
    for finding in findings.iter().take(50) {
        eprintln!(
            "- {}:{} [{}] {REDACTED}",
            finding.file, finding.line, finding.detector
        );
    }
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_keep_locations_and_never_keep_the_matched_text() {
        let detectors = [("fixture", Regex::new("marker").unwrap())];
        let mut findings = Vec::new();
        file_findings(
            "notes.txt",
            "plain\r\nmarker\u{2028}plain\rmarker",
            &detectors,
            &mut findings,
        )
        .unwrap();
        assert_eq!(
            findings
                .iter()
                .map(|finding| finding.line)
                .collect::<Vec<_>>(),
            [2, 4]
        );
        assert_eq!(
            findings[0].report_entry().get("snippet"),
            Some(&Py::Str(REDACTED.to_string()))
        );
    }

    #[test]
    fn paths_and_source_ignore_invalid_utf8() {
        assert_eq!(utf8_ignoring_errors(b"a\xffb\xe2\x82"), "ab");
        assert!(DETECTORS.as_ref().is_ok());
    }
}
