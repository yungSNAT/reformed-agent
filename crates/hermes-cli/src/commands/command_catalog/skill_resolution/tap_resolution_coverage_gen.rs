use super::*;

fn config_error<T>(result: Result<T, AgentError>) -> String {
    match result {
        Err(AgentError::Config(message)) => message,
        Err(other) => panic!("expected a configuration error, got {other:?}"),
        Ok(_) => panic!("expected a configuration error, got success"),
    }
}

struct OfflineDns;

impl reqwest::dns::Resolve for OfflineDns {
    fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async { Err(std::io::Error::other("DNS disabled in this test").into()) })
    }
}

fn offline_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(std::sync::Arc::new(OfflineDns))
        .build()
        .expect("build offline client")
}

#[tokio::test]
async fn unavailable_taps_produce_no_matches_or_suggestions() {
    let client = offline_client();
    let taps = [String::new(), "fixture/skills".to_owned()];
    assert!(parse_skill_tap_spec(&taps[0]).is_none());
    assert!(parse_skill_tap_spec(&taps[1]).is_some());

    let message = config_error(resolve_skill_via_taps(&client, &taps, "Example").await);
    assert_eq!(
        message,
        "Skill 'Example' not found in configured taps. Suggestions: none"
    );

    let matches = search_skills_via_taps(&client, &taps, "Example", 5)
        .await
        .expect("search should skip malformed and unavailable taps");
    assert!(matches.is_empty(), "unexpected matches: {matches:?}");
}

#[tokio::test]
async fn github_resolution_and_fetch_propagate_lookup_failures() {
    let client = offline_client();
    let source = ResolvedSkillSource {
        repo: "fixture/skills".to_owned(),
        branch: "main".to_owned(),
        skill_dir: "skills/example".to_owned(),
    };

    let resolution_error =
        config_error(resolve_skill_in_repo(&client, &source.repo, "example", None).await);
    assert!(!resolution_error.is_empty());
    assert!(!resolution_error.contains("Suggestions:"));

    let fetch_error = config_error(fetch_skill_files_from_github(&client, &source).await);
    assert!(!fetch_error.is_empty());
    assert!(!fetch_error.contains("missing SKILL.md"));
}

#[tokio::test]
async fn catalog_fetches_report_transport_failures() {
    let client = offline_client();

    let lobe_error = config_error(fetch_lobehub_skill_files(&client, "example").await);
    assert!(
        lobe_error.starts_with("LobeHub request failed: "),
        "{lobe_error}"
    );

    let claw_error = config_error(fetch_clawhub_skill_files(&client, "example", None).await);
    assert!(
        claw_error.starts_with("ClawHub detail request failed: "),
        "{claw_error}"
    );
}

fn stored_zip(entries: &[(&[u8], &[u8], u32)]) -> Vec<u8> {
    // Write stored entries directly so tests can control raw names and CRCs.
    let mut archive = Vec::new();
    let mut directory = Vec::new();

    for &(name, data, crc) in entries {
        let offset = u32::try_from(archive.len()).unwrap();
        let size = u32::try_from(data.len()).unwrap();
        let name_len = u16::try_from(name.len()).unwrap();
        let mut metadata = Vec::new();
        metadata.extend_from_slice(&20_u16.to_le_bytes()); // Version needed.
        metadata.extend_from_slice(&0x0800_u16.to_le_bytes()); // UTF-8 names.
        metadata.extend_from_slice(&0_u16.to_le_bytes()); // Stored compression.
        metadata.extend_from_slice(&0_u16.to_le_bytes()); // DOS time.
        metadata.extend_from_slice(&0x0021_u16.to_le_bytes()); // 1980-01-01.
        metadata.extend_from_slice(&crc.to_le_bytes());
        metadata.extend_from_slice(&size.to_le_bytes()); // Compressed size.
        metadata.extend_from_slice(&size.to_le_bytes()); // Uncompressed size.
        metadata.extend_from_slice(&name_len.to_le_bytes());
        metadata.extend_from_slice(&0_u16.to_le_bytes()); // Extra field length.

        archive.extend_from_slice(b"PK\x03\x04");
        archive.extend_from_slice(&metadata);
        archive.extend_from_slice(name);
        archive.extend_from_slice(data);

        directory.extend_from_slice(b"PK\x01\x02");
        directory.extend_from_slice(&20_u16.to_le_bytes()); // Version made by.
        directory.extend_from_slice(&metadata);
        directory.extend_from_slice(&[0; 6]); // Comment length, disk, internal attributes.
        let attributes = if name.ends_with(b"/") { 0x10_u32 } else { 0 };
        directory.extend_from_slice(&attributes.to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name);
    }

    let count = u16::try_from(entries.len()).unwrap();
    let directory_offset = u32::try_from(archive.len()).unwrap();
    let directory_size = u32::try_from(directory.len()).unwrap();
    archive.extend_from_slice(&directory);
    archive.extend_from_slice(b"PK\x05\x06");
    archive.extend_from_slice(&[0; 4]); // Disk numbers.
    archive.extend_from_slice(&count.to_le_bytes());
    archive.extend_from_slice(&count.to_le_bytes());
    archive.extend_from_slice(&directory_size.to_le_bytes());
    archive.extend_from_slice(&directory_offset.to_le_bytes());
    archive.extend_from_slice(&0_u16.to_le_bytes()); // Archive comment length.
    archive
}

