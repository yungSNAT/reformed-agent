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
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open for the call and `info` is a valid out pointer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return None;
    }
    Some(FileIdentity {
        volume: u64::from(info.dwVolumeSerialNumber),
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: u64::from(info.nNumberOfLinks),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
