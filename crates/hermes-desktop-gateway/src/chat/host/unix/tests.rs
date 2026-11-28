use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;

use super::complete::items;
use super::supervise::{kept_env, supervise, Ended, Slot, RUNNING};
use super::{allowed, run_cli};
use crate::chat::Chat;
pub(super) fn texts(items: &[Value]) -> Vec<&str> {
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
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
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
pub(super) async fn slot(conn: u64) -> Slot {
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
