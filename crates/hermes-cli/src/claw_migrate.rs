//! `hermes claw migrate` — OpenClaw migration command.
//!
//! Ported from Python `hermes_cli/claw.py`.
//! Detects `~/.openclaw` directory and imports settings to `~/.hermes/`.
//!
//! A link in the source is never followed and nothing is written through a
//! link in the home. A file, skill or name that looks like it holds a
//! credential is left out and reported; the API keys in `.env` are the
//! exception, merged into this home's own `.env`, written owner-only.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::agent_import::{
    check_destination, looks_secret, read_regular_to_string, write_file, HIDDEN_NAME,
};
use crate::source_tree::{read_all, Entry, SourceDir, Tree};

/// Known OpenClaw directory names (current + legacy).
const OPENCLAW_DIR_NAMES: &[&str] = &[".openclaw", ".clawdbot", ".moldbot"];

/// Files to migrate from OpenClaw.
const PERSONALITY_FILES: &[&str] = &["SOUL.md", "MEMORY.md", "USER.md"];

/// Result of a migration operation.
#[derive(Debug, Clone)]
pub struct MigrationResult {
    pub migrated: Vec<MigrationItem>,
    pub skipped: Vec<MigrationItem>,
    pub errors: Vec<MigrationItem>,
}

/// A single migration item.
#[derive(Debug, Clone)]
pub struct MigrationItem {
    pub kind: String,
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
    pub status: MigrationStatus,
    pub reason: Option<String>,
}

/// Status of a migration item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationStatus {
    Migrated,
    Skipped,
    Conflict,
    Error,
}

/// Options for the migration command.
#[derive(Debug, Clone)]
pub struct MigrateOptions {
    /// Source directory (defaults to `~/.openclaw`).
    pub source: Option<PathBuf>,
    /// Whether to only preview changes without executing.
    pub dry_run: bool,
    /// Migration preset: "full", "minimal", "custom".
    pub preset: String,
    /// Whether to overwrite existing files.
    pub overwrite: bool,
}

impl Default for MigrateOptions {
    fn default() -> Self {
        Self {
            source: None,
            dry_run: false,
            preset: "full".to_string(),
            overwrite: false,
        }
    }
}

/// Find the OpenClaw source directory: `explicit_source`, else the first
/// known name in the home folder that is a folder. A lookup that fails other
/// than with not-found is an error, so a folder that cannot be looked up is
/// never taken for a missing one.
pub fn find_openclaw_dir(explicit_source: Option<&Path>) -> Result<Option<PathBuf>, String> {
    let candidates = match (explicit_source, dirs::home_dir()) {
        (Some(source), _) => vec![source.to_path_buf()],
        (None, Some(home)) => OPENCLAW_DIR_NAMES
            .iter()
            .map(|name| home.join(name))
            .collect(),
        (None, None) => Vec::new(),
    };
    for candidate in candidates {
        match std::fs::metadata(&candidate) {
            Ok(meta) if meta.is_dir() => return Ok(Some(candidate)),
            Ok(_) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(err) => return Err(format!("could not look up {}: {err}", candidate.display())),
        }
    }
    Ok(None)
}

/// Get the hermes home directory, as `hermes_config::hermes_home` resolves it.
pub fn get_hermes_home() -> Option<PathBuf> {
    Some(hermes_config::hermes_home())
}

