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
fn numbered_pickers_preserve_selection_and_handle_piped_input() {
    if !crate::isolated_test_profile::enter_with_input(
        "checklist::tests::numbered_pickers_preserve_selection_and_handle_piped_input",
        b"invalid\n99\n1\n2\n\n1\n\n99\n",
    ) {
        return;
    }
    assert!(!atty_is_tty());
    let items = ["Daily report".into(), "Weekly summary".into()];
    let selections = std::cell::RefCell::new(Vec::new());
    let status = |selected: &HashSet<usize>| {
        selections.borrow_mut().push(selected.clone());
        format!("Selected {}", selected.len())
    };
    let result = curses_checklist("Reports", &items, &HashSet::from([0]), Some(&status));
    assert!(result.confirmed);
    assert_eq!(result.selected, HashSet::from([1]));
    let selections = selections.into_inner();
    assert_eq!(selections.first(), Some(&HashSet::from([0])));
    assert!(selections.iter().any(HashSet::is_empty));
    assert_eq!(selections.last(), Some(&HashSet::from([1])));

    let selected = curses_select("Choose a report", &items, 1);
    assert!(selected.confirmed);
    assert_eq!(selected.index, 0);

    let default = curses_select_embedded("Keep a report", &items, 99);
    assert!(default.confirmed);
    assert_eq!(default.index, 1);

    let invalid = curses_select_embedded("Choose a report", &items, 0);
    assert!(!invalid.confirmed);
    assert_eq!(invalid.index, 0);
}

#[test]
fn sanitize_display_text_flattens_multiline_labels() {
    let raw = "line1\nline2\tline3\rline4";
    let clean = sanitize_display_text(raw);
    assert_eq!(clean, "line1 line2 line3 line4");
}
