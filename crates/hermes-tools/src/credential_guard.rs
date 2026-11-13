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
const WRITE_DENY_HOME_PREFIX: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".azure",
    ".config/gh",
];

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
/// manager credential files, and the active Hermes home's `.env`. The path is
/// judged where the local backend writes it (`resolve_path` in
/// hermes-environments): `~` is the `HOME` or `USERPROFILE` value as set, and
/// another relative path sits under a non-empty `TERMINAL_CWD`.
///
/// shortcut: the path is judged before the write, not at it, so a symlink
/// swapped in between is followed. Closing that needs the write itself to
/// open each component without following links (`openat` with `O_NOFOLLOW`).
pub fn is_write_denied(path: &Path) -> bool {
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
    let Some(path) = expand_home(&path, backend_home.as_deref()) else {
        return true;
    };
    is_expanded_write_denied(
        &path,
        // Unlike `std::env::home_dir`, an empty `HOME` falls back to the
        // account's home here instead of disabling the home rules.
        dirs::home_dir().as_deref(),
        Some(hermes_config::hermes_home().as_path()),
        std::env::var_os("HERMES_WRITE_SAFE_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .as_deref(),
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
        Some(expanded) => is_expanded_write_denied(&expanded, home, hermes_home, safe_root),
        None => true,
    }
}

/// `is_write_denied_with_roots_and_safe_root` for a path whose `~` the caller
/// has already expanded, so it is not expanded a second time.
fn is_expanded_write_denied(
    path: &Path,
    home: Option<&Path>,
    hermes_home: Option<&Path>,
    safe_root: Option<&Path>,
) -> bool {
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

    if let Some(hermes_home) = hermes_home {
        let env_forms = root_forms(&hermes_home.join(".env"));
        if forms.iter().any(|form| env_forms.contains(form)) {
            return true;
        }
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
