use super::*;

#[test]
fn current_policy_profile_maps_presets_in_isolated_processes() {
    const PRESET: &str = "HERMES_TOOL_POLICY_PRESET";
    const CHILD_EXPECTED: &str = "HERMES_POLICY_CONTEXT_COVERAGE_EXPECTED_PROFILE";
    const CHILD_OK: &str = "policy-context-profile-assertion-passed";

    if let Ok(expected) = std::env::var(CHILD_EXPECTED) {
        assert_eq!(current_policy_profile_name(), expected.as_str());
        println!("{CHILD_OK}");
        return;
    }

    let qualified_name = concat!(
        module_path!(),
        "::current_policy_profile_maps_presets_in_isolated_processes"
    );
    let (_, test_name) = qualified_name
        .split_once("::")
        .expect("the test name should include a crate prefix");
    let executable = std::env::current_exe().expect("locate the unit test harness"); // nosemgrep: rust.lang.security.current-exe.current-exe

    // Set presets only in the child so other tests never see these values.
    for (preset, expected) in [
        (None, "standard"),
        (Some(" \tStRiCt\n"), "strict"),
        (Some(" DeV "), "dev"),
        (Some("balanced"), "standard"),
        (Some("unrecognized-preset"), "standard"),
    ] {
        let mut child = std::process::Command::new(&executable);
        child
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD_EXPECTED, expected);
        match preset {
            Some(value) => child.env(PRESET, value),
            None => child.env_remove(PRESET),
        };
        let output = child.output().expect("run the isolated profile test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(CHILD_OK),
            "profile assertion for preset {preset:?} did not pass: {}\nstdout: {stdout}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn truncate_chars_respects_unicode_character_limits() {
    assert_eq!(truncate_chars("é🦀界", 0), "");
    assert_eq!(truncate_chars("", 5), "");
    assert_eq!(truncate_chars("é🦀界", 3), "é🦀界");
    assert_eq!(truncate_chars("é🦀界", 5), "é🦀界");
    assert_eq!(truncate_chars("é🦀界x", 3), "é🦀…");
    assert_eq!(truncate_chars("é🦀界", 1), "…");
}
