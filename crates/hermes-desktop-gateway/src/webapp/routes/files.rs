//! Host folders, file bytes and repository state for the Webapp's file picker, attachments and
//! coding rail: `/api/fs/list`, `/api/fs/read-data-url`, `/api/git/status` and
//! `/api/git/review/rev-parse`.
//!
//! Upstream at the pin: `hermes_cli/web_routers/files.py` and `web_routers/git.py` with
//! `hermes_cli/web_git.py`. Stricter than upstream, each on purpose:
//! - a path must resolve inside the user's home folder or the served profile's working folder
//!   (upstream: the whole host on a loopback Webapp), and the Hermes home is refused wherever it
//!   is (it holds the browser credential and saved gateway tokens, which upstream's list lacks);
//! - paths are absolute or `~`; the resolved path is opened by walking it from the root's
//!   descriptor without following links, so a link swapped in after the check is refused;
//! - only a loopback peer reaches these routes, and never the remote-mode token caller;
//! - `rev-parse` takes a ref of plain ref characters only, after `--end-of-options`;
//! - git never searches above the root, runs no hook, external diff or configured program.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use axum::extract::{ConnectInfo, Query};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::startup::{blocking, default_cwd_for, machine_root};
use crate::webapp::api::{served_profile, ApiError, ApiResult, Caller, Ctx};

pub fn router() -> Router {
    Router::new()
        .route("/api/fs/list", get(fs_list))
        .route("/api/fs/read-data-url", get(read_data_url))
        .route("/api/git/status", get(git_status))
        .route("/api/git/review/rev-parse", get(rev_parse))
}

/// The largest file `read-data-url` returns (upstream's `WEBAPP_ATTACHMENT_MAX_BYTES`).
pub(crate) const DATA_URL_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// The most entries one listing returns; past it the answer says `truncated`.
pub(crate) const LIST_MAX_ENTRIES: usize = 10_000;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const UNTRACKED_SCAN_CAP: usize = 500;
const UNTRACKED_LINE_MAX_BYTES: u64 = 1024 * 1024;
const STATUS_FILES_MAX: usize = 200;

/// Folder names a listing leaves out (upstream's `_FS_READDIR_HIDDEN`).
const LIST_HIDDEN: [&str; 13] = [
    ".git",
    ".hg",
    ".svn",
    ".cache",
    ".next",
    ".turbo",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "venv",
];
/// Credential file names, matched case-insensitively wherever they sit (upstream's managed-file
/// list and home credential files, plus this gateway's own secret stores).
const SENSITIVE_FILES: [&str; 23] = [
    "auth.json",
    "auth.lock",
    "credentials",
    "config.yaml",
    ".anthropic_oauth.json",
    "google_token.json",
    "google_client_secret.json",
    "google_oauth_pending.json",
    "google_oauth.json",
    "webhook_subscriptions.json",
    ".op.env",
    "vault.key",
    "vault.json.enc",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".pgpass",
    ".dockercfg",
    "id_rsa",
    "gateway-connections.json",
    "gateway-connection-tokens.json",
    "session-token",
];
/// Credential folders, matched as consecutive path components anywhere.
const SENSITIVE_DIRS: [&[&str]; 10] = [
    &[".ssh"],
    &[".aws"],
    &[".gnupg"],
    &[".kube"],
    &[".docker"],
    &[".azure"],
    &[".config", "gh"],
    &[".config", "gcloud"],
    &["mcp-tokens"],
    &["whatsapp", "session"],
];

pub(crate) fn sensitive(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    let Some(name) = parts.last() else {
        return false;
    };
    if name == ".env"
        || name.starts_with(".env.")
        || name == ".envrc"
        || SENSITIVE_FILES.contains(&name.as_str())
    {
        return true;
    }
    SENSITIVE_DIRS.iter().any(|seq| {
        parts
            .windows(seq.len())
            .any(|w| w.iter().zip(seq.iter()).all(|(a, b)| a == b))
    })
}

