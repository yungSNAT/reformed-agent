fn stream_chunk_event(
    content: Option<&str>,
    extra: serde_json::Value,
    usage: Option<hermes_core::UsageStats>,
) -> Event {
    Event::StreamChunk(StreamChunk {
        delta: Some(hermes_core::StreamDelta {
            content: content.map(str::to_owned),
            tool_calls: None,
            extra: Some(extra),
        }),
        finish_reason: None,
        usage,
    })
}

#[test]
fn streaming_counts_each_character_once_and_applies_controls() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::streaming_counts_each_character_once_and_applies_controls",
    ) {
        return;
    }
    let mut app = build_test_app(&hermes_config::hermes_home());
    let mut state = TuiState {
        processing: true,
        auto_follow_transcript: true,
        scroll_offset: 12,
        ..TuiState::default()
    };
    assert!(!process_stream_lane_event(
        &mut app,
        &mut state,
        Event::Resize(80, 24)
    ));
    assert!(process_stream_lane_event(
        &mut app,
        &mut state,
        Event::StreamDelta(String::new())
    ));
    assert_eq!(state.stream_chunk_count, 0);
    assert!(!state.saw_first_token);

    assert!(process_stream_lane_event(
        &mut app,
        &mut state,
        stream_chunk_event(Some("aé"), serde_json::Value::Null, None)
    ));
    assert_eq!(state.stream_char_count, 2);
    assert_eq!(state.stream_chunk_count, 1);
    assert_eq!(state.scroll_offset, 0);
    assert!(state.saw_first_token);

    for (content, control) in [
        (
            Some("hidden"),
            serde_json::json!({"control": "mute_post_response", "enabled": true}),
        ),
        (None, serde_json::json!({"control": "stream_break"})),
        (
            Some("visible"),
            serde_json::json!({"control": "mute_post_response"}),
        ),
    ] {
        assert!(process_stream_lane_event(
            &mut app,
            &mut state,
            stream_chunk_event(content, control, None)
        ));
    }
    assert_eq!(state.stream_buffer, "aé\n\nvisible");
    assert_eq!(state.stream_char_count, 15);
    assert_eq!(state.stream_chunk_count, 4);
    assert!(!state.stream_needs_break);
    assert!(!state.stream_muted);

    state.awaiting_run_complete = true;
    assert!(process_stream_lane_event(
        &mut app,
        &mut state,
        Event::AgentDone
    ));
    assert!(state.status_message.contains("Finalizing"));
    assert!(!state.stream_buffer.is_empty());
    state.awaiting_run_complete = false;
    assert!(process_stream_lane_event(
        &mut app,
        &mut state,
        Event::AgentDone
    ));
    assert!(state.stream_buffer.is_empty());
    assert!(state.status_message.is_empty());
    assert!(!state.processing);
}

#[test]
fn streaming_raw_deltas_record_only_one_first_token() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::streaming_raw_deltas_record_only_one_first_token",
    ) {
        return;
    }
    let mut app = build_test_app(&hermes_config::hermes_home());
    let mut state = TuiState {
        processing_started_at: Some(Instant::now()),
        ..TuiState::default()
    };
    for delta in ["café", " ready"] {
        assert!(process_stream_lane_event(
            &mut app,
            &mut state,
            Event::StreamDelta(delta.to_owned())
        ));
    }
    assert_eq!(state.stream_buffer, "café ready");
    assert_eq!(state.stream_chunk_count, 2);
    assert_eq!(state.stream_char_count, 10);
    assert!(state.saw_first_token);
    assert_eq!(state.recent_activity.len(), 1);
    assert!(state.recent_activity[0].contains("first token in "));
}

#[test]
fn streaming_tracks_tools_thinking_phases_fallbacks_and_usage() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::streaming_tracks_tools_thinking_phases_fallbacks_and_usage",
    ) {
        return;
    }
    let mut app = build_test_app(&hermes_config::hermes_home());
    let mut state = TuiState {
        processing: true,
        ..TuiState::default()
    };
    let send = |app: &mut App, state: &mut TuiState, extra| {
        assert!(process_stream_lane_event(
            app,
            state,
            stream_chunk_event(None, extra, None)
        ));
    };
    send(
        &mut app,
        &mut state,
        serde_json::json!({
            "ui_event": "tool_start", "tool": " fixture_read ", "args_preview": " notes.md ",
        }),
    );
    send(
        &mut app,
        &mut state,
        serde_json::json!({
            "ui_event": "tool_start", "tool": "fixture_read",
        }),
    );
    assert_eq!(state.active_tools, ["fixture_read"]);
    send(
        &mut app,
        &mut state,
        serde_json::json!({
            "ui_event": "tool_complete", "tool": "fixture_read",
            "result_preview": "Read docs/notes.md",
        }),
    );
    assert!(state.active_tools.is_empty());

    send(
        &mut app,
        &mut state,
        serde_json::json!({
            "ui_event": "phase", "phase": "Route", "label": "Collecting notes", "progress_pct": 42,
        }),
    );
    assert_eq!(state.processing_phase, "route");
    assert_eq!(state.processing_phase_progress, 42);
    send(
        &mut app,
        &mut state,
        serde_json::json!({
            "ui_event": "status", "event_type": "fixture", "message": "ready",
        }),
    );
    send(
        &mut app,
        &mut state,
        serde_json::json!({
            "ui_event": "thinking", "text": "First thought. ", "thinking": "Second thought.",
        }),
    );
    assert!(state.live_thinking.contains("First thought."));
    assert!(state.live_thinking.contains("Second thought."));

    for index in 0..6 {
        send(
            &mut app,
            &mut state,
            serde_json::json!({
                "ui_event": "lifecycle", "message": format!("Fallback fixture {index}"),
            }),
        );
    }
    assert!(state.processing_degraded);
    assert_eq!(
        state.degraded_notes,
        [
            "Fallback fixture 2",
            "Fallback fixture 3",
            "Fallback fixture 4",
            "Fallback fixture 5"
        ]
    );

    for tokens in [10, 20, 74] {
        let usage = hermes_core::UsageStats {
            prompt_tokens: tokens - 2,
            completion_tokens: 2,
            total_tokens: tokens,
            ..hermes_core::UsageStats::default()
        };
        assert!(process_stream_lane_event(
            &mut app,
            &mut state,
            stream_chunk_event(None, serde_json::Value::Null, Some(usage))
        ));
    }
    assert_eq!(state.last_usage, Some((72, 2, 74)));
    assert_eq!(state.last_usage_total_emitted, Some(74));
    let activity = state.recent_activity.join("\n");
    for expected in [
        "▶ fixture_read notes.md",
        "✓ fixture_read Read docs/notes.md",
        "Δfiles docs/notes.md",
        "[fixture] ready",
        "t=10 (+10)",
        "t=74 (+64)",
    ] {
        assert!(activity.contains(expected), "{activity}");
    }
    assert!(!activity.contains("t=20"));

    send(
        &mut app,
        &mut state,
        serde_json::json!({"ui_event": "tool_complete", "tool": " fixture_write "}),
    );
    assert!(state.active_tools.is_empty());
    assert!(state
        .recent_activity
        .last()
        .unwrap()
        .ends_with("✓ fixture_write"));

    send(
        &mut app,
        &mut state,
        serde_json::json!({"ui_event": "moa_reference", "text": "Reference without a label."}),
    );
    assert!(state
        .live_thinking
        .contains("MoA reference reference: Reference without a label."));
}
