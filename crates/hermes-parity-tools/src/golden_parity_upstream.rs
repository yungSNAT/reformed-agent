//! Static upstream slash-command extraction for `run-golden-parity-harness`.
//! Crates ban spawning Python (`runtime_source_guards`), so `hermes_cli/commands.py`
//! is parsed statically after stripping `#` comments and Python docstrings.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use regex::Regex;

use crate::slash_gate::extract_upstream_slash_commands;

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
                i += if b[i] == b'\\' && i + 1 < b.len() {
                    2
                } else {
                    1
                };
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

pub fn parse_upstream_commands(upstream_root: &Path) -> Result<BTreeSet<String>, String> {
    let commands_py = upstream_root.join("hermes_cli/commands.py");
    let raw = fs::read_to_string(&commands_py)
        .map_err(|e| format!("failed to read upstream {}: {e}", commands_py.display()))?;
    let stripped = strip_py_comments_and_docstrings(&raw);
    let mut cmds = extract_upstream_slash_commands(&stripped);
    let def_re = Regex::new(r#"(?s)CommandDef\s*\(\s*["']([a-z0-9_-]+)["']"#).expect("valid regex");
    let alias_re =
        Regex::new(r#"(?s)aliases\s*=\s*(?:\(([^)]*)\)|\[([^\]]*)\])"#).expect("valid regex");
    let qstr_re = Regex::new(r#"["']/?([a-z0-9_-]+)["']"#).expect("valid regex");
    let dict_re = Regex::new(r#"["'](/[a-zA-Z0-9_-]+)["']\s*:"#).expect("valid regex");
    for cap in def_re.captures_iter(&stripped) {
        cmds.insert(format!("/{}", cap[1].trim()));
    }
    for cap in alias_re.captures_iter(&stripped) {
        let group = cap.get(1).or_else(|| cap.get(2)).map_or("", |m| m.as_str());
        for q in qstr_re.captures_iter(group) {
            cmds.insert(format!("/{}", q[1].trim()));
        }
    }
    for cap in dict_re.captures_iter(&stripped) {
        cmds.insert(cap[1].trim().to_string());
    }
    if cmds.is_empty() {
        return Err(format!(
            "failed to extract any COMMANDS from {}",
            commands_py.display()
        ));
    }
    Ok(cmds)
}