#[derive(Debug, PartialEq)]
pub(crate) enum Refusal {
    BadPath(&'static str),
    Outside,
    Sensitive,
    NotFound,
    NotADirectory,
    NotAFile,
    Denied,
    TooLarge,
}

impl From<Refusal> for ApiError {
    fn from(r: Refusal) -> Self {
        let (status, text) = match r {
            Refusal::BadPath(why) => (StatusCode::BAD_REQUEST, why),
            Refusal::Outside => (
                StatusCode::FORBIDDEN,
                "Outside the folders this Webapp may read",
            ),
            Refusal::Sensitive => (
                StatusCode::FORBIDDEN,
                "Access to sensitive files is not allowed",
            ),
            Refusal::NotFound => (StatusCode::NOT_FOUND, "Path not found"),
            Refusal::NotADirectory => (StatusCode::BAD_REQUEST, "Path is not a folder"),
            Refusal::NotAFile => (StatusCode::BAD_REQUEST, "Only regular files can be read"),
            Refusal::Denied => (StatusCode::FORBIDDEN, "Permission denied"),
            Refusal::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "File too large"),
        };
        ApiError(status, text.to_owned())
    }
}

/// Where paths may resolve, and the trees refused inside them. All canonical.
#[derive(Clone, Debug)]
pub(crate) struct Roots {
    pub allowed: Vec<PathBuf>,
    pub refused: Vec<PathBuf>,
}

impl Roots {
    pub(crate) fn new(allowed: &[PathBuf], refused: &[PathBuf]) -> Self {
        let canon = |paths: &[PathBuf]| -> Vec<PathBuf> {
            paths
                .iter()
                .filter_map(|p| std::fs::canonicalize(p).ok())
                .collect()
        };
        Roots {
            allowed: canon(allowed),
            refused: canon(refused),
        }
    }
}

/// A path resolved and checked: its root and the canonical path under it.
#[derive(Debug)]
pub(crate) struct Resolved {
    root: PathBuf,
    path: PathBuf,
}

/// Expands `~`, requires an absolute path, canonicalises it and checks it against `roots` and
/// the sensitive lists (requested and resolved spellings both).
pub(crate) fn resolve(raw: &str, roots: &Roots, home: Option<&Path>) -> Result<Resolved, Refusal> {
    if raw.contains('\0') {
        return Err(Refusal::BadPath("Invalid path"));
    }
    let raw = raw.trim();
    let requested = if raw == "~" {
        home.ok_or(Refusal::BadPath("No home folder"))?
            .to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.ok_or(Refusal::BadPath("No home folder"))?.join(rest)
    } else {
        PathBuf::from(raw)
    };
    if !requested.is_absolute() {
        return Err(Refusal::BadPath("Path must be absolute"));
    }
    if sensitive(&requested) {
        return Err(Refusal::Sensitive);
    }
    let path = std::fs::canonicalize(&requested).map_err(|e| io_refusal(&e))?;
    if sensitive(&path) {
        return Err(Refusal::Sensitive);
    }
    if roots.refused.iter().any(|r| path.starts_with(r)) {
        return Err(Refusal::Sensitive);
    }
    let root = roots
        .allowed
        .iter()
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
        .ok_or(Refusal::Outside)?;
    Ok(Resolved {
        root: root.clone(),
        path,
    })
}

fn io_refusal(err: &std::io::Error) -> Refusal {
    match err.kind() {
        std::io::ErrorKind::NotFound => Refusal::NotFound,
        std::io::ErrorKind::PermissionDenied => Refusal::Denied,
        std::io::ErrorKind::NotADirectory => Refusal::NotADirectory,
        _ => match err.raw_os_error() {
            Some(code) if code == libc::ELOOP => Refusal::Outside,
            Some(code) if code == libc::ENOTDIR => Refusal::NotADirectory,
            _ => Refusal::NotFound,
        },
    }
}

/// What a walk opened.
pub(crate) enum Opened {
    Dir(std::os::fd::OwnedFd),
    File(std::fs::File, libc::stat),
}

