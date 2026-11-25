use super::*;
use serde_json::json;
use wiremock::matchers::{body_json, body_string, header, path_regex, query_param};

const ROOM: &str = "!room:fixture.invalid";
const TOKEN: &str = "fixture-matrix-room-key";
const MEMBERS: &str = "/_matrix/client/v3/rooms/!room:fixture.invalid/members";
const ROOM_NAME: &str = "/_matrix/client/v3/rooms/!room:fixture.invalid/state/m.room.name/";
const POWER_LEVELS: &str =
    "/_matrix/client/v3/rooms/!room:fixture.invalid/state/m.room.power_levels/";
const LEAVE: &str = "/_matrix/client/v3/rooms/!room:fixture.invalid/leave";
const UPLOAD: &str = "/_matrix/media/v3/upload";
const SEND: &str = r"^/_matrix/client/v3/rooms/!room:fixture\.invalid/send/m\.room\.message/[^/]+$";
const SYNC: &str = "/_matrix/client/v3/sync";

fn room_adapter(server: &MockServer) -> MatrixAdapter {
    let mut adapter = MatrixAdapter::new(MatrixConfig {
        homeserver_url: server.uri(),
        user_id: "@bot:fixture.invalid".into(),
        access_token: TOKEN.into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    })
    .expect("loopback adapter");
    adapter.client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("loopback-only client");
    adapter
}

#[tokio::test]
async fn room_files_upload_original_bytes_and_send_the_matching_media_type() {
    let server = MockServer::start().await;
    let adapter = room_adapter(&server);
    let dir = tempfile::tempdir().unwrap();
    let authorization = format!("Bearer {TOKEN}");
    let cases = [
        (
            "a photo.png",
            "image/png",
            "m.image",
            Some("Fixture caption"),
        ),
        ("a clip.mp4", "video/mp4", "m.video", None),
        ("a note.ogg", "audio/ogg", "m.audio", None),
        ("a file.txt", "text/plain", "m.file", None),
    ];
    for (index, (name, mime, message_type, caption)) in cases.into_iter().enumerate() {
        let data = format!("fixture media bytes for {name}");
        let source = dir.path().join(name);
        std::fs::write(&source, &data).unwrap();
        let mxc = format!("mxc://fixture.invalid/file-{index}");
        Mock::given(method("POST"))
            .and(path(UPLOAD))
            .and(query_param("filename", name))
            .and(header("authorization", authorization.clone()))
            .and(header("content-type", mime))
            .and(body_string(&data))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"content_uri": mxc})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path_regex(SEND))
            .and(header("authorization", authorization.clone()))
            .and(body_json(json!({
                "msgtype": message_type, "body":caption.unwrap_or(name), "url":mxc,
                "info":{"mimetype":mime,"size":data.len()}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id": "$fixture"})))
            .expect(1)
            .mount(&server)
            .await;
        adapter
            .send_file(ROOM, source.to_str().unwrap(), caption)
            .await
            .unwrap();
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        cases.len() * 2
    );
}

#[tokio::test]
async fn rejected_uploads_and_bad_replies_never_send_a_room_event() {
    let server = MockServer::start().await;
    let adapter = room_adapter(&server);
    let dir = tempfile::tempdir().unwrap();
    for (name, status, body, expected) in [
        (
            "rejected.png",
            413,
            "fixture too large",
            "fixture too large",
        ),
        ("bad-json.png", 200, "{", "Matrix upload parse"),
        ("missing-uri.png", 200, "{}", "No content_uri"),
    ] {
        let source = dir.path().join(name);
        std::fs::write(&source, b"fixture").unwrap();
        Mock::given(method("POST"))
            .and(path(UPLOAD))
            .and(query_param("filename", name))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            adapter.send_file(ROOM, source.to_str().unwrap(), None).await,
            Err(GatewayError::SendFailed(detail)) if detail.contains(expected)
        ));
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.url.path() == UPLOAD));
}

#[tokio::test]
async fn sync_delivers_messages_and_stops_when_the_continuation_is_rejected() {
    let server = MockServer::start().await;
    let mut adapter = room_adapter(&server);
    adapter.native_decrypt = None;
    adapter.decrypt_ffi = None;
    let authorization = format!("Bearer {TOKEN}");
    Mock::given(method("GET"))
        .and(path(SYNC))
        .and(header("authorization", authorization.clone()))
        .and(query_param("since", "fixture-next"))
        .respond_with(ResponseTemplate::new(403).set_body_string("fixture sync access denied"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(SYNC))
        .and(header("authorization", authorization))
        .and(|request: &wiremock::Request| !request.url.query_pairs().any(|(key, _)| key == "since"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "next_batch":"fixture-next",
            "rooms":{"join":{(ROOM):{"timeline":{"events":[
                {"type":"m.room.message","event_id":"$sync-fixture","sender":"@alice:fixture.invalid",
                 "content":{"msgtype":"m.text","body":"fixture message"}}
            ]}}}}
        })))
        .expect(1)
        .mount(&server)
        .await;
    adapter.start().await.unwrap();
    let mut messages = Vec::new();
    let stopped = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        adapter.sync_loop(|batch| messages.extend(batch)),
    )
    .await
    .expect("Matrix sync fixture timed out");
    assert!(
        matches!(stopped, Err(GatewayError::Auth(detail)) if detail.contains("fixture sync access denied"))
    );
    assert!(!adapter.is_running());
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "fixture message");
    assert_eq!(messages[0].event_id, "$sync-fixture");
}

