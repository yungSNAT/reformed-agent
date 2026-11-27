use super::*;

// The embedded picker flag is shared by all tests in this process.
static PICKER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn embedded_picker_guard_toggles_active_flag() {
    let _lock = PICKER_TEST_LOCK.lock().expect("picker test lock");
    assert!(!embedded_picker_active());
    assert!(tui_event_reader().is_some());
    {
        let _guard = EmbeddedPickerGuard::enter();
        assert!(embedded_picker_active());
        assert!(tui_event_reader().is_none());
    }
    assert!(!embedded_picker_active());
    assert!(tui_event_reader().is_some());
}

#[test]
fn embedded_picker_waits_for_a_tui_read_in_progress() {
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::time::{Duration, Instant};

    let _lock = PICKER_TEST_LOCK.lock().expect("picker test lock");
    std::thread::scope(|scope| {
        let reader = tui_event_reader().expect("TUI owns the terminal input");
        let (sent, received) = channel();
        scope.spawn(move || {
            let _ = sent.send(tui_event_reader().is_none());
        });
        assert_eq!(received.recv_timeout(Duration::from_secs(5)), Ok(true));

        let (entered, picker) = channel();
        scope.spawn(move || {
            let _guard = EmbeddedPickerGuard::enter();
            entered.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !embedded_picker_active() {
            assert!(
                Instant::now() < deadline,
                "picker did not request terminal input"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            picker.recv_timeout(Duration::from_millis(20)),
            Err(RecvTimeoutError::Timeout),
        );
        drop(reader);
        picker.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    assert!(tui_event_reader().is_some());
}

#[test]
fn embedded_picker_guard_drop_resets_flag() {
    let _lock = PICKER_TEST_LOCK.lock().expect("picker test lock");
    let guard = EmbeddedPickerGuard::enter();
    assert!(embedded_picker_active());
    drop(guard);
    assert!(!embedded_picker_active());
}

#[test]
fn curses_select_embedded_empty_items_returns_unconfirmed() {
    let _lock = PICKER_TEST_LOCK.lock().expect("picker test lock");
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