fn fstat(fd: &impl std::os::fd::AsRawFd) -> std::io::Result<libc::stat> {
    // SAFETY: an all-zero stat is a valid out buffer; fd is open for the call's duration.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

fn openat(
    dir: libc::c_int,
    name: &OsStr,
    flags: libc::c_int,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: dir is an open descriptor or AT_FDCWD and name a NUL-terminated string.
    let fd = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor this function owns.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// Opens `resolved` by walking it from its root's descriptor, one segment at a time without
/// following a link, so what opens is what was checked. Only a folder or a regular file opens;
/// a FIFO or device is refused without blocking.
pub(crate) fn open_resolved(resolved: &Resolved) -> Result<Opened, Refusal> {
    use std::os::fd::AsRawFd;
    let rel = resolved
        .path
        .strip_prefix(&resolved.root)
        .map_err(|_| Refusal::Outside)?;
    let mut dir = openat(
        libc::AT_FDCWD,
        resolved.root.as_os_str(),
        libc::O_RDONLY | libc::O_DIRECTORY,
    )
    .map_err(|e| io_refusal(&e))?;
    let parts: Vec<&OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(n) => Ok(n),
            _ => Err(Refusal::Outside),
        })
        .collect::<Result<_, _>>()?;
    let Some((last, folders)) = parts.split_last() else {
        return Ok(Opened::Dir(dir));
    };
    for part in folders {
        dir = openat(dir.as_raw_fd(), part, libc::O_RDONLY | libc::O_DIRECTORY)
            .map_err(|e| io_refusal(&e))?;
    }
    let fd = openat(dir.as_raw_fd(), last, libc::O_RDONLY | libc::O_NONBLOCK)
        .map_err(|e| io_refusal(&e))?;
    let st = fstat(&fd).map_err(|e| io_refusal(&e))?;
    match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Ok(Opened::Dir(fd)),
        libc::S_IFREG => Ok(Opened::File(std::fs::File::from(fd), st)),
        _ => Err(Refusal::NotAFile),
    }
}

/// The entries of an opened folder, as upstream lists them: folders first, then by name;
/// build folders and sensitive entries left out; links listed by their own type.
pub(crate) fn list_entries(
    dir: std::os::fd::OwnedFd,
    shown: &Path,
) -> std::io::Result<(Vec<Value>, bool)> {
    use std::os::fd::IntoRawFd;
    use std::os::unix::ffi::OsStrExt;
    let raw = dir.into_raw_fd();
    // SAFETY: raw is an open directory descriptor whose ownership passes to the stream.
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let err = std::io::Error::last_os_error();
        // SAFETY: fdopendir failed, so raw is still ours to close.
        unsafe { libc::close(raw) };
        return Err(err);
    }
    let mut entries: Vec<(bool, String)> = Vec::new();
    let mut truncated = false;
    loop {
        // SAFETY: stream is a valid open directory stream until closedir below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: readdir returned a valid entry whose d_name is NUL-terminated.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let os = OsStr::from_bytes(bytes);
        let text = os.to_string_lossy().into_owned();
        if LIST_HIDDEN.contains(&text.as_str()) || sensitive(&shown.join(os)) {
            continue;
        }
        if entries.len() == LIST_MAX_ENTRIES {
            truncated = true;
            break;
        }
        // SAFETY: an all-zero stat is a valid out buffer; the stream's descriptor is open.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let is_dir = unsafe {
            libc::fstatat(
                libc::dirfd(stream),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
            && st.st_mode & libc::S_IFMT == libc::S_IFDIR;
        entries.push((is_dir, text));
    }
    // SAFETY: stream came from fdopendir and is closed once.
    unsafe { libc::closedir(stream) };
    entries.sort_by(|a, b| (!a.0, a.1.to_lowercase(), &a.1).cmp(&(!b.0, b.1.to_lowercase(), &b.1)));
    let list = entries
        .into_iter()
        .map(|(is_dir, name)| json!({"name": name, "path": shown.join(&name).to_string_lossy(), "isDirectory": is_dir}))
        .collect();
    Ok((list, truncated))
}

fn mime_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/vnd.microsoft.icon",
        Some("avif") => "image/avif",
        Some("heic") => "image/heic",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js" | "mjs") => "text/javascript",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

