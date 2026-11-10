use std::collections::HashSet;
use std::fs::{self, File, FileTimes};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use hermes_core::{AgentError, FunctionCall, Message, MessageRole, ToolCall};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;

/// Counts for an import of Hermes JSONL conversation snapshots.
#[derive(Debug, Default)]
pub struct HermesHistorySummary {
    pub sessions: usize,
    pub messages: usize,
    pub excluded_rows: usize,
}

#[derive(Deserialize)]
struct ExportSession {
    id: String,
    started_at: f64,
    ended_at: Option<f64>,
    last_activity_at: Option<f64>,
    parent_session_id: Option<String>,
    title: Option<String>,
    model: Option<String>,
    source: Option<String>,
    system_prompt: Option<String>,
    messages: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ExportToolCall {
    Nested {
        id: String,
        function: FunctionCall,
        extra_content: Option<Value>,
    },
    Flat(ToolCall),
}

impl From<ExportToolCall> for ToolCall {
    fn from(value: ExportToolCall) -> Self {
        match value {
            ExportToolCall::Nested {
                id,
                function,
                extra_content,
            } => Self {
                id,
                function,
                extra_content,
            },
            ExportToolCall::Flat(call) => call,
        }
    }
}

fn safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        && !id.eq_ignore_ascii_case("latest")
        && !windows_device_name(id)
}

fn windows_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_lowercase();
    matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) || stem
        .strip_prefix("com")
        .or_else(|| stem.strip_prefix("lpt"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn epoch_time(seconds: f64) -> Option<SystemTime> {
    let elapsed = Duration::try_from_secs_f64(seconds).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(elapsed)
}

fn epoch_text(seconds: f64) -> Option<String> {
    let elapsed = Duration::try_from_secs_f64(seconds).ok()?;
    chrono::DateTime::from_timestamp(
        i64::try_from(elapsed.as_secs()).ok()?,
        elapsed.subsec_nanos(),
    )
    .map(|time| time.to_rfc3339())
}

fn convert_message(mut row: Value) -> Result<Option<Message>, &'static str> {
    match row.get("active") {
        Some(Value::Bool(false)) => return Ok(None),
        Some(Value::Number(active)) if active.as_i64() == Some(0) => return Ok(None),
        None | Some(Value::Null | Value::Bool(true)) => {}
        Some(Value::Number(active)) if active.as_i64() == Some(1) => {}
        _ => return Err("invalid active flag"),
    }
    if row.get("role").and_then(Value::as_str) == Some("session_meta") {
        return Ok(None);
    }
    let fields = row.as_object_mut().ok_or("invalid message")?;
    if let Some(tool_name) = fields.remove("tool_name") {
        fields.entry("name").or_insert(tool_name);
    }
    if let Some(calls) = fields
        .get_mut("tool_calls")
        .filter(|calls| !calls.is_null())
    {
        let decoded: Vec<ExportToolCall> =
            serde_json::from_value(calls.take()).map_err(|_| "invalid tool calls")?;
        let decoded: Vec<ToolCall> = decoded.into_iter().map(ToolCall::from).collect();
        if decoded
            .iter()
            .any(|call| call.id.is_empty() || call.function.name.is_empty())
        {
            return Err("tool call missing ID or function name");
        }
        *calls = json!(decoded);
    }
    let message: Message = serde_json::from_value(row)
        .map_err(|_| "message cannot be represented as a Reformed transcript")?;
    match message.role {
        MessageRole::Tool if message.tool_call_id.as_deref().is_none_or(str::is_empty) => {
            return Err("tool result missing call ID");
        }
        MessageRole::Tool if message.content.is_none() => {
            return Err("tool result missing text content");
        }
        MessageRole::Assistant
            if message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty()) => {}
        MessageRole::Tool => {}
        _ if message.content.as_deref().is_none_or(str::is_empty) => {
            return Err("active message missing text or tool calls");
        }
        _ => {}
    }
    Ok(Some(message))
}

fn validate_tool_history(messages: &[Message]) -> Result<(), &'static str> {
    let mut used = HashSet::new();
    let mut pending = HashSet::new();
    for message in messages {
        let calls = message.tool_calls.as_deref().unwrap_or_default();
        if message.role != MessageRole::Assistant && !calls.is_empty() {
            return Err("tool calls must come from an assistant");
        }
        if message.role == MessageRole::Tool {
            let id = message
                .tool_call_id
                .as_deref()
                .ok_or("tool result missing call ID")?;
            if !pending.remove(id) {
                return Err("tool result without a matching pending call");
            }
            continue;
        }
        if !pending.is_empty() {
            return Err("tool call missing a result before the next turn");
        }
        for call in calls {
            if !used.insert(call.id.as_str()) {
                return Err("duplicate tool call ID");
            }
            pending.insert(call.id.as_str());
        }
    }
    if !pending.is_empty() {
        return Err("tool call missing a result at the end of the session");
    }
    Ok(())
}

