#![cfg(target_os = "linux")]

#[path = "support/picker_pty.rs"]
mod picker_pty;
#[path = "support/isolated_hermes.rs"]
mod support;

use serde_json::{json, Value};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::time::Duration;
use tokio::process::Child;
use tokio::time::{sleep, timeout};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn terminal_flags(terminal: &File) -> libc::tcflag_t {
    // termios holds only integers and arrays, so all-zero bytes are a valid value.
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.as_raw_fd(), &mut attributes) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    attributes.c_lflag
}

fn start_in_pty(
    sandbox: &Path,
    args: &[&str],
    allow_loopback: bool,
) -> (Child, File, libc::tcflag_t) {
    let mut command = support::isolated_hermes(sandbox);
    command
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .args(args);
    if allow_loopback {
        command.env("NO_PROXY", "127.0.0.1");
    }
    start_command_in_pty(command)
}

fn start_command_in_pty(mut command: tokio::process::Command) -> (Child, File, libc::tcflag_t) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let dimensions = libc::winsize {
        ws_row: 32,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // openpty fills both descriptors on success. These owned files close them.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &dimensions,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    let (master, slave) = unsafe { (File::from_raw_fd(master_fd), File::from_raw_fd(slave_fd)) };
    for descriptor in [master.as_raw_fd(), slave.as_raw_fd()] {
        assert_eq!(
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) },
            0,
            "{}",
            io::Error::last_os_error()
        );
    }
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert_ne!(flags, -1, "{}", io::Error::last_os_error());
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0,
        "{}",
        io::Error::last_os_error()
    );

    let initial_flags = terminal_flags(&slave);
    command
        .env("TERM", "xterm-256color")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // The child attaches only its own PTY slave so crossterm can open /dev/tty.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("start isolated CLI in PTY");
    drop(command);
    (child, master, initial_flags)
}

async fn read_until(terminal: &mut File, expected: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let result = timeout(Duration::from_secs(12), async {
        let mut chunk = [0; 4096];
        loop {
            match terminal.read(&mut chunk) {
                Ok(0) => return false,
                Ok(read) => {
                    output.extend_from_slice(&chunk[..read]);
                    if output
                        .windows(expected.len())
                        .any(|window| window == expected)
                    {
                        return true;
                    }
                    if output.len() > 128 * 1024 {
                        output.drain(..output.len() - 64 * 1024);
                    }
                    tokio::task::yield_now().await;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    sleep(Duration::from_millis(10)).await;
                }
                // Linux reports EIO on the PTY master after its final slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return false,
                Err(error) => panic!("read the isolated PTY: {error}"),
            }
        }
    })
    .await;
    let tail = &output[output.len().saturating_sub(1400)..];
    assert!(
        matches!(result, Ok(true)),
        "missing {:?} at end of PTY output: {:?}",
        String::from_utf8_lossy(expected),
        String::from_utf8_lossy(tail)
    );
    output
}

