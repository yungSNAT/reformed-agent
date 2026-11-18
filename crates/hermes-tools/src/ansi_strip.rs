//! Terminal escape sequences removed from command output before the model
//! sees it.
//!
//! Port of upstream `tools/ansi_strip.py`: a model that sees colour codes in
//! tool output copies them into the files it writes. Covers ECMA-48: CSI
//! (private `?` prefix, colon parameters, intermediate bytes), OSC ended by
//! BEL or ST, DCS, SOS, PM and APC strings, nF escapes, single-byte Fp, Fe
//! and Fs escapes, and 8-bit C1 controls.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

fn escape_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"\x1b(?:",
            r"\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]", // CSI
            r"|\][\s\S]*?(?:\x07|\x1b\\)",            // OSC, BEL or ST
            r"|[PX^_][\s\S]*?\x1b\\",                 // DCS, SOS, PM, APC
            r"|[\x20-\x2f]+[\x30-\x7e]",              // nF
            r"|[\x30-\x7e]",                          // Fp, Fe, Fs
            r")",
            r"|\x{9b}[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]", // 8-bit CSI
            r"|\x{9d}[\s\S]*?(?:\x07|\x{9c})",             // 8-bit OSC
            r"|[\x{80}-\x{9f}]",                           // other C1 controls
        ))
        .expect("ANSI escape regex")
    })
}

/// `text` without its terminal escape sequences; borrowed when it has none.
pub fn strip_ansi(text: &str) -> Cow<'_, str> {
    let has_escape = text
        .chars()
        .any(|c| c == '\u{1b}' || ('\u{80}'..='\u{9f}').contains(&c));
    if !has_escape {
        return Cow::Borrowed(text);
    }
    escape_regex().replace_all(text, "")
}

/// Strip escape sequences from every string in `value`, such as a process
/// poll's output, before it is serialised: in JSON text they are escaped and
/// [`strip_ansi`] would no longer see them.
pub fn strip_ansi_in_value(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Cow::Owned(stripped) = strip_ansi(text) {
                *text = stripped;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_ansi_in_value),
        Value::Object(fields) => fields.values_mut().for_each(strip_ansi_in_value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{strip_ansi, strip_ansi_in_value};

    /// Cases from upstream `tests/tools/test_ansi_strip.py`.
    #[test]
    fn removes_every_kind_of_escape_sequence() {
        let cases = [
            ("\x1b[0m", ""),
            ("\x1b[31;1m", ""),
            ("\x1b[38;2;255;0;0m", ""),
            ("\x1b[38:2:255:0:0m", ""),
            ("\x1b[?25h\x1b[?1049l\x1b[?2004h", ""),
            ("\x1b[0 q\x1b[6 q", ""),
            ("\x1b]0;title\x07", ""),
            ("\x1b]0;title\x1b\\", ""),
            (
                "\x1b]8;;https://example.com\x1b\\click\x1b]8;;\x1b\\",
                "click",
            ),
            ("\x1b7\x1b8\x1b=\x1b>", ""),
            ("\x1bM\x1bc\x1bD\x1bE", ""),
            ("\x1b(A\x1b(B\x1b(0", ""),
            ("\x1bP+q\x1b\\", ""),
            ("\u{9b}31m\u{9b}38;2;255;0;0m", ""),
            ("\u{9c}\u{9d}\u{90}", ""),
            (
                "\x1b[32m#!/usr/bin/env python3\x1b[0m\nprint('hello')",
                "#!/usr/bin/env python3\nprint('hello')",
            ),
            ("\x1b[1m\x1b[31m\x1b[42mhello\x1b[0m", "hello"),
            (
                "def foo(\x1b[33m):\x1b[0m\n    return 42",
                "def foo():\n    return 42",
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(strip_ansi(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn strips_every_string_in_a_json_value() {
        let mut value = serde_json::json!({
            "status": "running",
            "output": "\x1b[1mbuild\x1b[0m ok",
            "lines": ["\x1b[33mwarn\x1b[0m", 3]
        });
        strip_ansi_in_value(&mut value);
        assert_eq!(
            value,
            serde_json::json!({"status": "running", "output": "build ok", "lines": ["warn", 3]})
        );
    }

    #[test]
    fn leaves_clean_text_alone() {
        for text in [
            "",
            "normal text",
            "line1\nline2\ttab",
            "emoji \u{1f389} and \u{f1} caf\u{e9}",
            "path = 'C:\\\\Users\\\\test'",
            "arr[0] = arr[31]",
        ] {
            assert!(matches!(strip_ansi(text), std::borrow::Cow::Borrowed(t) if t == text));
        }
    }
}