fn import_records(input: File, sessions: &Path) -> Result<HermesHistorySummary, AgentError> {
    let mut summary = HermesHistorySummary::default();
    let mut reader = BufReader::new(input);
    let mut bytes = Vec::new();
    let mut line = 0;
    let mut ids = HashSet::new();
    loop {
        bytes.clear();
        let read = reader
            .by_ref()
            .take(MAX_RECORD_BYTES + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(io_error)?;
        if read == 0 {
            break;
        }
        line += 1;
        let invalid =
            |reason: &str| AgentError::Config(format!("Hermes export line {line}: {reason}"));
        if read as u64 > MAX_RECORD_BYTES {
            return Err(invalid("session exceeds the 64 MiB record limit"));
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let exported: ExportSession =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid session record"))?;
        if !safe_session_id(&exported.id) || !ids.insert(exported.id.to_ascii_lowercase()) {
            return Err(invalid("invalid or duplicate session ID"));
        }
        let created_at =
            epoch_text(exported.started_at).ok_or_else(|| invalid("invalid start time"))?;
        let modified = epoch_time(
            exported
                .last_activity_at
                .or(exported.ended_at)
                .unwrap_or(exported.started_at),
        )
        .ok_or_else(|| invalid("invalid activity time"))?;
        let mut messages = Vec::with_capacity(exported.messages.len());
        for row in exported.messages {
            match convert_message(row).map_err(invalid)? {
                Some(message) => messages.push(message),
                None => summary.excluded_rows += 1,
            }
        }
        validate_tool_history(&messages).map_err(invalid)?;
        let snapshot = json!({
            "session_info": {
                "session_id": exported.id,
                "created_at": created_at,
                "model": exported.model,
                "title": exported.title,
                "source": exported.source,
                "parent_session_id": exported.parent_session_id,
                "message_count": messages.len()
            },
            "system_prompt": exported.system_prompt,
            "messages": messages,
        });
        let path = sessions.join(format!("{}.json", exported.id));
        let file = File::create_new(path).map_err(io_error)?;
        let mut writer = BufWriter::new(&file);
        serde_json::to_writer(&mut writer, &snapshot)
            .map_err(|e| AgentError::Io(format!("Could not write session snapshot: {e}")))?;
        writer.flush().map_err(io_error)?;
        file.set_times(FileTimes::new().set_modified(modified))
            .map_err(io_error)?;
        summary.sessions += 1;
        summary.messages += messages.len();
    }
    Ok(summary)
}

fn io_error(error: io::Error) -> AgentError {
    AgentError::Io(format!("Hermes session import: {error}"))
}

#[cfg(windows)]
fn create_private_profile(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateDirectoryW, GetVolumeInformationByHandleW, FILE_FLAG_BACKUP_SEMANTICS,
    };
    use windows_sys::Win32::System::SystemServices::FILE_PERSISTENT_ACLS;

    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let directory = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(parent)?;
    let mut volume_flags = 0;
    // The directory handle and flags buffer remain valid for the call.
    if unsafe {
        GetVolumeInformationByHandleW(
            directory.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut volume_flags,
            null_mut(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if volume_flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "profile volume does not support persistent ACLs",
        ));
    }

    let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    name.push(0);
    // Protected, inheritable full access for the owner, SYSTEM and administrators.
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)\0"
        .encode_utf16()
        .collect();
    let mut descriptor = null_mut();
    // The input is NUL-terminated and the output lives until LocalFree below.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // Both arguments remain valid for the synchronous directory creation.
    let created = unsafe { CreateDirectoryW(name.as_ptr(), &security) };
    let error = (created == 0).then(io::Error::last_os_error);
    // The descriptor was allocated by the Windows conversion function.
    unsafe { LocalFree(descriptor) };
    error.map_or(Ok(()), Err)
}

#[cfg(unix)]
fn create_private_profile(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

/// Convert the export in a fresh profile, publishing the sessions directory only after validation.
pub fn import_hermes_sessions(
    export: &Path,
    profile: &Path,
) -> Result<HermesHistorySummary, AgentError> {
    if !fs::metadata(export).map_err(io_error)?.is_file() {
        return Err(AgentError::Config(
            "Hermes export must be a regular file".into(),
        ));
    }
    let input = File::open(export).map_err(io_error)?;
    let leaf = profile
        .file_name()
        .ok_or_else(|| AgentError::Config("A new profile directory name is required".into()))?;
    #[cfg(windows)]
    {
        let name = leaf.to_string_lossy();
        if name.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*'])
            || name.chars().any(char::is_control)
            || name.ends_with([' ', '.'])
            || windows_device_name(&name)
        {
            return Err(AgentError::Config(
                "Invalid Windows profile directory name".into(),
            ));
        }
    }
    let parent = profile
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let profile = fs::canonicalize(parent).map_err(io_error)?.join(leaf);
    create_private_profile(&profile).map_err(io_error)?;
    let result = (|| {
        let stage = tempfile::tempdir_in(&profile).map_err(io_error)?;
        let sessions = stage.path().join("sessions");
        fs::create_dir(&sessions).map_err(io_error)?;
        let summary = import_records(input, &sessions)?;
        fs::rename(sessions, profile.join("sessions")).map_err(io_error)?;
        Ok(summary)
    })();
    if result.is_err() {
        // Only remove the empty directory reserved by this invocation.
        let _ = fs::remove_dir(&profile);
    }
    result
}
