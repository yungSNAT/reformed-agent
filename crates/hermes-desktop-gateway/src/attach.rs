//! `file.attach`: writes a file the desktop sends as a base64 `data_url` to
//! `<working folder>/.hermes/attachments/`, so the next prompt can name it with an `@file:`
//! reference, which the agent confines to the working folder.
//!
//! No host path a client names is read: in remote mode the desktop always sends the bytes.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;

/// The attachments folder, from the working folder down.
const FOLDERS: [&str; 2] = [".hermes", "attachments"];

/// Base64 characters decoded at a time: a multiple of 4.
const CHUNK: usize = 64 << 10;

const INVALID: &str = "invalid data_url payload";

/// Device names Windows reserves in any folder, with or without an extension.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A written attachment: where it is and its `@file:` path, relative to the working folder.
pub(crate) struct Staged {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) ref_path: String,
}

/// Writes `data_url`'s bytes to a new file in `workspace`'s attachments folder, named after
/// `name`. The payload's form is checked before anything is written; a payload that fails to
/// decode leaves no file.
pub(crate) fn stage(workspace: &Path, name: &str, data_url: &str) -> Result<Staged, String> {
    let payload = base64_payload(data_url).ok_or(INVALID)?;
    let dir = attachments_folder(workspace)?;
    let (path, file) = create_new(&dir, &file_name(name))?;
    if let Err(e) = decode_into(payload, file) {
        let _ = fs::remove_file(&path);
        return Err(e);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ref_path = format!("{}/{}/{name}", FOLDERS[0], FOLDERS[1]);
    Ok(Staged {
        path,
        name,
        ref_path,
    })
}

/// The base64 text of a `data:<type>[;<param>=<value>]...;base64,<data>` URL, or of bare base64
/// as upstream also accepts. `None` for any other `data:` URL.
fn base64_payload(data_url: &str) -> Option<&str> {
    let text = data_url.trim();
    let Some(prefix) = text.get(..5).filter(|p| p.eq_ignore_ascii_case("data:")) else {
        return Some(text);
    };
    let (header, data) = text[prefix.len()..].split_once(',')?;
    let marker = header.len().checked_sub(";base64".len())?;
    header
        .get(marker..)
        .filter(|m| m.eq_ignore_ascii_case(";base64"))
        .map(|_| data)
}

/// Strictly decodes `payload` (standard alphabet and padding; whitespace ignored, as upstream)
/// into `file`, a chunk at a time, so no decoded copy of the whole file is held.
fn decode_into(payload: &str, mut file: File) -> Result<(), String> {
    let mut chunk = Vec::with_capacity(CHUNK);
    let mut decoded = Vec::with_capacity(CHUNK / 4 * 3);
    let mut write = |chunk: &[u8]| {
        decoded.clear();
        base64::engine::general_purpose::STANDARD
            .decode_vec(chunk, &mut decoded)
            .map_err(|_| INVALID.to_string())?;
        file.write_all(&decoded)
            .map_err(|e| format!("cannot write the attachment: {e}"))
    };
    for byte in payload.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if chunk.len() == CHUNK {
            // More follows, so padding here would end the data early.
            if chunk.last() == Some(&b'=') {
                return Err(INVALID.to_string());
            }
            write(&chunk)?;
            chunk.clear();
        }
        chunk.push(byte);
    }
    write(&chunk)
}

/// Makes the attachments folder, refusing a link or a file anywhere from `workspace` down,
/// as either could put the file outside the working folder.
fn attachments_folder(workspace: &Path) -> Result<PathBuf, String> {
    let mut dir = workspace.to_path_buf();
    for folder in FOLDERS {
        dir.push(folder);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
        }
        // `symlink_metadata` describes a link (a Windows junction included), not its target.
        let is_folder = fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir());
        if !is_folder {
            return Err(format!("{} is not a folder", dir.display()));
        }
    }
    Ok(dir)
}

/// Creates a new file in `dir` named `name`, or `<stem>-2<ext>` and so on when it is taken.
fn create_new(dir: &Path, name: &str) -> Result<(PathBuf, File), String> {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    let mut candidate = name.to_string();
    for n in 2u64.. {
        let path = dir.join(&candidate);
        // Any existing entry, a dangling link included, takes the name: Windows' create-new
        // follows a dangling link and would create its target.
        if fs::symlink_metadata(&path).is_err() {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("cannot create {}: {e}", path.display())),
            }
        }
        candidate = format!("{stem}-{n}{ext}");
    }
    unreachable!("names run out only after u64::MAX files")
}

