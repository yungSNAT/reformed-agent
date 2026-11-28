use super::*;
use aes::cipher::BlockEncrypt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wiremock::matchers::{body_json, body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TS: &str = "1700000000";
const PATH: &str = "/wecom/callback";

type Inbound = Arc<RwLock<Option<mpsc::Sender<IncomingMessage>>>>;
type Seen = Arc<RwLock<HashMap<String, Instant>>>;

/// A nonce generated once per test process, so no cryptographic value is hard-coded.
fn nonce() -> &'static str {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NONCE.get_or_init(|| uuid::Uuid::new_v4().simple().to_string())
}

/// Formats as the per-process nonce, so format strings can name it inline.
struct ProcessNonce;

impl std::fmt::Display for ProcessNonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(nonce())
    }
}

const NONCE: ProcessNonce = ProcessNonce;

/// A key per seed, salted once per test process: stable within a run, never hard-coded.
fn key_bytes(seed: u8) -> [u8; 32] {
    static SALT: std::sync::OnceLock<[u8; 16]> = std::sync::OnceLock::new();
    let salt = SALT.get_or_init(|| uuid::Uuid::new_v4().into_bytes());
    std::array::from_fn(|i| seed.wrapping_add(i as u8) ^ salt[i % 16])
}

fn encoding_key(key: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(key)
        .trim_end_matches('=')
        .to_string()
}

fn make_app(name: &str, corp_id: &str, seed: u8) -> WeComCallbackApp {
    WeComCallbackApp {
        name: name.to_string(),
        corp_id: corp_id.to_string(),
        corp_secret: "secret".to_string(),
        agent_id: "100001".to_string(),
        token: "token-abc".to_string(),
        encoding_aes_key: encoding_key(&key_bytes(seed)),
    }
}

fn make_config(host: &str, port: u16, apps: Vec<WeComCallbackApp>) -> WeComCallbackConfig {
    WeComCallbackConfig {
        host: host.to_string(),
        port,
        path: PATH.to_string(),
        apps,
        proxy: Default::default(),
    }
}

fn pad32(mut data: Vec<u8>) -> Vec<u8> {
    let pad = 32 - data.len() % 32;
    let new_len = data.len() + pad;
    data.resize(new_len, pad as u8);
    data
}

fn build_plain(xml: &[u8], xml_len: u32, receive_id: &[u8]) -> Vec<u8> {
    let mut v = vec![9u8; 16];
    v.extend_from_slice(&xml_len.to_be_bytes());
    v.extend_from_slice(xml);
    v.extend_from_slice(receive_id);
    pad32(v)
}

fn cbc_encrypt(key: &[u8; 32], plain: &[u8]) -> String {
    assert_eq!(plain.len() % 16, 0);
    let aes = Aes256::new(GenericArray::from_slice(key));
    let mut prev = key[..16].to_vec();
    let mut out = Vec::with_capacity(plain.len());
    for block in plain.chunks(16) {
        let mut b = GenericArray::clone_from_slice(block);
        for (x, p) in b.iter_mut().zip(prev.iter()) {
            *x ^= *p;
        }
        aes.encrypt_block(&mut b);
        prev.copy_from_slice(&b);
        out.extend_from_slice(&b);
    }
    base64::engine::general_purpose::STANDARD.encode(out)
}

fn encrypt_xml(seed: u8, xml: &str, receive_id: &str) -> String {
    let plain = build_plain(xml.as_bytes(), xml.len() as u32, receive_id.as_bytes());
    cbc_encrypt(&key_bytes(seed), &plain)
}

fn sign(app: &WeComCallbackApp, enc: &str) -> String {
    WeComCallbackAdapter::sha1_signature(&app.token, TS, nonce(), enc)
}

fn err_text<T>(r: Result<T, GatewayError>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => format!("{e}"),
    }
}

fn decrypt_err(app: &WeComCallbackApp, enc: &str) -> String {
    err_text(WeComCallbackAdapter::decrypt_xml(
        app,
        &sign(app, enc),
        TS,
        nonce(),
        enc,
    ))
}

fn new_state() -> (Inbound, mpsc::Receiver<IncomingMessage>, Seen) {
    let (tx, rx) = mpsc::channel(8);
    (
        Arc::new(RwLock::new(Some(tx))),
        rx,
        Arc::new(RwLock::new(HashMap::new())),
    )
}

async fn exchange(
    apps: &[WeComCallbackApp],
    request: &[u8],
    inbound: Inbound,
    seen: Seen,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).await.unwrap();
    let (server, peer) = listener.accept().await.unwrap();
    client.write_all(request).await.unwrap();
    handle_callback_request(server, peer, apps, PATH, inbound, seen)
        .await
        .expect("handler should succeed");
    let mut out = Vec::new();
    client.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

