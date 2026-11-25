//! `/diff`: git changes in the working directory (upstream
//! `tools/working_diff.py` and the CLI's `_handle_diff_command`).
//!
//! Git runs through [`hermes_core::git::read_only_git`], so the
//! repository's own configuration cannot make it run a program, and with
//! `--no-ext-diff --no-textconv`, so no diff or textconv driver runs either.
//! Submodules are read only for their recorded commit, as the workspace
//! snapshot does: a submodule has its own configuration, so git never looks
//! inside its working tree.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::{emit_command_output, AgentError, App, CommandResult};

/// `/diff [staged|all] [--stat] [path...]` for the working directory. Git
/// runs on the blocking pool, as one call may take up to [`PATCH_TIMEOUT`].
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
        let cwd = diff_cwd(std::env::var_os("TERMINAL_CWD"))?;
        let diff = collect_working_diff(&cwd, &request)?;
        Ok::<_, String>(render_working_diff(&request, &diff))
    })
    .await
    .unwrap_or_else(|err| Err(format!("/diff stopped: {err}")))
    .unwrap_or_else(|reason| reason);
    emit_command_output(app, text);
    Ok(CommandResult::Handled)
}

/// The folder `/diff` reads and `/init` writes AGENTS.md in: `TERMINAL_CWD`
/// when set, as for the terminal tool, else the process's working directory.
pub(super) fn diff_cwd(terminal_cwd: Option<OsString>) -> Result<PathBuf, String> {
    match terminal_cwd.filter(|cwd| !cwd.is_empty()) {
        Some(cwd) => Ok(PathBuf::from(cwd)),
        None => std::env::current_dir()
            .map_err(|err| format!("Cannot read the working directory: {err}")),
    }
}

/// How long `rev-parse` may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the summary, the file list and each untracked file may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the tracked patch may take.
const PATCH_TIMEOUT: Duration = Duration::from_secs(30);
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

