//! Stdio framing: the MCP specification's one-message-per-line form and the
//! older Content-Length form, on the client and on `hermes mcp serve`.

use super::*;
use tokio::io::duplex;

#[tokio::test]
async fn the_server_answers_a_newline_client_in_newline_framing() {
    let (mut client, server_side) = duplex(4096);
    let (input, output) = tokio::io::split(server_side);
    let mut server = ServerStdioTransport::with_io(input, output);
    server.start().await.unwrap();
    // Two messages in one write, as a client may send them.
    client
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1}\n{\"jsonrpc\":\"2.0\",\"id\":2}\n")
        .await
        .unwrap();

    for id in [1, 2] {
        let message = tokio::time::timeout(Duration::from_secs(5), server.receive())
            .await
            .expect("the server waited for a header")
            .unwrap();
        assert_eq!(message["id"], id);
    }
    server
        .send(serde_json::json!({"jsonrpc":"2.0","id":2,"result":{}}))
        .await
        .unwrap();

    let mut reply = BufReader::new(client);
    let mut line = String::new();
    reply.read_line(&mut line).await.unwrap();
    assert!(line.starts_with('{') && line.ends_with('\n'), "{line:?}");
}

#[tokio::test]
async fn the_server_answers_a_content_length_client_in_kind() {
    let (mut client, server_side) = duplex(4096);
    let (input, output) = tokio::io::split(server_side);
    let mut server = ServerStdioTransport::with_io(input, output);
    server.start().await.unwrap();
    client
        .write_all(b"Content-Length: 8\r\n\r\n{\"id\":1}Content-Length: 8\r\n\r\n{\"id\":2}")
        .await
        .unwrap();

    for id in [1, 2] {
        let message = tokio::time::timeout(Duration::from_secs(5), server.receive())
            .await
            .expect("the second message was lost")
            .unwrap();
        assert_eq!(message["id"], id);
    }
    server.send(serde_json::json!({"id":2})).await.unwrap();

    let expected = b"Content-Length: 8\r\n\r\n{\"id\":2}";
    let mut reply = vec![0; expected.len()];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply[..], &expected[..]);
}

#[tokio::test]
async fn newline_framing_skips_blank_lines() {
    let (mut writer, reader) = duplex(256);
    let mut reader = BufReader::new(reader);
    let mut pending = Vec::new();
    writer
        .write_all(b"\n\r\n \t\n{\"id\":1}\n\n{\"id\":2}\n")
        .await
        .unwrap();
    for id in [1, 2] {
        let message = read_stdio_line(&mut reader, &mut pending, 64)
            .await
            .unwrap();
        assert_eq!(message["id"], id);
    }
}

#[tokio::test]
async fn a_header_under_newline_framing_names_legacy_framed() {
    let (mut writer, reader) = duplex(256);
    let mut reader = BufReader::new(reader);
    writer
        .write_all(b"Content-Length: 8\r\n\r\n{\"id\":1}")
        .await
        .unwrap();
    let error = read_stdio_line(&mut reader, &mut Vec::new(), 64)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("stdio_mode: legacy_framed"), "{error}");
}

#[tokio::test]
async fn a_json_line_under_header_framing_names_the_mode() {
    let (mut writer, reader) = duplex(256);
    let mut reader = BufReader::new(reader);
    writer.write_all(b"{\"id\":1}\n").await.unwrap();
    let error = read_content_length_message(&mut reader, 64)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("stdio_mode"), "{error}");
}

#[tokio::test]
async fn header_framing_caps_a_header_line_and_the_header_count() {
    let (mut writer, reader) = duplex(1 << 16);
    let mut reader = BufReader::new(reader);
    // One endless header line, with the writer still open.
    writer.write_all(&[b'X'; 8192]).await.unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        read_content_length_message(&mut reader, 64),
    )
    .await
    .expect("read the whole line")
    .unwrap_err()
    .to_string();
    assert!(error.contains("header"), "{error}");

    let (mut writer, reader) = duplex(1 << 16);
    let mut reader = BufReader::new(reader);
    writer
        .write_all(&b"X-Pad: 1\r\n".repeat(100))
        .await
        .unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        read_content_length_message(&mut reader, 64),
    )
    .await
    .expect("read every header")
    .unwrap_err()
    .to_string();
    assert!(error.contains("header"), "{error}");
}

#[tokio::test]
async fn header_framing_accepts_a_lower_case_header_name() {
    let (mut writer, reader) = duplex(256);
    let mut reader = BufReader::new(reader);
    writer
        .write_all(b"content-length: 8\r\n\r\n{\"id\":1}")
        .await
        .unwrap();
    let message = read_content_length_message(&mut reader, 64).await.unwrap();
    assert_eq!(message["id"], 1);
}

#[tokio::test]
async fn the_server_sniffs_past_blank_lines_a_split_brace_and_a_bom() {
    let (mut client, server_side) = duplex(4096);
    let (input, output) = tokio::io::split(server_side);
    let mut server = ServerStdioTransport::with_io(input, output);
    server.start().await.unwrap();
    client.write_all(b"\r\n").await.unwrap();
    client.write_all(b"{").await.unwrap();
    client.write_all(b"\"id\":1}\n").await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), server.receive())
        .await
        .expect("the split brace was judged a header")
        .unwrap();
    assert_eq!(message["id"], 1);

    let (mut client, server_side) = duplex(4096);
    let (input, output) = tokio::io::split(server_side);
    let mut server = ServerStdioTransport::with_io(input, output);
    server.start().await.unwrap();
    client.write_all(b"\xEF\xBB\xBF{\"id\":2}\n").await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), server.receive())
        .await
        .expect("the BOM was judged a header")
        .unwrap();
    assert_eq!(message["id"], 2);
}

