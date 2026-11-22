use super::*;
use futures::FutureExt;
use serde_json::json;
use tokio_tungstenite::tungstenite::{http::StatusCode, Error as WsError};

const INVALID_URL: &str = "http://[invalid host";

fn offline_adapter(reply_to_mode: &str) -> MattermostAdapter {
    // Both HTTP and WebSocket clients reject this URL before opening a socket.
    MattermostAdapter::new(MattermostConfig {
        server_url: INVALID_URL.to_owned(),
        token: "coverage-token".to_owned(),
        team_id: None,
        proxy: AdapterProxyConfig::default(),
        reply_to_mode: reply_to_mode.to_owned(),
    })
    .unwrap()
}

fn assert_send_failure(error: GatewayError, prefix: &str) {
    match error {
        GatewayError::SendFailed(message) => {
            assert!(message.starts_with(prefix), "unexpected message: {message}");
        }
        other => panic!("expected SendFailed, got {other:?}"),
    }
}

#[test]
fn deserialized_config_defaults_to_flat_replies_and_preserves_ws_scheme() {
    let config = serde_json::from_value(json!({
        "server_url": "ws://mattermost.invalid///",
        "token": "coverage-token"
    }))
    .unwrap();
    let adapter = MattermostAdapter::new(config).unwrap();

    assert_eq!(adapter.config().reply_to_mode, "off");
    assert_eq!(adapter.config().token, "coverage-token");
    assert_eq!(adapter.config().team_id, None);
    assert_eq!(
        adapter.websocket_url(),
        "ws://mattermost.invalid/api/v4/websocket"
    );
}

#[test]
fn configured_event_parser_threads_channel_posts_but_ignores_edits() {
    let mut event = MattermostWsEvent {
        event: "posted".to_owned(),
        data: Some(json!({
            "channel_type": "O",
            "post": json!({
                "id": "post-1",
                "channel_id": "channel-1",
                "user_id": "user-1",
                "message": "new post"
            }).to_string()
        })),
        broadcast: None,
        seq: None,
    };
    let threaded = offline_adapter("ThReAd");
    let message = threaded.parse_ws_event_for_config(&event).unwrap();
    assert_eq!(message.message, "new post");
    assert_eq!(message.thread_id.as_deref(), Some("post-1"));

    let flat = offline_adapter("off");
    assert_eq!(
        flat.parse_ws_event_for_config(&event).unwrap().thread_id,
        None
    );

    event.event = "post_edited".to_owned();
    assert!(threaded.parse_ws_event_for_config(&event).is_none());
}

fn map_http_status(status: StatusCode) -> GatewayError {
    let mut error = WsError::Http(Default::default());
    let WsError::Http(response) = &mut error else {
        unreachable!("constructed an HTTP error");
    };
    *response.status_mut() = status;
    MattermostAdapter::map_ws_error(error)
}

#[test]
fn websocket_error_mapping_distinguishes_auth_handshake_and_stream_errors() {
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        let expected = format!("Mattermost WebSocket auth error: {status}");
        assert!(matches!(
            map_http_status(status),
            GatewayError::Auth(message) if message == expected
        ));
    }

    let status = StatusCode::SERVICE_UNAVAILABLE;
    let expected = format!("Mattermost WebSocket handshake error: {status}");
    assert!(matches!(
        map_http_status(status),
        GatewayError::ConnectionFailed(message) if message == expected
    ));

    let closed = WsError::ConnectionClosed;
    let expected = format!("Mattermost WebSocket error: {closed}");
    assert!(matches!(
        MattermostAdapter::map_ws_error(closed),
        GatewayError::ConnectionFailed(message) if message == expected
    ));
}

#[tokio::test]
async fn lifecycle_updates_running_state_and_notifies_stop() {
    let adapter = offline_adapter("off");
    adapter.base.mark_stopped();

    assert_eq!(adapter.platform_name(), "mattermost");
    assert!(!adapter.is_running());

    adapter.start().await.unwrap();
    assert!(adapter.is_running());
    assert!(adapter.stop_signal.notified().now_or_never().is_none());

    adapter.stop().await.unwrap();
    assert!(!adapter.is_running());
    assert_eq!(adapter.stop_signal.notified().now_or_never(), Some(()));

    adapter
        .ws_loop(|_| panic!("a stopped adapter must not deliver events"))
        .now_or_never()
        .expect("a stopped adapter must return without connecting")
        .unwrap();
}

#[tokio::test]
async fn malformed_websocket_url_returns_connection_failure() {
    let adapter = offline_adapter("off");
    let error = adapter
        .ws_connect_and_listen(&mut |_| panic!("an invalid URL cannot deliver an event"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        GatewayError::ConnectionFailed(message)
            if message.starts_with("Mattermost WebSocket error:")
    ));
}

#[tokio::test]
async fn websocket_stop_permit_interrupts_connection_error_backoff() {
    let adapter = offline_adapter("off");
    adapter.start().await.unwrap();

    // Queue the notification without clearing the running flag so the loop
    // attempts the malformed URL and consumes the permit during backoff.
    adapter.stop_signal.notify_one();
    adapter
        .ws_loop(|_| panic!("an invalid URL cannot deliver an event"))
        .now_or_never()
        .expect("a queued stop permit must prevent waiting for the timer")
        .unwrap();

    assert!(adapter.is_running());
    assert!(adapter.stop_signal.notified().now_or_never().is_none());
}

