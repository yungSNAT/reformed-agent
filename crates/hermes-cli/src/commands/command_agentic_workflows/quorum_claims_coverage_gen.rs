use super::*;

#[test]
fn swarm_mode_defaults_to_concurrent() {
    for input in [None, Some(" CoNcUrReNt "), Some("unknown"), Some(" ")] {
        assert!(
            matches!(parse_swarm_mode(input), SwarmExecutionMode::Concurrent),
            "expected concurrent mode for {input:?}"
        );
    }
}

#[test]
fn swarm_mode_recognizes_sequential_and_graph_aliases() {
    for input in ["sequential", " SeQuEnCe "] {
        assert!(
            matches!(
                parse_swarm_mode(Some(input)),
                SwarmExecutionMode::Sequential
            ),
            "expected sequential mode for {input:?}"
        );
    }

    for input in ["graph", " DAG "] {
        assert!(
            matches!(parse_swarm_mode(Some(input)), SwarmExecutionMode::Graph),
            "expected graph mode for {input:?}"
        );
    }
}

#[test]
fn swarm_pass_cap_handles_aliases_defaults_and_bounds() {
    const PASS_CAP_ENV: &str = "HERMES_QUORUM_VOTER_PASSES";
    const EXPECTED_ENV: &str = "HERMES_CLI_QUORUM_COVERAGE_EXPECTED_PASS_CAP";

    if let Ok(expected) = std::env::var(EXPECTED_ENV) {
        assert_eq!(
            read_swarm_pass_cap(),
            expected
                .parse::<usize>()
                .expect("numeric expected pass cap")
        );
        return;
    }

    // Each input gets its own environment so parallel tests cannot see the override.
    let executable = std::env::current_exe().expect("locate the test executable");
    let (_, test_name) = concat!(
        module_path!(),
        "::swarm_pass_cap_handles_aliases_defaults_and_bounds"
    )
    .split_once("::")
    .expect("test path starts with the crate name");

    for (input, expected) in [
        (None, 6),
        (Some("not-a-number"), 6),
        (Some("0"), 64),
        (Some(" OFF "), 64),
        (Some("unlimited"), 64),
        (Some("InFiNiTe"), 64),
        (Some(" 17 "), 17),
        (Some("00"), 1),
        (Some("65"), 64),
    ] {
        let mut command = std::process::Command::new(&executable);
        command
            .args(["--exact", test_name])
            .env(EXPECTED_ENV, expected.to_string());
        if let Some(value) = input {
            command.env(PASS_CAP_ENV, value);
        } else {
            command.env_remove(PASS_CAP_ENV);
        }

        let output = command.output().expect("run the isolated pass-cap test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("1 passed;"),
            "expected pass cap {expected} for {input:?}:\n{stdout}\n{stderr}"
        );
    }
}
