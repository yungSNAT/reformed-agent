use std::os::unix::fs::{symlink, PermissionsExt};

use super::*;

fn texts(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .map(|item| item["text"].as_str().unwrap())
        .collect()
}

#[test]
fn a_listing_stays_under_its_root_and_follows_no_link() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir_all(root.join("src/chat")).unwrap();
    std::fs::write(root.join("src/chat.rs"), "").unwrap();
    std::fs::write(root.join(".hidden"), "").unwrap();
    symlink(outside.path(), root.join("out")).unwrap();
    symlink(outside.path(), root.join("src/away")).unwrap();

    assert_eq!(texts(&items(root, "src/ch")), ["src/chat/", "src/chat.rs"]);
    assert_eq!(
        texts(&items(root, "./src/c")),
        ["./src/chat/", "./src/chat.rs"]
    );
    // A link is listed as an entry, never as a folder, and never walked into.
    assert_eq!(texts(&items(root, "src/a")), ["src/away"]);
    assert!(items(root, "out/").is_empty());
    assert!(items(root, "out/se").is_empty());
    assert!(items(root, "src/away/").is_empty());
    assert!(items(root, "@file:out/").is_empty());
    // Nothing outside the root, however it is named.
    let absolute = format!("{}/", outside.path().display());
    assert!(items(root, &absolute).is_empty());
    assert!(items(root, "~/").is_empty());
    assert!(items(root, "../").is_empty());
    assert!(items(root, "src/../../").is_empty());
    assert!(items(root, "C:/").is_empty());
    // Hidden names only when the typed part starts with a dot.
    assert!(!texts(&items(root, "")).contains(&".hidden"));
    assert!(texts(&items(root, ".h")).contains(&".hidden"));
}

#[test]
fn context_words_tag_files_and_folders_and_a_bare_at_gives_the_hints() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("dev.md"), "").unwrap();
    assert_eq!(texts(&items(root, "@"))[0], "@diff");
    assert_eq!(texts(&items(root, "@d")), ["@file:dev.md", "@folder:docs/"]);
    assert_eq!(texts(&items(root, "@folder:")), ["@folder:docs/"]);
    assert_eq!(
        texts(&items(root, "@/d")),
        ["@file:dev.md", "@folder:docs/"]
    );
    assert!(items(root, "@t").is_empty());
    assert!(items(root, "@url:x").is_empty());
}

#[test]
fn only_a_listed_profile_delete_is_allowed() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    std::fs::write(home.join("profiles/work.yaml"), "").unwrap();
    symlink(
        home.join("profiles/work.yaml"),
        home.join("profiles/linked.yaml"),
    )
    .unwrap();
    let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    assert!(allowed(
        home,
        &argv(&["profile", "delete", "work", "--yes"])
    ));
    for refused in [
        &["profile", "delete", "other", "--yes"][..],
        &["profile", "delete", "linked", "--yes"],
        &["profile", "delete", "work"],
        &["profile", "delete", "work", "--yes", "--force"],
        &["profile", "delete", "--yes", "work"],
        &["profile", "delete", "../work", "--yes"],
        &["profile", "describe", "work", "--text", "x"],
        &["--profile", "work", "config", "get", "model"],
        &["gateway"],
        &[],
    ] {
        assert!(!allowed(home, &argv(refused)), "{refused:?}");
    }
}

#[test]
fn a_child_keeps_only_listed_names_and_no_credential_looking_value() {
    let sample = format!("{}_{}", "ghp", "abcdefghijklmnopqrstuvwxyz0123456789");
    let vars = [
        ("PATH", "/usr/bin:/bin"),
        ("LC_ALL", "C"),
        ("OPENAI_API_KEY", "plain"),
        ("HERMES_DASHBOARD_SESSION_TOKEN", "plain"),
        ("TERM", sample.as_str()),
    ]
    .map(|(n, v)| (n.to_string(), v.to_string()));
    let kept = kept_env(vars.into_iter());
    let names: Vec<_> = kept.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["PATH", "LC_ALL"]);
}

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("hermes");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn the_cli_gets_each_argument_whole_and_no_shell() {
    let dir = tempfile::tempdir().unwrap();
    let program = script(
        dir.path(),
        r#"for a in "$@"; do printf '<%s>\n' "$a"; done"#,
    );
    let chat = Chat::new(hermes_config::GatewayConfig::default());
    let argv = ["a b", "$(touch pwned)", ";", "'q'"].map(String::from);
    let reply = run_cli(&program, &chat, &argv, Duration::from_secs(10), Slot(9001))
        .await
        .unwrap();
    assert_eq!(reply["output"], "<a b>\n<$(touch pwned)>\n<;>\n<'q'>");
    assert_eq!(reply["code"], 0);
    assert!(!Path::new("pwned").exists());
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn gone(pid: i32) -> bool {
    tokio::time::timeout(Duration::from_secs(10), async {
        while alive(pid) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .is_ok()
}

fn background_pid(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

async fn written(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !std::fs::read_to_string(path).is_ok_and(|text| text.ends_with('\n')) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the child never started");
}

fn shell(pidfile: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "sleep 300 & echo $! > '{}'; wait",
        pidfile.display()
    ));
    command
}

#[tokio::test]
async fn a_timeout_kills_the_whole_group_and_reaps_it() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let ended = supervise(
        shell(&pidfile),
        dir.path().to_path_buf(),
        Duration::from_secs(2),
        Slot(9002),
    )
    .await;
    assert!(matches!(ended, Ended::TimedOut));
    assert!(
        gone(background_pid(&pidfile)).await,
        "a descendant outlived the timeout"
    );
}

#[tokio::test]
async fn a_dropped_request_kills_the_whole_group_and_frees_its_slot() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let run = tokio::spawn(supervise(
        shell(&pidfile),
        dir.path().to_path_buf(),
        Duration::from_secs(300),
        Slot::take(9003).unwrap(),
    ));
    written(&pidfile).await;
    run.abort();
    assert!(
        gone(background_pid(&pidfile)).await,
        "a descendant outlived the request"
    );
    // The supervisor gave the slot back once the child was reaped.
    for _ in 0..100 {
        if Slot::take(9003).is_ok() && Slot::take(9003).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the slot was never given back");
}

/// A shell that starts a background `perl` running `escape`, waits until it has escaped
/// and written its pid to `pid`, then exits. The `perl` keeps both output pipes open.
fn escapee(escape: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "perl -MPOSIX -e '{escape}; open my $f, \">\", \"pid.tmp\"; print $f \"$$\\n\"; \
         close $f; rename \"pid.tmp\", \"pid\"; sleep 300' & \
         while [ ! -e pid ]; do sleep 0.05; done"
    ));
    command
}

