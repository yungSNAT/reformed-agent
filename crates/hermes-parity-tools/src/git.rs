//! Git as the Python scripts ran it: `subprocess.run(["git", ...],
//! cwd=repo_root, text=True, capture_output=True)`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

pub struct GitOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn git(repo_root: &Path, args: &[&str]) -> Result<GitOutput, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    // Python decoded git's output strictly (`text=True` under a UTF-8
    // locale), so a file name that is not UTF-8 stops the run.
    let decode = |bytes: Vec<u8>| {
        String::from_utf8(bytes)
            .map_err(|e| format!("git {} printed text that is not UTF-8: {e}", args.join(" ")))
    };
    Ok(GitOutput {
        ok: output.status.success(),
        stdout: decode(output.stdout)?,
        stderr: decode(output.stderr)?,
    })
}

/// `run_git(..., check=True)`: stdout, or an error naming the command.
pub fn git_checked(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo_root, args)?;
    if out.ok {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            crate::py::strip(&out.stderr)
        ))
    }
}

/// Which of `<ref>:<path>` exist, as `git cat-file -e` would say one at a
/// time, asked of one `git cat-file --batch-check` process.
pub fn objects_exist(repo_root: &Path, rev: &str, paths: &[String]) -> Result<Vec<bool>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = Command::new("git")
        .args(["cat-file", "--batch-check"])
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run git: {e}"))?;
    let mut input = String::new();
    for path in paths {
        input.push_str(rev);
        input.push(':');
        input.push_str(path);
        input.push('\n');
    }
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child
        .wait_with_output()
        .map_err(|e| format!("git cat-file failed: {e}"))?;
    writer
        .join()
        .map_err(|_| "git cat-file input thread panicked".to_string())?
        .map_err(|e| format!("git cat-file input failed: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != paths.len() {
        return Err(format!(
            "git cat-file --batch-check answered {} of {} paths",
            lines.len(),
            paths.len()
        ));
    }
    Ok(lines
        .iter()
        .map(|line| !(line.ends_with(" missing") || line.ends_with(" ambiguous")))
        .collect())
}

pub(crate) fn ensure_remote(repo_root: &Path, remote: &str, url: &str) -> Result<(), String> {
    let out = git(repo_root, &["remote"])?;
    if crate::py::strip(&out.stdout)
        .lines()
        .any(|line| line == remote)
    {
        return Ok(());
    }
    git_checked(repo_root, &["remote", "add", remote, url]).map(|_| ())
}

pub(crate) fn fetch_remote_branch(
    repo_root: &Path,
    remote: &str,
    branch: &str,
) -> Result<(), String> {
    let remote_ref = format!("{remote}/{branch}");
    let refspec = format!("refs/heads/{branch}:refs/remotes/{remote}/{branch}");
    let out = git(repo_root, &["fetch", "--no-tags", remote, &refspec])?;
    if out.ok {
        return Ok(());
    }
    let existing = git(repo_root, &["rev-parse", "--verify", &remote_ref])?;
    if !crate::py::strip(&existing.stdout).is_empty() {
        eprintln!(
            "warning: scoped fetch for {remote_ref} failed; using existing ref: {}",
            crate::py::strip(&out.stderr)
        );
        return Ok(());
    }
    Err(format!(
        "git fetch {remote} {branch} failed: {}",
        crate::py::strip(&out.stderr)
    ))
}

pub(crate) fn git_text(root: &Path, args: &[&str], checked: bool) -> Result<String, String> {
    let stdout = if checked {
        git_checked(root, args)?
    } else {
        git(root, args)?.stdout
    };
    Ok(crate::py::strip(&stdout)
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}

pub(crate) fn python_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split_terminator([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ])
}

pub(crate) fn python_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}'))
        .filter(|word| !word.is_empty())
}

pub(crate) fn ls_tree_blobs(
    root: &Path,
    reference: &str,
    trim_lines: bool,
) -> Result<BTreeMap<String, String>, String> {
    let output = git_text(root, &["ls-tree", "-r", "--full-tree", reference], true)?;
    Ok(python_lines(&output)
        .filter_map(|line| {
            let line = if trim_lines {
                crate::py::strip(line)
            } else {
                line
            };
            let (object, path) = line.split_once('\t')?;
            let mut fields = python_words(object);
            let (_, kind, hash) = (fields.next()?, fields.next()?, fields.next()?);
            (kind == "blob" && fields.next().is_none()).then(|| (path.to_owned(), hash.to_owned()))
        })
        .collect())
}

pub(crate) fn branch_from_remote_ref<'a>(remote: &str, upstream_ref: &'a str) -> &'a str {
    match upstream_ref
        .strip_prefix(remote)
        .and_then(|r| r.strip_prefix('/'))
    {
        Some(branch) if !branch.is_empty() => branch,
        _ => "main",
    }
}