#[tokio::test]
async fn room_management_preserves_membership_and_names_across_loopback_requests() {
    let server = MockServer::start().await;
    let adapter = room_adapter(&server);
    let authorization = format!("Bearer {TOKEN}");
    let requested = "!requested:fixture.invalid";
    for (endpoint, body) in [
        (
            "/_matrix/client/v3/join/!requested:fixture.invalid",
            json!({"room_id": ROOM}),
        ),
        (LEAVE, json!({})),
    ] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .and(header("authorization", authorization.clone()))
            .and(body_json(json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
    }
    let powers = json!({"users": {"@alice:fixture.invalid": 100}, "users_default": 0});
    for (endpoint, body, calls) in [
        (ROOM_NAME, json!({"name": " Fixture room "}), 1),
        (POWER_LEVELS, powers.clone(), 1),
        (
            MEMBERS,
            json!({"chunk": [
                {"state_key": "@alice:fixture.invalid", "content": {"membership": "join", "displayname": "Alice"}},
                {"state_key": "@bob:fixture.invalid", "content": {"membership": "join"}},
                {"state_key": "@bot:fixture.invalid", "content": {"membership": "join"}},
                {"state_key": "@left:fixture.invalid", "content": {}}
            ]}),
            2,
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .and(header("authorization", authorization.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(calls)
            .mount(&server)
            .await;
    }
    assert_eq!(adapter.join_room(requested).await.unwrap(), ROOM);
    let members = adapter.get_room_members(ROOM).await.unwrap();
    let members: Vec<_> = members
        .iter()
        .map(|member| {
            (
                member.user_id.as_str(),
                member.membership.as_str(),
                member.display_name.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        members,
        [
            ("@alice:fixture.invalid", "join", Some("Alice")),
            ("@bob:fixture.invalid", "join", None),
            ("@bot:fixture.invalid", "join", None),
            ("@left:fixture.invalid", "leave", None),
        ]
    );
    let identity = adapter.resolve_room_identity(ROOM, true).await;
    assert_eq!(identity.room_id, ROOM);
    assert_eq!(identity.room_name.as_deref(), Some(" Fixture room "));
    assert_eq!(identity.display_name, "Fixture room");
    assert_eq!(identity.server_name.as_deref(), Some("fixture.invalid"));
    assert_eq!(identity.joined_member_count, Some(3));
    assert_eq!(identity.chat_type, "room");
    assert!(identity.is_direct_account_data && identity.direct_conflict);
    assert_eq!(adapter.get_room_power_levels(ROOM).await.unwrap(), powers);
    adapter.leave_room(ROOM).await.unwrap();
}

#[tokio::test]
async fn room_management_reports_failed_requests_and_uses_available_metadata() {
    let server = MockServer::start().await;
    let adapter = room_adapter(&server);
    for (verb, endpoint, status, body, calls) in [
        (
            "POST",
            "/_matrix/client/v3/join/!room:fixture.invalid",
            403,
            "fixture join denied",
            1,
        ),
        ("POST", LEAVE, 503, "fixture leave unavailable", 1),
        ("GET", ROOM_NAME, 503, "fixture name unavailable", 2),
        ("GET", MEMBERS, 200, "invalid fixture members", 1),
        ("GET", POWER_LEVELS, 200, "invalid fixture power levels", 1),
        (
            "GET",
            "/_matrix/client/v3/rooms/!nameless:fixture.invalid/state/m.room.name/",
            404,
            "",
            1,
        ),
        (
            "GET",
            "/_matrix/client/v3/rooms/!partial:fixture.invalid/state/m.room.name/",
            200,
            r#"{"name":"Known room"}"#,
            1,
        ),
        (
            "GET",
            "/_matrix/client/v3/rooms/!partial:fixture.invalid/members",
            503,
            "fixture members unavailable",
            1,
        ),
    ] {
        Mock::given(method(verb))
            .and(path(endpoint))
            .and(header("authorization", format!("Bearer {TOKEN}")))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .expect(calls)
            .mount(&server)
            .await;
    }
    let join = adapter.join_room(ROOM).await.unwrap_err().to_string();
    assert!(
        join.contains("Matrix join room error: fixture join denied"),
        "{join}"
    );
    let leave = adapter.leave_room(ROOM).await.unwrap_err().to_string();
    assert!(
        leave.contains("Matrix leave room error: fixture leave unavailable"),
        "{leave}"
    );
    let name = adapter.get_room_name(ROOM).await.unwrap_err().to_string();
    assert!(
        name.contains("Matrix room name error: fixture name unavailable"),
        "{name}"
    );
    let identity = adapter.resolve_room_identity(ROOM, true).await;
    assert_eq!(identity.display_name, ROOM);
    assert_eq!(identity.room_name, None);
    assert_eq!(identity.joined_member_count, None);
    assert_eq!(identity.chat_type, "dm");
    assert!(!identity.direct_conflict);

    let partial = adapter
        .resolve_room_identity("!partial:fixture.invalid", true)
        .await;
    assert_eq!(partial.room_name.as_deref(), Some("Known room"));
    assert_eq!(partial.display_name, "Known room");
    assert_eq!(partial.joined_member_count, None);
    assert_eq!(partial.chat_type, "room");
    assert!(partial.direct_conflict);

    let power = adapter
        .get_room_power_levels(ROOM)
        .await
        .unwrap_err()
        .to_string();
    assert!(power.contains("Matrix power levels parse"), "{power}");
    assert_eq!(
        adapter
            .get_room_name("!nameless:fixture.invalid")
            .await
            .unwrap(),
        None
    );
}
