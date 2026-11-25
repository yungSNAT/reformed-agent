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

fn command_def_names(args: &str) -> Vec<String> {
    let name_re =
        Regex::new(r#"^\s*(?:name\s*=\s*)?["']([A-Za-z0-9_-]+)["']"#).expect("valid regex");
    let Some(name) = name_re.captures(args).map(|c| c[1].to_string()) else {
        return Vec::new();
    };
    let mut names = vec![name];
    let alias_re = Regex::new(r"\baliases\s*=\s*[(\[]").expect("valid regex");
    let qstr_re = Regex::new(r#"["']/?([A-Za-z0-9_-]+)["']"#).expect("valid regex");
    if let Some(m) = alias_re.find(args) {
        let open = m.end() - 1;
        if let Some(close) = matching_close(args, open) {
            names.extend(
                qstr_re
                    .captures_iter(&args[open..close])
                    .map(|q| q[1].to_string()),
            );
        }
    }
    names
}

/// Slash commands upstream's runtime `COMMANDS` table holds, read from source.
pub(crate) fn extract_runtime_commands(source: &str) -> BTreeSet<String> {
    let stripped = strip_py_comments_and_docstrings(source);
    let gateway_only_re = Regex::new(r"\bgateway_only\s*=\s*True\b").expect("valid regex");
    let calls = command_def_calls(&stripped);
    if calls.is_empty() {
        let dict_re = Regex::new(r#"["'](/[A-Za-z0-9_-]+)["']\s*:"#).expect("valid regex");
        return dict_re
            .captures_iter(&stripped)
            .map(|c| c[1].to_string())
            .collect();
    }
    calls
        .into_iter()
        .filter(|args| !gateway_only_re.is_match(args))
        .flat_map(command_def_names)
        .map(|name| format!("/{name}"))
        .collect()
}

pub fn parse_upstream_commands(upstream_root: &Path) -> Result<BTreeSet<String>, String> {
    let commands_py = upstream_root.join("hermes_cli/commands.py");
    let raw = fs::read_to_string(&commands_py)
        .map_err(|e| format!("failed to read upstream {}: {e}", commands_py.display()))?;
    let cmds = extract_runtime_commands(&raw);
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
    CommandDef("clear", "Clear (screen)", "Session", cli_only=True),
    CommandDef(name="Upper_Case", description="d", category="c", aliases=["uc", "/u2"]),
]
"#;
        let got: Vec<String> = extract_runtime_commands(src).into_iter().collect();
        assert_eq!(
            got,
            set(&["/Upper_Case", "/clear", "/new", "/reset", "/u2", "/uc"])
        );
    }

    #[test]
    fn comments_docstrings_and_unrelated_dicts_are_ignored() {
        let src = r#"
"""CommandDef("doc_cmd", "d", "c")"""
# CommandDef("comment_cmd", "d", "c")
SUBCOMMANDS = {"/new": ["a"]}
COMMAND_REGISTRY = [CommandDef("help", "Show help (é)", "Info")]
"#;
        let got: Vec<String> = extract_runtime_commands(src).into_iter().collect();
        assert_eq!(got, set(&["/help"]));
    }

    #[test]
    fn legacy_dict_literal_is_read_without_a_registry() {
        let src = "COMMANDS = {\n    \"/help\": \"Show help\",\n    '/quit': 'Exit',\n}\n";
        let got: Vec<String> = extract_runtime_commands(src).into_iter().collect();
        assert_eq!(got, set(&["/help", "/quit"]));
    }
}
