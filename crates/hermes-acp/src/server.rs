//! ACP server (JSON-RPC over stdin/stdout).
//!
//! Full implementation with:
//! - Agent capabilities declaration
//! - Session state machine (create/resume/complete/cancel)
//! - Message streaming support
//! - Tool approval callback integration
//! - MCP server config block generation
//! - Progress events (thinking, tool_call, tool_result steps)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinSet;

use crate::events::{AcpEvent, EventSink};
use crate::handler::AcpHandler;
use crate::permissions::PermissionStore;
use crate::protocol::{AcpMethod, AcpRequest, AcpResponse};
use crate::session::{SessionManager, SessionPhase};

fn flush_events_before_response(method: &str) -> bool {
    matches!(
        method,
        "session/load" | "load_session" | "session/resume" | "resume_session"
    )
}

/// ACP JSON-RPC server with full protocol support.
pub struct AcpServer {
    handler: Arc<dyn AcpHandler>,
    event_sink: Arc<EventSink>,
    session_manager: Arc<SessionManager>,
    permission_store: Arc<PermissionStore>,
}

impl AcpServer {
    pub fn new(handler: Arc<dyn AcpHandler>) -> Self {
        Self {
            handler,
            event_sink: Arc::new(EventSink::default()),
            session_manager: Arc::new(SessionManager::new()),
            permission_store: Arc::new(PermissionStore::new()),
        }
    }

    /// Create a server with shared session manager and event sink.
    pub fn with_components(
        handler: Arc<dyn AcpHandler>,
        session_manager: Arc<SessionManager>,
        event_sink: Arc<EventSink>,
        permission_store: Arc<PermissionStore>,
    ) -> Self {
        Self {
            handler,
            event_sink,
            session_manager,
            permission_store,
        }
    }

    /// Get a reference to the event sink.
    pub fn event_sink(&self) -> &Arc<EventSink> {
        &self.event_sink
    }

    /// Get a reference to the session manager.
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get a reference to the permission store.
    pub fn permission_store(&self) -> &Arc<PermissionStore> {
        &self.permission_store
    }

