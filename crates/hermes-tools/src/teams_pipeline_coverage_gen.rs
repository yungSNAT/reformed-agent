use super::*;

#[test]
fn config_coerces_scalar_values_and_keeps_only_object_sink_configs() {
    assert_eq!(
        TeamsPipelineConfig::from_value(None),
        TeamsPipelineConfig::default()
    );
    assert_eq!(
        TeamsPipelineConfig::from_value(Some(json!(["not an object"]))),
        TeamsPipelineConfig::default()
    );
    assert_eq!(
        TeamsPipelineConfig::from_value(Some(json!({}))),
        TeamsPipelineConfig::default()
    );

    assert_eq!(
        TeamsPipelineConfig::from_value(Some(json!({
            "transcript_preferred": " OFF ",
            "transcript_required": " YeS ",
            "transcription_fallback": 0,
            "stt_model": 42,
            "ffmpeg_extract_audio": "unrecognized",
            "transcript_min_chars": " 128 ",
            "tmp_dir": " fixture-audio ",
            "notion": {"database_id": "notes"},
            "linear": ["not a config"],
            "teams_delivery": {"chat_id": "chat"}
        }))),
        TeamsPipelineConfig {
            transcript_preferred: false,
            transcript_required: true,
            transcription_fallback: false,
            stt_model: Some("42".into()),
            ffmpeg_extract_audio: true,
            transcript_min_chars: 128,
            tmp_dir: Some(PathBuf::from("fixture-audio")),
            notion: Some(json!({"database_id": "notes"})),
            linear: None,
            teams_delivery: Some(json!({"chat_id": "chat"})),
        }
    );

    for (value, default, expected) in [
        (json!(-4), false, true),
        (json!(1.5), true, true),
        (json!(1.5), false, false),
        (json!(null), true, true),
        (json!("unknown"), false, false),
    ] {
        assert_eq!(parse_bool(Some(&value), default), expected, "{value}");
    }
    for (value, expected) in [
        (json!(0), 0),
        (json!(12), 12),
        (json!(-1), 80),
        (json!("bad"), 80),
        (json!([]), 80),
    ] {
        assert_eq!(parse_usize(Some(&value), 80), expected, "{value}");
    }

    assert_eq!(value_to_string(&json!(true)).as_deref(), Some("true"));
    assert_eq!(value_to_string(&json!(" \t ")), None);
    assert_eq!(value_to_string(&json!({"value": 1})), None);
}

#[test]
fn utility_functions_preserve_nonempty_data_and_encode_path_segments() {
    assert_eq!(
        canonical_json(&json!({"z": [null, true, "a\n\"b", 4], "a": {"b": false}})),
        r#"{"a":{"b":false},"z":[null,true,"a\n\"b",4]}"#
    );
    assert_eq!(
        strip_empty_object_keys(json!({
            "null": null,
            "empty_array": [],
            "empty_object": {},
            "empty_string": "",
            "false": false,
            "array": [null],
            "object": {"nested_null": null}
        })),
        json!({
            "empty_string": "",
            "false": false,
            "array": [null],
            "object": {"nested_null": null}
        })
    );
    assert_eq!(
        path_percent_encode("Az09-_.~ /+?#%é"),
        "Az09-_.~%20%2F%2B%3F%23%25%C3%A9"
    );
    assert_eq!(parse_datetime_utc(" \t "), None);
    assert_eq!(
        parse_datetime_utc(" 2025-03-04T12:30:00+02:00 "),
        Some("2025-03-04T10:30:00Z".parse::<Timestamp>().unwrap())
    );
    for (resource, expected) in [
        (" \t ", None),
        ("///", None),
        ("/users/u/onlineMeetings/m/transcripts/t/", Some("m")),
        ("/users/u/onlineMeetings/", Some("onlineMeetings")),
        ("/users/u/events/e/", Some("e")),
    ] {
        assert_eq!(
            extract_meeting_id_from_resource(resource).as_deref(),
            expected
        );
    }
}

