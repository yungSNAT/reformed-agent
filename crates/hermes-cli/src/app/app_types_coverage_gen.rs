use super::*;

#[test]
fn pet_dock_as_str_covers_both_variants() {
    assert_eq!(PetDock::Left.as_str(), "left");
    assert_eq!(PetDock::Right.as_str(), "right");
    assert_eq!(PetDock::default().as_str(), "right");
}

#[test]
fn list_session_snapshot_entries_missing_dir_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist");
    let entries = list_session_snapshot_entries(&missing);
    assert!(entries.is_empty());
}

#[test]
fn list_session_snapshot_entries_filters_non_json_and_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let json_file = dir.path().join("session-a.json");
    std::fs::write(&json_file, b"{}").expect("write json");
    std::fs::write(dir.path().join("notes.txt"), b"hello").expect("write txt");
    std::fs::write(dir.path().join("noext"), b"x").expect("write noext");
    std::fs::create_dir(dir.path().join("subdir.json")).expect("create dir");

    let entries = list_session_snapshot_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, json_file);
    assert_eq!(entries[0].size_bytes, 2);
}

#[test]
fn available_disk_space_bytes_nonexistent_path_is_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("nope").join("deeper");
    assert_eq!(available_disk_space_bytes(&missing), None);
}

#[test]
fn merge_usage_stats_sums_costs_when_both_present() {
    let prev = UsageStats {
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
        estimated_cost: Some(0.5),
        cache_read_tokens: 1,
        cache_write_tokens: 2,
    };
    let new = UsageStats {
        prompt_tokens: 3,
        completion_tokens: 4,
        total_tokens: 7,
        estimated_cost: Some(0.25),
        cache_read_tokens: 3,
        cache_write_tokens: 4,
    };
    let merged = merge_usage_stats(Some(prev), &new);
    assert_eq!(merged.prompt_tokens, 13);
    assert_eq!(merged.completion_tokens, 9);
    assert_eq!(merged.total_tokens, 22);
    assert_eq!(merged.estimated_cost, Some(0.75));
    assert_eq!(merged.cache_read_tokens, 4);
    assert_eq!(merged.cache_write_tokens, 6);
}

#[test]
fn merge_usage_stats_takes_new_cost_when_previous_missing() {
    let prev = UsageStats {
        prompt_tokens: 1,
        completion_tokens: 1,
        total_tokens: 2,
        estimated_cost: None,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let new = UsageStats {
        prompt_tokens: 2,
        completion_tokens: 2,
        total_tokens: 4,
        estimated_cost: Some(1.5),
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let merged = merge_usage_stats(Some(prev), &new);
    assert_eq!(merged.estimated_cost, Some(1.5));
    assert_eq!(merged.total_tokens, 6);
}

#[test]
fn merge_usage_stats_keeps_none_when_both_costs_missing() {
    let prev = UsageStats {
        prompt_tokens: 1,
        completion_tokens: 0,
        total_tokens: 1,
        estimated_cost: None,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let new = UsageStats {
        prompt_tokens: 1,
        completion_tokens: 0,
        total_tokens: 1,
        estimated_cost: None,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let merged = merge_usage_stats(Some(prev), &new);
    assert_eq!(merged.estimated_cost, None);
    assert_eq!(merged.prompt_tokens, 2);
}

#[test]
fn merge_usage_stats_keeps_previous_cost_when_new_missing() {
    let prev = UsageStats {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        estimated_cost: Some(2.0),
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let new = UsageStats {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        estimated_cost: None,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let merged = merge_usage_stats(Some(prev), &new);
    assert_eq!(merged.estimated_cost, Some(2.0));
}
