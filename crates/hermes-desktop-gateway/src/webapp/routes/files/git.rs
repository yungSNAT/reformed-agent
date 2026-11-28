use std::ffi::OsStr;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rustix::fs::OFlags;
use serde_json::{json, Value};

use hermes_core::git::GitRead;

use super::fs::{open_dir_walk, open_resolved, openat, read_capped, Opened};
use super::roots::{resolve, sensitive, Refusal, Resolved, Roots, SENSITIVE_DIRS, SENSITIVE_FILES};
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
        .config("status.relativePaths", "false")
        .config("diff.relative", "false")
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

fn run_git<S: AsRef<OsStr>>(mut cmd: Command, args: &[S]) -> Option<(bool, String)> {
    cmd.args(args);
    let out = hermes_core::git::output_within_timeout(&mut cmd, GIT_TIMEOUT)?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Descriptor-backed path for git: `/proc/<pid>/fd/<fd>` on Linux, `fcntl(F_GETPATH)` on Apple,
/// failing closed elsewhere so git never falls back to an unpinned pathname.
fn fd_path(fd: &OwnedFd) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        Some(PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            fd.as_raw_fd()
        )))
    }
    #[cfg(target_vendor = "apple")]
    {
        use std::os::unix::ffi::OsStrExt;
        let raw = rustix::fs::getpath(fd).ok()?;
        Some(PathBuf::from(OsStr::from_bytes(raw.as_bytes())))
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    {
        let _ = fd;
        None
    }
}

/// Pathspecs excluding every refused tree under `top` and every sensitive file or folder name,
/// passed after `--` so git neither reads nor reports sensitive or refused paths.
pub(crate) fn git_pathspecs(top: &Path, refused: &[PathBuf]) -> Vec<String> {
    let mut specs = vec![
        ":(top)".to_owned(),
        ":(top,exclude,icase,glob)**/.env".to_owned(),
        ":(top,exclude,icase,glob)**/.env.*".to_owned(),
        ":(top,exclude,icase,glob)**/.envrc".to_owned(),
    ];
    for rel in refused
        .iter()
        .filter_map(|r| r.strip_prefix(top).ok())
        .filter(|rel| !rel.as_os_str().is_empty())
    {
        specs.push(format!(":(top,exclude,literal){}", rel.to_string_lossy()));
    }
    for name in SENSITIVE_FILES {
        specs.push(format!(":(top,exclude,icase,glob)**/{name}"));
    }
    for seq in SENSITIVE_DIRS {
        let joined = seq.join("/");
        specs.push(format!(":(top,exclude,icase,glob)**/{joined}"));
        specs.push(format!(":(top,exclude,icase,glob)**/{joined}/**"));
    }
    specs
}

fn with_pathspecs(base: &[&str], specs: &[String]) -> Vec<String> {
    base.iter()
        .copied()
        .chain(["--"])
        .map(str::to_owned)
        .chain(specs.iter().cloned())
        .collect()
}

fn hidden_repo_path(top: &Path, rel: &str, refused: &[PathBuf]) -> bool {
    let full = top.join(rel);
    sensitive(&full) || refused.iter().any(|r| full.starts_with(r))
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

/// A validated repository whose work tree, git folder and common folder are held as open
/// descriptors, pinning subsequent git commands against `.git` and directory swaps.
pub(crate) struct PinnedRepo {
    pub(crate) root: PathBuf,
    pub(crate) top: PathBuf,
    top_fd: OwnedFd,
    git_dir_fd: OwnedFd,
    common_dir_fd: OwnedFd,
}

impl PinnedRepo {
    fn commondir_check(&self) -> Option<bool> {
        let git_real = std::fs::canonicalize(fd_path(&self.git_dir_fd)?).ok()?;
        let common_real = std::fs::canonicalize(fd_path(&self.common_dir_fd)?).ok()?;
        Some(
            match openat(&self.git_dir_fd, OsStr::new("commondir"), OFlags::RDONLY) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => common_real == git_real,
                Ok(fd) => read_capped(std::fs::File::from(fd), 4096)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty() && !s.contains(['\n', '\r']))
                    .and_then(|s| std::fs::canonicalize(git_real.join(s)).ok())
                    .is_some_and(|real| real == common_real),
                Err(_) => false,
            },
        )
    }

    pub(crate) fn git(&self, read: GitRead) -> Option<Command> {
        if !self.commondir_check().unwrap_or(false) {
            return None;
        }
        let git_dir = fd_path(&self.git_dir_fd)?;
        let common_dir = fd_path(&self.common_dir_fd)?;
        let work_tree = fd_path(&self.top_fd)?;
        let mut cmd = git(&git_dir, &self.root, read)?;
        cmd.env("GIT_DIR", &git_dir)
            .env("GIT_COMMON_DIR", &common_dir)
            .env("GIT_WORK_TREE", &work_tree);
        Some(cmd)
    }

    fn out<S: AsRef<OsStr>>(&self, read: GitRead, args: &[S]) -> Option<(bool, String)> {
        run_git(self.git(read)?, args)
    }

    pub(crate) fn line(&self, read: GitRead, args: &[&str]) -> Option<String> {
        match self.out(read, args)? {
            (true, out) => out
                .lines()
                .next()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned),
            _ => None,
        }
    }
}