#[test]
fn errors_keep_their_category_and_original_message() {
    let error = TeamsPipelineError::from(std::io::Error::other("fixture IO failure"));
    assert!(matches!(&error, TeamsPipelineError::Io(message) if message == "fixture IO failure"));
    assert_eq!(
        error.to_string(),
        "Teams pipeline IO error: fixture IO failure"
    );

    let malformed = "{not JSON}";
    let original_message = serde_json::from_str::<Value>(malformed)
        .unwrap_err()
        .to_string();
    let json_error = parse_summary_json(malformed).unwrap_err();
    assert!(
        matches!(&json_error, TeamsPipelineError::Json(message) if message == &original_message)
    );

    for (error, name, retryable) in [
        (
            TeamsPipelineError::Config("fixture".into()),
            "Config",
            false,
        ),
        (
            TeamsPipelineError::Invalid("fixture".into()),
            "Invalid",
            false,
        ),
        (TeamsPipelineError::Store("fixture".into()), "Store", false),
        (error, "Io", false),
        (json_error, "Json", false),
        (
            TeamsPipelineError::Retryable("fixture".into()),
            "Retryable",
            true,
        ),
        (
            TeamsPipelineError::ArtifactNotFound("fixture".into()),
            "ArtifactNotFound",
            true,
        ),
        (TeamsPipelineError::Sink("fixture".into()), "Sink", false),
        (
            TeamsPipelineError::Graph {
                status: 503,
                message: "fixture".into(),
            },
            "Graph",
            false,
        ),
    ] {
        assert_eq!(error_type_name(&error), name);
        assert_eq!(error.is_retryable(), retryable, "{name}");
    }
}

fn run_exact_test_in_child(name: &str, configure: impl FnOnce(&mut std::process::Command)) {
    let (_, module) = module_path!().split_once("::").unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg(format!("{module}::{name}"))
        .arg("--nocapture");
    configure(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == "running 1 test"),
        "isolated test {name} failed or did not run:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn store_path_uses_explicit_then_environment_then_temporary_home() {
    const MODE: &str = "HERMES_COVERAGE_STORE_PATH_CHILD";
    const KEY: &str = "MSGRAPH_WEBHOOK_STORE_PATH";

    if let Ok(mode) = std::env::var(MODE) {
        let explicit = Path::new("explicit-store.json");
        assert_eq!(resolve_teams_pipeline_store_path(Some(explicit)), explicit);
        if mode == "configured" {
            assert_eq!(env_nonempty(KEY).as_deref(), Some("override/store.json"));
            assert_eq!(
                resolve_teams_pipeline_store_path(Some(Path::new(""))),
                PathBuf::from("override/store.json")
            );
        } else {
            assert_eq!(mode, "blank");
            let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
            assert_eq!(env_nonempty(KEY), None);
            assert_eq!(
                resolve_teams_pipeline_store_path(None),
                home.join(DEFAULT_TEAMS_PIPELINE_STORE_FILENAME)
            );
        }
        return;
    }

    let home = tempfile::tempdir().unwrap();
    for (mode, value) in [
        ("configured", "  override/store.json \t"),
        ("blank", " \t "),
    ] {
        run_exact_test_in_child(
            "store_path_uses_explicit_then_environment_then_temporary_home",
            |command| {
                command
                    .env(MODE, mode)
                    .env(KEY, value)
                    .env("HERMES_HOME", home.path())
                    .env("HOME", home.path())
                    .env("USERPROFILE", home.path());
            },
        );
    }
}

fn meeting_ref(id: &str) -> TeamsMeetingRef {
    TeamsMeetingRef {
        meeting_id: id.into(),
        organizer_user_id: None,
        join_web_url: None,
        calendar_event_id: None,
        thread_id: None,
        tenant_id: None,
        metadata: Map::new(),
    }
}

fn artifact(kind: &str, id: &str, metadata: Value) -> MeetingArtifact {
    serde_json::from_value(json!({
        "artifact_type": kind,
        "artifact_id": id,
        "meeting_id": "meeting-42",
        "metadata": metadata
    }))
    .unwrap()
}