#[tokio::test]
async fn reconnect_helper_retries_after_clean_disconnect_and_empty_backoff() {
    let adapter = offline_adapter("off");
    adapter.start().await.unwrap();

    let mut attempts = 0;
    adapter
        .ws_reconnect_loop_with_backoff(&[], || {
            attempts += 1;
            std::future::ready(match attempts {
                1 => Ok(()),
                2 => Err(GatewayError::ConnectionFailed("transient failure".into())),
                3 => {
                    adapter.base.mark_stopped();
                    Ok(())
                }
                _ => panic!("reconnected after being marked stopped"),
            })
        })
        .await
        .unwrap();

    assert_eq!(attempts, 3);
    assert!(!adapter.is_running());
}

#[tokio::test]
async fn thread_lookup_rejects_unsafe_ids_and_keeps_candidate_on_request_failure() {
    let adapter = offline_adapter("thread");
    assert_eq!(adapter.resolve_root_id("two/segments").await, None);
    assert_eq!(adapter.thread_root_for_send(Some("   ")).await, None);
    assert_eq!(
        adapter
            .thread_root_for_send(Some("  reply-1  "))
            .await
            .as_deref(),
        Some("reply-1")
    );

    let flat = offline_adapter("off");
    assert_eq!(flat.thread_root_for_send(Some("reply-1")).await, None);
}

#[test]
fn post_failure_only_includes_api_status_when_available() {
    let with_status = MattermostPostFailure {
        status: Some(404),
        body: "missing post".to_owned(),
    };
    assert!(matches!(
        with_status.into_gateway_error(),
        GatewayError::SendFailed(message)
            if message == "Mattermost API error (404): missing post"
    ));

    let without_status = MattermostPostFailure {
        status: None,
        body: "request failed".to_owned(),
    };
    assert!(matches!(
        without_status.into_gateway_error(),
        GatewayError::SendFailed(message) if message == "request failed"
    ));
}

#[tokio::test]
async fn text_send_entry_points_propagate_request_failure() {
    let adapter = offline_adapter("thread");
    for error in [
        adapter.send_text("channel-1", "hello").await.unwrap_err(),
        adapter
            .send_message("channel-1", "hello", Some(ParseMode::Plain))
            .await
            .unwrap_err(),
        adapter
            .send_message_threaded("channel-1", "hello", None, Some("reply-1"))
            .await
            .unwrap_err(),
        adapter
            .send_message_with_options("channel-1", "hello", None, SendMessageOptions::default())
            .await
            .unwrap_err(),
    ] {
        assert_send_failure(error, "Mattermost send failed:");
    }
}

#[tokio::test]
async fn profile_and_edit_report_their_own_request_failures() {
    let adapter = offline_adapter("off");
    assert!(matches!(
        adapter.get_me().await.unwrap_err(),
        GatewayError::ConnectionFailed(message)
            if message.starts_with("Mattermost get_me failed:")
    ));

    assert!(matches!(
        adapter
            .edit_message("channel-1", "two/segments", "updated")
            .await
            .unwrap_err(),
        GatewayError::SendFailed(message)
            if message == "Mattermost post id contains unsafe API path segment"
    ));
    assert_send_failure(
        adapter
            .edit_message("channel-1", "  post-1  ", "updated")
            .await
            .unwrap_err(),
        "Mattermost edit failed:",
    );
}

#[tokio::test]
async fn attachment_send_distinguishes_missing_file_from_upload_failure() {
    let adapter = offline_adapter("thread");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("attachment.txt");
    let path_str = path.to_str().unwrap();

    assert_send_failure(
        adapter
            .send_file("channel-1", path_str, None)
            .await
            .unwrap_err(),
        "Failed to read file:",
    );

    tokio::fs::write(&path, b"attachment contents")
        .await
        .unwrap();
    assert_send_failure(
        adapter
            .send_file_with_options(
                "channel-1",
                path_str,
                Some("caption"),
                SendMessageOptions::default(),
            )
            .await
            .unwrap_err(),
        "Mattermost file upload failed:",
    );
}

#[test]
fn image_extension_mapping_normalizes_mime_and_rejects_unknown_types() {
    for (mime, expected) in [
        (" IMAGE/PNG ; charset=binary", Some("png")),
        ("image/gif", Some("gif")),
        ("image/webp", Some("webp")),
        ("image/bmp", Some("bmp")),
        ("image/tiff", Some("tiff")),
        ("image/svg+xml", Some("svg")),
        ("image/heic", Some("heic")),
        ("image/heif", Some("heif")),
        ("image/avif", Some("avif")),
        ("image/unknown", None),
        ("text/plain", None),
    ] {
        assert_eq!(
            image_extension_from_content_type(Some(mime)),
            expected,
            "{mime}"
        );
    }
    assert_eq!(
        remote_image_file_name("/?unused=1#fragment", Some("image/webp")),
        "image.webp"
    );
    assert_eq!(
        remote_image_file_name("/diagram", Some("text/plain")),
        "diagram.png"
    );
}

#[test]
fn image_fallback_omits_an_absent_or_blank_caption() {
    let url = "https://media.invalid/figure.png";
    for caption in [None, Some(" \t\n ")] {
        assert_eq!(image_fallback_text(url, caption), url);
    }
}

#[tokio::test]
async fn failed_image_download_propagates_the_text_fallback_error() {
    let adapter = offline_adapter("off");
    assert_send_failure(
        adapter
            .send_image_url("channel-1", INVALID_URL, None)
            .await
            .unwrap_err(),
        "Mattermost send failed:",
    );
}
