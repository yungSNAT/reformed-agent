use super::*;

fn picker_item(label: &str, detail: &str, value: &str) -> PickerItem {
    PickerItem {
        label: label.to_owned(),
        detail: detail.to_owned(),
        value: value.to_owned(),
    }
}

#[test]
fn picker_ranks_matches_and_clamps_selection_after_filtering() {
    let mut picker = PickerModal::new(
        PickerKind::Personality,
        "Choose a personality",
        vec![
            picker_item("Zebra", "Related to CAT", "detail"),
            picker_item("The Cat", "", "contains-later"),
            picker_item("Caterpillar", "", "long-prefix"),
            picker_item("Cart", "", "subsequence"),
            picker_item("cat", "", "exact"),
            picker_item("Cats", "", "short-prefix"),
            picker_item("A Cat", "", "contains-earlier"),
            picker_item("Dog", "", "cat"),
        ],
    );
    picker.move_selection(-1);
    assert_eq!(
        picker.selected_item().map(|item| item.label.as_str()),
        Some("Dog")
    );

    picker.query = "  cAt  ".to_owned();
    picker.refresh_filter();

    assert_eq!(picker.filtered_indices, [4, 5, 2, 6, 1, 0, 3]);
    assert_eq!(picker.selected_filtered, 6);
    assert_eq!(
        picker.selected_item().map(|item| item.value.as_str()),
        Some("subsequence")
    );
}

#[test]
fn picker_handles_movement_and_windows_when_no_items_match() {
    let mut picker = PickerModal::new(
        PickerKind::Personality,
        "Choose a personality",
        vec![picker_item("One", "", "one"), picker_item("Two", "", "two")],
    );
    picker.move_selection(1);
    assert_eq!(picker.selected_filtered, 1);

    picker.query = "zzzz".to_owned();
    picker.refresh_filter();

    assert!(picker.filtered_indices.is_empty());
    assert_eq!(picker.selected_filtered, 0);
    assert!(picker.selected_item().is_none());
    assert_eq!(picker.visible_window(), (0, 0));

    picker.move_selection(-3);
    assert_eq!(picker.selected_filtered, 0);
    picker.page_move(1);
    assert!(picker.selected_item().is_none());
    assert_eq!(picker.visible_window(), (0, 0));
}

#[test]
fn picker_wraps_selection_and_scrolls_the_visible_window() {
    let mut picker = PickerModal::new(
        PickerKind::Personality,
        "Choose a personality",
        ["One", "Two", "Three", "Four", "Five"]
            .into_iter()
            .map(|label| picker_item(label, "", label))
            .collect(),
    );
    picker.page_size = 2;
    assert_eq!(picker.visible_window(), (0, 2));

    picker.move_selection(-6);
    assert_eq!(picker.selected_filtered, 4);
    assert_eq!(picker.visible_window(), (3, 5));

    picker.move_selection(2);
    assert_eq!(picker.selected_filtered, 1);
    assert_eq!(picker.visible_window(), (0, 2));

    picker.page_move(1);
    assert_eq!(picker.selected_filtered, 3);
    assert_eq!(picker.visible_window(), (2, 4));

    picker.page_move(-2);
    assert_eq!(picker.selected_filtered, 4);
    assert_eq!(picker.visible_window(), (3, 5));

    picker.page_size = 0;
    assert_eq!(picker.visible_window(), (4, 5));
    picker.page_move(1);
    assert_eq!(picker.selected_filtered, 0);
    assert_eq!(picker.visible_window(), (0, 1));

    picker.page_size = 10;
    assert_eq!(picker.visible_window(), (0, 5));
}

#[test]
fn picker_toggles_values_only_in_multi_select_mode_with_a_selected_item() {
    let mut picker = PickerModal::new(
        PickerKind::Personality,
        "Choose personalities",
        vec![
            picker_item("Same label", "", "first"),
            picker_item("Same label", "", "second"),
        ],
    );

    picker.toggle_selected();
    assert!(picker.selected_values.is_empty());

    picker.allow_multi = true;
    picker.toggle_selected();
    assert_eq!(picker.selected_values, HashSet::from(["first".to_owned()]));

    picker.move_selection(1);
    picker.toggle_selected();
    assert_eq!(
        picker.selected_values,
        HashSet::from(["first".to_owned(), "second".to_owned()])
    );

    picker.toggle_selected();
    assert_eq!(picker.selected_values, HashSet::from(["first".to_owned()]));

    picker.query = "no matches".to_owned();
    picker.refresh_filter();
    assert!(picker.selected_item().is_none());
    picker.toggle_selected();
    assert_eq!(picker.selected_values, HashSet::from(["first".to_owned()]));
}

fn restored_tui() -> Tui {
    // A fixed viewport avoids querying the terminal size. Mark cleanup as
    // complete so neither restore nor Drop touches the terminal.
    let terminal = ratatui::Terminal::with_options(
        CrosstermBackend::new(std::io::stdout()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 8, 3)),
        },
    )
    .expect("fixed viewport should not require an attached terminal");
    let (event_sender, events) = mpsc::unbounded_channel();
    let (stream_sender, stream_events) = mpsc::unbounded_channel();
    Tui {
        terminal,
        events,
        stream_events,
        event_sender,
        stream_sender,
        theme: Theme::default(),
        restored: true,
        mouse_capture_enabled: false,
    }
}

#[test]
fn tui_senders_deliver_to_separate_receivers() {
    let mut tui = restored_tui();
    tui.event_sender()
        .send(Event::Message("control".to_owned()))
        .expect("control receiver should be open");
    assert!(matches!(
        tui.stream_events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        tui.events.try_recv(),
        Ok(Event::Message(text)) if text == "control"
    ));

    tui.stream_sender()
        .send(Event::StreamDelta("delta".to_owned()))
        .expect("stream receiver should be open");
    assert!(matches!(
        tui.events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        tui.stream_events.try_recv(),
        Ok(Event::StreamDelta(text)) if text == "delta"
    ));
}

#[test]
fn tui_redundant_terminal_operations_leave_state_unchanged() {
    let mut tui = restored_tui();
    tui.set_mouse_capture(false)
        .expect("requesting the current mouse capture state should succeed");
    assert!(!tui.mouse_capture_enabled);

    // Use the flag as a sentinel to distinguish an early return from cleanup.
    tui.mouse_capture_enabled = true;
    tui.restore().expect("repeated restoration should succeed");
    assert!(tui.restored);
    assert!(tui.mouse_capture_enabled);
}

#[test]
fn tui_theme_borrows_the_stored_theme() {
    let tui = restored_tui();
    assert!(std::ptr::eq(tui.theme(), &tui.theme));
}

#[tokio::test(flavor = "current_thread")]
async fn abort_and_join_task_empties_the_slot_and_waits_for_cancellation() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        started_tx.send(()).expect("test should receive readiness");
        std::future::pending::<()>().await;
    });
    let completion = handle.abort_handle();
    let mut task = Some(handle);
    started_rx.await.expect("background task should start");

    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        abort_and_join_task(&mut task),
    )
    .await
    .expect("pending task should be cancelled and joined");

    assert!(task.is_none());
    assert!(
        completion.is_finished(),
        "cancellation should already be complete"
    );

    abort_and_join_task(&mut task).await;
    assert!(task.is_none());
}
