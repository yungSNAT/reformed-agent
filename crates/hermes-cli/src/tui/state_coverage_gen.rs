use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

fn plain_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn control_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

#[test]
fn expanded_tool_output_preserves_the_original_text() {
    let output = "first\nsecond\nthird\nfourth\n";
    let mut section = ToolOutputSection::new("test tool".to_string(), output.to_string());

    assert_eq!(
        section.display_text(),
        "first\nsecond\nthird\n  ... (1 more lines, press Enter to expand)"
    );
    section.is_expanded = true;
    assert_eq!(section.display_text(), output);
}

#[test]
fn scrolling_down_only_resumes_following_at_the_latest_row() {
    let mut state = TuiState::default();
    state.scroll_history_up(4);

    state.scroll_history_down(0);
    assert_eq!(state.scroll_offset, 3);
    assert!(!state.auto_follow_transcript);

    state.scroll_history_down(5);
    assert_eq!(state.scroll_offset, 0);
    assert!(state.auto_follow_transcript);
}

#[test]
fn blank_activity_does_not_create_a_row_or_consume_a_sequence_number() {
    let mut state = TuiState::default();
    state.push_activity("  first  ");
    state.push_activity(" \t\n ");
    state.push_activity("\nsecond ");

    assert_eq!(state.recent_activity, ["01. first", "02. second"]);
}

#[test]
fn live_thinking_joins_nonempty_chunks_with_one_space() {
    let mut state = TuiState::default();
    state.append_live_thinking("  checking ");
    state.append_live_thinking("\t\n ");
    assert_eq!(state.live_thinking, "checking");

    state.append_live_thinking("\n contexte ");
    assert_eq!(state.live_thinking, "checking contexte");
}

#[test]
fn blocking_action_sets_the_phase_and_reports_progress() {
    let mut state = TuiState {
        processing: true,
        processing_phase: "preflight".to_string(),
        ..TuiState::default()
    };

    state.mark_blocking_action(" \n ");
    assert_eq!(state.processing_phase, "preflight");
    assert_eq!(state.processing_phase_progress, 0);
    assert!(state.recent_activity.is_empty());
    assert!(state.last_progress_pulse_at.is_none());

    state.mark_blocking_action("  loading local data  ");
    assert_eq!(state.processing_phase, "command");
    assert_eq!(state.processing_phase_label, "loading local data");
    assert_eq!(state.processing_phase_progress, 5);
    assert_eq!(
        state.recent_activity,
        [
            "01. ◈ loading local data",
            "02. … working 0.0s • 0 chunks • 0 chars • no active tools • phase: loading local data (5%)",
        ]
    );

    state.processing_phase_progress = 70;
    state.last_progress_pulse_at = Some(Instant::now() + Duration::from_secs(3600));
    let long_label = "é".repeat(125);
    state.mark_blocking_action(&long_label);

    let shortened = truncate_chars(&long_label, 100);
    assert_ne!(shortened, long_label);
    assert_eq!(state.processing_phase_label, shortened);
    assert_eq!(state.processing_phase_progress, 70);
    assert_eq!(state.recent_activity.len(), 3);
    assert_eq!(state.recent_activity[2], format!("03. ◈ {shortened}"));
}

#[test]
fn progress_pulses_skip_idle_and_recent_updates_without_waiting() {
    let mut state = TuiState::default();
    assert_eq!(state.processing_elapsed(), Duration::ZERO);
    state.maybe_emit_progress_pulse();
    assert!(state.recent_activity.is_empty());
    assert!(state.last_progress_pulse_at.is_none());

    let age = Duration::from_secs(3);
    let started = Instant::now() - age;
    state.processing = true;
    state.processing_started_at = Some(started);
    state.stream_chunk_count = 3;
    state.stream_char_count = 8;
    state.active_tools.push("local lookup".to_string());
    assert!(state.processing_elapsed() >= age);

    let recent = Instant::now() + Duration::from_secs(3600);
    state.last_progress_pulse_at = Some(recent);
    state.maybe_emit_progress_pulse();
    assert!(state.recent_activity.is_empty());
    assert_eq!(state.last_progress_pulse_at, Some(recent));

    state.last_progress_pulse_at = Some(started);
    state.maybe_emit_progress_pulse();
    assert_eq!(state.recent_activity.len(), 1);
    let row = &state.recent_activity[0];
    assert!(row.starts_with("01. … working "));
    assert!(row.ends_with("s • 3 chunks • 8 chars • 1 active tool(s) • phase: n/a"));
    assert!(state
        .last_progress_pulse_at
        .is_some_and(|pulse| pulse > started));
}

