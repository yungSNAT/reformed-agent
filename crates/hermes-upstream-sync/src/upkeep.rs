//! The per-commit upkeep issue queue (`maybe_create_upkeep_commit_issues`
//! in the script): one GitHub issue per new upstream commit of a push, with
//! a state file so a commit gets its issue once.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use hermes_py::Py;

use crate::event::SyncEvent;
use crate::worker::run_child;
use crate::{log, Level};

pub struct UpkeepConfig {
    pub enabled: bool,
    pub labels: Vec<String>,
    pub parent_issue: i64,
    pub state_path: PathBuf,
    pub max_commits_per_event: i64,
    pub allow_python_test_surfaces: bool,
}

const GH_TIMEOUT: Duration = Duration::from_secs(600);

/// True when every path is under `tests/` or `test/` (after Python's
/// `strip().lstrip("./")`); an empty list is false. Such upstream commits
/// change only Python tests, which the Rust port does not carry.
pub fn commit_is_python_test_only(paths: &[&str]) -> bool {
    !paths.is_empty()
        && paths.iter().all(|p| {
            let p = hermes_py::strip(p).trim_start_matches(['.', '/']);
            p.starts_with("tests/") || p.starts_with("test/")
        })
}

pub fn classify_commit_workstream(paths: &[&str]) -> &'static str {
    const CHECKS: [(&str, &str); 10] = [
        ("crates/hermes-agent/", "WS2 core runtime parity"),
        ("crates/hermes-gateway/", "WS3 tools/adapters parity"),
        ("crates/hermes-tools/", "WS3 tools/adapters parity"),
        ("crates/hermes-cron/", "WS3 tools/adapters parity"),
        ("crates/hermes-cli/src/tui.rs", "WS5 UX parity"),
        ("crates/hermes-cli/", "WS5 UX parity"),
        ("skills/", "WS4 skills parity"),
        ("optional-skills/", "WS4 skills parity"),
        (".github/workflows/", "WS6 tests/CI parity"),
        ("scripts/", "WS7 security/store/upkeep parity"),
    ];
    CHECKS
        .iter()
        .find(|(prefix, _)| paths.iter().any(|p| p.starts_with(prefix)))
        .map_or("WS8 compatibility/divergence policy", |(_, ws)| ws)
}

/// `run_git(..., check=False)`: success and stripped stdout.
fn git(repo_root: &Path, args: &[&str]) -> (bool, String) {
    let mut command = Command::new("git");
    command.args(args).current_dir(repo_root);
    match run_child(command, GH_TIMEOUT) {
        Ok(run) => (
            run.code == Some(0),
            hermes_py::strip(&run.stdout).to_string(),
        ),
        Err(_) => (false, String::new()),
    }
}

pub fn git_ref_exists(repo_root: &Path, rev: &str) -> bool {
    git(repo_root, &["rev-parse", "--verify", rev]).0
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(hermes_py::strip)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The commits a push brought: `before..after` when both exist, else the
/// last commits up to `after`.
pub fn discover_event_commits(
    repo_root: &Path,
    event: &SyncEvent,
    max_commits: i64,
) -> Vec<String> {
    let before = hermes_py::strip(&event.before_sha);
    let after = hermes_py::strip(&event.after_sha);
    if after.is_empty() {
        return Vec::new();
    }
    if !before.is_empty()
        && before != "0".repeat(40)
        && git_ref_exists(repo_root, before)
        && git_ref_exists(repo_root, after)
    {
        let (ok, out) = git(
            repo_root,
            &["rev-list", "--reverse", &format!("{before}..{after}")],
        );
        if ok {
            let mut commits = lines(&out);
            if max_commits > 0 {
                let keep = commits.len().saturating_sub(max_commits as usize);
                commits.drain(..keep);
            }
            return commits;
        }
    }
    if git_ref_exists(repo_root, after) {
        let max = max_commits.max(1).to_string();
        let (ok, out) = git(
            repo_root,
            &["rev-list", "--reverse", "--max-count", &max, after],
        );
        if ok {
            return lines(&out);
        }
    }
    Vec::new()
}

/// `shutil.which(name) is not None`.
pub fn on_path(name: &str) -> bool {
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(&exe).is_file()))
        .unwrap_or(false)
}

