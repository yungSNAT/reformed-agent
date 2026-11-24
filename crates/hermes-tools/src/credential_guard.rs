//! Credential file protection — prevents tools from reading or writing
//! sensitive credential files, and scans content for leaked secrets.
//!
//! Requirement 22.3

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use hermes_core::ToolError;
use regex::Regex;

// ---------------------------------------------------------------------------
// Protected file patterns
// ---------------------------------------------------------------------------

/// File name patterns that indicate credential / secret files.
/// Matched case-insensitively against the file name (not the full path).
const PROTECTED_FILE_NAMES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
    ".env.staging",
    ".env.test",
    "credentials",
    "credentials.json",
    "secrets",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
    "secret_key",
    "service-account",
    "service_account.json",
    "service-account.json",
    "gcloud-service-key.json",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "id_dsa",
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".jks",
    ".keystore",
    ".pub", // SSH public keys — still sensitive in some contexts
];

/// Directory names that are always protected.
const PROTECTED_DIR_NAMES: &[&str] = &[".ssh", ".gnupg", ".aws", ".config/gcloud", ".kube"];

const WRITE_DENY_EXACT_ABSOLUTE: &[&str] = &[
    "/etc/hosts",
    "/etc/shadow",
    "/etc/passwd",
    "/etc/sudoers",
    "/private/etc/hosts",
    "/private/etc/passwd",
    "/private/etc/shadow",
    "/private/etc/sudoers",
];
const WRITE_DENY_PREFIX_ABSOLUTE: &[&str] = &[
    "/boot",
    "/etc/sudoers.d",
    "/etc/systemd",
    "/private/etc/ssh",
    "/private/etc/sudoers.d",
    "/private/var",
];
/// macOS temporary directories. `$TMPDIR` is under `/var/folders` and the
/// shared one is `/var/tmp`; `/var` is a symlink into `/private/var`, so the
/// resolved form of every temporary path would otherwise hit the
/// `/private/var` deny.
const WRITE_ALLOW_UNDER_DENIED_PREFIX: &[&str] = &["/private/var/folders", "/private/var/tmp"];
const WRITE_DENY_HOME_EXACT: &[&str] = &[
    ".netrc",
    ".bashrc",
    ".zshrc",
    ".profile",
    ".bash_profile",
    ".zprofile",
    ".npmrc",
    ".pypirc",
    ".pgpass",
];
/// Home files that claw migrate, import-agent and auth discovery read as
/// tokens, and the memory plugins' shared configs (an API key and the
/// endpoint it is sent to). Matched as stores are: by any case or stream
/// spelling, and where the file or a folder above it links.
const HOME_TOKEN_SOURCES: &[&str] = &[
    ".claude/.credentials.json",
    ".codex/auth.json",
    ".pi/agent/auth.json",
    ".hermes/auth.json",
    ".hermes-agent-ultra/auth.json",
    ".hermes/.anthropic_oauth.json",
    ".hermes/.nous_oauth.json",
    ".qwen/oauth_creds.json",
    ".honcho/config.json",
    ".hermes/honcho.json",
    ".hindsight/config.json",
];
/// Home folders whose every file is a token source: the Hindsight plugin
/// writes `<name>.env` here with the LLM key and base URL its local daemon
/// starts with. Matched as stores are.
const HOME_TOKEN_FOLDERS: &[&str] = &[".hindsight/profiles"];
/// Memory plugin configs under each profile's Hermes home, matched as
/// `HOME_TOKEN_SOURCES` are. The plugins and `hermes memory setup` write them
/// without the file tools.
const HERMES_HOME_TOKEN_SOURCES: &[&str] = &[
    "honcho.json",
    "hindsight/config.json",
    "openviking.json",
    "mem0.json",
    "supermemory.json",
    "contextlattice.json",
];
const WRITE_DENY_HOME_PREFIX: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".azure",
    ".config/gh",
];
/// The files this fork writes in `auth/`.
const AUTH_STORE_FILES: &[&str] = &[
    "tokens.json",
    "pool.json",
    "provenance.key",
    "google_oauth.json",
];

/// A key for comparing a path with a store: each component folded to lower
/// case and cut at its first `:`, so the spellings that a case-insensitive
/// file system (Windows, macOS) or an NTFS stream suffix (`::$DATA`,
/// `:name`) give the same file match. On a case-sensitive file system this
/// also denies the few names that differ from a store only in case.
fn store_key(path: &Path) -> PathBuf {
    path.components()
        .map(|component| match component {
            Component::Normal(name) => {
                // Upper then lower case folds the forms APFS folds,
                // such as the long s.
                let name: String = name
                    .to_string_lossy()
                    .chars()
                    .flat_map(char::to_uppercase)
                    .flat_map(char::to_lowercase)
                    .collect();
                match name.split(':').next() {
                    Some(stem) if !stem.is_empty() => stem.to_string(),
                    _ => name,
                }
                .into()
            }
            other => other.as_os_str().to_ascii_lowercase(),
        })
        .collect()
}

/// Credential stores under each profile's Hermes home, each denied with
/// everything below it: provider keys (`.env`); the Nous OAuth token
/// (`auth.json`); the token store, credential pool, provenance key and Gemini
/// OAuth file (`auth/`); MCP server tokens (`mcp_auth.json`) and the skills
/// hub token (`hub_token`). A store, or a file in `auth/`, that links
/// elsewhere is denied where it points, too. `config.yaml` stays writable, as upstream keeps its
/// control files (upstream #45947).
const WRITE_DENY_HERMES_HOME_PREFIX: &[&str] =
    &[".env", "auth.json", "auth", "mcp_auth.json", "hub_token"];

// ---------------------------------------------------------------------------
// Content secret patterns
// ---------------------------------------------------------------------------

static SECRET_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        (
            "Private key",
            Regex::new(r"(?i)-----BEGIN (?:RSA |EC |DSA )?PRIVATE KEY-----").unwrap(),
        ),
        (
            "API key (sk-)",
            Regex::new(r"\bsk-[a-zA-Z0-9]{20,}\b").unwrap(),
        ),
        (
            "API key (sk_live)",
            Regex::new(r"\bsk_live_[a-zA-Z0-9]{20,}\b").unwrap(),
        ),
        (
            "GitHub token",
            Regex::new(r"\bgh[ps]_[a-zA-Z0-9]{36,}\b").unwrap(),
        ),
        (
            "AWS access key",
            Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(),
        ),
        (
            "AWS secret key",
            Regex::new(r"(?i)aws_secret_access_key\s*=\s*\S+").unwrap(),
        ),
        (
            "Google API key",
            Regex::new(r"\bAIza[0-9A-Za-z\-_]{35}\b").unwrap(),
        ),
        (
            "Slack token",
            Regex::new(r"\bxox[bpras]-[0-9a-zA-Z\-]{10,}\b").unwrap(),
        ),
        (
            "Telegram bot token",
            Regex::new(r"\b\d{8,12}:[A-Za-z0-9_-]{30,}\b").unwrap(),
        ),
        (
            "Stripe key",
            Regex::new(r"\b[rk]k_live_[0-9a-zA-Z]{24,}\b").unwrap(),
        ),
        (
            "JWT token",
            Regex::new(r"\beyJ[A-Za-z0-9-_]+\.eyJ[A-Za-z0-9-_]+\.[A-Za-z0-9-_]+\b").unwrap(),
        ),
        (
            "Password assignment",
            Regex::new(r"(?i)(?:password|passwd|pwd)\s*[:=]\s*\S+").unwrap(),
        ),
        (
            "Generic secret assignment",
            Regex::new(r"(?i)(?:secret|token|api_key|apikey|access_key)\s*[:=]\s*\S{8,}").unwrap(),
        ),
    ]
});

// ---------------------------------------------------------------------------
// CredentialGuard
// ---------------------------------------------------------------------------

/// Guards against reading or writing credential files and content containing secrets.
#[derive(Debug, Clone)]
pub struct CredentialGuard {
    /// Additional file name patterns to protect (case-insensitive glob).
    extra_protected_names: Vec<String>,
    /// If true, also block writes that contain detected secrets.
    scan_content: bool,
}

impl CredentialGuard {
    /// Create a new guard with default protected patterns and content scanning enabled.
    pub fn new() -> Self {
        Self {
            extra_protected_names: Vec::new(),
            scan_content: true,
        }
    }

    /// Create a guard that only checks file paths, not content.
    pub fn path_only() -> Self {
        Self {
            extra_protected_names: Vec::new(),
            scan_content: false,
        }
    }

    /// Add an extra protected file name pattern.
    pub fn with_extra_protected_name(mut self, name: &str) -> Self {
        self.extra_protected_names.push(name.to_lowercase());
        self
    }

    /// Check whether a file path refers to a protected credential file.
    pub fn is_protected_file(&self, path: &Path) -> bool {
        is_protected_file_with_extra(path, &self.extra_protected_names)
    }

    /// Check whether reading a file is allowed.
    ///
    /// Returns `Err(ToolError)` if the file is a protected credential file.
    pub fn check_read_access(&self, path: &Path) -> Result<(), ToolError> {
        if self.is_protected_file(path) {
            return Err(ToolError::ExecutionFailed(format!(
                "Access denied: '{}' is a protected credential file",
                path.display()
            )));
        }
        Ok(())
    }

