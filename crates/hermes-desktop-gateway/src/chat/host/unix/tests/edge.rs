use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hermes_config::TerminalBackendType;
use serde_json::{json, Map, Value};
use tokio::sync::watch;

use super::super::super::{text, RpcError, NOT_AVAILABLE};
use super::super::complete::{complete_path, items};
use super::super::supervise::{kill_run, new_session, read_capped, wait_unreaped, Ended, Slot};
use super::super::{
    cli_exec_beside, run_cli, shell_exec, shell_reply, BAD_ARGV, BLOCKED, BUSY, CLI_FAILED,
    CLI_TIMED_OUT, NOT_STARTED, TIMED_OUT,
};
use super::{slot, texts};
use crate::chat::{Chat, Request};
#[test]
fn an_absent_or_null_text_is_empty_and_any_other_kind_is_refused() {
    let mut params = Map::new();
    assert_eq!(text(&params, "word", 8), Ok(""));
    params.insert("word".into(), Value::Null);
    assert_eq!(text(&params, "word", 8), Ok(""));
    params.insert("word".into(), json!(7));
    assert_eq!(
        text(&params, "word", 8),
        Err((crate::chat::INVALID_PARAMS, "word must be a string".into()))
    );
}

fn chat_at(home: &Path) -> Chat {
    Chat::new(hermes_config::GatewayConfig {
        home_dir: Some(home.to_string_lossy().into_owned()),
        ..Default::default()
    })
}

/// An executable `sh` script at `dir/name`.
fn named_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[tokio::test]
async fn a_cli_run_that_prints_nothing_says_so_with_its_code() {
    let dir = tempfile::tempdir().unwrap();
    let program = named_script(dir.path(), "quiet", "exit 3");
    let chat = chat_at(dir.path());
    let reply = run_cli(
        &program,
        &chat,
        &[],
        Duration::from_secs(30),
        slot(9010).await,
    )
    .await
    .unwrap();
    assert_eq!(
        reply,
        json!({"blocked": false, "code": 3, "output": "(no output)"})
    );
}

#[tokio::test]
async fn a_cli_run_past_its_limit_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let program = named_script(dir.path(), "slow", "sleep 20");
    let chat = chat_at(dir.path());
    let reply = run_cli(
        &program,
        &chat,
        &[],
        Duration::from_secs(1),
        slot(9011).await,
    )
    .await;
    assert_eq!(reply, Err((CLI_TIMED_OUT, "cli.exec: timeout".into())));
}

#[tokio::test]
async fn a_cli_program_that_is_missing_cannot_start() {
    let dir = tempfile::tempdir().unwrap();
    let chat = chat_at(dir.path());
    let program = dir.path().join("absent");
    let reply = run_cli(
        &program,
        &chat,
        &[],
        Duration::from_secs(30),
        slot(9012).await,
    )
    .await;
    assert_eq!(
        reply,
        Err((
            CLI_FAILED,
            format!("could not start hermes: {}", std::io::ErrorKind::NotFound)
        ))
    );
}

/// Group 0 is the caller's own: killing it would end this test process.
#[test]
fn a_leader_of_0_or_1_is_never_killed() {
    kill_run(0);
    kill_run(1);
}

#[tokio::test]
async fn a_missing_stream_reads_as_empty() {
    let (_stop, stopped) = watch::channel(());
    assert_eq!(read_capped(None::<tokio::io::Empty>, stopped).await, "");
}

// Other tests hold slots too, so the first 2 wait for the process's 4.
#[tokio::test]
async fn a_connection_runs_at_most_2_children() {
    let first = slot(9004).await;
    let _second = slot(9004).await;
    assert_eq!(Slot::take(9004).err().unwrap().0, BUSY);
    let third = tokio::spawn(slot(9004));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!third.is_finished(), "a third child started beside 2");
    drop(first);
    third.await.unwrap();
}

fn request(chat: Chat, conn: u64, params: Value) -> Request {
    let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
    Request {
        chat: std::sync::Arc::new(chat),
        conn: crate::chat::Conn::new(conn, outbox, false),
        params: params.as_object().unwrap().clone(),
    }
}

#[tokio::test]
async fn shell_exec_runs_only_with_the_local_terminal_backend() {
    let mut config = hermes_config::GatewayConfig::default();
    config.terminal.backend = TerminalBackendType::Docker;
    let reply = shell_exec(request(
        Chat::new(config),
        9020,
        json!({"command": "echo hi"}),
    ))
    .await;
    assert_eq!(
        reply,
        Err((
            NOT_AVAILABLE,
            "shell.exec runs only with the local terminal backend".into()
        ))
    );
}

#[tokio::test]
async fn a_dangerous_command_the_hardline_passes_is_still_refused() {
    let chat = Chat::new(hermes_config::GatewayConfig::default());
    let command = json!({"command": "sqlite3 app.db 'DELETE FROM users'"});
    let reply = shell_exec(request(chat, 9021, command)).await;
    assert_eq!(
        reply,
        Err((
            BLOCKED,
            "blocked: SQL DELETE without WHERE. Use the agent for dangerous commands.".into()
        ))
    );
}

