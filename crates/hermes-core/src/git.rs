//! Git commands for reading a repository whose own configuration may be
//! hostile: nothing in it can make git run a program or fetch.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::subprocess::CommandNoWindowExt;

/// How `workspace_git` keeps git from fetching an object the repository lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LazyFetchGuard {
    /// Git 2.45 and later take `--no-lazy-fetch`.
    Flag,
    /// Git 2.39.1 to 2.44 have no such option, and fetch only from a promisor
    /// remote, so the snapshot is kept for a repository whose config names none.
    NoPromisorRemote,
}

/// The guard the installed git supports, or `None` for a git that cannot
/// produce the snapshot safely.
pub fn lazy_fetch_guard() -> Option<LazyFetchGuard> {
    probe_lazy_fetch_guard(std::ffi::OsStr::new("git"))
}

/// [`lazy_fetch_guard`] for the git program `git`. Each probe has the
/// snapshot's time limit, so a git that never answers gives no guard.
fn probe_lazy_fetch_guard(git: &std::ffi::OsStr) -> Option<LazyFetchGuard> {
    // `git version` reads no repository.
    let version = |option: Option<&str>| {
        output_within_timeout(
            Command::new(git)
                .args(option)
                .arg("version")
                .stdin(std::process::Stdio::null())
                .suppress_windows_console(),
            GIT_SNAPSHOT_TIMEOUT,
        )
        .filter(|output| output.status.success())
    };
    if version(Some("--no-lazy-fetch")).is_some() {
        return Some(LazyFetchGuard::Flag);
    }
    let printed = version(None)?;
    is_git_2_39_1_or_later(&String::from_utf8_lossy(&printed.stdout))
        .then_some(LazyFetchGuard::NoPromisorRemote)
}

/// Whether `git version` printed 2.39.1 or a later version. An older git gets
/// no snapshot. `git status` reads the workspace's `.gitattributes`, and a
/// hostile one can overflow the attribute parser before 2.39.1
/// (CVE-2022-23521). Before 2.31 git also ignores the indexed `GIT_CONFIG_*`
/// variables that carry the filter overrides. A distribution's patched build
/// of an older version is refused too, as its number does not show the patch.
pub fn is_git_2_39_1_or_later(version: &str) -> bool {
    let mut numbers = version
        .trim()
        .strip_prefix("git version ")
        .unwrap_or_default()
        .split(|c: char| c == '.' || c.is_whitespace())
        .map(|number| number.parse::<u32>().ok());
    let (Some(Some(major)), Some(Some(minor)), Some(Some(patch))) =
        (numbers.next(), numbers.next(), numbers.next())
    else {
        return false;
    };
    (major, minor, patch) >= (2, 39, 1)
}

/// A `git` command for reading the workspace that the workspace's own
/// `.git/config` cannot use to run a program: an empty `core.fsmonitor`
/// disables the fsmonitor program on every git version, `log.showSignature`
/// off stops `git log` running `gpg.program`, and `--no-optional-locks` keeps
/// `git status` from writing the index, so no index hook runs. Disable every
/// configured filter too: status can run clean/process commands while hashing
/// tracked files. Reading config names itself does not execute those commands.
/// Git must not fetch an object the repository lacks from a promisor remote
/// either, which runs `core.sshCommand` or a remote helper: `guard` says how.
/// A repository whose own config names a user (see [`names_a_user`]) gets
/// no command.
pub fn workspace_git(root: &Path, guard: LazyFetchGuard) -> Option<Command> {
    workspace_git_with(std::ffi::OsStr::new("git"), root, guard)
}

/// [`workspace_git`] for the git program `git`.
fn workspace_git_with(
    git: &std::ffi::OsStr,
    root: &Path,
    guard: LazyFetchGuard,
) -> Option<Command> {
    // Git follows the repository's includes, expanding their paths and
    // conditions, as soon as it reads its config. So the repository's own
    // config files are first read here, each without its includes, and their
    // includes followed here: a `~name` among them is refused before git
    // reads them.
    if repository_config_names_a_user(git, root)? {
        return None;
    }
    // Every config key git reads for this repository, from every scope and
    // included file. `git config` alone reads the file `GIT_CONFIG` names in
    // their place, so that variable is dropped. The keys are matched here,
    // byte for byte: git's own `--get-regexp` passes over a key holding a byte
    // its locale cannot decode, so a repository could hide a filter or a
    // remote from it.
    let listed = output_within_timeout(
        Command::new(git)
            .arg("-C")
            .arg(root)
            .args(["config", "--includes", "--null", "--show-scope", "--list"])
            .env_remove("GIT_CONFIG")
            .stdin(std::process::Stdio::null())
            .suppress_windows_console(),
        GIT_SNAPSHOT_TIMEOUT,
    )?;
    // Refuse the snapshot if the listing fails, rather than running status
    // with an incomplete set of overrides.
    if !listed.status.success() {
        return None;
    }
    let mut drivers = BTreeMap::new();
    let mut filter_keys = 0;
    // Each entry is its scope, then its key and, unless it has none, its value.
    let mut fields = listed.stdout.split(|byte| *byte == 0);
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
            Some(newline) => (&entry[..newline], Some(&entry[newline + 1..])),
            None => (entry, None),
        };
        // The repository's own config, with the files it includes, cannot
        // make git look up a user. The machine's and the user's config are
        // trusted.
        if matches!(scope, b"local" | b"worktree") && names_a_user(key, value) {
            return None;
        }
        // Git prints a key as `section.name` or `section.subsection.name`, with
        // the section and the name in lower case. Only a subsection holds dots.
        let Some(first) = key.iter().position(|byte| *byte == b'.') else {
            continue;
        };
        let last = key.iter().rposition(|byte| *byte == b'.').unwrap_or(first);
        match (&key[..first], &key[last + 1..]) {
            (b"filter", b"clean" | b"smudge" | b"process" | b"required") => {
                if filter_keys == 256 {
                    return None;
                }
                filter_keys += 1;
                drivers.insert(std::str::from_utf8(&key[..last]).ok()?, ());
            }
            // Any of these keys can make a promisor remote, with or without a
            // remote name, and nothing on the command line switches one off.
            // Whatever its value, without `--no-lazy-fetch` the repository
            // gets no snapshot.
            (b"remote", b"promisor" | b"partialclonefilter") | (b"extensions", b"partialclone")
                if guard == LazyFetchGuard::NoPromisorRemote =>
            {
                return None;
            }
            _ => {}
        }
    }
    let mut cmd = Command::new(git);
    cmd.arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=", "-c", "log.showSignature=false"])
        .arg("--no-optional-locks")
        .stdin(std::process::Stdio::null())
        .suppress_windows_console();
    match guard {
        LazyFetchGuard::Flag => cmd.arg("--no-lazy-fetch"),
        // A second barrier. Git 2.39.4, 2.40.2, 2.41.1, 2.42.2, 2.43.4, 2.44.1
        // and the releases after each refuse to fetch when this is set; the
        // ones before them ignore it.
        LazyFetchGuard::NoPromisorRemote => cmd.env("GIT_NO_LAZY_FETCH", "1"),
    };
    let mut config_count = 0;
    for driver in drivers.keys() {
        for (setting, value) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            // -c splits on '=', which is also valid inside a driver name.
            // Indexed environment entries keep the key separate from its value.
            cmd.env(
                format!("GIT_CONFIG_KEY_{config_count}"),
                format!("{driver}.{setting}"),
            )
            .env(format!("GIT_CONFIG_VALUE_{config_count}"), value);
            config_count += 1;
        }
    }
    cmd.env("GIT_CONFIG_COUNT", config_count.to_string())
        .env_remove("GIT_CONFIG_PARAMETERS");
    Some(cmd)
}

type ConfigEntry = (Vec<u8>, Option<Vec<u8>>);

struct GitDirs {
    git_dir: PathBuf,
    common_dir: PathBuf,
    git_dir_candidates: Vec<PathBuf>,
}

fn includeif_condition(key: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = key.strip_prefix(b"includeif.")?;
    let dot = rest.iter().rposition(|byte| *byte == b'.')?;
    Some((&rest[..dot], &rest[dot + 1..]))
}

/// Whether git would expand the value, or an include's `gitdir` or
/// `worktree` condition, as a path that begins `~name`: git finds that user's
/// home through `getpwnam`, which can ask a directory service about a name
/// the repository chose. Any value counts, as git takes many keys as paths.
/// Git tests an `includeIf` condition whatever the variable under it.
fn names_a_user(key: &[u8], value: Option<&[u8]>) -> bool {
    let condition = includeif_condition(key).and_then(|(condition, _)| {
        [&b"gitdir:"[..], b"gitdir/i:", b"worktree:", b"worktree/i:"]
            .iter()
            .find_map(|prefix| condition.strip_prefix(*prefix))
    });
    value.into_iter().chain(condition).any(|path| {
        // Git 2.52 and later take a path marked optional on path-typed settings.
        let path = path.strip_prefix(b":(optional)").unwrap_or(path);
        path.len() > 1 && path[0] == b'~' && path[1] != b'/'
    })
}