#[test]
fn heuristic_summary_handles_empty_input_limits_and_classification() {
    let empty = parse_summary_json(" \n ").unwrap();
    assert_eq!(
        empty.summary,
        "Transcript unavailable or too sparse for a confident summary."
    );
    assert_eq!(empty.confidence, "low");
    assert!(empty.key_decisions.is_empty());
    assert!(empty.action_items.is_empty());
    assert!(empty.risks.is_empty());
    assert_eq!(
        empty.confidence_notes,
        "Generated with heuristic fallback because no LLM summary response was available."
    );

    let parsed = heuristic_summary(
        " \t- * \n * DECISION: ship Friday\n - Action: notify QA\n \
         \tTODO: update docs\nNext step: deploy\nFollow up: check status\n\
         RISK: delay\nA blocker remains\nWe decided to wait",
    );
    assert_eq!(
        parsed.summary,
        "DECISION: ship Friday Action: notify QA TODO: update docs"
    );
    assert_eq!(
        parsed.key_decisions,
        ["DECISION: ship Friday", "We decided to wait"]
    );
    assert_eq!(
        parsed.action_items,
        [
            "Action: notify QA",
            "TODO: update docs",
            "Next step: deploy",
            "Follow up: check status"
        ]
    );
    assert_eq!(parsed.risks, ["RISK: delay", "A blocker remains"]);
    assert_eq!(parsed.confidence, "low");

    let long_line = "é".repeat(1201);
    let repeated = "Action: record decision and risk\n".repeat(9);
    let capped = heuristic_summary(&format!("{long_line}\n{repeated}"));
    assert_eq!(capped.summary, "é".repeat(1200));
    assert_eq!(capped.action_items.len(), 8);
    assert_eq!(capped.key_decisions.len(), 6);
    assert_eq!(capped.risks.len(), 6);
    assert_eq!(capped.confidence, "medium");
    assert_eq!(heuristic_summary(&"é".repeat(299)).confidence, "low");
    assert_eq!(heuristic_summary(&"é".repeat(300)).confidence, "medium");

    let from_json = parse_summary_json(
        "```json\n{\"summary\":\" Ready \",\"action_items\":[\" Update \", 2, true, null, \" \"]}\n```",
    )
    .unwrap();
    assert_eq!(from_json.summary, "Ready");
    assert_eq!(from_json.action_items, ["Update", "2", "true"]);
    assert_eq!(from_json.confidence, "medium");
}

#[tokio::test]
async fn summarizer_collects_metrics_and_targets_and_renders_markdown() {
    let mut meeting = meeting_ref("meeting-42");
    meeting.metadata.insert(
        "participants".into(),
        json!([
            {"displayName": "Avery", "identity": {"user": {"displayName": "Ignored"}}},
            {"identity": {"user": {"displayName": "Sam"}}},
            {"displayName": 42}
        ]),
    );
    let artifacts = [
        artifact("transcript", "t1", json!({"metrics": {"ignore": true}})),
        artifact(
            "call_record",
            "missing",
            json!({"metrics": "not an object"}),
        ),
        artifact(
            "call_record",
            "c1",
            json!({"metrics": {"duration": 9, "jitter": 3}}),
        ),
        artifact(
            "call_record",
            "c2",
            json!({"metrics": {"duration": 20, "artifact_count": 99}}),
        ),
    ];
    let config = TeamsPipelineConfig::from_value(Some(json!({
        "notion": {"database_id": " notes "},
        "linear": {"team_id": 42},
        "teams_delivery": {"channel_id": " \t ", "chat_id": "chat"}
    })));
    let mut payload = HeuristicTeamsSummarizer
        .summarize(
            &meeting,
            "Decision: ship\nAction: verify\nRisk: rain",
            &artifacts,
            &config,
        )
        .await
        .unwrap();
    assert_eq!(payload.title.as_deref(), Some("Meeting meeting-42"));
    assert_eq!(payload.participants, ["Avery", "Sam"]);
    assert_eq!(payload.notion_target.as_deref(), Some("notes"));
    assert_eq!(payload.linear_target.as_deref(), Some("42"));
    assert_eq!(payload.teams_target.as_deref(), Some("chat"));
    assert_eq!(
        Value::Object(payload.call_metrics.clone()),
        json!({"duration": 20, "jitter": 3, "artifact_count": 4})
    );
    assert_eq!(
        render_summary_markdown(&payload),
        "# Meeting meeting-42\n\n## Summary\nDecision: ship Action: verify Risk: rain\n\n\
         ## Key Decisions\n- Decision: ship\n\n## Action Items\n- Action: verify\n\n\
         ## Risks\n- Risk: rain\n\nConfidence: low\n\
         Generated with heuristic fallback because no LLM summary response was available."
    );

    payload.title = None;
    payload.summary = None;
    payload.key_decisions.clear();
    payload.action_items.clear();
    payload.risks.clear();
    payload.confidence = None;
    payload.confidence_notes = Some(" \t ".into());
    assert_eq!(
        render_summary_markdown(&payload),
        "# Meeting meeting-42\n\n## Summary\nNo summary available.\n\n\
         ## Key Decisions\n- None\n\n## Action Items\n- None\n\n\
         ## Risks\n- None\n\nConfidence: unknown"
    );

    let text = "é".repeat(18_001);
    assert_eq!(
        build_summary_prompt(&meeting, &text, &[]),
        format!(
            "Meeting ID: meeting-42\nTitle: Unknown\nArtifacts:\n- none\n\nTranscript:\n{}",
            "é".repeat(18_000)
        )
    );
}