/// Reads `/diff` arguments; `Err` holds the message to show. Quoted paths
/// keep their spaces, as upstream's shell-style split does.
///
/// shortcut: the dispatcher has already split the line on whitespace, so the
/// words are rejoined with single spaces before the shell-style split, and a
/// quoted path with 2 spaces in a row reaches git with one. Passing the raw
/// argument text to commands would lift this.
pub(super) fn parse_diff_args(args: &[&str]) -> Result<DiffRequest, String> {
    let words = shlex::split(&args.join(" "))
        .ok_or_else(|| format!("/diff: a quote is not closed.\n{DIFF_USAGE}"))?;
    let mut request = DiffRequest {
        mode: DiffMode::Working,
        stat_only: false,
        paths: Vec::new(),
    };
    for word in words {
        match word.to_ascii_lowercase().as_str() {
            "--stat" | "stat" => request.stat_only = true,
            "staged" | "--staged" | "cached" | "--cached" => request.mode = DiffMode::Staged,
            "all" | "--all" | "head" => request.mode = DiffMode::All,
            "working" => request.mode = DiffMode::Working,
            "session" => return Err(DIFF_USAGE.to_string()),
            _ => request.paths.push(word),
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

/// How a git call ended.
#[derive(Debug)]
enum Git {
    /// Exit code (none for a signal) and standard output.
    Done(Option<i32>, Vec<u8>),
    /// Killed at its time limit.
    TimedOut,
}

/// Runs a hardened git command for `cwd` with `args` within `timeout`.
/// `Err` holds the reason git cannot run safely here.
fn git<S: AsRef<OsStr>>(
    cwd: &Path,
    args: impl IntoIterator<Item = S>,
    timeout: Duration,
) -> Result<Git, String> {
    let mut cmd: Command = hermes_core::git::read_only_git(cwd).ok_or_else(|| {
        "git could not be run safely here: it is missing, older than 2.39.1, or this repository's configuration could not be read or names a user.".to_string()
    })?;
    // `diff.submodule=diff` would start a second diff inside a submodule.
    cmd.args(["-c", "core.quotePath=false", "-c", "diff.submodule=short"])
        .args(args);
    Ok(
        match hermes_core::git::output_within_timeout(&mut cmd, timeout) {
            Some(output) => Git::Done(output.status.code(), output.stdout),
            None => Git::TimedOut,
        },
    )
}

/// An untracked file: its name as git printed it, for display, and as the
/// exact bytes, for naming it to git and the filesystem. `path` is `None`
/// where the platform cannot hold those bytes in a path.
#[derive(Debug)]
struct Untracked {
    shown: String,
    path: Option<OsString>,
}

pub(super) fn collect_working_diff(
    cwd: &Path,
    request: &DiffRequest,
) -> Result<WorkingDiff, String> {
    match git(cwd, ["rev-parse", "--is-inside-work-tree"], PROBE_TIMEOUT)? {
        Git::Done(Some(0), out) if out.trim_ascii() == b"true" => {}
        Git::TimedOut => return Err("git rev-parse timed out.".to_string()),
        Git::Done(..) => return Err("Not a git repository.".to_string()),
    }
    let mut base = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=dirty",
    ];
    base.extend_from_slice(request.mode.args());
    let pathspec: Vec<&str> = std::iter::once("--")
        .chain(request.paths.iter().map(String::as_str))
        .collect();
    let run_diff = |extra: &[&str], timeout: Duration| -> Result<String, String> {
        let args = base
            .iter()
            .chain(extra)
            .chain(&pathspec)
            .copied()
            .collect::<Vec<_>>();
        match git(cwd, args, timeout)? {
            Git::Done(Some(0), out) => Ok(String::from_utf8_lossy(&out).trim().to_string()),
            Git::Done(code, _) => Err(format!(
                "git diff failed (exit {}).",
                code.map_or("signal".to_string(), |c| c.to_string())
            )),
            Git::TimedOut => Err("git diff timed out.".to_string()),
        }
    };
    let stat = run_diff(&["--stat"], GIT_TIMEOUT)?;
    // `--stat` prints only the summary and the untracked names.
    let mut diff = if request.stat_only {
        String::new()
    } else {
        run_diff(&[], PATCH_TIMEOUT)?
    };
    let untracked = if request.mode.lists_untracked() && request.paths.is_empty() {
        untracked_files(cwd)?
    } else {
        Vec::new()
    };
    if !request.stat_only && !untracked.is_empty() {
        let chunks = untracked_diff(cwd, &untracked, |path| {
            git(
                cwd,
                [
                    OsStr::new("diff"),
                    OsStr::new("--no-ext-diff"),
                    OsStr::new("--no-textconv"),
                    OsStr::new("--no-index"),
                    OsStr::new("--"),
                    OsStr::new("/dev/null"),
                    path,
                ],
                GIT_TIMEOUT,
            )
        })?;
        diff = format!("{diff}\n{chunks}").trim().to_string();
    }
    Ok(WorkingDiff {
        stat,
        diff,
        untracked: untracked.into_iter().map(|file| file.shown).collect(),
    })
}

fn untracked_files(cwd: &Path) -> Result<Vec<Untracked>, String> {
    let out = match git(
        cwd,
        ["ls-files", "-z", "--others", "--exclude-standard"],
        GIT_TIMEOUT,
    )? {
        Git::Done(Some(0), out) => out,
        Git::Done(..) => return Ok(Vec::new()),
        Git::TimedOut => return Err("git ls-files timed out.".to_string()),
    };
    Ok(out
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| Untracked {
            shown: String::from_utf8_lossy(name).into_owned(),
            path: path_from_bytes(name),
        })
        .collect())
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStrExt;
    Some(OsStr::from_bytes(bytes).to_os_string())
}

/// Git prints paths as UTF-8 on Windows.
#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> Option<OsString> {
    std::str::from_utf8(bytes).ok().map(OsString::from)
}

/// New-file diffs for the first [`MAX_UNTRACKED_DIFFS`] untracked files.
/// Only a regular file goes to `run` (`git diff --no-index`); a symbolic link
/// shows its target text, as git shows a committed one, and anything else
/// (a FIFO, a socket, a nested repository's folder) is left out, so nothing
/// outside the working tree is read. A file whose diff times out is skipped.
fn untracked_diff(
    cwd: &Path,
    files: &[Untracked],
    run: impl Fn(&OsStr) -> Result<Git, String>,
) -> Result<String, String> {
    let mut chunks = Vec::new();
    for file in files.iter().take(MAX_UNTRACKED_DIFFS) {
        let Some(path) = &file.path else { continue };
        let Ok(meta) = std::fs::symlink_metadata(cwd.join(path)) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(cwd.join(path)) {
                chunks.push(symlink_diff(&file.shown, &target.to_string_lossy()));
            }
        } else if meta.is_file() {
            // `--no-index` exits 1 when the files differ, which is the case here.
            if let Git::Done(_, out) = run(path.as_os_str())? {
                let out = String::from_utf8_lossy(&out);
                if !out.trim().is_empty() {
                    chunks.push(out.trim_end_matches('\n').to_string());
                }
            }
        }
    }
    if files.len() > MAX_UNTRACKED_DIFFS {
        chunks.push(format!(
            "... ({} more untracked files not shown)",
            files.len() - MAX_UNTRACKED_DIFFS
        ));
    }
    Ok(chunks.join("\n"))
}

