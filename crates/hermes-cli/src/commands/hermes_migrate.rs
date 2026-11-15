//! `migrate`: move a Hermes Agent home into a Reformed home.
//!
//! A dry run unless `--apply`. The Hermes home is only read: links and special
//! files are never followed or copied, `state.db` is opened read-only without
//! creating sidecar files, and no file content is printed. Every write is a new
//! entry, published without replacing anything, and a failed run removes what
//! it created.

mod cron;
mod state_db;
#[cfg(test)]
mod tests;

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use hermes_agent::session_persistence::SessionPersistence;
use hermes_core::AgentError;
use tempfile::NamedTempFile;

use super::hermes_history::{
    create_private_profile, new_profile_path, HermesHistorySummary, SessionImporter,
};

/// Files copied byte for byte; `config.yaml` and `.env` hold secrets.
const FILES: [&str; 3] = ["config.yaml", ".env", "SOUL.md"];
/// Folders copied as trees.
const TREES: [&str; 4] = ["memories", "skills", "plugins", "scripts"];
/// Largest `cron/jobs.json` read.
const MAX_JOBS_BYTES: u64 = 64 * 1024 * 1024;

pub struct MigrateRequest<'a> {
    pub from: &'a Path,
    pub to: &'a Path,
    pub apply: bool,
    pub into_existing: bool,
}

/// One line per item; `failed` means the plan has an error, so `--apply`
/// wrote nothing.
#[derive(Debug, Default)]
pub struct MigrateReport {
    pub lines: Vec<String>,
    pub failed: bool,
}

/// Hermes's own default home: `~/.hermes`, or `%LOCALAPPDATA%\hermes` on
/// Windows, with `HERMES_DATA_DIR_SUFFIX` appended. Never `HERMES_HOME`, which
/// names Reformed's home here.
pub fn default_hermes_home() -> PathBuf {
    let mut name = std::ffi::OsString::from(if cfg!(windows) { "hermes" } else { ".hermes" });
    name.push(std::env::var_os("HERMES_DATA_DIR_SUFFIX").unwrap_or_default());
    #[cfg(windows)]
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .map_or_else(
            || hermes_config::user_home_dir().join("AppData").join("Local"),
            PathBuf::from,
        );
    #[cfg(not(windows))]
    let base = hermes_config::user_home_dir();
    base.join(name)
}

fn refuse(message: String) -> AgentError {
    AgentError::Config(message)
}

/// Plan the migration and, with `apply`, carry it out.
pub fn migrate_hermes(request: &MigrateRequest<'_>) -> Result<MigrateReport, AgentError> {
    let from = fs::canonicalize(request.from)
        .ok()
        .filter(|path| path.is_dir())
        .ok_or_else(|| refuse(format!("No Hermes home at {}", request.from.display())))?;
    let existing = present(request.to);
    if existing && !request.into_existing {
        return Err(refuse(format!(
            "{} already exists; pass --into-existing to add to it without replacing anything",
            request.to.display()
        )));
    }
    let to = if existing {
        fs::canonicalize(request.to)
    } else {
        new_profile_path(request.to)
    }
    .map_err(|error| refuse(format!("Reformed home {}: {error}", request.to.display())))?;
    if existing && !to.is_dir() {
        return Err(refuse(format!("{} is not a folder", request.to.display())));
    }
    if to.starts_with(&from) || from.starts_with(&to) {
        return Err(refuse(
            "The Hermes and Reformed homes must be separate folders, neither inside the other"
                .into(),
        ));
    }
    let header = vec![
        format!("Hermes home: {}", request.from.display()),
        format!(
            "Reformed home: {}{}",
            request.to.display(),
            if existing { " (existing)" } else { " (new)" }
        ),
    ];
    let plan = Run::new(&from, &to, None).items()?;
    if !request.apply || plan.failed {
        let mut report = with_header(header, plan);
        report.lines.push(if request.apply {
            "Nothing was written: fix the errors above, then run again.".into()
        } else {
            "Dry run: nothing was written. Run again with --apply to migrate.".into()
        });
        return Ok(report);
    }
    if !existing {
        create_private_profile(&to).map_err(write_error)?;
    }
    let mut writer = Writer::default();
    let outcome = Run::new(&from, &to, Some(&mut writer)).items();
    match outcome {
        Ok(report) if !report.failed => {
            let mut report = with_header(header, report);
            if existing && cfg!(windows) {
                report
                    .lines
                    .push("Files inherit the existing Reformed home's permissions.".into());
            }
            Ok(report)
        }
        outcome => {
            writer.rollback();
            if !existing {
                let _ = fs::remove_dir(&to);
            }
            let mut report = with_header(header, outcome?);
            report
                .lines
                .push("Nothing was written: the Hermes home changed during the run.".into());
            Ok(report)
        }
    }
}