#[test]
fn finishing_a_degraded_cycle_reports_fallback_and_releases_processing_state() {
    let mut state = TuiState {
        processing: true,
        awaiting_run_complete: true,
        processing_degraded: true,
        degraded_notes: vec!["local fallback".to_string(), "retry succeeded".to_string()],
        stream_chunk_count: 2,
        stream_char_count: 11,
        saw_first_token: true,
        processing_phase: "stream".to_string(),
        processing_phase_label: "streaming".to_string(),
        processing_phase_progress: 90,
        ..TuiState::default()
    };
    state.scroll_history_up(8);

    state.finish_processing_cycle("✔ completed in");
    assert_eq!(
        state.recent_activity,
        [
            "01. ⚠ completed with fallback in 0.00s • 2 chunks • 11 chars",
            "02. fallback notes: local fallback | retry succeeded",
        ]
    );
    assert!(!state.processing);
    assert!(!state.awaiting_run_complete);
    assert!(!state.processing_degraded);
    assert!(state.degraded_notes.is_empty());
    assert!(!state.saw_first_token);
    assert_eq!((state.stream_chunk_count, state.stream_char_count), (0, 0));
    assert!(state.processing_phase.is_empty());
    assert!(state.processing_phase_label.is_empty());
    assert_eq!(state.processing_phase_progress, 0);
    assert_eq!(state.scroll_offset, 0);
    assert!(state.auto_follow_transcript);

    state.scroll_history_up(3);
    state.finish_processing_cycle("✔ duplicate completion in");
    assert_eq!(state.recent_activity.len(), 2);
    assert_eq!(state.scroll_offset, 3);
    assert!(!state.auto_follow_transcript);
}

#[test]
fn phase_updates_ignore_idle_cycles_and_use_the_phase_name_without_a_label() {
    let mut state = TuiState::default();
    state.update_processing_phase("prepare", "should be ignored", Some(20));
    assert!(state.processing_phase.is_empty());
    assert!(state.processing_phase_label.is_empty());
    assert_eq!(state.processing_phase_progress, 0);
    assert!(state.recent_activity.is_empty());
    assert_eq!(state.processing_stage_label(), "idle");

    state.processing = true;
    assert_eq!(state.processing_stage_label(), "awaiting first token");
    state.update_processing_phase("  PLAN  ", "", Some(130));
    assert_eq!(state.processing_phase, "plan");
    assert_eq!(state.processing_phase_progress, 100);
    assert_eq!(state.recent_activity, ["01. ◈ phase 100% • plan"]);

    state.update_processing_phase(" \t ", "\n", None);
    assert_eq!(state.processing_phase, "plan");
    assert_eq!(state.processing_phase_progress, 100);
    assert_eq!(state.recent_activity.len(), 1);

    state.update_processing_phase("", "  checking inputs  ", Some(40));
    assert_eq!(state.processing_stage_label(), "phase-driven");
    assert_eq!(state.processing_phase, "plan");
    assert_eq!(
        state.recent_activity[1],
        "02. ◈ phase 40% • checking inputs"
    );
}

#[test]
fn modal_keys_do_not_act_on_a_closed_picker() {
    let mut state = TuiState {
        input: "draft".to_string(),
        cursor_position: 5,
        ..TuiState::default()
    };
    state.close_modal();
    assert!(!state.modal_active());

    for code in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('1')] {
        assert!(matches!(
            state.handle_modal_key(plain_key(code)),
            ModalAction::None
        ));
    }
    assert_eq!(state.input, "draft");
    assert_eq!(state.cursor_position, 5);
    assert!(!state.modal_active());
}

