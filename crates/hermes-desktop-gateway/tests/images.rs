//! `image.attach` (an image named by a path on the gateway's host, as the terminal UI and a
//! local desktop send it) and `image.detach` (a removed attachment chip). The model is a
//! scripted OpenAI-compatible endpoint that keeps each request body.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

type Requests = Arc<Mutex<Vec<Value>>>;

/// Streams `ok` and keeps the request body.
async fn complete(State(requests): State<Requests>, Json(body): Json<Value>) -> impl IntoResponse {
    requests.lock().unwrap().push(body);
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let sse = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk(json!({"content": "ok"}), Value::Null),
        chunk(json!({}), json!("stop"))
    );
    ([("content-type", "text/event-stream")], sse)
}

struct Started {
    addr: SocketAddr,
    requests: Requests,
    home: tempfile::TempDir,
}

/// A gateway whose model takes image parts (`image_input_mode: native`).
async fn start() -> Started {
    let requests = Requests::default();
    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(complete))
        .with_state(Arc::clone(&requests));
    tokio::spawn(async move { axum::serve(model_listener, app).await });

    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "model:\n  default: mock-model\n  provider: mock\nproviders:\n  mock:\n    \
             api: http://{model_addr}/v1\n    name: Mock\n    api_mode: chat_completions\n    \
             key_env: DESKTOP_IMAGES_TEST_KEY\n    models:\n      mock-model: {{}}\n\
             agent:\n  image_input_mode: native\n"
        ),
    )
    .unwrap();
    let config = hermes_config::load_config(home.path().to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chat = hermes_desktop_gateway::Chat::new(config);
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        chat,
    ));
    Started {
        addr,
        requests,
        home,
    }
}

struct Client {
    socket: Socket,
}

async fn connect(addr: SocketAddr) -> Client {
    let request = format!("ws://{addr}/api/ws?token={TOKEN}")
        .into_client_request()
        .unwrap();
    let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let mut client = Client { socket };
    assert_eq!(client.next_json().await["params"]["type"], "gateway.ready");
    client
}

impl Client {
    async fn next_json(&mut self) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(20), self.socket.next())
                .await
                .expect("no frame within 20 seconds")
                .expect("socket closed");
            match frame.unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    /// The reply to `method`; events that arrive meanwhile are dropped.
    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        loop {
            let frame = self.next_json().await;
            if frame["method"] != "event" {
                assert_eq!(frame["id"], id, "{frame}");
                return frame;
            }
        }
    }

    async fn create(&mut self) -> String {
        let reply = self.call(1, "session.create", json!({"cols": 96})).await;
        reply["result"]["session_id"].as_str().unwrap().to_string()
    }

    /// Submits `text` and waits for the turn's `message.complete`.
    async fn turn(&mut self, id: u64, session: &str, text: &str) {
        let reply = self
            .call(
                id,
                "prompt.submit",
                json!({"session_id": session, "text": text}),
            )
            .await;
        assert!(reply["result"].is_object(), "{reply}");
        loop {
            let frame = self.next_json().await;
            if frame["params"]["type"] == "message.complete" {
                assert_eq!(frame["params"]["payload"]["status"], "complete", "{frame}");
                return;
            }
        }
    }
}

/// A small payload that starts as a PNG does: the gateway checks only the signature.
fn png(tag: &str) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn data_url(bytes: &[u8]) -> String {
    format!("data:image/png;base64,{}", b64(bytes))
}