/// Git's limit on nested includes.
const MAX_INCLUDE_DEPTH: usize = 10;

fn trim_trailing_crlf(mut bytes: &[u8]) -> &[u8] {
    while let Some((&last, rest)) = bytes.split_last() {
        if last == b'\n' || last == b'\r' {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

fn bytes_to_path(bytes: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

fn path_to_slash_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().replace('\\', "/").into_bytes()
    }
}

fn parse_git_bool(value: Option<&[u8]>) -> Option<bool> {
    let Some(raw) = value else {
        return Some(true);
    };
    let lower: Vec<u8> = raw.iter().map(u8::to_ascii_lowercase).collect();
    match lower.as_slice() {
        b"true" | b"yes" | b"on" => Some(true),
        b"false" | b"no" | b"off" | b"" => Some(false),
        other => std::str::from_utf8(other)
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .map(|n| n != 0),
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => match out.last() {
                Some(std::path::Component::Normal(_)) => {
                    out.pop();
                }
                Some(std::path::Component::RootDir | std::path::Component::Prefix(_)) => {}
                _ => out.push(comp),
            },
            _ => out.push(comp),
        }
    }
    if out.is_empty() {
        PathBuf::from(".")
    } else {
        out.iter().collect()
    }
}

fn lexical_parent(file: &Path) -> PathBuf {
    match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => lexical_normalize(p),
        _ => PathBuf::new(),
    }
}

fn read_gitfile(file: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(file).ok()?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(file).ok()?;
    let rest = bytes.strip_prefix(b"gitdir: ")?;
    let target = trim_trailing_crlf(rest);
    if target.is_empty() {
        return None;
    }
    let target_path = bytes_to_path(target)?;
    Some(lexical_normalize(
        &file.parent().unwrap_or(Path::new("")).join(target_path),
    ))
}

fn resolve_commondir(git_dir: &Path) -> Option<Option<PathBuf>> {
    let commondir = git_dir.join("commondir");
    match std::fs::metadata(&commondir) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
        Ok(meta) if !meta.is_file() => None,
        Ok(_) => {
            let bytes = std::fs::read(&commondir).ok()?;
            let trimmed = trim_trailing_crlf(&bytes);
            if trimmed.is_empty() {
                return None;
            }
            Some(Some(lexical_normalize(
                &git_dir.join(bytes_to_path(trimmed)?),
            )))
        }
    }
}

fn parse_head_branch(head: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(head).ok()?;
    if meta.file_type().is_symlink() {
        let target = path_to_slash_bytes(&std::fs::read_link(head).ok()?);
        return target.strip_prefix(b"refs/heads/").map(|b| b.to_vec());
    }
    if !meta.is_file() {
        return None;
    }
    let bytes = std::fs::read(head).ok()?;
    let trimmed = trim_trailing_crlf(&bytes);
    let after_ref = trimmed.strip_prefix(b"ref:")?;
    let mut i = 0;
    while i < after_ref.len() && matches!(after_ref[i], b' ' | b'\t') {
        i += 1;
    }
    after_ref[i..]
        .strip_prefix(b"refs/heads/")
        .filter(|b| !b.iter().any(|c| matches!(c, b'\0' | b'\n' | b'\r')))
        .map(|b| b.to_vec())
}

fn is_valid_head(head: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(head) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return std::fs::read_link(head)
            .map(|target| path_to_slash_bytes(&target).starts_with(b"refs/"))
            .unwrap_or(false);
    }
    if !meta.is_file() {
        return false;
    }
    let Ok(bytes) = std::fs::read(head) else {
        return false;
    };
    let first_line = match bytes.iter().position(|&b| b == b'\n') {
        Some(nl) => &bytes[..nl],
        None => &bytes,
    };
    let trimmed = first_line.strip_suffix(b"\r").unwrap_or(first_line);
    if let Some(after_ref) = trimmed.strip_prefix(b"ref:") {
        let mut i = 0;
        while i < after_ref.len() && matches!(after_ref[i], b' ' | b'\t') {
            i += 1;
        }
        let target = &after_ref[i..];
        return target.starts_with(b"refs/")
            && !target.iter().any(|b| matches!(b, b'\0' | b'\n' | b'\r'));
    }
    matches!(trimmed.len(), 40 | 64) && trimmed.iter().all(u8::is_ascii_hexdigit)
}

fn is_git_directory(dir: &Path, root: &Path) -> Option<bool> {
    if !is_valid_head(&dir.join("HEAD")) {
        return Some(false);
    }
    let common = if let Some(gcd) = std::env::var_os("GIT_COMMON_DIR").filter(|d| !d.is_empty()) {
        lexical_normalize(&root.join(gcd))
    } else {
        resolve_commondir(dir)?.unwrap_or_else(|| dir.to_path_buf())
    };
    let has_objects =
        if let Some(obj_dir) = std::env::var_os("GIT_OBJECT_DIRECTORY").filter(|d| !d.is_empty()) {
            lexical_normalize(&root.join(obj_dir)).is_dir()
        } else {
            common.join("objects").is_dir()
        };
    Some(has_objects && common.join("refs").is_dir())
}

fn simplify_windows_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(s) = path.to_str() {
            if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{rest}"));
            }
            if let Some(rest) = s.strip_prefix(r"\\?\") {
                return PathBuf::from(rest);
            }
        }
    }
    path
}

fn canonicalize_clean(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(simplify_windows_verbatim)
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.contains(&candidate) {
        paths.push(candidate);
    }
}

