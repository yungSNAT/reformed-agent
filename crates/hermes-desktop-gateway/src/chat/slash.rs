//! Slash commands and pasted text: `slash.exec`, `command.dispatch` and `paste.collapse`
//! (upstream tui_gateway/methods_tools.py and methods_complete.py).
//!
//! The CLI's own slash commands run against the interactive CLI and cannot be reached from
//! here, so `slash.exec` answers a few itself and refuses the rest with 4018, which sends both
//! clients on to `command.dispatch`. That serves quick-command aliases, installed skills,
//! `/queue` and `/steer`. Nothing here runs a command or reads a path a client names: an exec
//! quick command is refused, and a paste is written under `<home>/pastes`.
//!
//! Upstream writes a paste with default permissions and follows a link in the folder's place.
//! Here a paste file is owner-only (mode 0600 in a 0700 folder on Unix; on Windows the protected
//! descriptor `attach` gives staged images: owner, SYSTEM and administrators), and a link or a
//! file in the folder's place is refused. On Unix the file is created relative to the folder
//! opened without following a link; on Windows a link swapped in after the folder is checked is
//! not caught.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hermes_tools::skill_commands::{
    installed_skill_slash_command_snapshot, render_skill_slash_command_snapshot,
    resolve_installed_skill_slash_command, SkillCommandResolverConfig,
};
use serde_json::{json, Map, Value};

use super::{Chat, Handler, Request, RpcError, INVALID_PARAMS, SESSION_NOT_FOUND};

pub(super) const METHODS: &[(&str, Handler)] = &[
    ("slash.exec", |r| Box::pin(blocking(r, slash_exec))),
    ("command.dispatch", |r| {
        Box::pin(blocking(r, command_dispatch))
    }),
    ("paste.collapse", |r| Box::pin(blocking(r, paste_collapse))),
];

/// Upstream's codes: an empty command or paste, and a command this backend cannot run.
const EMPTY: i64 = 4004;
const DISPATCH_FAILED: i64 = 4018;
const PASTE_FAILED: i64 = 5021;

/// Runs `handler` on the blocking pool: skills are read from disk and pastes written to it.
async fn blocking(
    req: Request,
    handler: fn(&Chat, &Map<String, Value>) -> Result<Value, RpcError>,
) -> Result<Value, RpcError> {
    let Request { chat, params, .. } = req;
    tokio::task::spawn_blocking(move || handler(&chat, &params))
        .await
        .unwrap_or_else(|_| Err((DISPATCH_FAILED, "the command stopped unexpectedly".into())))
}

/// A string parameter: absent or null is empty; another type is refused.
fn string<'a>(params: &'a Map<String, Value>, key: &str) -> Result<&'a str, RpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err((INVALID_PARAMS, format!("{key} must be a string"))),
    }
}

/// `slash.exec {command, session_id}`: `{output}` for a command served here, else 4018.
fn slash_exec(chat: &Chat, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let id = string(params, "session_id")?;
    if !chat.lock_sessions().contains_key(id) {
        return Err((SESSION_NOT_FOUND, "session not found".into()));
    }
    let command = string(params, "command")?.trim();
    let (name, arg) = split(command);
    if name.is_empty() {
        return Err((EMPTY, "empty command".into()));
    }
    let output = match name.as_str() {
        "skills" if matches!(arg, "" | "list") => render_skill_slash_command_snapshot(
            &installed_skill_slash_command_snapshot(&skill_config(chat)),
        ),
        "tools" if arg.is_empty() || arg == "list" => {
            "No tools are available: this backend's turns run without tools.".to_string()
        }
        "version" => hermes_core::version::version_label(),
        _ => {
            // command.dispatch serves these; both clients call it after this refusal.
            let text = if dispatchable(chat, &name, arg)? {
                format!("use command.dispatch for /{name}")
            } else {
                format!("/{name} is not available on this backend")
            };
            return Err((DISPATCH_FAILED, text));
        }
    };
    Ok(json!({"output": output}))
}

/// Whether `command.dispatch` would serve `/name`.
fn dispatchable(chat: &Chat, name: &str, arg: &str) -> Result<bool, RpcError> {
    if let Some(quick) = chat.config.quick_commands.get(name) {
        // command.dispatch answers a quick command before anything else, and serves only an
        // alias with a target: an exec quick command runs nothing here.
        return Ok(quick.kind.trim().eq_ignore_ascii_case("alias")
            && quick
                .target
                .as_deref()
                .is_some_and(|t| !t.trim().is_empty()));
    }
    Ok(matches!(name, "queue" | "q" | "steer") || skill(chat, name, arg)?.is_some())
}

