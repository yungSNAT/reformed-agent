//! `/diff`: git changes in the working directory (upstream
//! `tools/working_diff.py` and the CLI's `_handle_diff_command`).
//!
//! Git runs through [`hermes_agent::coding_context::read_only_git`], so the
//! repository's own configuration cannot make it run a program, and with
//! `--no-ext-diff --no-textconv`, so no diff or textconv driver runs either.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::{emit_command_output, AgentError, App, CommandResult};

/// `/diff [staged|all] [--stat] [path...]` for the working directory. Git
/// runs on the blocking pool, as one call may take up to [`GIT_TIMEOUT`].
pub(super) async fn handle_diff_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let request = match parse_diff_args(args) {
        Ok(request) => request,
        Err(usage) => {
            emit_command_output(app, usage);
            return Ok(CommandResult::Handled);
        }
    };
    let text = tokio::task::spawn_blocking(move || {
        let cwd = std::env::current_dir()
            .map_err(|err| format!("Cannot read the working directory: {err}"))?;
        let diff = collect_working_diff(&cwd, &request)?;
        Ok::<_, String>(render_working_diff(&request, &diff))
    })
    .await
    .unwrap_or_else(|err| Err(format!("/diff stopped: {err}")))
    .unwrap_or_else(|reason| reason);
    emit_command_output(app, text);
    Ok(CommandResult::Handled)
}

/// How long one git call may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(15);
/// Untracked files shown as new-file diffs; more are counted, not shown.
const MAX_UNTRACKED_DIFFS: usize = 50;
/// Untracked files listed by name.
const MAX_UNTRACKED_NAMES: usize = 20;
/// Diff lines printed before pointing to `--stat`.
const MAX_DIFF_LINES: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffMode {
    /// Unstaged changes and untracked files.
    Working,
    /// `git diff --cached`.
    Staged,
    /// Everything since HEAD, and untracked files.
    All,
}

impl DiffMode {
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Working => &[],
            Self::Staged => &["--cached"],
            Self::All => &["HEAD"],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Working => "Unstaged",
            Self::Staged => "Staged",
            Self::All => "All (vs HEAD)",
        }
    }

    fn lists_untracked(self) -> bool {
        self != Self::Staged
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DiffRequest {
    pub mode: DiffMode,
    pub stat_only: bool,
    pub paths: Vec<String>,
}

pub(super) const DIFF_USAGE: &str =
    "Usage: /diff [staged|all] [--stat] [path...]\n`/diff session` needs checkpoints, which Reformed does not keep; plain /diff reads git directly.";

/// Reads `/diff` arguments; `Err` holds the message to show.
pub(super) fn parse_diff_args(args: &[&str]) -> Result<DiffRequest, String> {
    let mut request = DiffRequest {
        mode: DiffMode::Working,
        stat_only: false,
        paths: Vec::new(),
    };
    for arg in args {
        match arg.to_ascii_lowercase().as_str() {
            "--stat" | "stat" => request.stat_only = true,
            "staged" | "--staged" | "cached" | "--cached" => request.mode = DiffMode::Staged,
            "all" | "--all" | "head" => request.mode = DiffMode::All,
            "working" => request.mode = DiffMode::Working,
            "session" | "help" | "--help" => return Err(DIFF_USAGE.to_string()),
            _ => request.paths.push((*arg).to_string()),
        }
    }
    Ok(request)
}

/// The diff git gives for `request` in the repository at `cwd`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct WorkingDiff {
    pub stat: String,
    pub diff: String,
    pub untracked: Vec<String>,
}