/// The git and common folders that git's discovery finds from `root`.
fn find_git_dirs(root: &Path) -> Option<Option<GitDirs>> {
    let root = canonicalize_clean(root).ok()?;
    let mut candidates = Vec::new();
    let git_dir = if let Some(env_dir) = std::env::var_os("GIT_DIR").filter(|d| !d.is_empty()) {
        let path = lexical_normalize(&root.join(env_dir));
        let resolved = match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => read_gitfile(&path)?,
            _ => path,
        };
        push_unique_path(&mut candidates, resolved.clone());
        resolved
    } else {
        let mut found = None;
        for dir in root.ancestors() {
            let dot_git = dir.join(".git");
            match std::fs::symlink_metadata(&dot_git) {
                Ok(lmeta) => {
                    let is_sym = lmeta.file_type().is_symlink();
                    let Ok(meta) = std::fs::metadata(&dot_git) else {
                        if !is_sym {
                            return None;
                        }
                        if is_git_directory(dir, &root)? {
                            push_unique_path(&mut candidates, dir.to_path_buf());
                            found = Some(dir.to_path_buf());
                            break;
                        }
                        continue;
                    };
                    if meta.is_file() {
                        let resolved = read_gitfile(&dot_git)?;
                        push_unique_path(&mut candidates, resolved.clone());
                        found = Some(resolved);
                        break;
                    }
                    if meta.is_dir() && is_git_directory(&dot_git, &root)? {
                        push_unique_path(&mut candidates, dot_git.clone());
                        found = Some(dot_git);
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
            if is_git_directory(dir, &root)? {
                push_unique_path(&mut candidates, dir.to_path_buf());
                found = Some(dir.to_path_buf());
                break;
            }
        }
        match found {
            Some(gd) => gd,
            None => return Some(None),
        }
    };
    if let Ok(canon) = canonicalize_clean(&git_dir) {
        push_unique_path(&mut candidates, canon);
    }
    let common_dir = if let Some(gcd) = std::env::var_os("GIT_COMMON_DIR").filter(|d| !d.is_empty())
    {
        lexical_normalize(&root.join(gcd))
    } else {
        resolve_commondir(&git_dir)?.unwrap_or_else(|| git_dir.clone())
    };
    Some(Some(GitDirs {
        git_dir,
        common_dir,
        git_dir_candidates: candidates,
    }))
}

fn byte_eq(a: u8, b: u8, icase: bool) -> bool {
    if icase {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn match_posix_class(name: &[u8], ch: u8, icase: bool) -> Option<bool> {
    Some(match name {
        b"alnum" => ch.is_ascii_alphanumeric(),
        b"alpha" => ch.is_ascii_alphabetic(),
        b"blank" => matches!(ch, b' ' | b'\t'),
        b"cntrl" => ch.is_ascii_control(),
        b"digit" => ch.is_ascii_digit(),
        b"graph" => ch.is_ascii_graphic(),
        b"lower" => ch.is_ascii_lowercase() || (icase && ch.is_ascii_uppercase()),
        b"print" => ch == b' ' || ch.is_ascii_graphic(),
        b"punct" => ch.is_ascii_punctuation(),
        b"space" => matches!(ch, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
        b"upper" => ch.is_ascii_uppercase() || (icase && ch.is_ascii_lowercase()),
        b"xdigit" => ch.is_ascii_hexdigit(),
        _ => return None,
    })
}

fn match_char_class(pat: &[u8], ch: u8, icase: bool) -> Option<(bool, usize)> {
    let mut i = 0;
    let negated = if i < pat.len() && matches!(pat[i], b'!' | b'^') {
        i += 1;
        true
    } else {
        false
    };
    let mut matched = false;
    let mut first = true;
    while i < pat.len() {
        if pat[i] == b']' && !first {
            return Some((matched ^ negated, i + 1));
        }
        first = false;
        if pat[i..].starts_with(b"[:") {
            if let Some(end_rel) = pat[i + 2..].windows(2).position(|w| w == b":]") {
                let name = &pat[i + 2..i + 2 + end_rel];
                if match_posix_class(name, ch, icase)? {
                    matched = true;
                }
                i += 4 + end_rel;
                continue;
            }
        }
        let start = if pat[i] == b'\\' && i + 1 < pat.len() {
            i += 1;
            pat[i]
        } else {
            pat[i]
        };
        i += 1;
        if i + 1 < pat.len() && pat[i] == b'-' && pat[i + 1] != b']' {
            i += 1;
            let end = if pat[i] == b'\\' && i + 1 < pat.len() {
                i += 1;
                pat[i]
            } else {
                pat[i]
            };
            i += 1;
            let c = if icase { ch.to_ascii_lowercase() } else { ch };
            let lo = if icase {
                start.to_ascii_lowercase()
            } else {
                start
            };
            let hi = if icase { end.to_ascii_lowercase() } else { end };
            if c >= lo && c <= hi {
                matched = true;
            }
        } else if byte_eq(ch, start, icase) {
            matched = true;
        }
    }
    None
}

fn wildmatch_segment(pat: &[u8], text: &[u8], icase: bool) -> bool {
    let mut dp = vec![false; text.len() + 1];
    dp[0] = true;
    let mut i = 0;
    while i < pat.len() {
        let mut next = vec![false; text.len() + 1];
        if pat[i] == b'*' {
            while i + 1 < pat.len() && pat[i + 1] == b'*' {
                i += 1;
            }
            let mut seen = false;
            for j in 0..=text.len() {
                if dp[j] {
                    seen = true;
                }
                next[j] = seen;
            }
        } else if pat[i] == b'?' {
            next[1..=text.len()].copy_from_slice(&dp[..text.len()]);
        } else if pat[i] == b'\\' {
            if i + 1 >= pat.len() {
                return false;
            }
            i += 1;
            let lit = pat[i];
            for j in 0..text.len() {
                if dp[j] && byte_eq(text[j], lit, icase) {
                    next[j + 1] = true;
                }
            }
        } else if pat[i] == b'[' {
            let Some((_, consumed)) = match_char_class(&pat[i + 1..], 0, icase) else {
                return false;
            };
            for j in 0..text.len() {
                if dp[j]
                    && match_char_class(&pat[i + 1..], text[j], icase).is_some_and(|(hit, _)| hit)
                {
                    next[j + 1] = true;
                }
            }
            i += consumed;
        } else {
            let lit = pat[i];
            for j in 0..text.len() {
                if dp[j] && byte_eq(text[j], lit, icase) {
                    next[j + 1] = true;
                }
            }
        }
        dp = next;
        i += 1;
    }
    dp[text.len()]
}

fn wildmatch_pathname(pat: &[u8], text: &[u8], icase: bool) -> bool {
    let p_segs: Vec<&[u8]> = pat.split(|b| *b == b'/').collect();
    let t_segs: Vec<&[u8]> = text.split(|b| *b == b'/').collect();
    let mut dp = vec![false; t_segs.len() + 1];
    dp[0] = true;
    for (idx, p) in p_segs.iter().enumerate() {
        let mut next = vec![false; t_segs.len() + 1];
        if *p == b"**" {
            let is_last = idx + 1 == p_segs.len();
            let mut seen = false;
            for j in 0..=t_segs.len() {
                if !is_last && dp[j] {
                    seen = true;
                }
                if seen {
                    next[j] = true;
                }
                if is_last && dp[j] {
                    seen = true;
                }
            }
        } else {
            for j in 0..t_segs.len() {
                if dp[j] && wildmatch_segment(p, t_segs[j], icase) {
                    next[j + 1] = true;
                }
            }
        }
        dp = next;
    }
    dp[t_segs.len()]
}

fn git_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    #[cfg(windows)]
    if let (Some(mut drive), Some(path)) = (
        std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty()),
        std::env::var_os("HOMEPATH").filter(|v| !v.is_empty()),
    ) {
        drive.push(path);
        return Some(PathBuf::from(drive));
    }
    std::env::home_dir()
}

fn join_slash_pattern(base: &Path, rest: &[u8]) -> Vec<u8> {
    let mut out = path_to_slash_bytes(base);
    if !out.ends_with(b"/") {
        out.push(b'/');
    }
    out.extend_from_slice(rest);
    out
}

fn gitdir_condition_matches(
    raw_pat: &[u8],
    config_file: &Path,
    candidates: &[PathBuf],
    icase: bool,
) -> bool {
    let patterns: Vec<Vec<u8>> = if let Some(rest) = raw_pat.strip_prefix(b"~/") {
        let Some(home) = git_home_dir() else {
            return true;
        };
        let mut bases = vec![lexical_normalize(&home)];
        if let Ok(canon) = canonicalize_clean(&home) {
            push_unique_path(&mut bases, canon);
        }
        bases
            .into_iter()
            .map(|base| join_slash_pattern(&base, rest))
            .collect()
    } else if let Some(rest) = raw_pat.strip_prefix(b"./") {
        let mut bases = vec![lexical_parent(config_file)];
        let canon = canonical_parent(config_file);
        if !canon.as_os_str().is_empty() {
            push_unique_path(&mut bases, canon);
        }
        bases
            .into_iter()
            .map(|base| join_slash_pattern(&base, rest))
            .collect()
    } else {
        vec![raw_pat.to_vec()]
    };

    let icase = icase || cfg!(windows);
    patterns.iter().any(|pat| {
        let is_abs = pat.starts_with(b"/")
            || (cfg!(windows)
                && pat.len() >= 3
                && pat[0].is_ascii_alphabetic()
                && pat[1] == b':'
                && pat[2] == b'/');
        let mut final_pat = Vec::new();
        if !is_abs {
            final_pat.extend_from_slice(b"**/");
        }
        final_pat.extend_from_slice(pat);
        if final_pat.ends_with(b"/") {
            final_pat.extend_from_slice(b"**");
        }
        candidates
            .iter()
            .any(|gd| wildmatch_pathname(&final_pat, &path_to_slash_bytes(gd), icase))
    })
}

fn canonical_parent(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(Path::new(""));
    if parent.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        canonicalize_clean(parent).unwrap_or_else(|_| parent.to_path_buf())
    }
}

fn list_config_file<'a>(
    git: &std::ffi::OsStr,
    real: &Path,
    cache: &'a mut HashMap<PathBuf, Vec<ConfigEntry>>,
) -> Option<&'a [ConfigEntry]> {
    if !cache.contains_key(real) {
        #[cfg(unix)]
        let no_repo_git_dir = PathBuf::from("/dev/null/no-repository");
        #[cfg(not(unix))]
        let no_repo_git_dir = std::env::current_exe()
            .map(|exe| simplify_windows_verbatim(exe).join("no-repository"))
            .unwrap_or_else(|_| real.join("no-repository"));
        let listed = output_within_timeout(
            Command::new(git)
                .args(["config", "--no-includes", "--null", "--list", "--file"])
                .arg(real)
                // Never allow repo/env discovery when listing a single file.
                .env("GIT_DIR", &no_repo_git_dir)
                .env_remove("GIT_COMMON_DIR")
                .env_remove("GIT_OBJECT_DIRECTORY")
                .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_CONFIG")
                .env_remove("GIT_CONFIG_GLOBAL")
                .env_remove("GIT_CONFIG_SYSTEM")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .env_remove("GIT_CONFIG_COUNT")
                .env_remove("GIT_CONFIG_NOSYSTEM")
                .stdin(Stdio::null())
                .suppress_windows_console(),
            GIT_SNAPSHOT_TIMEOUT,
        )?;
        if !listed.status.success() {
            return None;
        }
        let entries = listed
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .map(|entry| match entry.iter().position(|byte| *byte == b'\n') {
                Some(newline) => (
                    entry[..newline].to_vec(),
                    Some(entry[newline + 1..].to_vec()),
                ),
                None => (entry.to_vec(), None),
            })
            .collect();
        cache.insert(real.to_path_buf(), entries);
    }
    cache.get(real).map(Vec::as_slice)
}

