//! A source folder read without following the links inside it, for
//! `claw migrate` and `import-agent`. On Unix each entry is opened relative
//! to its folder's open descriptor, without following a link and without
//! blocking, and is classed by what was opened, so the entry that was checked
//! is the one that is read: one swapped for a link in between is not followed.
//! On Windows each entry is opened relative to its folder's open handle
//! (`NtCreateFile` with a root directory) without following a reparse point,
//! and is classed from that handle, which is the one read.

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
    #[cfg(windows)]
    handle: File,
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
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
            // A folder opens only with backup semantics.
            let handle = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)?;
            if handle.metadata()?.is_dir() {
                Ok(Self { handle })
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
        #[cfg(windows)]
        {
            #[cfg(test)]
            tests::BETWEEN.with(|between| {
                if let Some(between) = &*between.borrow() {
                    between(name);
                }
            });
            // One open, which classes the entry too: there is no lookup
            // before it for the entry to change after.
            match win::open_relative(&self.handle, name)? {
                Some(handle) => win::class(handle).map(Some),
                None => Ok(None),
            }
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
        #[cfg(windows)]
        let mut names = win::names(&self.handle)?;
        names.sort();
        Ok(names)
    }
}

/// An entry that went away between its classing and its open. Not of kind
/// `NotFound`, which callers read as a folder that was never there.
#[cfg(unix)]
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

