use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use super::*;

const SENDER: &str = "agent@example.invalid";
const RECIPIENT: &str = "reader@example.invalid";
const FIXTURE_PASSWORD: &str = "fixture-local";

fn adapter(port: u16) -> EmailAdapter {
    let config = serde_json::from_value(json!({
        "imap_host": "127.0.0.1",
        "smtp_host": "127.0.0.1",
        "smtp_port": port,
        "username": SENDER,
        "password": FIXTURE_PASSWORD,
    }))
    .unwrap();
    EmailAdapter::new(config).unwrap()
}

async fn expect_line(reader: &mut BufReader<TcpStream>, expected: &str) {
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, format!("{expected}\r\n"));
}

async fn smtp_exchange(listener: TcpListener, ehlo: &str, data_reply: &str) -> String {
    tokio::time::timeout(Duration::from_secs(4), async {
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(socket);
        reader
            .get_mut()
            .write_all(b"220 fixture\r\n")
            .await
            .unwrap();

        for (expected, reply) in [
            ("EHLO hermes-agent".to_string(), ehlo),
            ("AUTH LOGIN".to_string(), "334 VXNlcm5hbWU6\r\n"),
            (STANDARD.encode(SENDER), "334 UGFzc3dvcmQ6\r\n"),
            (STANDARD.encode(FIXTURE_PASSWORD), "235 Authenticated\r\n"),
            (format!("MAIL FROM:<{SENDER}>"), "250 Sender accepted\r\n"),
            (
                format!("RCPT TO:<{RECIPIENT}>"),
                "250 Recipient accepted\r\n",
            ),
            ("DATA".to_string(), "354 Send message\r\n"),
        ] {
            expect_line(&mut reader, &expected).await;
            reader.get_mut().write_all(reply.as_bytes()).await.unwrap();
        }

        let mut message = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(
                reader.read_line(&mut line).await.unwrap(),
                0,
                "DATA ended before the terminator"
            );
            if line == ".\r\n" {
                break;
            }
            message.push_str(&line);
        }
        reader
            .get_mut()
            .write_all(data_reply.as_bytes())
            .await
            .unwrap();
        if data_reply.starts_with("250") {
            expect_line(&mut reader, "QUIT").await;
            reader
                .get_mut()
                .write_all(b"221 Goodbye\r\n")
                .await
                .unwrap();
        } else {
            let mut line = String::new();
            assert_eq!(reader.read_line(&mut line).await.unwrap(), 0);
        }
        message
    })
    .await
    .expect("fixture SMTP exchange timed out")
}

#[tokio::test]
async fn single_line_ehlo_delivers_a_plain_text_message() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let adapter = adapter(listener.local_addr().unwrap().port());
    let server = tokio::spawn(smtp_exchange(listener, "250 fixture\r\n", "250 Queued\r\n"));
    let sent = adapter.send_message(RECIPIENT, "Hello café", None).await;
    let message = server.await.unwrap();
    sent.unwrap();
    assert_eq!(
        message,
        format!(
            "From: {SENDER}\r\nTo: {RECIPIENT}\r\nSubject: Hermes Agent\r\n\
             Content-Type: text/plain; charset=utf-8\r\nMIME-Version: 1.0\r\n\r\nHello café\r\n"
        )
    );
}