    /// Check whether writing to a file is allowed.
    ///
    /// Returns `Err(ToolError)` if the target is a protected file or if the
    /// content contains detectable secrets (when content scanning is enabled).
    pub fn check_write_access(&self, path: &Path, content: &str) -> Result<(), ToolError> {
        if is_write_denied(path) {
            return Err(ToolError::ExecutionFailed(format!(
                "Write denied: '{}' is a protected system or credential path",
                path.display()
            )));
        }

        // Check path first
        if self.is_protected_file(path) {
            return Err(ToolError::ExecutionFailed(format!(
                "Write denied: '{}' is a protected credential file",
                path.display()
            )));
        }

        // Optionally scan content for secrets
        if self.scan_content {
            if let Some(detection) = detect_secrets(content) {
                return Err(ToolError::ExecutionFailed(format!(
                    "Write denied: content contains detected secret ({})",
                    detection
                )));
            }
        }

        Ok(())
    }
}

impl Default for CredentialGuard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Standalone helper functions
// ---------------------------------------------------------------------------

/// Check whether a file path refers to a protected credential file
/// (using the built-in pattern list only).
pub fn is_protected_file(path: &Path) -> bool {
    is_protected_file_with_extra(path, &[])
}

/// Check whether a file path is write-denied even if it is not a credential
/// file by name. This covers sensitive system files, shell profiles, package
/// manager credentials, the active Hermes home's stores and the configured
/// OAuth store. The path is
/// judged where the local backend writes it (`local_write_path`).
///
/// This guards the file tools only: the terminal tool (redirects, `cp`, `mv`,
/// `ln`) is outside it, as upstream's rule is defence in depth, not a boundary.
///
/// shortcut: the path is judged before the write, not at it, so a symlink
/// swapped in between is followed. Closing that needs the write itself to
/// open each component without following links (`openat` with `O_NOFOLLOW`).
pub fn is_write_denied(path: &Path) -> bool {
    let safe_root = std::env::var_os("HERMES_WRITE_SAFE_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    judge_write(path, safe_root.as_deref())
}

/// `is_write_denied` without the `HERMES_WRITE_SAFE_ROOT` sandbox, for a
/// writer outside the file tools that follows a link a model could plant
/// (the memory tool): the safe root confines the file tools only.
pub fn is_link_target_write_denied(path: &Path) -> bool {
    judge_write(path, None)
}

/// `path` as an absolute path where the local backend writes it
/// (`resolve_path` in hermes-environments): `~` is the `HOME` or
/// `USERPROFILE` value as set, another relative path sits under a non-empty
/// `TERMINAL_CWD`, else under the process folder. A tool that writes a file
/// itself takes its path from here, so it judges and writes one file. None
/// for a `~user` path, which `is_write_denied` refuses.
pub fn local_write_path(path: &Path) -> Option<PathBuf> {
    let terminal_cwd = std::env::var_os("TERMINAL_CWD").filter(|cwd| !cwd.is_empty());
    let path = match terminal_cwd {
        Some(cwd) if path.is_relative() && !path.to_string_lossy().starts_with('~') => {
            PathBuf::from(cwd).join(path)
        }
        _ => path.to_path_buf(),
    };
    let backend_home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let path = expand_home(&path, backend_home.as_deref())?;
    Some(std::path::absolute(&path).unwrap_or(path))
}

fn judge_write(path: &Path, safe_root: Option<&Path>) -> bool {
    let Some(path) = local_write_path(path) else {
        return true;
    };
    is_expanded_write_denied(
        &path,
        // Unlike `std::env::home_dir`, an empty `HOME` falls back to the
        // account's home here instead of disabling the home rules.
        dirs::home_dir().as_deref(),
        Some(hermes_config::hermes_home().as_path()),
        hermes_config::paths::auth_json_override().as_deref(),
        safe_root,
    )
}

/// `path` with a leading `~` or `~/` expanded once to `home`, as the local
/// backend does. None for a `~user` path: the backend expands it to that
/// account's home, which this guard does not look up, so it cannot be judged.
/// Other names after `~` are not user names, and the backend keeps them as
/// written.
fn expand_home(path: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return Some(home.unwrap_or(path).to_path_buf());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home {
            return Some(home.join(rest.trim_start_matches('/')));
        }
    }
    let is_tilde_user = raw.strip_prefix('~').is_some_and(|rest| {
        let user = rest.split('/').next().unwrap_or_default();
        !user.is_empty()
            && user
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    });
    (!is_tilde_user).then(|| path.to_path_buf())
}

/// `path` with `.` and `..` folded and repeated separators dropped, without
/// touching the filesystem.
fn fold_dots(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    folded
}

/// `path` as the write walks it: each component that exists is resolved
/// through its symlinks before the next is added, and `..` steps up from that
/// resolved place. A missing component is kept as written, since the write
/// creates it as a plain directory. None when a component is a symlink that
/// does not resolve, dangling or through a directory the write has yet to
/// create: its target cannot be judged before it exists.
fn resolve_components(path: &Path) -> Option<PathBuf> {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other);
                match resolved.canonicalize() {
                    Ok(real) => resolved = real,
                    Err(_)
                        if resolved
                            .symlink_metadata()
                            .is_ok_and(|meta| meta.file_type().is_symlink()) =>
                    {
                        return None;
                    }
                    Err(_) => {}
                }
            }
        }
    }
    Some(resolved)
}

/// The 2 forms of `path` the guard judges: as written (made absolute and
/// folded) and as the filesystem resolves it. Path text and symlinks can then
/// not step around a rule. None when the path cannot be resolved.
fn path_forms(path: &Path) -> Option<[PathBuf; 2]> {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    Some([fold_dots(&absolute), resolve_components(&absolute)?])
}

/// `path_forms` for a path a rule names (a home, the Hermes `.env` or the safe
/// root), which is taken as written when it cannot be resolved: a write that
/// walks through it is refused anyway.
fn root_forms(root: &Path) -> [PathBuf; 2] {
    path_forms(root).unwrap_or_else(|| {
        let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let folded = fold_dots(&absolute);
        [folded.clone(), folded]
    })
}

fn path_has_component_prefix(path: &Path, root: &Path, prefix: &str) -> bool {
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return false,
    };
    rel == Path::new(prefix) || rel.starts_with(Path::new(prefix))
}

/// Root-injected form for deterministic tests and callers that already know
/// the active profile/home roots.
pub fn is_write_denied_with_roots(
    path: &Path,
    home: Option<&Path>,
    hermes_home: Option<&Path>,
) -> bool {
    is_write_denied_with_roots_and_safe_root(path, home, hermes_home, None)
}

/// Root-injected form including the optional `HERMES_WRITE_SAFE_ROOT` sandbox.
/// Static system and credential denies always win over the safe-root allowance.
pub fn is_write_denied_with_roots_and_safe_root(
    path: &Path,
    home: Option<&Path>,
    hermes_home: Option<&Path>,
    safe_root: Option<&Path>,
) -> bool {
    match expand_home(path, home) {
        Some(expanded) => is_expanded_write_denied(&expanded, home, hermes_home, None, safe_root),
        None => true,
    }
}

/// `hermes_home` and every other profile home `--profile` can select: the
/// root home (the grandparent when `hermes_home` is itself a profile's
/// folder) and each folder in `<root>/profiles/` that a profile name selects.
/// As `--profile default` resolves it, any parent of a folder named
/// `profiles` counts as the root. The second value is `<root>/profiles/`
/// when it exists but cannot be listed in full, so a profile in it may be
/// missing from the list; the caller protects it whole.
fn profile_homes(hermes_home: &Path) -> (Vec<PathBuf>, Option<PathBuf>) {
    let root = match hermes_home.parent() {
        Some(parent) if parent.file_name() == Some("profiles".as_ref()) => {
            parent.parent().unwrap_or(parent)
        }
        _ => hermes_home,
    };
    let folder = root.join("profiles");
    let mut homes = vec![hermes_home.to_path_buf()];
    let mut add = |home: PathBuf| {
        if !homes.contains(&home) {
            homes.push(home);
        }
    };
    add(root.to_path_buf());
    let mut listed = true;
    match std::fs::read_dir(&folder) {
        Ok(entries) => {
            for entry in entries {
                let Ok(entry) = entry else {
                    listed = false;
                    continue;
                };
                let selectable = entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| is_profile_name(&name.to_ascii_lowercase()));
                if selectable && entry.path().is_dir() {
                    add(entry.path());
                }
            }
        }
        Err(_) => listed = !folder.is_dir(),
    }
    (homes, (!listed).then_some(folder))
}

