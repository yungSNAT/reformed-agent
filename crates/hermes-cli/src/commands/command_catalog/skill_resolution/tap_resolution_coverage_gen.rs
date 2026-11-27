use super::*;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn tree_entries<const N: usize>(entries: [(&str, &str); N]) -> [GitHubTreeEntry; N] {
    entries.map(|(kind, path)| GitHubTreeEntry {
        kind: kind.to_string(),
        path: path.to_string(),
    })
}

#[derive(Default)]
struct FixtureTapSource {
    requests: std::sync::Mutex<Vec<String>>,
}

impl SkillTapSource for FixtureTapSource {
    async fn branch_and_tree(
        &self,
        repo: &str,
    ) -> Result<(String, Vec<GitHubTreeEntry>), AgentError> {
        self.requests.lock().unwrap().push(repo.to_owned());
        match repo {
            "fixture/primary" => Ok((
                "catalogue".into(),
                tree_entries([
                    ("blob", "skills/report-brief/SKILL.md"),
                    ("blob", "skills/report-a/SKILL.md"),
                    ("blob", "skills/REPORT/SKILL.md"),
                    ("blob", "skills/notes/SKILL.md"),
                    ("blob", "skillset/report-hidden/SKILL.md"),
                ])
                .into(),
            )),
            "fixture/secondary" => Ok((
                "secondary".into(),
                tree_entries([("blob", "reports/Report/SKILL.md")]).into(),
            )),
            "fixture/archive" => Ok((
                "reports".into(),
                (1..=9)
                    .rev()
                    .map(|index| GitHubTreeEntry {
                        kind: "blob".into(),
                        path: format!("skills/report-{index}/SKILL.md"),
                    })
                    .collect(),
            )),
            _ => Err(AgentError::Config("fixture repository unavailable".into())),
        }
    }
}

#[tokio::test]
async fn tap_lookup_respects_order_and_lists_only_in_scope_suggestions() {
    let source = FixtureTapSource::default();
    let taps = [
        String::new(),
        "fixture/unavailable".into(),
        "fixture/primary".into(),
        "fixture/secondary::".into(),
    ];
    let found = resolve_skill_via_taps(&source, &taps, "report")
        .await
        .expect("resolve preferred tap");
    assert_eq!(
        found,
        ResolvedSkillSource {
            repo: "fixture/primary".into(),
            branch: "catalogue".into(),
            skill_dir: "skills/REPORT".into(),
        }
    );
    assert_eq!(
        source.requests.lock().unwrap().as_slice(),
        ["fixture/unavailable", "fixture/primary"]
    );
    let root = resolve_skill_via_taps(&source, &taps[3..], "report")
        .await
        .expect("resolve root-scoped tap");
    assert_eq!(root.skill_dir, "reports/Report");
    assert_eq!(
        config_error(resolve_skill_via_taps(&source, &taps, "report-").await),
        "Skill 'report-' not found in configured taps. Suggestions: report-a, report-brief"
    );
    let archive = ["fixture/archive".into()];
    assert_eq!(
        config_error(resolve_skill_via_taps(&source, &archive, "report").await),
        "Skill 'report' not found in configured taps. Suggestions: report-1, report-2, report-3, report-4, report-5, report-6, report-7, report-8"
    );
}

#[tokio::test]
async fn tap_search_keeps_unique_paths_and_honours_the_result_limit() {
    let taps = [
        "fixture/primary".into(),
        "fixture/primary".into(),
        "fixture/secondary::".into(),
    ];
    let expected: [(String, String); 4] = [
        ("report-brief".into(), "fixture/primary/skills/report-brief".into()),
        ("report-a".into(), "fixture/primary/skills/report-a".into()),
        ("REPORT".into(), "fixture/primary/skills/REPORT".into()),
        ("Report".into(), "fixture/secondary/reports/Report".into()),
    ];
    let full = search_skills_via_taps(&FixtureTapSource::default(), &taps, "REPORT", 10)
        .await
        .expect("search fixture taps");
    assert_eq!(full, expected);
    let source = FixtureTapSource::default();
    let limited = search_skills_via_taps(&source, &taps, "REPORT", 2)
        .await
        .expect("limited search");
    assert_eq!(limited, expected[..2]);
    assert_eq!(
        source.requests.lock().unwrap().as_slice(),
        ["fixture/primary"]
    );
}

#[test]
fn tap_skill_directories_only_include_manifest_blobs_beneath_the_prefix() {
    let tree = tree_entries([
        ("blob", "skills/North/README.md"),
        ("blob", "skillset/East/SKILL.md"),
        ("tree", "skills/South/SKILL.md"),
        ("blob", "skills/North/SKILL.md"),
        ("blob", "skills/surveys/West/SKILL.md"),
    ]);

    assert_eq!(
        skill_directories(&tree, "skills/").collect::<Vec<_>>(),
        [("skills/North", "North"), ("skills/surveys/West", "West")]
    );
}

#[test]
fn repository_skill_selection_prefers_the_requested_prefix_then_shortest_path() {
    let tree = tree_entries([
        ("blob", "skills/z/NORTH/SKILL.md"),
        ("blob", "skills/a/NORTH/SKILL.md"),
        ("blob", "skills/0-longer/NORTH/SKILL.md"),
        ("blob", "north/SKILL.md"),
    ]);

    assert_eq!(
        select_repository_skill(&tree, "fixture/catalog", "north", Some("/skills/")).unwrap(),
        "skills/a/NORTH"
    );
    assert_eq!(
        select_repository_skill(&tree, "fixture/catalog", "NORTH", None).unwrap(),
        "north"
    );
}

