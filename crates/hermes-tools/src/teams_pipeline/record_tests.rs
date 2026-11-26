use super::*;

#[test]
fn records_accept_provider_field_names_and_require_subscription_details() {
    let payload = json!({
        "id": "fixture-subscription",
        "resource": "communications/onlineMeetings",
        "changeType": "created",
        "notificationUrl": "https://fixture.invalid/notifications",
        "expirationDateTime": "2026-06-01T16:00:00Z"
    });
    let subscription = GraphSubscription::from_value(payload.clone()).unwrap();
    assert_eq!(subscription.subscription_id, "fixture-subscription");
    assert_eq!(subscription.change_type, "created");

    for (field, expected) in [
        ("id", "subscription_id"),
        ("resource", "resource"),
        ("changeType", "change_type"),
        ("notificationUrl", "notification_url"),
        ("expirationDateTime", "expiration_datetime"),
    ] {
        let mut incomplete = payload.clone();
        incomplete[field] = json!(" ");
        assert!(matches!(
            GraphSubscription::from_value(incomplete),
            Err(TeamsPipelineError::Invalid(message))
                if message == format!("GraphSubscription.{expected} is required.")
        ));
    }
}

#[test]
fn meeting_records_validate_ids_and_keep_provider_artifact_metadata() {
    let meeting = TeamsMeetingRef::from_value(json!({
        "id": "fixture-meeting",
        "threadId": "fixture-thread"
    }))
    .unwrap();
    assert_eq!(meeting.meeting_id, "fixture-meeting");
    assert_eq!(meeting.thread_id.as_deref(), Some("fixture-thread"));
    assert!(matches!(
        TeamsMeetingRef::from_value(json!({"id": " "})),
        Err(TeamsPipelineError::Invalid(message))
            if message == "TeamsMeetingRef.meeting_id is required."
    ));

    let artifact = MeetingArtifact::from_value(json!({
        "artifactType": "transcript",
        "id": "fixture-artifact",
        "displayName": "Park survey discussion"
    }))
    .unwrap();
    assert_eq!(artifact.artifact_id, "fixture-artifact");
    assert_eq!(
        artifact.display_name.as_deref(),
        Some("Park survey discussion")
    );
    assert!(matches!(
        MeetingArtifact::from_value(json!({"artifactType": "notes", "id": "notes"})),
        Err(TeamsPipelineError::Invalid(message)) if message.contains("artifact_type")
    ));
    assert!(matches!(
        MeetingArtifact::from_value(json!({"artifactType": "recording"})),
        Err(TeamsPipelineError::Invalid(message)) if message.contains("artifact_id")
    ));

    let payload = TeamsMeetingSummaryPayload::from_value(json!({
        "meeting_ref": {"id": "fixture-meeting"},
        "sourceArtifacts": [artifact],
        "actionItems": ["Check the survey dates"]
    }))
    .unwrap();
    assert_eq!(payload.meeting_ref.meeting_id, "fixture-meeting");
    assert_eq!(payload.source_artifacts[0].artifact_type, "transcript");
    assert_eq!(payload.action_items, ["Check the survey dates"]);
    assert!(matches!(
        TeamsMeetingSummaryPayload::from_value(json!({"title": "Survey"})),
        Err(TeamsPipelineError::Json(_))
    ));
}

#[test]
fn imported_pipeline_jobs_require_an_identity_source_and_status() {
    let payload = json!({
        "jobId": "fixture-job",
        "eventId": "fixture-event",
        "sourceEventType": "meeting-ended",
        "dedupeKey": "fixture-date",
        "status": "received",
        "retryCount": 2
    });
    let job = TeamsMeetingPipelineJob::from_value(payload.clone()).unwrap();
    assert_eq!(job.job_id, "fixture-job");
    assert_eq!(job.retry_count, 2);

    for (field, expected) in [
        ("jobId", "job_id"),
        ("eventId", "event_id"),
        ("sourceEventType", "source_event_type"),
        ("dedupeKey", "dedupe_key"),
        ("status", "status"),
    ] {
        let mut incomplete = payload.clone();
        incomplete.as_object_mut().unwrap().remove(field);
        assert!(matches!(
            TeamsMeetingPipelineJob::from_value(incomplete),
            Err(TeamsPipelineError::Invalid(message))
                if message == format!("TeamsMeetingPipelineJob.{expected} is required.")
        ));
    }
}

#[test]
fn reloaded_store_preserves_receipts_and_subscription_removal() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("meeting-records.json");
    std::fs::write(&path, b" \n").unwrap();
    {
        let store = TeamsPipelineStore::new(&path).unwrap();
        assert!(!store.has_notification_receipt("fixture-event").unwrap());
        let original = store
            .upsert_subscription("fixture-subscription", json!({"status": "ready"}))
            .unwrap();
        let updated = store
            .upsert_subscription("fixture-subscription", json!({"status": "retired"}))
            .unwrap();
        assert_eq!(updated["created_at"], original["created_at"]);
        assert_eq!(updated["status"], "retired");

        assert!(store
            .record_notification_receipt("fixture-event", None, None)
            .unwrap());
        assert!(!store
            .record_notification_receipt("fixture-event", Some(json!({"duplicate": true})), None)
            .unwrap());
        assert!(store.delete_subscription("fixture-subscription").unwrap());
        assert!(!store.delete_subscription("fixture-subscription").unwrap());
    }
    let store = TeamsPipelineStore::new(&path).unwrap();
    assert!(store.has_notification_receipt("fixture-event").unwrap());
    assert!(store.list_subscriptions().unwrap().is_empty());
    assert!(matches!(
        TeamsPipelineStore::new(temp.path()),
        Err(TeamsPipelineError::Io(_))
    ));
}
