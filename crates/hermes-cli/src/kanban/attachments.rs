use std::path::{Path, PathBuf};

use hermes_core::AgentError;

use super::{
    normalize_optional, now_rfc3339, slugify_board_id, KanbanAttachment, KanbanBoard, KanbanTask,
    KANBAN_STATE_DIR,
};

const KANBAN_ATTACHMENTS_ENV: &str = "HERMES_KANBAN_ATTACHMENTS_ROOT";

pub fn attachments_root(board: &KanbanBoard) -> PathBuf {
    if let Ok(override_root) = std::env::var(KANBAN_ATTACHMENTS_ENV) {
        let trimmed = override_root.trim();
        if !trimmed.is_empty() {
            return expand_tilde_path(trimmed);
        }
    }
    hermes_config::hermes_home()
        .join(KANBAN_STATE_DIR)
        .join("attachments")
        .join(slugify_board_id(&board.id))
}

pub fn task_attachments_dir(board: &KanbanBoard, task_id: &str) -> PathBuf {
    attachments_root(board).join(sanitize_path_segment(task_id))
}

pub fn add_attachment_to_task(
    board: &mut KanbanBoard,
    task_ref: &str,
    source_path: impl AsRef<Path>,
    uploaded_by: Option<String>,
) -> Result<KanbanAttachment, AgentError> {
    let task_idx = board
        .tasks
        .iter()
        .position(|task| {
            task.id.eq_ignore_ascii_case(task_ref) || task.title.eq_ignore_ascii_case(task_ref)
        })
        .ok_or_else(|| AgentError::Config(format!("Task not found: {task_ref}")))?;
    let source = expand_tilde_path(source_path.as_ref().to_string_lossy().as_ref());
    let metadata = std::fs::metadata(&source).map_err(|e| {
        AgentError::Io(format!(
            "read attachment {} failed: {}",
            source.display(),
            e
        ))
    })?;
    if !metadata.is_file() {
        return Err(AgentError::Config(format!(
            "Attachment source is not a file: {}",
            source.display()
        )));
    }
    let raw_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AgentError::Config("attachment filename is required".to_string()))?;
    let safe_name = sanitize_filename(raw_name);
    if safe_name.is_empty() {
        return Err(AgentError::Config(
            "attachment filename is required".to_string(),
        ));
    }

    let task_id = board.tasks[task_idx].id.clone();
    let dest_dir = task_attachments_dir(board, &task_id);
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| AgentError::Io(format!("create {} failed: {}", dest_dir.display(), e)))?;
    let dest = unique_attachment_path(&dest_dir, &safe_name);
    std::fs::copy(&source, &dest).map_err(|e| {
        AgentError::Io(format!(
            "copy attachment {} -> {} failed: {}",
            source.display(),
            dest.display(),
            e
        ))
    })?;
    let stored_path = dest.canonicalize().unwrap_or(dest);
    let stored_name = stored_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&safe_name)
        .to_string();
    let attachment = KanbanAttachment {
        id: next_attachment_id(&board.tasks[task_idx]),
        filename: stored_name,
        stored_path: stored_path.display().to_string(),
        content_type: guess_content_type(&source),
        size: metadata.len(),
        uploaded_by: normalize_optional(uploaded_by),
        created_at: now_rfc3339(),
    };
    let now = now_rfc3339();
    board.tasks[task_idx].attachments.push(attachment.clone());
    board.tasks[task_idx].updated_at = now.clone();
    board.updated_at = now;
    Ok(attachment)
}

pub fn remove_attachment_from_task(
    board: &mut KanbanBoard,
    task_ref: &str,
    attachment_ref: &str,
) -> Result<Option<KanbanAttachment>, AgentError> {
    let Some(task_idx) = board.tasks.iter().position(|task| {
        task.id.eq_ignore_ascii_case(task_ref) || task.title.eq_ignore_ascii_case(task_ref)
    }) else {
        return Ok(None);
    };
    let Some(att_idx) = board.tasks[task_idx]
        .attachments
        .iter()
        .position(|attachment| {
            attachment.id.eq_ignore_ascii_case(attachment_ref)
                || attachment.filename.eq_ignore_ascii_case(attachment_ref)
        })
    else {
        return Ok(None);
    };
    let attachment = board.tasks[task_idx].attachments.remove(att_idx);
    let stored = PathBuf::from(&attachment.stored_path);
    if stored.is_file() {
        let _ = std::fs::remove_file(&stored);
    }
    let now = now_rfc3339();
    board.tasks[task_idx].updated_at = now.clone();
    board.updated_at = now;
    Ok(Some(attachment))
}

fn expand_tilde_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

fn sanitize_path_segment(raw: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .trim_matches('_')
        .to_string();
    if sanitized.is_empty() {
        "item".to_string()
    } else {
        sanitized
    }
}

fn sanitize_filename(raw: &str) -> String {
    sanitize_path_segment(
        Path::new(raw)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(raw),
    )
}

fn unique_attachment_path(dir: &Path, filename: &str) -> PathBuf {
    let base = Path::new(filename);
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment");
    let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("");
    let first = dir.join(filename);
    if !first.exists() {
        return first;
    }
    for idx in 1..10_000 {
        let candidate_name = if ext.is_empty() {
            format!("{stem}-{idx}")
        } else {
            format!("{stem}-{idx}.{ext}")
        };
        let candidate = dir.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{}-{}", stem, uuid::Uuid::new_v4().simple()))
}

fn next_attachment_id(task: &KanbanTask) -> String {
    let mut max_seen = 0u64;
    for attachment in &task.attachments {
        if let Some(raw) = attachment.id.strip_prefix("A-") {
            if let Ok(value) = raw.parse::<u64>() {
                max_seen = max_seen.max(value);
            }
        }
    }
    format!("A-{:04}", max_seen.saturating_add(1))
}

fn guess_content_type(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let content_type = match ext.as_str() {
        "txt" | "md" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "json" | "yaml" | "yml"
        | "toml" => "text/plain",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        _ => return None,
    };
    Some(content_type.to_string())
}