/// A name `--profile` accepts (upstream's `PROFILE_ID_RE`,
/// `^[a-z0-9][a-z0-9_-]{0,63}$`), after it folds the name to lower case.
fn is_profile_name(name: &str) -> bool {
    name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The credential stores and token sources under these roots, and under
/// every other profile home beside `hermes_home`, each with `true` for a
/// token source (one file) or `false` for a store (denied with everything
/// below it), and each one's link target as well, including a target that
/// does not exist yet. A profiles folder that cannot be listed counts as a
/// store.
fn credential_stores(
    home: Option<&Path>,
    hermes_home: Option<&Path>,
    auth_override: Option<&Path>,
) -> Vec<(PathBuf, bool)> {
    let (hermes_homes, unlisted_profiles) = hermes_home.map(profile_homes).unwrap_or_default();
    let profile_stores = hermes_homes.iter().flat_map(|hermes_home| {
        // Include known stores even when the directory cannot be listed.
        let auth = hermes_home.join("auth");
        WRITE_DENY_HERMES_HOME_PREFIX
            .iter()
            .map(move |store| hermes_home.join(store))
            .chain(
                std::fs::read_dir(&auth)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path()),
            )
            .chain(AUTH_STORE_FILES.iter().map(move |name| auth.join(name)))
    });
    let with_link = |store: PathBuf| {
        let target = hermes_config::resolve_symlink_target(&store);
        let linked = (target != store).then_some(target);
        std::iter::once(store).chain(linked)
    };
    let home_folders = home.into_iter().flat_map(|home| {
        HOME_TOKEN_FOLDERS
            .iter()
            .map(move |folder| home.join(folder))
    });
    let stores = auth_override
        .map(Path::to_path_buf)
        .into_iter()
        .chain(profile_stores)
        .chain(home_folders)
        .chain(unlisted_profiles)
        .flat_map(with_link)
        .map(|store| (store, false));
    let sources = home
        .into_iter()
        .flat_map(|home| {
            HOME_TOKEN_SOURCES
                .iter()
                .map(move |source| home.join(source))
        })
        .chain(hermes_homes.iter().flat_map(|hermes_home| {
            HERMES_HOME_TOKEN_SOURCES
                .iter()
                .map(move |source| hermes_home.join(source))
        }))
        .flat_map(with_link)
        .map(|source| (source, true));
    stores.chain(sources).collect()
}

#[cfg(test)]
thread_local! {
    static GUARD_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SUBTREE_DIRS_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Path judgements and set lookups made on this thread, for tests that bound
/// how many a delete makes.
#[cfg(test)]
pub(crate) fn guard_calls() -> usize {
    GUARD_CALLS.with(std::cell::Cell::get)
}

/// The protected paths, gathered once so that many deletes can be judged
/// without judging each file: the credential stores and token sources with
/// where each links, and the home files and folders the write rule names.
/// The fixed system paths (`/etc` and the like) are not included; a delete
/// these tools make stays inside the Hermes home or a temporary folder.
pub struct ProtectedPaths {
    /// Each path in both forms, its comparison key, and whether it is one
    /// file (`true`) or a folder protected with everything below it.
    members: Vec<(PathBuf, PathBuf, bool)>,
}

impl ProtectedPaths {
    /// The paths the file tools protect now: the account's home, the active
    /// Hermes home and a configured auth store.
    pub fn current() -> Self {
        Self::build(
            dirs::home_dir().as_deref(),
            Some(&hermes_config::hermes_home()),
            hermes_config::paths::auth_json_override().as_deref(),
        )
    }

    /// The paths under the given roots.
    pub fn with_roots(home: Option<&Path>, hermes_home: Option<&Path>) -> Self {
        Self::build(home, hermes_home, None)
    }

    /// These paths and another Hermes home's stores and sources.
    pub fn and_hermes_home(mut self, hermes_home: &Path) -> Self {
        self.members
            .extend(Self::build(None, Some(hermes_home), None).members);
        self
    }

    fn build(
        home: Option<&Path>,
        hermes_home: Option<&Path>,
        auth_override: Option<&Path>,
    ) -> Self {
        let mut paths = credential_stores(home, hermes_home, auth_override);
        if let Some(home) = home {
            paths.extend(
                WRITE_DENY_HOME_EXACT
                    .iter()
                    .map(|name| (home.join(name), true)),
            );
            paths.extend(
                WRITE_DENY_HOME_PREFIX
                    .iter()
                    .map(|name| (home.join(name), false)),
            );
        }
        let members = paths
            .into_iter()
            .flat_map(|(path, exact)| {
                root_forms(&path).map(|form| {
                    let key = store_key(&form);
                    (form, key, exact)
                })
            })
            .collect();
        Self { members }
    }

    /// Both comparison keys of `path`, or None when a link in it does not
    /// resolve.
    fn keys(path: &Path) -> Option<[PathBuf; 2]> {
        #[cfg(test)]
        GUARD_CALLS.with(|calls| calls.set(calls.get() + 1));
        path_forms(path).map(|forms| forms.each_ref().map(|form| store_key(form)))
    }

    /// Whether `path` is itself protected: a token source, or in a store.
    /// A path with a link that does not resolve counts as protected.
    pub fn contains(&self, path: &Path) -> bool {
        let Some(keys) = Self::keys(path) else {
            return true;
        };
        self.members.iter().any(|(_, member, exact)| {
            keys.iter().any(|key| {
                if *exact {
                    key == member
                } else {
                    key.starts_with(member)
                }
            })
        })
    }

    /// A protected path that deleting `path` would remove: one at or below
    /// it, or the store it lies in. Decided from the paths alone, without
    /// reading the folder; a link inside it is not followed by the delete,
    /// so a store it points at is kept and is not counted.
    pub fn removed_by(&self, path: &Path) -> Option<PathBuf> {
        let Some(keys) = Self::keys(path) else {
            return Some(path.to_path_buf());
        };
        self.members
            .iter()
            .find(|(_, member, exact)| {
                keys.iter()
                    .any(|key| member.starts_with(key) || (!*exact && key.starts_with(member)))
            })
            .map(|(form, _, _)| form.clone())
    }
}

#[cfg(target_os = "linux")]
fn read_linux_mounts() -> Option<Vec<PathBuf>> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let mut mounts: Vec<PathBuf> = mountinfo
        .lines()
        .filter_map(|line| line.split_whitespace().nth(4))
        .map(|mp| PathBuf::from(mp.replace(r"\040", " ")))
        .collect();
    if mounts.is_empty() {
        return None;
    }
    mounts.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
    Some(mounts)
}

/// Whether `target_path` (the resolved path of a `links == 1` write) may name
/// a file inside `store_dir` under a spelling path checks did not see (such as
/// a UNC loopback share, cross-drive/subst prefix, macOS firmlink, Linux bind
/// mount, or a symlinked store root).
fn may_have_single_link_alias(
    target_path: &Path,
    store_dir: &Path,
    #[cfg(target_os = "linux")] linux_mounts: Option<&[PathBuf]>,
) -> bool {
    let Ok(canon_store) = std::fs::canonicalize(store_dir) else {
        return true;
    };
    if target_path.starts_with(&canon_store) {
        return true;
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let disk_of = |p: &Path| match p.components().next() {
            Some(Component::Prefix(pre)) => match pre.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some(d.to_ascii_uppercase()),
                _ => None,
            },
            _ => None,
        };
        let Some(t_disk) = disk_of(target_path) else {
            return true;
        };
        disk_of(&canon_store) != Some(t_disk) || disk_of(store_dir) != Some(t_disk)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = canon_store;
        target_path.starts_with("/System/Volumes/")
            || target_path.starts_with("/Volumes/")
            || target_path.starts_with("/dev/")
    }
    #[cfg(target_os = "linux")]
    {
        if target_path.starts_with("/proc/") || target_path.starts_with("/dev/") {
            return true;
        }
        let Some(mounts) = linux_mounts else {
            return true;
        };
        let mp = |p: &Path| mounts.iter().find(|m| p.starts_with(m));
        if mp(target_path) != mp(&canon_store) {
            return true;
        }
        mounts
            .iter()
            .any(|m| m != &canon_store && m.starts_with(&canon_store))
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = canon_store;
        true
    }
}

/// Whether `path` names, under another name, the same file as a protected
/// one: a credential store or token source, a file inside a store folder or
/// a protected home folder, or a home or system file the rule names. A hard
/// link made outside the file tools (the terminal's `ln`) gives such a file a
/// name the path rules cannot see, and a write through it changes the file in
/// place; so can another spelling of the same file (a Windows loopback share,
/// a bind mount). Tracks visited directory `(volume, file)` identities so
/// `auth/` overlap and bind-mount cycles cannot re-walk, and skips subtree
/// recursion on ordinary local `links == 1` writes without an alternate
/// spelling (whilst directory symlinks/junctions below protected roots remain
/// excluded by `entry.file_type().is_dir()`).
fn is_hard_link_to_protected_file(
    path: &Path,
    home: Option<&Path>,
    hermes_home: Option<&Path>,
    auth_override: Option<&Path>,
) -> bool {
    let Some(target) = hermes_config::file_identity(path) else {
        return false;
    };
    let stores = credential_stores(home, hermes_home, auth_override);
    let exact_home_files = home.into_iter().flat_map(|home| {
        WRITE_DENY_HOME_EXACT
            .iter()
            .map(move |name| home.join(name))
    });
    let exact_system_files = WRITE_DENY_EXACT_ABSOLUTE.iter().map(PathBuf::from);
    let mut exact_files: Vec<PathBuf> = exact_home_files.chain(exact_system_files).collect();
    let mut pending: Vec<PathBuf> = home
        .into_iter()
        .flat_map(|home| {
            WRITE_DENY_HOME_PREFIX
                .iter()
                .map(move |name| home.join(name))
        })
        .collect();
    for (candidate, is_exact_file) in stores {
        if is_exact_file {
            exact_files.push(candidate);
        } else {
            pending.push(candidate);
        }
    }
    for file in exact_files {
        if hermes_config::file_identity(&file).is_some_and(|id| id.same_file(&target)) {
            return true;
        }
    }

    #[cfg(target_os = "linux")]
    let linux_mounts = if target.links < 2 {
        read_linux_mounts()
    } else {
        None
    };

    let mut seen_dirs: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    while let Some(protected) = pending.pop() {
        let Some(id) = hermes_config::file_identity(&protected) else {
            continue;
        };
        if id.same_file(&target) {
            return true;
        }
        if !protected.is_dir() {
            continue;
        }
        if target.links < 2
            && !may_have_single_link_alias(
                path,
                &protected,
                #[cfg(target_os = "linux")]
                linux_mounts.as_deref(),
            )
        {
            continue;
        }
        if !seen_dirs.insert((id.volume, id.file)) {
            continue;
        }
        #[cfg(test)]
        SUBTREE_DIRS_READ.with(|reads| reads.set(reads.get() + 1));
        // A folder's entries, links not followed; a file has none.
        for entry in std::fs::read_dir(&protected)
            .into_iter()
            .flatten()
            .flatten()
        {
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let entry_path = entry.path();
            if is_dir {
                pending.push(entry_path);
            } else if hermes_config::file_identity(&entry_path)
                .is_some_and(|entry_id| entry_id.same_file(&target))
            {
                return true;
            }
        }
    }
    false
}

