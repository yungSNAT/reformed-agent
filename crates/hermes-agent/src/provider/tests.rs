use super::*;

fn codex_jwt_with_account(account_id: Option<&str>) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = match account_id {
        Some(account_id) => serde_json::json!({
            "sub": "user-xyz",
            "exp": 9_999_999_999_i64,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": "plus"
            }
        }),
        None => serde_json::json!({
            "sub": "user-xyz",
            "exp": 9_999_999_999_i64
        }),
    };
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    format!("{header}.{payload}.sig")
}

fn header_value<'a>(headers: &'a [(String, String)], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

include!("tests/codex_generic.rs");

include!("tests/profile_sanitization.rs");

include!("tests/direct_providers.rs");

include!("tests/openai_parsing.rs");

include!("tests/anthropic_requests.rs");

include!("tests/anthropic_openrouter_parsing.rs");

#[tokio::test]
async fn a_silent_stream_fails_once_the_idle_timeout_passes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n")
            .await
            .unwrap();
        // Then silence, with the connection held open.
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop(socket);
    });

    let client = http_client_with_idle_timeout(None, Duration::from_millis(300));
    let response = client.get(format!("http://{addr}/")).send().await.unwrap();
    let mut stream = response.bytes_stream();
    assert_eq!(&stream.next().await.unwrap().unwrap()[..], b"hello");
    let next = tokio::time::timeout(Duration::from_secs(20), stream.next())
        .await
        .expect("the idle timeout ends the read");
    assert!(next.is_some_and(|read| read.is_err()));
}