/// Run the OpenClaw → Hermes migration.
pub fn run_migration(options: &MigrateOptions) -> MigrationResult {
    let mut result = MigrationResult {
        migrated: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };

    let source_dir = match find_openclaw_dir(options.source.as_deref()) {
        Ok(Some(dir)) => dir,
        found => {
            let reason = found
                .err()
                .unwrap_or_else(|| "OpenClaw directory not found".to_string());
            result.errors.push(MigrationItem {
                kind: "source".to_string(),
                source: options.source.clone(),
                destination: None,
                status: MigrationStatus::Error,
                reason: Some(reason),
            });
            return result;
        }
    };

    let hermes_home = match get_hermes_home() {
        Some(h) => h,
        None => {
            result.errors.push(MigrationItem {
                kind: "target".to_string(),
                source: None,
                destination: None,
                status: MigrationStatus::Error,
                reason: Some("Could not determine home directory".to_string()),
            });
            return result;
        }
    };

    // Ensure hermes home exists
    if !options.dry_run {
        let _ = std::fs::create_dir_all(&hermes_home);
    }

    // Every entry is opened relative to this, never through a link.
    let root = match SourceDir::open(&source_dir) {
        Ok(root) => root,
        Err(err) => {
            let reason = format!("could not open it: {err}");
            let failed = Err((MigrationStatus::Error, reason));
            record(
                &mut result,
                "source".to_string(),
                Some(source_dir),
                None,
                failed,
            );
            return result;
        }
    };

    // Personality files (SOUL.md, MEMORY.md, USER.md)
    for &filename in PERSONALITY_FILES {
        let dst = hermes_home.join(filename);
        let outcome = match look_up(&root, filename) {
            Ok(None) => Err((
                MigrationStatus::Skipped,
                "Source file not found".to_string(),
            )),
            Ok(Some(entry)) => migrate_file(entry, &hermes_home, &dst, options),
            Err(failed) => Err(failed),
        };
        let kind = format!("personality:{}", filename);
        let src = source_dir.join(filename);
        record(&mut result, kind, Some(src), Some(dst), outcome);
    }

    // Skills, each to skills/openclaw-imports/
    let skills = hermes_home.join("skills").join("openclaw-imports");
    for_each_entry(
        &root,
        &source_dir,
        "skills",
        "skill",
        &mut result,
        |entry, name| {
            let dst = skills.join(name);
            let outcome = match entry {
                Entry::Dir(skill) => migrate_skill(&skill, &hermes_home, &dst, options),
                entry => migrate_file(entry, &hermes_home, &dst, options),
            };
            (Some(dst), outcome)
        },
    );

    // Session files, each to sessions/
    let sessions = hermes_home.join("sessions");
    for_each_entry(
        &root,
        &source_dir,
        "sessions",
        "session",
        &mut result,
        |entry, name| {
            if let Entry::Dir(_) = entry {
                let reason = "it is a folder; only session files are copied".to_string();
                return (None, Err((MigrationStatus::Skipped, reason)));
            }
            let dst = sessions.join(name);
            let outcome = migrate_file(entry, &hermes_home, &dst, options);
            (Some(dst), outcome)
        },
    );

    // .env file (API keys): this home keeps its keys in its own .env.
    let dst_env = hermes_home.join(".env");
    let outcome = match look_up(&root, ".env") {
        Ok(None) => None,
        Ok(Some(entry)) => Some(migrate_env(entry, &hermes_home, &dst_env, options)),
        Err(failed) => Some(Err(failed)),
    };
    if let Some(outcome) = outcome {
        let src_env = source_dir.join(".env");
        let kind = "api-keys".to_string();
        record(&mut result, kind, Some(src_env), Some(dst_env), outcome);
    }

    // Platform configs (config.yaml / gateway.json)
    for config_name in ["config.yaml", "gateway.json"] {
        let dst = hermes_home.join(config_name);
        let outcome = match look_up(&root, config_name) {
            Ok(None) => continue,
            Ok(Some(entry)) => migrate_file(entry, &hermes_home, &dst, options),
            Err(failed) => Err(failed),
        };
        let kind = format!("config:{}", config_name);
        let src = source_dir.join(config_name);
        record(&mut result, kind, Some(src), Some(dst), outcome);
    }

    result
}

/// The entry `name` of `dir`, opened without following a link. A lookup that
/// fails other than with not-found is an error, so an entry that cannot be
/// looked up is never taken for a missing one.
fn look_up(dir: &SourceDir, name: impl AsRef<OsStr>) -> Result<Option<Entry>, Failure> {
    dir.entry(name).map_err(|err| {
        (
            MigrationStatus::Error,
            format!("could not look it up: {err}"),
        )
    })
}