/// Validates that `dir` sits inside a repository whose work tree, git folder and common folder
/// stay inside the allowed root, outside `refused` and sensitive trees, and anchored to an
/// ancestor of `dir` carrying its own `.git` entry, then pins descriptors for all 3.
pub(crate) fn open_repo(dir: &RepoDir, refused: &[PathBuf]) -> Option<PinnedRepo> {
    let run_in = dir.git_path()?;
    let root = dir.resolved.root.as_path();
    let (_, out) = run_git(
        git(&run_in, root, GitRead::RevParse)?,
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
    let checked = std::fs::canonicalize(&run_in).ok()?;
    let inside = |p: &Path| {
        p.starts_with(root) && !refused.iter().any(|r| p.starts_with(r)) && !sensitive(p)
    };
    let dot_git_file =
        std::fs::symlink_metadata(top.join(".git")).is_ok_and(|m| m.file_type().is_file());
    if !(inside(&top)
        && inside(&git_dir)
        && inside(&common_dir)
        && checked.starts_with(&top)
        && (git_dir == top.join(".git") || dot_git_file))
    {
        return None;
    }
    Some(PinnedRepo {
        root: dir.resolved.root.clone(),
        top_fd: open_dir_walk(&top).ok()?,
        git_dir_fd: open_dir_walk(&git_dir).ok()?,
        common_dir_fd: open_dir_walk(&common_dir).ok()?,
        top,
    })
}

fn default_branch(repo: &PinnedRepo) -> Option<String> {
    if let Some(branch) = repo
        .line(GitRead::RevParse, &["--abbrev-ref", "origin/HEAD"])
        .and_then(|head| {
            head.strip_prefix("origin/")
                .filter(|b| !b.is_empty() && *b != "HEAD")
                .map(str::to_owned)
        })
    {
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
        repo.out(GitRead::RevParse, &["--verify", "--quiet", *r])
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

pub(crate) fn numstat_counts(numstat: &str, top: &Path, refused: &[PathBuf]) -> (u64, u64) {
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
        if paths.iter().any(|p| hidden_repo_path(top, p, refused)) {
            continue;
        }
        added += a.parse::<u64>().unwrap_or(0);
        removed += r.parse::<u64>().unwrap_or(0);
    }
    (added, removed)
}

/// Upstream's `repo_status` on an already-pinned repository: refused and sensitive paths are
/// excluded at git execution and filtered defensively from parsed output.
pub(crate) fn repo_status_pinned(repo: &PinnedRepo, refused: &[PathBuf]) -> Option<Value> {
    let specs = git_pathspecs(&repo.top, refused);
    let status_args = with_pathspecs(&STATUS_ARGS, &specs);
    let (_, raw) = repo
        .out(GitRead::Status, &status_args)
        .filter(|(ok, _)| *ok)?;
    let (branch, detached, ahead, behind) = parse_branch_headers(&raw);
    let entries: Vec<_> = walk_entries(&raw)
        .into_iter()
        .filter(|(_, _, p)| !hidden_repo_path(&repo.top, p, refused))
        .collect();
    let files: Vec<Value> = entries
        .iter()
        .map(|(t, xy, p)| classify(*t, xy, p))
        .collect();
    let diff_args = with_pathspecs(&DIFF_ARGS, &specs);
    let (mut added, removed) = repo
        .out(GitRead::Diff, &diff_args)
        .filter(|(ok, _)| *ok)
        .map_or((0, 0), |(_, out)| numstat_counts(&out, &repo.top, refused));
    added += entries
        .iter()
        .filter(|(t, ..)| *t == '?')
        .take(UNTRACKED_SCAN_CAP)
        .map(|(_, _, p)| untracked_lines(&repo.root, &repo.top, p, refused))
        .sum::<u64>();
    let count = |flag: &str| files.iter().filter(|f| f[flag] == true).count();
    Some(json!({
        "branch": branch,
        "defaultBranch": default_branch(repo),
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

/// Upstream's `repo_status`: `None` for a folder outside any repository.
pub(crate) fn repo_status(dir: &RepoDir, refused: &[PathBuf]) -> Option<Value> {
    let repo = open_repo(dir, refused)?;
    repo_status_pinned(&repo, refused)
}

/// A repository folder a git route names: checked as `fs/list` checks a folder and held open.
pub(crate) struct RepoDir {
    pub(crate) resolved: Resolved,
    fd: OwnedFd,
}

impl RepoDir {
    /// Descriptor-backed path git is pointed at so it stays bound to the folder that was checked.
    pub(crate) fn git_path(&self) -> Option<PathBuf> {
        fd_path(&self.fd)
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
