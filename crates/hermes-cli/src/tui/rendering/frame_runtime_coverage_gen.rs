use super::*;
use ratatui::text::Line;
use std::time::Duration;

#[test]
fn stream_lane_budget_from_quality_profile() {
    let (cap, budget) = stream_lane_budget_from("advisory", "quality", false, 0);
    assert_eq!(cap, 120);
    assert_eq!(budget, Duration::from_millis(8));
}

#[test]
fn stream_lane_budget_from_reliability_profile() {
    let (cap, budget) = stream_lane_budget_from("advisory", "reliability", false, 0);
    assert_eq!(cap, 192);
    assert_eq!(budget, Duration::from_millis(12));
}

#[test]
fn stream_lane_budget_from_safety_profile() {
    let (cap, budget) = stream_lane_budget_from("advisory", "safety", false, 0);
    assert_eq!(cap, 96);
    assert_eq!(budget, Duration::from_millis(8));
}

#[test]
fn stream_lane_budget_from_processing_many_chunks_raises_floor() {
    let (cap, budget) = stream_lane_budget_from("advisory", "balanced", true, 41);
    assert_eq!(cap, 224);
    assert_eq!(budget, Duration::from_millis(12));

    let (cap, budget) = stream_lane_budget_from("advisory", "quality", true, 100);
    assert_eq!(cap, 224);
    assert_eq!(budget, Duration::from_millis(12));

    let (cap, budget) = stream_lane_budget_from("advisory", "throughput", true, 100);
    assert_eq!(cap, 320);
    assert_eq!(budget, Duration::from_millis(16));

    // Exactly 40 chunks does not trigger the floor.
    let (cap, budget) = stream_lane_budget_from("advisory", "balanced", true, 40);
    assert_eq!(cap, 96);
    assert_eq!(budget, Duration::from_millis(6));
}

#[test]
fn stream_lane_budget_matches_env_derived_inputs() {
    let profile = std::env::var("HERMES_PERF_AUTOPILOT_PROFILE")
        .ok()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "balanced".to_string());
    let mode = std::env::var("HERMES_PERF_AUTOPILOT_MODE")
        .ok()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "advisory".to_string());

    let expected = stream_lane_budget_from(mode.as_str(), profile.as_str(), true, 50);
    let actual = stream_lane_budget(true, 50);
    assert_eq!(actual, expected);

    let expected_idle = stream_lane_budget_from(mode.as_str(), profile.as_str(), false, 0);
    assert_eq!(stream_lane_budget(false, 0), expected_idle);
}

#[test]
fn find_anchor_line_index_empty_lines_returns_none() {
    let lines: Vec<Line<'static>> = Vec::new();
    assert_eq!(find_anchor_line_index(&lines, "anything", 0), None);
}

#[test]
fn find_anchor_line_index_falls_back_to_full_scan() {
    let radius = OFFSET_ANCHOR_SEARCH_RADIUS;
    let len = radius.saturating_mul(2).saturating_add(10);
    let mut lines: Vec<Line<'static>> = (0..len).map(|i| Line::from(format!("line {i}"))).collect();
    let anchor_idx = len - 1;
    lines[anchor_idx] = Line::from("the-anchor".to_string());

    // Expected index 0 => search window is [0, radius], anchor lies outside it.
    assert!(anchor_idx > radius);
    assert_eq!(
        find_anchor_line_index(&lines, "the-anchor", 0),
        Some(anchor_idx)
    );
}

#[test]
fn find_anchor_line_index_missing_anchor_returns_none() {
    let lines: Vec<Line<'static>> = (0..5).map(|i| Line::from(format!("row {i}"))).collect();
    assert_eq!(find_anchor_line_index(&lines, "absent", 2), None);
}