/// Why a link is skipped.
const LINK_REASON: &str = "it is a link, which this migration does not follow";
/// Why a file or folder holding what looks like a credential is skipped.
const SECRET_REASON: &str = "it looks like it holds a credential; add it yourself";

/// Why an item was not migrated: a status and the reason.
type Failure = (MigrationStatus, String);
/// What became of one item: migrated (with an optional note), or why not.
type Outcome = Result<Option<String>, Failure>;

/// Files `outcome` under migrated, errors or skipped (skips and conflicts).
fn record(
    result: &mut MigrationResult,
    kind: String,
    source: Option<PathBuf>,
    destination: Option<PathBuf>,
    outcome: Outcome,
) {
    let (list, status, reason) = match outcome {
        Ok(note) => (&mut result.migrated, MigrationStatus::Migrated, note),
        Err((MigrationStatus::Error, reason)) => {
            (&mut result.errors, MigrationStatus::Error, Some(reason))
        }
        Err((status, reason)) => (&mut result.skipped, status, Some(reason)),
    };
    list.push(MigrationItem {
        kind,
        source,
        destination,
        status,
        reason,
    });
}

/// The bytes of the source file `entry`. A link is skipped; a folder, FIFO,
/// socket or device is an error.
fn read_file(entry: Entry) -> Result<Vec<u8>, Failure> {
    match entry {
        Entry::File(file) => read_all(file).map_err(|e| (MigrationStatus::Error, e.to_string())),
        Entry::Link => Err((MigrationStatus::Skipped, LINK_REASON.to_string())),
        Entry::Dir(_) => Err((MigrationStatus::Error, "it is a folder".to_string())),
        Entry::Other => Err((
            MigrationStatus::Error,
            "it is not a regular file".to_string(),
        )),
    }
}

/// Copies the source file `entry` to `dst` in `home`, owner-only. A link, a
/// file that looks like it holds a credential and a destination reached
/// through a link are refused.
fn migrate_file(entry: Entry, home: &Path, dst: &Path, options: &MigrateOptions) -> Outcome {
    let content = read_file(entry)?;
    if looks_secret(&String::from_utf8_lossy(&content)) {
        return Err((MigrationStatus::Skipped, SECRET_REASON.to_string()));
    }
    check_destination(home, dst).map_err(|err| (MigrationStatus::Error, err))?;
    if dst.exists() && !options.overwrite {
        return Err((MigrationStatus::Conflict, "Already exists".to_string()));
    }
    if options.dry_run {
        return Ok(Some("(dry run)".to_string()));
    }
    write_file(dst, &content)
        .map(|()| None)
        .map_err(|err| (MigrationStatus::Error, err))
}

/// Hands each entry of the folder `folder` in `root` (at `source`) to
/// `migrate`, which gives the destination and outcome to record under the
/// kind `<item>:<name>`. The folder is passed over when missing; a folder
/// that is a link or cannot be looked up or listed is recorded instead, as is
/// an entry whose name looks like a credential (under a hidden name).
fn for_each_entry(
    root: &SourceDir,
    source: &Path,
    folder: &str,
    item: &str,
    result: &mut MigrationResult,
    mut migrate: impl FnMut(Entry, &OsStr) -> (Option<PathBuf>, Outcome),
) {
    let src = source.join(folder);
    let listed = match look_up(root, folder) {
        Ok(None) => return,
        Ok(Some(Entry::Dir(dir))) => dir
            .names()
            .map(|names| (dir, names))
            .map_err(|e| (MigrationStatus::Error, format!("could not read it: {e}"))),
        Ok(Some(Entry::Link)) => Err((MigrationStatus::Skipped, LINK_REASON.to_string())),
        Ok(Some(_)) => Err((MigrationStatus::Error, "it is not a folder".to_string())),
        Err(failed) => Err(failed),
    };
    let (dir, names) = match listed {
        Ok(listed) => listed,
        Err(why) => return record(result, folder.to_string(), Some(src), None, Err(why)),
    };
    for name in names {
        let shown = name.to_string_lossy();
        if looks_secret(&shown) {
            let reason = "its name looks like a credential; add it yourself".to_string();
            let kind = format!("{item}:{HIDDEN_NAME}");
            record(
                result,
                kind,
                None,
                None,
                Err((MigrationStatus::Skipped, reason)),
            );
            continue;
        }
        let kind = format!("{item}:{shown}");
        let path = src.join(&name);
        let (dst, outcome) = match look_up(&dir, &name) {
            Ok(Some(entry)) => migrate(entry, &name),
            Ok(None) => (
                None,
                Err((
                    MigrationStatus::Error,
                    "it went away while it was read".to_string(),
                )),
            ),
            Err(failed) => (None, Err(failed)),
        };
        record(result, kind, Some(path), dst, outcome);
    }
}

