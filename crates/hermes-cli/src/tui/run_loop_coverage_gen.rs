use super::{tui_event, Event};
use crossterm::event::{Event as CrosstermEvent, KeyModifiers, MouseEvent, MouseEventKind};

#[test]
fn tui_event_preserves_resize_dimensions() {
    assert!(matches!(
        tui_event(CrosstermEvent::Resize(113, 37)),
        Some(Event::Resize(113, 37))
    ));
}

#[test]
fn tui_event_preserves_mouse_kind_position_and_modifiers() {
    let mouse = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 17,
        row: 9,
        modifiers: KeyModifiers::SHIFT,
    };

    let Some(Event::Mouse(actual)) = tui_event(CrosstermEvent::Mouse(mouse)) else {
        panic!("expected a TUI mouse event");
    };
    assert_eq!(actual, mouse);
}

#[test]
fn tui_event_preserves_multiline_paste_verbatim() {
    let text = "  first line\r\ncafé\n";

    let Some(Event::Paste(actual)) = tui_event(CrosstermEvent::Paste(text.to_owned())) else {
        panic!("expected a TUI paste event");
    };
    assert_eq!(actual, text);
}
