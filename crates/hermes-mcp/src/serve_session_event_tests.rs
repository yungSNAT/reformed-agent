use super::*;

#[tokio::test]
async fn event_wait_filters_already_seen_and_unrelated_messages() {
    let bridge = Arc::new(EventBridge::default());
    let server = HermesMcpServe::new(Arc::new(InMemorySessionStore::default()))
        .with_event_bridge(Arc::clone(&bridge));
    bridge.push("message", "fixture", HashMap::new());
    let after = bridge.current_cursor();
    let wait = server.handle_tool_call(
        "events_wait",
        json!({"after_cursor": after.to_string(), "session_key": "fixture", "timeout_ms": 2000}),
    );
    let send = async {
        bridge.push("message", "other-fixture", HashMap::new());
        tokio::task::yield_now().await;
        server
            .handle_tool_call(
                "session_send",
                json!({"session_key": "fixture", "message": "Hello from the in-memory fixture."}),
            )
            .await
    };
    let (event, sent) = tokio::join!(biased; wait, send);
    assert_eq!(
        sent.unwrap(),
        json!({"sent": true, "session_key": "fixture"})
    );
    let event = event.unwrap();
    assert_eq!(
        event["event"],
        json!({
            "type": "message",
            "cursor": after + 2,
            "session_key": "fixture",
            "role": "user",
            "content": "Hello from the in-memory fixture.",
        })
    );
    let none = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        server.handle_tool_call(
            "events_wait",
            json!({"after_cursor": after + 2, "session_key": "fixture", "timeout_ms": 5}),
        ),
    )
    .await
    .expect("event wait must use the requested deadline")
    .unwrap();
    assert_eq!(none, json!({"event": null, "reason": "timeout"}));
}

#[tokio::test]
async fn approval_tools_emit_one_resolution_per_synthetic_request() {
    let store = Arc::new(ApprovalStore::default());
    let server = HermesMcpServe::new(Arc::new(InMemorySessionStore::default()))
        .with_approval_store(Arc::clone(&store));
    for (id, tool, decision) in [
        ("allow-fixture", "approve", "allow-once"),
        ("deny-fixture", "deny", "deny"),
    ] {
        store.add_pending(PendingApproval {
            id: id.to_string(),
            session_key: "fixture".to_string(),
            command: "fixture action".to_string(),
            description: "In-memory test request".to_string(),
            created_at: "2000-01-01".to_string(),
        });
        assert!(matches!(
            server.handle_tool_call(tool, json!({})).await,
            Err(McpError::InvalidParams(reason)) if reason == "missing id"
        ));
        let result = server
            .handle_tool_call(tool, json!({"id": id}))
            .await
            .unwrap();
        assert_eq!(
            result,
            json!({"resolved": true, "id": id, "decision": decision})
        );
        let replay = server
            .handle_tool_call(tool, json!({"id": id}))
            .await
            .unwrap();
        assert_eq!(
            replay,
            json!({"error": format!("Approval not found: {id}")})
        );
    }
    assert!(store.list_pending().is_empty());
    let events = server
        .handle_tool_call("events_poll", json!({"session_key": "fixture"}))
        .await
        .unwrap();
    assert_eq!(
        events,
        json!({
            "events": [
                {"cursor": 1, "type": "approval_resolved", "session_key": "fixture",
                    "approval_id": "allow-fixture", "decision": "allow-once"},
                {"cursor": 2, "type": "approval_resolved", "session_key": "fixture",
                    "approval_id": "deny-fixture", "decision": "deny"},
            ],
            "next_cursor": 2,
        })
    );
}

#[tokio::test]
async fn session_tools_filter_and_read_the_most_recent_messages() {
    let store = Arc::new(InMemorySessionStore::default());
    for (key, id, platform, label) in [
        ("fixture", "stored", "cli", "Fixture display name"),
        ("unrelated", "other", "other-platform", "Fixture elsewhere"),
        ("archive", "archived", "cli", "Older conversation"),
    ] {
        store.add_session(SessionEntry {
            session_key: key.to_string(),
            session_id: id.to_string(),
            platform: platform.to_string(),
            display_name: label.to_string(),
            updated_at: String::new(),
        });
    }
    for text in ["older fixture", "newest fixture"] {
        store.add_message(
            "stored",
            SessionMessage {
                id: text.to_string(),
                role: "user".to_string(),
                content: text.to_string(),
                timestamp: String::new(),
            },
        );
    }
    let server = HermesMcpServe::new(store);
    let list = server
        .handle_tool_call(
            "session_list",
            json!({"search": "FIXTURE", "platform": "CLI"}),
        )
        .await
        .unwrap();
    assert_eq!(list["count"], 1);
    assert_eq!(list["sessions"][0]["session_key"], "fixture");
    let read = server
        .handle_tool_call(
            "session_read",
            json!({"session_key": "fixture", "limit": "1"}),
        )
        .await
        .unwrap();
    assert_eq!(read["count"], 1);
    assert_eq!(read["messages"][0]["content"], "newest fixture");
    let empty = server
        .handle_tool_call("session_read", json!({"session_key": "unrelated"}))
        .await
        .unwrap();
    assert_eq!(empty["messages"], json!([]));
    assert!(matches!(
        server.handle_tool_call("session_read", json!({"session_key": "missing"})).await,
        Err(McpError::ResourceNotFound(key)) if key == "missing"
    ));
}