/// Copies the skill folder `skill` to the new folder `dst` in `home` (links
/// inside it are not copied). A folder holding what looks like a credential
/// and an existing folder are skipped: a folder is never replaced. What is
/// checked for credentials is what is copied.
fn migrate_skill(skill: &SourceDir, home: &Path, dst: &Path, options: &MigrateOptions) -> Outcome {
    let tree = Tree::read(skill).map_err(|e| {
        (
            MigrationStatus::Error,
            format!("could not check it for credentials: {e}"),
        )
    })?;
    if tree.holds_secret() {
        return Err((MigrationStatus::Skipped, SECRET_REASON.to_string()));
    }
    check_destination(home, dst).map_err(|err| (MigrationStatus::Error, err))?;
    if dst.exists() {
        return Err((MigrationStatus::Conflict, "Already exists".to_string()));
    }
    if options.dry_run {
        return Ok(Some("(dry run)".to_string()));
    }
    dst.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| tree.write(dst))
        .map_err(|e| (MigrationStatus::Error, format!("could not copy it: {e}")))?;
    let links = tree.links;
    Ok((links > 0).then(|| format!("{links} links inside it are not copied")))
}

/// Merges the source `.env` file `entry` into this home's `.env` at `dst`,
/// written owner-only.
fn migrate_env(entry: Entry, home: &Path, dst: &Path, options: &MigrateOptions) -> Outcome {
    let content = String::from_utf8(read_file(entry)?).map_err(|e| {
        (
            MigrationStatus::Error,
            format!("Failed to read source .env: {e}"),
        )
    })?;
    check_destination(home, dst).map_err(|err| (MigrationStatus::Error, err))?;
    if dst.exists() && !options.overwrite {
        return Err((
            MigrationStatus::Conflict,
            "Already exists (use --overwrite)".to_string(),
        ));
    }
    if options.dry_run {
        return Ok(Some("(dry run)".to_string()));
    }
    merge_env_files(&content, dst)
        .map(|count| Some(format!("{} keys imported", count)))
        .map_err(|err| (MigrationStatus::Error, err))
}

