use super::*;

#[test]
fn embedded_picker_guard_toggles_active_flag() {
    assert!(!embedded_picker_active());
    {
        let _guard = EmbeddedPickerGuard::enter();
        assert!(embedded_picker_active());
    }
    assert!(!embedded_picker_active());
}

#[test]
fn embedded_picker_guard_nested_drop_resets_flag() {
    let outer = EmbeddedPickerGuard::enter();
    assert!(embedded_picker_active());
    drop(outer);
    assert!(!embedded_picker_active());
}

#[test]
fn curses_select_embedded_empty_items_returns_unconfirmed() {
    let result = curses_select_embedded("Pick", &[], 5);
    assert_eq!(result.index, 0);
    assert!(!result.confirmed);
    assert!(!embedded_picker_active());
}

#[test]
fn truncate_for_terminal_zero_width_is_empty() {
    assert_eq!(truncate_for_terminal("hello", 0), "");
}

#[test]
fn truncate_for_terminal_limits_chars() {
    assert_eq!(truncate_for_terminal("hello world", 5), "hello");
    assert_eq!(truncate_for_terminal("abc", 10), "abc");
    assert_eq!(truncate_for_terminal("✦✓❯xyz", 3), "✦✓❯");
    assert_eq!(truncate_for_terminal("", 4), "");
}

#[test]
fn sanitize_display_text_replaces_control_chars_and_collapses_spaces() {
    let raw = "  a\u{0007}b \u{001b}  c\n\n d  ";
    assert_eq!(sanitize_display_text(raw), "a b c d");
    assert_eq!(sanitize_display_text("\n\t\r"), "");
}

#[test]
fn select_result_and_checklist_result_are_cloneable() {
    let sel = SelectResult {
        index: 3,
        confirmed: true,
    };
    let copied = sel;
    assert_eq!(copied.index, 3);
    assert!(copied.confirmed);

    let mut set = HashSet::new();
    set.insert(1usize);
    let res = ChecklistResult {
        selected: set,
        confirmed: false,
    };
    let cloned = res.clone();
    assert!(cloned.selected.contains(&1));
    assert!(!cloned.confirmed);
    assert!(format!("{:?}", cloned).contains("ChecklistResult"));
}