#[test]
fn zip_extracts_root_files_and_normalizes_wrapped_paths() {
    let archive = stored_zip(&[
        (b"bundle/", b"", 0),
        (b"SKILL.md", b"abc", 0x3524_41c2),
        (b"bundle\\docs\\guide.txt", b"123456789", 0xcbf4_3926),
    ]);

    assert_eq!(
        extract_clawhub_archive(&archive).expect("extract stored ZIP"),
        vec![
            ("SKILL.md".to_owned(), Bytes::from_static(b"abc")),
            (
                "docs/guide.txt".to_owned(),
                Bytes::from_static(b"123456789")
            ),
        ]
    );
}

#[test]
fn zip_reports_an_archive_with_no_central_directory() {
    let message = config_error(extract_clawhub_archive(b"PK\x03\x04"));
    assert!(
        message.starts_with("Failed to parse ClawHub zip payload: "),
        "{message}"
    );
}

#[test]
fn zip_rejects_a_corrupt_local_entry_header() {
    let mut archive = stored_zip(&[
        (b"SKILL.md", b"abc", 0x3524_41c2),
        (b"bundle/notes.txt", b"", 0),
    ]);
    let second_header = 30 + b"SKILL.md".len() + b"abc".len();
    archive[second_header] = 0;

    let message = config_error(extract_clawhub_archive(&archive));
    assert!(
        message.starts_with("Failed to read ClawHub zip entry: ")
            || message.starts_with("Failed to parse ClawHub zip payload: "),
        "{message}"
    );
}

#[test]
fn zip_reports_a_bad_file_crc_as_a_payload_error() {
    let archive = stored_zip(&[(b"SKILL.md", b"abc", 0)]);

    let message = config_error(extract_clawhub_archive(&archive));
    assert!(
        message.starts_with("Failed to read ClawHub file payload: "),
        "{message}"
    );
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, data).expect("write gzip fixture");
    encoder.finish().expect("finish gzip fixture")
}

fn tar_header(kind: tar::EntryType, size: u64) -> tar::Header {
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(kind);
    header.set_mode(0o644);
    header.set_size(size);
    header
}

#[test]
fn tar_gz_extracts_wrapped_regular_files_only() {
    let mut tar = tar::Builder::new(Vec::new());
    let entries: [(&str, tar::EntryType, &[u8]); 4] = [
        ("bundle", tar::EntryType::Directory, b""),
        ("ignored.txt", tar::EntryType::Regular, b"unwrapped"),
        ("bundle/SKILL.md", tar::EntryType::Regular, b"# Example"),
        ("bundle/docs/notes.txt", tar::EntryType::Regular, b"notes"),
    ];
    for (path, kind, body) in entries {
        let mut header = tar_header(kind, u64::try_from(body.len()).unwrap());
        tar.append_data(&mut header, path, body)
            .expect("append tar fixture entry");
    }
    let archive = gzip(&tar.into_inner().expect("finish tar fixture"));

    assert_eq!(detect_archive_format(&archive), "tar.gz");
    assert_eq!(
        extract_clawhub_archive(&archive).expect("extract gzip tar"),
        vec![
            ("SKILL.md".to_owned(), Bytes::from_static(b"# Example")),
            ("docs/notes.txt".to_owned(), Bytes::from_static(b"notes")),
        ]
    );
}

#[test]
fn tar_gz_reports_a_truncated_gzip_header() {
    let message = config_error(extract_clawhub_archive(b"\x1f\x8b"));
    assert!(
        message.starts_with("Failed to read ClawHub tar entry: "),
        "{message}"
    );
}

#[test]
fn tar_gz_reports_a_stream_error_while_reading_file_data() {
    let mut header = tar_header(tar::EntryType::Regular, 4096);
    header.set_path("bundle/SKILL.md").expect("set tar path");
    header.set_cksum();

    let mut tar = header.as_bytes().to_vec();
    tar.extend_from_slice(b"partial");
    let mut archive = gzip(&tar);
    // The entry declares more bytes than the stream contains, so reading it
    // reaches the corrupted gzip footer before the file can be returned.
    let crc_offset = archive.len() - 8;
    archive[crc_offset] ^= 1;

    let message = config_error(extract_clawhub_archive(&archive));
    assert!(
        message.starts_with("Failed to read ClawHub tar payload: "),
        "{message}"
    );
}

#[test]
fn incomplete_magic_and_unrecognized_data_are_not_archives() {
    for data in [
        b"".as_slice(),
        b"PK\x03",
        b"\x1f",
        b"\x1f\x00",
        b"plain text",
    ] {
        assert_eq!(detect_archive_format(data), "unknown");
        assert_eq!(
            config_error(extract_clawhub_archive(data)),
            "Unsupported ClawHub archive format (expected zip or tar.gz)."
        );
    }
}
