//! A source folder read without following the links inside it, for
//! `claw migrate` and `import-agent`. On Unix each entry is opened relative
//! to its folder's open descriptor, without following a link and without
//! blocking, and is classed by what was opened, so the entry that was checked
//! is the one that is read: one swapped for a link in between is not followed.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Permissions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::agent_import::looks_secret;

/// One entry of a [`SourceDir`], as opened.
pub(crate) enum Entry {
    File(File),
    Dir(SourceDir),
    Link,
    /// A FIFO, socket or device, which is never read.
    Other,
}

/// An open source folder.
pub(crate) struct SourceDir {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    // shortcut: off Unix an entry is looked up by path and then opened, so an
    // entry swapped for a link in between is followed. Closing that needs
    // handle-relative opens (NtCreateFile with a root directory).
    #[cfg(not(unix))]
    path: PathBuf,
}

impl SourceDir {
    /// Opens the folder `path`, following it when it is a link: the root of
    /// a read is a folder the user named or their own.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
            Ok(Self {
                fd: rustix::fs::open(path, flags, Mode::empty())?,
            })
        }
        #[cfg(not(unix))]
        {
            if std::fs::metadata(path)?.is_dir() {
                Ok(Self {
                    path: path.to_path_buf(),
                })
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "it is not a folder",
                ))
            }
        }
    }

    /// The entry `name`, or `None` when there is none. An entry that goes
    /// away between its classing and its open is an error, not `None`, so it
    /// is never taken for one that was never there.
    pub(crate) fn entry(&self, name: impl AsRef<OsStr>) -> io::Result<Option<Entry>> {
        let name = name.as_ref();
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, FileType, Mode, OFlags};
            use rustix::io::Errno;
            // Classed before it is opened, as opening some devices can block
            // or act on them; the open below classes it again, as the entry
            // can change in between.
            match rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                    FileType::Symlink => return Ok(Some(Entry::Link)),
                    FileType::RegularFile | FileType::Directory => {}
                    _ => return Ok(Some(Entry::Other)),
                },
                Err(Errno::NOENT) => return Ok(None),
                Err(err) => return Err(err.into()),
            }
            #[cfg(test)]
            tests::BETWEEN.with(|between| {
                if let Some(between) = &*between.borrow() {
                    between(name);
                }
            });
            let flags = OFlags::RDONLY
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::NOCTTY
                | OFlags::CLOEXEC;
            let fd = match rustix::fs::openat(&self.fd, name, flags, Mode::empty()) {
                Ok(fd) => fd,
                Err(Errno::NOENT) => return Err(gone()),
                // A link refused by O_NOFOLLOW (EMLINK on FreeBSD).
                Err(Errno::LOOP | Errno::MLINK) => return Ok(Some(Entry::Link)),
                // A socket swapped in, which cannot be opened.
                Err(Errno::NXIO) => return Ok(Some(Entry::Other)),
                Err(err) => return Err(err.into()),
            };
            Ok(Some(
                match FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) {
                    FileType::RegularFile => Entry::File(File::from(fd)),
                    FileType::Directory => Entry::Dir(Self { fd }),
                    _ => Entry::Other,
                },
            ))
        }
        #[cfg(not(unix))]
        {
            let path = self.path.join(name);
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err),
            };
            Ok(Some(if meta.file_type().is_symlink() {
                Entry::Link
            } else if meta.is_dir() {
                Entry::Dir(Self { path })
            } else if meta.is_file() {
                Entry::File(File::open(&path).map_err(|err| match err.kind() {
                    io::ErrorKind::NotFound => gone(),
                    _ => err,
                })?)
            } else {
                Entry::Other
            }))
        }
    }

    /// The names in the folder, in order. An entry that cannot be read fails
    /// the whole listing, so a shorter listing is never taken for the folder.
    pub(crate) fn names(&self) -> io::Result<Vec<OsString>> {
        #[cfg(unix)]
        let mut names = {
            use std::os::unix::ffi::OsStrExt;
            let mut names = Vec::new();
            for entry in rustix::fs::Dir::read_from(&self.fd)? {
                let entry = entry?;
                let name = entry.file_name().to_bytes();
                if name != b"." && name != b".." {
                    names.push(OsStr::from_bytes(name).to_owned());
                }
            }
            names
        };
        #[cfg(not(unix))]
        let mut names = std::fs::read_dir(&self.path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }
}

/// An entry that went away between its classing and its open. Not of kind
/// `NotFound`, which callers read as a folder that was never there.
fn gone() -> io::Error {
    io::Error::other("it went away while it was read")
}