    /// Run the server, reading JSON-RPC requests from stdin and writing
    /// responses to stdout.
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!("ACP server started, listening on stdin");
        self.run_on(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Run the server on a reader and writer (stdio, tests or another
    /// transport).
    ///
    /// A prompt runs in the background: its `session/update` notifications
    /// are written as they are pushed and before its response, and
    /// `session/cancel`, `/steer` and other requests are handled whilst it
    /// runs. A request that starts a session (new, load, resume or fork)
    /// also runs in the background, as starting the MCP servers it names
    /// can be slow. Other requests are handled one at a time in arrival
    /// order. A notification (a request without an `id`) gets no response.
    /// Returns once the reader ends and every background request has
    /// answered.
    pub async fn run_on<R, W>(
        &self,
        reader: R,
        mut writer: W,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let outbox: Outbox = Arc::new(Mutex::new(tx));
        let mut lines = BufReader::new(reader).lines();
        let mut prompts = Prompts::default();
        let mut reading = true;

        loop {
            if !reading && prompts.tasks.is_empty() {
                send_events(&lock(&outbox), self.event_sink.drain_all());
                while let Ok(line) = rx.try_recv() {
                    write_line(&mut writer, &line).await?;
                }
                writer.flush().await?;
                return Ok(());
            }
            // Unbiased, so a stream of updates cannot hold off reading a cancel.
            tokio::select! {
                Some(line) = rx.recv() => {
                    write_line(&mut writer, &line).await?;
                    if rx.is_empty() {
                        writer.flush().await?;
                    }
                }
                Some(joined) = prompts.tasks.join_next_with_id(), if !prompts.tasks.is_empty() => {
                    match joined {
                        Ok((id, ())) => {
                            prompts.requests.remove(&id);
                        }
                        Err(error) => self.prompt_failed(&mut prompts, error, &outbox),
                    }
                }
                line = lines.next_line(), if reading => match line? {
                    Some(line) => self.dispatch(&line, &outbox, &mut prompts).await,
                    None => reading = false,
                },
            }
        }
    }

    /// A prompt task that panicked: answer its request with an error, mark
    /// its session failed, and keep serving the others.
    fn prompt_failed(&self, prompts: &mut Prompts, error: tokio::task::JoinError, outbox: &Outbox) {
        tracing::error!("ACP prompt task failed: {error}");
        let (request_id, session_id, seq) =
            prompts.requests.remove(&error.id()).unwrap_or_default();
        // Ends the session's turn only if it was this prompt's.
        if let Some(seq) = seq {
            self.session_manager
                .finish_turn(&session_id, seq, SessionPhase::Failed);
        }
        if request_id.is_some() {
            send(
                &lock(outbox),
                &AcpResponse::error(request_id, -32603, "Internal error: the prompt failed"),
            );
        }
    }

    async fn dispatch(&self, line: &str, outbox: &Outbox, prompts: &mut Prompts) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let request: AcpRequest = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(e) => {
                let error = AcpResponse::error(None, -32700, format!("Parse error: {}", e));
                send(&lock(outbox), &error);
                return;
            }
        };

        if AcpMethod::from(request.method.as_str()) == AcpMethod::Prompt {
            let session_id = prompt_session_id(&request);
            // Numbered before anything else is read, so a cancel that follows
            // covers this prompt even if its turn has not started.
            let seq = self.session_manager.prompt_received(&session_id);
            let request_id = request.id.clone();
            let task = prompts.tasks.spawn(run_prompt(
                self.handler.clone(),
                self.event_sink.clone(),
                outbox.clone(),
                session_id.clone(),
                seq,
                request,
            ));
            prompts
                .requests
                .insert(task.id(), (request_id, session_id, seq));
            return;
        }

        if starts_mcp_servers(&request.method) {
            // Starting a client's MCP servers can take up to their connect
            // timeout, so it must not hold off a cancel or other requests.
            let request_id = request.id.clone();
            let task = prompts.tasks.spawn(answer(
                self.handler.clone(),
                self.event_sink.clone(),
                outbox.clone(),
                request,
            ));
            prompts
                .requests
                .insert(task.id(), (request_id, String::new(), None));
            return;
        }

        answer(
            self.handler.clone(),
            self.event_sink.clone(),
            outbox.clone(),
            request,
        )
        .await;
    }
}

/// The requests that start the MCP servers a client names.
fn starts_mcp_servers(method: &str) -> bool {
    matches!(
        AcpMethod::from(method),
        AcpMethod::NewSession
            | AcpMethod::LoadSession
            | AcpMethod::ResumeSession
            | AcpMethod::ForkSession
    )
}

/// Handle a request other than a prompt and write its response with the
/// session updates it generated.
async fn answer(
    handler: Arc<dyn AcpHandler>,
    event_sink: Arc<EventSink>,
    outbox: Outbox,
    request: AcpRequest,
) {
    let flush_before_response = flush_events_before_response(&request.method);
    let notification = request.id.is_none();
    let response = handler.handle_request(request).await;
    let out = lock(&outbox);
    if flush_before_response {
        send_events(&out, event_sink.drain_all());
    }
    if !notification {
        send(&out, &response);
    }
    if !flush_before_response {
        // Most responses are followed by the session/update
        // notifications the handler generated.
        send_events(&out, event_sink.drain_all());
    }
}

/// Requests running in the background (prompts and session starts), with
/// the request and session of each, so one that panics can still be
/// answered.
#[derive(Default)]
struct Prompts {
    tasks: JoinSet<()>,
    requests: HashMap<tokio::task::Id, (Option<serde_json::Value>, String, Option<u64>)>,
}