/// The final component of `name` as a file name that every platform accepts and that an
/// `@file:` reference names unchanged; `attachment` when nothing usable is left.
fn file_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    // References end at whitespace and take no quotes, so whitespace becomes `_` too.
    let safe: String = last
        .chars()
        .map(|c| {
            if c.is_control() || c.is_whitespace() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut safe = safe.trim_matches('.').to_string();
    let stem = safe.split('.').next().unwrap_or_default();
    if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        safe.insert(0, '_');
    }
    let names_itself = |n: &str| {
        let path = format!("{}/{}/{n}", FOLDERS[0], FOLDERS[1]);
        let refs = hermes_agent::parse_context_references(&format!("@file:{path}"));
        matches!(refs.as_slice(), [r] if r.target == path && r.line_start.is_none())
    };
    if safe.is_empty() || !names_itself(&safe) {
        return "attachment".to_string();
    }
    safe
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn a_data_url_is_written_under_the_working_folder_with_a_reference_to_it() {
        let work = tempfile::tempdir().unwrap();
        let staged = stage(work.path(), "notes.txt", "data:text/plain;base64,aGVsbG8=").unwrap();
        assert_eq!(staged.name, "notes.txt");
        assert_eq!(staged.ref_path, ".hermes/attachments/notes.txt");
        assert_eq!(
            staged.path,
            work.path().join(".hermes/attachments/notes.txt")
        );
        assert_eq!(fs::read(&staged.path).unwrap(), b"hello");

        // A media type parameter, any case, and bare base64 are accepted, as upstream.
        let staged = stage(
            work.path(),
            "b.txt",
            "DATA:text/plain;charset=utf-8;BASE64,aGk=",
        );
        assert_eq!(fs::read(staged.unwrap().path).unwrap(), b"hi");
        let staged = stage(work.path(), "c.txt", " aGk= ").unwrap();
        assert_eq!(fs::read(staged.path).unwrap(), b"hi");
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let work = tempfile::tempdir().unwrap();
        for (n, data) in ["YQ==", "Yg==", "Yw=="].into_iter().enumerate() {
            let staged = stage(work.path(), "notes.txt", data).unwrap();
            let want = ["notes.txt", "notes-2.txt", "notes-3.txt"][n];
            assert_eq!(staged.name, want);
        }
        let dir = work.path().join(".hermes/attachments");
        assert_eq!(fs::read(dir.join("notes.txt")).unwrap(), b"a");
        assert_eq!(fs::read(dir.join("notes-3.txt")).unwrap(), b"c");
    }

    #[test]
    fn a_long_payload_with_line_breaks_decodes_across_chunks() {
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let wrapped: Vec<&str> = encoded
            .as_bytes()
            .chunks(76)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect();
        let data_url = format!(
            "data:application/octet-stream;base64,{}",
            wrapped.join("\r\n")
        );
        assert!(data_url.len() > 3 * CHUNK);
        let work = tempfile::tempdir().unwrap();
        let staged = stage(work.path(), "blob.bin", &data_url).unwrap();
        assert_eq!(fs::read(staged.path).unwrap(), bytes);
    }

    #[test]
    fn invalid_payloads_are_refused_and_leave_no_file() {
        // Padding that ends a full chunk while more follows.
        let mut early_end = "A".repeat(CHUNK - 4);
        early_end.push_str("AA==AAAA");
        for data_url in [
            "data:text/plain,hello",
            "data:text/plain;base64",
            "data:;base64x,aGk=",
            "aGVsbG8",
            "aGk=aGk=",
            "aG!k",
            early_end.as_str(),
        ] {
            let work = tempfile::tempdir().unwrap();
            let refused = stage(work.path(), "x.txt", data_url);
            assert_eq!(refused.err().as_deref(), Some("invalid data_url payload"));
            assert!(files(&work.path().join(".hermes/attachments")).is_empty());
        }
        // Not a base64 data URL: refused before the folder is made.
        let work = tempfile::tempdir().unwrap();
        assert!(stage(work.path(), "x.txt", "data:text/plain,hi").is_err());
        assert!(files(work.path()).is_empty());
    }

    #[test]
    fn names_keep_only_a_safe_final_component() {
        for (given, want) in [
            ("../../etc/passwd", "passwd"),
            ("C:\\Users\\me\\report.pdf", "report.pdf"),
            ("my notes.txt", "my_notes.txt"),
            ("a:b|c?.txt", "a_b_c_.txt"),
            ("line\nbreak", "line_break"),
            ("...", "attachment"),
            ("", "attachment"),
            ("dir/", "attachment"),
            (".env.", "env"),
            ("NUL.txt", "_NUL.txt"),
            ("com1", "_com1"),
            ("console.log", "console.log"),
            ("done!", "attachment"),
            ("f(x)", "f(x)"),
            ("f)", "attachment"),
        ] {
            assert_eq!(file_name(given), want, "{given:?}");
        }
    }

    #[test]
    fn a_file_in_place_of_a_folder_is_refused() {
        let work = tempfile::tempdir().unwrap();
        fs::write(work.path().join(".hermes"), "").unwrap();
        let refused = stage(work.path(), "x.txt", "aGk=").err().unwrap();
        assert!(refused.contains("is not a folder"), "{refused}");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_on_the_way_or_at_the_name_is_not_followed() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        for linked in [".hermes", ".hermes/attachments"] {
            let work = tempfile::tempdir().unwrap();
            let link = work.path().join(linked);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(outside.path(), &link).unwrap();
            let refused = stage(work.path(), "x.txt", "aGk=").err().unwrap();
            assert!(refused.contains("is not a folder"), "{linked}: {refused}");
            assert!(files(outside.path()).is_empty());
        }

        // A dangling link takes its name; its target is never created.
        let work = tempfile::tempdir().unwrap();
        let dir = work.path().join(".hermes/attachments");
        fs::create_dir_all(&dir).unwrap();
        symlink(outside.path().join("planted"), dir.join("x.txt")).unwrap();
        let staged = stage(work.path(), "x.txt", "aGk=").unwrap();
        assert_eq!(staged.name, "x-2.txt");
        assert!(files(outside.path()).is_empty());
    }
}
