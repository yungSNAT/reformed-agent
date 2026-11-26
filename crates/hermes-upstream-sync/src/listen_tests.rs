use std::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

const SECRET: &str = "It's a Secret to Everybody";
const PATH: &str = "/github/upstream-sync";
const PUSH: &str = r#"{"ref": "refs/heads/main", "before": "aaa", "after": "bbb", "compare": "https://example.test/c", "repository": {"full_name": "NousResearch/hermes-agent"}}"#;

fn config(secret: &str, host: &str, allow_unsigned: bool) -> ListenConfig {
    ListenConfig {
        host: host.to_string(),
        port: 8099,
        path: PATH.to_string(),
        secret: secret.to_string(),
        allow_unsigned,
        expected_repo: "NousResearch/hermes-agent".to_string(),
        expected_ref: "refs/heads/main".to_string(),
        backend_name: "sqlite".to_string(),
    }
}

/// Records deliveries in memory; a delivery id or body seen before is a
/// duplicate.
#[derive(Default)]
struct Recorder(Mutex<Vec<SyncEvent>>, Mutex<Vec<String>>);

impl Publisher for Recorder {
    fn enqueue(&self, event: &SyncEvent, body_sha256: &str) -> Result<bool, String> {
        let mut seen = self.0.lock().unwrap();
        let mut bodies = self.1.lock().unwrap();
        if seen.iter().any(|e| e.delivery_id == event.delivery_id)
            || bodies.iter().any(|b| b == body_sha256)
        {
            return Ok(false);
        }
        seen.push(event.clone());
        bodies.push(body_sha256.to_string());
        Ok(true)
    }
}

struct Failing;

impl Publisher for Failing {
    fn enqueue(&self, _: &SyncEvent, _: &str) -> Result<bool, String> {
        Err("disk full".to_string())
    }
}

async fn start(secret: &str, queue: Arc<dyn Publisher>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(run(
        listener,
        config(secret, "127.0.0.1", secret.is_empty()),
        queue,
    ));
    addr
}

fn sign(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(body);
    let digest = mac.finalize().into_bytes();
    format!(
        "sha256={}",
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// Sends raw request bytes and returns the status and body.
async fn send(addr: SocketAddr, raw: Vec<u8>) -> (u16, String) {
    exchange(tokio::net::TcpStream::connect(addr).await.unwrap(), raw).await
}

/// A connection from `local`, another loopback address; `None` where it
/// cannot be bound (macOS configures only 127.0.0.1; Linux, where CI runs
/// these tests, answers on all of 127.0.0.0/8).
async fn connect_from(local: &str, addr: SocketAddr) -> Option<tokio::net::TcpStream> {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(format!("{local}:0").parse().unwrap()).ok()?;
    Some(socket.connect(addr).await.unwrap())
}

/// Reads until the server closes the connection; `None` if it is still open
/// after `limit`.
async fn closed_within(stream: &mut tokio::net::TcpStream, limit: Duration) -> Option<Vec<u8>> {
    let mut rest = Vec::new();
    match tokio::time::timeout(limit, stream.read_to_end(&mut rest)).await {
        Ok(_) => Some(rest),
        Err(_) => None,
    }
}

async fn exchange(mut stream: tokio::net::TcpStream, raw: Vec<u8>) -> (u16, String) {
    stream.write_all(&raw).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8(response).unwrap();
    let status = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

fn request(method: &str, target: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut raw = format!(
        "{method} {target} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        raw.push_str(&format!("{name}: {value}\r\n"));
    }
    raw.push_str("\r\n");
    let mut raw = raw.into_bytes();
    raw.extend_from_slice(body);
    raw
}

fn push(body: &str, delivery: &str, signature: Option<&str>) -> Vec<u8> {
    let sig = signature
        .map(str::to_string)
        .unwrap_or_else(|| sign(body.as_bytes()));
    request(
        "POST",
        PATH,
        &[
            ("X-GitHub-Event", "push"),
            ("X-GitHub-Delivery", delivery),
            ("X-Hub-Signature-256", &sig),
        ],
        body.as_bytes(),
    )
}

#[tokio::test]
async fn signed_push_is_queued_once_with_the_scripts_reply() {
    let queue = Arc::new(Recorder::default());
    let addr = start(SECRET, queue.clone()).await;
    let accepted = r#"{"accepted": true, "queued": true, "delivery_id": "d-1", "after_sha": "bbb", "repository": "NousResearch/hermes-agent", "ref": "refs/heads/main"}"#;
    assert_eq!(
        send(addr, push(PUSH, "d-1", None)).await,
        (200, accepted.to_string())
    );
    assert_eq!(
        send(addr, push(PUSH, "d-1", None)).await,
        (
            200,
            accepted.replace("\"queued\": true", "\"queued\": false")
        )
    );
    let seen = queue.0.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].before_sha.as_str(), seen[0].compare_url.as_str()),
        ("aaa", "https://example.test/c")
    );
}