/// A new symbolic link as git shows one: its target as the only line.
fn symlink_diff(name: &str, target: &str) -> String {
    format!(
        "diff --git a/{name} b/{name}\nnew file mode 120000\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1 @@\n+{target}\n\\ No newline at end of file"
    )
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
    fn help_is_a_path_and_quotes_keep_a_path_whole() {
        assert_eq!(request(&["help", "--help"]).paths, ["help", "--help"]);
        // The dispatcher splits `/diff "a b.txt" c` on whitespace.
        assert_eq!(request(&["\"a", "b.txt\"", "c"]).paths, ["a b.txt", "c"]);
        assert!(parse_diff_args(&["\"a"]).is_err());
    }

    #[test]
    fn terminal_cwd_chooses_the_folder_when_set() {
        assert_eq!(
            diff_cwd(Some(OsString::from("/elsewhere"))),
            Ok(PathBuf::from("/elsewhere"))
        );
        let here = std::env::current_dir().unwrap();
        assert_eq!(diff_cwd(Some(OsString::new())), Ok(here.clone()));
        assert_eq!(diff_cwd(None), Ok(here));
    }

    #[test]
    fn working_staged_and_all_follow_git() {
        let _env = crate::test_env_lock::lock();
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
    fn stat_only_lists_untracked_names_without_diffs() {
        let _env = crate::test_env_lock::lock();
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();
        let diff = collect_working_diff(dir.path(), &request(&["--stat"])).unwrap();
        assert!(diff.stat.contains("a.txt"), "{diff:?}");
        assert_eq!(diff.untracked, ["new.txt"]);
        assert_eq!(diff.diff, "");
    }

    #[test]
    fn trailing_spaces_survive_between_untracked_files() {
        let _env = crate::test_env_lock::lock();
        let dir = repo();
        std::fs::write(dir.path().join("u1.txt"), "x \n").unwrap();
        std::fs::write(dir.path().join("u2.txt"), "y\n").unwrap();
        let diff = collect_working_diff(dir.path(), &request(&[])).unwrap();
        assert!(diff.diff.contains("\n+x \n"), "{diff:?}");
        assert!(diff.diff.ends_with("+y"), "{diff:?}");
    }

    #[test]
    fn an_untracked_file_that_times_out_is_skipped_and_the_rest_kept() {
        let _env = crate::test_env_lock::lock();
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<Untracked> = ["slow.txt", "fast.txt"]
            .iter()
            .map(|name| {
                std::fs::write(dir.path().join(name), "x\n").unwrap();
                Untracked {
                    shown: name.to_string(),
                    path: Some(OsString::from(name)),
                }
            })
            .collect();
        let chunks = untracked_diff(dir.path(), &files, |path| {
            Ok(if path == "slow.txt" {
                Git::TimedOut
            } else {
                Git::Done(Some(1), b"+fast\n".to_vec())
            })
        })
        .unwrap();
        assert_eq!(chunks, "+fast");
    }

    #[test]
    fn a_path_limits_the_diff_and_skips_untracked_files() {
        let _env = crate::test_env_lock::lock();
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
        let _env = crate::test_env_lock::lock();
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
        let _env = crate::test_env_lock::lock();
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

    /// A name that is not UTF-8 must reach git as its own bytes: decoded
    /// lossily it could name a link (`\u{FFFD}`) to a folder outside.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_read_inside_the_tree() {
        use std::os::unix::ffi::OsStrExt;
        let _env = crate::test_env_lock::lock();
        let dir = repo();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("f"), "outside secret\n").unwrap();
        let odd = dir.path().join(OsStr::from_bytes(b"\xff"));
        std::fs::create_dir(&odd).unwrap();
        std::fs::write(odd.join("f"), "inside\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("\u{FFFD}")).unwrap();
        let diff = collect_working_diff(dir.path(), &request(&[])).unwrap();
        assert!(diff.diff.contains("+inside"), "{diff:?}");
        assert!(!diff.diff.contains("outside secret"), "{diff:?}");
    }

    /// An untracked link shows its target text; git never opens what it
    /// points to, so a FIFO outside cannot feed or stall the diff.
    #[cfg(unix)]
    #[test]
    fn untracked_links_and_fifos_are_not_read() {
        let _env = crate::test_env_lock::lock();
        let dir = repo();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "outside secret\n").unwrap();
        let fifo = outside.path().join("pipe");
        let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        std::os::unix::fs::symlink(&secret, dir.path().join("to-secret")).unwrap();
        std::os::unix::fs::symlink(&fifo, dir.path().join("to-pipe")).unwrap();
        let local_fifo = dir.path().join("local-pipe");
        assert!(Command::new("mkfifo")
            .arg(&local_fifo)
            .status()
            .unwrap()
            .success());
        let started = std::time::Instant::now();
        let diff = collect_working_diff(dir.path(), &request(&[])).unwrap();
        assert!(started.elapsed() < GIT_TIMEOUT, "{:?}", started.elapsed());
        assert!(!diff.diff.contains("outside secret"), "{diff:?}");
        assert!(
            diff.diff.contains(&format!(
                "new file mode 120000\n--- /dev/null\n+++ b/to-secret\n@@ -0,0 +1 @@\n+{}",
                secret.display()
            )),
            "{diff:?}"
        );
    }

    /// A submodule has its own configuration; `/diff` must not run git in
    /// its working tree, where that configuration could run a program.
    #[cfg(unix)]
    #[test]
    fn a_submodule_configuration_cannot_run_a_program() {
        let _env = crate::test_env_lock::lock();
        use std::os::unix::fs::PermissionsExt;
        let sub_origin = repo();
        let dir = repo();
        run(
            dir.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub_origin.path().to_str().unwrap(),
                "sub",
            ],
        );
        run(dir.path(), &["commit", "-qm", "sub"]);
        let marker = dir.path().join("ran");
        let script = dir.path().join("hook.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sub = dir.path().join("sub");
        let driver = script.display().to_string();
        run(&sub, &["config", "core.fsmonitor", &driver]);
        run(&sub, &["config", "filter.evil.clean", &driver]);
        std::fs::write(sub.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        std::fs::write(sub.join("a.txt"), "changed\n").unwrap();
        run(dir.path(), &["config", "diff.submodule", "diff"]);
        for mode in [&[][..], &["all"][..]] {
            collect_working_diff(dir.path(), &request(mode)).unwrap();
        }
        assert!(!marker.exists(), "a submodule-configured program ran");
    }

    /// A submodule moved to a new commit shows as its changed gitlink in every
    /// mode, whilst its own configuration, kept in the superproject's
    /// `.git/modules`, runs nothing and fetches nothing.
    #[cfg(unix)]
    #[test]
    fn a_changed_submodule_commit_shows_as_its_gitlink_and_runs_nothing() {
        let _env = crate::test_env_lock::lock();
        use std::os::unix::fs::PermissionsExt;
        let sub_origin = repo();
        let dir = repo();
        run(
            dir.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub_origin.path().to_str().unwrap(),
                "sub",
            ],
        );
        run(dir.path(), &["commit", "-qm", "sub"]);
        let sub = dir.path().join("sub");
        // The submodule's git directory lives outside its working tree.
        let gitfile = std::fs::read_to_string(sub.join(".git")).unwrap();
        assert!(gitfile.starts_with("gitdir: "), "{gitfile}");
        let head = || {
            let out = Command::new("git")
                .arg("-C")
                .arg(&sub)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let old = head();
        run(&sub, &["config", "user.email", "t@example.com"]);
        run(&sub, &["config", "user.name", "t"]);
        run(&sub, &["config", "commit.gpgsign", "false"]);
        std::fs::write(sub.join("a.txt"), "moved\n").unwrap();
        run(&sub, &["commit", "-qam", "move"]);
        let new = head();

        let marker = dir.path().join("ran");
        let script = dir.path().join("hook.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let driver = script.display().to_string();
        run(&sub, &["config", "core.fsmonitor", &driver]);
        run(&sub, &["config", "filter.evil.clean", &driver]);
        run(&sub, &["config", "protocol.ext.allow", "always"]);
        run(
            &sub,
            &["config", "remote.origin.url", &format!("ext::{driver}")],
        );
        std::fs::write(sub.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        std::fs::write(sub.join("a.txt"), "dirty\n").unwrap();
        run(dir.path(), &["config", "diff.submodule", "diff"]);
        run(dir.path(), &["config", "fetch.recurseSubmodules", "true"]);

        let gitlink = format!("-Subproject commit {old}\n+Subproject commit {new}");
        let working = collect_working_diff(dir.path(), &request(&[])).unwrap();
        assert!(working.diff.contains(&gitlink), "{working:?}");
        assert!(working.stat.contains("sub"), "{working:?}");
        assert!(!marker.exists(), "a submodule-configured program ran");
        // Staged without running git in the submodule.
        run(
            dir.path(),
            &["update-index", "--cacheinfo", &format!("160000,{new},sub")],
        );
        for mode in [&["staged"][..], &["all"][..]] {
            let diff = collect_working_diff(dir.path(), &request(mode)).unwrap();
            assert!(diff.diff.contains(&gitlink), "{mode:?}: {diff:?}");
        }
        assert!(!marker.exists(), "a submodule-configured program ran");
    }

    #[test]
    fn long_diffs_and_many_untracked_files_are_cut() {
        let _env = crate::test_env_lock::lock();
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

#[cfg(test)]
#[path = "diff_command_coverage_gen.rs"]
mod coverage_gen_commands_diff_command;
