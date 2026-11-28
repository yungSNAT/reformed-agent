use std::path::Path;

use rustix::fs::{openat, statat, AtFlags, Dir, FileType, Mode, OFlags, CWD};
use serde_json::{json, Value};

use super::super::{text, RpcError};
use super::folder;
use crate::chat::Request;

/// `complete.path {word, session_id?, cwd?}`: completions under the session's folder. The
/// client's `cwd` is not used: the folder is the session's.
pub(in crate::chat::host) async fn complete_path(req: Request) -> Result<Value, RpcError> {
    let word = text(&req.params, "word", 1024)?.to_string();
    if word.is_empty() {
        return Ok(json!({"items": []}));
    }
    let root = folder()?;
    let items = tokio::task::spawn_blocking(move || items(&root, &word))
        .await
        .unwrap_or_default();
    Ok(json!({"items": items}))
}

const AT_HINTS: &[(&str, &str)] = &[
    ("@diff", "git diff"),
    ("@staged", "staged diff"),
    ("@file:", "attach file"),
    ("@folder:", "attach folder"),
    ("@url:", "fetch url"),
    ("@git:", "git log"),
];
const SKIPPED_IN_CONTEXT: &[&str] = &[".git", "node_modules", "target", "__pycache__", ".venv"];
const MAX_ITEMS: usize = 30;
const MAX_ENTRIES: usize = 5000;

fn item(text: String, meta: &str, display: String) -> Value {
    json!({"text": text, "display": display, "meta": meta})
}

/// Splits `query` into `(tag, folder parts, typed prefix)`, refusing paths outside `root`.
fn parse_word(context: bool, query: &str) -> Option<(&str, Vec<&str>, &str)> {
    let (tag, path) = match query.split_once(':') {
        Some((tag @ ("file" | "folder"), path)) if context => (tag, path),
        Some(_) if context => return None,
        _ if context && matches!(query, "file" | "folder") => (query, ""),
        _ => ("", query),
    };
    // `@/foo` means foo from here, as upstream reads it when no such absolute path exists.
    let path = if context {
        path.trim_start_matches('/')
    } else {
        path
    };
    if path.starts_with(['/', '~', '\\'])
        || path.as_bytes().get(1) == Some(&b':')
        || path.contains('\\')
    {
        return None;
    }
    let (folder, typed) = match path.rsplit_once('/') {
        Some((folder, typed)) => (folder, typed),
        None => ("", path),
    };
    let mut parts = Vec::new();
    for part in folder.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    Some((tag, parts, typed))
}

/// Upstream's listing (`_dir_listing_items`) confined to `root`: a word naming an absolute
/// path, `~`, a drive or a `..` component lists nothing, and each folder on the way is
/// opened from its parent's handle without following a link.
pub(super) fn items(root: &Path, word: &str) -> Vec<Value> {
    let context = word.starts_with('@');
    let query = if context { &word[1..] } else { word };
    if context && query.is_empty() {
        return AT_HINTS
            .iter()
            .map(|(text, meta)| item((*text).into(), meta, (*text).into()))
            .collect();
    }
    let Some((tag, parts, typed)) = parse_word(context, query) else {
        return Vec::new();
    };
    let Some(entries) = list(root, &parts) else {
        return Vec::new();
    };
    let typed_lower = typed.to_lowercase();
    let mut items = Vec::new();
    for (name, is_dir) in entries {
        if !name.to_lowercase().starts_with(&typed_lower)
            || (name.starts_with('.') && !typed.starts_with('.'))
            || (context && SKIPPED_IN_CONTEXT.contains(&name.as_str()))
            || (!tag.is_empty() && (tag == "folder") != is_dir)
        {
            continue;
        }
        items.push(entry_item(word, context, tag, &parts, &name, is_dir));
        if items.len() >= MAX_ITEMS {
            break;
        }
    }
    items
}

fn entry_item(
    word: &str,
    context: bool,
    tag: &str,
    parts: &[&str],
    name: &str,
    is_dir: bool,
) -> Value {
    let suffix = if is_dir { "/" } else { "" };
    let rel = parts
        .iter()
        .copied()
        .chain([name])
        .collect::<Vec<_>>()
        .join("/");
    let text = if context {
        let tag = match (tag, is_dir) {
            ("", true) => "folder",
            ("", false) => "file",
            (tag, _) => tag,
        };
        format!("@{tag}:{rel}{suffix}")
    } else {
        let dot = if word.starts_with("./") { "./" } else { "" };
        format!("{dot}{rel}{suffix}")
    };
    item(
        text,
        if is_dir { "dir" } else { "" },
        format!("{name}{suffix}"),
    )
}

/// The sorted `(name, is_dir)` entries of `root/parts…`, each part opened relative to the
/// one before with `O_NOFOLLOW`, so a link anywhere on the way lists nothing. A link inside
/// the folder is listed as a plain entry and not followed.
fn list(root: &Path, parts: &[&str]) -> Option<Vec<(String, bool)>> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut dir = openat(CWD, root, flags, Mode::empty()).ok()?;
    for part in parts {
        dir = openat(&dir, *part, flags | OFlags::NOFOLLOW, Mode::empty()).ok()?;
    }
    let mut entries = Vec::new();
    let reader = Dir::read_from(&dir).ok()?;
    for entry in reader.take(MAX_ENTRIES) {
        let Ok(entry) = entry else { break };
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        #[allow(clippy::unnecessary_cast, reason = "st_mode is u16 on macOS")]
        let is_dir = statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|stat| {
            FileType::from_raw_mode(stat.st_mode as rustix::fs::RawMode) == FileType::Directory
        });
        entries.push((name.to_string(), is_dir));
    }
    entries.sort();
    Some(entries)
}