#[cfg(windows)]
mod win {
    use super::{Entry, SourceDir};
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        NtCreateFile, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    };
    use windows_sys::Win32::Foundation::{
        RtlNtStatusToDosError, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
        OBJ_CASE_INSENSITIVE, STATUS_OBJECT_NAME_NOT_FOUND, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
        GetFileInformationByHandleEx, ReOpenFile, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FULL_DIR_INFO, FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_AF_UNIX;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    /// Set on the tags of links, junctions and other reparse points that
    /// name another file (`IsReparseTagNameSurrogate`).
    const NAME_SURROGATE: u32 = 0x2000_0000;

    /// Opens the entry `name` of the folder `dir` without following a
    /// reparse point, or `None` when there is none. A name that is not a
    /// single entry of the folder is refused.
    pub(super) fn open_relative(dir: &File, name: &OsStr) -> io::Result<Option<OwnedHandle>> {
        let mut wide: Vec<u16> = name.encode_wide().collect();
        let single = !wide.is_empty()
            && wide.len() <= usize::from(u16::MAX / 2)
            && name != OsStr::new(".")
            && name != OsStr::new("..")
            && !wide
                .iter()
                .any(|&c| matches!(c, 0 | 0x2f /* / */ | 0x3a /* : */ | 0x5c /* \ */));
        if !single {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a single folder entry",
            ));
        }
        let bytes = (wide.len() * 2) as u16;
        let object_name = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: wide.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: dir.as_raw_handle() as HANDLE,
            ObjectName: &object_name,
            // As every Win32 open is.
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        let mut status_block = IO_STATUS_BLOCK::default();
        // SAFETY: every pointer is to a live local; `object_name` borrows
        // `wide`, which outlives the call.
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                FILE_GENERIC_READ,
                &attributes,
                &mut status_block,
                std::ptr::null(),
                0,
                SHARE,
                FILE_OPEN,
                FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        if status == STATUS_OBJECT_NAME_NOT_FOUND {
            return Ok(None);
        }
        if status < 0 {
            // SAFETY: a plain status-code translation.
            let code = unsafe { RtlNtStatusToDosError(status) };
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        // SAFETY: a successful NtCreateFile returns a handle the caller owns.
        Ok(Some(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    /// Classes an entry opened by [`open_relative`] from its own handle.
    /// A link is never read. Another reparse point (a cloud placeholder or a
    /// deduplicated file) is reopened from the same handle, so its filter
    /// serves the data without a second lookup by name.
    pub(super) fn class(handle: OwnedHandle) -> io::Result<Entry> {
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        // SAFETY: `tag` is a writable buffer of the size passed.
        if unsafe {
            GetFileInformationByHandleEx(
                handle.as_raw_handle() as HANDLE,
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let folder = tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let mut handle = handle;
        if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            if tag.ReparseTag & NAME_SURROGATE != 0 {
                return Ok(Entry::Link);
            }
            if tag.ReparseTag == IO_REPARSE_TAG_AF_UNIX {
                return Ok(Entry::Other);
            }
            let flags = if folder {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                0
            };
            // SAFETY: `handle` is open; the result is checked below.
            let reopened = unsafe {
                ReOpenFile(
                    handle.as_raw_handle() as HANDLE,
                    FILE_GENERIC_READ,
                    SHARE,
                    flags,
                )
            };
            if reopened == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a successful ReOpenFile returns a handle the caller owns.
            handle = unsafe { OwnedHandle::from_raw_handle(reopened) };
        }
        let handle = File::from(handle);
        Ok(if folder {
            Entry::Dir(SourceDir { handle })
        } else {
            Entry::File(handle)
        })
    }

    /// The names in the open folder `dir`, read from its handle.
    pub(super) fn names(dir: &File) -> io::Result<Vec<OsString>> {
        // u64 words keep each record 8-byte aligned.
        let mut buffer = vec![0u64; 8 * 1024];
        let mut kind = FileFullDirectoryRestartInfo;
        let mut names = Vec::new();
        loop {
            // SAFETY: `buffer` is writable for the size passed.
            if unsafe {
                GetFileInformationByHandleEx(
                    dir.as_raw_handle() as HANDLE,
                    kind,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * 8) as u32,
                )
            } == 0
            {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    return Ok(names);
                }
                return Err(err);
            }
            kind = FileFullDirectoryInfo;
            let base = buffer.as_ptr().cast::<u8>();
            let mut offset = 0;
            loop {
                // SAFETY: the system filled the buffer with a chain of
                // FILE_FULL_DIR_INFO records, each name inside the buffer.
                let (next, name) = unsafe {
                    let record = base.add(offset).cast::<FILE_FULL_DIR_INFO>();
                    let len = (*record).FileNameLength as usize / 2;
                    let start = std::ptr::addr_of!((*record).FileName).cast::<u16>();
                    (
                        (*record).NextEntryOffset as usize,
                        std::slice::from_raw_parts(start, len),
                    )
                };
                if name != [0x2e] && name != [0x2e, 0x2e] {
                    names.push(OsString::from_wide(name));
                }
                if next == 0 {
                    break;
                }
                offset += next;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    pub(crate) type Hook = Option<Box<dyn Fn(&OsStr)>>;

    thread_local! {
        /// Runs before an entry's open (on Unix, after its classing), so a
        /// test can change the entry in between.
        pub(crate) static BETWEEN: RefCell<Hook> = RefCell::new(None);
    }

    /// A file and a folder swapped for links to outside ones just before
    /// their open are counted as links, and nothing outside is read.
    #[cfg(windows)]
    #[test]
    fn an_entry_swapped_for_a_link_before_its_open_is_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "outside bytes\n").unwrap();
        let source = tmp.path().join("source");
        std::fs::create_dir_all(source.join("refs")).unwrap();
        std::fs::write(source.join("SKILL.md"), "a skill\n").unwrap();
        std::fs::write(source.join("refs").join("note.md"), "a note\n").unwrap();
        // A file symlink needs the privilege or Developer Mode; a junction
        // needs neither.
        let probe = tmp.path().join("probe");
        let file_links = std::os::windows::fs::symlink_file(outside.join("secret.txt"), &probe)
            .map(|()| std::fs::remove_file(&probe).unwrap())
            .is_ok();
        if !file_links {
            eprintln!("no file symlink privilege: the folder case alone runs");
        }

        let dir = SourceDir::open(&source).unwrap();
        let (at, to) = (source.clone(), outside.clone());
        BETWEEN.with(|between| {
            *between.borrow_mut() = Some(Box::new(move |name: &OsStr| {
                if name == "SKILL.md" && file_links {
                    std::fs::remove_file(at.join("SKILL.md")).unwrap();
                    std::os::windows::fs::symlink_file(to.join("secret.txt"), at.join("SKILL.md"))
                        .unwrap();
                } else if name == "refs" {
                    std::fs::remove_dir_all(at.join("refs")).unwrap();
                    let made = std::process::Command::new("cmd")
                        .args(["/C", "mklink", "/J"])
                        .arg(at.join("refs"))
                        .arg(&to)
                        .output()
                        .unwrap();
                    assert!(made.status.success(), "{made:?}");
                }
            }))
        });
        let tree = Tree::read(&dir);
        BETWEEN.with(|between| *between.borrow_mut() = None);

        let tree = tree.unwrap();
        let read: Vec<_> = tree.nodes.iter().map(|(path, _)| path.clone()).collect();
        assert!(
            !tree.nodes.iter().any(|(_, node)| matches!(node,
                Node::File(bytes, _) if bytes.as_slice() == b"outside bytes\n")),
            "read outside the folder: {read:?}"
        );
        assert_eq!(tree.links, if file_links { 2 } else { 1 }, "{read:?}");
        let expected: Vec<PathBuf> = if file_links {
            Vec::new()
        } else {
            vec!["SKILL.md".into()]
        };
        assert_eq!(read, expected);
    }

    #[cfg(unix)]
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