fn etag(path: &Path, st: &libc::stat) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut hash = Sha256::new();
    hash.update(path.as_os_str().as_bytes());
    hash.update(format!(
        "\0{}\0{}\0{}\0{}.{}",
        st.st_dev, st.st_ino, st.st_size, st.st_mtime, st.st_mtime_nsec
    ));
    let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{}\"", &hex[..32])
}

/// A regular file's bytes, read through its descriptor and capped by what is read.
pub(crate) fn read_capped(file: std::fs::File, max: u64) -> Result<Vec<u8>, Refusal> {
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_refusal(&e))?;
    if bytes.len() as u64 > max {
        return Err(Refusal::TooLarge);
    }
    Ok(bytes)
}

// ---- Git ------------------------------------------------------------------------------------

/// A ref as `rev-parse` may take it: plain ref characters, no option and no range.
pub(crate) fn valid_ref(r: &str) -> bool {
    (1..=256).contains(&r.len())
        && !r.starts_with('-')
        && !r.contains("..")
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/@{}^~-".contains(&b))
}

/// A git command for the folder `dir` under `root`: the repository's configuration cannot run
/// a program, discovery stops at the root, and no hook or external diff runs.
fn git(dir: &Path, root: &Path) -> Option<Command> {
    let mut cmd = hermes_core::git::read_only_git(dir)?;
    let ceiling = root.parent().unwrap_or(root);
    cmd.env("GIT_CEILING_DIRECTORIES", ceiling)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "diff.external="]);
    Some(cmd)
}

