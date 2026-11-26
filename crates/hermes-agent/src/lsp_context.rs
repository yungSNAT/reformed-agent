//! LSP-style context injection hooks.
//!
//! This module augments the conversation with diagnostics + symbol/reference
//! hints after file tool calls. It is designed to be lightweight and
//! deterministic, while still giving the model immediate code-intel feedback.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use hermes_core::{ToolCall, ToolResult};

use crate::code_index::{CodeIndex, SymbolInfo};

const DEFAULT_MAX_CHARS: usize = 2_800;
const DEFAULT_MAX_REF_HITS: usize = 12;
const DEFAULT_MAX_SYMBOLS_PER_FILE: usize = 10;

#[derive(Debug, Clone)]
pub struct LspContextConfig {
    pub enabled: bool,
    pub diagnostics_on_write: bool,
    pub hover_on_read: bool,
    pub references_on_rename: bool,
    pub max_chars: usize,
    pub max_reference_hits: usize,
    pub max_symbols_per_file: usize,
}

impl Default for LspContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            diagnostics_on_write: true,
            hover_on_read: true,
            references_on_rename: true,
            max_chars: DEFAULT_MAX_CHARS,
            max_reference_hits: DEFAULT_MAX_REF_HITS,
            max_symbols_per_file: DEFAULT_MAX_SYMBOLS_PER_FILE,
        }
    }
}