fn prompt_session_id(request: &AcpRequest) -> String {
    request
        .params
        .as_ref()
        .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Output lines waiting for the writer. Holding the lock across a drain of
/// the event sink and its sends keeps updates in the order they were pushed.
type Outbox = Arc<Mutex<UnboundedSender<String>>>;

fn lock(outbox: &Outbox) -> std::sync::MutexGuard<'_, UnboundedSender<String>> {
    outbox
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run one prompt, writing its session's updates as they are pushed, then
/// the rest of them and its response.
async fn run_prompt(
    handler: Arc<dyn AcpHandler>,
    event_sink: Arc<EventSink>,
    outbox: Outbox,
    session_id: String,
    seq: Option<u64>,
    request: AcpRequest,
) {
    let notification = request.id.is_none();
    let turn = crate::session::PROMPT_SEQ.scope(seq, handler.handle_request(request));
    tokio::pin!(turn);
    let response = loop {
        let pushed = event_sink.pushed();
        tokio::pin!(pushed);
        pushed.as_mut().enable();
        send_events(&lock(&outbox), event_sink.drain_for_session(&session_id));
        tokio::select! {
            response = &mut turn => break response,
            _ = pushed => {}
        }
    };
    let out = lock(&outbox);
    send_events(&out, event_sink.drain_for_session(&session_id));
    if !notification {
        send(&out, &response);
    }
}

fn send(out: &UnboundedSender<String>, value: &impl serde::Serialize) {
    match serde_json::to_string(value) {
        // The receiver lives as long as the read loop; a failed send means
        // the server is returning and nothing more can be written.
        Ok(json) => {
            let _ = out.send(json);
        }
        Err(e) => tracing::error!("ACP output not serialisable: {e}"),
    }
}

/// Send each event that has an ACP form as a `session/update` notification.
fn send_events(out: &UnboundedSender<String>, events: Vec<AcpEvent>) {
    for params in events.iter().filter_map(AcpEvent::session_update_params) {
        send(
            out,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": params,
            }),
        );
    }
}

async fn write_line(writer: &mut (impl AsyncWrite + Unpin), line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await
}