fn post_request(app: &WeComCallbackApp, seed: u8, xml: &str) -> Vec<u8> {
    let enc = encrypt_xml(seed, xml, &app.corp_id);
    let sig = sign(app, &enc);
    let body = format!(
        "<xml><ToUserName>{}</ToUserName><Encrypt>{enc}</Encrypt></xml>",
        app.corp_id
    );
    format!(
        "POST {PATH}?msg_signature={sig}&timestamp={TS}&nonce={NONCE} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[test]
fn config_defaults_apply_when_fields_missing() {
    let cfg: WeComCallbackConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(cfg.host, "0.0.0.0");
    assert_eq!(cfg.port, 8645);
    assert_eq!(cfg.path, "/wecom/callback");
    assert!(cfg.apps.is_empty());
}

#[test]
fn image_extension_maps_known_types() {
    let cases = [
        ("image/jpeg", Some("jpg")),
        ("image/png", Some("png")),
        ("image/gif", Some("gif")),
        ("image/webp", Some("webp")),
        ("image/bmp", Some("bmp")),
        ("image/tiff", Some("tiff")),
        ("image/svg+xml", Some("svg")),
        ("image/heic", Some("heic")),
        ("image/heif", Some("heif")),
        ("IMAGE/AVIF; q=1", Some("avif")),
        ("image/x-unknown", None),
        ("text/html", None),
    ];
    for (ct, expected) in cases {
        assert_eq!(
            image_extension_from_content_type(Some(ct)),
            expected,
            "{ct}"
        );
    }
    assert_eq!(
        remote_image_file_name("https://x.example/a/pic", Some("image/gif")),
        "pic.gif"
    );
}

#[test]
fn content_length_skips_other_headers() {
    assert_eq!(
        http_content_length("Host: example\r\nX-Other: 5\r\nContent-Length: 12"),
        Some(12)
    );
    assert_eq!(http_content_length("Host: example\r\nno-colon-line"), None);
}

#[test]
fn new_rejects_missing_or_invalid_apps() {
    let e = err_text(WeComCallbackAdapter::new(make_config(
        "127.0.0.1",
        0,
        vec![],
    )));
    assert!(e.contains("at least one app"), "{e}");

    let mut bad = make_app("a", "ww_test", 1);
    bad.encoding_aes_key = "short".to_string();
    let e = err_text(WeComCallbackAdapter::new(make_config(
        "127.0.0.1",
        0,
        vec![bad],
    )));
    assert!(e.contains("invalid wecom_callback app config"), "{e}");

    let mut bad = make_app("a", "ww_test", 1);
    bad.token = "   ".to_string();
    let e = err_text(WeComCallbackAdapter::new(make_config(
        "127.0.0.1",
        0,
        vec![bad],
    )));
    assert!(e.contains("invalid wecom_callback app config"), "{e}");
}

#[test]
fn xml_tag_extracts_and_handles_missing() {
    let xml = "<xml><A>one</A><B>two</B><C>open</xml>";
    assert_eq!(
        WeComCallbackAdapter::xml_tag(xml, "A"),
        Some("one".to_string())
    );
    assert_eq!(
        WeComCallbackAdapter::xml_tag(xml, "B"),
        Some("two".to_string())
    );
    assert_eq!(WeComCallbackAdapter::xml_tag(xml, "Z"), None);
    assert_eq!(WeComCallbackAdapter::xml_tag(xml, "C"), None);
}

#[test]
fn pkcs7_unpad_validates_padding() {
    let e = err_text(WeComCallbackAdapter::pkcs7_unpad(&[]));
    assert!(e.contains("empty decrypted payload"), "{e}");
    let e = err_text(WeComCallbackAdapter::pkcs7_unpad(&[1, 0]));
    assert!(e.contains("invalid pkcs7 padding"), "{e}");
    let e = err_text(WeComCallbackAdapter::pkcs7_unpad(&[33u8; 40]));
    assert!(e.contains("invalid pkcs7 padding"), "{e}");
    let e = err_text(WeComCallbackAdapter::pkcs7_unpad(&[5, 5]));
    assert!(e.contains("invalid pkcs7 padding"), "{e}");
    let e = err_text(WeComCallbackAdapter::pkcs7_unpad(&[1, 2, 3, 3]));
    assert!(e.contains("malformed pkcs7 padding"), "{e}");
    assert_eq!(
        WeComCallbackAdapter::pkcs7_unpad(&[1, 2, 2]).unwrap(),
        vec![1]
    );
}

#[test]
fn decrypt_xml_roundtrip_succeeds() {
    let app = make_app("a", "ww_test", 3);
    let xml = "<xml><Content>hi</Content></xml>";
    let enc = encrypt_xml(3, xml, "ww_test");
    let out =
        WeComCallbackAdapter::decrypt_xml(&app, &sign(&app, &enc), TS, nonce(), &enc).unwrap();
    assert_eq!(out, xml);
}

#[test]
fn decrypt_xml_rejects_bad_keys() {
    let mut app = make_app("a", "ww_test", 3);
    app.encoding_aes_key = "*".repeat(43);
    let e = decrypt_err(&app, "AAAA");
    assert!(e.contains("invalid aes key"), "{e}");

    let short = base64::engine::general_purpose::STANDARD.encode([7u8; 29]);
    app.encoding_aes_key = short.trim_end_matches('=').to_string();
    let e = decrypt_err(&app, "AAAA");
    assert!(e.contains("invalid aes key length"), "{e}");
}

include!("wecom_callback_part2_coverage_gen.rs");