impl LspContextConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("HERMES_LSP_CONTEXT_ENABLED", true),
            diagnostics_on_write: env_bool("HERMES_LSP_DIAGNOSTICS_ON_WRITE", true),
            hover_on_read: env_bool("HERMES_LSP_HOVER_ON_READ", true),
            references_on_rename: env_bool("HERMES_LSP_REFERENCES_ON_RENAME", true),
            max_chars: env_usize("HERMES_LSP_CONTEXT_MAX_CHARS", DEFAULT_MAX_CHARS).max(400),
            max_reference_hits: env_usize("HERMES_LSP_REFERENCE_HITS", DEFAULT_MAX_REF_HITS)
                .clamp(1, 100),
            max_symbols_per_file: env_usize(
                "HERMES_LSP_SYMBOLS_PER_FILE",
                DEFAULT_MAX_SYMBOLS_PER_FILE,
            )
            .clamp(1, 40),
        }
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| {
            !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

/// Build a compact LSP-style context block from successful file-tool calls.
pub fn build_lsp_context_note(
    tool_calls: &[ToolCall],
    results: &[ToolResult],
    code_index: &mut CodeIndex,
    cfg: &LspContextConfig,
) -> Option<String> {
    if !cfg.enabled {
        return None;
    }

    let mut touched_paths = BTreeSet::<PathBuf>::new();
    let mut sections = Vec::<String>::new();

    for result in results {
        if result.is_error {
            continue;
        }
        let Some(call) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
            continue;
        };
        let Ok(args) = serde_json::from_str::<Value>(&call.function.arguments) else {
            continue;
        };

        match call.function.name.as_str() {
            "read_file" if cfg.hover_on_read => {
                if let Some(path) = parse_path_arg(&args) {
                    touched_paths.insert(path.clone());
                    if let Some(section) =
                        build_hover_section(code_index, &path, args.get("offset"), cfg)
                    {
                        sections.push(section);
                    }
                }
            }
            "write_file" | "patch_file" | "edit_file" => {
                if let Some(path) = parse_path_arg(&args) {
                    touched_paths.insert(path.clone());
                    if cfg.diagnostics_on_write {
                        if let Some(diag) = build_diagnostics_section(&path) {
                            sections.push(diag);
                        }
                    }
                    if cfg.references_on_rename && call.function.name == "patch_file" {
                        if let Some(refs) = build_reference_section(code_index, &args, cfg) {
                            sections.push(refs);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !touched_paths.is_empty() {
        code_index.refresh_paths(touched_paths);
    }
    if sections.is_empty() {
        return None;
    }

    let mut out = String::from("## LSP Context Injection\n");
    for section in sections {
        out.push_str(&section);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Some(truncate(&out, cfg.max_chars))
}

fn parse_path_arg(args: &Value) -> Option<PathBuf> {
    args.get("path").and_then(|v| v.as_str()).map(PathBuf::from)
}

fn build_hover_section(
    code_index: &CodeIndex,
    path: &Path,
    read_offset: Option<&Value>,
    cfg: &LspContextConfig,
) -> Option<String> {
    let symbols = code_index.list_file_symbols(path, cfg.max_symbols_per_file);
    if symbols.is_empty() {
        return None;
    }
    let hint_line = read_offset
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .map(|v| v as usize);

    let focus = nearest_symbol(&symbols, hint_line);
    let mut block = format!("- Hover [{}]\n", path.display());
    if let Some(sym) = focus {
        block.push_str(&format!(
            "  - focus: {} {} (line {})\n",
            sym.kind, sym.name, sym.line
        ));
    }
    block.push_str("  - top symbols:");
    for sym in symbols.iter().take(5) {
        block.push_str(&format!(" {}:{}@{},", sym.kind, sym.name, sym.line));
    }
    if block.ends_with(',') {
        block.pop();
    }
    block.push('\n');
    Some(block)
}

fn nearest_symbol(symbols: &[SymbolInfo], line: Option<usize>) -> Option<&SymbolInfo> {
    let Some(target) = line else {
        return symbols.first();
    };
    symbols
        .iter()
        .filter(|s| s.line <= target)
        .max_by_key(|s| s.line)
        .or_else(|| symbols.first())
}

fn build_diagnostics_section(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some(diagnose_rust(path)),
        "py" => Some(diagnose_python(path)),
        _ => None,
    }
}

fn diagnose_rust(path: &Path) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return format!(
                "- Diagnostics [{}]\n  - rust: failed to read file: {}\n",
                path.display(),
                e
            );
        }
    };
    match syn::parse_file(&content) {
        Ok(_) => format!("- Diagnostics [{}]\n  - rust syntax: ok\n", path.display()),
        Err(e) => format!(
            "- Diagnostics [{}]\n  - rust syntax: error: {}\n",
            path.display(),
            e
        ),
    }
}

fn diagnose_python(path: &Path) -> String {
    format!(
        "- Diagnostics [{}]\n  - python syntax: skipped (Rust-only runtime does not spawn Python)\n",
        path.display()
    )
}

fn build_reference_section(
    code_index: &CodeIndex,
    args: &Value,
    cfg: &LspContextConfig,
) -> Option<String> {
    let old = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let new = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if old.is_empty() || new.is_empty() || old == new {
        return None;
    }
    if !looks_like_identifier(old) || !looks_like_identifier(new) {
        return None;
    }
    let refs = code_index.find_references(old, cfg.max_reference_hits);
    if refs.is_empty() {
        return None;
    }
    let mut block = format!("- References before rename `{old}` -> `{new}`\n");
    for hit in refs {
        block.push_str(&format!(
            "  - {}:{}: {}\n",
            hit.path.display(),
            hit.line,
            truncate(hit.snippet.trim(), 120)
        ));
    }
    Some(block)
}

fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out = input.chars().take(max_chars).collect::<String>();
    out.push_str("...");
    out
}

fn looks_like_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_index::{CodeIndex, CodeIndexConfig};
    use hermes_core::FunctionCall;
    use serde_json::json;

    #[test]
    fn rename_reference_section_uses_identifier_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("m.rs");
        std::fs::write(&file, "fn old_name() {}\nlet x = old_name();\n").unwrap();
        let mut index = CodeIndex::new(tmp.path(), CodeIndexConfig::default());
        index.refresh_full();
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "old_string": "old_name",
            "new_string": "new_name"
        });
        let cfg = LspContextConfig::default();
        let section = build_reference_section(&index, &args, &cfg);
        assert!(section.unwrap_or_default().contains("old_name"));
    }

    #[test]
    fn file_tool_notes_show_hover_diagnostics_and_rename_references() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("sample.rs");
        let added = tmp.path().join("added.rs");
        let broken = tmp.path().join("broken.rs");
        let missing = tmp.path().join("missing.rs");
        let python = tmp.path().join("sample.py");
        std::fs::write(&source, "fn initial() {}\n\nfn target() {}\n").unwrap();
        let mut index = CodeIndex::new(tmp.path(), CodeIndexConfig::default());
        index.refresh_full();
        std::fs::write(&added, "fn new_symbol() {}\n").unwrap();
        std::fs::write(&broken, "fn incomplete(\n").unwrap();
        std::fs::write(&python, "def fixture(): pass\n").unwrap();
        assert!(index.list_file_symbols(&added, 5).is_empty());

        let call = |id: &str, name: &str, arguments: Value| ToolCall {
            id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.to_string(),
            },
            extra_content: None,
        };
        let calls = [
            call("read", "read_file", json!({"path": source, "offset": "3"})),
            call("write", "write_file", json!({"path": added})),
            call("invalid", "edit_file", json!({"path": broken})),
            call("unreadable", "write_file", json!({"path": missing})),
            call("python", "edit_file", json!({"path": python})),
            call(
                "rename",
                "patch_file",
                json!({"path": source, "old_string": "target", "new_string": "replacement"}),
            ),
        ];
        let results = calls
            .iter()
            .map(|tc| ToolResult::ok(&tc.id, "fixture"))
            .collect::<Vec<_>>();
        let cfg = LspContextConfig::default();
        let note = build_lsp_context_note(&calls, &results, &mut index, &cfg).unwrap();
        for line in [
            "## LSP Context Injection",
            "focus: function target (line 3)",
            "rust syntax: ok",
            "rust syntax: error:",
            "rust: failed to read file:",
            "python syntax: skipped",
        ] {
            assert!(note.contains(line), "{line} missing from {note}");
        }
        let (_, references) = note
            .split_once("References before rename `target` -> `replacement`")
            .expect("rename references section");
        let expected_hit = format!("{}:3: fn target() {{}}", source.display());
        assert!(references.contains(&expected_hit), "{references}");
        assert_eq!(index.list_file_symbols(&added, 1)[0].name, "new_symbol");
        assert!(build_lsp_context_note(
            &calls,
            &results,
            &mut index,
            &LspContextConfig {
                enabled: false,
                ..cfg.clone()
            }
        )
        .is_none());
        let truncated = build_lsp_context_note(
            &calls,
            &results,
            &mut index,
            &LspContextConfig {
                max_chars: 30,
                ..cfg
            },
        )
        .unwrap();
        assert_eq!(
            truncated,
            format!("{}...", note.chars().take(30).collect::<String>())
        );
    }

    #[test]
    fn file_tool_notes_skip_failed_unmatched_and_unusable_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let mut index = CodeIndex::new(tmp.path(), CodeIndexConfig::default());
        let failed = json!({"path": tmp.path().join("failed.rs")}).to_string();
        let unindexed = json!({"path": tmp.path().join("unindexed.rs")}).to_string();
        let non_source = json!({"path": tmp.path().join("fixture.txt")}).to_string();
        let calls = [
            ("failed", "write_file", failed.as_str()),
            ("malformed", "read_file", "{"),
            ("no-path", "read_file", r#"{"path":null}"#),
            ("unindexed", "read_file", unindexed.as_str()),
            ("non-source", "write_file", non_source.as_str()),
            ("other-tool", "search", "{}"),
        ]
        .map(|(id, name, arguments)| ToolCall {
            id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
            extra_content: None,
        });
        let mut results = calls[1..]
            .iter()
            .map(|tc| ToolResult::ok(&tc.id, "fixture"))
            .collect::<Vec<_>>();
        results.push(ToolResult::err("failed", "fixture write failure"));
        results.push(ToolResult::ok("unmatched", "fixture"));
        assert!(
            build_lsp_context_note(&calls, &results, &mut index, &LspContextConfig::default())
                .is_none()
        );
    }
}