#[test]
fn display_control_chords_toggle_back_and_update_the_status() {
    let mut state = TuiState::default();
    assert!(!state.handle_insert_control_chord(plain_key(KeyCode::Char('l'))));
    assert!(!state.handle_insert_control_chord(control_key(KeyCode::Char('z'))));
    assert!(state.activity_lane_open);
    assert!(state.status_message.is_empty());

    let lane_visibility = control_key(KeyCode::Char('l'));
    assert!(state.handle_insert_control_chord(lane_visibility));
    assert!(!state.activity_lane_open);
    assert_eq!(state.status_message, "Activity lane hidden");
    assert!(state.handle_insert_control_chord(lane_visibility));
    assert!(state.activity_lane_open);
    assert_eq!(state.status_message, "Activity lane enabled");

    let lane_mode = control_key(KeyCode::Char('o'));
    assert!(state.handle_insert_control_chord(lane_mode));
    assert!(matches!(
        state.activity_lane_mode,
        ActivityLaneMode::Cockpit
    ));
    assert_eq!(state.status_message, "Activity lane mode: ops cockpit");
    assert!(state.handle_insert_control_chord(lane_mode));
    assert!(matches!(state.activity_lane_mode, ActivityLaneMode::Live));
    assert_eq!(state.status_message, "Activity lane mode: live");

    let density = control_key(KeyCode::Char('d'));
    assert!(state.handle_insert_control_chord(density));
    assert!(matches!(state.view_density, ViewDensity::Compact));
    assert_eq!(state.status_message, "Compact transcript mode");
    assert!(state.handle_insert_control_chord(density));
    assert!(matches!(state.view_density, ViewDensity::Detailed));
    assert_eq!(state.status_message, "Detailed transcript mode");

    let timestamps = control_key(KeyCode::Char('t'));
    assert!(state.handle_insert_control_chord(timestamps));
    assert!(state.show_timestamps);
    assert_eq!(state.status_message, "Timestamps visible");
    assert!(state.handle_insert_control_chord(timestamps));
    assert!(!state.show_timestamps);
    assert_eq!(state.status_message, "Timestamps hidden");

    let cards = control_key(KeyCode::Char('e'));
    assert!(state.handle_insert_control_chord(cards));
    assert!(state.expanded_tool_cards.contains("__all__"));
    assert_eq!(state.status_message, "Expanded tool cards");
    assert!(state.handle_insert_control_chord(cards));
    assert!(state.expanded_tool_cards.is_empty());
    assert_eq!(state.status_message, "Collapsed tool cards");
}

#[test]
fn timestamps_command_toggles_and_returns_the_value_to_persist() {
    let mut state = TuiState::default();

    let (message, persist) = state.apply_timestamps_command(&[]);
    assert!(state.show_timestamps);
    assert_eq!(persist, Some(true));
    assert_eq!(message, "Timestamps visible");
    assert_eq!(state.status_message, message);

    let (message, persist) = state.apply_timestamps_command(&["  ToGgLe  ".to_string()]);
    assert!(!state.show_timestamps);
    assert_eq!(persist, Some(false));
    assert_eq!(message, "Timestamps hidden");
    assert_eq!(state.status_message, message);
}

#[test]
fn input_lines_preserve_empty_rows_and_vertical_movement_stays_on_boundaries() {
    let mut state = TuiState::default();
    assert_eq!(state.input_line_text(), [Line::from("")]);

    state.input = "é界\nβ\n\n".to_string();
    assert_eq!(
        state.input_line_text(),
        [
            Line::from("é界"),
            Line::from("β"),
            Line::from(""),
            Line::from("")
        ]
    );

    state.cursor_position = "é界".len();
    state.apply_editor_input(plain_key(KeyCode::Down));
    assert_eq!(state.cursor_position, "é界\nβ".len());
    state.apply_editor_input(plain_key(KeyCode::Up));
    assert_eq!(state.cursor_position, "é".len());
    assert_eq!(
        TuiState::row_col_to_byte_offset(&state.input, 20, 1),
        state.input.len()
    );
}

#[test]
fn horizontal_editor_motion_clamps_mid_character_offsets_and_stops_at_the_ends() {
    let mut state = TuiState {
        input: "aé界".to_string(),
        cursor_position: 2,
        ..TuiState::default()
    };

    state.apply_editor_input(plain_key(KeyCode::Left));
    assert_eq!(state.cursor_position, 0);
    for expected in [1, 3, 6, 6] {
        state.apply_editor_input(plain_key(KeyCode::Right));
        assert_eq!(state.cursor_position, expected);
    }
    for expected in [3, 1, 0, 0] {
        state.apply_editor_input(plain_key(KeyCode::Left));
        assert_eq!(state.cursor_position, expected);
    }

    state.apply_editor_input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
    assert_eq!(state.input, "aé界");
    assert_eq!(state.cursor_position, 0);
}

