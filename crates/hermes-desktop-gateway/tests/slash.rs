//! Slash commands and pasted text as the terminal UI and Hermes Desktop send them:
//! `slash.exec` first, `command.dispatch` when it refuses, and `paste.collapse`.

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const CONFIG: &str = "quick_commands:\n  rn:\n    type: alias\n    target: /review-notes\n  \
                      up:\n    type: exec\n    command: uptime\n  nt:\n    type: alias\n  \
                      bg:\n    type: bogus\n    target: /review-notes\n";

/// A gateway whose home has one installed skill, `review-notes`, and the quick commands above.
async fn start() -> (Socket, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), CONFIG).unwrap();
    let skill = home.path().join("skills").join("review-notes");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: review-notes\ndescription: Review the notes\n---\nList the open points in the notes.\n",
    )
    .unwrap();
    let config = hermes_config::load_config(home.path().to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        hermes_desktop_gateway::Chat::new(config),
    ));
    let request = format!("ws://{addr}/api/ws?token={TOKEN}")
        .into_client_request()
        .unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(next(&mut socket).await["params"]["type"], "gateway.ready");
    (socket, home)
}

async fn next(socket: &mut Socket) -> Value {
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(20), socket.next())
            .await
            .expect("no frame within 20 seconds")
            .expect("socket closed")
            .unwrap();
        if let Message::Text(text) = frame {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn call(socket: &mut Socket, method: &str, params: Value) -> Value {
    let request = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
    socket
        .send(Message::text(request.to_string()))
        .await
        .unwrap();
    loop {
        let frame = next(socket).await;
        if frame["id"] == 7 {
            return frame;
        }
    }
}

async fn session(socket: &mut Socket) -> String {
    let reply = call(socket, "session.create", json!({"cols": 80})).await;
    reply["result"]["session_id"].as_str().unwrap().to_string()
}

fn error(reply: &Value) -> (i64, &str) {
    let error = &reply["error"];
    (
        error["code"]
            .as_i64()
            .unwrap_or_else(|| panic!("no error in {reply}")),
        error["message"].as_str().unwrap(),
    )
}

#[tokio::test]
async fn slash_exec_refuses_an_unknown_session_and_an_empty_or_malformed_command() {
    let (mut socket, _home) = start().await;
    let reply = call(
        &mut socket,
        "slash.exec",
        json!({"command": "version", "session_id": "nope"}),
    )
    .await;
    assert_eq!(error(&reply), (4001, "session not found"));
    let id = session(&mut socket).await;
    let reply = call(
        &mut socket,
        "slash.exec",
        json!({"command": "  / ", "session_id": id}),
    )
    .await;
    assert_eq!(error(&reply), (4004, "empty command"));
    let reply = call(
        &mut socket,
        "slash.exec",
        json!({"command": 3, "session_id": id}),
    )
    .await;
    assert_eq!(error(&reply), (4000, "command must be a string"));
}

#[tokio::test]
async fn slash_exec_answers_the_commands_this_backend_serves() {
    let (mut socket, _home) = start().await;
    let id = session(&mut socket).await;
    let version = call(
        &mut socket,
        "slash.exec",
        json!({"command": "/version", "session_id": id}),
    )
    .await;
    assert_eq!(
        version["result"]["output"],
        hermes_core::version::version_label()
    );
    let tools = call(
        &mut socket,
        "slash.exec",
        json!({"command": "tools", "session_id": id}),
    )
    .await;
    // True until tool-running turns land: whichever of that change and this one merges second
    // lists the offered tools here and changes this assertion.
    assert!(
        tools["result"]["output"]
            .as_str()
            .unwrap()
            .contains("run without tools"),
        "{tools}"
    );
    let skills = call(
        &mut socket,
        "slash.exec",
        json!({"command": "skills list", "session_id": id}),
    )
    .await;
    let listing = skills["result"]["output"].as_str().unwrap();
    assert!(listing.contains("available=1 blocked=0"), "{listing}");
}

#[tokio::test]
async fn slash_exec_sends_what_command_dispatch_serves_there_and_names_the_rest() {
    let (mut socket, _home) = start().await;
    let id = session(&mut socket).await;
    for (command, name) in [
        ("queue later", "queue"),
        ("Review-Notes now", "review-notes"),
        ("rn", "rn"),
    ] {
        let reply = call(
            &mut socket,
            "slash.exec",
            json!({"command": command, "session_id": id}),
        )
        .await;
        assert_eq!(
            error(&reply),
            (4018, format!("use command.dispatch for /{name}").as_str())
        );
    }
    // An exec quick command is one command.dispatch refuses, so it is named, not sent there.
    for (command, name) in [("frobnicate x", "frobnicate"), ("up", "up")] {
        let reply = call(
            &mut socket,
            "slash.exec",
            json!({"command": command, "session_id": id}),
        )
        .await;
        assert_eq!(
            error(&reply),
            (
                4018,
                format!("/{name} is not available on this backend").as_str()
            )
        );
    }
}

#[tokio::test]
async fn command_dispatch_serves_aliases_skills_queue_and_steer() {
    let (mut socket, _home) = start().await;
    let alias = call(
        &mut socket,
        "command.dispatch",
        json!({"name": "rn", "arg": ""}),
    )
    .await;
    assert_eq!(
        alias["result"],
        json!({"type": "alias", "target": "review-notes"})
    );
    let skill = call(
        &mut socket,
        "command.dispatch",
        json!({"name": "/review-notes", "arg": "for Friday"}),
    )
    .await;
    let result = &skill["result"];
    assert_eq!(
        (&result["type"], &result["name"]),
        (&json!("skill"), &json!("review-notes"))
    );
    let message = result["message"].as_str().unwrap();
    assert!(
        message.contains("List the open points in the notes."),
        "{message}"
    );
    assert!(message.contains("for Friday"), "{message}");
    let queued = call(
        &mut socket,
        "command.dispatch",
        json!({"name": "q", "arg": "then this"}),
    )
    .await;
    assert_eq!(
        queued["result"],
        json!({"type": "send", "message": "then this"})
    );
    let steer = call(
        &mut socket,
        "command.dispatch",
        json!({"name": "steer", "arg": "shorter"}),
    )
    .await;
    assert_eq!(
        steer["result"],
        json!({"type": "send", "message": "shorter"})
    );
    let empty = call(&mut socket, "command.dispatch", json!({"name": "queue"})).await;
    assert_eq!(error(&empty), (4004, "usage: /queue <prompt>"));
}

#[tokio::test]
async fn command_dispatch_runs_no_exec_quick_command_and_names_an_unknown_one() {
    let (mut socket, _home) = start().await;
    let exec = call(&mut socket, "command.dispatch", json!({"name": "up"})).await;
    assert_eq!(
        error(&exec),
        (
            4018,
            "quick command /up runs a shell command, which this backend does not run"
        )
    );
    let unknown = call(&mut socket, "command.dispatch", json!({"name": "nope"})).await;
    assert_eq!(
        error(&unknown),
        (4018, "not a quick/plugin/bundle/skill command: nope")
    );
    let malformed = call(&mut socket, "command.dispatch", json!({"name": ["x"]})).await;
    assert_eq!(error(&malformed), (4000, "name must be a string"));
}

#[tokio::test]
async fn command_dispatch_names_a_quick_command_with_no_target_or_an_unknown_type() {
    let (mut socket, _home) = start().await;
    let no_target = call(&mut socket, "command.dispatch", json!({"name": "nt"})).await;
    assert_eq!(error(&no_target), (4018, "quick command /nt has no target"));
    let bogus = call(&mut socket, "command.dispatch", json!({"name": "bg"})).await;
    assert_eq!(
        error(&bogus),
        (4018, "quick command /bg has unsupported type bogus")
    );
}

/// The number in `[Pasted text #<n>: ...` and the `HHMMSS` second of day in its file name.
fn paste_number_and_second(reply: &Value) -> (u64, u64) {
    let placeholder = reply["result"]["placeholder"].as_str().unwrap();
    let number = placeholder["[Pasted text #".len()..]
        .split(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let path = std::path::Path::new(reply["result"]["path"].as_str().unwrap());
    let clock = path
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .rsplit('_')
        .next()
        .unwrap();
    let field = |at: usize| clock[at..at + 2].parse::<u64>().unwrap();
    (number, field(0) * 3600 + field(2) * 60 + field(4))
}

#[tokio::test]
async fn paste_collapse_takes_the_next_number_when_a_file_already_has_it() {
    let (mut socket, home) = start().await;
    let first = call(&mut socket, "paste.collapse", json!({"text": "first"})).await;
    let (number, second) = paste_number_and_second(&first);
    // Files an earlier run left for the next 20 numbers, at this second and the 10 after it.
    let pastes = home.path().join("pastes");
    let mut left = Vec::new();
    for taken in number + 1..=number + 20 {
        for later in second..=second + 10 {
            let s = later % 86_400;
            let name = format!(
                "paste_{taken}_{:02}{:02}{:02}.txt",
                s / 3600,
                s / 60 % 60,
                s % 60
            );
            std::fs::write(pastes.join(&name), "earlier").unwrap();
            left.push(pastes.join(name));
        }
    }
    let next = call(&mut socket, "paste.collapse", json!({"text": "next"})).await;
    let (next_number, _) = paste_number_and_second(&next);
    assert!(
        next_number > number + 20,
        "took #{next_number}, a number left on disk"
    );
    let path = std::path::PathBuf::from(next["result"]["path"].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "next");
    for file in left {
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "earlier",
            "{}",
            file.display()
        );
    }
}

#[tokio::test]
async fn paste_collapse_saves_each_paste_to_its_own_owner_only_file() {
    let (mut socket, home) = start().await;
    let reply = call(
        &mut socket,
        "paste.collapse",
        json!({"text": "one\ntwo\nthree"}),
    )
    .await;
    let result = &reply["result"];
    assert_eq!(result["lines"], 3);
    let path = std::path::PathBuf::from(result["path"].as_str().unwrap());
    assert_eq!(path.parent().unwrap(), home.path().join("pastes"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\nthree");
    let placeholder = result["placeholder"].as_str().unwrap();
    assert!(placeholder.starts_with("[Pasted text #"), "{placeholder}");
    assert!(
        placeholder.ends_with(&format!(": 3 lines \u{2192} {}]", path.display())),
        "{placeholder}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let again = call(&mut socket, "paste.collapse", json!({"text": "four"})).await;
    assert_ne!(again["result"]["path"], result["path"]);
    let empty = call(&mut socket, "paste.collapse", json!({"text": ""})).await;
    assert_eq!(error(&empty), (4004, "empty paste"));
}

/// Upstream's paste file takes the folder's access list; here it is owner-only even in a folder
/// everyone may read.
#[cfg(windows)]
#[tokio::test]
async fn paste_collapse_writes_an_owner_only_file_in_an_open_folder() {
    let icacls = |path: &std::path::Path, grant: &[&str]| {
        let out = std::process::Command::new("icacls")
            .arg(path)
            .args(grant)
            .output()
            .unwrap();
        assert!(out.status.success(), "icacls {}", path.display());
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let (mut socket, home) = start().await;
    let pastes = home.path().join("pastes");
    std::fs::create_dir(&pastes).unwrap();
    // Everyone (S-1-1-0) may read whatever the folder holds.
    icacls(&pastes, &["/grant", "*S-1-1-0:(OI)(CI)R"]);
    let reply = call(&mut socket, "paste.collapse", json!({"text": "secret"})).await;
    let path = std::path::PathBuf::from(reply["result"]["path"].as_str().unwrap());
    let acl = icacls(&path, &[]);
    assert!(!acl.contains("Everyone") && !acl.contains("(I)"), "{acl}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
}

/// Upstream follows a link in the pastes folder's place; here the paste is refused and nothing
/// is written where the link points.
#[cfg(unix)]
#[tokio::test]
async fn paste_collapse_refuses_a_link_in_the_pastes_folders_place() {
    let (mut socket, home) = start().await;
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), home.path().join("pastes")).unwrap();
    let reply = call(&mut socket, "paste.collapse", json!({"text": "secret"})).await;
    assert_eq!(reply["error"]["code"], 5021, "{reply}");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}
