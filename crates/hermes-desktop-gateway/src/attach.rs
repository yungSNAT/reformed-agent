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

/// Largest file `file.attach` writes, as upstream (`_ATTACHMENT_MAX_BYTES` in
/// tui_gateway/prompt_attachments.py).
pub(crate) const MAX_ATTACHMENT_BYTES: usize = 256 << 20;

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
    stage_within(workspace, name, data_url, MAX_ATTACHMENT_BYTES)
}

/// `stage`, refusing a payload that decodes to more than `limit` bytes before anything is made.
fn stage_within(
    workspace: &Path,
    name: &str,
    data_url: &str,
    limit: usize,
) -> Result<Staged, String> {
    let payload = base64_payload(data_url).ok_or(INVALID)?;
    if decoded_len(payload) > limit {
        return Err(format!("attachment larger than {limit} bytes"));
    }
    let dir = attachments_folder(workspace)?;
    let (path, file) = create_new(&dir, &file_name(name), 0o666)?;
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

/// Largest image `image.attach_bytes` takes, decoded, as upstream (`_ATTACH_BYTES_MAX_BYTES`
/// in tui_gateway/prompt_attachments.py).
pub(crate) const MAX_IMAGE_BYTES: usize = 25 << 20;

/// Why an image was not staged, with upstream's error code for each.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImageError {
    /// 4017: not valid base64, or empty.
    Invalid(String),
    /// 4018: larger than [`MAX_IMAGE_BYTES`].
    TooLarge,
    /// 4016: not a PNG, JPEG, GIF, WebP or BMP image.
    Unsupported,
    /// 5027: the file could not be written.
    Write(String),
}

/// A staged image: where it is, its media type and size.
pub(crate) struct StagedImage {
    pub(crate) path: PathBuf,
    pub(crate) mime: &'static str,
    pub(crate) bytes: usize,
}

/// Writes the image `payload` (base64, bare or as a `data:` URL) holds to a new file in
/// `images`, made owner-only if absent. The bytes decide the type and the name's extension;
/// the client's name and hints are not used, so it cannot choose where or as what the file
/// lands. Nothing is written for a payload that is refused.
pub(crate) fn stage_image(images: &Path, payload: &str) -> Result<StagedImage, ImageError> {
    let payload = base64_payload(payload).ok_or_else(|| ImageError::Invalid(INVALID.into()))?;
    if decoded_len(payload) > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge);
    }
    let compact: Vec<u8> = payload
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let data = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|_| ImageError::Invalid("data is not valid base64".into()))?;
    if data.is_empty() {
        return Err(ImageError::Invalid("image is empty".into()));
    }
    let (ext, mime) = image_kind(&data).ok_or(ImageError::Unsupported)?;
    private_folder(images).map_err(ImageError::Write)?;
    let name = format!("upload_{}.{ext}", uuid::Uuid::new_v4().simple());
    // Owner-only, as upstream's mkstemp, whatever the folder's own mode.
    let (path, mut file) = create_new(images, &name, 0o600).map_err(ImageError::Write)?;
    if let Err(e) = file.write_all(&data) {
        let _ = fs::remove_file(&path);
        return Err(ImageError::Write(format!("cannot write the image: {e}")));
    }
    Ok(StagedImage {
        path,
        mime,
        bytes: data.len(),
    })
}

/// Extension and media type from an image's first bytes.
fn image_kind(data: &[u8]) -> Option<(&'static str, &'static str)> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some(("png", "image/png"))
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(("jpg", "image/jpeg"))
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some(("gif", "image/gif"))
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP".as_slice()) {
        Some(("webp", "image/webp"))
    } else if data.starts_with(b"BM") {
        Some(("bmp", "image/bmp"))
    } else {
        None
    }
}

/// Creates `dir` and its parents, `dir` itself owner-only on Unix, and refuses it when it is a
/// link or a file.
fn private_folder(dir: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // `symlink_metadata` describes a link (a Windows junction included), not its target.
    if !fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return Err(format!("{} is not a folder", dir.display()));
    }
    Ok(())
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

