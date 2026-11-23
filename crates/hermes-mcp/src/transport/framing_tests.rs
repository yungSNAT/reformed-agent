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

#[cfg(unix)]
mod processes {
    use super::*;
    use crate::{McpClient, McpServerConfig};
    use hermes_core::mcp::McpStdioMode;
    use std::os::unix::fs::PermissionsExt;

    /// Replies to initialize and tools/list, one JSON message per line.
    const NEWLINE_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    Content-Length*) exit 3 ;;
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

    /// The same replies in Content-Length framing; exits on anything else.
    const HEADER_SERVER: &str = r#"#!/bin/sh
while IFS= read -r header; do
  case "$header" in
    Content-Length:*) ;;
    *) exit 3 ;;
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

    #[tokio::test]
    async fn a_mismatched_framing_fails_instead_of_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let header = script(&dir, "header-server", HEADER_SERVER);
        let newline = script(&dir, "newline-server", NEWLINE_SERVER);
        assert!(tools_of(header, None).await.is_err());
        assert!(tools_of(newline, Some(McpStdioMode::LegacyFramed))
            .await
            .is_err());
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
