//! `/review [instructions]`: an independent reviewer for the work just
//! discussed (upstream `agent/review_engine.py`). It runs as a background
//! job, as `/btw` does, with the recent conversation as its evidence.

use hermes_core::{Message, MessageRole};
use hermes_py::{self as py, Py};

use super::{emit_command_output, queue_background_job, AgentError, App, CommandResult};

/// Recent user and assistant messages the reviewer gets.
const CONTEXT_MESSAGES: usize = 10;
/// Characters kept from one message (upstream's `_MESSAGE_CHAR_CAP`).
const MESSAGE_CAP: usize = 12_000;
/// Marks a cut message.
const TRUNCATED: &str = "\n[... truncated ...]";
/// Skills the reviewer is told to load, at most (upstream's limit).
const SKILL_LIMIT: usize = 8;

const REVIEW_GOAL: &str = "Act as an independent senior reviewer. Thoroughly review the work presented in the conversation excerpt below: investigate any code, pull request, branch, commit, documentation, design, or other artifact it references (open the PR, read the diff, run the code or tests where feasible) rather than judging from the excerpt alone. Produce a full, structured review: what the work does, whether it is correct and complete, concrete defects or risks found (with file/line references where possible), what was verified vs. only read, and a clear final verdict with recommended next steps.";
const EXCERPT_INTRO: &str = "You were spawned by the /review command. The following is an excerpt of the most recent conversation between the user and their primary agent. It is your starting evidence: the work to review is referenced in it.";
const EXCERPT_START: &str = "--- Recent conversation (oldest first) ---";
const EXCERPT_END: &str = "--- End of conversation excerpt ---";
const INSTRUCTIONS_INTRO: &str = "Additional review instructions from the user:";
const CLOSING: &str = "Your review is read by the primary agent's user. Be direct and specific; do not soften findings.";

/// `/review [instructions]`: queues the reviewer and says how to read it.
pub(super) fn handle_review_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    match build_review_task(&app.messages, &args.join(" ")) {
        Ok(task) => {
            let job = queue_background_job(&task)?;
            emit_command_output(
                app,
                format!(
                    "Review started as background job {id}.\nRead it with `/background tail {id}` once `/background status` shows it finished.",
                    id = job.id
                ),
            );
        }
        Err(reason) => emit_command_output(app, reason),
    }
    Ok(CommandResult::Handled)
}

/// The reviewer's briefing: the goal, the newest user and assistant
/// messages (oldest first), each cut to [`MESSAGE_CAP`] characters, the
/// skills the agent loaded and the user's instructions. `Err` holds the
/// message to show.
pub(super) fn build_review_task(
    messages: &[Message],
    instructions: &str,
) -> Result<String, String> {
    let mut excerpt: Vec<(&str, String)> = messages
        .iter()
        .rev()
        .filter_map(|message| {
            let label = match message.role {
                MessageRole::User => "[USER]",
                MessageRole::Assistant => "[PRIMARY AGENT]",
                _ => return None,
            };
            let text = py::strip(message.content.as_deref().unwrap_or_default());
            let text = match text.char_indices().nth(MESSAGE_CAP) {
                Some((at, _)) => format!("{}{TRUNCATED}", &text[..at]),
                None => text.to_string(),
            };
            (!text.is_empty()).then_some((label, text))
        })
        .take(CONTEXT_MESSAGES)
        .collect();
    if excerpt.is_empty() {
        return Err("Nothing to review yet: the conversation is empty.".to_string());
    }
    excerpt.reverse();
    let mut lines = vec![REVIEW_GOAL, "", EXCERPT_INTRO, "", EXCERPT_START];
    for (label, text) in &excerpt {
        lines.extend([*label, text.as_str(), ""]);
    }
    lines.push(EXCERPT_END);
    let skills = loaded_skills(messages);
    let skills_line = (!skills.is_empty()).then(|| format!("The primary agent was operating under these loaded skills: {}. Before reviewing, load each with skill_view(name=...) and treat their conventions, invariants, and review standards as binding for your assessment: the work was produced under them and must be judged against them.", skills.join(", ")));
    if let Some(line) = &skills_line {
        lines.extend(["", line.as_str()]);
    }
    let instructions = py::strip(instructions);
    if !instructions.is_empty() {
        lines.extend(["", INSTRUCTIONS_INTRO, instructions]);
    }
    lines.extend(["", CLOSING]);
    Ok(lines.join("\n"))
}

