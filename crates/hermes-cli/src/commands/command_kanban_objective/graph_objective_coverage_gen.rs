// Preserve the targeted test filter when graph_objective.rs is included
// directly in command_kanban_objective rather than declared as a module.
mod commands {
    mod command_kanban_objective {
        mod graph_objective {
            mod coverage_gen {
                use super::super::super::super::super::{
                    parse_objective_wait_request, parse_objective_wait_seconds,
                    ObjectiveWaitRequest,
                };

                #[test]
                fn wait_seconds_parses_positive_numbers_with_optional_units() {
                    for (raw, expected) in [
                        (" 12 ", 12),
                        ("1s", 1),
                        ("5s", 5),
                        (" 7 sec ", 7),
                        ("18446744073709551615", u64::MAX),
                    ] {
                        assert_eq!(parse_objective_wait_seconds(raw), Some(expected), "{raw:?}");
                    }
                }

                #[test]
                fn wait_seconds_rejects_blank_zero_invalid_and_overflowing_values() {
                    for raw in [
                        "",
                        " \t ",
                        "0s",
                        "-1",
                        "1.5",
                        "soon",
                        "18446744073709551616",
                    ] {
                        assert_eq!(parse_objective_wait_seconds(raw), None, "{raw:?}");
                    }
                }

                #[test]
                fn wait_request_reports_usage_when_an_argument_is_missing() {
                    let target_usage = concat!(
                        "Usage: /objective wait <pid|session_id|session:<id>|--session <id>",
                        "|--seconds <n>|for <n>s> [reason...]"
                    );
                    let cases: &[(&[&str], &str)] = &[
                        (&["wait"], target_usage),
                        (&["wait", " \t "], target_usage),
                        (
                            &["wait", "--session"],
                            "Usage: /objective wait --session <session_id> [reason...]",
                        ),
                        (
                            &["wait", "session", "  "],
                            "Usage: /objective wait --session <session_id> [reason...]",
                        ),
                        (
                            &["wait", "--seconds"],
                            "Usage: /objective wait --seconds <seconds> [reason...]",
                        ),
                        (
                            &["wait", "for", "  "],
                            "Usage: /objective wait --seconds <seconds> [reason...]",
                        ),
                    ];
                    for &(args, expected) in cases {
                        assert_eq!(
                            parse_objective_wait_request(args).err().as_deref(),
                            Some(expected),
                            "{args:?}"
                        );
                    }
                }

                #[test]
                fn wait_request_rejects_empty_session_ids_and_nonpositive_waits() {
                    let cases: &[(&[&str], &str)] = &[
                        (
                            &["wait", "session:  "],
                            "objective wait session id cannot be empty",
                        ),
                        (
                            &["wait", "sid:"],
                            "objective wait session id cannot be empty",
                        ),
                        (
                            &["wait", "seconds:"],
                            "objective wait seconds must be positive",
                        ),
                        (
                            &["wait", "for:0s"],
                            "objective wait seconds must be positive",
                        ),
                        (
                            &["wait", "--seconds", "-2"],
                            "objective wait seconds must be positive",
                        ),
                        (&["wait", "0"], "objective wait pid must be positive"),
                    ];
                    for &(args, expected) in cases {
                        assert_eq!(
                            parse_objective_wait_request(args).err().as_deref(),
                            Some(expected),
                            "{args:?}"
                        );
                    }
                }

                #[test]
                fn wait_request_preserves_session_ids_and_collects_reasons() {
                    let cases: &[(&[&str], &str, &str)] = &[
                        (
                            &["wait", "session:  Build-42 ", "  after", "tests  "],
                            "Build-42",
                            "after tests",
                        ),
                        (&["wait", "sid:Review-5"], "Review-5", ""),
                        (
                            &["wait", "--SESSION", " 007 ", " hold", "for review "],
                            "007",
                            "hold for review",
                        ),
                        (&["wait", "  Job-9 ", " queued "], "Job-9", "queued"),
                    ];
                    for &(args, expected_id, expected_reason) in cases {
                        let ObjectiveWaitRequest::Session(id, reason) =
                            parse_objective_wait_request(args).expect("valid session wait")
                        else {
                            panic!("expected a session wait for {args:?}");
                        };
                        assert_eq!(id, expected_id, "{args:?}");
                        assert_eq!(reason, expected_reason, "{args:?}");
                    }
                }

                #[test]
                fn wait_request_recognizes_flag_prefixed_and_bare_timers() {
                    let cases: &[(&[&str], u64, &str)] = &[
                        (
                            &["wait", "--seconds", " 8 ", " after", "build "],
                            8,
                            "after build",
                        ),
                        (&["wait", "FOR", "5s", " cooldown "], 5, "cooldown"),
                        (&["wait", "secs:12sec", " next", "check "], 12, "next check"),
                        (&["wait", "seconds:4294967296"], 4_294_967_296, ""),
                        (&["wait", "for: 3s", "  "], 3, ""),
                        (&["wait", " 7s ", " retry "], 7, "retry"),
                        (&["wait", "9sec"], 9, ""),
                    ];
                    for &(args, expected_seconds, expected_reason) in cases {
                        let ObjectiveWaitRequest::Seconds(seconds, reason) =
                            parse_objective_wait_request(args).expect("valid timer wait")
                        else {
                            panic!("expected a timer wait for {args:?}");
                        };
                        assert_eq!(seconds, expected_seconds, "{args:?}");
                        assert_eq!(reason, expected_reason, "{args:?}");
                    }
                }

                #[test]
                fn wait_request_treats_an_unqualified_positive_integer_as_a_pid() {
                    let args = ["wait", " 4294967295 ", " process", "cleanup "];
                    let ObjectiveWaitRequest::Pid(pid, reason) =
                        parse_objective_wait_request(&args).expect("valid PID wait")
                    else {
                        panic!("an unqualified integer should select a PID wait");
                    };
                    assert_eq!(pid, u32::MAX);
                    assert_eq!(reason, "process cleanup");
                }
            }
        }
    }
}