/// `is_write_denied_with_roots_and_safe_root` for a path whose `~` the caller
/// has already expanded, so it is not expanded a second time.
fn is_expanded_write_denied(
    path: &Path,
    home: Option<&Path>,
    hermes_home: Option<&Path>,
    auth_override: Option<&Path>,
    safe_root: Option<&Path>,
) -> bool {
    #[cfg(test)]
    GUARD_CALLS.with(|calls| calls.set(calls.get() + 1));
    let Some(forms) = path_forms(path) else {
        return true;
    };

    for form in &forms {
        if WRITE_DENY_EXACT_ABSOLUTE
            .iter()
            .any(|candidate| form == Path::new(candidate))
        {
            return true;
        }
        if WRITE_DENY_PREFIX_ABSOLUTE
            .iter()
            .any(|prefix| form.starts_with(prefix))
            && !WRITE_ALLOW_UNDER_DENIED_PREFIX
                .iter()
                .any(|allowed| form.starts_with(allowed))
        {
            return true;
        }
    }

    if let Some(home) = home {
        for home in root_forms(home) {
            for form in &forms {
                let Ok(rel) = form.strip_prefix(&home) else {
                    continue;
                };
                if WRITE_DENY_HOME_EXACT
                    .iter()
                    .any(|candidate| rel == Path::new(candidate))
                    || WRITE_DENY_HOME_PREFIX
                        .iter()
                        .any(|prefix| path_has_component_prefix(form, &home, prefix))
                {
                    return true;
                }
            }
        }
    }

    let form_keys = forms.each_ref().map(|form| store_key(form));
    for (store, exact) in credential_stores(home, hermes_home, auth_override) {
        for store_form in root_forms(&store) {
            let store_form = store_key(&store_form);
            if form_keys.iter().any(|form| {
                if exact {
                    *form == store_form
                } else {
                    form.starts_with(&store_form)
                }
            }) {
                return true;
            }
        }
    }

    if is_hard_link_to_protected_file(&forms[1], home, hermes_home, auth_override) {
        return true;
    }

    if let Some(safe_root) = safe_root {
        let Some(safe_root) = expand_home(safe_root, home) else {
            return true;
        };
        // Both forms must stay inside the matching form of the root, so a
        // symlink out of the root does not carry writes with it.
        if !safe_root.as_os_str().is_empty()
            && forms
                .iter()
                .zip(root_forms(&safe_root))
                .any(|(form, root)| !form.starts_with(root))
        {
            return true;
        }
    }

    false
}

fn is_protected_file_with_extra(path: &Path, extra: &[String]) -> bool {
    let path_str = path.to_string_lossy();

    // Check directory components
    for component in path.components() {
        if let std::path::Component::Normal(os_str) = component {
            if let Some(s) = os_str.to_str() {
                let lower = s.to_lowercase();
                for &dir in PROTECTED_DIR_NAMES {
                    if lower == dir || path_str.contains(&format!("/{}/", dir)) {
                        return true;
                    }
                }
            }
        }
    }

    // Check file name
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let lower = name.to_lowercase();

        // Exact matches
        for &protected in PROTECTED_FILE_NAMES {
            if lower == protected {
                return true;
            }
        }

        // Extension matches
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext_lower = ext.to_lowercase();
            for &protected in PROTECTED_FILE_NAMES {
                if protected.starts_with('.') && ext_lower == protected[1..] {
                    return true;
                }
            }
        }

        // Substring matches for credential/secret
        if lower.contains("credential") || lower.contains("secret") || lower.contains("private_key")
        {
            return true;
        }

        // Extra patterns
        for pat in extra {
            if lower == *pat {
                return true;
            }
        }
    }

    false
}

