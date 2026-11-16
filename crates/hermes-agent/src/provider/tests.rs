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

include!("tests/openai_parsing.rs");

include!("tests/anthropic_requests.rs");

include!("tests/anthropic_openrouter_parsing.rs");

#[tokio::test]
async fn a_character_split_across_network_chunks_streams_intact() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 65536];
        let _ = socket.read(&mut request).await;
        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"caf\u{e9}\"}}]}\n\ndata: [DONE]\n\n";
        let bytes = body.as_bytes();
        // End the first HTTP chunk inside the 2-byte é.
        let split = bytes.iter().position(|b| *b == 0xC3).unwrap() + 1;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        for part in [&bytes[..split], &bytes[split..]] {
            socket
                .write_all(format!("{:x}\r\n", part.len()).as_bytes())
                .await
                .unwrap();
            socket.write_all(part).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        socket.write_all(b"0\r\n\r\n").await.unwrap();
    });

    let provider = GenericProvider::new(format!("http://{addr}/v1"), "test-key", "test-model");
    let mut stream =
        provider.chat_completion_stream(&[Message::user("hi")], &[], None, None, None, None);
    let mut content = String::new();
    while let Some(chunk) = stream.next().await {
        if let Some(text) = chunk.unwrap().delta.and_then(|delta| delta.content) {
            content.push_str(&text);
        }
    }
    assert_eq!(content, "caf\u{e9}");
}
