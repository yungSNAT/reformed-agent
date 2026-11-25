#[test]
fn picker_confirmation_updates_personality_and_keeps_question_answers_as_drafts() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::picker_confirmation_updates_personality_and_keeps_question_answers_as_drafts",
    ) {
        return;
    }
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut app = build_test_app(&hermes_config::hermes_home());
    let mut state = TuiState::default();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    runtime.block_on(async {
        process_modal_confirm(&mut state, &mut app).await.unwrap();
        let descriptions = hermes_agent::builtin_personality_descriptions();
        let (previous, _) = descriptions[1];
        app.switch_personality(previous);
        open_personality_modal(&mut state, &app);
        let modal = state.modal.as_ref().unwrap();
        assert!(matches!(modal.kind, PickerKind::Personality));
        assert_eq!(modal.selected_item().unwrap().value, previous);

        let modal = state.modal.as_mut().unwrap();
        modal.query = "zz-fixture-no-matching-personality".into();
        modal.refresh_filter();
        process_modal_confirm(&mut state, &mut app).await.unwrap();
        assert_eq!(state.status_message, "Nothing selected");
        assert_eq!(app.current_personality.as_deref(), Some(previous));
        let modal = state.modal.as_mut().unwrap();
        modal.query.clear();
        modal.refresh_filter();
        let (selected, _) = descriptions[0];
        state.handle_modal_key(key(KeyCode::Home));
        assert!(matches!(
            state.handle_modal_key(key(KeyCode::Enter)),
            ModalAction::Confirm
        ));
        process_modal_confirm(&mut state, &mut app).await.unwrap();
        assert!(!state.modal_active());
        assert_eq!(app.current_personality.as_deref(), Some(selected));
        assert_eq!(state.status_message, format!("Personality: {selected}"));
        assert_eq!(
            app.ui_messages.last().unwrap().message.content.as_deref(),
            Some(format!("Switched personality to `{selected}`.").as_str())
        );

        let transcript = app.transcript_messages();
        let question =
            parse_interactive_question_request("/ask Pick a drink | Tea | Café :: warm").unwrap();
        open_interactive_question_modal(&mut state, question);
        assert!(matches!(
            state.handle_modal_key(key(KeyCode::Char('2'))),
            ModalAction::Confirm
        ));
        process_modal_confirm(&mut state, &mut app).await.unwrap();
        assert!(!state.modal_active());
        assert_eq!(state.input, "Pick a drink\nAnswer: Café");
        assert_eq!(state.cursor_position, state.input.len());
        assert_eq!(
            state.status_message,
            "Interactive answer inserted. Press Enter to send."
        );
        assert_eq!(app.transcript_messages(), transcript);
    });
}

#[test]
fn model_picker_applies_an_offline_preset_and_preserves_it_on_failure() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::model_picker_applies_an_offline_preset_and_preserves_it_on_failure",
    ) {
        return;
    }

    let mut app = build_test_app(&hermes_config::hermes_home());
    let mut state = TuiState::default();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let preset = |id: &str| PickerItem {
        label: id.to_owned(),
        detail: String::new(),
        value: id.to_owned(),
    };
    runtime.block_on(async {
        open_provider_model_modal(&mut state, &app, "fixture-unknown").await;
        assert_eq!(
            state.status_message,
            "No models available for provider `fixture-unknown`"
        );
        assert!(!state.modal_active());

        state.open_modal(PickerModal::new(
            PickerKind::ModelProvider,
            "Select Provider",
            vec![preset("moa")],
        ));
        process_modal_confirm(&mut state, &mut app).await.unwrap();
        assert_eq!(state.status_message, "Provider selected: moa");
        let modal = state.modal.as_ref().unwrap();
        assert!(matches!(&modal.kind, PickerKind::ModelForProvider { provider } if provider == "moa"));
        assert_eq!(modal.selected_item().unwrap().value, "default");

        process_modal_confirm(&mut state, &mut app).await.unwrap();
        assert_eq!(app.current_model, "moa:default");
        assert_eq!(state.status_message, "Switched model to moa:default");
        assert!(!state.modal_active());

        state.open_modal(PickerModal::new(
            PickerKind::ModelForProvider { provider: "moa".into() },
            "Select moa model",
            vec![preset("fixture-unknown")],
        ));
        process_modal_confirm(&mut state, &mut app).await.unwrap();
        assert_eq!(app.current_model, "moa:default");
        assert!(!state.modal_active());
        assert!(state.status_message.contains("unsupported MoA preset 'fixture-unknown'"));
        assert!(state.status_message.ends_with("staying on moa:default."));
        assert_eq!(
            app.ui_messages.last().unwrap().message.content.as_deref(),
            Some(state.status_message.as_str())
        );
    });
}

fn pending_test_turn() -> TuiState {
    TuiState {
        processing: true,
        awaiting_run_complete: true,
        stream_buffer: "partial fixture response".into(),
        stream_muted: true,
        stream_needs_break: true,
        active_tools: vec!["fixture tool".into()],
        ..TuiState::default()
    }
}