fn git_out(dir: &Path, root: &Path, args: &[&str]) -> Option<(bool, String)> {
    let mut cmd = git(dir, root)?;
    cmd.args(args);
    let out = hermes_core::git::output_within_timeout(&mut cmd, GIT_TIMEOUT)?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

fn git_line(dir: &Path, root: &Path, args: &[&str]) -> Option<String> {
    match git_out(dir, root, args)? {
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

fn classify(tag: char, xy: &str, path: &str) -> Value {
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

fn untracked_lines(dir: &Path, rel: &str) -> u64 {
    let Ok(resolved) = resolve_under(dir, rel) else {
        return 0;
    };
    let Ok(Opened::File(file, st)) = open_resolved(&resolved) else {
        return 0;
    };
    if st.st_size as u64 > UNTRACKED_LINE_MAX_BYTES {
        return 0;
    }
    let Ok(data) = read_capped(file, UNTRACKED_LINE_MAX_BYTES) else {
        return 0;
    };
    if data.contains(&0) {
        return 0;
    }
    let lines = data.iter().filter(|&&b| b == b'\n').count() as u64;
    if !data.is_empty() && !data.ends_with(b"\n") {
        lines + 1
    } else {
        lines
    }
}

/// `rel` (a path git printed) under the folder `dir`, refused if it leaves it or is sensitive.
fn resolve_under(dir: &Path, rel: &str) -> Result<Resolved, Refusal> {
    let rel = Path::new(rel);
    if rel.is_absolute()
        || rel.components().any(|c| !matches!(c, Component::Normal(_)))
        || sensitive(rel)
    {
        return Err(Refusal::Outside);
    }
    Ok(Resolved {
        root: dir.to_path_buf(),
        path: dir.join(rel),
    })
}

fn default_branch(dir: &Path, root: &Path) -> Option<String> {
    if let Some(head) = git_line(dir, root, &["rev-parse", "--abbrev-ref", "origin/HEAD"]) {
        if head != "origin/HEAD" {
            return head
                .split_once('/')
                .map(|(_, b)| b.to_owned())
                .or(Some(head));
        }
    }
    [
        "refs/heads/main",
        "refs/heads/master",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ]
    .into_iter()
    .find(|r| {
        git_out(dir, root, &["rev-parse", "--verify", "--quiet", *r]).is_some_and(|(ok, _)| ok)
    })
    .map(|r| r.rsplit('/').next().unwrap_or(r).to_owned())
}

/// Upstream's `repo_status`: `None` for a folder outside any repository.
pub(crate) fn repo_status(dir: &Path, root: &Path) -> Option<Value> {
    let (ok, raw) = git_out(dir, root, &["status", "--porcelain=v2", "--branch", "-z"])?;
    if !ok {
        return None;
    }
    let (mut branch, mut detached, mut ahead, mut behind) = (None, false, 0i64, 0i64);
    for rec in raw.split('\0') {
        if let Some(head) = rec.strip_prefix("# branch.head ") {
            detached = head == "(detached)";
            branch = (!detached).then(|| head.to_owned());
        } else if let Some(ab) = rec.strip_prefix("# branch.ab ") {
            for tok in ab.split_whitespace() {
                if let Some(n) = tok.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = tok.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        }
    }
    let entries = walk_entries(&raw);
    let files: Vec<Value> = entries
        .iter()
        .map(|(t, xy, p)| classify(*t, xy, p))
        .collect();
    let (mut added, mut removed) = (0u64, 0u64);
    if let Some((true, numstat)) = git_out(
        dir,
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "HEAD",
        ],
    ) {
        for line in numstat.lines() {
            let mut parts = line.split('\t');
            added += parts
                .next()
                .and_then(|a| a.parse::<u64>().ok())
                .unwrap_or(0);
            removed += parts
                .next()
                .and_then(|r| r.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }
    added += entries
        .iter()
        .take(UNTRACKED_SCAN_CAP)
        .filter(|(t, ..)| *t == '?')
        .map(|(_, _, p)| untracked_lines(dir, p))
        .sum::<u64>();
    let count = |flag: &str| files.iter().filter(|f| f[flag] == true).count();
    Some(json!({
        "branch": branch,
        "defaultBranch": default_branch(dir, root),
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

// ---- Routes ---------------------------------------------------------------------------------

/// Browser and native callers on this machine only: the remote-mode token caller and any
/// non-loopback peer are refused (a missing peer address refuses too).
pub(crate) fn local_only(caller: Caller, peer: Option<SocketAddr>) -> Result<(), ApiError> {
    let local = peer.is_some_and(|p| match p.ip() {
        std::net::IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map_or(v6.is_loopback(), |v4| v4.is_loopback()),
        ip => ip.is_loopback(),
    });
    match caller {
        Caller::Browser { .. } | Caller::Native if local => Ok(()),
        _ => Err(ApiError(
            StatusCode::FORBIDDEN,
            "File access is local only".into(),
        )),
    }
}

fn admit(
    ctx: &Ctx,
    ext: &Extensions,
    q: &HashMap<String, String>,
) -> Result<(Roots, Option<PathBuf>), ApiError> {
    local_only(
        ctx.caller,
        ext.get::<ConnectInfo<SocketAddr>>().map(|c| c.0),
    )?;
    served_profile(&ctx.api, q.get("profile").map(String::as_str))?;
    let hermes_home = ctx
        .api
        .home
        .clone()
        .unwrap_or_else(hermes_config::hermes_home);
    let user_home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    let mut allowed: Vec<PathBuf> = user_home.iter().cloned().collect();
    allowed.push(default_cwd_for(&hermes_home));
    let refused = [hermes_home.clone(), machine_root(ctx)];
    Ok((Roots::new(&allowed, &refused), user_home))
}

fn path_param(q: &HashMap<String, String>) -> Result<String, ApiError> {
    q.get("path")
        .filter(|p| !p.trim().is_empty())
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "path is required".into()))
}

async fn fs_list(ctx: Ctx, ext: Extensions, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let (roots, home) = admit(&ctx, &ext, &q)?;
    let raw = path_param(&q)?;
    blocking(move || {
        let resolved = match resolve(&raw, &roots, home.as_deref()) {
            Ok(r) => r,
            Err(Refusal::NotFound) => return Ok(json!({"entries": [], "error": "ENOENT"})),
            Err(Refusal::NotADirectory) => return Ok(json!({"entries": [], "error": "ENOTDIR"})),
            Err(Refusal::Denied) => return Ok(json!({"entries": [], "error": "EACCES"})),
            Err(other) => return Err(other.into()),
        };
        let dir = match open_resolved(&resolved) {
            Ok(Opened::Dir(dir)) => dir,
            Ok(Opened::File(..)) | Err(Refusal::NotADirectory | Refusal::NotAFile) => {
                return Ok(json!({"entries": [], "error": "ENOTDIR"}))
            }
            Err(Refusal::NotFound) => return Ok(json!({"entries": [], "error": "ENOENT"})),
            Err(Refusal::Denied) => return Ok(json!({"entries": [], "error": "EACCES"})),
            Err(other) => return Err(other.into()),
        };
        Ok(match list_entries(dir, &resolved.path) {
            Ok((entries, false)) => json!({"entries": entries}),
            Ok((entries, true)) => json!({"entries": entries, "truncated": true}),
            Err(_) => json!({"entries": [], "error": "read-error"}),
        })
    })
    .await
    .map(Json)
}

async fn read_data_url(
    ctx: Ctx,
    ext: Extensions,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let work = async {
        let (roots, home) = admit(&ctx, &ext, &q)?;
        let raw = path_param(&q)?;
        let wanted: Vec<String> = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(',')
                    .map(|t| t.trim().trim_start_matches("W/").to_owned())
                    .collect()
            })
            .unwrap_or_default();
        blocking(move || {
            let resolved = resolve(&raw, &roots, home.as_deref())?;
            let (file, st) = match open_resolved(&resolved)? {
                Opened::File(file, st) => (file, st),
                Opened::Dir(_) => return Err(Refusal::NotAFile.into()),
            };
            if st.st_size as u64 > DATA_URL_MAX_BYTES {
                return Err(Refusal::TooLarge.into());
            }
            let tag = etag(&resolved.path, &st);
            if wanted.iter().any(|w| *w == tag || w == "*") {
                return Ok((tag, None));
            }
            let bytes = read_capped(file, DATA_URL_MAX_BYTES)?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            Ok((
                tag,
                Some(format!(
                    "data:{};base64,{encoded}",
                    mime_type(&resolved.path)
                )),
            ))
        })
        .await
    };
    match work.await {
        Ok((tag, body)) => {
            let mut response = match body {
                Some(url) => Json(json!({"dataUrl": url})).into_response(),
                None => StatusCode::NOT_MODIFIED.into_response(),
            };
            let h = response.headers_mut();
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-cache"),
            );
            if let Ok(tag) = HeaderValue::from_str(&tag) {
                h.insert(header::ETAG, tag);
            }
            response
        }
        Err(err) => err.into_response(),
    }
}

/// The repository folder a git route names: a folder inside a root, resolved as `fs/list` does.
fn repo_dir(raw: &str, roots: &Roots, home: Option<&Path>) -> Result<Resolved, ApiError> {
    let resolved = resolve(raw, roots, home)?;
    match open_resolved(&resolved)? {
        Opened::Dir(_) => Ok(resolved),
        Opened::File(..) => Err(Refusal::NotADirectory.into()),
    }
}

async fn git_status(
    ctx: Ctx,
    ext: Extensions,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let (roots, home) = admit(&ctx, &ext, &q)?;
    let raw = path_param(&q)?;
    blocking(move || {
        let dir = match repo_dir(&raw, &roots, home.as_deref()) {
            Ok(dir) => dir,
            Err(ApiError(status, _))
                if status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST =>
            {
                return Ok(Value::Null)
            }
            Err(err) => return Err(err),
        };
        Ok(repo_status(&dir.path, &dir.root).unwrap_or(Value::Null))
    })
    .await
    .map(Json)
}

async fn rev_parse(
    ctx: Ctx,
    ext: Extensions,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let (roots, home) = admit(&ctx, &ext, &q)?;
    let raw = path_param(&q)?;
    let wanted = q
        .get("ref")
        .filter(|r| !r.is_empty())
        .cloned()
        .unwrap_or_else(|| "HEAD".into());
    if !valid_ref(&wanted) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Not a ref".into()));
    }
    blocking(move || {
        let dir = repo_dir(&raw, &roots, home.as_deref())?;
        let sha = git_line(
            &dir.path,
            &dir.root,
            &["rev-parse", "--verify", "--end-of-options", wanted.as_str()],
        );
        Ok(json!({"sha": sha}))
    })
    .await
    .map(Json)
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