/// The skills the agent loaded whole with `skill_view`, in load order,
/// each once, at most [`SKILL_LIMIT`]: upstream's
/// `collect_parent_loaded_skills`, reading each call's arguments as
/// Python's `json.loads` does and naming a skill as Python's `str()` prints
/// it. The CLI keeps no launch-preloaded marker to read. A call whose
/// `file_path` is truthy read one file of a skill, so it does not count.
fn loaded_skills(messages: &[Message]) -> Vec<String> {
    let truthy = |value: &Py| match value {
        Py::Null => false,
        Py::Bool(value) => *value,
        Py::Int(value) => *value != 0,
        Py::Float(value) => *value != 0.0,
        Py::Str(value) => !value.is_empty(),
        Py::List(value) => !value.is_empty(),
        Py::Dict(value) => !value.is_empty(),
    };
    let mut names: Vec<String> = Vec::new();
    let calls = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .flat_map(|message| message.tool_calls.iter().flatten())
        .filter(|call| call.function.name == "skill_view");
    for call in calls {
        let Ok(args) = py::loads(&call.function.arguments) else {
            continue;
        };
        if !args.is_dict() || args.get("file_path").is_some_and(truthy) {
            continue;
        }
        let Some(name) = args.get("name").filter(|name| truthy(name)) else {
            continue;
        };
        let name = py::str_of(name);
        let name = py::strip(&name);
        if !name.is_empty() && !names.iter().any(|known| known == name) {
            names.push(name.to_string());
        }
        if names.len() == SKILL_LIMIT {
            break;
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: MessageRole, text: &str) -> Message {
        let mut message = Message::system(text);
        message.role = role;
        message
    }

    #[test]
    fn the_briefing_holds_the_newest_turns_oldest_first_and_the_instructions() {
        let mut messages = vec![message(MessageRole::System, "system prompt")];
        for turn in 0..12 {
            messages.push(message(MessageRole::User, &format!("question {turn}")));
            messages.push(message(MessageRole::Tool, "tool output"));
            messages.push(message(MessageRole::Assistant, &format!("answer {turn}")));
            messages.push(message(MessageRole::Assistant, "  "));
        }
        let task = build_review_task(&messages, "  check the tests ").unwrap();
        assert!(task.starts_with(REVIEW_GOAL), "{task}");
        assert!(!task.contains("question 6\n") && !task.contains("system prompt"));
        assert!(!task.contains("tool output"));
        let excerpt = task.split(EXCERPT_START).nth(1).unwrap();
        assert!(
            excerpt.starts_with("\n[USER]\nquestion 7\n\n[PRIMARY AGENT]\nanswer 7\n"),
            "{excerpt}"
        );
        assert!(excerpt.contains("[PRIMARY AGENT]\nanswer 11\n\n--- End of conversation excerpt ---\n\nAdditional review instructions from the user:\ncheck the tests\n\n"), "{excerpt}");
        assert!(task.ends_with(CLOSING));
    }

    #[test]
    fn an_empty_conversation_has_nothing_to_review() {
        let messages = [
            message(MessageRole::System, "system prompt"),
            message(MessageRole::Assistant, " "),
        ];
        assert_eq!(
            build_review_task(&messages, ""),
            Err("Nothing to review yet: the conversation is empty.".to_string())
        );
    }

    #[test]
    fn ten_long_messages_are_all_kept_as_upstream_keeps_them() {
        // 12,000 characters each, with surrogate pairs, quotes and
        // backslashes: the task no longer travels on a command line.
        let body = "é😀\"\\".repeat(2_999) + "!!";
        let messages: Vec<_> = (0..11)
            .map(|turn| message(MessageRole::User, &format!("{turn:02}{body}")))
            .collect();
        let task = build_review_task(&messages, "").unwrap();
        for turn in 1..11 {
            assert!(
                task.contains(&format!("[USER]\n{turn:02}{body}\n")),
                "message {turn} was cut or dropped"
            );
        }
        assert!(!task.contains("[USER]\n00"), "an eleventh message was kept");
        assert!(!task.contains(TRUNCATED));
    }

    #[test]
    fn a_message_over_the_cap_keeps_its_first_12000_characters() {
        let text = "😀".repeat(MESSAGE_CAP + 1);
        let task = build_review_task(&[message(MessageRole::User, &text)], "").unwrap();
        let kept = format!("[USER]\n{}{TRUNCATED}\n", "😀".repeat(MESSAGE_CAP));
        assert!(task.contains(&kept), "the message was not cut at its cap");
        assert!(!task.contains(INSTRUCTIONS_INTRO));
    }

    #[test]
    fn long_instructions_are_kept_whole() {
        let instructions = "check \"this\" \\ ".repeat(5_000);
        let task =
            build_review_task(&[message(MessageRole::User, "question")], &instructions).unwrap();
        assert!(task.contains(&format!("{INSTRUCTIONS_INTRO}\n{}\n", instructions.trim())));
    }

    /// An assistant turn that only calls tools: `calls` holds each tool's
    /// name and its JSON arguments.
    fn tool_turn(calls: &[(&str, &str)]) -> Message {
        let mut turn = message(MessageRole::Assistant, "");
        turn.content = None;
        turn.tool_calls = Some(
            calls
                .iter()
                .map(|(name, arguments)| hermes_core::ToolCall {
                    id: String::new(),
                    function: hermes_core::FunctionCall {
                        name: (*name).to_string(),
                        arguments: (*arguments).to_string(),
                    },
                    extra_content: None,
                })
                .collect(),
        );
        turn
    }

    #[test]
    fn skills_the_agent_loaded_whole_are_listed_once_in_load_order() {
        let mut messages = vec![
            message(MessageRole::User, "fix the parser"),
            tool_turn(&[
                ("skill_view", r#"{"name":"rust"}"#),
                (
                    "skill_view",
                    r#"{"name":"docs","file_path":"references/a.md"}"#,
                ),
                ("skill_view", r#"{"name":" tdd ","file_path":""}"#),
                ("read_file", r#"{"name":"other"}"#),
                ("skill_view", "not json"),
            ]),
            tool_turn(&[("skill_view", r#"{"name":"rust"}"#)]),
            message(MessageRole::Assistant, "done"),
        ];
        let mut user_turn = tool_turn(&[("skill_view", r#"{"name":"from-user"}"#)]);
        user_turn.role = MessageRole::User;
        messages.push(user_turn);
        // The loads fall outside the excerpt, which keeps the last 10 messages.
        for n in 0..11 {
            messages.push(message(MessageRole::User, &format!("later {n}")));
        }
        let task = build_review_task(&messages, "").unwrap();
        assert!(!task.contains("fix the parser"), "{task}");
        let line = "The primary agent was operating under these loaded skills: rust, tdd. Before reviewing, load each with skill_view(name=...)";
        assert!(task.contains(&format!("{EXCERPT_END}\n\n{line}")), "{task}");
        assert_eq!(task.matches("loaded skills").count(), 1);

        let many: Vec<String> = (0..10)
            .map(|n| format!(r#"{{"name":"skill-{n}"}}"#))
            .collect();
        let calls: Vec<(&str, &str)> = many.iter().map(|a| ("skill_view", a.as_str())).collect();
        let task =
            build_review_task(&[message(MessageRole::User, "q"), tool_turn(&calls)], "").unwrap();
        assert!(task.contains("loaded skills: skill-0, skill-1, skill-2, skill-3, skill-4, skill-5, skill-6, skill-7. Before"), "{task}");

        let task = build_review_task(&[message(MessageRole::User, "q")], "").unwrap();
        assert!(!task.contains("loaded skills"));
    }

    #[test]
    fn skill_arguments_are_read_with_python_truthiness_as_upstream_reads_them() {
        let task = |arguments: &str| {
            build_review_task(
                &[
                    message(MessageRole::User, "q"),
                    tool_turn(&[("skill_view", arguments)]),
                ],
                "",
            )
            .unwrap()
        };
        for skipped in [
            r#"{"name":"rust","file_path":" "}"#,
            r#"{"name":"rust","file_path":1}"#,
            r#"{"name":"rust","file_path":["a"]}"#,
            r#"{"name":"rust","file_path":true}"#,
            r#"{"name":0}"#,
            r#"{"name":""}"#,
            r#"{"name":" "}"#,
            r#"["rust"]"#,
            "null",
            "",
        ] {
            assert!(!task(skipped).contains("loaded skills"), "{skipped}");
        }
        for (arguments, listed) in [
            (r#"{"name":42}"#, "42"),
            (r#"{"name":true}"#, "True"),
            (r#"{"name":"rust","file_path":""}"#, "rust"),
            (r#"{"name":"rust","file_path":0}"#, "rust"),
            (r#"{"name":"rust","file_path":null}"#, "rust"),
            (r#"{"name":"rust","file_path":false}"#, "rust"),
            (r#"{"name":"rust","file_path":[]}"#, "rust"),
            // Python's str() of what its json.loads reads: key order kept,
            // integers whole, floats as repr, and NaN accepted.
            (
                r#"{"name":{"b":1,"a":[null,"x"]}}"#,
                "{'b': 1, 'a': [None, 'x']}",
            ),
            (
                r#"{"name":123456789012345678901234567890}"#,
                "123456789012345678901234567890",
            ),
            (r#"{"name":0.00001}"#, "1e-05"),
            (r#"{"name":1e100}"#, "1e+100"),
            (r#"{"name":"rust","x":NaN}"#, "rust"),
            (r#"{"name":NaN}"#, "nan"),
        ] {
            assert!(
                task(arguments).contains(&format!("loaded skills: {listed}. Before")),
                "{arguments}"
            );
        }
        // Two calls upstream names alike are listed once.
        let task = build_review_task(
            &[
                message(MessageRole::User, "q"),
                tool_turn(&[
                    ("skill_view", r#"{"name":[true]}"#),
                    ("skill_view", r#"{"name":"[True]"}"#),
                ]),
            ],
            "",
        )
        .unwrap();
        assert!(task.contains("loaded skills: [True]. Before"), "{task}");
    }

    #[test]
    fn separators_python_strips_count_as_whitespace() {
        // Python's str.strip removes U+001C to U+001F; Rust's trim does not.
        let messages = [message(MessageRole::User, "\u{1c}\u{1d} \u{1e}\u{1f}")];
        assert_eq!(
            build_review_task(&messages, ""),
            Err("Nothing to review yet: the conversation is empty.".to_string())
        );
        let messages = [message(MessageRole::User, "\u{1f} question \u{1c}")];
        let task = build_review_task(&messages, "\u{1d}check\u{1e}").unwrap();
        assert!(task.contains("[USER]\nquestion\n"), "{task}");
        assert!(
            task.contains(&format!("{INSTRUCTIONS_INTRO}\ncheck\n")),
            "{task}"
        );
    }
}
