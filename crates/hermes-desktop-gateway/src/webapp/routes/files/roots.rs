use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};

use axum::extract::ConnectInfo;
use axum::http::{Extensions, StatusCode};

use crate::webapp::api::{ApiError, Caller};

/// Credential file names, matched case-insensitively wherever they sit (upstream's managed-file
/// list and home credential files, plus this gateway's own secret stores).
const SENSITIVE_FILES: [&str; 32] = [
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
    "id_ed25519",
    "id_ecdsa",
    "id_dsa",
    "gateway-connections.json",
    "gateway-connection-tokens.json",
    "session-token",
    "cookies",
    "cookies.sqlite",
    "logins.json",
    "key3.db",
    "key4.db",
    "login data",
];

/// Credential folders, matched as consecutive lower-case path components anywhere: browser
/// profiles (cookies, saved passwords), password stores and keyrings among them.
const SENSITIVE_DIRS: [&[&str]; 27] = [
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
    &[".mozilla"],
    &[".password-store"],
    &[".local", "share", "keyrings"],
    &[".config", "google-chrome"],
    &[".config", "chromium"],
    &[".config", "bravesoftware"],
    &[".config", "microsoft-edge"],
    &[".config", "vivaldi"],
    &[".config", "opera"],
    &["library", "cookies"],
    &["library", "keychains"],
    &["library", "safari"],
    &["library", "application support", "google"],
    &["library", "application support", "chromium"],
    &["library", "application support", "bravesoftware"],
    &["library", "application support", "microsoft edge"],
    &["library", "application support", "firefox"],
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
    pub(crate) root: PathBuf,
    pub(crate) path: PathBuf,
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
    let path = match std::fs::canonicalize(&requested) {
        Ok(path) => path,
        // A path that does not resolve answers as its nearest existing folder places it: outside
        // the roots, missing and unreadable answer as an existing path does, so nothing there is
        // revealed.
        Err(err) => return Err(unresolved_refusal(&requested, roots, &err)),
    };
    if roots.refused.iter().any(|r| path.starts_with(r)) {
        return Err(Refusal::Sensitive);
    }
    let root = roots
        .allowed
        .iter()
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
        .ok_or(Refusal::Outside)?;
    if sensitive(&path) {
        return Err(Refusal::Sensitive);
    }
    Ok(Resolved {
        root: root.clone(),
        path,
    })
}

fn unresolved_refusal(requested: &Path, roots: &Roots, err: &std::io::Error) -> Refusal {
    let near = requested.ancestors().skip(1).find_map(|a| {
        let real = std::fs::canonicalize(a).ok()?;
        let tail = requested.strip_prefix(a).ok()?;
        Some((real, tail))
    });
    match near {
        Some((near, _)) if roots.refused.iter().any(|r| near.starts_with(r)) => Refusal::Sensitive,
        Some((near, tail)) if roots.allowed.iter().any(|r| near.starts_with(r)) => {
            if sensitive(&near.join(tail)) {
                Refusal::Sensitive
            } else {
                io_refusal(err)
            }
        }
        _ => Refusal::Outside,
    }
}

pub(crate) fn io_refusal(err: &std::io::Error) -> Refusal {
    match err.kind() {
        std::io::ErrorKind::NotFound => Refusal::NotFound,
        std::io::ErrorKind::PermissionDenied => Refusal::Denied,
        std::io::ErrorKind::NotADirectory => Refusal::NotADirectory,
        // `O_NOFOLLOW` meeting a link; std gives ELOOP no stable kind.
        _ if err.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => Refusal::Outside,
        _ => Refusal::NotFound,
    }
}

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

/// The connection's peer, as the listener recorded it.
pub(crate) fn peer(ext: &Extensions) -> Option<SocketAddr> {
    ext.get::<ConnectInfo<SocketAddr>>().map(|c| c.0)
}

/// System folders a configured working folder may neither be nor hold.
const SYSTEM_DIRS: [&str; 25] = [
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/home",
    "/lib",
    "/lib64",
    "/opt",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/srv",
    "/sys",
    "/tmp",
    "/usr",
    "/var",
    "/Applications",
    "/Library",
    "/System",
    "/Users",
    "/Volumes",
    "/cores",
    "/private",
    "/private/tmp",
];

/// Kernel file systems: a root in one would expose `/proc/<pid>/environ` and the like.
const KERNEL_DIRS: [&str; 3] = ["/proc", "/sys", "/dev"];

/// System state (runtime secrets, configuration, root's home, service data), macOS's spellings
/// included: never a root, except a user's home that lies beneath `/var` (`/var/home/<user>`).
const STATE_DIRS: [&str; 6] = [
    "/run",
    "/etc",
    "/root",
    "/var",
    "/private/etc",
    "/private/var",
];

/// Whether the canonical working folder `dir` may be a root beside the home folder: never `/`, a
/// folder in a kernel file system or one at or beneath a system state folder (bar a home under
/// `/var`), and outside `home` never a folder that is or holds a system folder.
pub(crate) fn extra_root_allowed(dir: &Path, home: Option<&Path>) -> bool {
    if dir.parent().is_none() || KERNEL_DIRS.iter().any(|k| dir.starts_with(k)) {
        return false;
    }
    let in_var_home = home.is_some_and(|h| h.starts_with("/var") && dir.starts_with(h));
    if STATE_DIRS.iter().any(|s| dir.starts_with(s)) && !in_var_home {
        return false;
    }
    if home.is_some_and(|h| dir.starts_with(h)) {
        return true;
    }
    !SYSTEM_DIRS.iter().any(|s| Path::new(s).starts_with(dir))
}

/// Whether the canonical `$HOME` folder `dir` may be a root: same rules as
/// [`extra_root_allowed`], admitting a user home beneath `/var/home/<user>` without admitting
/// `/var` or `/var/home` itself.
pub(crate) fn home_root_allowed(dir: &Path) -> bool {
    let var_home = (dir.starts_with("/var/home") && dir != Path::new("/var/home")).then_some(dir);
    extra_root_allowed(dir, var_home)
}

/// The roots `admit` builds from `$HOME`, the profile's working folder and the refused trees:
/// both `$HOME` and the working folder must pass the root checks.
pub(crate) fn roots_for(user_home: Option<&Path>, cwd: &Path, refused: &[PathBuf]) -> Roots {
    let real_home = user_home
        .and_then(|h| std::fs::canonicalize(h).ok())
        .filter(|h| home_root_allowed(h));
    let mut allowed: Vec<PathBuf> = real_home.iter().cloned().collect();
    allowed.extend(
        std::fs::canonicalize(cwd)
            .ok()
            .filter(|cwd| extra_root_allowed(cwd, real_home.as_deref())),
    );
    Roots::new(&allowed, refused)
}
