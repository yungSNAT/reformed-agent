use crate::tui::{fit_status_line, fuzzy_subsequence_score, hard_wrap_segments, truncate_chars};

#[test]
fn fuzzy_subsequence_score_empty_inputs_return_zero() {
    assert_eq!(fuzzy_subsequence_score("", "model"), 0);
    assert_eq!(fuzzy_subsequence_score("m", ""), 0);
    assert_eq!(fuzzy_subsequence_score("", ""), 0);
}

#[test]
fn fuzzy_subsequence_score_counts_matches() {
    // Two consecutive matches, two points each.
    assert_eq!(fuzzy_subsequence_score("ab", "ab"), 4);
    // 'm' at index 0 (+2), 'c' right after '-' (+2 +1 bonus).
    assert_eq!(fuzzy_subsequence_score("mc", "model-config"), 5);
    // Skips non-matching characters without penalty.
    assert_eq!(fuzzy_subsequence_score("ac", "abc"), 4);
}

#[test]
fn fuzzy_subsequence_score_missing_char_returns_zero() {
    assert_eq!(fuzzy_subsequence_score("z", "abc"), 0);
    // Out-of-order subsequence is not a match.
    assert_eq!(fuzzy_subsequence_score("ba", "ab"), 0);
    // Needle longer than haystack.
    assert_eq!(fuzzy_subsequence_score("abcd", "abc"), 0);
}

#[test]
fn truncate_chars_edge_cases() {
    assert_eq!(truncate_chars("abc", 0), "");
    assert_eq!(truncate_chars("abc", 1), "…");
    assert_eq!(truncate_chars("abcdef", 4), "abc…");
    assert_eq!(truncate_chars("ab", 5), "ab");
    assert_eq!(truncate_chars("abc", 3), "abc");
}

#[test]
fn fit_status_line_zero_width_returns_empty() {
    assert_eq!(fit_status_line("anything", 0), "");
}

#[test]
fn fit_status_line_skips_zero_width_chars_and_pads() {
    let out = fit_status_line("a\u{0301}b", 5);
    assert_eq!(out, "ab   ");
    assert_eq!(out.chars().count(), 5);
}

#[test]
fn fit_status_line_clips_to_width() {
    assert_eq!(fit_status_line("abcdef", 3), "abc");
}

#[test]
fn hard_wrap_segments_empty_and_whitespace() {
    assert_eq!(hard_wrap_segments("", 5), vec![String::new()]);
    assert_eq!(hard_wrap_segments("    ", 5), vec![String::new()]);
}

#[test]
fn hard_wrap_segments_long_token_flushes_current() {
    let segs = hard_wrap_segments("hi abcdefgh", 3);
    assert_eq!(
        segs,
        vec![
            "hi".to_string(),
            "abc".to_string(),
            "def".to_string(),
            "gh".to_string()
        ]
    );
}

#[test]
fn hard_wrap_segments_word_wrap() {
    let segs = hard_wrap_segments("aa bb cc", 5);
    assert_eq!(segs, vec!["aa bb".to_string(), "cc".to_string()]);
}
