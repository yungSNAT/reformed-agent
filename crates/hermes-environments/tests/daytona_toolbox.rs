#![cfg(feature = "daytona")]

use std::collections::BTreeMap;
use std::time::Duration;

use hermes_core::{AgentError, TerminalBackend};
use hermes_environments::daytona::DaytonaBackend;
use reqwest::Url;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::timeout;

struct Request {
    method: String,
    url: Url,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn respond(listener: &TcpListener, status: &str, response: &str) -> Request {
    respond_with_headers(listener, status, "", response).await
}

async fn respond_with_headers(
    listener: &TcpListener,
    status: &str,
    headers_out: &str,
    response: &str,
) -> Request {
    timeout(Duration::from_secs(5), async {
        let (socket, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(socket);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let mut request_line = line.split_whitespace();
        let method = request_line.next().unwrap().to_owned();
        let url = Url::parse(&format!("http://fixture{}", request_line.next().unwrap())).unwrap();

        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
        let size = headers
            .get("content-length")
            .map_or(0, |size| size.parse().unwrap());
        let mut body = vec![0; size];
        stream.read_exact(&mut body).await.unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers_out}Connection: close\r\n\r\n{response}",
            response.len(),
        );
        stream.get_mut().write_all(response.as_bytes()).await.unwrap();
        Request {
            method,
            url,
            headers,
            body,
        }
    })
    .await
    .expect("stub request deadline")
}

fn backend(listener: &TcpListener) -> DaytonaBackend {
    let address = listener.local_addr().unwrap();
    DaytonaBackend::new(
        Some(format!("http://{address}/toolbox/")),
        Some("fixture-key".into()),
        Some("sandbox-fixture".into()),
        20,
        64,
    )
}

#[tokio::test]
async fn executes_with_the_toolbox_contract() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = backend(&listener);
    let server = tokio::spawn(async move {
        respond(
            &listener,
            "200 OK",
            r#"{"exitCode":7,"result":"stdout and stderr are combined"}"#,
        )
        .await
    });
    let output = backend
        .execute_command(
            "printf fixture",
            Some(7),
            Some("/home/daytona"),
            false,
            false,
        )
        .await;
    let request = server.await.unwrap();

    assert_eq!(request.method, "POST");
    assert_eq!(
        request.url.path(),
        "/toolbox/sandbox-fixture/process/execute"
    );
    assert_eq!(request.headers["authorization"], "Bearer fixture-key");
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({"command": "printf fixture", "cwd": "/home/daytona", "timeout": 7}),
    );
    let output = output.unwrap();
    assert_eq!(output.exit_code, 7);
    assert_eq!(output.stdout, "stdout and stderr are combined");
    assert!(output.stderr.is_empty());
}

#[tokio::test]
async fn reads_writes_and_checks_files_over_the_toolbox_api() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = backend(&listener);
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in [
            ("200 OK", "one\ntwo\nthree\n"),
            ("200 OK", "{}"),
            ("200 OK", "{}"),
            ("404 Not Found", r#"{"message":"missing"}"#),
            ("403 Forbidden", r#"{"message":"fixture error body"}"#),
        ] {
            requests.push(respond(&listener, status, body).await);
        }
        requests
    });

    let path = "/workspace/notes & draft.txt";
    let read = backend.read_file(path, Some(1), Some(1)).await;
    let write = backend.write_file(path, "fixture contents\n").await;
    let exists = backend.file_exists(path).await;
    let missing = backend.file_exists(path).await;
    let forbidden = backend.file_exists(path).await;
    let requests = server.await.unwrap();

    for (request, (method, endpoint)) in requests.iter().zip([
        ("GET", "download"),
        ("POST", "upload-v2"),
        ("GET", "info"),
        ("GET", "info"),
        ("GET", "info"),
    ]) {
        assert_eq!(request.method, method);
        assert_eq!(
            request.url.path(),
            format!("/toolbox/sandbox-fixture/files/{endpoint}"),
        );
        let query = request.url.query_pairs().collect::<Vec<_>>();
        assert_eq!(query.len(), 1);
        assert_eq!(query[0], ("path".into(), path.into()));
        assert_eq!(request.headers["authorization"], "Bearer fixture-key");
    }
    assert_eq!(
        requests[1].headers["content-type"],
        "application/octet-stream"
    );
    assert_eq!(requests[1].body, b"fixture contents\n");
    assert_eq!(read.unwrap(), "two");
    write.unwrap();
    assert!(exists.unwrap());
    assert!(!missing.unwrap());
    let error = forbidden.unwrap_err().to_string();
    assert!(error.contains("403"));
    assert!(!error.contains("fixture error body"));
}

#[tokio::test]
async fn keeps_the_request_on_the_configured_toolbox_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = backend(&listener);
    let server = tokio::spawn(async move {
        respond_with_headers(
            &listener,
            "307 Temporary Redirect",
            "Location: /another-endpoint\r\n",
            "",
        )
        .await;
        timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err()
    });
    let error = backend
        .execute_command("printf fixture", None, None, false, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("307"));
    assert!(server.await.unwrap());
}

#[tokio::test]
async fn reports_a_remote_command_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = backend(&listener);
    let server = tokio::spawn(async move {
        respond(&listener, "408 Request Timeout", r#"{"message":"expired"}"#).await;
    });
    let error = backend
        .execute_command("printf fixture", Some(1), None, false, false)
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(error, AgentError::Timeout(_)));
}