#[tokio::test]
async fn multiline_ehlo_delivers_a_multipart_file() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let adapter = adapter(listener.local_addr().unwrap().port());
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("fixture.txt");
    let payload = b"fixture attachment";
    std::fs::write(&file, payload).unwrap();
    let server = tokio::spawn(smtp_exchange(
        listener,
        "250-fixture\r\n250-SIZE 2048\r\n250 AUTH LOGIN\r\n",
        "250 Queued\r\n",
    ));
    let sent = adapter
        .send_file(RECIPIENT, file.to_str().unwrap(), Some("Fixture caption"))
        .await;
    let message = server.await.unwrap();
    sent.unwrap();
    let (headers, body) = message.split_once("\r\n\r\n").unwrap();
    assert!(headers.contains("\r\nSubject: Fixture caption\r\n"));
    assert!(headers.lines().any(|line| line == "MIME-Version: 1.0"));
    let boundary = headers
        .lines()
        .find_map(|line| line.strip_prefix("Content-Type: multipart/mixed; boundary=\""))
        .unwrap()
        .strip_suffix('"')
        .unwrap();
    assert!(!boundary.is_empty());
    assert!(body.starts_with(&format!(
        "--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nFixture caption\r\n"
    )));
    assert!(body.contains(&format!(
        "--{boundary}\r\nContent-Type: text/plain; name=\"fixture.txt\"\r\n\
         Content-Disposition: attachment; filename=\"fixture.txt\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}--\r\n",
        STANDARD.encode(payload)
    )));
}

#[tokio::test]
async fn data_rejection_is_reported_to_the_email_sender() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let adapter = adapter(listener.local_addr().unwrap().port());
    let server = tokio::spawn(smtp_exchange(
        listener,
        "250 fixture\r\n",
        "550 Mailbox unavailable\r\n",
    ));
    let error = adapter
        .send_email(RECIPIENT, "Fixture subject", "Fixture body")
        .await
        .unwrap_err();
    let message = server.await.unwrap();
    assert!(message.ends_with("\r\n\r\nFixture body\r\n"));
    assert!(matches!(
        error,
        GatewayError::SendFailed(message) if message == "SMTP DATA rejected: 550 Mailbox unavailable"
    ));
}

#[tokio::test]
async fn incomplete_or_inconsistent_replies_stop_the_session_before_auth() {
    for (greeting, error) in [
        ("220-fixture", "SMTP reply ended early"),
        ("220-fixture\r\n250 Done\r\n", "Invalid SMTP reply"),
    ] {
        assert_smtp_aborted(greeting, Vec::new(), error).await;
    }
}

#[tokio::test]
async fn rejected_handshake_steps_stop_sending_commands_and_credentials() {
    assert_smtp_aborted(
        "554 Service unavailable\r\n",
        Vec::new(),
        "SMTP greeting rejected: 554",
    )
    .await;
    assert_smtp_aborted(
        "220 fixture\r\n",
        vec![("EHLO hermes-agent".into(), "550 EHLO rejected\r\n")],
        "SMTP EHLO rejected: 550",
    )
    .await;
    assert_smtp_aborted(
        "220 fixture\r\n",
        vec![
            ("EHLO hermes-agent".into(), "250 fixture\r\n"),
            ("AUTH LOGIN".into(), "504 Unsupported\r\n"),
        ],
        "SMTP AUTH LOGIN rejected: 504",
    )
    .await;
    assert_smtp_aborted(
        "220 fixture\r\n",
        vec![
            ("EHLO hermes-agent".into(), "250 fixture\r\n"),
            ("AUTH LOGIN".into(), "334 VXNlcm5hbWU6\r\n"),
            (STANDARD.encode(SENDER), "535 Username rejected\r\n"),
        ],
        "SMTP username rejected: 535",
    )
    .await;
}

async fn assert_smtp_aborted(
    greeting: &'static str,
    transcript: Vec<(String, &'static str)>,
    expected: &str,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let adapter = adapter(listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(4), async {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            reader
                .get_mut()
                .write_all(greeting.as_bytes())
                .await
                .unwrap();
            for (command, reply) in transcript {
                expect_line(&mut reader, &command).await;
                reader.get_mut().write_all(reply.as_bytes()).await.unwrap();
            }
            reader.get_mut().shutdown().await.unwrap();
            let mut extra = Vec::new();
            reader.read_to_end(&mut extra).await.unwrap();
            assert!(extra.is_empty(), "client continued after server rejection");
        })
        .await
        .expect("fixture SMTP exchange timed out");
    });
    let error = adapter
        .send_email(RECIPIENT, "Fixture subject", "Fixture body")
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(error, GatewayError::SendFailed(message) if message == expected));
}
