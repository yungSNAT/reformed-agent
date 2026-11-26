use super::*;

fn mk_lines(texts: &[&str]) -> Vec<Line<'static>> {
    texts.iter().map(|t| Line::from((*t).to_string())).collect()
}

fn texts_of(lines: &[Line<'static>]) -> Vec<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

#[test]
fn project_window_empty_lines_returns_empty_and_zero_offset() {
    let lines: Vec<Line<'static>> = Vec::new();
    let (window, offset) = project_transcript_window(&lines, 10, 5, 3);
    assert!(window.is_empty());
    assert_eq!(offset, 0);
}

#[test]
fn project_window_top_row_past_end_clamps_to_last_line() {
    let lines = mk_lines(&["a", "b"]);
    let (window, offset) = project_transcript_window(&lines, 10, 5, 3);
    assert_eq!(texts_of(&window), vec!["b".to_string()]);
    assert_eq!(offset, 0);
}

#[test]
fn project_window_rebases_when_intra_offset_exceeds_u16() {
    let long = "a".repeat(70_000);
    let lines = vec![
        Line::from(long),
        Line::from("b".to_string()),
        Line::from("c".to_string()),
    ];
    let (window, offset) = project_transcript_window(&lines, 1, 66_000, 1);
    assert_eq!(texts_of(&window), vec!["b".to_string()]);
    assert_eq!(offset, 0);
    assert!(offset <= u16::MAX as usize);
}

#[test]
fn project_window_rebase_collects_enough_rows_for_viewport() {
    let long = "x".repeat(70_000);
    let lines = vec![
        Line::from(long),
        Line::from("b".to_string()),
        Line::from("c".to_string()),
        Line::from("d".to_string()),
    ];
    let (window, offset) = project_transcript_window(&lines, 1, 69_000, 2);
    assert_eq!(offset, 0);
    assert_eq!(texts_of(&window), vec!["b".to_string(), "c".to_string()]);
}

#[test]
fn project_window_normal_case_starts_mid_line() {
    // width 2: "aaaa" -> 2 rows, "bb" -> 1 row, "cc" -> 1 row
    let lines = mk_lines(&["aaaa", "bb", "cc"]);
    let (window, offset) = project_transcript_window(&lines, 2, 1, 2);
    assert_eq!(offset, 1);
    assert_eq!(
        texts_of(&window),
        vec!["aaaa".to_string(), "bb".to_string()]
    );
}

#[test]
fn approximate_visual_rows_counts_wrapped_rows() {
    let lines = mk_lines(&["abcdef", "", "xy"]);
    // width 4: 6 chars -> 2 rows, empty -> 1 row, 2 chars -> 1 row
    assert_eq!(approximate_visual_rows(&lines, 4), 4);
    let empty: Vec<Line<'static>> = Vec::new();
    assert_eq!(approximate_visual_rows(&empty, 4), 1);
    // width 0 treated as 1
    assert_eq!(approximate_visual_rows(&mk_lines(&["abc"]), 0), 3);
}

#[test]
fn line_visual_rows_handles_exact_multiples() {
    let line = Line::from("abcd".to_string());
    assert_eq!(line_visual_rows(&line, 4), 1);
    assert_eq!(line_visual_rows(&line, 2), 2);
    assert_eq!(line_visual_rows(&line, 3), 2);
    assert_eq!(line_visual_rows(&line, 0), 4);
}
