use super::Gateway;

// The slash-command impl is included in `gateway` rather than declared as
// a module. Keep the requested test filter usable in that layout.
mod gateway {
    mod slash_command_methods {
        mod coverage_gen {
            use super::super::super::Gateway;

            #[test]
            fn verbose_tool_progress_normalizes_to_a_mode_that_cycles_to_off() {
                for input in ["verbose", "VERBOSE", " \tVeRbOsE\r\n"] {
                    let mode =
                        Gateway::normalize_tool_progress_mode(input).expect("verbose is valid");
                    assert_eq!(mode, "verbose", "input: {input:?}");
                    assert_eq!(Gateway::next_tool_progress_mode(&mode), "off");
                }
            }

            #[test]
            fn unrecognized_tool_progress_modes_are_rejected() {
                for input in ["", " \t\n", "quiet", "verbose extra", "ver bose", "2"] {
                    assert_eq!(
                        Gateway::normalize_tool_progress_mode(input),
                        None,
                        "input: {input:?}"
                    );
                }
            }
        }
    }
}
