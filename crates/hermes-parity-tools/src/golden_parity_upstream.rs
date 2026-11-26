//! Static upstream slash-command extraction for `run-golden-parity-harness`.
//! Crates ban spawning Python (`runtime_source_guards`), so `hermes_cli/commands.py`
//! is parsed statically after stripping `#` comments and Python docstrings.
//!
//! Upstream's runtime `COMMANDS` table holds every `CommandDef` in the registry
//! and its aliases, except `gateway_only=True` definitions. Sources from before
//! the registry declared `COMMANDS` as a dict literal; its `"/name":` keys are
//! read only when no `CommandDef(...)` call is present.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use regex::Regex;

fn skip_quoted(src: &str, mut i: usize, quote: u8, out: &mut String) -> usize {
    let b = src.as_bytes();
    out.push(quote as char);
    i += 1;
    while i < b.len() {
        let ch = src[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
        if ch == '\\' && i < b.len() {
            let esc = src[i..].chars().next().unwrap_or('\0');
            out.push(esc);
            i += esc.len_utf8();
        } else if u32::from(ch) == u32::from(quote) {
            break;
        }
    }
    i
}

pub(crate) fn strip_py_comments_and_docstrings(src: &str) -> String {
    let b = src.as_bytes();
    let (mut out, mut i) = (String::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"\"\"\"") || b[i..].starts_with(b"'''") {
            let q = b[i];
            i += 3;
            while i + 2 < b.len() && !(b[i] == q && b[i + 1] == q && b[i + 2] == q) {
                if b[i] == b'\\' {
                    i += 1;
                    if i < b.len() {
                        i += src[i..].chars().next().map_or(1, char::len_utf8);
                    }
                } else {
                    i += src[i..].chars().next().map_or(1, char::len_utf8);
                }
            }
            i = (i + 3).min(b.len());
            out.push(' ');
        } else if b[i] == b'"' || b[i] == b'\'' {
            i = skip_quoted(src, i, b[i], &mut out);
        } else {
            let ch = src[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Byte index just past the parenthesis or bracket that closes the one at
/// `open`, skipping string literals; `None` when it never closes.
fn matching_close(src: &str, open: usize) -> Option<usize> {
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                let mut sink = String::new();
                i = skip_quoted(src, i, b[i], &mut sink);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Argument text of every `CommandDef(...)` call, parentheses excluded.
fn command_def_calls(stripped: &str) -> Vec<&str> {
    let call_re = Regex::new(r"\bCommandDef\s*\(").expect("valid regex");
    call_re
        .find_iter(stripped)
        .filter_map(|m| {
            let open = m.end() - 1;
            matching_close(stripped, open).map(|close| &stripped[open + 1..close - 1])
        })
        .collect()
}

/// `src` with the inside of every string literal blanked byte for byte, so
/// keyword searches cannot match description text and offsets still line up.
fn mask_strings(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' || b[i] == b'\'' {
            let mut sink = String::new();
            let end = skip_quoted(src, i, b[i], &mut sink);
            out.push(b[i]);
            out.extend(std::iter::repeat_n(b' ', end.saturating_sub(i + 2)));
            if end > i + 1 {
                out.push(b[end - 1]);
            }
            i = end;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Text of the string literal whose opening quote is at `at`.
fn literal_at(src: &str, at: usize) -> String {
    let mut sink = String::new();
    let end = skip_quoted(src, at, src.as_bytes()[at], &mut sink);
    src.get(at + 1..end.saturating_sub(1))
        .unwrap_or("")
        .trim()
        .to_string()
}

fn command_def_names(args: &str) -> Result<Option<Vec<String>>, String> {
    let masked = mask_strings(args);
    let gateway_re =
        Regex::new(r"\bgateway_only\s*=\s*([A-Za-z_][A-Za-z0-9_.]*)").expect("valid regex");
    if let Some(c) = gateway_re.captures(&masked) {
        match &c[1] {
            "True" => return Ok(None),
            "False" => {}
            other => return Err(format!("CommandDef gateway_only={other} is not a literal")),
        }
    }
    let positional_re = Regex::new(r#"^\s*["']"#).expect("valid regex");
    let keyword_re = Regex::new(r#"\bname\s*=\s*["']"#).expect("valid regex");
    let name_at = positional_re
        .find(&masked)
        .or_else(|| keyword_re.find(&masked))
        .map(|m| m.end() - 1)
        .ok_or_else(|| format!("CommandDef call without a literal name: {}", args.trim()))?;
    let mut names = vec![literal_at(args, name_at)];
    let alias_re = Regex::new(r"\baliases\s*=\s*(\S)").expect("valid regex");
    if let Some(c) = alias_re.captures(&masked) {
        let open = c.get(1).map_or(0, |m| m.start());
        if !matches!(&c[1], "(" | "[") {
            return Err(format!(
                "CommandDef aliases are not a literal: {}",
                args.trim()
            ));
        }
        let close = matching_close(&masked, open)
            .ok_or_else(|| format!("unclosed CommandDef aliases: {}", args.trim()))?;
        let mut i = open;
        while i < close {
            let byte = masked.as_bytes()[i];
            if byte == b'"' || byte == b'\'' {
                names.push(literal_at(args, i));
                let mut sink = String::new();
                i = skip_quoted(&masked, i, byte, &mut sink);
            } else {
                i += 1;
            }
        }
    }
    Ok(Some(names))
}

/// Slash commands upstream's runtime `COMMANDS` table holds, read from source.
pub(crate) fn extract_runtime_commands(source: &str) -> Result<BTreeSet<String>, String> {
    let stripped = strip_py_comments_and_docstrings(source);
    let calls = command_def_calls(&stripped);
    if calls.is_empty() {
        let dict_re = Regex::new(r#"["'](/[A-Za-z0-9_-]+)["']\s*:"#).expect("valid regex");
        return Ok(dict_re
            .captures_iter(&stripped)
            .map(|c| c[1].to_string())
            .collect());
    }
    let mut cmds = BTreeSet::new();
    for args in calls {
        for name in command_def_names(args)?.unwrap_or_default() {
            cmds.insert(format!("/{name}"));
        }
    }
    Ok(cmds)
}

pub fn parse_upstream_commands(upstream_root: &Path) -> Result<BTreeSet<String>, String> {
    let commands_py = upstream_root.join("hermes_cli/commands.py");
    let raw = fs::read_to_string(&commands_py)
        .map_err(|e| format!("failed to read upstream {}: {e}", commands_py.display()))?;
    let cmds =
        extract_runtime_commands(&raw).map_err(|e| format!("{}: {e}", commands_py.display()))?;
    if cmds.is_empty() {
        return Err(format!(
            "failed to extract any COMMANDS from {}",
            commands_py.display()
        ));
    }
    Ok(cmds)
}

#[cfg(test)]
mod tests {
    use super::extract_runtime_commands;

    fn names(src: &str) -> Vec<String> {
        extract_runtime_commands(src)
            .expect("extract")
            .into_iter()
            .collect()
    }

    fn set(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn registry_skips_gateway_only_definitions_and_keeps_aliases() {
        let src = r#"
COMMAND_REGISTRY: list[CommandDef] = [
    CommandDef("start", "Platform pings (x)", "Session",
               gateway_only=True, busy_policy="dispatch"),
    CommandDef("new", "Start a new session (fresh)", "Session",
               aliases=("reset",), args_hint="[name]"),
    CommandDef("sethome", "Set home", "Session",
               gateway_only=True, aliases=("set-home",)),
    CommandDef("clear", "Clear (screen)", "Session", cli_only=True, gateway_only=False),
    CommandDef(description="d", name="Upper.Case", category="c", aliases=["uc", "/u2"]),
]
"#;
        // Runtime keys are f"/{name}", so an alias written "/u2" becomes "//u2".
        assert_eq!(
            names(src),
            set(&["//u2", "/Upper.Case", "/clear", "/new", "/reset", "/uc"])
        );
    }

    #[test]
    fn keywords_inside_description_strings_do_not_count() {
        let src = r#"
COMMAND_REGISTRY = [
    CommandDef("x", "Set gateway_only=True to hide", "Info"),
    CommandDef("y", "uses aliases=('fake',) in text", "Info", aliases=("why",)),
    CommandDef("z", 'name="nope" here', "Info"),
]
"#;
        assert_eq!(names(src), set(&["/why", "/x", "/y", "/z"]));
    }

    #[test]
    fn non_literal_arguments_fail_loudly() {
        for src in [
            "R = [CommandDef(\"a\", \"d\", \"c\", aliases=_ALIASES)]",
            "R = [CommandDef(\"a\", \"d\", \"c\", gateway_only=_FLAG)]",
            "R = [CommandDef(NAME, \"d\", \"c\")]",
        ] {
            assert!(extract_runtime_commands(src).is_err(), "{src}");
        }
    }

    #[test]
    fn comments_docstrings_and_unrelated_dicts_are_ignored() {
        let src = r#"
"""CommandDef("doc_cmd", "d", "c")"""
# CommandDef("comment_cmd", "d", "c")
SUBCOMMANDS = {"/new": ["a"]}
COMMAND_REGISTRY = [CommandDef("help", "Show help (é)", "Info")]
"#;
        assert_eq!(names(src), set(&["/help"]));
    }

    #[test]
    fn legacy_dict_literal_is_read_without_a_registry() {
        let src = "COMMANDS = {\n    \"/help\": \"Show help\",\n    '/quit': 'Exit',\n}\n";
        assert_eq!(names(src), set(&["/help", "/quit"]));
    }
}