#[cfg(unix)]
mod processes {
    use super::*;
    use crate::{McpClient, McpServerConfig};
    use hermes_core::mcp::McpStdioMode;
    use std::os::unix::fs::PermissionsExt;

    /// Replies to initialize and tools/list, one JSON message per line; a line
    /// it cannot parse gets a parse error, as SDK servers answer.
    const NEWLINE_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    Content-Length*)
      printf '{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}\n'
      continue ;;
  esac
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      v=$(printf '%s' "$line" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{"tools":{}},"serverInfo":{"name":"stub","version":"1"}}}\n' "$id" "$v" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
  esac
done
"#;

    /// The same replies in Content-Length framing; other lines are ignored,
    /// as an older `hermes mcp serve` ignores them, so a newline client waits.
    const HEADER_SERVER: &str = r#"#!/bin/sh
while IFS= read -r header; do
  case "$header" in
    Content-Length:*) ;;
    *) continue ;;
  esac
  n=$(printf '%s' "$header" | tr -dc '0-9')
  IFS= read -r blank
  line=$(dd bs=1 count="$n" 2>/dev/null)
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      v=$(printf '%s' "$line" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
      body=$(printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{"tools":{}},"serverInfo":{"name":"stub","version":"1"}}}' "$id" "$v") ;;
    *'"method":"tools/list"'*)
      body=$(printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}' "$id") ;;
    *) continue ;;
  esac
  printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
done
"#;

    fn script(dir: &tempfile::TempDir, name: &str, body: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    async fn tools_of(
        command: String,
        mode: Option<McpStdioMode>,
    ) -> Result<Vec<String>, McpError> {
        let mut config = McpServerConfig::stdio(command, Vec::new());
        config.stdio_mode = mode;
        let mut client = McpClient::new(config);
        tokio::time::timeout(Duration::from_secs(20), client.connect())
            .await
            .map_err(|_| McpError::ConnectionError("connect hung".into()))??;
        let tools = client.list_tools().await?;
        Ok(tools.into_iter().map(|tool| tool.name).collect())
    }

    #[tokio::test]
    async fn an_entry_without_stdio_mode_reaches_a_newline_server() {
        let dir = tempfile::tempdir().unwrap();
        let server = script(&dir, "newline-server", NEWLINE_SERVER);
        for mode in [None, Some(McpStdioMode::StandardLegacy)] {
            assert_eq!(
                tools_of(server.clone(), mode).await.unwrap(),
                ["echo"],
                "{mode:?}"
            );
        }
    }

    #[tokio::test]
    async fn legacy_framed_still_reaches_a_content_length_server() {
        let dir = tempfile::tempdir().unwrap();
        let server = script(&dir, "header-server", HEADER_SERVER);
        assert_eq!(
            tools_of(server, Some(McpStdioMode::LegacyFramed))
                .await
                .unwrap(),
            ["echo"]
        );
    }

    /// Connects with `timeout` and returns the error and how long it took.
    async fn connect_error(
        command: String,
        mode: Option<McpStdioMode>,
        timeout: Duration,
    ) -> (String, Duration) {
        let mut config = McpServerConfig::stdio(command, Vec::new());
        config.stdio_mode = mode;
        let mut client = McpClient::new(config);
        client.connect_timeout = timeout;
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(timeout * 3, client.connect())
            .await
            .expect("connect outlived its own timeout");
        (
            result.expect_err("connected").to_string(),
            started.elapsed(),
        )
    }

    #[tokio::test]
    async fn a_silent_header_server_times_out_naming_legacy_framed() {
        let dir = tempfile::tempdir().unwrap();
        let header = script(&dir, "header-server", HEADER_SERVER);
        let (error, took) = connect_error(header, None, Duration::from_secs(2)).await;
        assert!(error.contains("stdio_mode: legacy_framed"), "{error}");
        assert!(took < Duration::from_secs(5), "{took:?}");
    }

    #[tokio::test]
    async fn a_header_reply_under_newline_framing_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        // Answers a line it cannot read with a framed parse error.
        let server = script(
            &dir,
            "header-answers",
            "#!/bin/sh\nread -r line\nprintf 'Content-Length: 2\\r\\n\\r\\n{}'\nsleep 30\n",
        );
        let (error, took) = connect_error(server, None, Duration::from_secs(20)).await;
        assert!(error.contains("stdio_mode: legacy_framed"), "{error}");
        assert!(took < Duration::from_secs(5), "{took:?}");
    }

    #[tokio::test]
    async fn a_json_line_under_header_framing_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let newline = script(&dir, "newline-server", NEWLINE_SERVER);
        let (error, took) = connect_error(
            newline,
            Some(McpStdioMode::LegacyFramed),
            Duration::from_secs(20),
        )
        .await;
        assert!(error.contains("stdio_mode"), "{error}");
        assert!(took < Duration::from_secs(5), "{took:?}");
    }

    #[tokio::test]
    async fn a_content_length_client_keeps_messages_read_together() {
        let dir = tempfile::tempdir().unwrap();
        let server = script(
            &dir,
            "two-at-once",
            "#!/bin/sh\nprintf 'Content-Length: 8\\r\\n\\r\\n{\"id\":1}Content-Length: 8\\r\\n\\r\\n{\"id\":2}'\nsleep 10\n",
        );
        let mut transport = StdioTransport::new(server, &[], &HashMap::new(), false);
        transport.start().await.unwrap();
        for id in [1, 2] {
            let message = tokio::time::timeout(Duration::from_secs(5), transport.receive())
                .await
                .expect("the second message was lost")
                .unwrap();
            assert_eq!(message["id"], id);
        }
        transport.close().await.unwrap();
    }
}