/// The bytes of a file entry.
pub(crate) fn read_all(mut file: File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// A folder read whole, without following the links inside it.
// shortcut: every file is held in memory, which suits skill folders of text
// and scripts; a folder of large assets would want a staged copy checked in
// place instead.
pub(crate) struct Tree {
    /// The folders and files below the root, each folder before its contents.
    pub(crate) nodes: Vec<(PathBuf, Node)>,
    /// The links found, which are neither read nor copied.
    pub(crate) links: usize,
}

pub(crate) enum Node {
    Dir,
    File(Vec<u8>, Permissions),
}

impl Tree {
    /// Reads `dir` and everything under it. Any entry that cannot be read
    /// fails the read, as does a FIFO, socket or device, which is never read.
    pub(crate) fn read(dir: &SourceDir) -> io::Result<Self> {
        let mut tree = Self {
            nodes: Vec::new(),
            links: 0,
        };
        tree.read_into(dir, Path::new(""))?;
        Ok(tree)
    }

    fn read_into(&mut self, dir: &SourceDir, at: &Path) -> io::Result<()> {
        for name in dir.names()? {
            let path = at.join(&name);
            match dir.entry(&name)? {
                Some(Entry::Link) => self.links += 1,
                Some(Entry::Dir(sub)) => {
                    self.nodes.push((path.clone(), Node::Dir));
                    self.read_into(&sub, &path)?;
                }
                Some(Entry::File(file)) => {
                    let permissions = file.metadata()?.permissions();
                    self.nodes
                        .push((path, Node::File(read_all(file)?, permissions)));
                }
                Some(Entry::Other) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{} is not a regular file", path.display()),
                    ))
                }
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("{} went away while it was read", path.display()),
                    ))
                }
            }
        }
        Ok(())
    }

    /// Whether a name in it, or a file's text (read lossily when not UTF-8),
    /// looks like a credential.
    pub(crate) fn holds_secret(&self) -> bool {
        self.nodes.iter().any(|(path, node)| {
            path.file_name()
                .is_some_and(|name| looks_secret(&name.to_string_lossy()))
                || matches!(node, Node::File(bytes, _)
                    if looks_secret(&String::from_utf8_lossy(bytes)))
        })
    }

    /// Writes the tree as the new folder `destination`. It is built in a
    /// hidden folder beside it and renamed into place, so a write that fails
    /// part way leaves nothing a later run would take for the finished
    /// folder. Files are owner-only on Unix, keeping the owner's execute bit.
    pub(crate) fn write(&self, destination: &Path) -> io::Result<()> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let staging = tempfile::Builder::new()
            .prefix(".copying-")
            .tempdir_in(parent)?;
        let staged = staging.path().join("copy");
        std::fs::create_dir(&staged)?;
        for (path, node) in &self.nodes {
            let target = staged.join(path);
            match node {
                Node::Dir => std::fs::create_dir(&target)?,
                Node::File(bytes, permissions) => {
                    let mut to = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target)?;
                    to.write_all(bytes)?;
                    #[cfg(unix)]
                    let permissions = {
                        use std::os::unix::fs::PermissionsExt;
                        Permissions::from_mode(permissions.mode() & 0o700)
                    };
                    #[cfg(not(unix))]
                    let permissions = permissions.clone();
                    to.set_permissions(permissions)?;
                }
            }
        }
        std::fs::rename(&staged, destination)
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    pub(crate) type Hook = Option<Box<dyn Fn(&OsStr)>>;

    thread_local! {
        /// Runs between an entry's classing and its open, so a test can
        /// change the entry in between.
        pub(crate) static BETWEEN: RefCell<Hook> = RefCell::new(None);
    }

    #[test]
    fn an_entry_gone_between_its_class_and_its_open_is_an_error_not_none() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("SKILL.md");
        std::fs::write(&file, "a skill\n").unwrap();
        let dir = SourceDir::open(tmp.path()).unwrap();
        BETWEEN.with(|between| {
            *between.borrow_mut() = Some(Box::new(move |_: &OsStr| {
                let _ = std::fs::remove_file(&file);
            }))
        });
        let gone = dir.entry("SKILL.md");
        BETWEEN.with(|between| *between.borrow_mut() = None);
        let err = gone
            .err()
            .expect("an entry gone before its open was taken for none");
        assert_ne!(err.kind(), io::ErrorKind::NotFound, "{err}");
        assert!(dir.entry("SKILL.md").unwrap().is_none());
    }
}