/// A hardened git command for `cwd` with `args`, run with a time limit.
/// Returns the exit code and standard output.
fn git(cwd: &Path, args: &[&str]) -> Result<(Option<i32>, String), String> {
    let mut cmd: Command = hermes_agent::coding_context::read_only_git(cwd).ok_or_else(|| {
        "git could not be run safely here: it is missing, older than 2.39.1, or this repository's configuration could not be read.".to_string()
    })?;
    cmd.args(["-c", "core.quotePath=false"]).args(args);
    let output = hermes_agent::coding_context::output_within_timeout(&mut cmd, GIT_TIMEOUT)
        .ok_or_else(|| {
            format!(
                "git {} did not finish within {} s.",
                args[0],
                GIT_TIMEOUT.as_secs()
            )
        })?;
    Ok((
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

pub(super) fn collect_working_diff(
    cwd: &Path,
    request: &DiffRequest,
) -> Result<WorkingDiff, String> {
    match git(cwd, &["rev-parse", "--is-inside-work-tree"])? {
        (Some(0), out) if out.trim() == "true" => {}
        _ => return Err("Not a git repository.".to_string()),
    }
    let mut base = vec!["diff", "--no-ext-diff", "--no-textconv"];
    base.extend_from_slice(request.mode.args());
    let pathspec: Vec<&str> = std::iter::once("--")
        .chain(request.paths.iter().map(String::as_str))
        .collect();
    let run_diff = |extra: &[&str]| -> Result<String, String> {
        let args: Vec<&str> = base
            .iter()
            .copied()
            .chain(extra.iter().copied())
            .chain(pathspec.iter().copied())
            .collect();
        match git(cwd, &args)? {
            (Some(0), out) => Ok(out.trim().to_string()),
            (code, _) => Err(format!(
                "git diff failed (exit {}).",
                code.map_or("signal".to_string(), |c| c.to_string())
            )),
        }
    };
    let stat = run_diff(&["--stat"])?;
    let mut diff = run_diff(&[])?;
    let untracked = if request.mode.lists_untracked() && request.paths.is_empty() {
        untracked_files(cwd)?
    } else {
        Vec::new()
    };
    for rel in untracked.iter().take(MAX_UNTRACKED_DIFFS) {
        // `--no-index` exits 1 when the files differ, which is the case here.
        let (_, out) = git(
            cwd,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-index",
                "--",
                "/dev/null",
                rel,
            ],
        )?;
        if !out.trim().is_empty() {
            if !diff.is_empty() {
                diff.push('\n');
            }
            diff.push_str(out.trim_end());
        }
    }
    if untracked.len() > MAX_UNTRACKED_DIFFS {
        diff.push_str(&format!(
            "\n... ({} more untracked files not shown)",
            untracked.len() - MAX_UNTRACKED_DIFFS
        ));
    }
    Ok(WorkingDiff {
        stat,
        diff,
        untracked,
    })
}

fn untracked_files(cwd: &Path) -> Result<Vec<String>, String> {
    match git(cwd, &["ls-files", "--others", "--exclude-standard"])? {
        (Some(0), out) => Ok(out
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect()),
        _ => Ok(Vec::new()),
    }
}

/// The text `/diff` prints for `diff`.
pub(super) fn render_working_diff(request: &DiffRequest, diff: &WorkingDiff) -> String {
    if diff.stat.is_empty() && diff.diff.is_empty() && diff.untracked.is_empty() {
        return "No changes.".to_string();
    }
    let mut out = Vec::new();
    if !diff.stat.is_empty() {
        out.push(format!("{}:", request.mode.label()));
        out.push(diff.stat.clone());
    }
    if !diff.untracked.is_empty() {
        out.push("Untracked:".to_string());
        out.extend(
            diff.untracked
                .iter()
                .take(MAX_UNTRACKED_NAMES)
                .map(|rel| format!("  + {rel}")),
        );
        if diff.untracked.len() > MAX_UNTRACKED_NAMES {
            out.push(format!(
                "  ... and {} more",
                diff.untracked.len() - MAX_UNTRACKED_NAMES
            ));
        }
    }
    if !diff.diff.is_empty() && !request.stat_only {
        let lines: Vec<&str> = diff.diff.lines().collect();
        out.push(String::new());
        out.extend(
            lines
                .iter()
                .take(MAX_DIFF_LINES)
                .map(|line| line.to_string()),
        );
        if lines.len() > MAX_DIFF_LINES {
            out.push(format!(
                "... ({} more lines — run /diff --stat for a summary)",
                lines.len() - MAX_DIFF_LINES
            ));
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"]);
        run(dir.path(), &["config", "user.email", "t@example.com"]);
        run(dir.path(), &["config", "user.name", "t"]);
        run(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        run(dir.path(), &["add", "a.txt"]);
        run(dir.path(), &["commit", "-qm", "init"]);
        dir
    }

    fn request(args: &[&str]) -> DiffRequest {
        parse_diff_args(args).unwrap()
    }

    #[test]
    fn arguments_choose_the_mode_stat_and_paths() {
        assert_eq!(
            request(&["--cached", "--stat", "src/a.rs"]),
            DiffRequest {
                mode: DiffMode::Staged,
                stat_only: true,
                paths: vec!["src/a.rs".to_string()],
            }
        );
        assert_eq!(request(&["HEAD"]).mode, DiffMode::All);
        assert_eq!(request(&[]).mode, DiffMode::Working);
        assert_eq!(parse_diff_args(&["session"]), Err(DIFF_USAGE.to_string()));
    }

    #[test]
    fn working_staged_and_all_follow_git() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();

        let working = collect_working_diff(dir.path(), &request(&[])).unwrap();
        assert!(working.stat.contains("a.txt"), "{working:?}");
        assert!(working.diff.contains("+two"), "{working:?}");
        assert_eq!(working.untracked, vec!["new.txt".to_string()]);
        assert!(working.diff.contains("+fresh"), "{working:?}");

        assert_eq!(
            collect_working_diff(dir.path(), &request(&["staged"])).unwrap(),
            WorkingDiff::default()
        );
        run(dir.path(), &["add", "a.txt"]);
        let staged = collect_working_diff(dir.path(), &request(&["staged"])).unwrap();
        assert!(staged.diff.contains("+two"), "{staged:?}");
        assert!(staged.untracked.is_empty());
        let all = collect_working_diff(dir.path(), &request(&["all"])).unwrap();
        assert!(
            all.diff.contains("+two") && all.diff.contains("+fresh"),
            "{all:?}"
        );
    }

    #[test]
    fn a_path_limits_the_diff_and_skips_untracked_files() {
        let dir = repo();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        run(dir.path(), &["add", "b.txt"]);
        run(dir.path(), &["commit", "-qm", "b"]);
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\nc\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();
        let diff = collect_working_diff(dir.path(), &request(&["b.txt"])).unwrap();
        assert!(
            diff.diff.contains("+c") && !diff.diff.contains("+two"),
            "{diff:?}"
        );
        assert!(diff.untracked.is_empty());
    }

    #[test]
    fn a_clean_tree_reports_no_changes_and_a_folder_outside_git_is_refused() {
        let dir = repo();
        let req = request(&[]);
        let diff = collect_working_diff(dir.path(), &req).unwrap();
        assert_eq!(render_working_diff(&req, &diff), "No changes.");
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            collect_working_diff(plain.path(), &req),
            Err("Not a git repository.".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_repository_cannot_run_a_diff_or_textconv_driver() {
        let dir = repo();
        let marker = dir.path().join("ran");
        let script = dir.path().join("driver.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat \"$1\"\n", marker.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let driver = script.display().to_string();
        run(dir.path(), &["config", "diff.external", &driver]);
        run(dir.path(), &["config", "diff.evil.textconv", &driver]);
        run(dir.path(), &["config", "diff.evil.command", &driver]);
        std::fs::write(dir.path().join(".gitattributes"), "*.txt diff=evil\n").unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();
        let diff = collect_working_diff(dir.path(), &request(&["all"])).unwrap();
        assert!(diff.diff.contains("+two"), "{diff:?}");
        assert!(!marker.exists(), "a repository-configured driver ran");
    }

    #[test]
    fn long_diffs_and_many_untracked_files_are_cut() {
        let req = request(&[]);
        let diff = WorkingDiff {
            stat: " a.txt | 500 +".to_string(),
            diff: (0..450).map(|i| format!("+{i}\n")).collect(),
            untracked: (0..25).map(|i| format!("u{i}")).collect(),
        };
        let text = render_working_diff(&req, &diff);
        assert!(
            text.starts_with("Unstaged:\n a.txt | 500 +\nUntracked:\n  + u0\n"),
            "{text}"
        );
        assert!(text.contains("  + u19\n  ... and 5 more\n"), "{text}");
        assert!(
            text.contains("\n+399\n... (50 more lines — run /diff --stat for a summary)"),
            "{text}"
        );
        assert!(!text.contains("+400"));
        let stat_only = render_working_diff(&request(&["--stat"]), &diff);
        assert!(!stat_only.contains("+0"), "{stat_only}");
    }
}