#[test]
fn a_shell_run_that_times_out_or_cannot_start_says_so() {
    assert_eq!(
        shell_reply(Ended::TimedOut),
        Err((TIMED_OUT, "command timed out (30s)".into()))
    );
    let kind = std::io::ErrorKind::NotFound;
    assert_eq!(
        shell_reply(Ended::NotStarted(kind)),
        Err((NOT_STARTED, format!("could not start the command: {kind}")))
    );
}

#[tokio::test]
async fn cli_exec_takes_a_positive_timeout_and_refuses_0() {
    let dir = tempfile::tempdir().unwrap();
    let call = |timeout: u64| {
        request(
            chat_at(dir.path()),
            9022,
            json!({"argv": ["version"], "timeout": timeout}),
        )
    };
    assert_eq!(
        cli_exec_beside(call(0), None).await,
        Err((
            BAD_ARGV,
            "timeout must be a positive number of seconds".into()
        ))
    );
    let reply = cli_exec_beside(call(5), None).await.unwrap();
    assert_eq!(reply["blocked"], true);
}

async fn retry_cli_beside(
    call: impl Fn() -> Request,
    bin: &Path,
    first: Slot,
) -> Result<Value, RpcError> {
    let asked = async {
        loop {
            match cli_exec_beside(call(), Some(bin)).await {
                Err((BUSY, _)) => tokio::time::sleep(Duration::from_millis(100)).await,
                reply => return reply,
            }
        }
    };
    let free_one = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(first);
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(asked, free_one)
    })
    .await
    .expect("no slot came free")
    .0
}

#[tokio::test]
async fn an_allowed_cli_call_runs_the_hermes_beside_the_gateway() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("profiles")).unwrap();
    std::fs::write(home.path().join("profiles/work.yaml"), "").unwrap();
    let bin = tempfile::tempdir().unwrap();
    let call = || {
        request(
            chat_at(home.path()),
            9023,
            json!({"argv": ["profile", "delete", "work", "--yes"]}),
        )
    };
    assert_eq!(
        cli_exec_beside(call(), Some(bin.path())).await,
        Err((CLI_FAILED, "hermes CLI not found".into()))
    );
    named_script(
        bin.path(),
        "hermes",
        r#"printf '%s|' "$@"; printf '%s' "$HERMES_HOME""#,
    );
    // The connection already runs 2, so the call is busy until one is given back.
    let first = slot(9023).await;
    let _second = slot(9023).await;
    let busy = cli_exec_beside(call(), Some(bin.path())).await;
    assert_eq!(busy, Err((BUSY, "busy".into())));
    let reply = retry_cli_beside(&call, bin.path(), first).await;
    assert_eq!(
        reply,
        Ok(json!({
            "blocked": false,
            "code": 0,
            "output": format!("profile|delete|work|--yes|{}", home.path().display()),
        }))
    );
}

#[tokio::test]
async fn an_empty_word_completes_to_nothing() {
    let chat = Chat::new(hermes_config::GatewayConfig::default());
    let reply = complete_path(request(chat, 9024, json!({"word": ""}))).await;
    assert_eq!(reply, Ok(json!({"items": []})));
}

#[test]
fn a_listing_stops_at_30_items() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..31 {
        std::fs::write(dir.path().join(format!("f{n:02}")), "").unwrap();
    }
    let items = items(dir.path(), "f");
    let texts = texts(&items);
    assert_eq!(texts.len(), 30);
    assert_eq!((texts[0], texts[29]), ("f00", "f29"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_name_that_is_not_unicode_is_not_listed() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), "").unwrap();
    let odd = std::ffi::OsStr::from_bytes(b"a\xff");
    std::fs::write(dir.path().join(odd), "").unwrap();
    assert_eq!(texts(&items(dir.path(), "a")), ["a"]);
}

#[test]
fn waiting_on_a_process_that_is_not_a_child_returns_at_once() {
    wait_unreaped(i32::try_from(std::process::id()).unwrap());
}

const SESSION_CHILD: &str = "HERMES_HOST_TEST_SESSION_CHILD";

/// `new_session` runs in a forked child before exec, where coverage is never written, so
/// this test re-runs itself in a child process of the test binary that calls it and
/// exits normally.
#[test]
fn a_new_session_makes_the_process_its_leader_and_only_once() {
    let name = "a_new_session_makes_the_process_its_leader_and_only_once";
    if std::env::var_os(SESSION_CHILD).is_some() {
        new_session().unwrap();
        // SAFETY: getsid and getpgrp take plain integers and touch no memory of ours.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let (session, group) = unsafe { (libc::getsid(0), libc::getpgrp()) };
        let pid = i32::try_from(std::process::id()).unwrap();
        assert_eq!((session, group), (pid, pid));
        // A session leader cannot start another session.
        let again = new_session().unwrap_err();
        assert_eq!(again.raw_os_error(), Some(libc::EPERM));
        return;
    }
    let path = module_path!().split_once("::").unwrap().1;
    // Test binary re-executes itself in a child process to isolate setsid().
    // nosemgrep: rust.lang.security.current-exe.current-exe
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .args(["--exact", &format!("{path}::{name}"), "--test-threads", "1"])
        .env(SESSION_CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("1 passed"),
        "the child ran no test: {stdout}"
    );
}