/// The bytes a valid `payload` decodes to; an invalid one fails to decode later.
fn decoded_len(payload: &str) -> usize {
    let (mut chars, mut padding) = (0usize, 0usize);
    for byte in payload.bytes().filter(|b| !b.is_ascii_whitespace()) {
        chars += 1;
        padding = if byte == b'=' { padding + 1 } else { 0 };
    }
    (chars / 4 * 3).saturating_sub(padding.min(2))
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

/// Creates a new file in `dir` named `name`, or `<stem>-2<ext>` and so on when it is taken,
/// with Unix permissions `mode` before the umask.
fn create_new(dir: &Path, name: &str, mode: u32) -> Result<(PathBuf, File), String> {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    #[cfg(not(unix))]
    let _ = mode;
    let mut candidate = name.to_string();
    for n in 2u64.. {
        let path = dir.join(&candidate);
        // Any existing entry, a dangling link included, takes the name: Windows' create-new
        // follows a dangling link and would create its target.
        if fs::symlink_metadata(&path).is_err() {
            match options.open(&path) {
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
    fn a_payload_over_the_decoded_limit_is_refused_before_anything_is_written() {
        // "hello!" is 6 bytes, with and without line breaks; "hello" with padding is 5.
        let work = tempfile::tempdir().unwrap();
        let at = stage_within(work.path(), "a.txt", "aGVs\nbG8h", 6).unwrap();
        assert_eq!(fs::read(at.path).unwrap(), b"hello!");
        let at = stage_within(work.path(), "b.txt", "aGVsbG8=", 5).unwrap();
        assert_eq!(fs::read(at.path).unwrap(), b"hello");

        let work = tempfile::tempdir().unwrap();
        for (payload, limit) in [("aGVs\nbG8h", 5), ("aGVsbG8=", 4)] {
            let refused = stage_within(work.path(), "c.txt", payload, limit);
            assert_eq!(
                refused.err(),
                Some(format!("attachment larger than {limit} bytes"))
            );
        }
        assert!(files(work.path()).is_empty(), "no folder was made");
        assert_eq!(MAX_ATTACHMENT_BYTES, 256 << 20);

        // The size is only estimated from a valid payload's length; a malformed one under the
        // limit is still refused when it is decoded, leaving no file.
        let refused = stage_within(work.path(), "d.txt", "aGVsbG8", 5);
        assert_eq!(refused.err().as_deref(), Some(INVALID));
        assert!(files(&work.path().join(".hermes/attachments")).is_empty());
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

    #[test]
    fn each_image_kind_is_named_by_its_bytes() {
        use base64::Engine as _;
        let images = tempfile::tempdir().unwrap();
        let mut webp = b"RIFF0000WEBPVP8 ".to_vec();
        webp.push(1);
        let cases: [(&[u8], &str, &str); 5] = [
            (
                &[0x89, b'P', b'N', b'G', 13, 10, 26, 10, 1],
                "png",
                "image/png",
            ),
            (&[0xff, 0xd8, 0xff, 0xe0, 1], "jpg", "image/jpeg"),
            (b"GIF89a1", "gif", "image/gif"),
            (&webp, "webp", "image/webp"),
            (b"BM1234", "bmp", "image/bmp"),
        ];
        for (bytes, ext, mime) in cases {
            let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
            let staged = stage_image(images.path(), &payload).unwrap();
            assert_eq!(staged.path.extension().unwrap(), ext);
            assert_eq!(staged.mime, mime);
            assert_eq!(fs::read(&staged.path).unwrap(), bytes);
        }
        // A data URL's type is ignored too: the bytes decide.
        let staged = stage_image(images.path(), "data:image/png;base64,R0lGODlhMQ==").unwrap();
        assert_eq!(staged.mime, "image/gif");
    }

    #[test]
    fn a_refused_image_leaves_no_file() {
        use base64::Engine as _;
        let images = tempfile::tempdir().unwrap();
        let dir = images.path().join("images");
        let text = base64::engine::general_purpose::STANDARD.encode("hello");
        assert_eq!(
            stage_image(&dir, &text).err(),
            Some(ImageError::Unsupported)
        );
        assert!(matches!(
            stage_image(&dir, "!!"),
            Err(ImageError::Invalid(_))
        ));
        assert!(matches!(
            stage_image(&dir, "data:image/png,x"),
            Err(ImageError::Invalid(_))
        ));
        // Over the limit by its length alone, so nothing is decoded or made.
        let large = "A".repeat((MAX_IMAGE_BYTES / 3 + 1) * 4);
        assert_eq!(stage_image(&dir, &large).err(), Some(ImageError::TooLarge));
        assert!(!dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_staged_image_is_owner_only_in_an_existing_open_folder() {
        use std::os::unix::fs::PermissionsExt as _;
        let images = tempfile::tempdir().unwrap();
        fs::set_permissions(images.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let staged = stage_image(images.path(), "iVBORw0KGgoB").unwrap();
        let mode = fs::metadata(&staged.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_images_folder_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let link = home.path().join("images");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let png = "iVBORw0KGgoB";
        assert!(matches!(stage_image(&link, png), Err(ImageError::Write(_))));
        assert!(files(outside.path()).is_empty());
    }
}
