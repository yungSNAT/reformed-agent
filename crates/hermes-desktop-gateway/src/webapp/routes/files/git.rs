use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use hermes_core::git::GitRead;

use super::fs::{open_resolved, read_capped, Opened};
use super::roots::{resolve, sensitive, Refusal, Resolved, Roots};
use crate::webapp::api::ApiError;

const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const UNTRACKED_SCAN_CAP: usize = 200;
pub(crate) const UNTRACKED_LINE_MAX_BYTES: u64 = 1024 * 1024;
const STATUS_FILES_MAX: usize = 200;

/// A ref as `rev-parse` may take it: plain ref characters, no option and no range.
pub(crate) fn valid_ref(r: &str) -> bool {
    (1..=256).contains(&r.len())
        && !r.starts_with('-')
        && !r.contains("..")
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/@{}^~-".contains(&b))
}

/// A git command for the folder `dir` under `root`: the repository's configuration cannot run
/// a program, discovery stops at the root, and no hook, external diff or submodule diff runs.
pub(crate) fn git(dir: &Path, root: &Path, read: GitRead) -> Option<Command> {
    let mut cmd = hermes_core::git::read_only_git(dir)?
        .config("core.hooksPath", "/dev/null")
        .config("diff.external", "")
        .command(read);
    let ceiling = root.parent().unwrap_or(root);
    cmd.env("GIT_CEILING_DIRECTORIES", ceiling)
        .env("GIT_TERMINAL_PROMPT", "0");
    Some(cmd)
}

/// `status` options as `git/status` runs it. A submodule has its own configuration, which the
/// overrides do not reach, so status never looks inside one: `=all` follows and outranks the
/// `=dirty` guard [`GitRead::Status`] puts first.
pub(crate) const STATUS_ARGS: [&str; 4] = [
    "--porcelain=v2",
    "--branch",
    "-z",
    "--ignore-submodules=all",
];

/// The line counts of tracked changes, never inside a submodule.
pub(crate) const DIFF_ARGS: [&str; 6] = [
    "--no-ext-diff",
    "--no-textconv",
    "--numstat",
    "-z",
    "--ignore-submodules=all",
    "HEAD",
];

/// `rev-parse` options for a ref `valid_ref` passed, after `--end-of-options` all the same.
pub(crate) fn rev_parse_args(wanted: &str) -> [&str; 3] {
    ["--verify", "--end-of-options", wanted]
}

fn git_out(dir: &Path, root: &Path, read: GitRead, args: &[&str]) -> Option<(bool, String)> {
    let mut cmd = git(dir, root, read)?;
    cmd.args(args);
    let out = hermes_core::git::output_within_timeout(&mut cmd, GIT_TIMEOUT)?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

pub(crate) fn git_line(dir: &Path, root: &Path, read: GitRead, args: &[&str]) -> Option<String> {
    match git_out(dir, root, read, args)? {
        (true, out) => out
            .lines()
            .next()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned),
        _ => None,
    }
}

/// `(tag, xy, path)` per changed file of porcelain v2 `-z` output (upstream `_walk_entries`).
pub(crate) fn walk_entries(raw: &str) -> Vec<(char, String, String)> {
    let mut out = Vec::new();
    let mut records = raw.split('\0');
    while let Some(rec) = records.next() {
        let Some(tag) = rec.chars().next() else {
            continue;
        };
        let field = |n: usize| rec.split(' ').nth(n).unwrap_or("").to_owned();
        match tag {
            '?' => out.push(('?', "??".to_owned(), rec.get(2..).unwrap_or("").to_owned())),
            'u' => out.push((
                'u',
                field(1),
                rec.splitn(11, ' ').last().unwrap_or("").to_owned(),
            )),
            '1' => out.push((
                '1',
                field(1),
                rec.splitn(9, ' ').last().unwrap_or("").to_owned(),
            )),
            '2' => {
                out.push((
                    '2',
                    field(1),
                    rec.splitn(10, ' ').last().unwrap_or("").to_owned(),
                ));
                records.next();
            }
            _ => {}
        }
    }
    out
}

