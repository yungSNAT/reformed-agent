use super::*;

// The test-name filter used by the harness expects the path segment
// `agent_loop::methods_prompt_llm::coverage_gen::`. That segment only appears
// in the full test path if these tests sit in modules with those names, so
// the tests are nested under that chain.
//
// The parent module is reached through `super::*` at the top of this file.
// The innermost module reaches the same items through the explicit
// `super::super::super::super::*` path.
mod agent_loop {
    mod methods_prompt_llm {
        mod coverage_gen {
            use super::super::super::super::*;

            fn sample_tool_call(name: &str, args: &str) -> ToolCall {
                ToolCall {
                    id: "call_1".to_string(),
                    function: hermes_core::FunctionCall {
                        name: name.to_string(),
                        arguments: args.to_string(),
                    },
                    extra_content: None,
                }
            }

            #[test]
            fn context_pressure_warning_zero_tier_never_warns() {
                let mut warned = 0.0_f64;
                let mut last = None;
                let mut pct = 0.0_f64;
                let emitted = AgentLoop::should_emit_context_pressure_warning(
                    0.9,
                    0.0,
                    &mut warned,
                    &mut last,
                    &mut pct,
                );
                assert!(!emitted);
                assert!(last.is_none());
                assert!(warned.abs() < 1e-9);
                assert!(pct.abs() < 1e-9);
            }

            #[test]
            fn context_pressure_warning_tier_upgrade_then_cooldown_suppresses() {
                let mut warned = 0.0_f64;
                let mut last = None;
                let mut pct = 0.0_f64;

                let first = AgentLoop::should_emit_context_pressure_warning(
                    0.5,
                    0.5,
                    &mut warned,
                    &mut last,
                    &mut pct,
                );
                assert!(first);
                assert!((warned - 0.5).abs() < 1e-9);
                assert!(last.is_some());
                assert!((pct - 50.0).abs() < 1e-6);

                let second = AgentLoop::should_emit_context_pressure_warning(
                    0.52,
                    0.5,
                    &mut warned,
                    &mut last,
                    &mut pct,
                );
                assert!(!second);
                assert!((pct - 50.0).abs() < 1e-6);

                let third = AgentLoop::should_emit_context_pressure_warning(
                    0.7,
                    0.5,
                    &mut warned,
                    &mut last,
                    &mut pct,
                );
                assert!(!third);

                let fourth = AgentLoop::should_emit_context_pressure_warning(
                    0.71,
                    0.75,
                    &mut warned,
                    &mut last,
                    &mut pct,
                );
                assert!(fourth);
                assert!((warned - 0.75).abs() < 1e-9);
                assert!((pct - 71.0).abs() < 1e-6);
            }

            #[test]
            fn context_pressure_warning_without_previous_warning_requires_percent_step() {
                let mut warned = 0.8_f64;
                let mut last = None;
                let mut pct = 60.0_f64;

                let small = AgentLoop::should_emit_context_pressure_warning(
                    0.62,
                    0.8,
                    &mut warned,
                    &mut last,
                    &mut pct,
                );
                assert!(!small);
                assert!(last.is_none());

                let big = AgentLoop::should_emit_context_pressure_warning(
                    0.66,
                    0.8,
                    &mut warned,
                    &mut last,
                    &mut pct,
                );
                assert!(big);
                assert!((warned - 0.8).abs() < 1e-9);
                assert!(last.is_some());
                assert!((pct - 66.0).abs() < 1e-6);
            }

            #[test]
            fn assemble_stream_message_plain_text_keeps_reasoning() {
                let m = AgentLoop::assemble_stream_assistant_message("hello", "thinking hard", &[]);
                assert_eq!(m.content.as_deref(), Some("hello"));
                assert_eq!(m.reasoning_content.as_deref(), Some("thinking hard"));
                assert!(m.tool_calls.as_ref().map(|v| v.is_empty()).unwrap_or(true));
            }

            #[test]
            fn assemble_stream_message_plain_text_without_reasoning() {
                let m = AgentLoop::assemble_stream_assistant_message("hi", "", &[]);
                assert_eq!(m.content.as_deref(), Some("hi"));
                assert!(m.reasoning_content.is_none());
            }

            #[test]
            fn assemble_stream_message_with_tool_calls_keeps_reasoning() {
                let calls = vec![sample_tool_call("read_file", "{\"path\":\"a\"}")];
                let m = AgentLoop::assemble_stream_assistant_message("", "plan", &calls);
                assert!(m.content.is_none());
                assert_eq!(m.reasoning_content.as_deref(), Some("plan"));
                let tcs = m.tool_calls.expect("tool calls present");
                assert_eq!(tcs.len(), 1);
                assert_eq!(tcs[0].function.name, "read_file");
                assert_eq!(tcs[0].function.arguments, "{\"path\":\"a\"}");
            }

            #[test]
            fn assemble_stream_message_with_tool_calls_and_content() {
                let calls = vec![sample_tool_call("terminal", "{}")];
                let m = AgentLoop::assemble_stream_assistant_message("doing it", "", &calls);
                assert_eq!(m.content.as_deref(), Some("doing it"));
                assert!(m.reasoning_content.is_none());
                assert_eq!(m.tool_calls.map(|v| v.len()), Some(1));
            }

            #[test]
            fn assemble_stream_message_nameless_tool_calls_treated_as_text() {
                let calls = vec![sample_tool_call("", "")];
                let m = AgentLoop::assemble_stream_assistant_message("text", "r", &calls);
                assert_eq!(m.content.as_deref(), Some("text"));
                assert_eq!(m.reasoning_content.as_deref(), Some("r"));
                assert!(m.tool_calls.as_ref().map(|v| v.is_empty()).unwrap_or(true));
            }

            #[test]
            fn coerce_textual_tool_calls_without_content_returns_unchanged() {
                let msg = Message::assistant_with_tool_calls(None, Vec::new());
                let (m, calls, coerced) = AgentLoop::coerce_textual_tool_calls(msg);
                assert!(!coerced);
                assert!(calls.is_empty());
                assert!(m.content.is_none());
            }

            #[test]
            fn coerce_textual_tool_calls_with_declared_calls_returns_them() {
                let msg = Message::assistant_with_tool_calls(
                    Some("x".to_string()),
                    vec![sample_tool_call("terminal", "{}")],
                );
                let (m, calls, coerced) = AgentLoop::coerce_textual_tool_calls(msg);
                assert!(!coerced);
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].function.name, "terminal");
                assert_eq!(m.content.as_deref(), Some("x"));
            }
        }
    }
}

#[test]
fn coverage_gen_root_assemble_message_smoke() {
    let m = AgentLoop::assemble_stream_assistant_message("root", "", &[]);
    assert_eq!(m.content.as_deref(), Some("root"));
    assert!(m.reasoning_content.is_none());
}