/// Merge environment variables from the source .env text into destination
/// .env, written owner-only. Returns the number of keys imported.
fn merge_env_files(src_content: &str, dst: &Path) -> Result<usize, String> {
    let existing = if dst.exists() {
        read_regular_to_string(dst).map_err(|e| format!("Failed to read .env: {}", e))?
    } else {
        String::new()
    };

    let existing_keys: std::collections::HashSet<String> = existing
        .lines()
        .filter(|l| !l.starts_with('#') && l.contains('='))
        .filter_map(|l| l.split('=').next().map(|k| k.trim().to_string()))
        .collect();

    let mut new_lines = Vec::new();
    let mut count = 0;

    for line in src_content.lines() {
        if line.starts_with('#') || !line.contains('=') {
            continue;
        }
        if let Some(key) = line.split('=').next() {
            let key = key.trim();
            if !existing_keys.contains(key) {
                new_lines.push(line.to_string());
                count += 1;
            }
        }
    }

    if !new_lines.is_empty() {
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("# Imported from OpenClaw\n");
        content.push_str(&new_lines.join("\n"));
        content.push('\n');

        write_file(dst, content.as_bytes()).map_err(|e| format!("Failed to write .env: {}", e))?;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_target_is_the_own_home_not_upstreams() {
        let _lock = crate::test_env_lock::lock();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".hermes")).unwrap();
        let keys = ["HOME", "HERMES_HOME", "HERMES_AGENT_ULTRA_HOME"];
        let saved: Vec<_> = keys.iter().map(std::env::var_os).collect();
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("HERMES_HOME");
        std::env::remove_var("HERMES_AGENT_ULTRA_HOME");
        let target = get_hermes_home();
        for (key, value) in keys.iter().zip(saved) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        assert_eq!(target, Some(tmp.path().join(".hermes-agent-ultra")));
    }

    #[test]
    fn test_find_openclaw_dir_none() {
        let result = find_openclaw_dir(Some(Path::new("/nonexistent")));
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn test_find_openclaw_dir_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        let result = find_openclaw_dir(Some(tmp.path()));
        assert_eq!(result, Ok(Some(tmp.path().to_path_buf())));
    }

    #[test]
    fn test_migration_dry_run() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SOUL.md"), "Test personality").unwrap();

        let options = MigrateOptions {
            source: Some(source.clone()),
            dry_run: true,
            preset: "full".to_string(),
            overwrite: false,
        };

        let result = run_migration(&options);
        // In dry run, personality files from source should be listed as migrated
        // (or skipped if hermes home can't be determined)
        let total = result.migrated.len() + result.skipped.len() + result.errors.len();
        assert!(total > 0, "Expected at least one migration item");
    }

    #[test]
    fn test_migration_no_source() {
        let options = MigrateOptions {
            source: Some(PathBuf::from("/nonexistent/openclaw")),
            dry_run: true,
            ..Default::default()
        };

        let result = run_migration(&options);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_merge_env_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("dst.env");

        std::fs::write(&dst, "KEY1=existing\n").unwrap();

        let count = merge_env_files("KEY1=value1\nKEY2=value2\n", &dst).unwrap();
        assert_eq!(count, 1); // Only KEY2 should be imported

        let content = std::fs::read_to_string(&dst).unwrap();
        assert!(content.contains("KEY2=value2"));
        assert!(content.contains("KEY1=existing"));
    }

    /// Migrates `source` into `home` (as `HERMES_HOME`) and returns the result.
    fn migrate_into(source: &Path, home: &Path) -> MigrationResult {
        let _lock = crate::test_env_lock::lock();
        let saved = std::env::var_os("HERMES_HOME");
        std::env::set_var("HERMES_HOME", home);
        let result = run_migration(&MigrateOptions {
            source: Some(source.to_path_buf()),
            ..Default::default()
        });
        match saved {
            Some(value) => std::env::set_var("HERMES_HOME", value),
            None => std::env::remove_var("HERMES_HOME"),
        }
        result
    }

    /// Every file name and file text under `dir`.
    fn all_text(dir: &Path) -> String {
        let mut text = String::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            text.push_str(&entry.file_name().to_string_lossy());
            if path.is_dir() {
                text.push_str(&all_text(&path));
            } else if let Ok(bytes) = std::fs::read(&path) {
                text.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        text
    }

    fn items(result: &MigrationResult) -> String {
        format!("{result:?}")
    }

    #[test]
    fn files_holding_or_named_like_credentials_are_not_carried_over() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let home = tmp.path().join("home");
        for skill in ["notes", "keys", "names", "ghp_fixtureClaw03"] {
            std::fs::create_dir_all(source.join("skills").join(skill)).unwrap();
            std::fs::write(
                source.join("skills").join(skill).join("SKILL.md"),
                "A skill.\n",
            )
            .unwrap();
        }
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(source.join("SOUL.md"), "Be kind.\n").unwrap();
        std::fs::write(source.join("MEMORY.md"), "- token ghp_fixtureClaw01\n").unwrap();
        std::fs::write(source.join("skills/keys/keys.txt"), "ghp_fixtureClaw02\n").unwrap();
        std::fs::write(source.join("skills/names/ghp_fixtureClaw04.md"), "x\n").unwrap();
        std::fs::write(
            source.join("gateway.json"),
            "{\"token\": \"ghp_fixtureClaw05\"}\n",
        )
        .unwrap();

        let result = migrate_into(&source, &home);
        let report = items(&result);
        let files = all_text(&home);
        for planted in [
            "ghp_fixtureClaw01",
            "ghp_fixtureClaw02",
            "ghp_fixtureClaw03",
            "ghp_fixtureClaw04",
            "ghp_fixtureClaw05",
        ] {
            assert!(!files.contains(planted), "{planted} in the home:\n{files}");
            assert!(
                !report.contains(planted),
                "{planted} in the report:\n{report}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(home.join("SOUL.md")).unwrap(),
            "Be kind.\n"
        );
        assert!(home
            .join("skills/openclaw-imports/notes/SKILL.md")
            .is_file());
        let credential_skips = result
            .skipped
            .iter()
            .filter(|item| {
                item.reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("credential"))
            })
            .count();
        // MEMORY.md, gateway.json and the keys, names and credential-named skills.
        assert_eq!(credential_skips, 5, "{report}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_skills_folder_is_an_error_not_a_quiet_skip() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(source.join("skills/notes")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(source.join("skills/notes/SKILL.md"), "A skill.\n").unwrap();
        let skills = source.join("skills");
        std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(&skills).is_err();

        let result = migrate_into(&source, &home);
        std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !unreadable {
            return; // running as root: permissions do not stop reads
        }
        assert!(
            result.errors.iter().any(|item| item.kind == "skills"),
            "{}",
            items(&result)
        );
    }

    #[cfg(unix)]
    #[test]
    fn entries_that_cannot_be_looked_up_are_errors_not_missing() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(source.join("skills/notes")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(source.join("SOUL.md"), "Be kind.\n").unwrap();
        std::fs::write(source.join(".env"), "OPENAI_API_KEY=fixture\n").unwrap();
        std::fs::write(source.join("config.yaml"), "model: x\n").unwrap();
        // Readable but not searchable: its entries cannot be looked up.
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let unreadable = std::fs::symlink_metadata(source.join("SOUL.md")).is_err();

        let result = migrate_into(&source, &home);
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !unreadable {
            return; // running as root: permissions do not stop lookups
        }
        let report = items(&result);
        assert!(result.migrated.is_empty(), "{report}");
        assert!(!report.contains("Source file not found"), "{report}");
        for kind in [
            "personality:SOUL.md",
            "skills",
            "api-keys",
            "config:config.yaml",
        ] {
            assert!(
                result.errors.iter().any(|item| item.kind == kind),
                "no error for {kind}: {report}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_the_source_is_an_error_not_a_hang() {
        use crate::agent_import::fifo_fixture::{make_fifo, run_unblocking};
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(source.join("skills/piped")).unwrap();
        std::fs::create_dir_all(source.join("sessions")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(source.join("skills/piped/SKILL.md"), "piped\n").unwrap();
        let fifos = [
            "SOUL.md",
            ".env",
            "config.yaml",
            "sessions/s1.jsonl",
            "skills/piped/pipe",
        ]
        .map(|name| source.join(name));
        for fifo in &fifos {
            make_fifo(fifo);
        }

        let (from, to) = (source.clone(), home.clone());
        let result = run_unblocking(fifos.to_vec(), move || migrate_into(&from, &to)).unwrap();
        let report = items(&result);
        assert!(result.migrated.is_empty(), "{report}");
        for kind in [
            "personality:SOUL.md",
            "api-keys",
            "config:config.yaml",
            "session:s1.jsonl",
            "skill:piped",
        ] {
            assert!(
                result.errors.iter().any(|item| item.kind == kind),
                "no error for {kind}: {report}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_source_that_cannot_be_looked_up_is_an_error_not_missing() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(locked.join(".openclaw")).unwrap();
        // Readable but not searchable: nothing in it can be looked up.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
        let unreadable = std::fs::metadata(locked.join(".openclaw")).is_err();

        let named = run_migration(&MigrateOptions {
            source: Some(locked.join(".openclaw")),
            dry_run: true,
            ..Default::default()
        });
        let searched = {
            let _lock = crate::test_env_lock::lock();
            let saved = std::env::var_os("HOME");
            std::env::set_var("HOME", &locked);
            let result = run_migration(&MigrateOptions {
                dry_run: true,
                ..Default::default()
            });
            match saved {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            result
        };
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !unreadable {
            return; // running as root: permissions do not stop lookups
        }
        for result in [named, searched] {
            let report = items(&result);
            assert!(!report.contains("not found"), "{report}");
            assert!(
                result.errors.iter().any(|item| item.kind == "source"
                    && item
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("could not look up"))),
                "{report}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn api_keys_are_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(source.join(".env"), "OPENROUTER_API_KEY=fixture-value\n").unwrap();

        let result = migrate_into(&source, &home);
        assert!(result.errors.is_empty(), "{}", items(&result));
        let mode = std::fs::metadata(home.join(".env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn links_are_followed_neither_out_of_the_source_nor_out_of_the_home() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(source.join("skills")).unwrap();
        std::fs::create_dir_all(outside.join("skill")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(outside.join("soul.md"), "outside soul\n").unwrap();
        std::fs::write(outside.join("skill/SKILL.md"), "outside skill\n").unwrap();
        symlink(outside.join("soul.md"), source.join("SOUL.md")).unwrap();
        symlink(outside.join("skill"), source.join("skills/linked")).unwrap();
        std::fs::write(source.join("USER.md"), "A user.\n").unwrap();
        let elsewhere = tmp.path().join("elsewhere.md");
        symlink(&elsewhere, home.join("USER.md")).unwrap();

        let result = migrate_into(&source, &home);
        let files = all_text(&home);
        assert!(!files.contains("outside"), "{files}");
        assert!(!elsewhere.exists(), "written through the home's link");
        assert!(!result.errors.is_empty(), "{}", items(&result));
        assert!(result
            .skipped
            .iter()
            .any(|item| item.kind == "personality:SOUL.md"
                && item
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("link"))));
    }

    /// A source with `sessions/` holding `files`, and an empty home.
    fn sessions_fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(source.join("sessions")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        for (name, text) in files {
            std::fs::write(source.join("sessions").join(name), text).unwrap();
        }
        (tmp, source, home)
    }

    /// Whether `items` has `kind` with a reason containing `word`.
    fn has(items: &[MigrationItem], kind: &str, word: &str) -> bool {
        items.iter().any(|item| {
            item.kind == kind && item.reason.as_deref().is_some_and(|r| r.contains(word))
        })
    }

    #[test]
    fn a_session_holding_a_credential_is_skipped_and_the_rest_copied() {
        let (_tmp, source, home) =
            sessions_fixture(&[("ok.json", "hello\n"), ("k.json", "ghp_fixtureClaw06\n")]);

        let result = migrate_into(&source, &home);
        let report = items(&result);
        assert_eq!(
            std::fs::read_to_string(home.join("sessions/ok.json")).unwrap(),
            "hello\n"
        );
        assert!(!all_text(&home).contains("ghp_fixtureClaw06"), "{report}");
        assert!(
            has(&result.skipped, "session:k.json", "credential"),
            "{report}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_session_is_skipped_not_followed() {
        let (tmp, source, home) = sessions_fixture(&[]);
        let outside = tmp.path().join("outside.json");
        std::fs::write(&outside, "outside text\n").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("sessions/x.json")).unwrap();

        let result = migrate_into(&source, &home);
        assert!(!all_text(&home).contains("outside text"));
        assert!(
            has(&result.skipped, "session:x.json", "link"),
            "{}",
            items(&result)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_homes_sessions_folder_is_not_written_through() {
        let (tmp, source, home) = sessions_fixture(&[("a.json", "hello\n")]);
        let elsewhere = tmp.path().join("elsewhere.json");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.join("sessions/a.json")).unwrap();

        let result = migrate_into(&source, &home);
        assert!(!elsewhere.exists(), "written through the home's link");
        assert!(
            result
                .errors
                .iter()
                .any(|item| item.kind == "session:a.json"),
            "{}",
            items(&result)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_session_that_cannot_be_read_is_an_error_not_a_quiet_skip() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, source, home) = sessions_fixture(&[("u.json", "hello\n")]);
        let session = source.join("sessions/u.json");
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read(&session).is_err();

        let result = migrate_into(&source, &home);
        if !unreadable {
            return; // running as root: permissions do not stop reads
        }
        assert!(
            result
                .errors
                .iter()
                .any(|item| item.kind == "session:u.json"),
            "{}",
            items(&result)
        );
    }

    #[cfg(unix)]
    #[test]
    fn copied_skill_files_are_owner_only_and_keep_their_execute_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let home = tmp.path().join("home");
        let skill = source.join("skills/notes");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        for (name, mode) in [("SKILL.md", 0o644), ("run.sh", 0o755)] {
            std::fs::write(skill.join(name), "A skill.\n").unwrap();
            std::fs::set_permissions(skill.join(name), std::fs::Permissions::from_mode(mode))
                .unwrap();
        }

        migrate_into(&source, &home);
        let copied = home.join("skills/openclaw-imports/notes");
        let mode = |name: &str| {
            std::fs::metadata(copied.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode("SKILL.md"), 0o600);
        assert_eq!(mode("run.sh"), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_swapped_for_a_link_while_it_is_read_is_never_followed() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        let outside = tmp.path().join("outside.md");
        std::fs::create_dir_all(source.join("skills/notes")).unwrap();
        std::fs::write(source.join("skills/notes/SKILL.md"), "A skill.\n").unwrap();
        std::fs::write(&outside, "OUTSIDE THE SOURCE\n").unwrap();
        // Each entry keeps flipping between a file and a link out of the source.
        let flipped = [source.join("SOUL.md"), source.join("skills/notes/data.md")];
        let stop = Arc::new(AtomicBool::new(false));
        let flipper = {
            let (stop, outside) = (stop.clone(), outside.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for path in &flipped {
                        let _ = std::fs::remove_file(path);
                        if let Ok(mut file) = std::fs::File::options()
                            .write(true)
                            .create_new(true)
                            .open(path)
                        {
                            let _ = std::io::Write::write_all(&mut file, b"inside\n");
                        }
                        let _ = std::fs::remove_file(path);
                        let _ = symlink(&outside, path);
                    }
                }
            })
        };

        let mut followed = Vec::new();
        for run in 0..300 {
            let home = tmp.path().join(format!("home-{run}"));
            std::fs::create_dir_all(&home).unwrap();
            migrate_into(&source, &home);
            if all_text(&home).contains("OUTSIDE") {
                followed.push(run);
            }
        }
        stop.store(true, Ordering::Relaxed);
        flipper.join().unwrap();
        assert!(
            followed.is_empty(),
            "a link was followed in runs {followed:?}"
        );
    }

    #[test]
    fn test_migration_status_eq() {
        assert_eq!(MigrationStatus::Migrated, MigrationStatus::Migrated);
        assert_ne!(MigrationStatus::Migrated, MigrationStatus::Skipped);
    }
}