#[tokio::test]
async fn native_tui_handles_pasted_commands_and_restores_the_terminal_on_interrupt() {
    let sandbox = tempfile::tempdir().unwrap();
    let (mut child, mut terminal, initial_flags) = start_in_pty(
        sandbox.path(),
        &["--ignore-user-config", "--ignore-rules"],
        false,
    );
    read_until(&mut terminal, b"\x1b[?1049h").await;
    terminal
        .write_all(b"\x1b[200~/objective Check the synthetic fixture\x1b[201~")
        .unwrap();
    read_until(&mut terminal, b"Pasted").await;
    terminal.write_all(b"\r").unwrap();
    let contract = sandbox.path().join("hermes/alpha/objective_contract.json");
    let objective = timeout(Duration::from_secs(12), async {
        loop {
            if let Ok(bytes) = std::fs::read(&contract) {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    break value;
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pasted objective command produces a contract");
    assert_eq!(objective["objective_text"], "Check the synthetic fixture");
    terminal.write_all(b"\x03").unwrap();
    let restored = read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(restored.windows(8).any(|bytes| bytes == b"\x1b[?2004l"));

    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("TUI shutdown timeout")
        .expect("wait for isolated CLI")
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
    assert!(!sandbox
        .path()
        .join("hermes/interactive.session.lock")
        .exists());
}

#[tokio::test]
async fn native_tui_persists_timestamps_and_inserts_the_selected_question_answer() {
    let sandbox = tempfile::tempdir().unwrap();
    let (mut child, mut terminal, initial_flags) = start_in_pty(
        sandbox.path(),
        &["--ignore-user-config", "--ignore-rules"],
        false,
    );
    read_until(&mut terminal, b"\x1b[?1049h").await;
    terminal
        .write_all(b"\x1b[200~/timestamps on\x1b[201~\r")
        .unwrap();
    read_until(&mut terminal, b"Timestamps visible").await;
    let config = std::fs::read(sandbox.path().join("hermes/config.yaml")).unwrap();
    let config: serde_json::Value = serde_yaml::from_slice(&config).unwrap();
    assert_eq!(config["display"]["timestamps"], true);

    terminal.write_all(b"\x1b[200~/ask\x1b[201~\r").unwrap();
    read_until(&mut terminal, b"Usage:").await;
    terminal
        .write_all(b"\x1b[200~/question Which fixture? | Alpha fixture | Beta fixture\x1b[201~\r")
        .unwrap();
    read_until(&mut terminal, b"Interactive Question").await;
    terminal.write_all(b"\x1b[B\r").unwrap();
    let draft = sandbox.path().join("hermes/composer-drafts.json");
    timeout(Duration::from_secs(12), async {
        loop {
            if let Ok(bytes) = std::fs::read(&draft) {
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if value["drafts"][0]["text"] == "Which fixture?\nAnswer: Beta fixture" {
                    break;
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("selected answer is persisted in the composer draft");

    // Leave the answer in the composer instead of submitting a provider turn.
    terminal.write_all(b"\x03").unwrap();
    read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("TUI shutdown timeout")
        .expect("wait for isolated CLI")
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
}

#[tokio::test]
async fn native_tui_filters_pickers_and_applies_skin_and_personality() {
    let sandbox = tempfile::tempdir().unwrap();
    let (mut child, mut terminal, initial_flags) = start_in_pty(
        sandbox.path(),
        &["--ignore-user-config", "--ignore-rules"],
        false,
    );
    read_until(&mut terminal, b"\x1b[?1049h").await;

    terminal
        .write_all(b"\x1b[200~/skin list\x1b[201~\r")
        .unwrap();
    read_until(&mut terminal, b"Select Skin").await;
    terminal
        .write_all(b"\x1b[200~ignored fixture paste\x1b[201~")
        .unwrap();
    // The terminal's incremental redraw can split status text between words.
    read_until(&mut terminal, b"Paste ignored").await;
    terminal.write_all(b"unlisted-fixture\r").unwrap();
    read_until(&mut terminal, b"Nothing").await;
    terminal.write_all(b"\x15neon-glow\r").unwrap();
    read_until(&mut terminal, b"Skin: neon-glow").await;

    terminal
        .write_all(b"\x1b[200~/personality\x1b[201~\r")
        .unwrap();
    read_until(&mut terminal, b"Select Personality").await;
    terminal.write_all(b"\x1b").unwrap();
    read_until(&mut terminal, b"Picker").await;
    terminal
        .write_all(b"\x1b[200~/personality list\x1b[201~\r")
        .unwrap();
    read_until(&mut terminal, b"Select Personality").await;
    terminal.write_all(b"concise\r").unwrap();
    read_until(&mut terminal, b"Personality: concise").await;

    terminal.write_all(b"\x1b[200~/quit\x1b[201~\r").unwrap();
    read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("TUI shutdown timeout")
        .expect("wait for isolated CLI")
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
}

#[tokio::test]
async fn native_tui_model_picker_cancels_or_persists_the_confirmed_model() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    let config = home.join("config.yaml");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        &config,
        r#"model:
  default: fixture-a
  provider: local-test
tools: []
llm_providers:
  local-test:
    base_url: http://127.0.0.1:1/v1
    api_key: local-provider-fixture
    discover_models: false
    models:
      fixture-a: {}
      fixture-b: {}
"#,
    )
    .unwrap();

    let (mut child, mut terminal, initial_flags) =
        start_in_pty(sandbox.path(), &["--ignore-rules"], false);
    read_until(&mut terminal, b"\x1b[?1049h").await;
    let original = std::fs::read(&config).unwrap();
    let open_picker = b"\x1b[200~/model local-test\x1b[201~\r";
    terminal.write_all(open_picker).unwrap();
    read_until(&mut terminal, "❯ fixture-a".as_bytes()).await;
    terminal.write_all(b"j").unwrap();
    read_until(&mut terminal, "❯ fixture-b".as_bytes()).await;
    terminal.write_all(b"q").unwrap();
    read_until(&mut terminal, b"Model switch cancelled.").await;
    assert_eq!(std::fs::read(&config).unwrap(), original);

    terminal.write_all(open_picker).unwrap();
    read_until(&mut terminal, "❯ fixture-a".as_bytes()).await;
    terminal.write_all(b"j").unwrap();
    read_until(&mut terminal, "❯ fixture-b".as_bytes()).await;
    terminal.write_all(b"\r").unwrap();
    read_until(&mut terminal, b"Persisted default model").await;
    let persisted: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(persisted["model"]["default"].as_str(), Some("fixture-b"));
    assert_eq!(persisted["model"]["provider"].as_str(), Some("local-test"));
    assert_eq!(
        persisted["model"]["base_url"].as_str(),
        Some("http://127.0.0.1:1/v1")
    );

    terminal.write_all(b"\x1b[200~/quit\x1b[201~\r").unwrap();
    let restored = read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(restored.windows(8).any(|bytes| bytes == b"\x1b[?2004l"));
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("TUI shutdown timeout")
        .expect("wait for isolated CLI")
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
}

#[tokio::test]
async fn native_tui_streams_a_loopback_turn_and_persists_the_full_reply() {
    let provider = MockServer::start().await;
    let prompt = "Read back the synthetic fixture";
    let reply = "QuartzEcho from the loopback fixture.";
    let mut stream = String::new();
    for choice in [
        json!({"index": 0, "delta": {"role": "assistant", "content": "QuartzEcho"}}),
        json!({"index": 0, "delta": {"content": " from the loopback fixture."}}),
        json!({"index": 0, "delta": {}, "finish_reason": "stop"}),
    ] {
        let chunk = json!({
            "id": "fixture-stream", "object": "chat.completion.chunk",
            "created": 0, "model": "deterministic", "choices": [choice]
        });
        stream.push_str(&format!("data: {chunk}\n\n"));
    }
    stream.push_str("data: [DONE]\n\n");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(
            json!({"model": "deterministic", "stream": true}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_raw(stream, "text/event-stream"))
        .expect(1)
        .mount(&provider)
        .await;

    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!(
            r#"model: local-test:deterministic
tools: []
llm_providers:
  local-test:
    base_url: {}
    api_key: local-provider-fixture
    discover_models: false
    models:
      deterministic: {{}}
streaming:
  enabled: true
"#,
            provider.uri()
        ),
    )
    .unwrap();

    let (mut child, mut terminal, initial_flags) =
        start_in_pty(sandbox.path(), &["--ignore-rules"], true);
    read_until(&mut terminal, b"\x1b[?1049h").await;
    write!(terminal, "\x1b[200~{prompt}\x1b[201~\r").unwrap();
    // Separate frames can insert cursor escapes between the two deltas.
    let rendered = read_until(&mut terminal, b"from the loopback fixture.").await;
    assert!(String::from_utf8_lossy(&rendered).contains("QuartzEcho"));

    timeout(Duration::from_secs(12), async {
        loop {
            let saved = std::fs::read_dir(home.join("sessions"))
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .filter_map(|entry| std::fs::read(entry.path()).ok())
                .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .any(|snapshot| {
                    snapshot["messages"].as_array().is_some_and(|messages| {
                        let conversation = messages.iter().filter_map(|message| {
                            let role = message["role"].as_str()?;
                            let content = message["content"].as_str()?;
                            matches!(role, "User" | "Assistant").then_some((role, content))
                        });
                        conversation.eq([("User", prompt), ("Assistant", reply)])
                    })
                });
            if saved {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("completed provider turn is saved in the isolated session");

    terminal.write_all(b"\x1b[200~/undo\x1b[201~\r!").unwrap();
    let edited = format!("{prompt}!");
    let draft_path = home.join("composer-drafts.json");
    timeout(Duration::from_secs(12), async {
        loop {
            if let Ok(bytes) = std::fs::read(&draft_path) {
                if let Ok(drafts) = serde_json::from_slice::<Value>(&bytes) {
                    if drafts["drafts"][0]["text"] == edited {
                        break;
                    }
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("undo restores the prompt at the cursor and stores the edit as a draft");

    terminal.write_all(b"\x03").unwrap();
    read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("TUI shutdown timeout")
        .expect("wait for isolated CLI")
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);

    let saved_draft: Value = serde_json::from_slice(&std::fs::read(&draft_path).unwrap()).unwrap();
    assert_eq!(saved_draft["drafts"][0]["text"], edited);
    let requests = provider.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: Value = requests[0].body_json().unwrap();
    let message = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|message| message["role"] == "user")
        .expect("request has a user message");
    assert_eq!(message["content"], prompt);
}

#[tokio::test]
async fn native_tui_resume_uses_the_saved_model_and_conversation() {
    let provider = MockServer::start().await;
    let first_prompt = "Where is the fixture train going?";
    let first_reply = "It is heading north.";
    let followup = "What direction did you say?";
    let reply = "ResumeFixtureAnswer";
    let stream = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "fixture-resume",
            "object": "chat.completion.chunk",
            "model": "restored",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": reply},
                "finish_reason": "stop",
            }],
        })
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(
            json!({"model": "restored", "stream": true}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_raw(stream, "text/event-stream"))
        .expect(1)
        .mount(&provider)
        .await;

    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    let sessions = home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!(
            r#"model: local-test:initial
tools: []
llm_providers:
  local-test:
    base_url: {}
    api_key: local-provider-fixture
    discover_models: false
    models: [initial, restored]
streaming:
  enabled: true
"#,
            provider.uri()
        ),
    )
    .unwrap();
    std::fs::write(
        sessions.join("session_fixture-resume.json"),
        json!({
            "session_info": {
                "session_id": "fixture-resume",
                "model": "local-test:restored",
            },
            "messages": [
                {"role": "User", "content": first_prompt},
                {"role": "Assistant", "content": first_reply},
            ],
        })
        .to_string(),
    )
    .unwrap();

    let (mut child, mut terminal, initial_flags) = start_in_pty(
        sandbox.path(),
        &["--ignore-rules", "resume", "fixture-resume"],
        true,
    );
    read_until(&mut terminal, b"Resumed session").await;
    write!(terminal, "\x1b[200~{followup}\x1b[201~\r").unwrap();
    read_until(&mut terminal, reply.as_bytes()).await;
    terminal.write_all(b"\x03").unwrap();
    read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("resumed TUI shutdown timeout")
        .unwrap()
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);

    let requests = provider.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: Value = requests[0].body_json().unwrap();
    let history = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| {
            let role = message["role"].as_str()?;
            matches!(role, "user" | "assistant").then(|| (role, message["content"].as_str()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        history,
        [
            ("user", Some(first_prompt)),
            ("assistant", Some(first_reply)),
            ("user", Some(followup)),
        ]
    );
}

#[tokio::test]
async fn native_tui_interrupt_retains_the_inflight_prompt_and_the_next_draft() {
    assert_native_tui_shutdown_keeps_prompt_and_draft(|_, terminal| {
        terminal.write_all(b"\x03").unwrap();
    })
    .await;
}

#[tokio::test]
async fn native_tui_sigterm_retains_the_inflight_prompt_and_the_next_draft() {
    assert_native_tui_shutdown_keeps_prompt_and_draft(|child, _| {
        let pid = i32::try_from(child.id().expect("isolated TUI child has a PID")).unwrap();
        assert!(pid > 0);
        // Only signal the isolated child, which is still owned and awaited by this test.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    })
    .await;
}

async fn assert_native_tui_shutdown_keeps_prompt_and_draft(
    shut_down: impl FnOnce(&Child, &mut File),
) {
    use tokio::io::AsyncBufReadExt;

    let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!(
            r#"model: local-fixture:deterministic
tools: []
llm_providers:
  local-fixture:
    base_url: http://{}
    api_key: fixture-inflight-value
    discover_models: false
    models:
      deterministic: {{}}
streaming:
  enabled: true
"#,
            provider.local_addr().unwrap()
        ),
    )
    .unwrap();

    let (mut child, mut terminal, initial_flags) =
        start_in_pty(sandbox.path(), &["--ignore-rules"], true);
    read_until(&mut terminal, b"\x1b[?1049h").await;
    let in_flight = "Read the first synthetic fixture";
    write!(terminal, "\x1b[200~{in_flight}\x1b[201~\r").unwrap();
    let (connection, _) = timeout(Duration::from_secs(12), provider.accept())
        .await
        .expect("first request reaches the local provider")
        .unwrap();
    let mut connection = tokio::io::BufReader::new(connection);
    let mut request = String::new();
    timeout(Duration::from_secs(5), connection.read_line(&mut request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.trim_end(), "POST /chat/completions HTTP/1.1");

    // Keep the response pending so attempting another submission exercises the busy state.
    let draft = "Keep the second fixture for later";
    write!(terminal, "\x1b[200~{draft}\x1b[201~\r").unwrap();
    let draft_path = home.join("composer-drafts.json");
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(bytes) = std::fs::read(&draft_path) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if value["drafts"][0]["text"] == draft {
                        break;
                    }
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pasting the next prompt stores a draft during the active turn");
    shut_down(&child, &mut terminal);
    read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);

    let snapshots = std::fs::read_dir(home.join("sessions")).unwrap();
    let messages = snapshots
        .map(Result::unwrap)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice::<Value>(&std::fs::read(entry.path()).unwrap()).unwrap()
        })
        .find_map(|snapshot| {
            let messages = snapshot["messages"].as_array()?;
            messages
                .iter()
                .any(|message| message["content"] == in_flight)
                .then(|| messages.clone())
        })
        .expect("interrupted turn is saved");
    assert!(!messages
        .iter()
        .any(|message| message["role"] == "Assistant" || message["content"] == draft));
    let drafts: Value = serde_json::from_slice(&std::fs::read(draft_path).unwrap()).unwrap();
    assert_eq!(drafts["drafts"][0]["text"], draft);
    assert!(timeout(Duration::from_millis(100), provider.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn native_tui_plan_controls_set_depth_and_block_an_unsupported_task() {
    let sandbox = tempfile::tempdir().unwrap();
    let (mut child, mut terminal, initial_flags) = start_in_pty(
        sandbox.path(),
        &[
            "--ignore-user-config",
            "--ignore-rules",
            "--model",
            "openai:fixture-plan-no-reason",
        ],
        false,
    );
    read_until(&mut terminal, b"\x1b[?1049h").await;

    for (command, expected) in [
        ("/plan", "Planner controls"),
        ("/plan caps", "mode=off"),
        ("/plan depth list", "Task depth profiles"),
        ("/plan depth thorough", "max_iterations=120"),
        ("/plan depth exhaustive", "max_iterations=250"),
        ("/plan depth clear", "max_iterations=50"),
        ("/plan status", "queued=0"),
        ("/plan caps enforce", "router set to"),
        ("/plan Audit the fixture", "BLOCKED"),
    ] {
        write!(terminal, "\x1b[200~{command}\x1b[201~\r").unwrap();
        read_until(&mut terminal, expected.as_bytes()).await;
    }
    let jobs = sandbox.path().join("hermes/background_jobs");
    assert!(!jobs.exists() || std::fs::read_dir(jobs).unwrap().next().is_none());

    terminal.write_all(b"\x1b[200~/quit\x1b[201~\r").unwrap();
    read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("TUI shutdown timeout")
        .expect("wait for isolated CLI")
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
}

#[tokio::test]
async fn gateway_setup_leaves_the_existing_config_unchanged_when_cancelled() {
    let sandbox = tempfile::tempdir().unwrap();
    let config = sandbox.path().join("hermes/config.yaml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let existing = b"platforms:\n  api_server:\n    enabled: true\n    port: 18090\n";
    std::fs::write(&config, existing).unwrap();

    let (mut child, mut terminal, initial_flags) =
        start_in_pty(sandbox.path(), &["gateway", "setup"], false);
    read_until(&mut terminal, "[✓] API Server".as_bytes()).await;
    terminal.write_all(b"k ").unwrap();
    read_until(&mut terminal, b"none selected").await;
    terminal.write_all(b"q").unwrap();
    let restored = read_until(&mut terminal, b"Gateway setup cancelled.").await;
    assert!(restored.windows(8).any(|bytes| bytes == b"\x1b[?1049l"));
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("gateway setup shutdown timeout")
        .expect("wait for isolated CLI")
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
    assert_eq!(std::fs::read(config).unwrap(), existing);
}

#[tokio::test]
async fn gateway_setup_saves_the_selected_loopback_api_server() {
    let sandbox = tempfile::tempdir().unwrap();
    let (mut child, mut terminal, initial_flags) =
        start_in_pty(sandbox.path(), &["gateway", "setup"], false);
    read_until(&mut terminal, b"Select platforms to configure").await;
    terminal.write_all(b"kjk ").unwrap();
    read_until(&mut terminal, "[✓] API Server".as_bytes()).await;
    terminal.write_all(b"\r").unwrap();
    let restored = read_until(&mut terminal, b"API server host (default 127.0.0.1):").await;
    assert!(restored.windows(8).any(|bytes| bytes == b"\x1b[?1049l"));
    assert_eq!(terminal_flags(&terminal), initial_flags);

    terminal.write_all(b"127.0.0.1\r").unwrap();
    read_until(&mut terminal, b"API server port (default 8090):").await;
    terminal.write_all(b"18090\r").unwrap();
    read_until(
        &mut terminal,
        b"API server auth_token (required for non-loopback host):",
    )
    .await;
    terminal.write_all(b"\r").unwrap();
    read_until(&mut terminal, b"Gateway setup complete.").await;
    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("gateway setup shutdown timeout")
        .expect("wait for isolated CLI")
        .success());

    let config = std::fs::read(sandbox.path().join("hermes/config.yaml")).unwrap();
    let config: serde_json::Value = serde_yaml::from_slice(&config).unwrap();
    let platforms = config["platforms"].as_object().unwrap();
    assert_eq!(platforms.len(), 1);
    let api = &platforms["api_server"];
    assert_eq!(api["enabled"], true);
    assert_eq!(api["host"], "127.0.0.1");
    assert_eq!(api["port"], 18090);
    assert!(api.get("auth_token").is_none());
}