fn with_header(mut header: Vec<String>, report: MigrateReport) -> MigrateReport {
    header.extend(report.lines);
    MigrateReport {
        lines: header,
        failed: report.failed,
    }
}

fn write_error(error: io::Error) -> AgentError {
    AgentError::Io(format!("Migration write failed: {error}"))
}

/// True unless the path is known to be absent, so an unreadable entry counts
/// as present and is never written over.
fn present(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

enum Source {
    Missing,
    File,
    Dir,
    /// A link, special file or unreadable entry.
    Other,
}

fn source(path: &Path) -> Source {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Source::Missing,
        Ok(meta) if meta.is_file() => Source::File,
        Ok(meta) if meta.is_dir() => Source::Dir,
        _ => Source::Other,
    }
}

/// Opens a regular file without following a link at its last component.
fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Every entry this run created, so a failure removes exactly those.
#[derive(Default)]
struct Writer {
    created: Vec<PathBuf>,
}

impl Writer {
    fn mkdir(&mut self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(path)?;
        self.created.push(path.to_path_buf());
        Ok(())
    }

    fn copy_file(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        let mut input = open_regular(from)?;
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            input.metadata()?.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        let mut temp = NamedTempFile::new_in(parent(to)?)?;
        io::copy(&mut input, temp.as_file_mut())?;
        self.publish(temp, to, executable)
    }

    fn write_new(&mut self, bytes: &[u8], to: &Path) -> io::Result<()> {
        let mut temp = NamedTempFile::new_in(parent(to)?)?;
        temp.write_all(bytes)?;
        self.publish(temp, to, false)
    }

    /// Owner-only on Unix (`0700` for an executable), then a no-clobber rename.
    fn publish(&mut self, temp: NamedTempFile, to: &Path, executable: bool) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if executable { 0o700 } else { 0o600 };
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = executable;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(to).map_err(|error| error.error)?;
        self.created.push(to.to_path_buf());
        Ok(())
    }

    /// Hard-links a staged file into place, which fails rather than replace.
    fn link_new(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(from, fs::Permissions::from_mode(0o600))?;
        }
        fs::hard_link(from, to)?;
        self.created.push(to.to_path_buf());
        Ok(())
    }

    fn rollback(self) {
        for path in self.created.iter().rev() {
            if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
                let _ = fs::remove_dir(path);
            } else {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
}

/// One pass over the items: a plan without a writer, the migration with one.
struct Run<'a> {
    from: &'a Path,
    to: &'a Path,
    writer: Option<&'a mut Writer>,
    report: MigrateReport,
}

impl<'a> Run<'a> {
    fn new(from: &'a Path, to: &'a Path, writer: Option<&'a mut Writer>) -> Self {
        Self {
            from,
            to,
            writer,
            report: MigrateReport::default(),
        }
    }

    fn line(&mut self, line: String) {
        self.report.lines.push(line);
    }

    fn problem(&mut self, line: String) {
        self.report.failed = true;
        self.report.lines.push(format!("error: {line}"));
    }

