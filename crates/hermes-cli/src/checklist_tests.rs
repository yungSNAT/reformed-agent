use super::*;

#[test]
fn a_key_release_is_not_a_key_press() {
    let key = |kind| {
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
            kind,
        ))
    };
    assert_eq!(pressed_key(key(KeyEventKind::Press)), Some(KeyCode::Down));
    assert_eq!(pressed_key(key(KeyEventKind::Repeat)), Some(KeyCode::Down));
    assert_eq!(pressed_key(key(KeyEventKind::Release)), None);
}

#[test]
fn test_checklist_result_default() {
    let result = ChecklistResult {
        selected: HashSet::new(),
        confirmed: true,
    };
    assert!(result.confirmed);
    assert!(result.selected.is_empty());
}

#[test]
fn test_numbered_fallback_empty_items() {
    // With empty items, curses_checklist returns immediately
    let result = curses_checklist("Test", &[], &HashSet::new(), None);
    assert!(result.confirmed);
}

#[test]
fn test_curses_select_empty_items() {
    let result = curses_select("Select", &[], 0);
    assert!(!result.confirmed);
    assert_eq!(result.index, 0);
}

#[test]
fn sanitize_display_text_flattens_multiline_labels() {
    let raw = "line1\nline2\tline3\rline4";
    let clean = sanitize_display_text(raw);
    assert_eq!(clean, "line1 line2 line3 line4");
}
