//! `/review [instructions]`: an independent reviewer for the work just
//! discussed (upstream `agent/review_engine.py`). It runs as a background
//! job, as `/btw` does, with the recent conversation as its evidence.

use hermes_core::{Message, MessageRole};

use super::{emit_command_output, queue_background_job, AgentError, App, CommandResult};

/// Recent user and assistant messages the reviewer gets.
const CONTEXT_MESSAGES: usize = 10;
/// Text kept from one message. Sizes here are UTF-16 code units, as
/// Windows counts a command line, with a quote or backslash counted twice
/// for the escaping argument quoting may add.
const MESSAGE_CAP: usize = 12_000;
/// shortcut: a background job takes its task as one command-line argument,
/// and Windows caps a command line at 32,767 UTF-16 units, so the briefing
/// keeps the newest messages within this many, leaving room for the
/// program path and argument quoting. Passing the task on standard input or
/// in a file would lift the cap.
const BRIEFING_CAP: usize = 24_000;
/// Room the user's instructions must leave for the conversation.
const MIN_EXCERPT: usize = 2_000;
/// Marks a cut message; ASCII, so its length is its unit count.
const TRUNCATED: &str = "\n[... truncated ...]";

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

/// `c`'s length on a Windows command line: its UTF-16 units, plus one for a
/// quote or backslash, which quoting may escape.
fn char_units(c: char) -> usize {
    c.len_utf16() + usize::from(matches!(c, '"' | '\\'))
}

/// `text`'s length on a Windows command line, at most.
fn units(text: &str) -> usize {
    text.chars().map(char_units).sum()
}

/// The longest start of `text` within `limit` units.
fn cut(text: &str, limit: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices() {
        used += char_units(c);
        if used > limit {
            return &text[..at];
        }
    }
    text
}

/// The reviewer's briefing: the goal, the newest user and assistant
/// messages (oldest first) and the user's instructions, within
/// [`BRIEFING_CAP`] units. `Err` holds the message to show.
pub(super) fn build_review_task(
    messages: &[Message],
    instructions: &str,
) -> Result<String, String> {
    let instructions = instructions.trim();
    let mut lines = vec![REVIEW_GOAL, "", EXCERPT_INTRO, "", EXCERPT_START];
    let tail = if instructions.is_empty() {
        vec![EXCERPT_END, "", CLOSING]
    } else {
        vec![
            EXCERPT_END,
            "",
            INSTRUCTIONS_INTRO,
            instructions,
            "",
            CLOSING,
        ]
    };
    // Each line and the newline after it.
    let fixed: usize = lines.iter().chain(&tail).map(|line| units(line) + 1).sum();
    let Some(mut budget) = BRIEFING_CAP
        .checked_sub(fixed)
        .filter(|budget| *budget >= MIN_EXCERPT)
    else {
        let limit = BRIEFING_CAP + units(instructions) - fixed - MIN_EXCERPT;
        return Err(format!(
            "The review instructions are too long: keep them to {limit} characters."
        ));
    };
    let mut excerpt = Vec::new();
    for message in messages.iter().rev() {
        let label = match message.role {
            MessageRole::User => "[USER]",
            MessageRole::Assistant => "[PRIMARY AGENT]",
            _ => continue,
        };
        let text = message.content.as_deref().unwrap_or_default().trim();
        if text.is_empty() {
            continue;
        }
        // The label, the text and a blank line, each with its newline.
        let overhead = label.len() + 3;
        if excerpt.len() == CONTEXT_MESSAGES || budget < overhead + TRUNCATED.len() + 1 {
            break;
        }
        let room = MESSAGE_CAP.min(budget - overhead);
        let text = if units(text) <= room {
            text.to_string()
        } else {
            format!("{}{TRUNCATED}", cut(text, room - TRUNCATED.len()))
        };
        budget -= overhead + units(&text);
        excerpt.push((label, text));
    }
    if excerpt.is_empty() {
        return Err("Nothing to review yet: the conversation is empty.".to_string());
    }
    for (label, text) in excerpt.iter().rev() {
        lines.extend([*label, text.as_str(), ""]);
    }
    lines.extend(tail);
    Ok(lines.join("\n"))
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
    fn long_messages_are_cut_and_the_briefing_stays_within_its_cap() {
        // `é` is one UTF-16 unit and `😀` two, so the cap is not a char count.
        let messages: Vec<_> = (0..10)
            .map(|turn| message(MessageRole::User, &format!("{turn}{}", "é😀".repeat(8_000))))
            .collect();
        let task = build_review_task(&messages, "").unwrap();
        assert!(units(&task) <= BRIEFING_CAP, "{}", units(&task));
        // The newest message is kept, cut to the per-message cap without
        // splitting a character.
        let newest = task.split("[USER]\n").nth(2).unwrap();
        let newest = newest.split(TRUNCATED).next().unwrap();
        assert!(newest.starts_with("9é😀"));
        assert_eq!(units(newest), MESSAGE_CAP - TRUNCATED.len());
        assert!(
            task.contains("[USER]\n8é"),
            "the next message fills the rest"
        );
        assert!(
            !task.contains("[USER]\n7"),
            "the briefing kept more than fits"
        );
        let without = build_review_task(&messages[..1], "").unwrap();
        assert!(!without.contains(INSTRUCTIONS_INTRO));
    }

    /// `arg` quoted as Rust's standard library quotes a Windows argument:
    /// backslashes before a quote or the closing quote are doubled and each
    /// quote is escaped.
    fn windows_quoted(arg: &str) -> String {
        let mut quoted = String::from("\"");
        let mut backslashes = 0;
        for c in arg.chars() {
            if c == '\\' {
                backslashes += 1;
                continue;
            }
            let doubled = if c == '"' {
                2 * backslashes + 1
            } else {
                backslashes
            };
            quoted.push_str(&"\\".repeat(doubled));
            quoted.push(c);
            backslashes = 0;
        }
        quoted.push_str(&"\\".repeat(2 * backslashes));
        quoted.push('"');
        quoted
    }

    #[test]
    fn quotes_and_backslashes_still_fit_one_windows_command_line() {
        let messages: Vec<_> = (0..10)
            .map(|turn| {
                message(
                    MessageRole::User,
                    &format!("{turn}{}", "\\\"".repeat(8_000)),
                )
            })
            .collect();
        let task = build_review_task(&messages, "a \"b\" \\").unwrap();
        let length = windows_quoted(&task).encode_utf16().count();
        assert!(length <= BRIEFING_CAP + 2, "{length}");
        assert!(task.contains("[USER]\n9\\\""), "the newest message is kept");
    }

    #[test]
    fn instructions_too_long_for_the_briefing_are_refused() {
        let messages = [message(MessageRole::User, "question")];
        let reason = build_review_task(&messages, &"x".repeat(BRIEFING_CAP)).unwrap_err();
        assert!(
            reason.starts_with("The review instructions are too long"),
            "{reason}"
        );
        let limit: usize = reason
            .trim_end_matches(" characters.")
            .rsplit(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(build_review_task(&messages, &"x".repeat(limit)).is_ok());
        assert!(build_review_task(&messages, &"x".repeat(limit + 1)).is_err());
    }
}