    fn copied(&self) -> &'static str {
        if self.writer.is_some() {
            "copied"
        } else {
            "to copy"
        }
    }

    fn items(mut self) -> Result<MigrateReport, AgentError> {
        for name in FILES {
            self.file(name).map_err(write_error)?;
        }
        for name in TREES {
            self.tree(name).map_err(write_error)?;
        }
        self.sessions()?;
        self.cron().map_err(write_error)?;
        if present(&self.from.join("auth.json")) {
            self.line("auth.json: not migrated; sign in to each provider again in Reformed".into());
        }
        Ok(self.report)
    }

    fn file(&mut self, name: &str) -> io::Result<()> {
        let to = self.to.join(name);
        match source(&self.from.join(name)) {
            Source::Missing => self.line(format!("{name}: not in the Hermes home")),
            Source::Dir | Source::Other => {
                self.line(format!("{name}: skipped, not a regular file"));
            }
            Source::File if present(&to) => self.line(format!(
                "{name}: skipped, already in the Reformed home and left unchanged"
            )),
            Source::File => {
                if let Some(writer) = self.writer.as_deref_mut() {
                    writer.copy_file(&self.from.join(name), &to)?;
                }
                let copied = self.copied();
                self.line(format!("{name}: {copied}"));
            }
        }
        Ok(())
    }

    fn tree(&mut self, name: &str) -> io::Result<()> {
        let to = self.to.join(name);
        match source(&self.from.join(name)) {
            Source::Missing => self.line(format!("{name}/: not in the Hermes home")),
            Source::File | Source::Other => self.line(format!("{name}/: skipped, not a folder")),
            Source::Dir if present(&to) => self.line(format!(
                "{name}/: skipped, already in the Reformed home and left unchanged"
            )),
            Source::Dir => {
                if let Some(writer) = self.writer.as_deref_mut() {
                    writer.mkdir(&to)?;
                }
                let from = self.from.join(name);
                let mut files = 0;
                let mut skipped = Vec::new();
                self.copy_tree(&from, &to, Path::new(name), &mut files, &mut skipped)?;
                let copied = self.copied();
                self.line(format!("{name}/: {copied}, {files} files"));
                for path in skipped {
                    self.line(format!(
                        "  skipped {}: a link or special file",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
    }

    fn copy_tree(
        &mut self,
        from: &Path,
        to: &Path,
        relative: &Path,
        files: &mut usize,
        skipped: &mut Vec<PathBuf>,
    ) -> io::Result<()> {
        let mut entries = fs::read_dir(from)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        for name in entries {
            let (from, to, relative) = (from.join(&name), to.join(&name), relative.join(&name));
            match source(&from) {
                Source::Dir => {
                    if let Some(writer) = self.writer.as_deref_mut() {
                        writer.mkdir(&to)?;
                    }
                    self.copy_tree(&from, &to, &relative, files, skipped)?;
                }
                Source::File => {
                    if let Some(writer) = self.writer.as_deref_mut() {
                        writer.copy_file(&from, &to)?;
                    }
                    *files += 1;
                }
                Source::Missing | Source::Other => skipped.push(relative),
            }
        }
        Ok(())
    }

    fn sessions(&mut self) -> Result<(), AgentError> {
        let from = self.from.join("state.db");
        match source(&from) {
            Source::Missing => {
                self.line("state.db: not in the Hermes home".into());
                return Ok(());
            }
            Source::Dir | Source::Other => {
                self.line("state.db: skipped, not a regular file".into());
                return Ok(());
            }
            Source::File => {}
        }
        if present(&self.to.join("sessions.db")) || present(&self.to.join("sessions")) {
            self.line(
                "state.db: skipped, the Reformed home already has sessions, left unchanged".into(),
            );
            return Ok(());
        }
        let conn = match state_db::open(&from) {
            Ok(conn) => conn,
            Err(reason) => {
                self.problem(format!("state.db: {reason}"));
                return Ok(());
            }
        };
        let read = match self.writer.as_deref_mut() {
            None => {
                let mut importer = SessionImporter::new(None);
                state_db::read(&conn, &mut importer).map(|orphans| (importer.summary, orphans))
            }
            Some(writer) => import_sessions(&conn, self.to, writer),
        };
        match read {
            Ok((summary, orphans)) => {
                let copied = self.copied();
                self.line(format!(
                    "state.db: {copied} {} sessions with {} active messages; {} inactive or metadata rows left out",
                    summary.sessions, summary.messages, summary.excluded_rows
                ));
                if orphans > 0 {
                    self.line(format!(
                        "  {orphans} message rows belong to no session and are left out"
                    ));
                }
            }
            Err(error) => self.problem(format!("state.db: {error}")),
        }
        Ok(())
    }

    fn cron(&mut self) -> io::Result<()> {
        let from = self.from.join("cron").join("jobs.json");
        match source(&from) {
            Source::Missing => {
                self.line("cron/jobs.json: not in the Hermes home".into());
                return Ok(());
            }
            Source::Dir | Source::Other => {
                self.line("cron/jobs.json: skipped, not a regular file".into());
                return Ok(());
            }
            Source::File => {}
        }
        let mut bytes = Vec::new();
        open_regular(&from)?
            .take(MAX_JOBS_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_JOBS_BYTES {
            self.problem("cron/jobs.json: larger than 64 MiB".into());
            return Ok(());
        }
        let jobs = match cron::jobs(&bytes) {
            Ok(jobs) => jobs,
            Err(reason) => {
                self.problem(format!("cron/jobs.json: {reason}"));
                return Ok(());
            }
        };
        let folder = self.to.join("cron");
        let usable = match fs::symlink_metadata(&folder) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(writer) = self.writer.as_deref_mut() {
                    writer.mkdir(&folder)?;
                }
                true
            }
            Ok(meta) => meta.is_dir(),
            Err(_) => false,
        };
        if !usable {
            self.line("cron/jobs.json: skipped, cron in the Reformed home is not a folder".into());
            return Ok(());
        }
        let now = chrono::Utc::now();
        let scripts = self.from.join("scripts");
        let mut ids = std::collections::HashSet::new();
        let (mut written, mut skipped) = (0, 0);
        let mut details = Vec::new();
        for (position, job) in jobs.iter().enumerate() {
            let label = cron::label(job, position + 1);
            let outcome = match cron::convert(job, now, &scripts) {
                Err(reason) => Err(reason),
                Ok((job, note)) => {
                    let path = folder.join(format!("{}.json", job.id));
                    if !ids.insert(job.id.to_ascii_lowercase()) {
                        Err("another job has the same id".into())
                    } else if present(&path) {
                        Err("already in the Reformed home and left unchanged".into())
                    } else {
                        if let Some(writer) = self.writer.as_deref_mut() {
                            let bytes = serde_json::to_vec_pretty(&job).map_err(|error| {
                                io::Error::new(io::ErrorKind::InvalidData, error)
                            })?;
                            writer.write_new(&bytes, &path)?;
                        }
                        Ok(note)
                    }
                }
            };
            match outcome {
                Ok(note) => {
                    written += 1;
                    details.push(format!("  job {label}: {note}"));
                }
                Err(reason) => {
                    skipped += 1;
                    details.push(format!("  job {label}: skipped, {reason}"));
                }
            }
        }
        let verb = if self.writer.is_some() {
            "written"
        } else {
            "to write"
        };
        self.line(format!(
            "cron/jobs.json: {written} jobs {verb} paused, {skipped} skipped"
        ));
        self.report.lines.extend(details);
        if written > 0 {
            self.line(
                "  Check each job, then start it with `hermes-ultra cron resume <id>`.".into(),
            );
        }
        Ok(())
    }
}

/// Converts the sessions in a staging folder inside the target, then links
/// the database and snapshots into place.
fn import_sessions(
    conn: &rusqlite::Connection,
    to: &Path,
    writer: &mut Writer,
) -> Result<(HermesHistorySummary, usize), AgentError> {
    let stage = tempfile::tempdir_in(to).map_err(write_error)?;
    let staged = stage.path().join("sessions");
    fs::create_dir(&staged).map_err(write_error)?;
    let store = SessionPersistence::new(stage.path());
    let read = store.import_sessions(|import| {
        let mut importer = SessionImporter::new(Some((&staged, import)));
        let orphans = state_db::read(conn, &mut importer)?;
        Ok((importer.summary, orphans))
    })?;
    writer
        .link_new(store.db_path(), &to.join("sessions.db"))
        .map_err(write_error)?;
    let sessions = to.join("sessions");
    writer.mkdir(&sessions).map_err(write_error)?;
    let mut names = fs::read_dir(&staged)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<io::Result<Vec<_>>>()
        })
        .map_err(write_error)?;
    names.sort();
    for name in names {
        writer
            .link_new(&staged.join(&name), &sessions.join(&name))
            .map_err(write_error)?;
    }
    Ok(read)
}
