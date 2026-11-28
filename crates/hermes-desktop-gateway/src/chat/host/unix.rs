use std::path::{Path, PathBuf};
use std::time::Duration;

use hermes_config::TerminalBackendType;
use hermes_intelligence::redact::redact_sensitive_text;
use hermes_tools::approval::{
    check_all_command_guards_with_context, detect_dangerous_command, CommandGuardContext,
};
use serde_json::{json, Map, Value};
use tokio::process::Command;

use super::{text, RpcError, NOT_AVAILABLE};
use crate::chat::{Chat, Request};

mod complete;
mod supervise;
#[cfg(test)]
mod tests;

pub(super) use complete::complete_path;
use supervise::{supervise, Ended, Slot};

const EMPTY: i64 = 4004;
const BLOCKED: i64 = 4005;
const BAD_ARGV: i64 = 4003;
const BUSY: i64 = 4090;
const TIMED_OUT: i64 = 5002;
const NOT_STARTED: i64 = 5003;
const NO_FOLDER: i64 = 5004;
const CLI_TIMED_OUT: i64 = 5016;
const CLI_FAILED: i64 = 5017;

const SHELL_TIMEOUT: Duration = Duration::from_secs(30);

/// The `!cmd` grammar needs a shell, so the request's text is the shell program, passed as
/// one argument.
pub(super) async fn shell_exec(req: Request) -> Result<Value, RpcError> {
    let command = text(&req.params, "command", 8192)?;
    if command.trim().is_empty() {
        return Err((EMPTY, "empty command".into()));
    }
    if req.chat.config.terminal.backend != TerminalBackendType::Local {
        return Err((
            NOT_AVAILABLE,
            "shell.exec runs only with the local terminal backend".into(),
        ));
    }
    if let Some(reason) = hardline(command) {
        return Err((
            BLOCKED,
            format!("blocked (hardline): {reason}. Use the agent for dangerous commands."),
        ));
    }
    if let Some(finding) = detect_dangerous_command(command) {
        return Err((
            BLOCKED,
            format!(
                "blocked: {}. Use the agent for dangerous commands.",
                finding.description
            ),
        ));
    }
    let slot = Slot::take(req.conn.id)?;
    let mut child = Command::new("/bin/sh");
    child.arg("-c").arg(command);
    shell_reply(supervise(child, folder()?, SHELL_TIMEOUT, slot).await)
}

/// `shell.exec`'s answer for a run that ended as `ended`.
fn shell_reply(ended: Ended) -> Result<Value, RpcError> {
    match ended {
        Ended::Exited {
            stdout,
            stderr,
            code,
        } => Ok(json!({
            // Redacted before tailing, so a credential across the cut cannot survive in parts.
            "stdout": tail(&redact_sensitive_text(stdout), 4000),
            "stderr": tail(&redact_sensitive_text(stderr), 2000),
            "code": code,
        })),
        Ended::TimedOut => Err((TIMED_OUT, "command timed out (30s)".into())),
        Ended::NotStarted(kind) => {
            Err((NOT_STARTED, format!("could not start the command: {kind}")))
        }
    }
}

/// The hardline policy alone: with approval off the guard stops after it.
fn hardline(command: &str) -> Option<String> {
    let context = CommandGuardContext {
        approval_mode_off: true,
        ..CommandGuardContext::from_env()
    };
    // With approval off the guard stops at the hardline policy and cannot fail; were it to,
    // the command is refused.
    match check_all_command_guards_with_context(command, "local", context, None) {
        Ok(result) if result.approved => None,
        result => Some(
            result.map_or("the safety check could not run".to_string(), |result| {
                result.description.unwrap_or_default()
            }),
        ),
    }
}

/// `cli.exec {argv, timeout?}`: the `hermes` CLI beside this executable, with no shell, for
/// the exact argument sequences [`allowed`] lists; anything else is answered as blocked.
pub(super) async fn cli_exec(req: Request) -> Result<Value, RpcError> {
    // Locates the sibling `hermes` binary beside the gateway; not an access decision.
    // nosemgrep: rust.lang.security.current-exe.current-exe
    let exe = std::env::current_exe().ok();
    cli_exec_beside(req, exe.as_deref().and_then(Path::parent)).await
}