pub(crate) fn classify(tag: char, xy: &str, path: &str) -> Value {
    let x = xy.chars().next().unwrap_or('.');
    let y = xy.chars().nth(1).unwrap_or('.');
    let tracked = tag == '1' || tag == '2';
    json!({
        "path": path,
        "staged": tracked && x != '.' && x != '?',
        "unstaged": tag == '?' || (tracked && y != '.' && y != '?'),
        "untracked": tag == '?',
        "conflicted": tag == 'u',
    })
}

/// The line count of the untracked file `rel` (a path git printed, relative to the repository's
/// top `top`), walked from `root` without following a link; 0 when it leaves the root, is
/// sensitive or refused, or is not a small text file.
pub(crate) fn untracked_lines(root: &Path, top: &Path, rel: &str, refused: &[PathBuf]) -> u64 {
    let rel = Path::new(rel);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return 0;
    }
    let path = top.join(rel);
    if sensitive(&path) || refused.iter().any(|r| path.starts_with(r)) {
        return 0;
    }
    let resolved = Resolved {
        root: root.to_path_buf(),
        path,
    };
    let Ok(Opened::File(file, st)) = open_resolved(&resolved) else {
        return 0;
    };
    if st.st_size as u64 > UNTRACKED_LINE_MAX_BYTES {
        return 0;
    }
    let Some(data) = read_capped(file, UNTRACKED_LINE_MAX_BYTES)
        .ok()
        .filter(|data| !data.contains(&0))
    else {
        return 0;
    };
    let lines = data.iter().filter(|&&b| b == b'\n').count() as u64;
    if !data.is_empty() && !data.ends_with(b"\n") {
        lines + 1
    } else {
        lines
    }
}

/// The repository's canonical top for `run_in`, or `None` when `run_in` is not in a repository
/// whose work tree and git folder stay inside `root`, outside `refused` and sensitive trees, and
/// anchored to an ancestor of `run_in` carrying its own `.git` entry.
pub(crate) fn repo_top(run_in: &Path, root: &Path, refused: &[PathBuf]) -> Option<PathBuf> {
    let (_, out) = git_out(
        run_in,
        root,
        GitRead::RevParse,
        &[
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ],
    )
    .filter(|(ok, _)| *ok)?;
    let mut lines = out.lines();
    let (Some(top), Some(git_dir), Some(common_dir), None) =
        (lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return None;
    };
    let top = std::fs::canonicalize(top).ok()?;
    let git_dir = std::fs::canonicalize(git_dir).ok()?;
    let common_dir = std::fs::canonicalize(common_dir).ok()?;
    let checked = std::fs::canonicalize(run_in).ok()?;
    let inside = |p: &Path| {
        p.starts_with(root) && !refused.iter().any(|r| p.starts_with(r)) && !sensitive(p)
    };
    (inside(&top)
        && inside(&git_dir)
        && inside(&common_dir)
        && checked.starts_with(&top)
        && (git_dir == top.join(".git") || top.join(".git").is_file()))
    .then_some(top)
}

fn default_branch(dir: &Path, root: &Path) -> Option<String> {
    if let Some(branch) = git_line(
        dir,
        root,
        GitRead::RevParse,
        &["--abbrev-ref", "origin/HEAD"],
    )
    .and_then(|head| {
        head.strip_prefix("origin/")
            .filter(|b| !b.is_empty() && *b != "HEAD")
            .map(str::to_owned)
    }) {
        return Some(branch);
    }
    [
        "refs/heads/main",
        "refs/heads/master",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ]
    .into_iter()
    .find(|r| {
        git_out(dir, root, GitRead::RevParse, &["--verify", "--quiet", *r])
            .is_some_and(|(ok, _)| ok)
    })
    .map(|r| r.rsplit('/').next().unwrap_or(r).to_owned())
}

fn parse_branch_headers(raw: &str) -> (Option<String>, bool, i64, i64) {
    let (mut branch, mut detached, mut ahead, mut behind) = (None, false, 0i64, 0i64);
    for rec in raw.split('\0') {
        if let Some(head) = rec.strip_prefix("# branch.head ") {
            detached = head == "(detached)";
            branch = (!detached).then(|| head.to_owned());
        } else if let Some(ab) = rec.strip_prefix("# branch.ab ") {
            // `+<ahead> -<behind>`, in that order.
            let mut counts = ab
                .split_whitespace()
                .map(|t| t.get(1..).and_then(|n| n.parse().ok()).unwrap_or(0));
            ahead = counts.next().unwrap_or(0);
            behind = counts.next().unwrap_or(0);
        }
    }
    (branch, detached, ahead, behind)
}