/// The files under `<home>/images`.
fn staged_images(home: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(home.join("images")) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

/// The image data URLs the model was sent with request `n`'s last message.
fn sent_images(requests: &Requests, n: usize) -> Vec<String> {
    let requests = requests.lock().unwrap();
    let last = requests[n]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    match last["content"].as_array() {
        Some(parts) => parts
            .iter()
            .filter_map(|part| part["image_url"]["url"].as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
    }
}

/// Each refusal names upstream's code, and none stages a file.
#[tokio::test]
async fn image_attach_refuses_bad_requests() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let notes = folder.path().join("notes.txt");
    std::fs::write(&notes, "plain text").unwrap();
    let fake = folder.path().join("fake.png");
    std::fs::write(&fake, "not an image").unwrap();
    let big = folder.path().join("big.png");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(25 * 1024 * 1024 + 1).unwrap();
    let missing = folder.path().join("missing.png");
    let text = |p: &Path| p.display().to_string();

    let cases = [
        (json!({"path": text(&notes)}), 4000),
        (json!({"session_id": 7, "path": text(&notes)}), 4000),
        (json!({"session_id": "nope", "path": text(&notes)}), 4001),
        (json!({"session_id": session}), 4015),
        (json!({"session_id": session, "path": "   "}), 4015),
        (json!({"session_id": session, "path": 3}), 4000),
        (json!({"session_id": session, "path": text(&missing)}), 4016),
        (
            json!({"session_id": session, "path": text(folder.path())}),
            4016,
        ),
        (json!({"session_id": session, "path": text(&notes)}), 4016),
        (json!({"session_id": session, "path": text(&fake)}), 4016),
        (json!({"session_id": session, "path": text(&big)}), 4018),
    ];
    for (n, (params, code)) in cases.iter().enumerate() {
        let reply = client
            .call(10 + n as u64, "image.attach", params.clone())
            .await;
        assert_eq!(reply["error"]["code"], *code, "{params}: {reply}");
    }
    let reply = client
        .call(
            30,
            "image.attach",
            json!({"session_id": session, "path": text(&missing)}),
        )
        .await;
    assert_eq!(
        reply["error"]["message"],
        format!("image not found: {}", missing.display()),
        "{reply}"
    );
    let reply = client
        .call(
            31,
            "image.attach",
            json!({"session_id": session, "path": text(&notes)}),
        )
        .await;
    assert_eq!(
        reply["error"]["message"], "unsupported image: notes.txt",
        "{reply}"
    );
    assert!(staged_images(started.home.path()).is_empty());
}

/// A path the client names is copied under the gateway's home, and the next prompt sends the
/// model the copy, as `image.attach_bytes` does with bytes.
#[tokio::test]
async fn an_image_named_by_path_goes_with_the_next_prompt() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("shot one.png");
    let image = png("by-path");
    std::fs::write(&source, &image).unwrap();

    // Quoted, as a terminal drop of a path with a space gives it, with words after it.
    let raw = format!("\"{}\" what is this", source.display());
    let reply = client
        .call(
            2,
            "image.attach",
            json!({"session_id": session, "path": raw}),
        )
        .await;
    let result = &reply["result"];
    assert_eq!(result["attached"], true, "{reply}");
    assert_eq!(result["name"], "shot one.png");
    assert_eq!(result["remainder"], "what is this");
    assert_eq!(result["text"], "what is this");
    assert_eq!(result["count"], 1);
    assert_eq!(result["bytes"], image.len());
    let staged = PathBuf::from(result["path"].as_str().unwrap());
    assert_eq!(staged_images(started.home.path()), vec![staged.clone()]);
    assert_eq!(std::fs::read(&staged).unwrap(), image);

    // The copy is what goes: a later change to the source does not reach the model.
    std::fs::write(&source, png("changed")).unwrap();
    client.turn(3, &session, "describe").await;
    assert_eq!(sent_images(&started.requests, 0), vec![data_url(&image)]);
}

/// The ways a terminal drop spells a path: escaped spaces, a `file://` URL, a path relative to
/// the working folder, and an unquoted path with spaces followed by words.
#[tokio::test]
async fn dropped_path_spellings_resolve() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("my shot.png");
    std::fs::write(&source, png("spelling")).unwrap();
    let escaped = source.display().to_string().replace(' ', "\\ ");
    let url = url::Url::from_file_path(&source).unwrap().to_string();
    let spellings = [
        (format!("{escaped} and words"), "and words"),
        (url, ""),
        (format!("{} tell me", source.display()), "tell me"),
    ];
    for (n, (raw, remainder)) in spellings.iter().enumerate() {
        let reply = client
            .call(
                10 + n as u64,
                "image.attach",
                json!({"session_id": session, "path": raw}),
            )
            .await;
        assert_eq!(reply["result"]["name"], "my shot.png", "{raw}: {reply}");
        assert_eq!(reply["result"]["remainder"], *remainder, "{raw}");
    }
    let reply = client
        .call(
            20,
            "image.attach",
            json!({"session_id": session, "path": "my shot.png"}),
        )
        .await;
    assert_eq!(
        reply["error"]["code"], 4016,
        "relative to the gateway's folder: {reply}"
    );
    // With no words after the path, the text is upstream's attachment line.
    let reply = client
        .call(
            21,
            "image.attach",
            json!({"session_id": session, "path": source.display().to_string()}),
        )
        .await;
    assert_eq!(
        reply["result"]["text"],
        "[User attached image: my shot.png]"
    );
    assert_eq!(reply["result"]["count"], 4);
}

/// A detached image is not sent and its staged copy is removed; the others still go.
#[tokio::test]
async fn a_detached_image_is_not_sent() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let (kept, dropped) = (png("kept"), png("dropped"));
    let attach = |bytes: &[u8]| json!({"session_id": session, "content_base64": b64(bytes)});
    let reply = client.call(2, "image.attach_bytes", attach(&kept)).await;
    assert_eq!(reply["result"]["count"], 1, "{reply}");
    let reply = client.call(3, "image.attach_bytes", attach(&dropped)).await;
    let dropped_path = reply["result"]["path"].as_str().unwrap().to_string();

    let detach = json!({"session_id": session, "path": dropped_path});
    let reply = client.call(4, "image.detach", detach.clone()).await;
    assert_eq!(
        reply["result"],
        json!({"detached": true, "count": 1}),
        "{reply}"
    );
    assert!(!Path::new(&dropped_path).exists());
    let reply = client.call(5, "image.detach", detach).await;
    assert_eq!(
        reply["result"],
        json!({"detached": false, "count": 1}),
        "{reply}"
    );

    client.turn(6, &session, "describe").await;
    assert_eq!(sent_images(&started.requests, 0), vec![data_url(&kept)]);
}

