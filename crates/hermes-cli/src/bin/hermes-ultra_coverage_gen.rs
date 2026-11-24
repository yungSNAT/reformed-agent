use std::ffi::OsString;

use super::*;

#[test]
fn unique_targets_removes_duplicates_preserving_order() {
    let input = vec![
        OsString::from("a"),
        OsString::from("b"),
        OsString::from("a"),
        OsString::from("c"),
        OsString::from("b"),
    ];
    let out = unique_targets(input);
    assert_eq!(
        out,
        vec![
            OsString::from("a"),
            OsString::from("b"),
            OsString::from("c")
        ]
    );
}

#[test]
fn unique_targets_empty_input_yields_empty_output() {
    let out = unique_targets(Vec::new());
    assert!(out.is_empty());
}

#[test]
fn unique_targets_keeps_distinct_entries() {
    let input = vec![
        OsString::from("/usr/bin/hermes-agent-ultra"),
        OsString::from("hermes-agent-ultra"),
    ];
    let out = unique_targets(input.clone());
    assert_eq!(out, input);
}

#[test]
fn candidate_targets_always_ends_with_bare_binary_name() {
    let candidates = candidate_targets();
    assert!(!candidates.is_empty());
    assert_eq!(
        candidates.last().cloned(),
        Some(OsString::from("hermes-agent-ultra"))
    );

    // If an explicit, non-empty override is set in the environment, it must
    // either be the first candidate or have been ignored (pointing at self).
    if let Ok(explicit) = std::env::var("HERMES_ULTRA_BIN") {
        let explicit = explicit.trim();
        if !explicit.is_empty() && candidates.len() > 1 {
            let first = candidates[0].to_string_lossy().to_string();
            let current = std::env::current_exe() // nosemgrep: rust.lang.security.current-exe.current-exe
                .ok()
                .and_then(|p| p.canonicalize().ok());
            let explicit_canon = std::path::PathBuf::from(explicit).canonicalize().ok();
            if explicit_canon.is_none() || explicit_canon != current {
                assert_eq!(first, explicit);
            }
        }
    }
}

#[test]
fn candidate_targets_unique_still_contains_fallback() {
    let targets = unique_targets(candidate_targets());
    assert!(targets
        .iter()
        .any(|t| t == &OsString::from("hermes-agent-ultra")));
}

#[cfg(unix)]
#[test]
fn run_target_nonexistent_path_returns_error_with_target_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("definitely-missing-hermes-binary");
    let target = OsString::from(missing.as_os_str());
    let result = run_target(target, &[OsString::from("--version")]);
    let err = result.expect_err("exec of a missing binary must fail");
    let expected_prefix = missing.to_string_lossy().to_string();
    assert!(
        err.starts_with(&expected_prefix),
        "error {err:?} should start with {expected_prefix:?}"
    );
    assert!(err.contains(" ("));
    assert!(err.ends_with(')'));
}
