use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::*;

const USER: &str = "agent@example.invalid";
const PASSWORD: &str = "fixture-password";
const LOGIN: &str = "A0001 LOGIN agent@example.invalid fixture-password\r\n";

fn unseen_messages(
    exchange: &[(&str, &str)],
    policy: &EmailInboundAuthPolicy,
) -> Result<Vec<IncomingMessage>, GatewayError> {
    let (client, server) = UnixStream::pair().unwrap();
    for socket in [&client, &server] {
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(4)))
            .unwrap();
    }
    std::thread::scope(|scope| {
        let mut client = client;
        scope.spawn(move || {
            let mut server = BufReader::new(server);
            server
                .get_mut()
                .write_all(b"* OK fixture started\r\n")
                .unwrap();
            for &(command, response) in exchange {
                let mut line = String::new();
                server.read_line(&mut line).unwrap();
                assert_eq!(line, command);
                server.get_mut().write_all(response.as_bytes()).unwrap();
            }
            let mut extra = String::new();
            assert_eq!(server.read_line(&mut extra).unwrap(), 0, "{extra:?}");
        });
        read_unseen_imap_messages(&mut client, USER, PASSWORD, policy)
    })
}

#[test]
fn imap_exchange_fetches_authenticated_unseen_mail_and_logs_out() {
    let policy = EmailInboundAuthPolicy {
        require_authenticated_sender: true,
        authserv_id: Some("mx.example.invalid".to_owned()),
        allowed_users: vec!["surveyor@example.invalid".to_owned()],
        admin_users: Vec::new(),
        allow_all_users: false,
    };
    let messages = unseen_messages(
        &[
            (LOGIN, "A0001 OK logged in\r\n"),
            ("A0002 SELECT INBOX\r\n", "A0002 OK selected\r\n"),
            ("A0003 SEARCH UNSEEN\r\n", "* SEARCH 42\r\nA0003 OK search complete\r\n"),
            (
                "A0004 FETCH 42 (BODY[TEXT] BODY[HEADER.FIELDS (FROM SUBJECT AUTHENTICATION-RESULTS)])\r\n",
                concat!(
                    "* 42 FETCH (BODY[HEADER.FIELDS (FROM SUBJECT AUTHENTICATION-RESULTS)] {154}\r\n",
                    "From: Surveyor <surveyor@example.invalid>\r\n",
                    "Subject: Estuary survey\r\n",
                    "Authentication-Results: mx.example.invalid; dmarc=pass header.from=example.invalid\r\n",
                    "\r\n",
                    " BODY[TEXT] {19}\r\n",
                    "Survey completed.\r\n",
                    ")\r\n",
                    "A0004 OK fetch complete\r\n",
                ),
            ),
            ("A0005 LOGOUT\r\n", ""),
        ],
        &policy,
    )
    .unwrap();
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.chat_id, "surveyor@example.invalid");
    assert_eq!(message.user_id, "surveyor@example.invalid");
    assert_eq!(message.message_id.as_deref(), Some("42"));
    assert_eq!(message.text, "Survey completed.");
    assert!(message.is_dm);
}

#[test]
fn imap_login_failure_stops_polling_before_selecting_the_inbox() {
    let policy = EmailInboundAuthPolicy {
        require_authenticated_sender: true,
        authserv_id: None,
        allowed_users: Vec::new(),
        admin_users: Vec::new(),
        allow_all_users: false,
    };
    let error = unseen_messages(&[(LOGIN, "A0001 NO login rejected\r\n")], &policy).unwrap_err();
    assert!(error
        .to_string()
        .contains("IMAP LOGIN failed: A0001 NO login rejected"));
}