/// [`cli_exec`] with `bin` as the folder that holds `hermes`.
async fn cli_exec_beside(req: Request, bin: Option<&Path>) -> Result<Value, RpcError> {
    let argv = argv(&req.params)?;
    let timeout = match req.params.get("timeout") {
        None | Some(Value::Null) => 240,
        Some(value) => value
            .as_u64()
            .filter(|seconds| *seconds > 0)
            .ok_or((
                BAD_ARGV,
                "timeout must be a positive number of seconds".into(),
            ))?
            .min(600),
    };
    if !allowed(&req.chat.home, &argv) {
        return Ok(json!({"blocked": true, "hint": BLOCKED_HINT, "code": -1, "output": ""}));
    }
    let program = bin
        .map(|dir| dir.join("hermes"))
        .filter(|path| path.is_file())
        .ok_or((CLI_FAILED, "hermes CLI not found".into()))?;
    let slot = Slot::take(req.conn.id)?;
    run_cli(
        &program,
        &req.chat,
        &argv,
        Duration::from_secs(timeout),
        slot,
    )
    .await
}

const BLOCKED_HINT: &str =
    "this command does not run from the desktop here; run `hermes` in a terminal";

/// `argv`: 1 to 64 strings, each 1 to 4,096 bytes with no NUL.
fn argv(params: &Map<String, Value>) -> Result<Vec<String>, RpcError> {
    let bad = || (BAD_ARGV, "argv must be list[str]".to_string());
    let Some(Value::Array(items)) = params.get("argv") else {
        return Err(bad());
    };
    let argv = items
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(bad)?;
    if argv.is_empty()
        || argv.len() > 64
        || argv
            .iter()
            .any(|arg| arg.is_empty() || arg.len() > 4096 || arg.contains('\0'))
    {
        return Err((
            BAD_ARGV,
            "argv must hold 1 to 64 strings of 1 to 4096 bytes".into(),
        ));
    }
    Ok(argv)
}

/// The sequences the desktop sends that the Rust CLI serves: `profile delete <name> --yes`
/// for a profile this home lists. Anything else, an empty argv included, is not run.
fn allowed(home: &Path, argv: &[String]) -> bool {
    match argv {
        [profile, delete, name, yes]
            if profile == "profile" && delete == "delete" && yes == "--yes" =>
        {
            listed_profile(home, name)
        }
        _ => false,
    }
}

/// A plain name with its own `profiles/<name>.yaml` (or `.yml`) file, not a link.
fn listed_profile(home: &Path, name: &str) -> bool {
    let plain = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    plain
        && ["yaml", "yml"].iter().any(|ext| {
            std::fs::symlink_metadata(home.join("profiles").join(format!("{name}.{ext}")))
                .is_ok_and(|meta| meta.file_type().is_file())
        })
}

async fn run_cli(
    program: &Path,
    chat: &Chat,
    argv: &[String],
    limit: Duration,
    slot: Slot,
) -> Result<Value, RpcError> {
    let mut child = Command::new(program);
    child.args(argv);
    child.env("HERMES_HOME", &chat.home);
    match supervise(child, folder()?, limit, slot).await {
        Ended::Exited {
            stdout,
            stderr,
            code,
        } => {
            let joined = [stdout.trim_end(), stderr.trim_end()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let output = redact_sensitive_text(joined);
            let output = if output.is_empty() {
                "(no output)".to_string()
            } else {
                output.chars().take(48_000).collect()
            };
            Ok(json!({"blocked": false, "code": code, "output": output}))
        }
        Ended::TimedOut => Err((CLI_TIMED_OUT, "cli.exec: timeout".into())),
        Ended::NotStarted(kind) => Err((CLI_FAILED, format!("could not start hermes: {kind}"))),
    }
}

/// The session's folder: today the gateway's own working folder for every session. Never a
/// fallback when it is gone.
fn folder() -> Result<PathBuf, RpcError> {
    std::env::current_dir()
        .ok()
        .filter(|dir| dir.is_dir())
        .ok_or((NO_FOLDER, "working folder unavailable".into()))
}

/// The last `count` characters of `text`.
fn tail(text: &str, count: usize) -> String {
    let skip = text.chars().count().saturating_sub(count);
    text.chars().skip(skip).collect()
}