/// `gh issue create`; the issue URL from the last stdout line.
pub fn gh_issue_create(
    repo_root: &Path,
    title: &str,
    body: &str,
    labels: &[String],
) -> Option<String> {
    if !on_path("gh") {
        return None;
    }
    let mut command = Command::new("gh");
    command
        .args(["issue", "create", "--title", title, "--body", body])
        .current_dir(repo_root);
    for label in labels
        .iter()
        .map(|l| hermes_py::strip(l))
        .filter(|l| !l.is_empty())
    {
        command.args(["--label", label]);
    }
    let run = run_child(command, GH_TIMEOUT).ok()?;
    if run.code != Some(0) {
        log(
            Level::Warning,
            &format!("gh issue create failed: {}", hermes_py::strip(&run.stderr)),
        );
        return None;
    }
    lines(&run.stdout).pop()
}

pub fn gh_issue_comment(repo_root: &Path, issue: i64, body: &str) -> bool {
    if !on_path("gh") {
        return false;
    }
    let mut command = Command::new("gh");
    command
        .args(["issue", "comment", &issue.to_string(), "--body", body])
        .current_dir(repo_root);
    match run_child(command, GH_TIMEOUT) {
        Ok(run) if run.code == Some(0) => true,
        Ok(run) => {
            log(
                Level::Warning,
                &format!("gh issue comment failed: {}", hermes_py::strip(&run.stderr)),
            );
            false
        }
        Err(_) => false,
    }
}

/// `{"seen_commits": {...}, ...}` from the state file, or a fresh one when
/// it is missing or not that shape.
fn load_state(path: &Path) -> Py {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| hermes_py::loads(&text).ok())
        .filter(|state| state.get("seen_commits").is_some_and(Py::is_dict))
        .unwrap_or_else(|| {
            Py::dict([
                ("seen_commits", Py::Dict(Vec::new())),
                ("updated_at", Py::from(hermes_py::utc_isoformat())),
            ])
        })
}