/// Whether the config of the repository git would find from `root`, or a
/// file it actively includes, names a user (see [`names_a_user`]). Each file
/// is listed by `git config --file --no-includes`, isolated from repository
/// discovery and config environment variables; active includes are followed
/// here. `Some(false)` outside a repository; `None` when a file or target is
/// invalid or includes nest past Git's depth limit.
fn repository_config_names_a_user(git: &std::ffi::OsStr, root: &Path) -> Option<bool> {
    let Some(dirs) = find_git_dirs(root)? else {
        return Some(false);
    };
    let head_branch = parse_head_branch(&dirs.git_dir.join("HEAD"));
    let mut cache: HashMap<PathBuf, Vec<ConfigEntry>> = HashMap::new();
    let mut visited: HashSet<(PathBuf, PathBuf)> = HashSet::new();

    // Check `extensions.worktreeConfig` directly in `common_dir/config` (Git
    // ignores `extensions.worktreeConfig` set only via includes).
    let common_config = dirs.common_dir.join("config");
    let mut worktree_config = false;
    let mut reftable_storage = false;
    match std::fs::metadata(&common_config) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
        Ok(meta) if !meta.is_file() => return None,
        Ok(_) => {
            let real = canonicalize_clean(&common_config).ok()?;
            let entries = list_config_file(git, &real, &mut cache)?;
            for (key, value) in entries {
                if key == b"extensions.worktreeconfig" {
                    worktree_config = parse_git_bool(value.as_deref())?;
                } else if key == b"extensions.refstorage"
                    && value
                        .as_deref()
                        .is_some_and(|v| v.eq_ignore_ascii_case(b"reftable"))
                {
                    reftable_storage = true;
                }
            }
        }
    }
    let reftable_active = reftable_storage
        || head_branch.as_deref() == Some(b".invalid")
        || dirs.common_dir.join("reftable").is_dir();

    let mut pending = vec![(common_config, 0usize)];
    if worktree_config {
        pending.push((dirs.git_dir.join("config.worktree"), 0));
    }

    while let Some((file, depth)) = pending.pop() {
        match std::fs::metadata(&file) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
            Ok(meta) if !meta.is_file() => return None,
            Ok(_) => {}
        }
        if depth > MAX_INCLUDE_DEPTH {
            return None;
        }
        let Ok(real) = canonicalize_clean(&file) else {
            return None;
        };
        let parent = lexical_parent(&file);
        if !visited.insert((real.clone(), parent)) {
            continue;
        }
        let entries: Vec<ConfigEntry> = list_config_file(git, &real, &mut cache)?.to_vec();
        for (key, value) in entries {
            if names_a_user(&key, value.as_deref()) {
                return Some(true);
            }
            if key == b"include.path" {
                let value = value.as_deref()?;
                pending.push((include_path(&file, value)?, depth + 1));
            } else if let Some((cond, var)) = includeif_condition(&key) {
                if var != b"path" {
                    continue;
                }
                let value = value.as_deref()?;
                let target = include_path(&file, value)?;
                if let Some(pat) = cond.strip_prefix(b"gitdir:") {
                    if gitdir_condition_matches(pat, &file, &dirs.git_dir_candidates, false) {
                        pending.push((target, depth + 1));
                    }
                } else if let Some(pat) = cond.strip_prefix(b"gitdir/i:") {
                    if gitdir_condition_matches(pat, &file, &dirs.git_dir_candidates, true) {
                        pending.push((target, depth + 1));
                    }
                } else if let Some(pat) = cond.strip_prefix(b"onbranch:") {
                    if reftable_active {
                        pending.push((target, depth + 1));
                    } else if let Some(branch) = &head_branch {
                        let mut full_pat = pat.to_vec();
                        if full_pat.ends_with(b"/") {
                            full_pat.extend_from_slice(b"**");
                        }
                        if wildmatch_pathname(&full_pat, branch, false) {
                            pending.push((target, depth + 1));
                        }
                    }
                } else {
                    // Git speculatively parses `hasconfig:remote.*.url:` targets
                    // (expanding their nested includes and `~name`) even when
                    // unmatched, and future conditions must also fail closed.
                    pending.push((target, depth + 1));
                }
            }
        }
    }
    Some(false)
}

/// The file an include in `file` names, resolved as git resolves it: `~/`
/// under the home folder, or a path relative to `file`'s folder.
fn include_path(file: &Path, value: &[u8]) -> Option<PathBuf> {
    if let Some(mut rest) = value.strip_prefix(b"~/") {
        while let Some((&first, tail)) = rest.split_first() {
            let is_sep = if cfg!(windows) {
                first == b'/' || first == b'\\'
            } else {
                first == b'/'
            };
            if is_sep {
                rest = tail;
            } else {
                break;
            }
        }
        let home = git_home_dir()?;
        if rest.is_empty() {
            return Some(lexical_normalize(&home));
        }
        return Some(lexical_normalize(&home.join(bytes_to_path(rest)?)));
    }
    if value == b"~" {
        return git_home_dir().map(|home| lexical_normalize(&home));
    }
    Some(lexical_normalize(
        &file
            .parent()
            .unwrap_or(Path::new(""))
            .join(bytes_to_path(value)?),
    ))
}

/// A `git` command for reading the repository at `root` that its own
/// configuration cannot use to run a program, as for the snapshot, or `None`
/// when the installed git is older than 2.39.1 or the configuration cannot
/// be read. Run it with [`output_within_timeout`].
pub fn read_only_git(root: &Path) -> Option<Command> {
    workspace_git(root, lazy_fetch_guard()?)
}