#[test]
fn word_navigation_crosses_whitespace_and_uses_utf8_byte_offsets() {
    let mut state = TuiState {
        input: "é猫 \tβz  tail".to_string(),
        ..TuiState::default()
    };
    let right = control_key(KeyCode::Right);
    let left = control_key(KeyCode::Left);

    for expected in [5, 10, 16] {
        assert!(state.handle_insert_control_chord(right));
        assert_eq!(state.cursor_position, expected);
    }
    for expected in [12, 7, 0, 0] {
        assert!(state.handle_insert_control_chord(left));
        assert_eq!(state.cursor_position, expected);
    }

    state.input.clear();
    state.cursor_position = 16;
    assert!(state.handle_insert_control_chord(right));
    assert_eq!(state.cursor_position, 0);
    state.cursor_position = 16;
    assert!(state.handle_insert_control_chord(left));
    assert_eq!(state.cursor_position, 0);
}

#[test]
fn empty_paste_leaves_the_editor_alone_and_nonempty_paste_normalizes_newlines() {
    let mut state = TuiState {
        input: "é!".to_string(),
        cursor_position: 1,
        selection_anchor: Some(2),
        completions: vec!["/old".to_string()],
        completion_index: Some(0),
        ..TuiState::default()
    };

    state.insert_paste_at_cursor("");
    assert_eq!(state.input, "é!");
    assert_eq!(state.cursor_position, 1);
    assert_eq!(state.selection_anchor, Some(2));
    assert_eq!(state.completions, ["/old"]);
    assert_eq!(state.completion_index, Some(0));

    state.insert_paste_at_cursor("a\r\nb\rc");
    assert_eq!(state.input, "a\nb\ncé!");
    assert_eq!(state.cursor_position, "a\nb\nc".len());
    assert_eq!(state.selection_anchor, None);
    assert!(state.completions.is_empty());
    assert_eq!(state.completion_index, None);
}

#[test]
fn refreshing_completions_resets_the_selection_and_plain_text_clears_the_menu() {
    let mut state = TuiState {
        input: "/he".to_string(),
        completions: vec!["/old".to_string()],
        completion_index: Some(0),
        ..TuiState::default()
    };

    state.refresh_completions();
    assert!(state
        .completions
        .iter()
        .any(|entry| entry.starts_with("/help")));
    assert!(!state.completions.iter().any(|entry| entry == "/old"));
    assert_eq!(state.completion_index, None);

    state.input = "hello".to_string();
    state.completion_index = Some(0);
    state.update_completions();
    assert!(state.completions.is_empty());
    assert_eq!(state.completion_index, None);
    assert_eq!(state.input, "hello");
}

#[test]
fn completion_navigation_wraps_and_resets_when_no_options_remain() {
    let mut state = TuiState {
        completions: ["/one", "/two", "/three", "/four"]
            .map(String::from)
            .to_vec(),
        ..TuiState::default()
    };

    for (delta, expected) in [(-6, 2), (6, 0), (-1, 3), (1, 0)] {
        state.move_completion_selection(delta);
        assert_eq!(state.completion_index, Some(expected));
    }

    state.completions.clear();
    state.move_completion_selection(1);
    assert_eq!(state.completion_index, None);
}

#[test]
fn accepting_completion_prefers_the_selection_then_falls_back_to_the_first_item() {
    let mut state = TuiState {
        input: "/m".to_string(),
        cursor_position: 2,
        completions: vec!["/model".to_string(), "/model café".to_string()],
        completion_index: Some(1),
        ..TuiState::default()
    };

    state.accept_completion();
    assert_eq!(state.input, "/model café");
    assert_eq!(state.cursor_position, "/model café".len());

    state.completion_index = Some(10);
    state.accept_completion();
    assert_eq!(state.input, "/model");
    assert_eq!(state.cursor_position, "/model".len());

    state.completions.clear();
    state.accept_completion();
    assert_eq!(state.input, "/model");
    assert_eq!(state.cursor_position, "/model".len());
}

#[test]
fn ticking_the_pet_wraps_its_frame_without_advancing_the_spinner() {
    let mut state = TuiState {
        pet_frame: usize::MAX,
        spinner_frame: 3,
        ..TuiState::default()
    };

    state.tick_pet();
    assert_eq!(state.pet_frame, 0);
    state.tick_pet();
    assert_eq!(state.pet_frame, 1);
    assert_eq!(state.spinner_frame, 3);
}