fn write_json(path: &Path, value: &Py) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, hermes_py::dumps(value, Some(2), true))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Opens an issue for each new commit of the push and returns how many it
/// opened. Writes the state file and `upkeep-commit-queue-<delivery>.json`.
pub fn create_commit_issues(
    repo_root: &Path,
    report_dir: &Path,
    event: &SyncEvent,
    config: &UpkeepConfig,
) -> usize {
    if !config.enabled || !on_path("gh") {
        return 0;
    }
    let commits = discover_event_commits(repo_root, event, config.max_commits_per_event);
    let mut state = load_state(&config.state_path);
    let mut seen = match state.get("seen_commits") {
        Some(Py::Dict(pairs)) => pairs.clone(),
        _ => Vec::new(),
    };
    let (mut created, mut skipped_seen, mut skipped_rust_only) = (Vec::new(), 0usize, 0usize);
    for sha in &commits {
        if seen.iter().any(|(k, _)| k == sha) {
            skipped_seen += 1;
            continue;
        }
        let (ok, out) = git(
            repo_root,
            &["show", "-s", "--format=%H%x1f%an%x1f%aI%x1f%s", sha],
        );
        let fields: Vec<&str> = out.split('\u{1f}').collect();
        let [commit_sha, author, authored_at, subject] = fields[..] else {
            continue;
        };
        if !ok || out.is_empty() {
            continue;
        }
        let (_, names) = git(
            repo_root,
            &["show", "--pretty=format:", "--name-only", commit_sha],
        );
        let paths = lines(&names);
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let entry = |pairs: Vec<(&str, Py)>| (commit_sha.to_string(), Py::dict(pairs));
        if commit_is_python_test_only(&paths) && !config.allow_python_test_surfaces {
            seen.push(entry(vec![
                ("issue_url", Py::from("")),
                ("created_at", Py::from(hermes_py::utc_isoformat())),
                ("subject", Py::from(subject)),
                ("workstream", Py::from("superseded-rust-only-python-tests")),
                ("disposition", Py::from("superseded")),
                ("reason", Py::from("rust_only_parity_guard_python_tests")),
            ]));
            skipped_rust_only += 1;
            continue;
        }
        let workstream = classify_commit_workstream(&paths);
        let short: String = commit_sha.chars().take(12).collect();
        let mut title =
            hermes_py::strip(&format!("[UPKEEP][{short}] parity task: {subject}")).to_string();
        if title.chars().count() > 255 {
            title = format!("{}...", title.chars().take(252).collect::<String>());
        }
        let compare = if event.compare_url.is_empty() {
            "<none>"
        } else {
            &event.compare_url
        };
        let mut body = vec![
            "Automated upstream commit parity task.".to_string(),
            String::new(),
            format!("- upstream_sha: `{commit_sha}`"),
            format!("- subject: {subject}"),
            format!("- author: `{author}`"),
            format!("- authored_at: `{authored_at}`"),
            format!("- repository: `{}`", event.repository),
            format!("- ref: `{}`", event.git_ref),
            format!("- compare_url: {compare}"),
            format!("- suggested_workstream: `{workstream}`"),
            String::new(),
            "Touched paths (first 40):".to_string(),
        ];
        body.extend(paths.iter().take(40).map(|p| format!("- `{p}`")));
        body.extend(
            [
                "",
                "Implementation checklist:",
                "- [ ] assess if this is `ported` or `superseded` for Rust-first parity",
                "- [ ] update `docs/parity/upstream-missing-queue.json` with explicit evidence note",
                "- [ ] link resulting PR/commit",
            ]
            .map(str::to_string),
        );
        let Some(issue_url) = gh_issue_create(repo_root, &title, &body.join("\n"), &config.labels)
        else {
            continue;
        };
        seen.push(entry(vec![
            ("issue_url", Py::from(issue_url.as_str())),
            ("created_at", Py::from(hermes_py::utc_isoformat())),
            ("subject", Py::from(subject)),
            ("workstream", Py::from(workstream)),
        ]));
        created.push((
            commit_sha.to_string(),
            issue_url,
            subject.to_string(),
            workstream,
        ));
    }
    state.set("seen_commits", Py::Dict(seen));
    state.set("updated_at", Py::from(hermes_py::utc_isoformat()));
    if let Err(e) = write_json(&config.state_path, &state) {
        log(Level::Warning, &e);
    }
    let artifact_path = report_dir.join(format!("upkeep-commit-queue-{}.json", event.delivery_id));
    let artifact = Py::dict([
        ("generated_at", Py::from(hermes_py::utc_isoformat())),
        ("delivery_id", Py::from(event.delivery_id.as_str())),
        ("repository", Py::from(event.repository.as_str())),
        ("ref", Py::from(event.git_ref.as_str())),
        ("before_sha", Py::from(event.before_sha.as_str())),
        ("after_sha", Py::from(event.after_sha.as_str())),
        (
            "created",
            Py::List(
                created
                    .iter()
                    .map(|(sha, url, subject, ws)| {
                        Py::dict([
                            ("sha", Py::from(sha.as_str())),
                            ("issue_url", Py::from(url.as_str())),
                            ("subject", Py::from(subject.as_str())),
                            ("workstream", Py::from(*ws)),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("created_count", Py::from(created.len())),
        ("skipped_seen_count", Py::from(skipped_seen)),
        (
            "skipped_rust_only_python_tests_count",
            Py::from(skipped_rust_only),
        ),
        ("total_candidates", Py::from(commits.len())),
        (
            "state_path",
            Py::from(config.state_path.display().to_string()),
        ),
    ]);
    if let Err(e) = write_json(&artifact_path, &artifact) {
        log(Level::Warning, &e);
    }
    if config.parent_issue > 0 && !created.is_empty() {
        let mut summary = vec![
            "Automated upstream upkeep commit queue update:".to_string(),
            format!("- delivery_id: `{}`", event.delivery_id),
            format!("- created: `{}`", created.len()),
            format!("- skipped_seen: `{skipped_seen}`"),
            format!("- skipped_rust_only_python_tests: `{skipped_rust_only}`"),
            format!("- artifact: `{}`", artifact_path.display()),
        ];
        summary.extend(
            created
                .iter()
                .take(20)
                .map(|(sha, url, _, _)| format!("- `{}` → {url}", &sha[..sha.len().min(12)])),
        );
        gh_issue_comment(repo_root, config.parent_issue, &summary.join("\n"));
    }
    created.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    // tests/test_upstream_webhook_sync_rust_only_guard.py::test_commit_is_python_test_only
    #[test]
    fn commit_is_python_test_only_as_the_script_decided() {
        assert!(commit_is_python_test_only(&["tests/cli/test_commands.py"]));
        assert!(commit_is_python_test_only(&["test/helpers.py"]));
        assert!(!commit_is_python_test_only(&[
            "tests/cli/test_commands.py",
            "crates/hermes-cli/src/main.rs"
        ]));
        assert!(!commit_is_python_test_only(&[]));
        assert!(commit_is_python_test_only(&[
            " ./tests/a.py",
            "../test/b.py"
        ]));
    }

    #[test]
    fn workstreams_follow_the_first_matching_prefix() {
        assert_eq!(
            classify_commit_workstream(&["README.md", "crates/hermes-cli/src/tui.rs"]),
            "WS5 UX parity"
        );
        assert_eq!(
            classify_commit_workstream(&["scripts/x.sh", "crates/hermes-agent/a.rs"]),
            "WS2 core runtime parity"
        );
        assert_eq!(
            classify_commit_workstream(&["README.md"]),
            "WS8 compatibility/divergence policy"
        );
    }
}
