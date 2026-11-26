use super::SpotifyHttpMethod::{Delete, Get, Post, Put};
use super::SpotifyTool::{Albums, Devices, Library, Playback, Playlists, Queue, Search};
use super::*;
use std::sync::Mutex;

struct ExpectedBackend {
    request: Mutex<Option<SpotifyApiRequest>>,
}

#[async_trait]
impl SpotifyBackend for ExpectedBackend {
    async fn call(&self, request: SpotifyApiRequest) -> Result<Value, ToolError> {
        let expected = self
            .request
            .lock()
            .unwrap()
            .take()
            .expect("unexpected Spotify backend call");
        assert_eq!(request, expected);
        Ok(request.empty_response.unwrap_or_else(backend_payload))
    }
}

fn backend_payload() -> Value {
    json!({"backend": "ok"})
}

fn request(
    method: SpotifyHttpMethod,
    path: &str,
    query: &[(&str, &str)],
    body: Option<Value>,
) -> SpotifyApiRequest {
    SpotifyApiRequest {
        method,
        path: path.to_owned(),
        query: query
            .iter()
            .map(|&(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
        body,
        empty_response: None,
    }
}

async fn execute(tool: SpotifyTool, params: Value, expected: SpotifyApiRequest) -> Value {
    let backend = Arc::new(ExpectedBackend {
        request: Mutex::new(Some(expected)),
    });
    let handler = SpotifyHandler::new(tool, backend.clone());
    let output = handler.execute(params).await.unwrap();
    assert!(
        backend.request.lock().unwrap().is_none(),
        "expected a Spotify backend call"
    );
    serde_json::from_str(&output).unwrap()
}

async fn assert_forwarded(tool: SpotifyTool, params: Value, expected: SpotifyApiRequest) {
    assert_eq!(execute(tool, params, expected).await, backend_payload());
}

fn command_output(action: &str) -> Value {
    json!({"success": true, "action": action, "result": backend_payload()})
}

fn assert_invalid_params<T: std::fmt::Debug>(result: Result<T, ToolError>, expected: &str) {
    match result {
        Err(ToolError::InvalidParams(message)) => assert_eq!(message, expected),
        other => panic!("expected InvalidParams({expected:?}), got {other:?}"),
    }
}

#[tokio::test]
async fn currently_playing_describes_the_empty_response() {
    let message =
        "Spotify is not currently playing anything. Start playback in Spotify and try again.";
    let expected = SpotifyApiRequest {
        empty_response: Some(json!({"status_code": 204, "empty": true, "message": message})),
        ..request(
            Get,
            "/me/player/currently-playing",
            &[("market", "GB")],
            None,
        )
    };

    let output = execute(
        Playback,
        json!({"action": " GET_CURRENTLY_PLAYING ", "market": " GB "}),
        expected,
    )
    .await;
    assert_eq!(
        output,
        json!({
            "success": true,
            "action": "get_currently_playing",
            "is_playing": false,
            "status_code": 204,
            "message": message
        })
    );
}

#[test]
fn empty_playback_description_defaults_only_for_recognized_empty_responses() {
    for (payload, action) in [
        (json!(["raw playback"]), "get_currently_playing"),
        (
            json!({"empty": false, "is_playing": true}),
            "get_currently_playing",
        ),
        (json!({"empty": true, "extra": 1}), "other"),
    ] {
        assert_eq!(describe_empty_playback(payload.clone(), action), payload);
    }

    assert_eq!(
        describe_empty_playback(json!({"empty": true}), "get_currently_playing"),
        json!({
            "success": true,
            "action": "get_currently_playing",
            "is_playing": false,
            "status_code": 204,
            "message": "Spotify is not currently playing anything."
        })
    );
}

#[tokio::test]
async fn play_normalizes_context_and_tracks_and_cleans_offset() {
    let output = execute(
        Playback,
        json!({
            "action": "play",
            "device_id": " speaker ",
            "context_uri": "https://open.spotify.com/playlist/p1?si=test",
            "uris": [" t1 ", "spotify:track:t1", "spotify:track:t2"],
            "offset": {"position": 0, "uri": null},
            "position_ms": " 1500 "
        }),
        request(
            Put,
            "/me/player/play",
            &[("device_id", "speaker")],
            Some(json!({
                "context_uri": "spotify:playlist:p1",
                "uris": ["spotify:track:t1", "spotify:track:t2"],
                "offset": {"position": 0},
                "position_ms": 1500
            })),
        ),
    )
    .await;
    assert_eq!(output, command_output("play"));

    let output = execute(
        Playback,
        json!({
            "action": "play",
            "context_uri": "https://open.spotify.com/artist/a1",
            "offset": {"position": null}
        }),
        request(
            Put,
            "/me/player/play",
            &[],
            Some(json!({"context_uri": "spotify:artist:a1"})),
        ),
    )
    .await;
    assert_eq!(output, command_output("play"));
    assert_eq!(infer_context_type("spotify:track:t1"), None);
}

#[tokio::test]
async fn pause_next_and_previous_send_device_commands() {
    for (action, method, path) in [
        ("pause", Put, "/me/player/pause"),
        ("next", Post, "/me/player/next"),
        ("previous", Post, "/me/player/previous"),
    ] {
        let output = execute(
            Playback,
            json!({"action": action, "device_id": " speaker "}),
            request(method, path, &[("device_id", "speaker")], None),
        )
        .await;
        assert_eq!(output, command_output(action));
    }
}

#[tokio::test]
async fn playback_settings_normalize_query_values() {
    for (action, path, key, input, expected) in [
        (
            "seek",
            "/me/player/seek",
            "position_ms",
            json!(" 2500 "),
            "2500",
        ),
        (
            "set_repeat",
            "/me/player/repeat",
            "state",
            json!(" CONTEXT "),
            "context",
        ),
        (
            "set_shuffle",
            "/me/player/shuffle",
            "state",
            json!(" On "),
            "true",
        ),
        (
            "set_volume",
            "/me/player/volume",
            "volume_percent",
            json!(120),
            "100",
        ),
        (
            "set_volume",
            "/me/player/volume",
            "volume_percent",
            json!(-8),
            "0",
        ),
    ] {
        let mut params = json!({"action": action, "device_id": "speaker"});
        params[key] = input;
        let output = execute(
            Playback,
            params,
            request(
                Put,
                path,
                &[(key, expected), ("device_id", "speaker")],
                None,
            ),
        )
        .await;
        assert_eq!(output, command_output(action));
    }
}

#[tokio::test]
async fn recently_played_forwards_only_the_requested_cursor() {
    for (params, limit, cursor) in [
        (
            json!({"action": "recently_played", "after": " 123 ", "limit": 99}),
            "50",
            "after",
        ),
        (
            json!({"action": "recently_played", "after": null, "before": 123}),
            "20",
            "before",
        ),
    ] {
        assert_forwarded(
            Playback,
            params,
            request(
                Get,
                "/me/player/recently-played",
                &[("limit", limit), (cursor, "123")],
                None,
            ),
        )
        .await;
    }
}

#[tokio::test]
async fn devices_list_by_default_and_transfer_to_the_selected_device() {
    assert_forwarded(
        Devices,
        json!({}),
        request(Get, "/me/player/devices", &[], None),
    )
    .await;

    let output = execute(
        Devices,
        json!({"action": "transfer", "device_id": " speaker ", "play": " YES "}),
        request(
            Put,
            "/me/player",
            &[],
            Some(json!({"device_ids": ["speaker"], "play": true})),
        ),
    )
    .await;
    assert_eq!(output, command_output("transfer"));
}

#[tokio::test]
async fn queue_gets_by_default_and_add_returns_the_queued_uri() {
    assert_forwarded(
        Queue,
        json!({}),
        request(Get, "/me/player/queue", &[], None),
    )
    .await;

    let output = execute(
        Queue,
        json!({"action": "add", "uri": " spotify:episode:e1 ", "device_id": "speaker"}),
        request(
            Post,
            "/me/player/queue",
            &[("uri", "spotify:episode:e1"), ("device_id", "speaker")],
            None,
        ),
    )
    .await;
    assert_eq!(
        output,
        json!({
            "success": true,
            "action": "add",
            "uri": "spotify:episode:e1",
            "result": backend_payload()
        })
    );
}

#[tokio::test]
async fn playlists_list_get_and_create_shape_their_requests() {
    assert_forwarded(
        Playlists,
        json!({"limit": 0, "offset": -5}),
        request(
            Get,
            "/me/playlists",
            &[("limit", "1"), ("offset", "0")],
            None,
        ),
    )
    .await;

    assert_forwarded(
        Playlists,
        json!({
            "action": "get",
            "playlist_id": " https://open.spotify.com/playlist/p1?si=test ",
            "market": " GB "
        }),
        request(Get, "/playlists/p1", &[("market", "GB")], None),
    )
    .await;

    assert_forwarded(
        Playlists,
        json!({"action": "create", "name": " Road trip ", "public": true}),
        request(
            Post,
            "/me/playlists",
            &[],
            Some(json!({"name": "Road trip", "public": true, "collaborative": false})),
        ),
    )
    .await;
}

#[tokio::test]
async fn playlist_mutations_preserve_position_snapshot_and_non_null_details() {
    assert_forwarded(
        Playlists,
        json!({
            "action": "add_items",
            "playlist_id": "spotify:playlist:p1",
            "uris": [" spotify:track:t1 ", "spotify:track:t1", "spotify:episode:e1"],
            "position": " 2 "
        }),
        request(
            Post,
            "/playlists/p1/items",
            &[],
            Some(json!({"uris": ["spotify:track:t1", "spotify:episode:e1"], "position": 2})),
        ),
    )
    .await;

    assert_forwarded(
        Playlists,
        json!({
            "action": "remove_items",
            "playlist_id": "p1",
            "uris": " spotify:track:t1 ",
            "snapshot_id": " rev1 "
        }),
        request(
            Delete,
            "/playlists/p1/items",
            &[],
            Some(json!({"items": [{"uri": "spotify:track:t1"}], "snapshot_id": "rev1"})),
        ),
    )
    .await;

    assert_forwarded(
        Playlists,
        json!({
            "action": "update_details",
            "playlist_id": "p1",
            "name": " Keep these spaces ",
            "public": false,
            "description": null
        }),
        request(
            Put,
            "/playlists/p1",
            &[],
            Some(json!({"name": " Keep these spaces ", "public": false})),
        ),
    )
    .await;
}

#[tokio::test]
async fn albums_accept_the_id_alias_and_paginate_tracks() {
    assert_forwarded(
        Albums,
        json!({"id": "https://open.spotify.com/album/a1?si=test", "market": " CA "}),
        request(Get, "/albums/a1", &[("market", "CA")], None),
    )
    .await;

    assert_forwarded(
        Albums,
        json!({
            "action": "tracks",
            "album_id": "spotify:album:a1",
            "id": "ignored",
            "limit": 99,
            "offset": " 3 ",
            "market": "US"
        }),
        request(
            Get,
            "/albums/a1/tracks",
            &[("limit", "50"), ("offset", "3"), ("market", "US")],
            None,
        ),
    )
    .await;
}

#[tokio::test]
async fn library_lists_each_kind_and_saves_unique_normalized_uris() {
    for (kind, path) in [(" Tracks ", "/me/tracks"), ("Albums", "/me/albums")] {
        assert_forwarded(
            Library,
            json!({"kind": kind, "offset": null, "market": " NZ "}),
            request(
                Get,
                path,
                &[("limit", "20"), ("offset", "0"), ("market", "NZ")],
                None,
            ),
        )
        .await;
    }

    assert_forwarded(
        Library,
        json!({
            "kind": "albums",
            "action": "save",
            "items": ["a1", "spotify:album:a1", "https://open.spotify.com/album/a2?si=test"]
        }),
        request(
            Put,
            "/me/library",
            &[("uris", "spotify:album:a1,spotify:album:a2")],
            None,
        ),
    )
    .await;
}

#[tokio::test]
async fn handlers_reject_unknown_actions_without_calling_the_backend() {
    let backend = Arc::new(ExpectedBackend {
        request: Mutex::new(None),
    });
    for (tool, params, message) in [
        (
            Playback,
            json!({"action": " Unsupported "}),
            "Unknown spotify_playback action: unsupported",
        ),
        (
            Devices,
            json!({"action": "unsupported"}),
            "Unknown spotify_devices action: unsupported",
        ),
        (
            Queue,
            json!({"action": "unsupported"}),
            "Unknown spotify_queue action: unsupported",
        ),
        (
            Playlists,
            json!({"action": "unsupported"}),
            "Unknown spotify_playlists action: unsupported",
        ),
        (
            Albums,
            json!({"action": "unsupported", "album_id": "a1"}),
            "Unknown spotify_albums action: unsupported",
        ),
        (
            Library,
            json!({"action": "unsupported", "kind": "tracks"}),
            "Unknown spotify_library action: unsupported",
        ),
    ] {
        let handler = SpotifyHandler::new(tool, backend.clone());
        assert_invalid_params(handler.execute(params).await, message);
    }
}

#[tokio::test]
async fn invalid_parameters_are_rejected_before_any_backend_call() {
    let backend = Arc::new(ExpectedBackend {
        request: Mutex::new(None),
    });
    for (tool, params, message) in [
        (
            Playback,
            json!({"action": "recently_played", "after": 100, "before": 200}),
            "Provide only one of 'after' or 'before'",
        ),
        (
            Playback,
            json!({"action": " SEEK ", "position_ms": null}),
            "position_ms is required for action='seek'",
        ),
        (
            Search,
            json!({"query": "test", "types": " genre "}),
            "types must contain one or more of: album, artist, playlist, track, show, episode, audiobook",
        ),
        (
            Playlists,
            json!({"action": "add_items", "playlist_id": "p1", "uris": [null, " "]}),
            "At least one Spotify item is required.",
        ),
        (
            Albums,
            json!({"album_id": " "}),
            "Spotify id/uri/url is required.",
        ),
        (
            Library,
            json!({"kind": "artists"}),
            "kind must be one of: tracks, albums",
        ),
    ] {
        let handler = SpotifyHandler::new(tool, backend.clone());
        assert_invalid_params(handler.execute(params).await, message);
    }
}

#[test]
fn normalization_distinguishes_blank_items_and_wrong_item_types() {
    assert_invalid_params(
        normalize_spotify_id(" \t", Some("album")),
        "Spotify id/uri/url is required.",
    );
    assert_invalid_params(
        normalize_spotify_uri(" \t", Some("track")),
        "Spotify URI/url/id is required.",
    );

    for value in ["spotify:track:t1", "https://open.spotify.com/track/t1"] {
        assert_invalid_params(
            normalize_spotify_id(value, Some("album")),
            "Expected a Spotify album, got track.",
        );
        assert_invalid_params(
            normalize_spotify_uri(value, Some("album")),
            "Expected a Spotify album, got track.",
        );
        assert_eq!(normalize_spotify_id(value, None).unwrap(), "t1");
    }

    for incomplete in ["spotify:album", "https://open.spotify.com/album/"] {
        assert_eq!(
            normalize_spotify_id(incomplete, Some("album")).unwrap(),
            incomplete
        );
    }
}

#[test]
fn boolean_coercion_recognizes_values_and_uses_the_supplied_default() {
    for (raw, default, expected) in [
        (Some(json!(true)), false, true),
        (Some(json!(false)), true, false),
        (Some(json!(" 1 ")), false, true),
        (Some(json!(" OFF ")), true, false),
        (Some(json!("unknown")), true, true),
        (Some(json!("unknown")), false, false),
        (Some(json!(0)), true, true),
        (None, false, false),
    ] {
        assert_eq!(
            coerce_bool(raw.as_ref(), default),
            expected,
            "input: {raw:?}"
        );
    }
}

#[test]
fn list_coercion_keeps_scalar_values_and_ignores_nested_or_blank_entries() {
    let entries = json!([null, [], {}, 42, false, " track ", " "]);
    assert_eq!(as_list(Some(&entries)), ["42", "false", "track"]);
    assert_eq!(as_list(Some(&json!(" track "))), ["track"]);
    assert!(as_list(Some(&json!({}))).is_empty());
}

#[test]
fn non_object_request_body_is_preserved() {
    let body = json!([null, {"preserved": null}]);
    let request = SpotifyApiRequest::new(Post, "/test").with_body(body.clone());
    assert_eq!(request.body, Some(body));
}
