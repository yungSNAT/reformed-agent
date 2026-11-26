#[test]
fn keyboard_completions_wrap_and_accept_the_selected_command() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::keyboard_completions_wrap_and_accept_the_selected_command",
    ) {
        return;
    }
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut app = build_test_app(&hermes_config::hermes_home());
    let mut state = TuiState {
        input: "/".into(),
        cursor_position: 1,
        completions: ["/one", "/two", "/three", "/four"]
            .map(String::from)
            .to_vec(),
        ..TuiState::default()
    };
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    for (code, selected) in [
        (KeyCode::End, 3),
        (KeyCode::Down, 0),
        (KeyCode::Up, 3),
        (KeyCode::PageDown, 1),
        (KeyCode::PageUp, 3),
        (KeyCode::Home, 0),
        (KeyCode::Down, 1),
    ] {
        assert!(!state.handle_key(key(code), &mut app));
        assert_eq!(state.completion_index, Some(selected), "{code:?}");
        assert_eq!(state.input, "/");
        assert_eq!(state.scroll_offset, 0);
    }
    assert!(!state.handle_key(key(KeyCode::Tab), &mut app));
    assert_eq!(state.input, "/two");
    assert_eq!(state.cursor_position, 4);
    assert!(state.completions.is_empty());
    assert_eq!(state.completion_index, None);

    state.mode = InputMode::Normal;
    assert!(!state.handle_key(key(KeyCode::Char(':')), &mut app));
    assert_eq!(state.mode, InputMode::Command);
    assert!(state.input.is_empty());
    state.completions = ["/help", "/model"].map(String::from).to_vec();
    for expected in ["/help", "/model", "/help"] {
        assert!(!state.handle_key(key(KeyCode::Tab), &mut app));
        assert_eq!(state.input, expected);
        assert_eq!(state.cursor_position, expected.len());
    }
    assert!(!state.handle_key(key(KeyCode::Enter), &mut app));
    assert_eq!(state.mode, InputMode::Insert);
    assert!(state.input.is_empty());
    assert!(state.completions.is_empty());
    assert_eq!(state.cursor_position, 0);
}

#[test]
fn keyboard_history_scroll_and_multiline_edits_keep_separate_state() {
    if !crate::isolated_test_profile::enter(
        "tui::tests::keyboard_history_scroll_and_multiline_edits_keep_separate_state",
    ) {
        return;
    }
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut app = build_test_app(&hermes_config::hermes_home());
    app.input_history = ["collect draft", "read archive", "collect final"]
        .map(String::from)
        .to_vec();
    app.history_index = app.input_history.len();
    app.push_ui_user("Earlier prompt");
    app.push_ui_user(format!("  {}  ", "é".repeat(121)));
    app.push_ui_assistant("Latest response");
    let mut state = TuiState::default();
    let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let control = |code| KeyEvent::new(code, KeyModifiers::CONTROL);

    for (code, expected) in [
        (KeyCode::Up, "collect final"),
        (KeyCode::Up, "read archive"),
        (KeyCode::Down, "collect final"),
        (KeyCode::Up, "read archive"),
    ] {
        assert!(!state.handle_key(plain(code), &mut app));
        assert_eq!(state.input, expected);
        assert_eq!(state.cursor_position, expected.len());
    }
    assert!(!state.handle_key(control(KeyCode::Char('r')), &mut app));
    assert!(state.history_search_active);
    for ch in ['c', 'o'] {
        assert!(!state.handle_key(plain(KeyCode::Char(ch)), &mut app));
    }
    assert_eq!(state.input, "collect final");
    assert_eq!(state.history_search_query, "co");
    assert!(!state.handle_key(plain(KeyCode::Backspace), &mut app));
    assert_eq!(state.history_search_query, "c");
    assert!(!state.handle_key(plain(KeyCode::Esc), &mut app));
    assert!(!state.history_search_active);
    assert!(state.history_search_query.is_empty());
    assert_eq!(state.input, "collect final");

    for (code, modifiers, offset) in [
        (KeyCode::Up, KeyModifiers::CONTROL, 1),
        (KeyCode::Up, KeyModifiers::ALT, 2),
        (KeyCode::Down, KeyModifiers::SHIFT, 1),
        (KeyCode::PageUp, KeyModifiers::NONE, 9),
        (KeyCode::PageDown, KeyModifiers::NONE, 1),
        (KeyCode::Home, KeyModifiers::NONE, usize::MAX),
    ] {
        assert!(!state.handle_key(KeyEvent::new(code, modifiers), &mut app));
        assert_eq!(state.scroll_offset, offset, "{code:?}");
        assert!(!state.auto_follow_transcript);
        assert_eq!(state.input, "collect final");
    }
    state.refresh_sticky_prompt(&app);
    assert_eq!(state.sticky_prompt, format!("{}…", "é".repeat(119)));
    assert!(!state.handle_key(control(KeyCode::End), &mut app));
    state.refresh_sticky_prompt(&app);
    assert_eq!(state.scroll_offset, 0);
    assert!(state.auto_follow_transcript);
    assert!(state.sticky_prompt.is_empty());

    state.input = "café".into();
    state.cursor_position = 2;
    state.selection_anchor = Some(0);
    assert!(!state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &mut app));
    assert_eq!(state.input, "ca\nfé");
    assert_eq!(state.cursor_position, 3);
    assert_eq!(state.selection_anchor, None);
    assert!(!state.handle_key(control(KeyCode::Char('j')), &mut app));
    assert_eq!(state.input, "ca\n\nfé");
    assert_eq!(state.cursor_position, 4);
    assert!(!state.handle_key(plain(KeyCode::Esc), &mut app));
    assert!(state.input.is_empty());
    assert_eq!(state.cursor_position, 0);
    assert_eq!(state.mode, InputMode::Insert);
}