#[test]
fn completion_events_persist_final_messages_and_clear_processing_state() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::completion_events_persist_final_messages_and_clear_processing_state",
    ) {
        return;
    }
    let mut app = build_test_app(&hermes_config::hermes_home());
    let path = app
        .state_root
        .join("sessions")
        .join(format!("{}.json", app.session_id));
    for (interrupted, answer, activity) in [
        (
            false,
            "fixture early stop",
            "run stopped before natural finish",
        ),
        (
            true,
            "fixture interrupted",
            "run finished in 1.25s (total_turns=2)",
        ),
    ] {
        app.ui_messages.clear();
        let mut state = pending_test_turn();
        let messages = vec![
            Message::user("fixture question"),
            Message::assistant(answer),
        ];
        assert!(process_stream_lane_event(
            &mut app,
            &mut state,
            Event::AgentRunComplete {
                result: Ok(AgentResult {
                    messages: messages.clone(),
                    total_turns: 2,
                    interrupted,
                    ..AgentResult::default()
                }),
                elapsed_secs: 1.25,
            },
        ));
        assert_eq!(app.messages, messages);
        assert_eq!(app.ui_messages.len(), usize::from(interrupted));
        if interrupted {
            assert_eq!(
                app.ui_messages[0].message.content.as_deref(),
                Some("[Agent execution interrupted]")
            );
        }
        assert!(state
            .recent_activity
            .iter()
            .any(|line| line.ends_with(activity)));
        assert_turn_cleared(&state);
        let snapshot: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let persisted = snapshot["messages"].as_array().unwrap();
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[0]["role"], "User");
        assert_eq!(persisted[0]["content"], "fixture question");
        assert_eq!(persisted[1]["role"], "Assistant");
        assert_eq!(persisted[1]["content"], answer);
    }

    let mut completed = Box::new(app.clone());
    completed.push_ui_assistant("confirmed managed turn");
    let mut state = pending_test_turn();
    assert!(process_stream_lane_event(
        &mut app,
        &mut state,
        Event::ManagedAppRunComplete {
            result: Ok(completed),
            elapsed_secs: 0.5
        },
    ));
    assert_turn_cleared(&state);
    assert_eq!(
        app.ui_messages.last().unwrap().message.content.as_deref(),
        Some("confirmed managed turn")
    );
    assert!(state
        .recent_activity
        .iter()
        .any(|line| line.ends_with("managed run finished in 0.50s")));

    let messages = app.messages.clone();
    for (event, expected) in [
        (
            Event::AgentRunComplete {
                result: Err("fixture agent failure".into()),
                elapsed_secs: 0.25,
            },
            "Error: fixture agent failure",
        ),
        (
            Event::ManagedAppRunComplete {
                result: Err("fixture managed failure".into()),
                elapsed_secs: 0.25,
            },
            "Error: fixture managed failure",
        ),
    ] {
        let mut state = pending_test_turn();
        let previous_ui_count = app.ui_messages.len();
        assert!(process_stream_lane_event(&mut app, &mut state, event));
        assert_turn_cleared(&state);
        assert_eq!(state.status_message, expected);
        assert_eq!(app.ui_messages.len(), previous_ui_count + 1);
        assert_eq!(
            app.ui_messages.last().unwrap().message.content.as_deref(),
            Some(expected)
        );
        assert_eq!(app.messages, messages);
    }
}

fn assert_turn_cleared(state: &TuiState) {
    assert!(!state.processing);
    assert!(!state.awaiting_run_complete);
    assert!(state.stream_buffer.is_empty());
    assert!(!state.stream_muted);
    assert!(!state.stream_needs_break);
    assert!(state.active_tools.is_empty());
}

#[test]
fn interruption_flushes_partial_text_and_surfaces_snapshot_failure() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::interruption_flushes_partial_text_and_surfaces_snapshot_failure",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app(&home);
    let mut state = pending_test_turn();
    app.messages = vec![Message::user("fixture question")];
    finalize_interrupted_tui_session(&mut app, &mut state, "fixture");
    assert!(!state.awaiting_run_complete);
    assert!(state
        .recent_activity
        .iter()
        .any(|line| line.ends_with("interrupted session snapshot flushed")));
    let path = app
        .state_root
        .join("sessions")
        .join(format!("{}.json", app.session_id));
    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(snapshot["messages"][0]["content"], "fixture question");
    assert_eq!(
        snapshot["messages"][1]["content"],
        "partial fixture response"
    );

    let blocked = home.join("fixture-file");
    std::fs::write(&blocked, "not a directory").unwrap();
    app.state_root = blocked;
    state = pending_test_turn();
    state.stream_buffer = " ".into();
    finalize_interrupted_tui_session(&mut app, &mut state, "fixture");
    assert!(!state.awaiting_run_complete);
    assert_eq!(app.messages.len(), 2);
    assert!(state
        .recent_activity
        .iter()
        .any(|line| line.contains("interrupted autosave skipped")));
}