fn held(conn: u64) -> usize {
    let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    running.get(&conn).copied().unwrap_or(0)
}

/// A slot for `conn`, waiting while other tests hold the process's 4.
async fn slot(conn: u64) -> Slot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(slot) = Slot::take(conn) {
                return slot;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("no slot came free")
}

fn kill(pid: i32) {
    // SAFETY: kill takes plain integers and touches no memory of ours.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// Escaped processes, killed when the test ends however it ends.
#[derive(Default)]
struct Escaped(Vec<i32>);

impl Drop for Escaped {
    fn drop(&mut self) {
        self.0.iter().copied().for_each(kill);
    }
}

#[tokio::test]
async fn a_child_that_leaves_its_session_holds_neither_the_answer_nor_its_slot() {
    let dir = tempfile::tempdir().unwrap();
    let mut escaped = Escaped::default();
    // 5 in a row: a leaked slot fails the third (2 a connection), and `held` at once.
    for run in 0..5 {
        let folder = dir.path().join(run.to_string());
        std::fs::create_dir(&folder).unwrap();
        let ended = tokio::time::timeout(
            Duration::from_secs(15),
            supervise(
                escapee("POSIX::setsid()"),
                folder.clone(),
                Duration::from_secs(30),
                slot(9005).await,
            ),
        )
        .await;
        if folder.join("pid").exists() {
            escaped.0.push(background_pid(&folder.join("pid")));
        }
        let ended = ended.expect("the answer waited on a descendant that left the session");
        assert!(matches!(ended, Ended::Exited { code: Some(0), .. }));
        assert_eq!(held(9005), 0, "the slot was not given back");
    }
}

#[test]
fn a_run_polled_once_and_dropped_starts_no_child() {
    use futures::FutureExt;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let runs = dir.path().join("runs");
    let counting = || {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("echo run >> runs");
        command
    };
    let once = {
        let _enter = rt.enter();
        supervise(
            counting(),
            dir.path().to_path_buf(),
            Duration::from_secs(30),
            Slot(9007),
        )
        .now_or_never()
    };
    assert!(once.is_none(), "a child run answers only after a wait");
    // Keep the runtime idle, so no supervisor can kill what the poll started.
    std::thread::sleep(Duration::from_secs(1));
    let count = || {
        std::fs::read_to_string(&runs)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    };
    assert_eq!(count(), 0, "the dropped request started its command");
    let ended = rt.block_on(supervise(
        counting(),
        dir.path().to_path_buf(),
        Duration::from_secs(30),
        Slot(9007),
    ));
    assert!(matches!(ended, Ended::Exited { code: Some(0), .. }));
    assert_eq!(count(), 1);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_child_that_leaves_its_group_but_not_its_session_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let ended = tokio::time::timeout(
        Duration::from_secs(15),
        supervise(
            escapee("POSIX::setpgid(0, 0)"),
            dir.path().to_path_buf(),
            Duration::from_secs(30),
            slot(9006).await,
        ),
    )
    .await;
    let pid = background_pid(&dir.path().join("pid"));
    let _escaped = Escaped(vec![pid]);
    let survived = !gone(pid).await;
    assert!(ended.is_ok(), "the answer waited on a descendant");
    assert!(!survived, "a descendant in the run's session outlived it");
}

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
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    let asked = async {
        loop {
            match cli_exec_beside(call(), Some(bin.path())).await {
                Err((BUSY, _)) => tokio::time::sleep(Duration::from_millis(100)).await,
                reply => return reply,
            }
        }
    };
    let free_one = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(first);
    };
    let (reply, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(asked, free_one)
    })
    .await
    .expect("no slot came free");
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