#[test]
fn repository_skill_selection_returns_sorted_unique_fuzzy_suggestions() {
    let tree = tree_entries([
        ("blob", "skills/north-z/SKILL.md"),
        ("blob", "skills/north-a/SKILL.md"),
        ("blob", "extras/north-a/SKILL.md"),
        ("blob", "extras/south/SKILL.md"),
    ]);

    assert_eq!(
        config_error(select_repository_skill(&tree, "fixture/catalog", "North", None)),
        "Skill 'North' not found in repo fixture/catalog. Suggestions: north-a, north-z"
    );
    assert_eq!(
        config_error(select_repository_skill(&tree, "fixture/catalog", "east", None)),
        "Skill 'east' not found in repo fixture/catalog. Suggestions: none"
    );
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

    let lobe_error =
        config_error(fetch_lobehub_skill_files(&client, LOBEHUB_AGENTS_BASE, "example").await);
    assert!(
        lobe_error.starts_with("LobeHub request failed: "),
        "{lobe_error}"
    );

    let claw_error =
        config_error(fetch_clawhub_skill_files(&client, CLAWHUB_API_BASE, "example", None).await);
    assert!(
        claw_error.starts_with("ClawHub detail request failed: "),
        "{claw_error}"
    );
}

#[tokio::test]
async fn skill_registry_converts_an_agent_manifest_and_reports_bad_responses() {
    let server = MockServer::start().await;
    assert!(server.address().ip().is_loopback());
    let client = offline_client();
    let registry = server.uri();
    let manifest = serde_json::json!({
        "meta": {"title": "Estuary notes", "description": "Summarise survey notes"},
        "config": {"systemRole": "List the birds sighted."}
    });
    Mock::given(method("GET"))
        .and(path("/estuary.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
        .expect(1)
        .mount(&server)
        .await;
    let bundle = fetch_lobehub_skill_files(&client, &registry, "estuary")
        .await
        .unwrap();
    assert_eq!(bundle.len(), 1);
    assert_eq!(bundle[0].0, "SKILL.md");
    let markdown = std::str::from_utf8(&bundle[0].1).unwrap();
    assert!(markdown.contains("name: estuary\ndescription: Summarise survey notes"));
    assert!(markdown.contains("# Estuary notes\n"));
    assert!(markdown.ends_with("## Instructions\nList the birds sighted.\n"));

    for (slug, response, expected) in [
        ("missing", ResponseTemplate::new(404), "LobeHub lookup failed"),
        (
            "malformed",
            ResponseTemplate::new(200).set_body_string("invalid JSON"),
            "Invalid LobeHub payload",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/{slug}.json")))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let error = config_error(fetch_lobehub_skill_files(&client, &registry, slug).await);
        assert!(error.contains(expected), "{error}");
    }
}

#[tokio::test]
async fn skill_registry_downloads_pinned_and_latest_archives_and_retries_throttling() {
    let server = MockServer::start().await;
    assert!(server.address().ip().is_loopback());
    Mock::given(method("GET"))
        .and(path("/skills/estuary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"latestVersion": {"version": " 2.0.0 "}}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download"))
        .and(query_param("slug", "estuary"))
        .and(query_param("version", "2.0.0"))
        .respond_with(ResponseTemplate::new(429))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    let client = offline_client();
    let registry = server.uri();
    for (hint, version) in [(None, "2.0.0"), (Some("1.0.0"), "1.0.0")] {
        let archive = stored_zip(&[(b"bundle/SKILL.md", b"abc", 0x3524_41c2)]);
        Mock::given(method("GET"))
            .and(path("/download"))
            .and(query_param("slug", "estuary"))
            .and(query_param("version", version))
            .respond_with(ResponseTemplate::new(200).set_body_raw(archive, "application/zip"))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        let bundle = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            fetch_clawhub_skill_files(&client, &registry, "estuary", hint),
        )
        .await
        .expect("loopback registry download terminates")
        .unwrap();
        assert_eq!(bundle, vec![("SKILL.md".to_owned(), Bytes::from_static(b"abc"))]);
    }
}

#[tokio::test]
async fn skill_registry_reports_missing_versions_and_failed_downloads() {
    let server = MockServer::start().await;
    assert!(server.address().ip().is_loopback());
    Mock::given(method("GET"))
        .and(path("/skills/estuary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download"))
        .and(query_param("version", "1.0.0"))
        .respond_with(ResponseTemplate::new(503).set_body_string("archive temporarily unavailable"))
        .expect(1)
        .mount(&server)
        .await;
    let client = offline_client();
    let registry = server.uri();
    let missing = fetch_clawhub_skill_files(&client, &registry, "estuary", Some(" ")).await;
    assert!(config_error(missing).contains("No ClawHub version available for 'estuary'"));
    let failed = fetch_clawhub_skill_files(&client, &registry, "estuary", Some("1.0.0")).await;
    assert!(config_error(failed).contains("503 Service Unavailable): archive temporarily unavailable"));
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
