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

/// Records deliveries in memory; a second delivery id is a duplicate.
#[derive(Default)]
struct Recorder(Mutex<Vec<SyncEvent>>);

impl Publisher for Recorder {
    fn enqueue(&self, event: &SyncEvent) -> Result<bool, String> {
        let mut seen = self.0.lock().unwrap();
        if seen.iter().any(|e| e.delivery_id == event.delivery_id) {
            return Ok(false);
        }
        seen.push(event.clone());
        Ok(true)
    }
}

struct Failing;

impl Publisher for Failing {
    fn enqueue(&self, _: &SyncEvent) -> Result<bool, String> {
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
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
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
async fn bodies_without_a_declared_length_are_capped_as_they_arrive() {
    let body = Body::from(vec![b'x'; MAX_BODY_BYTES + 1]);
    let refused = read_body(&HeaderMap::new(), body).await.unwrap_err();
    assert_eq!(refused.0, StatusCode::PAYLOAD_TOO_LARGE);
    let body = Body::from(vec![b'x'; MAX_BODY_BYTES]);
    assert_eq!(
        read_body(&HeaderMap::new(), body).await.unwrap().len(),
        MAX_BODY_BYTES
    );
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
    assert!(check_secret(&config(SECRET, "0.0.0.0", false)).is_ok());
    let refused = check_secret(&config("", "127.0.0.1", false)).unwrap_err();
    assert!(
        refused.contains("GITHUB_WEBHOOK_SECRET is not set"),
        "{refused}"
    );
    assert!(check_secret(&config("", "127.0.0.1", true)).is_ok());
    assert!(check_secret(&config("", "::1", true)).is_ok());
    assert!(check_secret(&config("", "0.0.0.0", true))
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
