#[path = "support/isolated_hermes.rs"]
mod support;

use std::time::Duration;

use serde_json::{json, Value};
use support::isolated_hermes;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn stdout(command: &mut tokio::process::Command) -> String {
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("Teams pipeline command timed out")
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn teams_pipeline_list_filters_sorts_and_limits_local_jobs() {
    let sandbox = tempfile::tempdir().unwrap();
    let store_path = sandbox.path().join("teams.json");
    let mut jobs = serde_json::Map::new();
    for (id, status, updated_at) in [
        ("earlier", "done", "2026-11-23T01:00:00Z"),
        ("later", "done", "2026-11-23T02:00:00Z"),
        ("pending", "queued", "2026-11-23T03:00:00Z"),
    ] {
        jobs.insert(
            id.to_owned(),
            json!({
                "job_id": id,
                "status": status,
                "updated_at": updated_at,
                "meeting_ref": {"meeting_id": format!("meeting-{id}")}
            }),
        );
    }
    let expected_job = jobs["earlier"].clone();
    std::fs::write(&store_path, json!({"jobs": jobs}).to_string()).unwrap();
    let args = [
        "teams-pipeline",
        "list",
        "--status",
        " DONE ",
        "--store-path",
    ];
    let listed = stdout(
        isolated_hermes(sandbox.path())
            .args(args)
            .arg(&store_path)
            .args(["--limit", "1"]),
    )
    .await;
    assert!(listed.contains("1 Teams pipeline job(s):"));
    assert!(listed.contains("later  done  meeting=meeting-later"));
    assert!(!listed.contains("earlier"));
    assert!(!listed.contains("pending"));

    let listed = stdout(
        isolated_hermes(sandbox.path())
            .args(args)
            .arg(&store_path)
            .args(["--limit", "0"]),
    )
    .await;
    assert!(listed.contains("2 Teams pipeline job(s):"));
    let later = listed.find("meeting=meeting-later").unwrap();
    let earlier = listed.find("meeting=meeting-earlier").unwrap();
    assert!(later < earlier);
    assert!(!listed.contains("pending"));

    let shown = stdout(
        isolated_hermes(sandbox.path())
            .args(["teams-pipeline", "show", "earlier", "--store-path"])
            .arg(&store_path),
    )
    .await;
    let shown: Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(shown, expected_job);
}

#[tokio::test]
async fn teams_pipeline_fetch_returns_a_bounded_preview_and_artifacts() {
    let sandbox = tempfile::tempdir().unwrap();
    let graph = MockServer::start().await;
    let meeting = "/v1.0/communications/onlineMeetings/meeting-fixture";
    let fixture_token = "hermes-teams-cli-fixture";
    let expected_authorization = format!("Bearer {fixture_token}");
    let transcript = format!("WEBVTT\n\n{}tail", "é".repeat(600));
    for (request_path, reply) in [
        (
            meeting.to_owned(),
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "meeting-fixture",
                "subject": "Fixture meeting",
                "tenantId": "tenant-from-graph"
            })),
        ),
        (
            format!("{meeting}/transcripts"),
            ResponseTemplate::new(200).set_body_json(json!({
                "value": [
                    {"id": "pending-transcript", "status": "processing"},
                    {"id": "completed-transcript", "status": "completed"}
                ]
            })),
        ),
        (
            format!("{meeting}/transcripts/completed-transcript/content"),
            ResponseTemplate::new(200).set_body_string(transcript.clone()),
        ),
        (
            format!("{meeting}/recordings"),
            ResponseTemplate::new(200)
                .set_body_json(json!({"value": [{"id": "recording-fixture"}]})),
        ),
        (
            "/v1.0/communications/callRecords/call-fixture".into(),
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "call-fixture",
                "participants": [{"id": "participant-fixture"}]
            })),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(request_path))
            .and(header("authorization", expected_authorization.as_str()))
            .respond_with(reply)
            .expect(1)
            .mount(&graph)
            .await;
    }
    let fetched = stdout(
        isolated_hermes(sandbox.path())
            .env("MSGRAPH_BASE_URL", format!("{}/v1.0", graph.uri()))
            .env("MSGRAPH_ACCESS_TOKEN", fixture_token)
            .args([
                "teams-pipeline",
                "fetch",
                "--meeting-id",
                "meeting-fixture",
                "--tenant-id",
                "tenant-from-cli",
                "--call-record-id",
                "call-fixture",
            ]),
    )
    .await;
    assert!(!fetched.contains(fixture_token));
    let fetched: Value = serde_json::from_str(&fetched).unwrap();
    assert_eq!(fetched["meeting_ref"]["meeting_id"], "meeting-fixture");
    assert_eq!(fetched["meeting_ref"]["tenant_id"], "tenant-from-cli");
    assert_eq!(
        fetched["transcript"]["artifact"]["artifact_id"],
        "completed-transcript"
    );
    assert_eq!(
        fetched["transcript"]["transcript_chars"],
        transcript.chars().count()
    );
    let preview: String = transcript.chars().take(500).collect();
    assert_eq!(fetched["transcript"]["transcript_preview"], preview);
    assert_eq!(fetched["recordings"][0]["artifact_id"], "recording-fixture");
    assert_eq!(fetched["call_record"]["artifact_id"], "call-fixture");
    assert_eq!(
        fetched["call_record"]["metadata"]["metrics"]["participant_count"],
        1
    );
    graph.verify().await;
    assert_eq!(graph.received_requests().await.unwrap().len(), 5);
}
