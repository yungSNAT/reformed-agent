// Keep the coverage filter usable even when graph_backend.rs is included
// directly in teams_pipeline instead of being declared as a Rust module.
mod teams_pipeline {
    mod graph_backend {
        mod coverage_gen {
            use super::super::super::super::*;

            fn token_provider(direct_access_token: Option<&str>) -> MicrosoftGraphTokenProvider {
                MicrosoftGraphTokenProvider {
                    client: graph_http_client(),
                    config: MicrosoftGraphAuthConfig {
                        direct_access_token: direct_access_token.map(str::to_owned),
                        tenant_id: None,
                        client_id: None,
                        client_secret: None,
                        scope: "https://graph.microsoft.com/.default".into(),
                        authority_url: "https://login.microsoftonline.com".into(),
                    },
                    cache: Arc::new(tokio::sync::Mutex::new(None)),
                }
            }

            fn meeting_fixture() -> TeamsMeetingRef {
                normalize_meeting_ref(
                    &json!({
                        "id": "meeting/one",
                        "joinWebUrl": "https://teams.example.invalid/join",
                        "organizer": {"identity": {"user": {"id": "organizer"}}}
                    }),
                    Some("tenant"),
                )
                .unwrap()
            }

            #[test]
            fn graph_environment_configuration() {
                const CASE_KEY: &str = "HERMES_GRAPH_COVERAGE_CHILD_CASE";
                const GRAPH_KEYS: &[&str] = &[
                    "MSGRAPH_ACCESS_TOKEN",
                    "TEAMS_GRAPH_ACCESS_TOKEN",
                    "MSGRAPH_TENANT_ID",
                    "MSGRAPH_CLIENT_ID",
                    "MSGRAPH_CLIENT_SECRET",
                    "MSGRAPH_SCOPE",
                    "MSGRAPH_AUTHORITY_URL",
                    "MSGRAPH_BASE_URL",
                ];

                if let Ok(case) = std::env::var(CASE_KEY) {
                    let backend = MicrosoftGraphTeamsBackend::from_env();
                    if case == "missing" {
                        assert!(matches!(
                            backend,
                            Err(TeamsPipelineError::Config(message))
                                if message.contains("Microsoft Graph is not configured")
                                    && message.contains("MSGRAPH_CLIENT_SECRET")
                        ));
                        return;
                    }

                    let backend = backend.unwrap();
                    let client = backend.client();
                    let provider = client.token_provider();
                    let default_scope = "https://graph.microsoft.com/.default";
                    let default_authority = "https://login.microsoftonline.com";
                    let default_base = "https://graph.microsoft.com/v1.0";
                    let (token, scope, authority, base) = match case.as_str() {
                        "legacy" => (
                            Some("legacy-token"),
                            default_scope,
                            default_authority,
                            default_base,
                        ),
                        "primary" => (
                            Some("primary-token"),
                            default_scope,
                            default_authority,
                            default_base,
                        ),
                        "credentials" => (
                            None,
                            "test-scope",
                            "https://identity.example.invalid",
                            "https://graph.example.invalid/beta",
                        ),
                        _ => panic!("unknown environment fixture: {case}"),
                    };
                    let has_credentials = case == "credentials";
                    assert_eq!(client.base_url, base);
                    assert_eq!(provider.config.direct_access_token.as_deref(), token);
                    assert_eq!(
                        provider.inspect_token_health(),
                        json!({
                            "configured": true,
                            "direct_access_token": token.is_some(),
                            "tenant_id": has_credentials,
                            "client_id": has_credentials,
                            "client_secret": has_credentials,
                            "scope": scope,
                            "authority_url": authority
                        })
                    );
                    return;
                }

                // Configure each child without changing the parent test process's environment.
                let home = tempfile::tempdir().unwrap();
                let executable = std::env::current_exe().unwrap();
                let qualified_name = concat!(module_path!(), "::graph_environment_configuration");
                let (_, test_name) = qualified_name.split_once("::").unwrap();
                for case in ["missing", "legacy", "primary", "credentials"] {
                    let mut command = std::process::Command::new(&executable);
                    command
                        .args(["--exact", test_name])
                        .env(CASE_KEY, case)
                        .env("HOME", home.path())
                        .env("USERPROFILE", home.path());
                    for key in GRAPH_KEYS {
                        command.env_remove(key);
                    }
                    match case {
                        "missing" => {
                            command
                                .env("MSGRAPH_TENANT_ID", "tenant")
                                .env("MSGRAPH_CLIENT_ID", "client");
                        }
                        "legacy" => {
                            command
                                .env("MSGRAPH_ACCESS_TOKEN", "")
                                .env("TEAMS_GRAPH_ACCESS_TOKEN", "legacy-token");
                        }
                        "primary" => {
                            command
                                .env("MSGRAPH_ACCESS_TOKEN", "primary-token")
                                .env("TEAMS_GRAPH_ACCESS_TOKEN", "legacy-token");
                        }
                        "credentials" => {
                            command
                                .env("MSGRAPH_TENANT_ID", "tenant")
                                .env("MSGRAPH_CLIENT_ID", "client")
                                .env("MSGRAPH_CLIENT_SECRET", "secret")
                                .env("MSGRAPH_SCOPE", "test-scope")
                                .env("MSGRAPH_AUTHORITY_URL", "https://identity.example.invalid")
                                .env("MSGRAPH_BASE_URL", "https://graph.example.invalid/beta");
                        }
                        _ => unreachable!(),
                    }

                    let output = command.output().unwrap();
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert!(
                        output.status.success() && stdout.contains("1 passed"),
                        "{case}: {stdout}\n{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }

            #[tokio::test]
            async fn token_cache_requires_a_fresh_entry_and_honors_force_refresh() {
                let provider = token_provider(None);
                assert_eq!(provider.inspect_token_health()["configured"], json!(false));
                *provider.cache.lock().await = Some(CachedGraphToken {
                    access_token: "cached".into(),
                    expires_at: Timestamp::now() + SignedDuration::from_secs(600),
                });
                assert_eq!(provider.get_access_token(false).await.unwrap(), "cached");
                assert!(matches!(
                    provider.get_access_token(true).await,
                    Err(TeamsPipelineError::Config(message))
                        if message == "MSGRAPH_TENANT_ID is missing."
                ));

                provider.cache.lock().await.as_mut().unwrap().expires_at = Timestamp::now();
                assert!(matches!(
                    provider.get_access_token(false).await,
                    Err(TeamsPipelineError::Config(message))
                        if message == "MSGRAPH_TENANT_ID is missing."
                ));

                let direct = token_provider(Some("direct-token"));
                assert_eq!(direct.get_access_token(true).await.unwrap(), "direct-token");
                assert!(direct.cache.lock().await.is_none());
            }

            #[test]
            fn build_url_preserves_existing_query_and_encodes_new_parameters() {
                let client = MicrosoftGraphClient::new(
                    token_provider(None),
                    "https://graph.example.invalid/v1.0///",
                );
                let relative = client.build_url("///users", &[]).unwrap();
                assert_eq!(relative.host_str(), Some("graph.example.invalid"));
                assert_eq!(relative.path(), "/v1.0/users");

                let absolute = client
                    .build_url(
                        "https://pagination.example.invalid/items?cursor=old",
                        &[
                            ("cursor", "new+value".into()),
                            ("$filter", "name eq 'A&B'".into()),
                        ],
                    )
                    .unwrap();
                assert_eq!(absolute.host_str(), Some("pagination.example.invalid"));
                assert_eq!(absolute.path(), "/items");
                assert_eq!(
                    absolute.query_pairs().into_owned().collect::<Vec<_>>(),
                    vec![
                        ("cursor".into(), "old".into()),
                        ("cursor".into(), "new+value".into()),
                        ("$filter".into(), "name eq 'A&B'".into())
                    ]
                );
                assert!(matches!(
                    client.build_url("https://[invalid", &[]),
                    Err(TeamsPipelineError::Config(message))
                        if message.starts_with("invalid Graph URL:")
                ));
            }

            #[test]
            fn friendly_errors_preserve_status_and_extract_useful_details() {
                for (status, body, expected) in [
                    (
                        StatusCode::UNAUTHORIZED,
                        r#"{"error":{"message":"expired"}}"#,
                        "Microsoft Graph permission denied or token expired: expired",
                    ),
                    (
                        StatusCode::FORBIDDEN,
                        r#"{"error":"blocked"}"#,
                        "Microsoft Graph permission denied or token expired: blocked",
                    ),
                    (
                        StatusCode::NOT_FOUND,
                        "  missing\n",
                        "Microsoft Graph resource not found: missing",
                    ),
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        r#"{"error":{"message":"slow down"}}"#,
                        "Microsoft Graph rate limited: slow down",
                    ),
                    (
                        StatusCode::BAD_REQUEST,
                        r#"  {"error":{"message":42}}  "#,
                        r#"{"error":{"message":42}}"#,
                    ),
                ] {
                    assert!(matches!(
                        friendly_graph_error(status, body),
                        TeamsPipelineError::Graph { status: actual, message }
                            if actual == status.as_u16() && message == expected
                    ));
                }
            }

            #[test]
            fn meeting_normalization_extracts_nested_fields_and_only_keeps_known_metadata() {
                let expected_metadata = json!({
                    "subject": "Coverage review",
                    "startDateTime": "2025-01-02T12:00:00Z",
                    "endDateTime": "2025-01-02T13:00:00Z",
                    "createdDateTime": "2025-01-01T12:00:00Z",
                    "participants": {"attendees": []}
                });
                let mut payload = expected_metadata.clone();
                payload["id"] = json!("meeting/one");
                payload["organizer"] = json!({"identity": {"user": {"id": "organizer"}}});
                payload["joinWebUrl"] = json!("https://teams.example.invalid/join");
                payload["calendarEventId"] = json!("event");
                payload["chatInfo"] = json!({"threadId": "nested-thread"});
                payload["threadId"] = json!("flat-thread");
                payload["tenantId"] = json!("payload-tenant");
                payload["extra"] = json!("discarded");

                let meeting = normalize_meeting_ref(&payload, Some("override-tenant")).unwrap();
                assert_eq!(meeting.meeting_id, "meeting/one");
                assert_eq!(meeting.organizer_user_id.as_deref(), Some("organizer"));
                assert_eq!(
                    meeting.join_web_url.as_deref(),
                    Some("https://teams.example.invalid/join")
                );
                assert_eq!(meeting.calendar_event_id.as_deref(), Some("event"));
                assert_eq!(meeting.thread_id.as_deref(), Some("nested-thread"));
                assert_eq!(meeting.tenant_id.as_deref(), Some("override-tenant"));
                assert_eq!(Value::Object(meeting.metadata), expected_metadata);

                payload["chatInfo"] = json!({});
                let meeting = normalize_meeting_ref(&payload, None).unwrap();
                assert_eq!(meeting.tenant_id.as_deref(), Some("payload-tenant"));
                assert_eq!(meeting.thread_id.as_deref(), Some("flat-thread"));
                assert!(matches!(
                    normalize_meeting_ref(&json!([]), None),
                    Err(TeamsPipelineError::Invalid(message))
                        if message == "Graph meeting payload was not an object."
                ));
                assert!(matches!(
                    normalize_meeting_ref(&json!({"subject": "No ID"}), None),
                    Err(TeamsPipelineError::Invalid(_))
                ));
            }

            #[test]
            fn artifact_normalization_preserves_metadata_and_applies_field_priorities() {
                let mut payload = json!({
                    "id": "artifact/one",
                    "@microsoft.graph.downloadUrl": "https://example.invalid/annotation",
                    "downloadUrl": "https://example.invalid/download",
                    "recordingContentUrl": "https://example.invalid/recording",
                    "transcriptContentUrl": "https://example.invalid/transcript",
                    "webUrl": "https://example.invalid/web",
                    "contentUrl": "https://example.invalid/content",
                    "displayName": "Preferred name",
                    "name": "Fallback name",
                    "contentType": "text/vtt",
                    "fileMimeType": "text/plain",
                    "createdDateTime": "2025-01-01T12:00:00Z",
                    "lastModifiedDateTime": "2025-01-03T12:00:00Z",
                    "meetingEndDateTime": "2025-01-02T12:00:00Z",
                    "size": 123,
                    "status": "available"
                });
                let default_source = Some("https://example.invalid/default".to_owned());
                let artifact =
                    normalize_artifact("transcript", &payload, default_source.clone()).unwrap();
                assert_eq!(artifact.artifact_type, "transcript");
                assert_eq!(artifact.artifact_id, "artifact/one");
                assert_eq!(artifact.display_name.as_deref(), Some("Preferred name"));
                assert_eq!(artifact.content_type.as_deref(), Some("text/vtt"));
                assert_eq!(
                    artifact.source_url.as_deref(),
                    Some("https://example.invalid/web")
                );
                assert_eq!(artifact.created_at.as_deref(), Some("2025-01-01T12:00:00Z"));
                assert_eq!(
                    artifact.available_at.as_deref(),
                    Some("2025-01-03T12:00:00Z")
                );
                assert_eq!(artifact.size_bytes, Some(123));
                assert_eq!(Value::Object(artifact.metadata), payload);

                for (field, expected) in [
                    (
                        "@microsoft.graph.downloadUrl",
                        "https://example.invalid/annotation",
                    ),
                    ("downloadUrl", "https://example.invalid/download"),
                    ("recordingContentUrl", "https://example.invalid/recording"),
                    ("transcriptContentUrl", "https://example.invalid/transcript"),
                ] {
                    let artifact = normalize_artifact("transcript", &payload, None).unwrap();
                    assert_eq!(artifact.download_url.as_deref(), Some(expected));
                    assert!(payload.as_object_mut().unwrap().remove(field).is_some());
                }

                for field in [
                    "webUrl",
                    "displayName",
                    "contentType",
                    "lastModifiedDateTime",
                ] {
                    assert!(payload.as_object_mut().unwrap().remove(field).is_some());
                }
                let artifact =
                    normalize_artifact("transcript", &payload, default_source.clone()).unwrap();
                assert_eq!(artifact.download_url, None);
                assert_eq!(
                    artifact.source_url.as_deref(),
                    Some("https://example.invalid/content")
                );
                assert_eq!(artifact.display_name.as_deref(), Some("Fallback name"));
                assert_eq!(artifact.content_type.as_deref(), Some("text/plain"));
                assert_eq!(
                    artifact.available_at.as_deref(),
                    Some("2025-01-02T12:00:00Z")
                );
                assert!(payload
                    .as_object_mut()
                    .unwrap()
                    .remove("contentUrl")
                    .is_some());
                assert_eq!(
                    normalize_artifact("transcript", &payload, default_source.clone())
                        .unwrap()
                        .source_url,
                    default_source
                );
                assert!(normalize_artifact("transcript", &Value::Null, None).is_err());
                assert!(normalize_artifact("recording", &json!({"name": "No ID"}), None).is_err());
            }

            #[test]
            fn artifact_content_paths_use_explicit_urls_or_encode_both_identifiers() {
                let meeting = meeting_fixture();
                assert_eq!(
                    meeting_path(&meeting.meeting_id),
                    "/communications/onlineMeetings/meeting%2Fone"
                );
                let payload = json!({"id": "artifact/one"});
                let recording = normalize_artifact("recording", &payload, None).unwrap();
                let transcript = normalize_artifact("transcript", &payload, None).unwrap();
                assert_eq!(
                    recording_download_path(&meeting, &recording),
                    "/communications/onlineMeetings/meeting%2Fone/recordings/artifact%2Fone/content"
                );
                assert_eq!(
                    transcript_download_path(&meeting, &transcript),
                    "/communications/onlineMeetings/meeting%2Fone/transcripts/artifact%2Fone/content"
                );

                let url = "https://download.example.invalid/content?ticket=abc";
                let explicit = json!({"id": "artifact/one", "downloadUrl": url});
                let recording = normalize_artifact("recording", &explicit, None).unwrap();
                let transcript = normalize_artifact("transcript", &explicit, None).unwrap();
                assert_eq!(recording_download_path(&meeting, &recording), url);
                assert_eq!(transcript_download_path(&meeting, &transcript), url);
            }

            #[test]
            fn preferred_transcript_ranks_completion_then_url_then_effective_timestamp() {
                let recording = normalize_artifact(
                    "recording",
                    &json!({
                        "id": "recording", "status": "Completed",
                        "downloadUrl": "https://example.invalid/recording",
                        "createdDateTime": "2025-01-10T12:00:00Z"
                    }),
                    None,
                )
                .unwrap();
                assert!(select_preferred_transcript(&[]).is_none());
                assert!(select_preferred_transcript(std::slice::from_ref(&recording)).is_none());

                let mut candidates = vec![recording];
                for (payload, preferred_id) in [
                    (json!({"id": "unknown-status"}), "unknown-status"),
                    (
                        json!({
                            "id": "processing", "status": "processing",
                            "downloadUrl": "https://example.invalid/processing",
                            "createdDateTime": "2025-01-09T12:00:00Z"
                        }),
                        "processing",
                    ),
                    (
                        json!({
                            "id": "completed-no-url", "status": "COMPLETED",
                            "createdDateTime": "2025-01-08T12:00:00Z"
                        }),
                        "completed-no-url",
                    ),
                    (
                        json!({
                            "id": "available", "status": "Available",
                            "webUrl": "https://example.invalid/source",
                            "createdDateTime": "2025-01-03T12:00:00Z"
                        }),
                        "available",
                    ),
                    (
                        json!({
                            "id": "older", "status": "succeeded",
                            "downloadUrl": "https://example.invalid/older",
                            "createdDateTime": "2025-01-07T12:00:00Z",
                            "lastModifiedDateTime": "2025-01-02T12:00:00Z"
                        }),
                        "available",
                    ),
                    (
                        json!({
                            "id": "newest", "status": "Succeeded",
                            "downloadUrl": "https://example.invalid/newest",
                            "lastModifiedDateTime": "2025-01-04T12:00:00Z"
                        }),
                        "newest",
                    ),
                ] {
                    candidates.push(normalize_artifact("transcript", &payload, None).unwrap());
                    assert_eq!(
                        select_preferred_transcript(&candidates)
                            .unwrap()
                            .artifact_id,
                        preferred_id
                    );
                }
            }

            #[test]
            fn call_record_artifact_keeps_the_payload_and_reports_counts() {
                let payload = json!({
                    "id": "call/one",
                    "type": "groupCall",
                    "webUrl": "https://example.invalid/call",
                    "startDateTime": "2025-01-01T12:00:00Z",
                    "endDateTime": "2025-01-01T13:00:00Z",
                    "sessions": [{"id": "s1"}, {"id": "s2"}],
                    "participants": [{"id": "p1"}],
                    "version": 3,
                    "modalities": ["audio", "video"],
                    "organizer": {"identity": {"user": {"id": "organizer"}}}
                });
                let artifact = fetch_call_record_artifact_from_payload(&payload).unwrap();
                assert_eq!(artifact.artifact_type, "call_record");
                assert_eq!(artifact.artifact_id, "call/one");
                assert_eq!(artifact.display_name.as_deref(), Some("groupCall"));
                assert_eq!(
                    artifact.source_url.as_deref(),
                    Some("https://example.invalid/call")
                );
                assert_eq!(artifact.created_at.as_deref(), Some("2025-01-01T12:00:00Z"));
                assert_eq!(
                    artifact.available_at.as_deref(),
                    Some("2025-01-01T13:00:00Z")
                );
                assert!(artifact.download_url.is_none());
                assert!(artifact.content_type.is_none());
                assert!(artifact.size_bytes.is_none());
                assert_eq!(
                    Value::Object(artifact.metadata),
                    json!({
                        "call_record": payload,
                        "metrics": {
                            "version": 3,
                            "modalities": ["audio", "video"],
                            "participant_count": 1,
                            "session_count": 2,
                            "organizer": "organizer"
                        }
                    })
                );

                let minimal = fetch_call_record_artifact_from_payload(&json!({
                    "id": "minimal", "sessions": [], "participants": false
                }))
                .unwrap();
                assert_eq!(minimal.display_name.as_deref(), Some("call_record"));
                assert_eq!(minimal.metadata["metrics"], json!({"participant_count": 0}));
                assert!(fetch_call_record_artifact_from_payload(&json!({})).is_none());
                assert!(fetch_call_record_artifact_from_payload(&json!([])).is_none());
            }

            #[tokio::test]
            async fn unsupported_scheme_is_reported_as_a_transport_error() {
                // Reqwest rejects file URLs before opening a socket or a file.
                let client = MicrosoftGraphClient::new(
                    token_provider(Some("direct-token")),
                    "file:///graph",
                );
                assert!(matches!(
                    client.get_json("/meetings", &[]).await,
                    Err(TeamsPipelineError::Graph { status: 0, message })
                        if message.starts_with("Graph request failed:")
                ));
                assert!(matches!(
                    client.download_bytes("/content").await,
                    Err(TeamsPipelineError::Graph { status: 0, message })
                        if message.starts_with("Graph download failed:")
                ));
            }

            #[tokio::test]
            async fn blank_meeting_references_and_call_record_ids_do_not_issue_requests() {
                let backend = MicrosoftGraphTeamsBackend::new(MicrosoftGraphClient::new(
                    token_provider(None),
                    "file:///graph",
                ));
                assert!(matches!(
                    backend
                        .resolve_meeting_reference(Some(" \t"), Some("\n"), None)
                        .await,
                    Err(TeamsPipelineError::Invalid(message))
                        if message == "Either meeting_id or join_web_url is required."
                ));
                let meeting = meeting_fixture();
                for id in [None, Some(" \t")] {
                    assert!(backend
                        .enrich_meeting_with_call_record(&meeting, id)
                        .await
                        .unwrap()
                        .is_none());
                }
            }

            #[cfg(unix)]
            mod local_http {
                use super::*;
                use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

                const BASE_URL: &str = "http://graph.example.invalid/v1.0";

                struct Reply {
                    status: &'static str,
                    body: &'static [u8],
                    content_length: usize,
                }

                impl Reply {
                    fn new(status: &'static str, body: &'static [u8]) -> Self {
                        Self {
                            status,
                            body,
                            content_length: body.len(),
                        }
                    }
                }

                struct Request {
                    method: String,
                    target: String,
                    headers: Vec<(String, String)>,
                    body: Vec<u8>,
                }

                impl Request {
                    fn header(&self, name: &str) -> Option<&str> {
                        self.headers
                            .iter()
                            .find(|(key, _)| key == name)
                            .map(|(_, value)| value.as_str())
                    }

                    fn url(&self) -> reqwest::Url {
                        reqwest::Url::parse(&format!("http://graph.example.invalid{}", self.target))
                            .unwrap()
                    }

                    fn assert_graph_request(&self, method: &str, path: &str) {
                        assert_eq!(self.method, method);
                        assert_eq!(self.url().path(), path);
                        assert_eq!(self.header("authorization"), Some("Bearer direct-token"));
                    }
                }

                // HTTP framing runs over a temporary Unix socket, without TCP or DNS.
                struct Stub {
                    _directory: tempfile::TempDir,
                    client: Client,
                    requests: tokio::task::JoinHandle<Vec<Request>>,
                }

                impl Stub {
                    fn new(replies: Vec<Reply>) -> Self {
                        let directory = tempfile::tempdir().unwrap();
                        let socket = directory.path().join("graph.sock");
                        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
                        let client = Client::builder()
                            .no_proxy()
                            .http1_only()
                            .unix_socket(socket)
                            .timeout(Duration::from_secs(3))
                            .build()
                            .unwrap();
                        let requests = tokio::spawn(async move {
                            let mut requests = Vec::with_capacity(replies.len());
                            for reply in replies {
                                let (stream, _) = listener.accept().await.unwrap();
                                let mut stream = BufReader::new(stream);
                                let mut line = String::new();
                                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                                let mut words = line.split_ascii_whitespace();
                                let mut request = Request {
                                    method: words.next().unwrap().into(),
                                    target: words.next().unwrap().into(),
                                    headers: Vec::new(),
                                    body: Vec::new(),
                                };
                                loop {
                                    line.clear();
                                    assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                                    if line == "\r\n" {
                                        break;
                                    }
                                    let (name, value) = line.split_once(':').unwrap();
                                    request
                                        .headers
                                        .push((name.to_ascii_lowercase(), value.trim().to_owned()));
                                }
                                let length = request
                                    .header("content-length")
                                    .map_or(0, |value| value.parse::<usize>().unwrap());
                                request.body.resize(length, 0);
                                stream.read_exact(&mut request.body).await.unwrap();
                                requests.push(request);

                                let Reply {
                                    status,
                                    body,
                                    content_length,
                                } = reply;
                                let head = format!(
                                    "HTTP/1.1 {status}\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
                                );
                                stream.get_mut().write_all(head.as_bytes()).await.unwrap();
                                stream.get_mut().write_all(body).await.unwrap();
                                stream.get_mut().shutdown().await.unwrap();
                            }
                            requests
                        });
                        Self {
                            _directory: directory,
                            client,
                            requests,
                        }
                    }

                    fn graph_client(&self) -> MicrosoftGraphClient {
                        MicrosoftGraphClient {
                            client: self.client.clone(),
                            token_provider: token_provider(Some("direct-token")),
                            base_url: BASE_URL.into(),
                        }
                    }

                    async fn finish(self) -> Vec<Request> {
                        tokio::time::timeout(Duration::from_secs(3), self.requests)
                            .await
                            .expect("expected HTTP requests were not received")
                            .expect("Unix socket fixture failed")
                    }
                }

                #[tokio::test]
                async fn oauth_response_errors_do_not_cache_and_successful_tokens_are_reused() {
                    let stub = Stub::new(vec![
                        Reply::new("401 Unauthorized", b"fixture rejected the secret"),
                        Reply::new("200 OK", br#"{"access_token":" "}"#),
                        Reply::new("200 OK", br#"{"access_token":"issued","expires_in":600}"#),
                        Reply::new("200 OK", br#"{"access_token":"refreshed","expires_in":1}"#),
                    ]);
                    let mut provider = token_provider(None);
                    provider.client = stub.client.clone();
                    provider.config.tenant_id = Some("tenant/one".into());
                    provider.config.client_id = Some("client".into());
                    provider.config.client_secret = Some("secret+&".into());
                    provider.config.scope = "fixture-scope".into();
                    provider.config.authority_url =
                        "http://identity.example.invalid/authority///".into();

                    assert!(matches!(
                        provider.get_access_token(false).await,
                        Err(TeamsPipelineError::Config(message))
                            if message.contains("HTTP 401")
                                && message.contains("fixture rejected the secret")
                    ));
                    assert!(provider.cache.lock().await.is_none());
                    assert!(matches!(
                        provider.get_access_token(false).await,
                        Err(TeamsPipelineError::Config(message))
                            if message == "Graph token response omitted access_token."
                    ));
                    assert!(provider.cache.lock().await.is_none());

                    assert_eq!(provider.get_access_token(false).await.unwrap(), "issued");
                    assert_eq!(provider.get_access_token(false).await.unwrap(), "issued");
                    let before = Timestamp::now();
                    assert_eq!(provider.get_access_token(true).await.unwrap(), "refreshed");
                    let after = Timestamp::now();
                    let cached = provider.cache.lock().await.clone().unwrap();
                    assert_eq!(cached.access_token, "refreshed");
                    assert!(cached.expires_at >= before + SignedDuration::from_secs(60));
                    assert!(cached.expires_at <= after + SignedDuration::from_secs(60));

                    for request in stub.finish().await {
                        assert_eq!(request.method, "POST");
                        assert_eq!(
                            request.url().path(),
                            "/authority/tenant%2Fone/oauth2/v2.0/token"
                        );
                        assert_eq!(
                            url::form_urlencoded::parse(&request.body)
                                .into_owned()
                                .collect::<Vec<_>>(),
                            vec![
                                ("client_id".into(), "client".into()),
                                ("client_secret".into(), "secret+&".into()),
                                ("scope".into(), "fixture-scope".into()),
                                ("grant_type".into(), "client_credentials".into())
                            ]
                        );
                    }
                }

                #[tokio::test]
                async fn json_methods_send_correct_bodies_and_handle_empty_and_error_responses() {
                    let stub = Stub::new(vec![
                        Reply::new("201 Created", br#"{"id":"new"}"#),
                        Reply::new("200 OK", b" \r\n"),
                        Reply::new("204 No Content", b""),
                        Reply::new(
                            "429 Too Many Requests",
                            br#"{"error":{"message":"retry later"}}"#,
                        ),
                    ]);
                    let client = stub.graph_client();
                    let create = json!({"subject": "Original"});
                    let update = json!({"subject": "Updated"});
                    assert_eq!(
                        client.post_json("/meetings", create.clone()).await.unwrap(),
                        json!({"id": "new"})
                    );
                    assert_eq!(
                        client
                            .patch_json("/meetings/new", update.clone())
                            .await
                            .unwrap(),
                        json!({"success": true, "status_code": 200, "empty": true})
                    );
                    assert_eq!(
                        client.delete_json("/meetings/new").await.unwrap(),
                        json!({"success": true, "status_code": 204, "empty": true})
                    );
                    assert!(matches!(
                        client.get_json("/meetings", &[]).await,
                        Err(TeamsPipelineError::Graph { status: 429, message })
                            if message == "Microsoft Graph rate limited: retry later"
                    ));

                    let requests = stub.finish().await;
                    requests[0].assert_graph_request("POST", "/v1.0/meetings");
                    requests[1].assert_graph_request("PATCH", "/v1.0/meetings/new");
                    requests[2].assert_graph_request("DELETE", "/v1.0/meetings/new");
                    requests[3].assert_graph_request("GET", "/v1.0/meetings");
                    assert_eq!(
                        serde_json::from_slice::<Value>(&requests[0].body).unwrap(),
                        create
                    );
                    assert_eq!(
                        serde_json::from_slice::<Value>(&requests[1].body).unwrap(),
                        update
                    );
                    assert!(requests[2].body.is_empty());
                    assert!(requests[3].body.is_empty());
                }

                #[tokio::test]
                async fn downloads_preserve_binary_data_and_distinguish_status_from_read_errors() {
                    let stub = Stub::new(vec![
                        Reply::new("200 OK", b"\x00\xff\x80"),
                        Reply::new("404 Not Found", b" gone \n"),
                        Reply {
                            status: "200 OK",
                            body: b"short",
                            content_length: 40,
                        },
                    ]);
                    let client = stub.graph_client();
                    assert_eq!(
                        client.download_bytes("/binary").await.unwrap(),
                        b"\x00\xff\x80"
                    );
                    assert!(matches!(
                        client.download_bytes("/missing").await,
                        Err(TeamsPipelineError::Graph { status: 404, message })
                            if message == "Microsoft Graph resource not found: gone"
                    ));
                    assert!(matches!(
                        client.download_bytes("/truncated").await,
                        Err(TeamsPipelineError::Graph { status: 0, message })
                            if message.starts_with("Graph download read failed:")
                    ));

                    let requests = stub.finish().await;
                    for (request, path) in
                        requests
                            .iter()
                            .zip(["/v1.0/binary", "/v1.0/missing", "/v1.0/truncated"])
                    {
                        request.assert_graph_request("GET", path);
                    }
                }

                #[tokio::test]
                async fn meeting_resolution_prefers_id_and_escapes_apostrophes_in_join_url() {
                    let stub = Stub::new(vec![
                        Reply::new("200 OK", br#"{"id":"meeting/one","tenantId":"payload"}"#),
                        Reply::new("200 OK", br#"{"value":[]}"#),
                        Reply::new("200 OK", br#"{"value":[{"id":"first"},{"id":"second"}]}"#),
                    ]);
                    let backend = MicrosoftGraphTeamsBackend::new(stub.graph_client());
                    let join_url = "https://teams.example.invalid/join/O'Brien";
                    let by_id = backend
                        .resolve_meeting_reference(
                            Some("meeting/one"),
                            Some(join_url),
                            Some("override"),
                        )
                        .await
                        .unwrap();
                    assert_eq!(by_id.meeting_id, "meeting/one");
                    assert_eq!(by_id.tenant_id.as_deref(), Some("override"));
                    assert!(matches!(
                        backend.resolve_meeting_reference(Some(" "), Some(join_url), None).await,
                        Err(TeamsPipelineError::Invalid(message))
                            if message == format!("Teams meeting not found for join URL: {join_url}")
                    ));
                    let by_url = backend
                        .resolve_meeting_reference(None, Some(join_url), None)
                        .await
                        .unwrap();
                    assert_eq!(by_url.meeting_id, "first");

                    let requests = stub.finish().await;
                    requests[0].assert_graph_request(
                        "GET",
                        "/v1.0/communications/onlineMeetings/meeting%2Fone",
                    );
                    for request in &requests[1..] {
                        request.assert_graph_request("GET", "/v1.0/communications/onlineMeetings");
                        assert_eq!(
                            request.url().query_pairs().into_owned().collect::<Vec<_>>(),
                            vec![(
                                "$filter".into(),
                                "JoinWebUrl eq 'https://teams.example.invalid/join/O''Brien'"
                                    .into()
                            )]
                        );
                    }
                }

                #[tokio::test]
                async fn backend_filters_artifacts_and_downloads_the_preferred_transcript_and_recording(
                ) {
                    let stub = Stub::new(vec![
                        Reply::new(
                            "200 OK",
                            br#"{"value":[{},{"id":"pending","status":"processing"}],"@odata.nextLink":"http://graph.example.invalid/next?cursor=2"}"#,
                        ),
                        Reply::new(
                            "200 OK",
                            br#"{"value":[{"id":"final/vtt","status":"completed"}]}"#,
                        ),
                        Reply::new("200 OK", b" \nWEBVTT\nspeaker: hello \xff \r\n"),
                        Reply::new(
                            "200 OK",
                            br#"{"value":[null,{"id":"video","recordingContentUrl":"http://graph.example.invalid/download?ticket=1"}]}"#,
                        ),
                        Reply::new("200 OK", b"\x00\xffvideo"),
                    ]);
                    let backend = MicrosoftGraphTeamsBackend::new(stub.graph_client());
                    let meeting = meeting_fixture();

                    let (transcript, text) = backend
                        .fetch_preferred_transcript_text(&meeting)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(transcript.artifact_type, "transcript");
                    assert_eq!(transcript.artifact_id, "final/vtt");
                    assert_eq!(text, "WEBVTT\nspeaker: hello \u{fffd}");

                    let recordings = backend.list_recording_artifacts(&meeting).await.unwrap();
                    assert_eq!(recordings.len(), 1);
                    assert_eq!(recordings[0].artifact_type, "recording");
                    assert_eq!(recordings[0].artifact_id, "video");
                    assert_eq!(
                        backend
                            .download_recording_artifact(&meeting, &recordings[0])
                            .await
                            .unwrap(),
                        b"\x00\xffvideo"
                    );

                    let requests = stub.finish().await;
                    for (request, path) in requests.iter().zip([
                        "/v1.0/communications/onlineMeetings/meeting%2Fone/transcripts",
                        "/next",
                        "/v1.0/communications/onlineMeetings/meeting%2Fone/transcripts/final%2Fvtt/content",
                        "/v1.0/communications/onlineMeetings/meeting%2Fone/recordings",
                        "/download",
                    ]) {
                        request.assert_graph_request("GET", path);
                    }
                    assert_eq!(requests[1].url().query(), Some("cursor=2"));
                    assert_eq!(requests[4].url().query(), Some("ticket=1"));
                }

                #[tokio::test]
                async fn call_record_enrichment_ignores_only_permission_and_not_found_statuses() {
                    let stub = Stub::new(vec![
                        Reply::new("200 OK", br#"{"id":"call/one","participants":[{}]}"#),
                        Reply::new("401 Unauthorized", b"expired"),
                        Reply::new("403 Forbidden", b"denied"),
                        Reply::new("404 Not Found", b"missing"),
                        Reply::new("500 Internal Server Error", b"backend unavailable"),
                    ]);
                    let backend = MicrosoftGraphTeamsBackend::new(stub.graph_client());
                    let meeting = meeting_fixture();
                    let artifact = backend
                        .enrich_meeting_with_call_record(&meeting, Some("call/one"))
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(artifact.artifact_id, "call/one");
                    assert_eq!(
                        artifact.metadata["metrics"],
                        json!({"participant_count": 1})
                    );
                    for id in ["expired", "denied", "missing"] {
                        assert!(backend
                            .enrich_meeting_with_call_record(&meeting, Some(id))
                            .await
                            .unwrap()
                            .is_none());
                    }
                    assert!(matches!(
                        backend.enrich_meeting_with_call_record(&meeting, Some("failed")).await,
                        Err(TeamsPipelineError::Graph { status: 500, message })
                            if message == "backend unavailable"
                    ));
                    let requests = stub.finish().await;
                    for (request, id) in requests.iter().zip([
                        "call%2Fone",
                        "expired",
                        "denied",
                        "missing",
                        "failed",
                    ]) {
                        request.assert_graph_request(
                            "GET",
                            &format!("/v1.0/communications/callRecords/{id}"),
                        );
                    }
                }
            }
        }
    }
}