/// Detect secrets in content. Returns the name of the first detected pattern, or None.
pub fn detect_secrets(content: &str) -> Option<&'static str> {
    for (name, regex) in SECRET_PATTERNS.iter() {
        if regex.is_match(content) {
            return Some(name);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protected_env_files() {
        assert!(is_protected_file(Path::new(".env")));
        assert!(is_protected_file(Path::new("/home/user/project/.env")));
        assert!(is_protected_file(Path::new(".env.local")));
        assert!(is_protected_file(Path::new(".env.production")));
    }

    #[test]
    fn test_protected_key_files() {
        assert!(is_protected_file(Path::new("id_rsa")));
        assert!(is_protected_file(Path::new("/home/user/.ssh/id_rsa")));
        assert!(is_protected_file(Path::new("server.key")));
        assert!(is_protected_file(Path::new("cert.pem")));
        assert!(is_protected_file(Path::new("keystore.p12")));
        assert!(is_protected_file(Path::new("keystore.jks")));
    }

    #[test]
    fn test_protected_directory() {
        assert!(is_protected_file(Path::new("/home/user/.ssh/config")));
        assert!(is_protected_file(Path::new(
            "/home/user/.gnupg/pubring.gpg"
        )));
        assert!(is_protected_file(Path::new("/home/user/.aws/credentials")));
    }

    #[test]
    fn test_protected_name_patterns() {
        assert!(is_protected_file(Path::new("credentials.json")));
        assert!(is_protected_file(Path::new("secrets.yaml")));
        assert!(is_protected_file(Path::new("service-account.json")));
        assert!(is_protected_file(Path::new("my_credentials")));
        assert!(is_protected_file(Path::new("app_secrets")));
    }

    #[test]
    fn test_not_protected() {
        assert!(!is_protected_file(Path::new("main.rs")));
        assert!(!is_protected_file(Path::new("Cargo.toml")));
        assert!(!is_protected_file(Path::new("README.md")));
        assert!(!is_protected_file(Path::new("src/app.rs")));
        assert!(!is_protected_file(Path::new("config.yaml")));
    }

    #[test]
    fn test_check_read_access_blocked() {
        let guard = CredentialGuard::new();
        assert!(guard.check_read_access(Path::new(".env")).is_err());
        assert!(guard
            .check_read_access(Path::new("/home/.ssh/id_rsa"))
            .is_err());
    }

    #[test]
    fn test_check_read_access_allowed() {
        let guard = CredentialGuard::new();
        assert!(guard.check_read_access(Path::new("main.rs")).is_ok());
        assert!(guard.check_read_access(Path::new("config.yaml")).is_ok());
    }

    #[test]
    fn test_check_write_access_blocked_path() {
        let guard = CredentialGuard::new();
        assert!(guard
            .check_write_access(Path::new(".env"), "hello")
            .is_err());
        assert!(guard
            .check_write_access(Path::new("/etc/passwd"), "root:x:0:0")
            .is_err());
    }

    #[test]
    fn test_check_write_access_blocked_content() {
        let guard = CredentialGuard::new();
        // Contains an API key pattern
        assert!(guard
            .check_write_access(
                Path::new("config.txt"),
                "OPENAI_API_KEY=sk-abc123def456ghi789jkl012mno345"
            )
            .is_err());
        // Contains a private key
        assert!(guard
            .check_write_access(
                Path::new("notes.txt"),
                "-----BEGIN PRIVATE KEY-----\nMIIEvgABCD\n-----END PRIVATE KEY-----"
            )
            .is_err());
    }

    #[test]
    fn test_check_write_access_allowed() {
        let guard = CredentialGuard::new();
        assert!(guard
            .check_write_access(Path::new("output.txt"), "Hello world")
            .is_ok());
        assert!(guard
            .check_write_access(Path::new("main.rs"), "fn main() {}")
            .is_ok());
    }

    #[test]
    fn test_path_only_guard() {
        let guard = CredentialGuard::path_only();
        // Path check still works
        assert!(guard.check_read_access(Path::new(".env")).is_err());
        // But content scanning is disabled
        assert!(guard
            .check_write_access(
                Path::new("output.txt"),
                "OPENAI_API_KEY=sk-abc123def456ghi789jkl012mno345"
            )
            .is_ok());
    }

    #[test]
    fn test_detect_secrets() {
        assert!(detect_secrets("sk-abc123def456ghi789jkl012mno345").is_some());
        assert!(detect_secrets("-----BEGIN PRIVATE KEY-----\nMIIEvg").is_some());
        assert!(detect_secrets("password = hunter2").is_some());
        assert!(detect_secrets("api_key = 1234567890abcdef").is_some());
        assert!(detect_secrets("AKIA1234567890ABCDEF").is_some());
        assert!(detect_secrets("123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcd1234").is_some());
        assert!(detect_secrets("Hello, world!").is_none());
        assert!(detect_secrets("fn main() { println!(\"hello\"); }").is_none());
    }

    #[test]
    fn test_extra_protected_names() {
        let guard = CredentialGuard::new().with_extra_protected_name("my_custom_secret");
        assert!(guard.is_protected_file(Path::new("my_custom_secret")));
        assert!(guard.is_protected_file(Path::new(".env"))); // still works
    }

    #[test]
    fn test_case_insensitive() {
        assert!(is_protected_file(Path::new(".ENV")));
        assert!(is_protected_file(Path::new("Credentials.JSON")));
        assert!(is_protected_file(Path::new("ID_RSA")));
        assert!(is_protected_file(Path::new("Server.KEY")));
    }

    #[test]
    fn write_deny_covers_the_import_credential_sources() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("hermes");
        // Read as tokens by claw migrate, import-agent and auth discovery.
        for rel in [
            ".claude/.credentials.json",
            ".codex/auth.json",
            ".pi/agent/auth.json",
            ".hermes/auth.json",
            ".hermes-agent-ultra/auth.json",
            ".hermes/.anthropic_oauth.json",
            ".hermes/.nous_oauth.json",
            ".qwen/oauth_creds.json",
        ] {
            assert!(
                is_write_denied_with_roots(&home.join(rel), Some(&home), Some(&hermes_home)),
                "{rel} is writable"
            );
        }
        // Other files in those folders, and the same names outside the home.
        for path in [
            home.join(".codex/config.toml"),
            home.join(".pi/agent/settings.json"),
            home.join(".hermes-agent-ultra/config.yaml"),
            home.join(".qwen/settings.json"),
            tmp.path().join("project/.codex/auth.json"),
            tmp.path().join("project/.qwen/oauth_creds.json"),
        ] {
            assert!(
                !is_write_denied_with_roots(&path, Some(&home), Some(&hermes_home)),
                "{} is refused",
                path.display()
            );
        }
        // A case-insensitive file system opens AUTH.JSON as auth.json.
        assert!(is_write_denied_with_roots(
            &home.join(".codex/AUTH.JSON"),
            Some(&home),
            Some(&hermes_home)
        ));
    }

    #[test]
    fn write_deny_covers_the_memory_plugin_configs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        let denied =
            |path: &Path| is_write_denied_with_roots(path, Some(&home), Some(&hermes_home));
        // Each holds a memory provider's API key (Honcho's also OAuth tokens)
        // and the endpoint the plugin sends it and the memory traffic to.
        for path in [
            home.join(".honcho/config.json"),
            home.join(".hermes/honcho.json"),
            home.join(".hindsight/config.json"),
            hermes_home.join("honcho.json"),
            hermes_home.join("hindsight/config.json"),
            hermes_home.join("openviking.json"),
            hermes_home.join("mem0.json"),
            hermes_home.join("supermemory.json"),
            hermes_home.join("contextlattice.json"),
            // A case-insensitive file system opens these as the configs.
            home.join(".Honcho/CONFIG.json"),
            hermes_home.join("Mem0.JSON"),
            hermes_home.join("HindSight/config.json"),
            // An NTFS stream suffix opens the file itself.
            hermes_home.join("mem0.json::$DATA"),
            // The Hindsight plugin's env for its local daemon: the LLM key
            // and the base URL it is sent to.
            home.join(".hindsight/profiles/hermes.env"),
            home.join(".hindsight/profiles/work.env"),
        ] {
            assert!(denied(&path), "{} is writable", path.display());
        }
        // Their neighbours, and the same names outside both homes.
        for path in [
            home.join(".honcho/notes.json"),
            home.join(".hermes/config.yaml"),
            home.join(".hindsight/banks.json"),
            hermes_home.join("honcho.json.bak"),
            hermes_home.join("hindsight/data.json"),
            hermes_home.join("openviking.yaml"),
            hermes_home.join("memory.json"),
            // A token source is one file, not a folder.
            hermes_home.join("mem0.json/notes.txt"),
            hermes_home.join("supermemory.yaml"),
            hermes_home.join("contextlattice/notes.json"),
            tmp.path().join("project/honcho.json"),
            tmp.path().join("project/.honcho/config.json"),
            tmp.path().join("project/mem0.json"),
        ] {
            assert!(!denied(&path), "{} is refused", path.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_linked_memory_plugin_config_folders() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        let vault = tmp.path().join("vault");
        let banks = tmp.path().join("banks");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&hermes_home).unwrap();
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::create_dir_all(&banks).unwrap();
        std::os::unix::fs::symlink(&vault, home.join(".honcho")).unwrap();
        std::os::unix::fs::symlink(&banks, hermes_home.join("hindsight")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("m0"), hermes_home.join("mem0.json")).unwrap();
        let denied =
            |path: &Path| is_write_denied_with_roots(path, Some(&home), Some(&hermes_home));

        assert!(denied(&vault.join("config.json")));
        assert!(denied(&banks.join("config.json")));
        assert!(denied(&tmp.path().join("m0")));
        assert!(!denied(&vault.join("notes.json")));
        assert!(!denied(&banks.join("data.json")));
        assert!(!denied(&tmp.path().join("m1")));
    }

    #[test]
    fn protected_paths_name_what_a_delete_would_remove() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&hermes_home).unwrap();
        let protected = ProtectedPaths::with_roots(Some(&home), Some(&hermes_home));

        // A store, a folder holding one, and a file inside a store.
        assert!(protected.removed_by(&hermes_home.join("auth")).is_some());
        assert!(protected.removed_by(&hermes_home).is_some());
        assert!(protected.removed_by(&home).is_some());
        assert!(protected
            .removed_by(&hermes_home.join("auth/tokens.json"))
            .is_some());
        assert!(protected
            .removed_by(&home.join(".ssh/id_ed25519"))
            .is_some());
        assert!(protected.contains(&hermes_home.join("Mem0.json")));
        assert!(protected.contains(&home.join(".ssh/config")));
        // Neither, nor below a one-file source.
        assert_eq!(protected.removed_by(&tmp.path().join("project")), None);
        assert_eq!(protected.removed_by(&hermes_home.join("cache")), None);
        assert_eq!(
            protected.removed_by(&hermes_home.join("mem0.json/notes")),
            None
        );
        assert!(!protected.contains(&hermes_home.join("cache/a.png")));
    }

    #[cfg(unix)]
    #[test]
    fn protected_paths_find_a_linked_target_without_reading_the_folder() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&hermes_home).unwrap();
        std::fs::create_dir_all(workspace.join("nested")).unwrap();
        std::fs::write(workspace.join("nested/kept.json"), "{}").unwrap();
        let protected = || ProtectedPaths::with_roots(Some(&home), Some(&hermes_home));
        assert_eq!(protected().removed_by(&workspace), None);

        std::os::unix::fs::symlink(
            workspace.join("nested/kept.json"),
            hermes_home.join("honcho.json"),
        )
        .unwrap();
        let protected = protected();
        let found = protected.removed_by(&workspace).expect("the linked file");
        assert!(found.ends_with("kept.json"), "{}", found.display());
        assert!(protected.contains(&workspace.join("nested/kept.json")));
        assert!(!protected.contains(&workspace.join("nested")));
        // Decided from the paths: an unreadable folder gives the same answer.
        let nested = workspace.join("nested");
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = protected.removed_by(&workspace);
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(unreadable.is_some());

        // Deleting a link to a store removes the link, not the store.
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(hermes_home.join(".env"), other.join("env")).unwrap();
        std::os::unix::fs::symlink(&hermes_home, other.join("profile")).unwrap();
        assert_eq!(protected.removed_by(&other), None);
    }

    #[test]
    fn write_deny_covers_every_profile_home_beside_the_active_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let root = tmp.path().join("root");
        let work = root.join("profiles/work");
        let play = root.join("profiles/play");
        // Not a name `--profile` can select.
        let backup = root.join("profiles/Work.bak");
        for dir in [&home, &work, &play, &backup] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(root.join("profiles/default.yaml"), "").unwrap();

        for active in [&root, &work] {
            let denied = |path: &Path| is_write_denied_with_roots(path, Some(&home), Some(active));
            assert!(!denied(&backup.join(".env")));
            for profile in [&root, &work, &play] {
                for store in [
                    ".env",
                    "auth.json",
                    "auth/tokens.json",
                    "mem0.json",
                    "honcho.json",
                ] {
                    let path = profile.join(store);
                    assert!(
                        denied(&path),
                        "{} writable from {}",
                        path.display(),
                        active.display()
                    );
                }
                assert!(!denied(&profile.join("notes.txt")));
            }
            assert!(!denied(&root.join("profiles/default.yaml")));

            let protected = ProtectedPaths::with_roots(Some(&home), Some(active));
            assert!(protected.removed_by(&root.join("profiles")).is_some());
            assert!(protected.contains(&play.join(".env")));
            assert_eq!(protected.removed_by(&play.join("cache")), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_covers_profiles_it_cannot_list() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let root = tmp.path().join("root");
        let profiles = root.join("profiles");
        let work = profiles.join("work");
        for dir in [&home, &work] {
            std::fs::create_dir_all(dir).unwrap();
        }
        // Searchable but not readable: `--profile work` still finds the
        // folder, but it cannot be listed.
        let set_mode = |mode| {
            std::fs::set_permissions(&profiles, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        set_mode(0o100);
        let listed = std::fs::read_dir(&profiles).is_ok();
        let denied = is_write_denied_with_roots(&work.join(".env"), Some(&home), Some(&root));
        let removed = ProtectedPaths::with_roots(Some(&home), Some(&root)).removed_by(&work);
        set_mode(0o700);

        // Root can list any folder, so the case cannot arise.
        if !listed {
            assert!(denied, "an unlisted profile's .env is writable");
            assert!(removed.is_some());
        }
    }

    #[test]
    fn write_deny_refuses_an_import_credential_source_but_not_what_is_below_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("hermes");
        // Not a token file: a folder that happens to have the name.
        std::fs::create_dir_all(home.join(".codex/auth.json")).unwrap();

        assert!(is_write_denied_with_roots(
            &home.join(".codex/auth.json"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &home.join(".codex/auth.json/notes.txt"),
            Some(&home),
            Some(&hermes_home)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_an_import_credential_source_linked_to_a_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(home.join(".qwen")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("creds"), home.join(".qwen/oauth_creds.json"))
            .unwrap();
        let hermes_home = tmp.path().join("hermes");

        assert!(is_write_denied_with_roots(
            &elsewhere.join("creds"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &elsewhere.join("notes.txt"),
            Some(&home),
            Some(&hermes_home)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_a_linked_import_credential_folder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&vault).unwrap();
        std::os::unix::fs::symlink(&vault, home.join(".codex")).unwrap();
        let hermes_home = tmp.path().join("hermes");

        assert!(is_write_denied_with_roots(
            &vault.join("auth.json"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &vault.join("config.toml"),
            Some(&home),
            Some(&hermes_home)
        ));
    }

    #[test]
    fn write_deny_matches_upstream_sensitive_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&hermes_home).expect("profile");

        for path in ["/etc/shadow", "/etc/passwd", "/etc/sudoers"] {
            assert!(is_write_denied_with_roots(
                Path::new(path),
                Some(&home),
                Some(&hermes_home)
            ));
        }
        for path in [
            "/etc/hosts",
            "/private/etc/hosts",
            "/private/etc/passwd",
            "/private/etc/shadow",
            "/private/etc/sudoers",
        ] {
            assert!(is_write_denied_with_roots(
                Path::new(path),
                Some(&home),
                Some(&hermes_home)
            ));
        }
        assert!(is_write_denied_with_roots(
            Path::new("/etc/sudoers.d/custom"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(is_write_denied_with_roots(
            Path::new("/etc/systemd/system/evil.service"),
            Some(&home),
            Some(&hermes_home)
        ));
        for path in [
            "/private/etc/ssh/sshd_config",
            "/private/etc/sudoers.d/custom",
            "/private/var/db/something",
            "/boot/grub/grub.cfg",
        ] {
            assert!(is_write_denied_with_roots(
                Path::new(path),
                Some(&home),
                Some(&hermes_home)
            ));
        }

        for rel in [
            ".ssh/authorized_keys",
            ".ssh/id_ed25519",
            ".aws/credentials",
            ".gnupg/secring.gpg",
            ".kube/config",
            ".docker/config.json",
            ".azure/config",
            ".config/gh/hosts.yml",
            ".netrc",
            ".bashrc",
            ".zshrc",
            ".profile",
            ".bash_profile",
            ".zprofile",
            ".npmrc",
            ".pypirc",
            ".pgpass",
        ] {
            assert!(is_write_denied_with_roots(
                &home.join(rel),
                Some(&home),
                Some(&hermes_home)
            ));
        }

        assert!(is_write_denied_with_roots(
            &hermes_home.join(".env"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &hermes_home.join("config.yaml"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            Path::new("/tmp/safe_file.txt"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            Path::new("/home/user/project/main.py"),
            Some(&home),
            Some(&hermes_home)
        ));
    }

    #[test]
    fn write_deny_protects_the_credential_stores_under_the_hermes_home() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(hermes_home.join("auth")).expect("auth dir");
        let denied =
            |path: &Path| is_write_denied_with_roots(path, Some(&home), Some(&hermes_home));

        for rel in [
            "auth",
            "auth/tokens.json",
            "auth/pool.json",
            "auth/provenance.key",
            "auth/google_oauth.json",
            "auth/new-store.json",
            "mcp_auth.json",
            "hub_token",
            "auth.json",
        ] {
            assert!(denied(&hermes_home.join(rel)), "{rel} was writable");
        }
        // Control files stay writable, as upstream keeps them (#45947).
        for rel in ["config.yaml", "mcp_servers.json", "authority.md"] {
            assert!(!denied(&hermes_home.join(rel)), "{rel} was denied");
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_a_link_into_the_hermes_auth_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(hermes_home.join("auth")).expect("auth dir");
        let link = tmp.path().join("innocent");
        std::os::unix::fs::symlink(hermes_home.join("auth"), &link).expect("link");

        assert!(is_write_denied_with_roots(
            &link.join("tokens.json"),
            None,
            Some(&hermes_home)
        ));
        // `..` after a linked folder is taken where the link leads.
        std::fs::create_dir_all(hermes_home.join("skills")).expect("skills dir");
        let into_home = tmp.path().join("shortcut");
        std::os::unix::fs::symlink(hermes_home.join("skills"), &into_home).expect("link");
        assert!(is_write_denied_with_roots(
            &into_home.join("../auth/tokens.json"),
            None,
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &into_home.join("../authority.md"),
            None,
            Some(&hermes_home)
        ));
    }

    #[test]
    fn write_deny_matches_the_stores_whatever_their_case_or_stream_suffix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(&hermes_home).expect("home");
        let denied = |rel: &str| {
            is_write_denied_with_roots(&hermes_home.join(rel), None, Some(&hermes_home))
        };

        for rel in [
            "AUTH/pool.json",
            "Auth/tokens.json",
            "MCP_AUTH.JSON",
            "Auth.Json",
            ".ENV",
            "mcp_auth.json::$DATA",
            "hub_token:alt",
            "auth.jſon",
            "mcp_auth.jſon",
        ] {
            assert!(denied(rel), "{rel} was writable");
        }
        for rel in ["AUTHORITY.md", "Config.yaml", "notes:auth"] {
            assert!(!denied(rel), "{rel} was denied");
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_a_store_link_to_a_target_not_yet_created() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(hermes_home.join("auth")).expect("auth dir");
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(&vault).expect("vault");
        std::os::unix::fs::symlink(
            vault.join("custom.dat"),
            hermes_home.join("auth/tokens.json"),
        )
        .expect("link");

        assert!(is_write_denied_with_roots(
            &vault.join("custom.dat"),
            None,
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &vault.join("notes.md"),
            None,
            Some(&hermes_home)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_a_known_store_link_in_an_auth_folder_it_cannot_list() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("profile");
        let auth = hermes_home.join("auth");
        std::fs::create_dir_all(&auth).expect("auth dir");
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(&vault).expect("vault");
        std::fs::write(vault.join("pool.dat"), "{}").expect("store");
        std::os::unix::fs::symlink(vault.join("pool.dat"), auth.join("pool.json")).expect("link");
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o111)).expect("chmod");

        let pool_denied =
            is_write_denied_with_roots(&vault.join("pool.dat"), None, Some(&hermes_home));
        let notes_denied =
            is_write_denied_with_roots(&vault.join("notes.md"), None, Some(&hermes_home));
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        assert!(pool_denied, "the pool's link target was writable");
        assert!(!notes_denied);
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_a_store_file_linked_out_of_the_auth_folder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(hermes_home.join("auth")).expect("auth dir");
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(&vault).expect("vault");
        std::fs::write(vault.join("custom.dat"), "{}").expect("store");
        std::os::unix::fs::symlink(
            vault.join("custom.dat"),
            hermes_home.join("auth/tokens.json"),
        )
        .expect("link");

        assert!(is_write_denied_with_roots(
            &vault.join("custom.dat"),
            None,
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &vault.join("notes.md"),
            None,
            Some(&hermes_home)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_a_dangling_link_at_a_top_level_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(&hermes_home).expect("home");
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(&vault).expect("vault");
        for store in [".env", "auth.json", "mcp_auth.json", "hub_token"] {
            let target = vault.join(format!("new-{store}"));
            std::os::unix::fs::symlink(&target, hermes_home.join(store)).expect("link");
            assert!(
                is_write_denied_with_roots(&target, None, Some(&hermes_home)),
                "{store}"
            );
        }
        std::os::unix::fs::symlink(vault.join("new-auth"), hermes_home.join("auth"))
            .expect("auth link");

        assert!(is_write_denied_with_roots(
            &vault.join("new-auth").join("tokens.json"),
            None,
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &vault.join("notes.md"),
            None,
            Some(&hermes_home)
        ));
    }

    #[test]
    fn write_deny_refuses_a_hard_link_to_a_file_deep_in_a_protected_folder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let keys = home.join(".gnupg").join("private-keys-v1.d");
        std::fs::create_dir_all(&keys).expect("keys dir");
        let key = keys.join("ABCD.key");
        std::fs::write(&key, "x").expect("key");
        let alias = tmp.path().join("notes.txt");
        std::fs::hard_link(&key, &alias).expect("hard link");

        assert!(is_write_denied_with_roots(&alias, Some(&home), None));

        // 5-level deep subtree (past old depth-4 cap) stays protected too.
        let deep = home.join(".ssh").join("a/b/c/d/e");
        std::fs::create_dir_all(&deep).expect("deep dir");
        let deep_key = deep.join("id_deep");
        std::fs::write(&deep_key, "x").expect("deep key");
        let deep_alias = tmp.path().join("deep-notes.txt");
        std::fs::hard_link(&deep_key, &deep_alias).expect("deep hard link");
        assert!(is_write_denied_with_roots(&deep_alias, Some(&home), None));
    }

    #[test]
    fn write_deny_allows_unrelated_multiply_linked_file_past_4096_protected_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let many = home.join(".ssh").join("many");
        let elsewhere = tmp.path().join("work");
        std::fs::create_dir_all(&many).expect("many");
        std::fs::create_dir_all(&elsewhere).expect("work");
        for i in 0..4_100 {
            std::fs::write(many.join(format!("entry-{i}")), "x").expect("entry");
        }
        let plain = elsewhere.join("plain.txt");
        std::fs::write(&plain, "x").expect("plain");
        std::fs::hard_link(&plain, elsewhere.join("plain-2.txt")).expect("plain link");
        assert!(
            !is_write_denied_with_roots(&plain, Some(&home), None),
            "unrelated multiply-linked file denied due to protected entry count"
        );
        let protected_alias = elsewhere.join("leak-4099.txt");
        std::fs::hard_link(many.join("entry-4099"), &protected_alias).expect("protected alias");
        assert!(is_write_denied_with_roots(
            &protected_alias,
            Some(&home),
            None
        ));
    }

    #[test]
    fn ordinary_single_link_write_skips_protected_subtrees_and_seen_dedups_auth_subfolders() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        let auth_sub = hermes_home.join("auth").join("nested");
        let ssh_sub = home.join(".ssh").join("cache");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&auth_sub).expect("auth_sub");
        std::fs::create_dir_all(&ssh_sub).expect("ssh_sub");
        std::fs::create_dir_all(&work).expect("work");
        let token = auth_sub.join("token.json");
        std::fs::write(&token, "{}").expect("token");
        std::fs::write(ssh_sub.join("id_ed25519"), "k").expect("key");
        let plain = work.join("note.txt");
        std::fs::write(&plain, "ok").expect("plain");

        SUBTREE_DIRS_READ.with(|r| r.set(0));
        assert!(!is_write_denied_with_roots(
            &plain,
            Some(&home),
            Some(&hermes_home)
        ));
        assert_eq!(SUBTREE_DIRS_READ.with(std::cell::Cell::get), 0);

        let token_alias = work.join("token_alias.json");
        std::fs::hard_link(&token, &token_alias).expect("hard link");
        let unrelated_link = work.join("other_a.txt");
        std::fs::write(&unrelated_link, "x").expect("unrelated");
        std::fs::hard_link(&unrelated_link, work.join("other_b.txt")).expect("other link");
        SUBTREE_DIRS_READ.with(|r| r.set(0));
        assert!(!is_write_denied_with_roots(
            &unrelated_link,
            Some(&home),
            Some(&hermes_home)
        ));
        // 4 unique dirs: auth, auth/nested, .ssh, .ssh/cache -> auth/nested visited once.
        assert_eq!(SUBTREE_DIRS_READ.with(std::cell::Cell::get), 4);
        assert!(is_write_denied_with_roots(
            &token_alias,
            Some(&home),
            Some(&hermes_home)
        ));

        #[cfg(unix)]
        {
            let vault = tmp.path().join("vault");
            std::fs::create_dir_all(vault.join("kube/sub")).expect("vault kube");
            let linked_cfg = vault.join("kube/sub/config");
            std::fs::write(&linked_cfg, "k").expect("cfg");
            std::os::unix::fs::symlink(vault.join("kube"), home.join(".kube")).expect("kube link");
            assert!(is_write_denied_with_roots(&linked_cfg, Some(&home), None));
        }
    }

    /// The loopback share names a local file under a path the path rules do
    /// not know; it is the same file, with one link. Skipped where the
    /// administrative share cannot be reached.
    #[cfg(windows)]
    #[test]
    fn write_deny_refuses_a_protected_file_through_the_loopback_share() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        let deep = home.join(".ssh").join("a/b/c/d/e");
        std::fs::create_dir_all(&deep).expect("ssh deep");
        std::fs::create_dir_all(&hermes_home).expect("profile");
        std::fs::write(hermes_home.join(".env"), "x").expect("env");
        std::fs::write(home.join(".ssh").join("authorized_keys"), "x").expect("keys");
        std::fs::write(deep.join("id_ed25519"), "x").expect("deep key");
        for local in [
            hermes_home.join(".env"),
            home.join(".ssh").join("authorized_keys"),
            deep.join("id_ed25519"),
        ] {
            let local = local.to_string_lossy().to_string();
            let Some((drive, rest)) = local.split_once(":\\") else {
                return;
            };
            let share = PathBuf::from(format!(r"\\localhost\{drive}$\{rest}"));
            if hermes_config::file_identity(&share).is_none() {
                eprintln!("skipped: the loopback share is not reachable");
                return;
            }
            assert!(
                is_write_denied_with_roots(&share, Some(&home), Some(&hermes_home)),
                "{}",
                share.display()
            );
        }
    }

    #[test]
    fn write_deny_refuses_a_hard_link_to_a_protected_file_under_another_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes");
        let elsewhere = tmp.path().join("work");
        for dir in [
            hermes_home.join("auth"),
            home.join(".ssh"),
            home.join(".claude"),
            elsewhere.clone(),
        ] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        for (index, protected) in [
            hermes_home.join(".env"),
            hermes_home.join("auth").join("tokens.json"),
            home.join(".ssh").join("id_ed25519"),
            home.join(".bashrc"),
            home.join(".claude").join(".credentials.json"),
        ]
        .iter()
        .enumerate()
        {
            std::fs::write(protected, "x").expect("protected file");
            let alias = elsewhere.join(format!("notes-{index}.txt"));
            std::fs::hard_link(protected, &alias).expect("hard link");
            assert!(
                is_write_denied_with_roots(&alias, Some(&home), Some(&hermes_home)),
                "a hard link to {} was writable",
                protected.display()
            );
        }

        // A multiply-linked file that is no protected file stays writable.
        let plain = elsewhere.join("plain.txt");
        std::fs::write(&plain, "x").expect("plain");
        std::fs::hard_link(&plain, elsewhere.join("plain-2.txt")).expect("plain link");
        assert!(!is_write_denied_with_roots(
            &plain,
            Some(&home),
            Some(&hermes_home)
        ));
    }

    #[test]
    fn write_deny_expands_tilde_against_home_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).expect("home");

        assert!(is_write_denied_with_roots(
            Path::new("~/.ssh/id_rsa"),
            Some(&home),
            None
        ));
        assert!(is_write_denied_with_roots(
            Path::new("~/.netrc"),
            Some(&home),
            None
        ));
    }

    #[test]
    fn write_safe_root_sandboxes_non_static_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let safe_root = tmp.path().join("workspace");
        let outside = tmp.path().join("other/file.txt");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&safe_root).expect("safe root");

        assert!(!is_write_denied_with_roots_and_safe_root(
            &safe_root,
            Some(&home),
            None,
            Some(&safe_root)
        ));
        assert!(!is_write_denied_with_roots_and_safe_root(
            &safe_root.join("subdir/file.txt"),
            Some(&home),
            None,
            Some(&safe_root)
        ));
        assert!(is_write_denied_with_roots_and_safe_root(
            &outside,
            Some(&home),
            None,
            Some(&safe_root)
        ));
        assert!(is_write_denied_with_roots_and_safe_root(
            &home.join(".ssh/id_rsa"),
            Some(&home),
            None,
            Some(&home)
        ));
    }

    #[test]
    fn write_deny_judges_the_path_after_folding_dots_and_separators() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).expect("home");

        for path in [
            "//etc/passwd",
            "/etc/./shadow",
            "/etc/../etc/sudoers.d/x",
            "/tmp/../etc/systemd/system/evil.service",
            "/etc/nonexistent/../hosts",
        ] {
            assert!(
                is_write_denied_with_roots(Path::new(path), Some(&home), None),
                "{path} should be denied"
            );
        }
        assert!(is_write_denied_with_roots(
            &home.join("project/../.ssh/authorized_keys"),
            Some(&home),
            None
        ));
        assert!(is_write_denied_with_roots(
            Path::new("~/project/../.netrc"),
            Some(&home),
            None
        ));
        assert!(!is_write_denied_with_roots(
            &home.join("project/./src/../main.rs"),
            Some(&home),
            None
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_follows_symlinked_parents() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        for dir in [home.join(".ssh"), workspace.clone(), outside.clone()] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        std::os::unix::fs::symlink(home.join(".ssh"), workspace.join("keys")).expect("symlink");
        std::os::unix::fs::symlink(&outside, workspace.join("escape")).expect("symlink");

        assert!(is_write_denied_with_roots(
            &workspace.join("keys/authorized_keys"),
            Some(&home),
            None
        ));
        // The write creates `missing`, so `missing/..` is the workspace and
        // `keys` is followed.
        assert!(is_write_denied_with_roots(
            &workspace.join("missing/../keys/config"),
            Some(&home),
            None
        ));
        // A link out of the safe root does not keep its writes inside it.
        assert!(is_write_denied_with_roots_and_safe_root(
            &workspace.join("escape/file.txt"),
            Some(&home),
            None,
            Some(&workspace)
        ));
        assert!(!is_write_denied_with_roots_and_safe_root(
            &workspace.join("src/new/file.txt"),
            Some(&home),
            None,
            Some(&workspace)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_refuses_a_dangling_symlink_whose_target_it_cannot_judge() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let link = tmp.path().join("notes.txt");
        // Writing through the link would create the missing target.
        std::os::unix::fs::symlink(home.join(".ssh/authorized_keys"), &link).expect("symlink");
        assert!(is_write_denied_with_roots(&link, Some(&home), None));
    }

    #[cfg(unix)]
    #[test]
    fn write_deny_refuses_a_symlink_that_resolves_only_once_the_write_creates_a_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("work");
        std::fs::create_dir_all(home.join(".ssh")).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        // `keys` resolves to the home's `.ssh` once the write has created
        // `missing` on its way through `missing/..`.
        std::os::unix::fs::symlink(
            workspace.join("missing/../../home/.ssh"),
            workspace.join("keys"),
        )
        .expect("symlink");
        assert!(is_write_denied_with_roots(
            &workspace.join("missing/../keys/config"),
            Some(&home),
            None
        ));
    }

    #[test]
    fn write_deny_refuses_tilde_user_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        // The local backend expands `~user` to that account's home.
        assert!(is_write_denied_with_roots(
            Path::new("~someone/.ssh/authorized_keys"),
            Some(&home),
            None
        ));
        assert!(!is_write_denied_with_roots(
            &home.join("~draft.txt"),
            Some(&home),
            None
        ));
        // Not a user name, so the backend writes it as written.
        assert!(!is_write_denied_with_roots(
            Path::new("~draft!.txt"),
            Some(&home),
            None
        ));
    }

    #[test]
    fn write_deny_expands_tilde_as_the_local_backend_does() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        // `~//` is the home itself.
        let denied = with_env(
            &[
                ("HOME", Some(&home)),
                ("TERMINAL_CWD", None),
                ("HERMES_WRITE_SAFE_ROOT", None),
            ],
            || is_write_denied(Path::new("~//.ssh/authorized_keys")),
        );
        assert!(denied);
        // With `HOME` empty the backend writes `~/x` to `x` under the process
        // directory, not under the account's home.
        let cwd = std::env::current_dir().expect("cwd");
        let denied = with_env(
            &[
                ("HOME", Some(Path::new(""))),
                ("TERMINAL_CWD", None),
                ("HERMES_WRITE_SAFE_ROOT", Some(&cwd)),
            ],
            || is_write_denied(Path::new("~/notes.txt")),
        );
        assert!(!denied);
    }

    #[test]
    fn write_deny_does_not_take_an_expanded_tilde_path_for_a_user_name() {
        // With `HOME` empty the backend writes `~/~draft/notes.txt` to
        // `~draft/notes.txt` under the process directory: not a `~user` path.
        let denied = with_env(
            &[
                ("HOME", Some(Path::new(""))),
                ("TERMINAL_CWD", None),
                ("HERMES_WRITE_SAFE_ROOT", None),
            ],
            || is_write_denied(Path::new("~/~draft/notes.txt")),
        );
        assert!(!denied);
    }

    #[test]
    fn write_deny_expands_tilde_only_once() {
        // With `HOME` empty the backend writes `~/~/notes.txt` to
        // `~/notes.txt` under the process directory, so a safe root that
        // holds only the account's `~/notes.txt` must not admit it.
        let account_home = with_env(&[("HOME", Some(Path::new("")))], dirs::home_dir)
            .expect("the account's home directory");
        let denied = with_env(
            &[
                ("HOME", Some(Path::new(""))),
                ("TERMINAL_CWD", None),
                (
                    "HERMES_WRITE_SAFE_ROOT",
                    Some(&account_home.join("notes.txt")),
                ),
            ],
            || is_write_denied(Path::new("~/~/notes.txt")),
        );
        assert!(denied);
    }

    #[test]
    fn write_deny_keeps_home_rules_when_home_is_unset() {
        let _lock = hermes_config::managed_gateway::test_lock::lock();
        let saved = std::env::var_os("HOME");
        std::env::remove_var("HOME");
        let home = std::env::home_dir();
        let denied = home
            .as_ref()
            .map(|home| is_write_denied(&home.join(".ssh/authorized_keys")));
        if let Some(saved) = saved {
            std::env::set_var("HOME", saved);
        }
        let home = home.expect("the account's home directory");
        assert_eq!(denied, Some(true), "{}", home.display());
    }

    /// Runs `check` with the given variables set (`None` removes one), under
    /// the shared env lock, and restores them afterwards.
    fn with_env<T>(vars: &[(&str, Option<&Path>)], check: impl FnOnce() -> T) -> T {
        let _lock = hermes_config::managed_gateway::test_lock::lock();
        let saved: Vec<_> = vars
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in vars {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
        for (key, value) in saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }

    #[test]
    fn write_deny_judges_relative_paths_from_the_terminal_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project).expect("project");
        let denied = with_env(
            &[
                ("HOME", Some(&home)),
                ("TERMINAL_CWD", Some(&project)),
                ("HERMES_WRITE_SAFE_ROOT", None),
            ],
            || {
                (
                    is_write_denied(Path::new("../.ssh/authorized_keys")),
                    is_write_denied(Path::new("src/main.rs")),
                )
            },
        );
        assert_eq!(denied, (true, false));
    }

    #[test]
    fn write_deny_protects_the_default_home_env_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let denied = with_env(
            &[
                ("HOME", Some(&home)),
                ("HERMES_HOME", None),
                ("HERMES_AGENT_ULTRA_HOME", None),
                ("HERMES_WRITE_SAFE_ROOT", None),
            ],
            || is_write_denied(&hermes_config::hermes_home().join(".env")),
        );
        assert!(denied);
    }

    #[test]
    fn write_deny_protects_the_configured_auth_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("profile");
        let store = tmp.path().join("provider-state.json");
        let configured = PathBuf::from(format!("  {}  ", store.display()));

        with_env(
            &[
                ("HERMES_HOME", Some(&home)),
                ("HERMES_AUTH_FILE", Some(&configured)),
                ("HERMES_WRITE_SAFE_ROOT", Some(tmp.path())),
            ],
            || {
                assert!(is_write_denied(&store));
                assert!(is_link_target_write_denied(&store));
                // The safe root confines the file tools only.
                let outside = tmp
                    .path()
                    .with_file_name("hermes-outside-the-safe-root.txt");
                assert!(is_write_denied(&outside));
                assert!(!is_link_target_write_denied(&outside));
                assert!(is_write_denied(&home.join("auth.json")));
                assert!(!is_write_denied(
                    &tmp.path().join("provider-state-notes.json")
                ));
                #[cfg(unix)]
                {
                    let target = Path::new("linked-provider-state.json");
                    std::os::unix::fs::symlink(target, &store).expect("store symlink");
                    assert!(is_write_denied(&tmp.path().join(target)));
                }
            },
        );
    }

    #[test]
    fn write_deny_resolves_relative_auth_override_from_the_process_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let relative_store = Path::new("relative-provider-state.json");
        let process_dir = std::env::current_dir().expect("process directory");
        let store = process_dir.join(relative_store);

        with_env(
            &[
                ("HERMES_AUTH_FILE", Some(relative_store)),
                ("TERMINAL_CWD", Some(tmp.path())),
                ("HERMES_WRITE_SAFE_ROOT", None),
            ],
            || {
                assert!(is_write_denied(&store));
                assert!(is_link_target_write_denied(&store));
                assert!(!is_write_denied(relative_store));
            },
        );
    }

    #[test]
    fn write_deny_allows_the_temporary_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        // On macOS this resolves into /private/var/folders.
        assert!(!is_write_denied_with_roots(
            &std::env::temp_dir().join("scratch.txt"),
            Some(&home),
            None
        ));
    }

    #[test]
    fn write_deny_allows_the_shared_temporary_directory_on_macos() {
        // macOS `/var/tmp` resolves to `/private/var/tmp`.
        assert!(!is_write_denied_with_roots(
            Path::new("/private/var/tmp/scratch.txt"),
            None,
            None
        ));
    }

    #[test]
    fn write_deny_keeps_home_rules_when_home_is_empty() {
        let denied = with_env(&[("HOME", Some(Path::new("")))], || {
            dirs::home_dir().map(|home| is_write_denied(&home.join(".ssh/authorized_keys")))
        });
        assert_eq!(denied, Some(true));
    }

    #[test]
    fn write_safe_root_expands_tilde_and_ignores_absent_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let workspace = home.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");

        assert!(!is_write_denied_with_roots_and_safe_root(
            &workspace.join("file.txt"),
            Some(&home),
            None,
            Some(Path::new("~/workspace"))
        ));
        assert!(!is_write_denied_with_roots_and_safe_root(
            &tmp.path().join("regular.txt"),
            Some(&home),
            None,
            None
        ));
    }
}
