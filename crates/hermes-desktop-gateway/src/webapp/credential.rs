//! The browser credential (upstream gateway/webapp_listener.py `browser_session_token` and
//! hermes_cli/browser_credential.py): one token a Webapp tab holds, kept in
//! `<home>/webapp/session-token` (0600 in a 0700 folder) so open tabs survive a gateway restart.
//! Signing out writes a new one and bumps the generation, which ends every ticket and socket the
//! old one opened. Native clients never hold it.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use subtle::ConstantTimeEq;
use tokio::sync::watch;

pub const FOLDER: &str = "webapp";
pub const FILE: &str = "session-token";

/// The token's length: 32 random bytes in unpadded base64url, as `secrets.token_urlsafe(32)`.
pub const TOKEN_LEN: usize = 43;

/// 32 bytes from the OS in unpadded base64url (43 characters).
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Whether `token` has the shape [`random_token`] gives.
pub fn well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub struct Credential {
    path: PathBuf,
    token: Mutex<String>,
    generation: watch::Sender<u64>,
}

impl Credential {
    /// The stored credential of `home`, created or replaced as [`stored_token`] does.
    pub fn load(home: &Path) -> std::io::Result<Self> {
        let path = home.join(FOLDER).join(FILE);
        let token = stored_token(&path, false)?;
        Ok(Credential {
            path,
            token: Mutex::new(token),
            generation: watch::Sender::new(1),
        })
    }

    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }

    /// Whether `given` is the current token: its generation when it is.
    pub fn matches(&self, given: &str) -> Option<u64> {
        let token = self.token.lock().unwrap_or_else(|e| e.into_inner());
        bool::from(given.as_bytes().ct_eq(token.as_bytes())).then(|| self.generation())
    }

    /// The current token, for the one-use handoff that gives it to a browser.
    pub fn current(&self, generation: u64) -> Option<String> {
        let token = self.token.lock().unwrap_or_else(|e| e.into_inner());
        (self.generation() == generation).then(|| token.clone())
    }

    /// Writes a new token and bumps the generation: every grant of the old one ends.
    pub fn rotate(&self) -> std::io::Result<u64> {
        let mut token = self.token.lock().unwrap_or_else(|e| e.into_inner());
        *token = stored_token(&self.path, true)?;
        let mut next = 0;
        self.generation.send_modify(|g| {
            *g += 1;
            next = *g;
        });
        Ok(next)
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }
}

/// Replaces the stored credential of a home whose gateway is not running, so no browser can use
/// the old one when one starts (`hermes webapp --sign-out` with no gateway).
pub fn rotate_stored(home: &Path) -> std::io::Result<()> {
    stored_token(&home.join(FOLDER).join(FILE), true).map(|_| ())
}

fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// The token at `path`: read when it is well formed in a regular file of this user that nobody
/// else can read, else (or with `rotate`) a new one written atomically. Its folder is created 0700,
/// or closed to others again; a folder that is a link, not a directory or another user's is refused.
fn stored_token(path: &Path, rotate: bool) -> std::io::Result<String> {
    let folder = path.parent().expect("the token lives in a folder");
    private_folder(folder)?;
    if !rotate {
        if let Some(token) = read_private(path) {
            return Ok(token);
        }
    }
    let token = random_token();
    let staged = folder.join(format!(".{FILE}.{}", uuid::Uuid::new_v4().simple()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(format!("{token}\n").as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&staged, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&staged);
        return Err(std::io::Error::new(
            e.kind(),
            format!(
                "could not store the Webapp's browser credential in {} ({e})",
                path.display()
            ),
        ));
    }
    Ok(token)
}

fn private_folder(folder: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(folder) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(folder)?;
            Ok(())
        }
        Err(e) => Err(e),
        Ok(meta) if !meta.is_dir() || meta.uid() != own_uid() => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a private folder of this user, so the Webapp's browser credential \
                 cannot be kept there",
                folder.display()
            ),
        )),
        Ok(meta) if meta.mode() & 0o077 != 0 => {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
        }
        Ok(_) => Ok(()),
    }
}

fn read_private(path: &Path) -> Option<String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != own_uid() {
        return None;
    }
    let mut text = String::new();
    file.take(256).read_to_string(&mut text).ok()?;
    let token = text.trim_start_matches('\u{feff}').trim();
    well_formed(token).then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_file(home: &Path) -> PathBuf {
        home.join(FOLDER).join(FILE)
    }

    #[test]
    fn a_new_credential_is_private_and_kept_across_loads() {
        let home = tempfile::tempdir().unwrap();
        let first = Credential::load(home.path()).unwrap();
        let token = first.current(1).unwrap();
        assert!(well_formed(&token), "{token}");
        let path = token_file(home.path());
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let folder = std::fs::metadata(path.parent().unwrap()).unwrap();
        assert_eq!(folder.mode() & 0o777, 0o700);
        let again = Credential::load(home.path()).unwrap();
        assert_eq!(again.current(1), Some(token.clone()));
        assert_eq!(again.matches(&token), Some(1));
        assert_eq!(again.matches("x"), None);
    }

    #[test]
    fn a_readable_malformed_or_linked_token_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        let path = token_file(home.path());
        let token = Credential::load(home.path()).unwrap().current(1).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let replaced = Credential::load(home.path()).unwrap().current(1).unwrap();
        assert_ne!(replaced, token);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);

        std::fs::write(&path, "short\n").unwrap();
        let fresh = Credential::load(home.path()).unwrap().current(1).unwrap();
        assert!(well_formed(&fresh));

        // A link planted at the token path is replaced, and its target is neither read nor written.
        let outside = home.path().join("outside");
        let planted = random_token();
        std::fs::write(&outside, format!("{planted}\n")).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        let after = Credential::load(home.path()).unwrap().current(1).unwrap();
        assert_ne!(after, planted);
        assert!(!std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&outside).unwrap().trim(), planted);
    }

    #[test]
    fn an_open_folder_is_closed_and_a_linked_folder_refused() {
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join(FOLDER);
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        Credential::load(home.path()).unwrap();
        assert_eq!(std::fs::metadata(&folder).unwrap().mode() & 0o777, 0o700);

        let other = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), other.path().join(FOLDER)).unwrap();
        assert!(Credential::load(other.path()).is_err());
        assert!(!elsewhere.path().join(FILE).exists());
    }

    #[test]
    fn rotation_replaces_the_token_and_bumps_the_generation() {
        let home = tempfile::tempdir().unwrap();
        let credential = Credential::load(home.path()).unwrap();
        let old = credential.current(1).unwrap();
        let mut watcher = credential.subscribe();
        assert_eq!(credential.rotate().unwrap(), 2);
        assert!(watcher.has_changed().unwrap());
        assert_eq!(*watcher.borrow_and_update(), 2);
        assert_eq!(credential.matches(&old), None);
        assert_eq!(credential.current(1), None);
        let new = credential.current(2).unwrap();
        assert_eq!(credential.matches(&new), Some(2));
        assert_eq!(
            std::fs::read_to_string(token_file(home.path()))
                .unwrap()
                .trim(),
            new
        );
        rotate_stored(home.path()).unwrap();
        assert_ne!(
            std::fs::read_to_string(token_file(home.path()))
                .unwrap()
                .trim(),
            new
        );
    }
}