/// `command.dispatch {name, arg, session_id?}`: a directive for the client, in upstream's
/// stage order (quick command, skill, built-in), or 4018.
fn command_dispatch(chat: &Chat, params: &Map<String, Value>) -> Result<Value, RpcError> {
    string(params, "session_id")?;
    let name = string(params, "name")?
        .trim()
        .trim_start_matches('/')
        .to_ascii_lowercase();
    let arg = string(params, "arg")?.trim();
    if let Some(quick) = chat.config.quick_commands.get(&name) {
        return match quick.kind.trim().to_ascii_lowercase().as_str() {
            "alias" => match quick
                .target
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                Some(target) => {
                    Ok(json!({"type": "alias", "target": target.trim_start_matches('/')}))
                }
                None => Err((
                    DISPATCH_FAILED,
                    format!("quick command /{name} has no target"),
                )),
            },
            "exec" => Err((
                DISPATCH_FAILED,
                format!(
                    "quick command /{name} runs a shell command, which this backend does not run"
                ),
            )),
            other => Err((
                DISPATCH_FAILED,
                format!("quick command /{name} has unsupported type {other}"),
            )),
        };
    }
    if let Some((skill_name, message)) = skill(chat, &name, arg)? {
        return Ok(json!({"type": "skill", "name": skill_name, "message": message}));
    }
    match name.as_str() {
        "queue" | "q" if arg.is_empty() => Err((EMPTY, "usage: /queue <prompt>".into())),
        "queue" | "q" => Ok(json!({"type": "send", "message": arg})),
        "steer" if arg.is_empty() => Err((EMPTY, "usage: /steer <prompt>".into())),
        // This backend's turns cannot be steered: the text is the next prompt, which waits
        // behind a running turn.
        "steer" => Ok(json!({"type": "send", "message": arg})),
        _ => Err((
            DISPATCH_FAILED,
            format!("not a quick/plugin/bundle/skill command: {name}"),
        )),
    }
}

/// The installed skill `/name` names, as its name and the message that invokes it.
fn skill(chat: &Chat, name: &str, arg: &str) -> Result<Option<(String, String)>, RpcError> {
    resolve_installed_skill_slash_command(name, arg, &skill_config(chat))
        .map(|found| found.map(|invocation| (invocation.skill_name, invocation.message)))
        .map_err(|refusal| (DISPATCH_FAILED, refusal))
}

/// Skills come from this gateway's home only, filtered by its config.
fn skill_config(chat: &Chat) -> SkillCommandResolverConfig {
    SkillCommandResolverConfig {
        roots: vec![chat.skills_root()],
        enabled: chat.config.skills.enabled.clone(),
        disabled: chat.config.skills.disabled.clone(),
        ..SkillCommandResolverConfig::default()
    }
}

/// A command's lower-case name, without its slash, and the rest of the line.
fn split(command: &str) -> (String, &str) {
    let command = command.trim_start_matches('/');
    let (name, arg) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    (name.to_ascii_lowercase(), arg.trim())
}

/// Numbers this process's pastes, as upstream's counter.
static PASTES: AtomicU64 = AtomicU64::new(0);

/// `paste.collapse {text}`: writes the text to a new file under `<home>/pastes` (owner-only)
/// and answers `{placeholder, path, lines}`.
fn paste_collapse(chat: &Chat, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let text = string(params, "text")?;
    if text.is_empty() {
        return Err((EMPTY, "empty paste".into()));
    }
    let dir = chat.home.join("pastes");
    let (number, path) = write_paste(&dir, text).map_err(|err| {
        (
            PASTE_FAILED,
            format!("could not save the paste: {}", err.kind()),
        )
    })?;
    let lines = text.matches('\n').count() + 1;
    let shown = path.display();
    Ok(json!({
        "placeholder": format!("[Pasted text #{number}: {lines} lines \u{2192} {shown}]"),
        "path": shown.to_string(),
        "lines": lines,
    }))
}

/// Writes `text` to `paste_<n>_<HHMMSS>.txt` (UTC) in `dir`, never over an existing file.
fn write_paste(dir: &Path, text: &str) -> std::io::Result<(u64, PathBuf)> {
    crate::attach::private_folder(dir).map_err(std::io::Error::other)?;
    let folder = open_folder(dir)?;
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() % 86_400);
    let clock = format!(
        "{:02}{:02}{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    loop {
        let number = PASTES.fetch_add(1, Ordering::Relaxed) + 1;
        let name = format!("paste_{number}_{clock}.txt");
        match create_in(&folder, &name) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                return Ok((number, dir.join(name)));
            }
            // A file left by an earlier run of the gateway: take the next number.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
}

/// The pastes folder, opened without following a link in its place, so a link made there before
/// or after it was created is refused, and each file is created relative to it.
#[cfg(unix)]
fn open_folder(dir: &Path) -> std::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::open(dir, flags, Mode::empty())?)
}

/// Creates `name` in `folder`, owner-only, never over an existing file or through a link.
#[cfg(unix)]
fn create_in(folder: &std::os::fd::OwnedFd, name: &str) -> std::io::Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = rustix::fs::openat(folder, name, flags, Mode::RUSR | Mode::WUSR)?;
    Ok(std::fs::File::from(file))
}

/// The pastes folder, already checked to be a folder and not a link or junction.
#[cfg(windows)]
fn open_folder(dir: &Path) -> std::io::Result<PathBuf> {
    Ok(dir.to_path_buf())
}

/// Creates `name` in `folder`, owner-only, never over an existing entry.
#[cfg(windows)]
fn create_in(folder: &Path, name: &str) -> std::io::Result<std::fs::File> {
    let path = folder.join(name);
    // Any entry takes the name, a dangling link included: create-new would follow one.
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    crate::attach::owner_only::create_new(&path)
}