#[tokio::test]
async fn audio_preparation_passes_through_audio_and_disabled_extraction() {
    for (name, extract) in [
        ("recording.WaV", true),
        ("recording.webm", true),
        ("recording.mp4", false),
    ] {
        let input = Path::new(name);
        assert_eq!(prepare_audio_path(input, extract).await.unwrap(), input);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn ffmpeg_outcomes_use_only_a_temporary_executable() {
    use std::os::unix::fs::PermissionsExt;

    const MODE: &str = "HERMES_COVERAGE_FFMPEG_CHILD";
    if let Ok(mode) = std::env::var(MODE) {
        let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
        let input = home.join("clip.mp4");
        std::fs::write(&input, b"fixture video").unwrap();
        if mode == "missing" {
            let error = prepare_audio_path(&input, true).await.unwrap_err();
            assert!(matches!(
                error,
                TeamsPipelineError::Retryable(message)
                    if message.starts_with("Recording fallback requires ffmpeg for audio extraction, but ffmpeg failed to start:")
            ));
            assert!(!input.with_extension("wav").exists());
            return;
        }

        assert_eq!(mode, "stub");
        let output = prepare_audio_path(&input, true).await.unwrap();
        assert_eq!(output, input.with_extension("wav"));
        assert_eq!(std::fs::read(output).unwrap(), b"fixture audio");

        assert_eq!(hermes_home(), home);
        let recording = artifact("recording", "rejected", json!({}));
        let error = TranscriptionToolTeamsTranscriber
            .transcribe_recording(
                &recording,
                b"rejected fixture",
                &TeamsPipelineConfig::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            TeamsPipelineError::Retryable(message)
                if message == "ffmpeg audio extraction failed: rejected fixture"
        ));

        let root = home.join("tmp").join("teams_pipeline");
        let runs = std::fs::read_dir(root)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            std::fs::read(runs[0].path().join("rejected.mp4")).unwrap(),
            b"rejected fixture"
        );
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    let empty_bin = home.path().join("empty-bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::create_dir(&empty_bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\n\
         if [ \"$#\" = 4 ] && [ \"$1\" = -y ] && [ \"$2\" = -i ]; then\n\
           case \"$3\" in\n\
             */clip.mp4) printf 'fixture audio' > \"$4\" ;;\n\
             */rejected.mp4) printf '  rejected fixture\\n' >&2; false ;;\n\
             *) false ;;\n\
           esac\n\
         else\n\
           false\n\
         fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o700)).unwrap();

    for (mode, path) in [("missing", &empty_bin), ("stub", &bin)] {
        run_exact_test_in_child(
            "ffmpeg_outcomes_use_only_a_temporary_executable",
            |command| {
                command
                    .env(MODE, mode)
                    .env("PATH", path)
                    .env("HERMES_HOME", home.path())
                    .env("HOME", home.path());
            },
        );
    }
}

enum TranscriptFixture {
    Present,
    Missing,
    MustNotFetch,
}

#[async_trait]
impl TeamsGraphBackend for TranscriptFixture {
    async fn resolve_meeting_reference(
        &self,
        meeting_id: Option<&str>,
        join_web_url: Option<&str>,
        tenant_id: Option<&str>,
    ) -> TeamsPipelineResult<TeamsMeetingRef> {
        assert_eq!(meeting_id, Some("meeting-42"));
        let mut meeting = meeting_ref("meeting-42");
        meeting.join_web_url = join_web_url.map(str::to_owned);
        meeting.tenant_id = tenant_id.map(str::to_owned);
        Ok(meeting)
    }

    async fn fetch_preferred_transcript_text(
        &self,
        meeting: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Option<(MeetingArtifact, String)>> {
        assert_eq!(meeting.meeting_id, "meeting-42");
        match self {
            Self::Present => Ok(Some((
                artifact("transcript", "transcript-42", json!({})),
                "Decision: proceed with the release".into(),
            ))),
            Self::Missing => Ok(None),
            Self::MustNotFetch => panic!("transcript preference is disabled"),
        }
    }

    async fn list_recording_artifacts(
        &self,
        _: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Vec<MeetingArtifact>> {
        panic!("the fixture must not request a recording fallback")
    }

    async fn download_recording_artifact(
        &self,
        _: &TeamsMeetingRef,
        _: &MeetingArtifact,
    ) -> TeamsPipelineResult<Vec<u8>> {
        panic!("the fixture must not download recordings")
    }

    async fn enrich_meeting_with_call_record(
        &self,
        _: &TeamsMeetingRef,
        call_record_id: Option<&str>,
    ) -> TeamsPipelineResult<Option<MeetingArtifact>> {
        assert_eq!(call_record_id, Some("call-42"));
        Ok(Some(artifact(
            "call_record",
            "call-42",
            json!({"metrics": {"duration": 9}}),
        )))
    }
}

struct FixtureSink;

#[async_trait]
impl TeamsSinkWriter for FixtureSink {
    async fn write_summary(
        &self,
        sink: TeamsSinkKind,
        payload: &TeamsMeetingSummaryPayload,
        config: &Value,
        existing_record: Option<Value>,
    ) -> TeamsPipelineResult<Value> {
        assert_eq!(payload.meeting_ref.meeting_id, "meeting-42");
        Ok(json!({
            "destination": sink.as_str(),
            "label": config["label"],
            "previous": existing_record.as_ref().and_then(|record| record.get("entry_id")),
            "entry_id": "new-entry"
        }))
    }
}

fn fixture_pipeline(
    dir: &Path,
    transcript: TranscriptFixture,
    config: TeamsPipelineConfig,
) -> TeamsMeetingPipeline {
    let store = TeamsPipelineStore::new(dir.join("pipeline.json"))
        .expect("create a pipeline store in the temporary directory");
    TeamsMeetingPipeline::new(Arc::new(transcript), Arc::new(store), config)
}

fn notification() -> Value {
    json!({
        "changeType": "created",
        "resource": "/users/user/onlineMeetings/meeting-42/transcripts/t1",
        "resourceData": {
            "joinWebUrl": " https://example.invalid/join ",
            "callRecordId": " call-42 ",
            "tenantId": "tenant-42"
        }
    })
}

#[tokio::test]
async fn notification_deduplication_preserves_a_completed_job_and_its_sink_records() {
    let dir = tempfile::tempdir().unwrap();
    let config = TeamsPipelineConfig::from_value(Some(json!({
        "transcript_min_chars": 10,
        "notion": {"enabled": true, "label": "notes"},
        "linear": {"enabled": "on", "label": "issues"},
        "teams_delivery": {"enabled": 1, "label": "chat"}
    })));
    let pipeline = fixture_pipeline(dir.path(), TranscriptFixture::Present, config.clone())
        .with_notion_writer(Arc::new(FixtureSink))
        .with_linear_writer(Arc::new(FixtureSink))
        .with_teams_sender(Arc::new(FixtureSink));
    assert_eq!(pipeline.config(), &config);
    let store = pipeline.store();
    store
        .upsert_sink_record("linear:meeting-42", json!({"entry_id": "old-issue"}))
        .unwrap();

    let notification = notification();
    let received = pipeline
        .create_job_from_notification(notification.clone())
        .unwrap();
    assert_eq!(received.status, "received");
    let reference = received.meeting_ref.as_ref().unwrap();
    assert_eq!(reference.meeting_id, "meeting-42");
    assert_eq!(
        reference.metadata["join_web_url"],
        "https://example.invalid/join"
    );
    assert_eq!(reference.metadata["call_record_id"], "call-42");
    assert!(pipeline
        .find_job_by_dedupe_key("unrelated-fixture-event")
        .unwrap()
        .is_none());

    let completed = pipeline
        .run_notification(notification.clone())
        .await
        .unwrap();
    assert_eq!(completed.job_id, received.job_id);
    assert_eq!(completed.status, "completed");
    assert_eq!(
        completed.selected_artifact_strategy.as_deref(),
        Some("transcript_first")
    );
    let payload = completed.summary_payload.unwrap();
    assert_eq!(
        payload.meeting_ref.join_web_url.as_deref(),
        Some("https://example.invalid/join")
    );
    assert_eq!(payload.meeting_ref.tenant_id.as_deref(), Some("tenant-42"));
    assert_eq!(payload.call_metrics.get("duration"), Some(&json!(9)));

    let replay = pipeline.run_notification(notification).await.unwrap();
    assert_eq!(replay.job_id, received.job_id);
    assert_eq!(replay.status, "completed");
    assert_eq!(store.list_jobs().unwrap().len(), 1);
    for (sink, label, previous) in [
        ("notion", "notes", Value::Null),
        ("linear", "issues", json!("old-issue")),
        ("teams", "chat", Value::Null),
    ] {
        let record = store
            .get_sink_record(&format!("{sink}:meeting-42"))
            .unwrap()
            .unwrap();
        assert_eq!(record["destination"], sink);
        assert_eq!(record["label"], label);
        assert_eq!(record["entry_id"], "new-entry");
        assert_eq!(record["previous"], previous);
    }
}

#[tokio::test]
async fn invalid_jobs_return_an_error_or_persist_a_nonretryable_failure() {
    let dir = tempfile::tempdir().unwrap();
    let pipeline = fixture_pipeline(
        dir.path(),
        TranscriptFixture::MustNotFetch,
        TeamsPipelineConfig::default(),
    );
    assert!(matches!(
        pipeline.run_job("not-stored").await,
        Err(TeamsPipelineError::Invalid(message))
            if message == "Unknown Teams pipeline job: not-stored"
    ));

    let job = TeamsMeetingPipelineJob {
        job_id: "without-meeting".into(),
        event_id: "fixture-event".into(),
        source_event_type: "fixture".into(),
        dedupe_key: "fixture-event".into(),
        status: "received".into(),
        retry_count: 0,
        created_at: None,
        updated_at: None,
        meeting_ref: None,
        selected_artifact_strategy: None,
        summary_payload: None,
        error_info: Map::new(),
    };
    let failed = pipeline.run_job_record(job).await.unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.error_info["type"], "Invalid");
    assert_eq!(
        failed.error_info["message"],
        "Teams pipeline invalid data: Job without-meeting has no meeting_ref."
    );
    let stored = pipeline.store().get_job(&failed.job_id).unwrap().unwrap();
    assert_eq!(stored["status"], "failed");
    assert_eq!(stored["error_info"]["type"], "Invalid");
}

#[tokio::test]
async fn missing_transcripts_schedule_retries_without_downloading_recordings() {
    for (transcript, config, expected) in [
        (
            TranscriptFixture::Missing,
            TeamsPipelineConfig {
                transcript_required: true,
                ..TeamsPipelineConfig::default()
            },
            "Teams pipeline retryable error: Transcript unavailable for meeting meeting-42.",
        ),
        (
            TranscriptFixture::MustNotFetch,
            TeamsPipelineConfig {
                transcript_preferred: false,
                transcription_fallback: false,
                ..TeamsPipelineConfig::default()
            },
            "Teams pipeline artifact unavailable: No transcript available and transcription fallback disabled for meeting-42.",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let pipeline = fixture_pipeline(dir.path(), transcript, config);
        let retry = pipeline.run_notification(notification()).await.unwrap();
        assert_eq!(retry.status, "retry_scheduled");
        assert_eq!(retry.error_info["message"], expected);
        assert_eq!(retry.error_info["retryable"], true);
        assert!(retry.selected_artifact_strategy.is_none());
        assert!(retry.summary_payload.is_none());
        let stored = pipeline.store().get_job(&retry.job_id).unwrap().unwrap();
        assert_eq!(stored["status"], "retry_scheduled");
        assert_eq!(stored["error_info"]["message"], expected);
    }
}