/// Generate an MCP server config block for embedding in an ACP session.
///
/// This creates a stdio-based MCP server entry that points to the hermes
/// binary, suitable for inclusion in the ACP session's `mcp_servers` list.
pub fn generate_mcp_server_config(hermes_binary: &str) -> serde_json::Value {
    serde_json::json!({
        "hermes": {
            "command": hermes_binary,
            "args": ["mcp", "serve"],
            "env": {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AcpEvent;
    use crate::handler::HermesAcpHandler;
    use crate::permissions::{build_permission_request, PermissionOutcome};

    #[tokio::test]
    async fn run_on_flushes_pending_events_after_each_response() {
        let session_manager = Arc::new(SessionManager::new());
        let event_sink = Arc::new(EventSink::default());
        let permission_store = Arc::new(PermissionStore::new());
        event_sink.push(AcpEvent::tool_call_start(
            "session-1",
            "tc-read",
            "read_file",
            Some(serde_json::json!({"path": "/tmp/a.txt"})),
        ));

        let handler = Arc::new(HermesAcpHandler::new(
            session_manager.clone(),
            event_sink.clone(),
            permission_store.clone(),
        ));
        let server =
            AcpServer::with_components(handler, session_manager, event_sink, permission_store);

        let input = br#"{"jsonrpc":"2.0","id":1,"method":"status.get"}"#;
        let mut output = Vec::new();
        server.run_on(&input[..], &mut output).await.unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["result"]["status"], "ready");
        assert_eq!(lines[1]["method"], "session/update");
        let params = &lines[1]["params"];
        assert_eq!(params["sessionId"], "session-1");
        assert_eq!(params["update"]["sessionUpdate"], "tool_call");
        assert_eq!(params["update"]["toolCallId"], "tc-read");
        assert_eq!(params["update"]["kind"], "read");
        assert_eq!(params["update"]["title"], "read: /tmp/a.txt");
        assert_eq!(params["update"]["rawInput"]["path"], "/tmp/a.txt");
    }

    #[tokio::test]
    async fn run_on_advertises_available_commands_after_new_session() {
        let session_manager = Arc::new(SessionManager::new());
        let event_sink = Arc::new(EventSink::default());
        let permission_store = Arc::new(PermissionStore::new());
        let handler = Arc::new(HermesAcpHandler::new(
            session_manager.clone(),
            event_sink.clone(),
            permission_store.clone(),
        ));
        let server =
            AcpServer::with_components(handler, session_manager, event_sink, permission_store);

        let input = br#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp"}}"#;
        let mut output = Vec::new();
        server.run_on(&input[..], &mut output).await.unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(lines.len(), 3);
        let session_id = lines[0]["result"]["sessionId"].as_str().unwrap();
        assert_eq!(lines[1]["method"], "session/update");
        assert_eq!(lines[1]["params"]["sessionId"], session_id);
        let update = &lines[1]["params"]["update"];
        assert_eq!(update["sessionUpdate"], "available_commands_update");
        let commands = update["availableCommands"]
            .as_array()
            .expect("available commands");
        assert!(commands.iter().any(|command| command["name"] == "help"));
        let model = commands
            .iter()
            .find(|command| command["name"] == "model")
            .expect("model command");
        assert_eq!(model["inputHint"], "model name to switch to");
        assert_eq!(lines[2]["method"], "session/update");
        assert_eq!(lines[2]["params"]["sessionId"], session_id);
        let update = &lines[2]["params"]["update"];
        assert_eq!(update["sessionUpdate"], "usage_update");
        assert!(update["size"].as_u64().unwrap() > 0);
        assert!(update["used"].as_u64().is_some());
    }

    #[tokio::test]
    async fn run_on_replays_load_history_before_response() {
        let session_manager = Arc::new(SessionManager::new());
        let event_sink = Arc::new(EventSink::default());
        let permission_store = Arc::new(PermissionStore::new());
        let state = session_manager.create_session("/tmp");
        let session_id = state.session_id;
        session_manager.set_history(
            &session_id,
            vec![
                serde_json::json!({"role": "user", "content": "restore this thread"}),
                serde_json::json!({"role": "assistant", "content": "thread restored"}),
            ],
        );
        let handler = Arc::new(HermesAcpHandler::new(
            session_manager.clone(),
            event_sink.clone(),
            permission_store.clone(),
        ));
        let server =
            AcpServer::with_components(handler, session_manager, event_sink, permission_store);

        let input = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"session/load","params":{{"sessionId":"{session_id}","cwd":"/tmp"}}}}"#
        );
        let mut output = Vec::new();
        server.run_on(input.as_bytes(), &mut output).await.unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(lines.len(), 5);
        let update = |n: usize| &lines[n]["params"]["update"];
        assert_eq!(lines[0]["method"], "session/update");
        assert_eq!(lines[0]["params"]["sessionId"], session_id.as_str());
        assert_eq!(update(0)["sessionUpdate"], "user_message_chunk");
        assert_eq!(update(0)["content"]["text"], "restore this thread");
        assert_eq!(lines[1]["method"], "session/update");
        assert_eq!(update(1)["sessionUpdate"], "agent_message_chunk");
        assert_eq!(update(1)["content"]["text"], "thread restored");
        assert_eq!(lines[2]["method"], "session/update");
        assert_eq!(update(2)["sessionUpdate"], "available_commands_update");
        assert_eq!(lines[3]["method"], "session/update");
        assert_eq!(update(3)["sessionUpdate"], "usage_update");
        assert!(update(3)["size"].as_u64().unwrap() > 0);
        assert!(update(3)["used"].as_u64().unwrap() > 0);
        assert_eq!(lines[4]["id"], 1);
        assert_eq!(lines[4]["result"], serde_json::json!({}));
    }

    /// Pushes a reply delta to the sink, then waits until released or
    /// cancelled, as a streaming model call does.
    struct GatedExecutor {
        event_sink: Arc<EventSink>,
        release: Arc<tokio::sync::Notify>,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::handler::AcpPromptExecutor for GatedExecutor {
        async fn execute_prompt(
            &self,
            session: &crate::session::SessionState,
            _user_text: &str,
            _history: &[serde_json::Value],
        ) -> Result<crate::handler::PromptExecutionOutput, String> {
            self.event_sink
                .push(AcpEvent::message_delta(&session.session_id, "partial"));
            self.release.notified().await;
            Ok(crate::handler::PromptExecutionOutput {
                response_text: "partial".to_string(),
                streamed_text: true,
                ..Default::default()
            })
        }

        fn cancel_prompt(&self, _session: &crate::session::SessionState) {
            self.cancelled
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.release.notify_one();
        }
    }

    struct Client {
        lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Client {
        async fn send(&mut self, request: serde_json::Value) {
            let line = format!("{request}\n");
            self.writer.write_all(line.as_bytes()).await.unwrap();
        }

        async fn next(&mut self) -> serde_json::Value {
            let line =
                tokio::time::timeout(std::time::Duration::from_secs(10), self.lines.next_line())
                    .await
                    .expect("server wrote nothing for 10 s")
                    .unwrap()
                    .expect("server closed its output");
            serde_json::from_str(&line).unwrap()
        }

        /// Read until the response to `id`, returning the lines before it
        /// and the response.
        async fn until_response(&mut self, id: u64) -> (Vec<serde_json::Value>, serde_json::Value) {
            let mut before = Vec::new();
            loop {
                let line = self.next().await;
                if line.get("method").is_none() {
                    assert_eq!(line["id"], id, "unexpected response {line}");
                    return (before, line);
                }
                before.push(line);
            }
        }
    }

    fn gated_client() -> (
        Client,
        Arc<tokio::sync::Notify>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let event_sink = Arc::new(EventSink::default());
        let release = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executor = Arc::new(GatedExecutor {
            event_sink: event_sink.clone(),
            release: release.clone(),
            cancelled: cancelled.clone(),
        });
        (client_with(executor, event_sink), release, cancelled)
    }

    fn client_with(
        executor: Arc<dyn crate::handler::AcpPromptExecutor>,
        event_sink: Arc<EventSink>,
    ) -> Client {
        let session_manager = Arc::new(SessionManager::new());
        let permission_store = Arc::new(PermissionStore::new());
        let handler = Arc::new(
            HermesAcpHandler::new(
                session_manager.clone(),
                event_sink.clone(),
                permission_store.clone(),
            )
            .with_prompt_executor(executor),
        );
        let server = Arc::new(AcpServer::with_components(
            handler,
            session_manager,
            event_sink,
            permission_store,
        ));
        let (client_io, server_io) = tokio::io::duplex(1 << 16);
        let (server_read, server_write) = tokio::io::split(server_io);
        let (client_read, writer) = tokio::io::split(client_io);
        let server = tokio::spawn(async move {
            server.run_on(server_read, server_write).await.unwrap();
        });
        Client {
            lines: BufReader::new(client_read).lines(),
            writer,
            server,
        }
    }

    async fn new_session(client: &mut Client) -> String {
        client
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": {"cwd": "/tmp"}}))
            .await;
        let (_, response) = client.until_response(1).await;
        response["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn prompt_request(session_id: &str) -> serde_json::Value {
        prompt_with_id(session_id, 2)
    }

    fn prompt_with_id(session_id: &str, id: u64) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "hello"}]},
        })
    }

    fn is_reply_chunk(line: &serde_json::Value, text: &str) -> bool {
        line["method"] == "session/update"
            && line["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
            && line["params"]["update"]["content"]["text"] == text
    }

    #[tokio::test]
    async fn run_on_streams_prompt_updates_before_the_turn_ends() {
        let (mut client, release, _) = gated_client();
        let session_id = new_session(&mut client).await;
        client.send(prompt_request(&session_id)).await;

        // The prompt is still waiting for its release, so the update must
        // arrive whilst the turn runs, not with its response.
        loop {
            let line = client.next().await;
            assert!(
                line.get("method").is_some(),
                "response before release: {line}"
            );
            if is_reply_chunk(&line, "partial") {
                assert_eq!(line["params"]["sessionId"], session_id.as_str());
                break;
            }
        }

        release.notify_one();
        let (before, response) = client.until_response(2).await;
        assert_eq!(response["result"]["stopReason"], "end_turn");
        assert!(
            !before.iter().any(|line| is_reply_chunk(line, "partial")),
            "a streamed reply must not be sent again"
        );

        // Dropping one half of a split stream does not close it.
        client.writer.shutdown().await.unwrap();
        client.server.await.unwrap();
    }

    #[tokio::test]
    async fn run_on_cancels_a_running_prompt() {
        let (mut client, _, cancelled) = gated_client();
        let session_id = new_session(&mut client).await;
        client.send(prompt_request(&session_id)).await;
        while !is_reply_chunk(&client.next().await, "partial") {}

        // session/cancel is a notification: it has no id and gets no reply.
        client
            .send(serde_json::json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}))
            .await;
        let (_, response) = client.until_response(2).await;
        assert_eq!(response["result"]["stopReason"], "cancelled");
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));

        // Dropping one half of a split stream does not close it.
        client.writer.shutdown().await.unwrap();
        while let Ok(Some(line)) = client.lines.next_line().await {
            let line: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert!(
                line.get("method").is_some(),
                "reply to a notification: {line}"
            );
        }
        client.server.await.unwrap();
    }

    /// A session whose MCP server never answers must not hold off a cancel
    /// for another session or later requests.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_on_serves_other_requests_whilst_a_session_starts_its_mcp_servers() {
        let (mut client, _, cancelled) = gated_client();
        let session_id = new_session(&mut client).await;
        client.send(prompt_request(&session_id)).await;
        while !is_reply_chunk(&client.next().await, "partial") {}

        // `sleep` starts and never answers the MCP handshake.
        client
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "session/new",
                "params": {"cwd": "/tmp", "mcpServers": [
                    {"type": "stdio", "name": "silent", "command": "sleep", "args": ["30"], "env": []}
                ]},
            }))
            .await;
        client
            .send(serde_json::json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}))
            .await;
        let (_, response) = client.until_response(2).await;
        assert_eq!(response["result"]["stopReason"], "cancelled");
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));

        client
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "session/list", "params": {}}))
            .await;
        let (_, response) = client.until_response(4).await;
        assert!(response["result"]["sessions"].is_array(), "{response}");

        // Dropping the server kills the silent server.
        client.server.abort();
    }

    #[tokio::test]
    async fn run_on_cancels_a_prompt_whose_turn_has_not_started() {
        let (mut client, release, _) = gated_client();
        let session_id = new_session(&mut client).await;
        // Back to back: the cancel can be read before the turn starts.
        client.send(prompt_request(&session_id)).await;
        client
            .send(serde_json::json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}))
            .await;
        let (_, response) = client.until_response(2).await;
        assert_eq!(response["result"]["stopReason"], "cancelled");

        // The cancel covered only the prompts before it: the next one runs.
        client.send(prompt_with_id(&session_id, 3)).await;
        while !is_reply_chunk(&client.next().await, "partial") {}
        release.notify_one();
        let (_, response) = client.until_response(3).await;
        assert_eq!(response["result"]["stopReason"], "end_turn");

        client.writer.shutdown().await.unwrap();
        client.server.await.unwrap();
    }

    /// Records each prompt and holds its turn until released; a cancel does
    /// not release it, as a model call that ignores the interrupt would not.
    struct HoldingExecutor {
        event_sink: Arc<EventSink>,
        release: Arc<tokio::sync::Notify>,
        runs: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::handler::AcpPromptExecutor for HoldingExecutor {
        async fn execute_prompt(
            &self,
            session: &crate::session::SessionState,
            user_text: &str,
            _history: &[serde_json::Value],
        ) -> Result<crate::handler::PromptExecutionOutput, String> {
            self.runs.lock().unwrap().push(user_text.to_string());
            self.event_sink
                .push(AcpEvent::message_delta(&session.session_id, "partial"));
            self.release.notified().await;
            Ok(crate::handler::PromptExecutionOutput {
                response_text: "partial".to_string(),
                streamed_text: true,
                ..Default::default()
            })
        }
    }

    fn text_prompt(session_id: &str, id: u64, text: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": text}]},
        })
    }

    #[tokio::test]
    async fn run_on_keeps_a_cancelled_turn_its_own_and_runs_no_follow_up() {
        let event_sink = Arc::new(EventSink::default());
        let release = Arc::new(tokio::sync::Notify::new());
        let runs = Arc::new(Mutex::new(Vec::new()));
        let mut client = client_with(
            Arc::new(HoldingExecutor {
                event_sink: event_sink.clone(),
                release: release.clone(),
                runs: runs.clone(),
            }),
            event_sink,
        );
        let session_id = new_session(&mut client).await;
        client.send(text_prompt(&session_id, 2, "first")).await;
        while !is_reply_chunk(&client.next().await, "partial") {}

        client
            .send(text_prompt(&session_id, 3, "/queue follow up"))
            .await;
        let (_, queued) = client.until_response(3).await;
        assert_eq!(queued["result"]["stopReason"], "end_turn");

        // A cancel whilst the turn still runs, then another prompt: it is
        // queued behind the turn, not run and not cancelled.
        client
            .send(serde_json::json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}))
            .await;
        client.send(text_prompt(&session_id, 4, "second")).await;
        let (_, second) = client.until_response(4).await;
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        // Nor does /steer or /queue start a second turn after the cancel.
        client
            .send(text_prompt(&session_id, 5, "/steer do it differently"))
            .await;
        let (_, steer) = client.until_response(5).await;
        assert_eq!(steer["result"]["stopReason"], "end_turn", "{steer}");
        client
            .send(text_prompt(&session_id, 6, "/queue later"))
            .await;
        let (_, queue) = client.until_response(6).await;
        assert_eq!(queue["result"]["stopReason"], "end_turn", "{queue}");
        assert_eq!(runs.lock().unwrap().as_slice(), ["first"]);

        release.notify_one();
        let (_, first) = client.until_response(2).await;
        assert_eq!(first["result"]["stopReason"], "cancelled", "{first}");
        assert_eq!(runs.lock().unwrap().as_slice(), ["first"]);

        client.writer.shutdown().await.unwrap();
        client.server.await.unwrap();
    }

    struct PanickingExecutor;

    #[async_trait::async_trait]
    impl crate::handler::AcpPromptExecutor for PanickingExecutor {
        async fn execute_prompt(
            &self,
            _session: &crate::session::SessionState,
            _user_text: &str,
            _history: &[serde_json::Value],
        ) -> Result<crate::handler::PromptExecutionOutput, String> {
            panic!("executor failed");
        }
    }

    #[tokio::test]
    async fn run_on_answers_a_prompt_whose_task_panicked_and_keeps_serving() {
        let mut client = client_with(Arc::new(PanickingExecutor), Arc::new(EventSink::default()));
        let session_id = new_session(&mut client).await;
        client.send(prompt_request(&session_id)).await;
        let (_, response) = client.until_response(2).await;
        assert_eq!(response["error"]["code"], -32603, "{response}");

        // The session is not left running: the next prompt is tried, not
        // queued, and the server still answers other requests.
        client.send(prompt_with_id(&session_id, 3)).await;
        let (_, response) = client.until_response(3).await;
        assert_eq!(response["error"]["code"], -32603, "{response}");
        client
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "session/new", "params": {"cwd": "/tmp"}}))
            .await;
        let (_, response) = client.until_response(4).await;
        assert!(response["result"]["sessionId"].is_string(), "{response}");

        client.writer.shutdown().await.unwrap();
        client.server.await.unwrap();
    }

    /// Prints each `session/update` kind this server sends, one per line
    /// after `ACP-UPDATE `, for a check against the ACP SDK's schema
    /// (run with `--nocapture`).
    #[test]
    fn every_session_update_kind_has_its_protocol_form() {
        let plan = crate::events::plan_entries_from_todo_result(Some(
            r#"{"todos":[{"id":"1","content":"Write the test","status":"in_progress"},{"id":"2","content":"Ship it","status":"pending"}]}"#,
        ))
        .expect("plan entries");
        let events = [
            AcpEvent::agent_thought_chunk("s1", "weighing it"),
            AcpEvent::user_message_chunk("s1", "hello"),
            AcpEvent::message_delta("s1", "Hel"),
            AcpEvent::message_complete("s1", "Hello."),
            AcpEvent::tool_call_start(
                "s1",
                "tc1",
                "read_file",
                Some(serde_json::json!({"path": "src/main.rs"})),
            ),
            AcpEvent::tool_call_start(
                "s1",
                "tc2",
                "terminal",
                Some(serde_json::json!({"command": "cargo test"})),
            ),
            AcpEvent::tool_call_start(
                "s1",
                "tc3",
                "patch",
                Some(serde_json::json!({"path": "a.rs", "old_string": "a", "new_string": "b"})),
            ),
            AcpEvent::tool_call_complete("s1", "tc1", "read_file", Some("fn main() {}".into())),
            AcpEvent::tool_call_complete(
                "s1",
                "tc2",
                "terminal",
                Some(r#"{"error":"exit 1"}"#.into()),
            ),
            AcpEvent::tool_call_complete("s1", "tc3", "patch", None),
            AcpEvent::plan_update("s1", plan),
            AcpEvent::usage_update("s1", 200_000, 1_234),
            AcpEvent::available_commands_update(
                "s1",
                vec![crate::protocol::AvailableCommand {
                    name: "queue".into(),
                    description: "Queue a prompt for the next turn".into(),
                    input_hint: Some("prompt".into()),
                }],
            ),
            AcpEvent::session_info_update(
                "s1",
                Some("Fix the build".into()),
                "2026-11-17T00:00:00Z".into(),
                None,
            ),
        ];
        for event in &events {
            let params = event.session_update_params().expect("protocol form");
            assert_eq!(params["sessionId"], "s1");
            assert!(params["update"]["sessionUpdate"].is_string(), "{params}");
            println!("ACP-UPDATE {params}");
        }
    }

    #[test]
    fn session_update_params_follow_the_protocol() {
        let thought = AcpEvent::agent_thought_chunk("s1", "weighing it");
        assert_eq!(
            thought.session_update_params().unwrap(),
            serde_json::json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"type": "text", "text": "weighing it"},
                },
            })
        );
        let done = AcpEvent::tool_call_complete(
            "s1",
            "tc1",
            "read_file",
            Some(r#"{"error":"missing"}"#.into()),
        );
        let update = &done.session_update_params().unwrap()["update"];
        assert_eq!(update["sessionUpdate"], "tool_call_update");
        assert_eq!(update["toolCallId"], "tc1");
        assert_eq!(update["status"], "failed");
        assert_eq!(
            update["content"][0]["content"]["text"],
            "Read failed: missing"
        );
        // Internal markers have no protocol form.
        assert!(AcpEvent::thinking("s1", "Processing prompt...")
            .session_update_params()
            .is_none());
        assert!(AcpEvent::step_complete("s1", 1)
            .session_update_params()
            .is_none());
    }

    #[test]
    fn server_permission_stores_are_instance_scoped() {
        let handler_a = Arc::new(HermesAcpHandler::new(
            Arc::new(SessionManager::new()),
            Arc::new(EventSink::default()),
            Arc::new(PermissionStore::new()),
        ));
        let handler_b = Arc::new(HermesAcpHandler::new(
            Arc::new(SessionManager::new()),
            Arc::new(EventSink::default()),
            Arc::new(PermissionStore::new()),
        ));
        let server_a = AcpServer::new(handler_a);
        let server_b = AcpServer::new(handler_b);

        let mut request = build_permission_request(
            "acp-session-A",
            "rm -rf /tmp/a",
            "dangerous command",
            true,
            0,
        );
        request.id = "req-a".to_string();
        server_a.permission_store().add_pending(request);

        assert_eq!(server_a.permission_store().list_pending().len(), 1);
        assert!(server_b.permission_store().list_pending().is_empty());
        assert!(server_a
            .permission_store()
            .resolve("req-a", PermissionOutcome::Denied));
        assert!(!server_b
            .permission_store()
            .resolve("req-a", PermissionOutcome::Denied));
    }
}
