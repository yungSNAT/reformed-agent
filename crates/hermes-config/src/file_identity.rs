//! Which file a path names on disk, so hard links to one file can be matched.

use std::path::Path;

/// A file's volume and file number, and how many hard links name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub volume: u64,
    pub file: u64,
    pub links: u64,
}

impl FileIdentity {
    /// Whether both name the same file.
    pub fn same_file(&self, other: &Self) -> bool {
        self.volume == other.volume && self.file == other.file
    }
}

/// The identity of the file `path` names, following links. None when it
/// cannot be read, as for a missing file.
#[cfg(unix)]
pub fn file_identity(path: &Path) -> Option<FileIdentity> {
    let meta = std::fs::metadata(path).ok()?;
    Some(file_identity_from_metadata(&meta))
}

/// The identity of an already-opened file handle.
#[cfg(unix)]
pub fn file_identity_from_file(file: &std::fs::File) -> Option<FileIdentity> {
    let meta = file.metadata().ok()?;
    Some(file_identity_from_metadata(&meta))
}

/// The identity of a regular non-symlink file at `path`, without following a
/// leaf symlink or blocking on special files.
#[cfg(unix)]
pub(crate) fn file_identity_no_follow(path: &Path) -> Option<FileIdentity> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return None;
    }
    Some(file_identity_from_metadata(&meta))
}

#[cfg(unix)]
fn file_identity_from_metadata(meta: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        volume: meta.dev(),
        file: meta.ino(),
        links: meta.nlink(),
    }
}

/// The identity of the file `path` names, following links. None when it
/// cannot be opened, as for a missing file.
#[cfg(windows)]
pub fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    // No access is needed to read the identity, so a file another process
    // holds open still answers.
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    file_identity_from_file(&file)
}

/// The identity of an already-opened file handle.
#[cfg(windows)]
pub fn file_identity_from_file(file: &std::fs::File) -> Option<FileIdentity> {
    file_identity_and_kind_from_file(file).map(|(id, _)| id)
}

/// What a handle opened without following reparse points names.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandleKind {
    Regular,
    /// A reparse point that names no other file, such as a cloud placeholder
    /// or a deduplicated file.
    OtherReparse,
    /// A directory, or a reparse point that names another file: a symlink,
    /// junction or any other name-surrogate tag.
    LinkOrDir,
}

/// Sorts a handle by its file attributes and reparse tag. Only name-surrogate
/// tags (`IsReparseTagNameSurrogate`) redirect to another file.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn handle_kind(attributes: u32, reparse_tag: u32) -> HandleKind {
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const NAME_SURROGATE_BIT: u32 = 0x2000_0000;
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        HandleKind::LinkOrDir
    } else if attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        HandleKind::Regular
    } else if reparse_tag & NAME_SURROGATE_BIT != 0 {
        HandleKind::LinkOrDir
    } else {
        HandleKind::OtherReparse
    }
}

#[cfg(windows)]
pub(crate) fn file_identity_and_kind_from_file(
    file: &std::fs::File,
) -> Option<(FileIdentity, HandleKind)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_TAG_INFO,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open for the call and `info` is a valid out pointer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return None;
    }
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: as above; the size passed is the size of `tag`.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            (&raw mut tag).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    } == 0
    {
        return None;
    }
    let id = FileIdentity {
        volume: u64::from(info.dwVolumeSerialNumber),
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: u64::from(info.nNumberOfLinks),
    };
    Some((id, handle_kind(info.dwFileAttributes, tag.ReparseTag)))
}

/// The identity of the file at `path`, without following a leaf symlink or
/// junction. None for a directory or a link.
#[cfg(windows)]
pub(crate) fn file_identity_no_follow(path: &Path) -> Option<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    let (id, kind) = file_identity_and_kind_from_file(&file)?;
    (kind != HandleKind::LinkOrDir).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_name_surrogate_reparse_points_count_as_links() {
        const REPARSE: u32 = 0x400;
        const ARCHIVE: u32 = 0x20;
        // IO_REPARSE_TAG_SYMLINK, _MOUNT_POINT (junctions) and _LX_SYMLINK.
        for tag in [0xA000_000C, 0xA000_0003, 0xA000_001D] {
            assert_eq!(handle_kind(ARCHIVE | REPARSE, tag), HandleKind::LinkOrDir);
        }
        // IO_REPARSE_TAG_CLOUD_6 (OneDrive Files On-Demand) and _DEDUP.
        for tag in [0x9000_601A, 0x8000_0013] {
            assert_eq!(
                handle_kind(ARCHIVE | REPARSE, tag),
                HandleKind::OtherReparse
            );
        }
        assert_eq!(handle_kind(ARCHIVE, 0), HandleKind::Regular);
        assert_eq!(handle_kind(0x10, 0), HandleKind::LinkOrDir);
    }

    #[test]
    fn hard_links_share_an_identity_and_count_their_links() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let other = dir.path().join("other");
        std::fs::write(&first, "a").unwrap();
        std::fs::write(&other, "a").unwrap();
        assert_eq!(file_identity(&first).unwrap().links, 1);

        let second = dir.path().join("second");
        std::fs::hard_link(&first, &second).unwrap();
        let (first, second, other) = (
            file_identity(&first).unwrap(),
            file_identity(&second).unwrap(),
            file_identity(&other).unwrap(),
        );
        assert!(first.same_file(&second));
        assert!(!first.same_file(&other));
        assert_eq!(second.links, 2);
        assert!(file_identity(&dir.path().join("missing")).is_none());
    }
}