/// How long one git call of the snapshot may take. A config key can name a
/// FIFO (`include.path`, `mailmap.file`), and git then waits for a writer
/// that never comes.
pub const GIT_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// The command's exit status and standard output, or `None` when it cannot
/// start, outlasts `timeout` (it is then killed) or leaves its output open
/// past it.
///
/// shortcut: only git itself is killed, so a process git started and left
/// blocked keeps running, and so does the thread reading the output it
/// holds open; killing git's process group would end both.
pub fn output_within_timeout(cmd: &mut Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read whilst waiting, so a full pipe cannot stall git.
    let (read, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = read.send(stdout.read_to_end(&mut buf).map(|_| buf));
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // A process git started can hold the output open after git exits.
    let stdout = output
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()?
        .ok()?;
    Some(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_git_2_39_1_or_later_gets_the_older_git_fallback() {
        for (version, expected) in [
            ("git version 2.39.1\n", true),
            ("git version 2.39.3 (Apple Git-146)\n", true),
            ("git version 2.40.0.rc0\n", true),
            ("git version 2.43.0.windows.1\n", true),
            ("git version 2.44.1\n", true),
            ("git version 3.0.0\n", true),
            ("git version 2.39.0\n", false),
            ("git version 2.39.0.56.g57e2c6ebbe\n", false),
            ("git version 2.34.1\n", false),
            ("git version 2.30.9\n", false),
            ("git version 2.39\n", false),
            ("git version unknown\n", false),
            ("2.44.1\n", false),
            ("", false),
        ] {
            assert_eq!(is_git_2_39_1_or_later(version), expected, "{version:?}");
        }
    }

    /// A `git` that never answers `git version` must not hold up the caller.
    #[cfg(unix)]
    #[test]
    fn a_stalled_git_version_probe_gives_no_guard_in_bounded_time() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("git");
        std::fs::write(&git, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        assert_eq!(probe_lazy_fetch_guard(git.as_os_str()), None);
        assert!(started.elapsed() < 2 * GIT_SNAPSHOT_TIMEOUT + Duration::from_secs(5));
    }

    /// A process git leaves running can hold the output open after git exits.
    #[cfg(unix)]
    #[test]
    fn output_held_open_after_exit_is_given_up_on() {
        let started = Instant::now();
        let output = output_within_timeout(
            Command::new("sh").args(["-c", "sleep 30 & exit 0"]),
            GIT_SNAPSHOT_TIMEOUT,
        );
        assert!(output.is_none());
        assert!(started.elapsed() < GIT_SNAPSHOT_TIMEOUT + Duration::from_secs(5));
    }

    /// A user name no machine has, so a lookup cannot find a home for it.
    const USER: &str = "r2058-croft";

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    /// A new repository at `repo` that gets a command before any change.
    fn new_repo(repo: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        git(repo, &["init", "-q"]);
        assert!(workspace_git(repo, LazyFetchGuard::NoPromisorRemote).is_some());
    }

    /// Git expands a path that begins `~name` through `getpwnam`, which can
    /// ask a directory service for the name the repository chose.
    #[test]
    fn a_user_name_in_the_repository_config_gets_no_command() {
        for (key, value) in [
            ("core.excludesFile".to_string(), format!("~{USER}/x")),
            (
                "core.hooksPath".to_string(),
                format!(":(optional)~{USER}/x"),
            ),
            ("mailmap.file".to_string(), format!("~{USER}")),
            ("include.path".to_string(), format!("~{USER}/x")),
            (format!("includeIf.gitdir:~{USER}/.path"), "x".to_string()),
            (format!("includeIf.gitdir/i:~{USER}/.path"), "x".to_string()),
            // Git tests the condition before it looks at the variable.
            (format!("includeIf.gitdir:~{USER}/.note"), "x".to_string()),
            (format!("includeIf.gitdir/i:~{USER}/.note"), "x".to_string()),
            (format!("includeIf.worktree:~{USER}/.path"), "x".to_string()),
            (
                format!("includeIf.worktree/i:~{USER}/.note"),
                "x".to_string(),
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            new_repo(&repo);
            git(&repo, &["config", &key, &value]);
            assert!(
                workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_none(),
                "{key} = {value}"
            );
        }
    }

    #[test]
    fn the_home_directory_is_not_a_user_name() {
        for value in ["~/x", "~"] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            new_repo(&repo);
            git(&repo, &["config", "core.excludesFile", value]);
            assert!(
                workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some(),
                "{value}"
            );
        }
    }

    /// Git reports a value from an included file under the scope of the
    /// file that includes it.
    #[test]
    fn a_user_name_in_an_included_file_gets_no_command() {
        for (included, kept) in [
            (format!("[core]\n\texcludesFile = ~{USER}/x\n"), false),
            ("[core]\n\texcludesFile = ~/x\n".to_string(), true),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            new_repo(&repo);
            std::fs::write(repo.join("shared.config"), &included).unwrap();
            // Relative to the directory of the file that includes it, `.git`.
            git(&repo, &["config", "include.path", "../shared.config"]);
            assert_eq!(
                workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some(),
                kept,
                "{included}"
            );
        }
    }

    /// A linked worktree reads the main repository's config.
    #[test]
    fn a_user_name_in_a_linked_worktree_s_config_gets_no_command() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        git(
            &repo,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        let linked = tmp.path().join("linked");
        git(&repo, &["worktree", "add", "-q", linked.to_str().unwrap()]);
        assert!(workspace_git(&linked, LazyFetchGuard::NoPromisorRemote).is_some());
        git(
            &repo,
            &["config", "core.excludesFile", &format!("~{USER}/x")],
        );
        assert!(workspace_git(&linked, LazyFetchGuard::NoPromisorRemote).is_none());
    }

    /// With `extensions.worktreeConfig`, each worktree has a config of its
    /// own, which git lists under the `worktree` scope.
    #[test]
    fn a_user_name_in_a_worktree_s_own_config_gets_no_command() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        git(
            &repo,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        git(&repo, &["config", "extensions.worktreeConfig", "true"]);
        let linked = tmp.path().join("linked");
        git(&repo, &["worktree", "add", "-q", linked.to_str().unwrap()]);
        git(
            &linked,
            &[
                "config",
                "--worktree",
                "core.excludesFile",
                &format!("~{USER}/x"),
            ],
        );
        assert!(workspace_git(&linked, LazyFetchGuard::NoPromisorRemote).is_none());
        assert!(workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some());
    }

    /// A `git` that records each call's arguments and runs the real one.
    #[cfg(unix)]
    fn recording_git(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("calls.log");
        let spy = dir.join("git");
        std::fs::write(
            &spy,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec git \"$@\"\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&spy, std::fs::Permissions::from_mode(0o755)).unwrap();
        (spy, log)
    }

    /// The repository's own config, and the files it includes, are read
    /// without git following an include, so git never expands a `~name`
    /// include path or condition the repository chose.
    #[cfg(unix)]
    #[test]
    fn git_does_not_follow_the_repository_s_includes_before_they_are_checked() {
        for (key, value) in [
            ("include.path".to_string(), format!("~{USER}/x")),
            (format!("includeIf.gitdir:~{USER}/.path"), "x".to_string()),
            // Reached through a plain include of a file that names the user.
            ("include.path".to_string(), "../shared.config".to_string()),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            new_repo(&repo);
            std::fs::write(
                repo.join("shared.config"),
                format!("[include]\n\tpath = ~{USER}/y\n"),
            )
            .unwrap();
            git(&repo, &["config", &key, &value]);
            let (spy, log) = recording_git(tmp.path());

            let command =
                workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote);

            let calls = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(command.is_none(), "{key} = {value}");
            assert!(
                !calls.lines().any(|call| call.contains("--includes")),
                "{key} = {value}: git followed the includes: {calls}"
            );
        }
    }

    /// A clean repository's includes, relative and nested, are checked and
    /// the command is still given.
    #[cfg(unix)]
    #[test]
    fn a_repository_with_clean_includes_still_gets_a_command() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        std::fs::write(repo.join("a.config"), "[include]\n\tpath = b.config\n").unwrap();
        std::fs::write(repo.join("b.config"), "[core]\n\texcludesFile = ~/x\n").unwrap();
        git(&repo, &["config", "include.path", "../a.config"]);
        let (spy, _log) = recording_git(tmp.path());
        let nested = repo.join("src");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(
            workspace_git_with(spy.as_os_str(), &nested, LazyFetchGuard::NoPromisorRemote)
                .is_some()
        );
    }

    /// Each included file is listed once, however often it is included, so
    /// a repository cannot multiply the listings.
    #[cfg(unix)]
    #[test]
    fn an_include_named_many_times_is_listed_once() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        let fan = "[include]\n".to_string() + &"\tpath = next.config\n".repeat(8);
        std::fs::write(repo.join("first.config"), &fan).unwrap();
        std::fs::write(repo.join("next.config"), "[core]\n\tbare = false\n").unwrap();
        for _ in 0..8 {
            git(
                &repo,
                &["config", "--add", "include.path", "../first.config"],
            );
        }
        let (spy, log) = recording_git(tmp.path());

        let command = workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote);

        assert!(command.is_some());
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let listings = calls
            .lines()
            .filter(|call| call.contains("--no-includes"))
            .count();
        assert_eq!(listings, 3, "{listings} listings: {calls}");
    }

    fn run_in_child_env(test_name: &str, envs: &[(&str, &std::ffi::OsStr)]) {
        if std::env::var_os("HERMES_GIT_TEST_CHILD").is_some() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let mut cmd = Command::new(exe);
        cmd.args(["--exact", test_name, "--nocapture"])
            .env("HERMES_GIT_TEST_CHILD", "1");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "child test {test_name} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    fn preloaded_git(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let c_path = dir.join("pwprobe.c");
        let so_path = dir.join("libpwprobe.so");
        let pw_log = dir.join("pw.log");
        let calls_log = dir.join("calls.log");
        let spy = dir.join("git");
        std::fs::write(
            &c_path,
            r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <pwd.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void rec(const char *kind, const char *name) {
    const char *path = getenv("PWPROBE_LOG");
    if (!path || !name) return;
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (fd >= 0) {
        write(fd, kind, strlen(kind));
        write(fd, ":", 1);
        write(fd, name, strlen(name));
        write(fd, "\n", 1);
        close(fd);
    }
}

struct passwd *getpwnam(const char *name) {
    rec("getpwnam", name);
    struct passwd *(*orig)(const char *) = dlsym(RTLD_NEXT, "getpwnam");
    return orig(name);
}

int getpwnam_r(const char *name, struct passwd *pwd, char *buf, size_t buflen, struct passwd **result) {
    rec("getpwnam_r", name);
    int (*orig)(const char *, struct passwd *, char *, size_t, struct passwd **) = dlsym(RTLD_NEXT, "getpwnam_r");
    return orig(name, pwd, buf, buflen, result);
}
"#,
        )
        .unwrap();
        let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
        let compiled = Command::new(cc)
            .args(["-shared", "-fPIC", "-O2"])
            .arg(&c_path)
            .arg("-o")
            .arg(&so_path)
            .arg("-ldl")
            .output()
            .unwrap();
        assert!(compiled.status.success(), "cc failed: {compiled:?}");
        std::fs::write(
            &spy,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nPWPROBE_LOG='{}' LD_PRELOAD='{}' exec git \"$@\"\n",
                calls_log.display(),
                pw_log.display(),
                so_path.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&spy, std::fs::Permissions::from_mode(0o755)).unwrap();
        (spy, calls_log, pw_log)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn directory_include_and_git_common_dir_trigger_zero_getpwnam_lookups() {
        let tmp = tempfile::tempdir().unwrap();
        let (spy, calls_log, pw_log) = preloaded_git(tmp.path());

        // Verify instrumentation actually observes getpwnam when git expands ~USER.
        let canary = tmp.path().join("canary");
        new_repo(&canary);
        git(
            &canary,
            &["config", "core.excludesFile", &format!("~{USER}/x")],
        );
        let _ = Command::new(&spy)
            .args(["-C"])
            .arg(&canary)
            .arg("status")
            .output()
            .unwrap();
        let canary_pw = std::fs::read_to_string(&pw_log).unwrap_or_default();
        assert!(
            canary_pw.contains(USER),
            "probe missed getpwnam for {USER}: {canary_pw:?}"
        );
        std::fs::write(&pw_log, "").unwrap();
        std::fs::write(&calls_log, "").unwrap();

        // S1: include names a directory `D` holding `D/no-repository`.
        let s1_repo = tmp.path().join("s1_repo");
        new_repo(&s1_repo);
        let inc_dir = s1_repo.join("dir_inc");
        let trapped = inc_dir.join("no-repository");
        std::fs::create_dir_all(&trapped).unwrap();
        git(&trapped, &["init", "-q", "--bare"]);
        git(
            &trapped,
            &["config", "include.path", &format!("~{USER}/s1")],
        );
        git(&s1_repo, &["config", "include.path", "../dir_inc"]);
        assert!(
            workspace_git_with(spy.as_os_str(), &s1_repo, LazyFetchGuard::NoPromisorRemote)
                .is_none()
        );
        let s1_calls = std::fs::read_to_string(&calls_log).unwrap_or_default();
        let s1_pw = std::fs::read_to_string(&pw_log).unwrap_or_default();
        assert!(!s1_calls.contains("--includes"), "{s1_calls}");
        assert!(!s1_pw.contains(USER), "S1 leaked getpwnam lookup: {s1_pw}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn git_common_dir_triggers_zero_getpwnam_lookups() {
        if std::env::var_os("HERMES_GIT_TEST_CHILD").is_none() {
            let tmp = tempfile::tempdir().unwrap();
            let evil_common = tmp.path().join("evil-common");
            new_repo(&evil_common);
            let _ = std::fs::rename(evil_common.join(".git"), tmp.path().join("gcd"));
            let gcd = tmp.path().join("gcd");
            git(
                &gcd,
                &[
                    "--git-dir",
                    gcd.to_str().unwrap(),
                    "config",
                    "core.excludesFile",
                    &format!("~{USER}/gcd"),
                ],
            );
            run_in_child_env(
                "git::tests::git_common_dir_triggers_zero_getpwnam_lookups",
                &[("GIT_COMMON_DIR", gcd.as_os_str())],
            );
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let (spy, calls_log, pw_log) = preloaded_git(tmp.path());
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .env_remove("GIT_COMMON_DIR")
            .status()
            .unwrap()
            .success());
        assert!(
            workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote).is_none()
        );
        let calls = std::fs::read_to_string(&calls_log).unwrap_or_default();
        let pw = std::fs::read_to_string(&pw_log).unwrap_or_default();
        assert!(!calls.contains("--includes"), "{calls}");
        assert!(!pw.contains(USER), "S8 leaked getpwnam lookup: {pw}");

        // B5: `.git` holding only HEAD (no objects/, refs/, or commondir) still
        // resolves `GIT_COMMON_DIR` in git and must be refused without `getpwnam`.
        let min_repo = tmp.path().join("minimal_head_only");
        std::fs::create_dir_all(min_repo.join(".git")).unwrap();
        std::fs::write(min_repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(
            workspace_git_with(spy.as_os_str(), &min_repo, LazyFetchGuard::NoPromisorRemote)
                .is_none()
        );
        let calls = std::fs::read_to_string(&calls_log).unwrap_or_default();
        let pw = std::fs::read_to_string(&pw_log).unwrap_or_default();
        assert!(!calls.contains("--includes"), "{calls}");
        assert!(!pw.contains(USER), "B5 leaked getpwnam lookup: {pw}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_with_shared_include_do_not_hide_a_user_name() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        let a = repo.join("a");
        let b = repo.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let shared = repo.join("shared.conf");
        std::fs::write(&shared, "[include]\n\tpath = child.conf\n").unwrap();
        std::os::unix::fs::symlink(&shared, a.join("config")).unwrap();
        std::os::unix::fs::symlink(&shared, b.join("config")).unwrap();
        std::fs::write(
            a.join("child.conf"),
            format!("[core]\n\texcludesFile = ~{USER}/alias\n"),
        )
        .unwrap();
        std::fs::write(b.join("child.conf"), "[core]\n\tbare = false\n").unwrap();
        git(&repo, &["config", "--add", "include.path", "../a/config"]);
        git(&repo, &["config", "--add", "include.path", "../b/config"]);
        let (spy, log) = recording_git(tmp.path());
        assert!(
            workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote).is_none()
        );
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!calls.contains("--includes"), "{calls}");

        // Clean counterpart keeps command and parses shared once.
        std::fs::write(a.join("child.conf"), "[core]\n\tbare = false\n").unwrap();
        std::fs::write(&log, "").unwrap();
        assert!(
            workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote).is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn literal_optional_prefix_in_include_path_is_not_stripped() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        std::fs::write(
            repo.join(":(optional)inc"),
            format!("[core]\n\texcludesFile = ~{USER}/opt\n"),
        )
        .unwrap();
        git(&repo, &["config", "include.path", "../:(optional)inc"]);
        let (spy, log) = recording_git(tmp.path());
        assert!(
            workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote).is_none()
        );
        assert!(!std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("--includes"));

        std::fs::write(repo.join(":(optional)inc"), "[user]\n\tname = clean\n").unwrap();
        assert!(
            workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote).is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn tilde_double_slash_include_stays_under_home() {
        if std::env::var_os("HERMES_GIT_TEST_CHILD").is_none() {
            let home = tempfile::tempdir().unwrap();
            run_in_child_env(
                "git::tests::tilde_double_slash_include_stays_under_home",
                &[("HOME", home.path().as_os_str())],
            );
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        let home = std::env::home_dir().unwrap();
        let inc = home.join("inc");
        std::fs::write(&inc, format!("[core]\n\texcludesFile = ~{USER}/home\n")).unwrap();
        git(&repo, &["config", "include.path", "~//inc"]);
        let (spy, log) = recording_git(tmp.path());
        assert!(
            workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote).is_none()
        );
        assert!(!std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("--includes"));

        std::fs::write(&inc, "[user]\n\tname = clean\n").unwrap();
        assert!(
            workspace_git_with(spy.as_os_str(), &repo, LazyFetchGuard::NoPromisorRemote).is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_and_whitespace_gitdir_and_non_utf8_paths_are_handled() {
        use std::os::unix::ffi::OsStrExt;
        // S5: root symlink to physical repo/subdir
        let tmp = tempfile::tempdir().unwrap();
        let phys = tmp.path().join("phys");
        new_repo(&phys);
        let sub = phys.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        git(
            &phys,
            &["config", "core.excludesFile", &format!("~{USER}/sym")],
        );
        let link = tmp.path().join("link_sub");
        std::os::unix::fs::symlink(&sub, &link).unwrap();
        let (spy, log) = recording_git(tmp.path());
        assert!(
            workspace_git_with(spy.as_os_str(), &link, LazyFetchGuard::NoPromisorRemote).is_none()
        );
        assert!(!std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("--includes"));

        // S6: trailing space & embedded newline in gitfile target
        for target_bytes in [&b"gd_space "[..], &b"gd\nnl"[..]] {
            let t2 = tempfile::tempdir().unwrap();
            let wt = t2.path().join("wt");
            let gd = wt.join(std::ffi::OsStr::from_bytes(target_bytes));
            std::fs::create_dir_all(gd.join("objects")).unwrap();
            std::fs::create_dir_all(gd.join("refs")).unwrap();
            std::fs::write(gd.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(
                gd.join("config"),
                format!("[core]\n\texcludesFile = ~{USER}/gf\n"),
            )
            .unwrap();
            let mut gf = b"gitdir: ".to_vec();
            gf.extend_from_slice(target_bytes);
            gf.push(b'\n');
            std::fs::write(wt.join(".git"), &gf).unwrap();
            let (spy2, log2) = recording_git(t2.path());
            assert!(
                workspace_git_with(spy2.as_os_str(), &wt, LazyFetchGuard::NoPromisorRemote)
                    .is_none()
            );
            assert!(!std::fs::read_to_string(&log2)
                .unwrap_or_default()
                .contains("--includes"));
            std::fs::write(gd.join("config"), "[core]\n\tbare = false\n").unwrap();
            assert!(
                workspace_git_with(spy2.as_os_str(), &wt, LazyFetchGuard::NoPromisorRemote)
                    .is_some()
            );
        }

        // S7 + S13: non-UTF-8 commondir, gitfile target, and include filename (unsafe + clean)
        let t3 = tempfile::tempdir().unwrap();
        let wt3 = t3.path().join("wt3");
        let bad_gd = t3.path().join(std::ffi::OsStr::from_bytes(b"gd_\xff_dir"));
        let bad_common = t3
            .path()
            .join(std::ffi::OsStr::from_bytes(b"common_\xfe_dir"));
        std::fs::create_dir_all(&wt3).unwrap();
        std::fs::create_dir_all(&bad_gd).unwrap();
        std::fs::create_dir_all(bad_common.join("objects")).unwrap();
        std::fs::create_dir_all(bad_common.join("refs")).unwrap();
        std::fs::write(bad_gd.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let mut cm = b"../common_\xfe_dir\n".to_vec();
        std::fs::write(bad_gd.join("commondir"), &cm).unwrap();
        cm.clear();
        let mut gf3 = b"gitdir: ../gd_\xff_dir\n".to_vec();
        std::fs::write(wt3.join(".git"), &gf3).unwrap();
        gf3.clear();
        let bad_inc = t3
            .path()
            .join(std::ffi::OsStr::from_bytes(b"inc_\xfd.conf"));
        std::fs::write(&bad_inc, format!("[core]\n\texcludesFile = ~{USER}/raw\n")).unwrap();
        std::fs::write(
            bad_common.join("config"),
            b"[core]\n\trepositoryformatversion = 0\n[include]\n\tpath = ../inc_\xfd.conf\n",
        )
        .unwrap();
        let (spy3, log3) = recording_git(t3.path());
        assert!(
            workspace_git_with(spy3.as_os_str(), &wt3, LazyFetchGuard::NoPromisorRemote).is_none()
        );
        assert!(!std::fs::read_to_string(&log3)
            .unwrap_or_default()
            .contains("--includes"));
        std::fs::write(&bad_inc, "[user]\n\tname = RawBytes\n").unwrap();
        let mut cmd = workspace_git_with(spy3.as_os_str(), &wt3, LazyFetchGuard::NoPromisorRemote)
            .expect("clean non-UTF-8 paths should succeed");
        let out = cmd.args(["rev-parse", "--git-dir"]).output().unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    #[test]
    fn explicit_git_dir_gitfile_is_resolved_on_all_platforms() {
        // S9: GIT_DIR pointing to a gitfile containing an unsafe config.
        if std::env::var_os("HERMES_GIT_TEST_CHILD").is_none() {
            let tmp = tempfile::tempdir().unwrap();
            let sub = tmp.path().join("sub");
            let real_gd = tmp.path().join("real_gd");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::create_dir_all(real_gd.join("objects")).unwrap();
            std::fs::create_dir_all(real_gd.join("refs")).unwrap();
            std::fs::write(real_gd.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(
                real_gd.join("config"),
                format!("[core]\n\texcludesFile = ~{USER}/s9\n"),
            )
            .unwrap();
            std::fs::write(sub.join(".git"), "gitdir: ../real_gd\n").unwrap();
            run_in_child_env(
                "git::tests::explicit_git_dir_gitfile_is_resolved_on_all_platforms",
                &[
                    ("HERMES_S9_SUB", sub.as_os_str()),
                    ("GIT_DIR", ".git".as_ref()),
                ],
            );
            return;
        }
        let sub = std::path::PathBuf::from(std::env::var_os("HERMES_S9_SUB").unwrap());
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let (spy, log) = recording_git(tmp.path());
            assert!(
                workspace_git_with(spy.as_os_str(), &sub, LazyFetchGuard::NoPromisorRemote)
                    .is_none()
            );
            assert!(!std::fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("--includes"));
        }
        #[cfg(not(unix))]
        assert!(workspace_git(&sub, LazyFetchGuard::NoPromisorRemote).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn decoy_dotgit_does_not_shadow_unsafe_parent_and_unborn_symlink_and_obj_dir_work() {
        if std::env::var_os("HERMES_GIT_TEST_CHILD").is_none() {
            let tmp = tempfile::tempdir().unwrap();
            // Decoy .git inside parent unsafe repo
            let parent = tmp.path().join("parent");
            new_repo(&parent);
            git(
                &parent,
                &["config", "core.excludesFile", &format!("~{USER}/decoy")],
            );
            let child = parent.join("child");
            std::fs::create_dir_all(child.join(".git/objects")).unwrap();
            std::fs::create_dir_all(child.join(".git/refs")).unwrap();
            std::fs::write(child.join(".git/HEAD"), "not a valid head\n").unwrap();
            let (spy, log) = recording_git(tmp.path());
            assert!(
                workspace_git_with(spy.as_os_str(), &child, LazyFetchGuard::NoPromisorRemote)
                    .is_none()
            );
            assert!(!std::fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("--includes"));

            // Unborn symlink HEAD + GIT_OBJECT_DIRECTORY without objects/
            let sym_repo = tmp.path().join("sym_repo");
            let ext_obj = tmp.path().join("ext_obj");
            std::fs::create_dir_all(sym_repo.join(".git/refs")).unwrap();
            std::fs::create_dir_all(&ext_obj).unwrap();
            std::os::unix::fs::symlink("refs/heads/main", sym_repo.join(".git/HEAD")).unwrap();
            std::fs::write(sym_repo.join(".git/config"), "[core]\n\tbare = false\n").unwrap();
            run_in_child_env(
                "git::tests::decoy_dotgit_does_not_shadow_unsafe_parent_and_unborn_symlink_and_obj_dir_work",
                &[
                    ("HERMES_S10_REPO", sym_repo.as_os_str()),
                    ("GIT_OBJECT_DIRECTORY", ext_obj.as_os_str()),
                ],
            );
            return;
        }
        let repo = std::path::PathBuf::from(std::env::var_os("HERMES_S10_REPO").unwrap());
        assert!(workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some());
    }

    #[test]
    fn compatibility_inactive_worktree_false_include_if_and_many_includes() {
        // S11: inactive malformed config.worktree + depth-11 behind false includeIf
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("compat");
        new_repo(&repo);
        git(&repo, &["checkout", "-q", "-b", "feat/ok"]);
        std::fs::write(repo.join(".git/config.worktree"), "[broken\n").unwrap();
        for i in 0..12 {
            let p = repo.join(format!("deep_{i}.conf"));
            if i == 11 {
                std::fs::write(&p, "[user]\n\tname = deep\n").unwrap();
            } else {
                std::fs::write(&p, format!("[include]\n\tpath = deep_{}.conf\n", i + 1)).unwrap();
            }
        }
        git(
            &repo,
            &[
                "config",
                "includeIf.gitdir:/definitely-other/.path",
                "../deep_0.conf",
            ],
        );
        git(
            &repo,
            &["config", "includeIf.onbranch:other/.path", "../deep_0.conf"],
        );
        assert!(workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some());

        // S12: 70 clean distinct sibling includes succeed and git runs
        for i in 0..70 {
            let f = repo.join(format!("sib_{i}.conf"));
            std::fs::write(&f, format!("[user]\n\tname = u{i}\n")).unwrap();
            git(
                &repo,
                &[
                    "config",
                    "--add",
                    "include.path",
                    &format!("../sib_{i}.conf"),
                ],
            );
        }
        let mut cmd = workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).expect(">64 includes");
        assert!(cmd
            .args(["status", "--short"])
            .output()
            .unwrap()
            .status
            .success());

        // Unmatched `hasconfig:remote.*.url:` still causes Git to parse nested
        // includes in its target, so `~USER` there must still be refused.
        std::fs::write(
            repo.join("hc_bad.conf"),
            format!("[include]\n\tpath = ~{USER}/hc\n"),
        )
        .unwrap();
        git(
            &repo,
            &[
                "config",
                "includeIf.hasconfig:remote.*.url:https://never.invalid/**.path",
                "../hc_bad.conf",
            ],
        );
        assert!(workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_none());
    }

    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success())
        }
    }

    fn assert_refused_without_includes(repo: &Path, tmp: &Path) {
        #[cfg(unix)]
        {
            let (spy, log) = recording_git(tmp);
            assert!(
                workspace_git_with(spy.as_os_str(), repo, LazyFetchGuard::NoPromisorRemote)
                    .is_none(),
                "expected refusal for {}",
                repo.display()
            );
            assert!(
                !std::fs::read_to_string(&log)
                    .unwrap_or_default()
                    .contains("--includes"),
                "git --includes ran on {}",
                repo.display()
            );
        }
        #[cfg(not(unix))]
        {
            let _ = tmp;
            assert!(
                workspace_git(repo, LazyFetchGuard::NoPromisorRemote).is_none(),
                "expected refusal for {}",
                repo.display()
            );
        }
    }

    #[test]
    fn include_if_edge_cases_and_posix_classes_prevent_preflight_bypasses() {
        // B2 unit checks: all 12 POSIX classes, negation, icase [:upper:]/[:lower:], unknown.
        for (pat, yes, no) in [
            (b"[[:alnum:]]" as &[u8], b'Z', b'-'),
            (b"[[:alpha:]]", b'a', b'5'),
            (b"[[:blank:]]", b'\t', b'x'),
            (b"[[:cntrl:]]", b'\x07', b' '),
            (b"[[:digit:]]", b'9', b'a'),
            (b"[[:graph:]]", b'!', b' '),
            (b"[[:lower:]]", b'z', b'Z'),
            (b"[[:print:]]", b' ', b'\n'),
            (b"[[:punct:]]", b'/', b'a'),
            (b"[[:space:]]", b'\x0b', b'x'),
            (b"[[:upper:]]", b'A', b'a'),
            (b"[[:xdigit:]]", b'f', b'g'),
            (b"[![:digit:]]", b'a', b'4'),
        ] {
            assert!(wildmatch_segment(pat, &[yes], false));
            assert!(!wildmatch_segment(pat, &[no], false));
        }
        assert!(wildmatch_segment(b"[[:upper:]]", b"a", true));
        assert!(wildmatch_segment(b"[[:lower:]]", b"A", true));
        assert!(!wildmatch_segment(b"[[:unknown:]]", b"a", false));

        let tmp = tempfile::tempdir().unwrap();
        let bad_user = format!("[core]\n\texcludesFile = ~{USER}/b\n");

        // B1a: `../inc.conf` with `includeIf.gitdir:./`
        let b1a = tmp.path().join("b1a");
        new_repo(&b1a);
        std::fs::write(b1a.join("evil1a.conf"), &bad_user).unwrap();
        std::fs::write(
            b1a.join("inc1a.conf"),
            "[includeIf \"gitdir:./\"]\n\tpath = evil1a.conf\n",
        )
        .unwrap();
        git(&b1a, &["config", "include.path", "../inc1a.conf"]);
        assert_refused_without_includes(&b1a, tmp.path());

        // B1b: linked worktree `commondir` (`../..`) with `includeIf.gitdir:./worktrees/`
        let b1b = tmp.path().join("b1b");
        new_repo(&b1b);
        git(
            &b1b,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        let wt = tmp.path().join("b1b_wt");
        git(&b1b, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        std::fs::write(b1b.join("evil1b.conf"), &bad_user).unwrap();
        git(
            &b1b,
            &[
                "config",
                "includeIf.gitdir:./worktrees/.path",
                "../evil1b.conf",
            ],
        );
        assert_refused_without_includes(&wt, tmp.path());

        // B2: POSIX classes in `onbranch:`, `gitdir:`, and `gitdir/i:`
        let r5 = tmp.path().join("r5");
        new_repo(&r5);
        std::fs::write(r5.join("evil2.conf"), &bad_user).unwrap();
        git(&r5, &["checkout", "-q", "-b", "main"]);
        git(
            &r5,
            &[
                "config",
                "includeIf.onbranch:[[:alpha:]]*.path",
                "../evil2.conf",
            ],
        );
        assert_refused_without_includes(&r5, tmp.path());

        let r5b = tmp.path().join("b5");
        new_repo(&r5b);
        std::fs::write(r5b.join("evil2b.conf"), &bad_user).unwrap();
        git(&r5b, &["checkout", "-q", "-b", "1"]);
        git(
            &r5b,
            &[
                "config",
                "includeIf.gitdir:**/[[:alpha:]][[:digit:]]/.path",
                "../evil2b.conf",
            ],
        );
        assert_refused_without_includes(&r5b, tmp.path());

        let r5c = tmp.path().join("abc");
        new_repo(&r5c);
        std::fs::write(r5c.join("evil2c.conf"), &bad_user).unwrap();
        git(
            &r5c,
            &[
                "config",
                "includeIf.gitdir/i:**/[[:upper:]]*/.path",
                "../evil2c.conf",
            ],
        );
        assert_refused_without_includes(&r5c, tmp.path());

        // B3: escaped `\[` / `\]` on Windows and Unix
        let b3 = tmp.path().join("my[1]repo");
        new_repo(&b3);
        std::fs::write(b3.join("evil3.conf"), &bad_user).unwrap();
        git(
            &b3,
            &[
                "config",
                "includeIf.gitdir:**/my\\[1\\]repo/.path",
                "../evil3.conf",
            ],
        );
        assert_refused_without_includes(&b3, tmp.path());

        // B4: reftable (`ref: refs/heads/.invalid`) + `includeIf.onbranch:`
        let b4 = tmp.path().join("b4_rt");
        let rt_ok = Command::new("git")
            .args(["init", "-q", "--ref-format=reftable", "-b", "main"])
            .arg(&b4)
            .output()
            .is_ok_and(|o| o.status.success());
        if !rt_ok {
            new_repo(&b4);
            std::fs::create_dir_all(b4.join(".git/reftable")).unwrap();
            std::fs::write(b4.join(".git/HEAD"), "ref: refs/heads/.invalid\n").unwrap();
            git(&b4, &["config", "extensions.refStorage", "reftable"]);
        }
        std::fs::write(b4.join("evil4.conf"), &bad_user).unwrap();
        git(
            &b4,
            &["config", "includeIf.onbranch:main.path", "../evil4.conf"],
        );
        assert_refused_without_includes(&b4, tmp.path());

        // N8: multiline HEAD decoy inside unsafe parent still stops ancestor walk
        let parent = tmp.path().join("parent_ml");
        new_repo(&parent);
        git(
            &parent,
            &["config", "core.excludesFile", &format!("~{USER}/ml")],
        );
        let child = parent.join("child");
        new_repo(&child);
        std::fs::write(child.join(".git/HEAD"), "ref: refs/heads/main\nsecond\n").unwrap();
        assert!(workspace_git(&child, LazyFetchGuard::NoPromisorRemote).is_some());

        // S7: `dir_link -> real_sub` must not shadow `real_sub/inc.conf` in `visited`
        let real_sub = tmp.path().join("real_sub");
        let inner = real_sub.join("inner");
        new_repo(&inner);
        std::fs::write(real_sub.join("hit7.conf"), &bad_user).unwrap();
        std::fs::write(
            real_sub.join("inc7.conf"),
            "[includeIf \"gitdir:./\"]\n\tpath = hit7.conf\n",
        )
        .unwrap();
        let dir_link = tmp.path().join("dir_link");
        if link_dir(&real_sub, &dir_link) {
            git(
                &inner,
                &[
                    "config",
                    "--add",
                    "include.path",
                    "../../../dir_link/inc7.conf",
                ],
            );
            assert_refused_without_includes(&inner, tmp.path());
            git(
                &inner,
                &["config", "--add", "include.path", "../../inc7.conf"],
            );
            assert_refused_without_includes(&inner, tmp.path());
        }
    }

    #[test]
    fn symlinked_home_and_minimal_dotgit_with_common_dir_prevent_bypasses() {
        if let Some(r2_raw) = std::env::var_os("HERMES_B6_R2") {
            let r2 = PathBuf::from(r2_raw);
            assert!(workspace_git(&r2, LazyFetchGuard::NoPromisorRemote).is_none());
            return;
        }
        if let Some(min_raw) = std::env::var_os("HERMES_B5_MIN") {
            let min_repo = PathBuf::from(min_raw);
            assert!(workspace_git(&min_repo, LazyFetchGuard::NoPromisorRemote).is_none());
            return;
        }
        let tmp = tempfile::tempdir().unwrap();

        // B6: symlinked/junctioned HOME + `includeIf.gitdir:~/`
        let real_home = tmp.path().join("real_home");
        let r2 = real_home.join("r2");
        new_repo(&r2);
        std::fs::write(
            real_home.join("hit6.conf"),
            format!("[core]\n\texcludesFile = ~{USER}/b6\n"),
        )
        .unwrap();
        git(
            &r2,
            &["config", "includeIf.gitdir:~/r2/.path", "../../hit6.conf"],
        );
        let link_home = tmp.path().join("link_home");
        if link_dir(&real_home, &link_home) {
            run_in_child_env(
                "git::tests::symlinked_home_and_minimal_dotgit_with_common_dir_prevent_bypasses",
                &[
                    ("HOME", link_home.as_os_str()),
                    ("HERMES_B6_R2", r2.as_os_str()),
                ],
            );
        }

        // B5 cross-platform: minimal `.git` (HEAD only) + `GIT_COMMON_DIR`
        let evil_common = tmp.path().join("evil_common");
        new_repo(&evil_common);
        let gcd = tmp.path().join("gcd");
        std::fs::rename(evil_common.join(".git"), &gcd).unwrap();
        git(
            &gcd,
            &[
                "--git-dir",
                gcd.to_str().unwrap(),
                "config",
                "core.excludesFile",
                &format!("~{USER}/b5"),
            ],
        );
        let min_repo = tmp.path().join("min_repo");
        std::fs::create_dir_all(min_repo.join(".git")).unwrap();
        std::fs::write(min_repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        run_in_child_env(
            "git::tests::symlinked_home_and_minimal_dotgit_with_common_dir_prevent_bypasses",
            &[
                ("GIT_COMMON_DIR", gcd.as_os_str()),
                ("HERMES_B5_MIN", min_repo.as_os_str()),
            ],
        );
    }
}