fn numstat_counts(numstat: &str, top: &Path, refused: &[PathBuf]) -> (u64, u64) {
    // `-z`: `added<TAB>removed<TAB>path` records; a rename leaves `path` empty and is
    // followed by 2 NUL-terminated path records, which must not be parsed as counts.
    let (mut added, mut removed) = (0u64, 0u64);
    let mut records = numstat.split('\0');
    while let Some(record) = records.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let paths = if path.is_empty() {
            [records.next().unwrap_or(""), records.next().unwrap_or("")]
        } else {
            [path, path]
        };
        if paths
            .iter()
            .any(|p| refused.iter().any(|r| top.join(p).starts_with(r)))
        {
            continue;
        }
        added += a.parse::<u64>().unwrap_or(0);
        removed += r.parse::<u64>().unwrap_or(0);
    }
    (added, removed)
}

/// Upstream's `repo_status`: `None` for a folder outside any repository. Untracked files under
/// `refused` are not read, and entries under `refused` are omitted.
pub(crate) fn repo_status(dir: &RepoDir, refused: &[PathBuf]) -> Option<Value> {
    let run_in = dir.git_path();
    let root = dir.resolved.root.as_path();
    let top = repo_top(&run_in, root, refused)?;
    let (_, raw) = git_out(&run_in, root, GitRead::Status, &STATUS_ARGS).filter(|(ok, _)| *ok)?;
    let (branch, detached, ahead, behind) = parse_branch_headers(&raw);
    let entries: Vec<_> = walk_entries(&raw)
        .into_iter()
        .filter(|(_, _, p)| !refused.iter().any(|r| top.join(p).starts_with(r)))
        .collect();
    let files: Vec<Value> = entries
        .iter()
        .map(|(t, xy, p)| classify(*t, xy, p))
        .collect();
    let (mut added, removed) = git_out(&run_in, root, GitRead::Diff, &DIFF_ARGS)
        .filter(|(ok, _)| *ok)
        .map_or((0, 0), |(_, out)| numstat_counts(&out, &top, refused));
    added += entries
        .iter()
        .filter(|(t, ..)| *t == '?')
        .take(UNTRACKED_SCAN_CAP)
        .map(|(_, _, p)| untracked_lines(root, &top, p, refused))
        .sum::<u64>();
    let count = |flag: &str| files.iter().filter(|f| f[flag] == true).count();
    Some(json!({
        "branch": branch,
        "defaultBranch": default_branch(&run_in, root),
        "detached": detached,
        "ahead": ahead,
        "behind": behind,
        "staged": count("staged"),
        "unstaged": count("unstaged"),
        "untracked": count("untracked"),
        "conflicted": count("conflicted"),
        "changed": files.len(),
        "added": added,
        "removed": removed,
        "files": files.into_iter().take(STATUS_FILES_MAX).collect::<Vec<_>>(),
    }))
}

/// A repository folder a git route names: checked as `fs/list` checks a folder and held open.
pub(crate) struct RepoDir {
    pub(crate) resolved: Resolved,
    fd: OwnedFd,
}

impl RepoDir {
    /// The path git is pointed at. On Linux it names the open descriptor, so git (its own
    /// configuration read included) works in the folder that was checked even if a link is
    /// swapped in afterwards; elsewhere it is the checked path.
    pub(crate) fn git_path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                self.fd.as_raw_fd()
            ))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = &self.fd;
            self.resolved.path.clone()
        }
    }
}

/// The repository folder a git route names: a folder inside a root, resolved as `fs/list` does.
pub(crate) fn repo_dir(raw: &str, roots: &Roots, home: Option<&Path>) -> Result<RepoDir, ApiError> {
    let resolved = resolve(raw, roots, home)?;
    match open_resolved(&resolved)? {
        Opened::Dir(fd) => Ok(RepoDir { resolved, fd }),
        Opened::File(..) => Err(Refusal::NotADirectory.into()),
    }
}
