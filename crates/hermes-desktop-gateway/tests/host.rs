//! `shell.exec`, `cli.exec` and `complete.path` act on the host, so only a connection holding
//! the host-input capability reaches them: one that local mode's ticket opened. A token
//! connection is refused before any parameter is read, whoever presents the token: a desktop
//! tunnelled in from another machine and a child gateway dialled by its parent both arrive that
//! way.

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const TOKEN: &str = "host-input-test-token";
/// Text that a refusal must not echo.
const MARKER: &str = "zq-marker-7731";

async fn next(socket: &mut Socket) -> Value {
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(40), socket.next())
            .await
            .expect("no frame within 40 seconds")
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

/// Each host method with parameters it would serve, the marker in each.
fn requests() -> [(&'static str, Value); 3] {
    [
        ("shell.exec", json!({"command": format!("echo {MARKER}")})),
        (
            "cli.exec",
            json!({"argv": ["profile", "delete", MARKER, "--yes"]}),
        ),
        ("complete.path", json!({"word": MARKER})),
    ]
}

#[tokio::test]
async fn a_token_connection_is_refused_every_host_method_without_an_echo() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let home = tempfile::tempdir().unwrap();
    let config = hermes_config::GatewayConfig {
        home_dir: Some(home.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
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
    for (method, params) in requests() {
        let reply = call(&mut socket, method, params).await;
        assert_eq!(reply["error"]["code"], 4030, "{method}: {reply}");
        assert_eq!(
            reply["error"]["message"], "not available to this client",
            "{method}"
        );
        assert!(
            !reply.to_string().contains(MARKER),
            "{method} echoed: {reply}"
        );
    }
    // A malformed request is refused the same way: no parameter is read first.
    let reply = call(&mut socket, "shell.exec", json!({"command": 7})).await;
    assert_eq!(reply["error"]["code"], 4030, "{reply}");
}

#[cfg(unix)]
mod local {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    use hermes_desktop_gateway::local::{self, LocalRuntime};
    use serde_json::{json, Value};

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    use super::{call, next, requests, Socket, MARKER};

    fn control(home: &Path, request: Value) -> Value {
        let mut stream = UnixStream::connect(home.join("gateway.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(format!("{request}\n").as_bytes()).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// A local-mode runtime for a private home, and the address an interactive ticket opens.
    fn ticket_url() -> (tempfile::TempDir, LocalRuntime, String) {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        let config = hermes_config::GatewayConfig {
            home_dir: Some(home.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let runtime = local::start(&home, hermes_desktop_gateway::Chat::new(config)).unwrap();
        let reply = control(
            &home,
            json!({"protocol": 1, "id": 1, "verb": "session-ticket", "params": {
                "profile_id": home.to_str().unwrap(), "instance_id": runtime.instance_id(),
                "purpose": "interactive"}}),
        );
        let ticket = reply["result"]["ticket"].as_str().unwrap().to_string();
        let url = format!(
            "{}/api/ws?native_dial=n1&ticket={ticket}",
            runtime.api_origin().replacen("http", "ws", 1)
        );
        (dir, runtime, url)
    }

    /// A local-mode runtime for a private home, and a socket its interactive ticket opened.
    async fn dial() -> (tempfile::TempDir, LocalRuntime, Socket) {
        let (dir, runtime, url) = ticket_url();
        let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .unwrap();
        assert_eq!(next(&mut socket).await["params"]["type"], "gateway.ready");
        (dir, runtime, socket)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_mode_ticket_connection_is_served() {
        let (_dir, _runtime, mut socket) = dial().await;
        // The capability gate passes for every method: none answers 4030.
        for (method, params) in requests() {
            let reply = call(&mut socket, method, params).await;
            assert_ne!(reply["error"]["code"], 4030, "{method}: {reply}");
        }
        let reply = call(
            &mut socket,
            "shell.exec",
            json!({"command": "printf 'a\\nb'; printf e >&2; exit 3"}),
        )
        .await;
        assert_eq!(
            reply["result"],
            json!({"stdout": "a\nb", "stderr": "e", "code": 3}),
            "{reply}"
        );
        // Tests run in the crate's folder, the session's folder here.
        let reply = call(&mut socket, "complete.path", json!({"word": "Cargo.t"})).await;
        assert_eq!(reply["result"]["items"][0]["text"], "Cargo.toml", "{reply}");
    }

    /// The capability follows the credential, not the listener: a browser page cannot present
    /// a ticket on the native listener, as any `Origin` header refuses the upgrade.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_ticket_dial_from_a_browser_origin_is_refused() {
        let (_dir, runtime, url) = ticket_url();
        let mut request = url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("Origin", runtime.api_origin().parse().unwrap());
        let refused = tokio_tungstenite::connect_async(request).await.unwrap_err();
        assert!(
            matches!(&refused, tokio_tungstenite::tungstenite::Error::Http(reply) if reply.status() == 403),
            "{refused}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_served_connection_is_still_refused_what_the_rules_refuse() {
        let (_dir, _runtime, mut socket) = dial().await;
        let reply = call(&mut socket, "shell.exec", json!({"command": "rm -rf /"})).await;
        assert_eq!(reply["error"]["code"], 4005, "{reply}");
        let reply = call(&mut socket, "shell.exec", json!({"command": "  "})).await;
        assert_eq!(reply["error"]["code"], 4004, "{reply}");
        let long = "x".repeat(8193);
        let reply = call(&mut socket, "shell.exec", json!({"command": long})).await;
        assert_eq!(reply["error"]["code"], 4000);
        // An unlisted profile is not run.
        let reply = call(
            &mut socket,
            "cli.exec",
            json!({"argv": ["profile", "delete", MARKER, "--yes"]}),
        )
        .await;
        assert_eq!(reply["result"]["blocked"], true, "{reply}");
        let reply = call(&mut socket, "cli.exec", json!({"argv": "profile list"})).await;
        assert_eq!(reply["error"]["code"], 4003, "{reply}");
        let reply = call(&mut socket, "cli.exec", json!({"argv": []})).await;
        assert_eq!(reply["error"]["code"], 4003, "{reply}");
        // Nothing outside the session's folder.
        for word in ["/", "~/", "../", "@file:../"] {
            let reply = call(&mut socket, "complete.path", json!({"word": word})).await;
            assert_eq!(reply["result"]["items"], json!([]), "{word}: {reply}");
        }
    }
}