#[tokio::test]
async fn bad_signatures_are_refused_before_anything_else() {
    let queue = Arc::new(Recorder::default());
    let addr = start(SECRET, queue.clone()).await;
    let good = sign(PUSH.as_bytes());
    let wrong_secret = {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"other").unwrap();
        mac.update(PUSH.as_bytes());
        format!(
            "sha256={}",
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    };
    for sig in [
        wrong_secret,
        good.to_uppercase().replace("SHA256=", "sha256="),
        good.replace("sha256=", "sha1="),
        good[..good.len() - 2].to_string(),
        format!("{good}00"),
    ] {
        assert_eq!(
            send(addr, push(PUSH, "d-2", Some(&sig))).await,
            (401, "invalid signature".into()),
            "{sig}"
        );
    }
    let unsigned = request(
        "POST",
        PATH,
        &[("X-GitHub-Event", "push"), ("X-GitHub-Delivery", "d-3")],
        PUSH.as_bytes(),
    );
    assert_eq!(send(addr, unsigned).await.0, 401);
    // A signed body changed in transit fails too.
    let tampered = request(
        "POST",
        PATH,
        &[
            ("X-GitHub-Event", "push"),
            ("X-GitHub-Delivery", "d-4"),
            ("X-Hub-Signature-256", &good),
        ],
        PUSH.replace("bbb", "ccc").as_bytes(),
    );
    assert_eq!(send(addr, tampered).await.0, 401);
    // The signature is checked over the bytes received, before any parsing:
    // a body that is not JSON still gets 401, and the same JSON with other
    // spacing fails the signature made for the original bytes.
    let not_json = push("{not json", "d-7", Some(&sign(b"{}")));
    assert_eq!(send(addr, not_json).await.0, 401);
    let respaced = push(&PUSH.replace(": ", ":"), "d-8", Some(&good));
    assert_eq!(send(addr, respaced).await.0, 401);
    assert!(queue.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn request_checks_follow_the_scripts_order_and_replies() {
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let signed = |event: &str, delivery: &str, body: &str| {
        request(
            "POST",
            PATH,
            &[
                ("X-GitHub-Event", event),
                ("X-GitHub-Delivery", delivery),
                ("X-Hub-Signature-256", &sign(body.as_bytes())),
            ],
            body.as_bytes(),
        )
    };
    let ignored = r#"{"accepted": false, "reason": "ignored_event"}"#.to_string();
    assert_eq!(send(addr, signed("ping", "", "{}")).await, (200, ignored));
    assert_eq!(
        send(addr, signed("push", "", PUSH)).await,
        (400, "missing delivery id".into())
    );
    assert_eq!(
        send(addr, signed("push", "../../etc", PUSH)).await,
        (400, "invalid delivery id".into())
    );
    assert_eq!(
        send(addr, signed("push", "d", "{not json")).await,
        (400, "invalid json".into())
    );
    assert_eq!(
        send(addr, signed("push", "d", "[1]")).await,
        (400, "invalid json".into())
    );
    let other = PUSH
        .replace("NousResearch/hermes-agent", "me/fork")
        .replace("refs/heads/main", "refs/heads/dev");
    let mismatch = r#"{"accepted": false, "reason": "repo_or_ref_mismatch", "repository": "me/fork", "ref": "refs/heads/dev"}"#;
    assert_eq!(
        send(addr, signed("push", "d", &other)).await,
        (200, mismatch.into())
    );
    assert_eq!(
        send(addr, request("GET", PATH, &[], b"")).await,
        (501, "Unsupported method ('GET')".into())
    );
    assert_eq!(
        send(addr, request("POST", "/other", &[], b"")).await,
        (404, "Not Found".into())
    );
    assert_eq!(
        send(addr, request("POST", &format!("{PATH}?x=1"), &[], b""))
            .await
            .0,
        404
    );
}

#[tokio::test]
async fn oversized_bodies_are_refused_without_reading_them() {
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY_BYTES + 1
    );
    assert_eq!(
        send(addr, raw.into_bytes()).await,
        (413, "payload too large".into())
    );
}

#[tokio::test]
async fn the_cap_applies_whilst_the_body_is_read() {
    // Even a body longer than its declared length stops at the cap.
    let body = Full::new(Bytes::from(vec![b'x'; MAX_BODY_BYTES + 1]));
    assert_eq!(
        read_body(body, MAX_BODY_BYTES).await.unwrap_err().0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let body = Full::new(Bytes::from(vec![b'x'; MAX_BODY_BYTES]));
    let read = read_body(body, MAX_BODY_BYTES).await.unwrap();
    // One buffer of the declared length: no second copy, no spare room.
    assert_eq!((read.len(), read.capacity()), (MAX_BODY_BYTES, MAX_BODY_BYTES));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_LENGTH, (MAX_BODY_BYTES + 1).into());
    assert_eq!(
        declared_length(&headers),
        Err((StatusCode::PAYLOAD_TOO_LARGE, "payload too large"))
    );
}

#[tokio::test]
async fn one_peer_holds_at_most_4_slots_and_others_still_get_one() {
    let shared = Shared {
        config: config(SECRET, "127.0.0.1", false),
        queue: Arc::new(Recorder::default()),
        slots: Arc::new(Semaphore::new(MAX_SLOTS)),
        peer_slots: PerPeer::new(),
    };
    let (a, b): (IpAddr, IpAddr) = ("192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap());
    let mut held = Vec::new();
    for _ in 0..MAX_PEER_SLOTS {
        held.push(shared.take_slot(a).await.expect("a slot"));
    }
    let started = std::time::Instant::now();
    assert!(shared.take_slot(a).await.is_none());
    let waited = started.elapsed();
    assert!(waited >= SLOT_WAIT && waited < Duration::from_millis(6500), "{waited:?}");
    assert!(shared.take_slot(b).await.is_some());
    held.pop();
    assert!(shared.take_slot(a).await.is_some(), "a freed slot is reused");
}

#[tokio::test]
async fn a_body_without_a_declared_length_is_refused() {
    // The script read no body without Content-Length and failed the
    // signature; GitHub always sends one.
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let chunk = format!("{:x}\r\n{PUSH}\r\n0\r\n\r\n", PUSH.len());
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\nHost: t\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\
         X-GitHub-Event: push\r\nX-GitHub-Delivery: d-c\r\nX-Hub-Signature-256: {}\r\n\r\n{chunk}",
        sign(PUSH.as_bytes())
    );
    assert_eq!(
        send(addr, raw.into_bytes()).await,
        (411, "length required".into())
    );
}

#[tokio::test]
async fn neighbouring_addresses_share_one_groups_slots() {
    // One host can hold several IPv4 addresses side by side and usually a
    // whole IPv6 /64: limits count per /24 and per /64.
    let shared = Shared {
        config: config(SECRET, "127.0.0.1", false),
        queue: Arc::new(Recorder::default()),
        slots: Arc::new(Semaphore::new(MAX_SLOTS)),
        peer_slots: PerPeer::new(),
    };
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();
    let mut held = Vec::new();
    for peer in ["192.0.2.1", "192.0.2.2", "192.0.2.3", "192.0.2.4"] {
        held.push(shared.take_slot(ip(peer)).await.expect("a slot"));
    }
    for peer in ["2001:db8::1", "2001:db8::2", "2001:db8::3", "2001:db8::4"] {
        held.push(shared.take_slot(ip(peer)).await.expect("a slot"));
    }
    let (v4, v6, mapped) = tokio::join!(
        shared.take_slot(ip("192.0.2.200")),
        shared.take_slot(ip("2001:db8::ffff:1")),
        shared.take_slot(ip("::ffff:192.0.2.9")),
    );
    assert!(v4.is_none() && v6.is_none() && mapped.is_none());
    assert!(shared.take_slot(ip("192.0.3.1")).await.is_some());
    assert!(shared.take_slot(ip("2001:db8:0:1::1")).await.is_some());
}

#[tokio::test]
async fn a_connection_closes_once_its_reply_is_sent() {
    // One request per connection: without `Connection: close` the server
    // still closes straight after replying.
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let raw = "POST /other HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n";
    stream.write_all(raw.as_bytes()).await.unwrap();
    let reply = closed_within(&mut stream, Duration::from_secs(1))
        .await
        .expect("the connection stayed open after the reply");
    assert!(reply.starts_with(b"HTTP/1.1 404"), "{reply:?}");
}

#[test]
fn the_limits_are_the_documented_ones() {
    // The PR body's and module docs' table; change them together.
    assert_eq!(
        (MAX_SLOTS, MAX_PEER_SLOTS, MAX_CONNECTIONS, MAX_PEER_CONNECTIONS),
        (16, 4, 128, 32)
    );
    let secs = Duration::from_secs;
    assert_eq!(
        (HEAD_TIMEOUT, BODY_TIMEOUT, SLOT_WAIT, CONNECTION_DEADLINE),
        (secs(10), secs(10), secs(5), secs(60))
    );
    assert_eq!(MAX_BODY_BYTES, 25 * 1024 * 1024);
}

#[tokio::test]
async fn slow_unsigned_bodies_from_one_peer_leave_a_signed_push_served() {
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let Some(probe) = connect_from("127.0.1.2", addr).await else {
        eprintln!("skipped: 127.0.1.2 cannot be bound on this host");
        return;
    };
    drop(probe);
    // 16 requests from one address that promise a body and never send it.
    let mut held = Vec::new();
    for _ in 0..16 {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!("POST {PATH} HTTP/1.1\r\nHost: t\r\nContent-Length: 10\r\n\r\n");
        stream.write_all(head.as_bytes()).await.unwrap();
        held.push(stream);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let github = connect_from("127.0.1.2", addr).await.unwrap();
    let (status, body) = exchange(github, push(PUSH, "d-b1", None)).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""queued": true"#), "{body}");
    drop(held);
}

#[tokio::test]
async fn an_unfinished_or_absent_request_head_is_dropped_after_10_seconds() {
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let mut partial = tokio::net::TcpStream::connect(addr).await.unwrap();
    partial
        .write_all(format!("POST {PATH} HTTP/1.1\r\nHost: t\r\n").as_bytes())
        .await
        .unwrap();
    let mut silent = tokio::net::TcpStream::connect(addr).await.unwrap();
    let started = std::time::Instant::now();
    let (partial, silent) = tokio::join!(
        closed_within(&mut partial, Duration::from_secs(15)),
        closed_within(&mut silent, Duration::from_secs(15)),
    );
    assert!(partial.is_some(), "a half-sent head kept its connection open");
    assert!(silent.is_some(), "a connection that sent nothing stayed open");
    let waited = started.elapsed();
    assert!(waited >= Duration::from_secs(9) && waited < Duration::from_secs(12), "{waited:?}");
}

#[tokio::test]
async fn one_peer_may_hold_32_connections_and_no_more() {
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let mut held = Vec::new();
    for _ in 0..32 {
        held.push(tokio::net::TcpStream::connect(addr).await.unwrap());
    }
    let mut extra = tokio::net::TcpStream::connect(addr).await.unwrap();
    assert!(
        closed_within(&mut extra, Duration::from_secs(3))
            .await
            .is_some(),
        "a 33rd connection from one address was kept open"
    );
    let (status, body) = exchange(held.pop().unwrap(), push(PUSH, "d-cap", None)).await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn a_body_that_stalls_gets_408_after_10_seconds() {
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!("POST {PATH} HTTP/1.1\r\nHost: t\r\nContent-Length: 10\r\n\r\n12345");
    stream.write_all(head.as_bytes()).await.unwrap();
    let started = std::time::Instant::now();
    let reply = closed_within(&mut stream, Duration::from_secs(15))
        .await
        .expect("no reply within 15 seconds");
    let waited = started.elapsed();
    assert!(waited >= Duration::from_secs(9) && waited < Duration::from_secs(12), "{waited:?}");
    assert!(reply.starts_with(b"HTTP/1.1 408"), "{reply:?}");
}

#[tokio::test]
async fn a_signed_body_resent_under_a_new_delivery_id_is_not_queued_again() {
    let dir = tempfile::tempdir().unwrap();
    let queue = crate::queue::SqliteQueue::open(&dir.path().join("q.db")).unwrap();
    let addr = start(SECRET, Arc::new(queue)).await;
    let first = send(addr, push(PUSH, "d-r1", None)).await;
    assert!(first.1.contains(r#""queued": true"#), "{}", first.1);
    let replay = send(addr, push(PUSH, "d-r2", None)).await;
    assert_eq!(replay.0, 200);
    assert!(replay.1.contains(r#""queued": false"#), "{}", replay.1);
    let next = PUSH.replace("bbb", "ccc");
    let next = send(addr, push(&next, "d-r3", None)).await;
    assert!(next.1.contains(r#""queued": true"#), "{}", next.1);
}

#[tokio::test]
async fn an_absolute_form_target_does_not_match_the_path() {
    // The script compared the raw target, so `http://host/path` was a 404.
    let addr = start(SECRET, Arc::new(Recorder::default())).await;
    let mut raw = push(PUSH, "d-abs", None);
    let line_end = raw.iter().position(|&b| b == b'\r').unwrap();
    raw.splice(
        ..line_end,
        format!("POST http://test{PATH} HTTP/1.1").into_bytes(),
    );
    assert_eq!(send(addr, raw).await, (404, "Not Found".into()));
}

#[tokio::test]
async fn a_queue_failure_answers_500_without_detail() {
    let addr = start(SECRET, Arc::new(Failing)).await;
    assert_eq!(
        send(addr, push(PUSH, "d-5", None)).await,
        (500, "could not queue delivery".into())
    );
}

#[tokio::test]
async fn unsigned_mode_accepts_deliveries_without_a_signature() {
    let queue = Arc::new(Recorder::default());
    let addr = start("", queue.clone()).await;
    let unsigned = request(
        "POST",
        PATH,
        &[("X-GitHub-Event", "push"), ("X-GitHub-Delivery", "d-6")],
        PUSH.as_bytes(),
    );
    assert_eq!(send(addr, unsigned).await.0, 200);
    assert_eq!(queue.0.lock().unwrap().len(), 1);
}

#[test]
fn an_empty_secret_needs_allow_unsigned_and_a_loopback_host() {
    let check = |secret: &str, host: &str, allow_unsigned: bool| {
        check_secret(
            &config(secret, host, allow_unsigned),
            &resolve(host, 8099).unwrap(),
        )
    };
    assert!(check(SECRET, "0.0.0.0", false).is_ok());
    let refused = check("", "127.0.0.1", false).unwrap_err();
    assert!(
        refused.contains("GITHUB_WEBHOOK_SECRET is not set"),
        "{refused}"
    );
    assert!(check("", "127.0.0.1", true).is_ok());
    assert!(check("", "::1", true).is_ok());
    assert!(check("", "0.0.0.0", true)
        .unwrap_err()
        .contains("not loopback"));
}

#[test]
fn signature_check_matches_githubs_documented_example() {
    // The example in GitHub's 'Validating webhook deliveries' guide.
    let header = b"sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
    assert!(verify_signature(b"Hello, World!", SECRET, Some(header)));
    assert!(!verify_signature(b"Hello, World?", SECRET, Some(header)));
    assert!(!verify_signature(b"Hello, World!", SECRET, None));
}