#[tokio::test]
async fn image_detach_refuses_bad_requests() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let cases = [
        (json!({"path": "/x.png"}), 4000),
        (json!({"session_id": "nope", "path": "/x.png"}), 4001),
        (json!({"session_id": session}), 4015),
        (json!({"session_id": session, "path": ""}), 4015),
        (json!({"session_id": session, "path": false}), 4000),
    ];
    for (n, (params, code)) in cases.iter().enumerate() {
        let reply = client
            .call(10 + n as u64, "image.detach", params.clone())
            .await;
        assert_eq!(reply["error"]["code"], *code, "{params}: {reply}");
    }
}

/// A PNG signature followed by zeros, `len` bytes long, made without writing them.
fn sparse_png(path: &Path, len: u64) {
    std::fs::write(path, png("")).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(len)
        .unwrap();
}

/// An empty image file and a session at its staging limits are refused, and nothing more is
/// staged: 16 images, or 64 MiB, whichever comes first.
#[tokio::test]
async fn image_attach_keeps_each_session_within_its_limits() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let attach = |p: &Path| json!({"session_id": session, "path": p.display().to_string()});

    let empty = folder.path().join("empty.png");
    std::fs::write(&empty, b"").unwrap();
    let reply = client.call(2, "image.attach", attach(&empty)).await;
    assert_eq!(reply["error"]["code"], 4017, "{reply}");

    // 24 MiB each: the third would pass 64 MiB.
    let big = folder.path().join("big.png");
    sparse_png(&big, 24 << 20);
    for n in 0..2 {
        let reply = client.call(3 + n, "image.attach", attach(&big)).await;
        assert_eq!(reply["result"]["count"], n + 1, "{reply}");
    }
    let reply = client.call(5, "image.attach", attach(&big)).await;
    assert_eq!(reply["error"]["code"], 4018, "{reply}");
    assert_eq!(staged_images(started.home.path()).len(), 2);

    let session = client.create().await;
    let small = folder.path().join("small.png");
    std::fs::write(&small, png("small")).unwrap();
    let attach = json!({"session_id": session, "path": small.display().to_string()});
    for n in 0..16 {
        let reply = client.call(10 + n, "image.attach", attach.clone()).await;
        assert_eq!(reply["result"]["count"], n + 1, "{reply}");
    }
    let reply = client.call(30, "image.attach", attach).await;
    assert_eq!(reply["error"]["code"], 4018, "{reply}");
    assert_eq!(staged_images(started.home.path()).len(), 18);
}

/// A network share or device namespace path is refused without being opened.
#[tokio::test]
async fn image_attach_refuses_share_and_device_paths() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let paths = [
        r"\nohost.invalid\share\a.png",
        r"\?\C:\a.png",
        r"\.\pipe\a.png",
        "file://nohost.invalid/share/a.png",
    ];
    for (n, path) in paths.iter().enumerate() {
        let params = json!({"session_id": session, "path": path});
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            client.call(10 + n as u64, "image.attach", params),
        )
        .await
        .expect("refused at once");
        assert_eq!(reply["error"]["code"], 4016, "{path}: {reply}");
    }
}

/// The read is bounded by what it reads, not by the size the file reports: the kernel's page
/// map reports 0 bytes and holds far more.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn image_attach_bounds_the_read_itself() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let link = folder.path().join("map.png");
    std::os::unix::fs::symlink("/proc/self/pagemap", &link).unwrap();
    let params = json!({"session_id": session, "path": link.display().to_string()});
    let reply = client.call(2, "image.attach", params).await;
    assert_eq!(reply["error"]["code"], 4018, "{reply}");
    assert!(staged_images(started.home.path()).is_empty());
}

/// A FIFO is refused at once instead of waiting for a writer.
#[cfg(unix)]
#[tokio::test]
async fn image_attach_refuses_a_fifo() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let folder = tempfile::tempdir().unwrap();
    let fifo = folder.path().join("pipe.png");
    let name = std::ffi::CString::new(fifo.display().to_string()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let params = json!({"session_id": session, "path": fifo.display().to_string()});
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        client.call(2, "image.attach", params),
    )
    .await
    .expect("refused at once");
    assert_eq!(reply["error"]["code"], 4016, "{reply}");
}

/// A session cannot take back another session's staged image.
#[tokio::test]
async fn image_detach_reaches_only_its_own_session() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let first = client.create().await;
    let second = client.create().await;
    let attach = json!({"session_id": first, "content_base64": b64(&png("mine"))});
    let reply = client.call(2, "image.attach_bytes", attach).await;
    let path = reply["result"]["path"].as_str().unwrap().to_string();

    let detach = json!({"session_id": second, "path": path});
    let reply = client.call(3, "image.detach", detach).await;
    assert_eq!(
        reply["result"],
        json!({"detached": false, "count": 0}),
        "{reply}"
    );
    assert!(Path::new(&path).exists());
}
