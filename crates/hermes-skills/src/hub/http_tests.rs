use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::{Skill, SkillError, SkillMeta, SkillsHubClient};

async fn stub_skill_hub(
    request_line: &'static str,
    request_body: Option<Value>,
    status: &'static str,
    response_body: &'static str,
) -> (SkillsHubClient, JoinHandle<()>) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let deadline = Duration::from_secs(5);
    let mut client = SkillsHubClient::with_base_url(
        format!("http://{}/api/v1", listener.local_addr().unwrap()),
        "",
    );
    client.client = reqwest::Client::builder()
        .no_proxy()
        .timeout(deadline)
        .build()
        .unwrap();

    let server = tokio::spawn(async move {
        tokio::time::timeout(deadline, async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut request = BufReader::new(stream);
            let mut line = String::new();
            request.read_line(&mut line).await.unwrap();
            assert_eq!(line.trim_end(), request_line);

            let mut length = 0;
            loop {
                line.clear();
                request.read_line(&mut line).await.unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            request.read_exact(&mut body).await.unwrap();
            match request_body {
                Some(expected) => assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected),
                None => assert!(body.is_empty()),
            }

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            request.get_mut().write_all(response.as_bytes()).await.unwrap();
        })
        .await
        .expect("loopback skill hub request completes");
    });
    (client, server)
}

#[tokio::test]
async fn search_sends_the_query_and_an_optional_category() {
    for (category, request) in [
        (None, json!({"query": "garden"})),
        (
            Some("planning"),
            json!({"query": "garden", "category": "planning"}),
        ),
    ] {
        let (client, server) = stub_skill_hub(
            "POST /api/v1/skills/search HTTP/1.1",
            Some(request),
            "200 OK",
            r#"{"skills":[{"name":"garden-plan","category":"planning","description":"Draft a seasonal plan."}]}"#,
        )
        .await;
        let skills = client.search_skills("garden", category).await.unwrap();
        server.await.unwrap();
        assert_eq!(
            skills,
            [SkillMeta {
                name: "garden-plan".into(),
                category: Some("planning".into()),
                description: Some("Draft a seasonal plan.".into()),
            }]
        );
    }
}

#[tokio::test]
async fn updates_skip_empty_input_and_decode_available_versions() {
    let (client, server) = stub_skill_hub(
        "POST /api/v1/skills/check-updates HTTP/1.1",
        Some(json!({"skills":[{"name":"garden-plan","version":""}]})),
        "200 OK",
        r#"{"updates":[{"name":"garden-plan","current_version":"1","latest_version":"2","changelog":"Add autumn planting dates."}]}"#,
    )
    .await;
    assert!(client.check_updates(&[]).await.unwrap().is_empty());
    let updates = client
        .check_updates(&[SkillMeta {
            name: "garden-plan".into(),
            category: Some("planning".into()),
            description: None,
        }])
        .await
        .unwrap();
    server.await.unwrap();
    let [update] = updates.as_slice() else {
        panic!("expected one skill update, got {updates:?}");
    };
    assert_eq!(update.name, "garden-plan");
    assert_eq!(update.current_version, "1");
    assert_eq!(update.latest_version, "2");
    assert_eq!(
        update.changelog.as_deref(),
        Some("Add autumn planting dates.")
    );
}

#[tokio::test]
async fn download_preserves_the_skill_fields() {
    let (client, server) = stub_skill_hub(
        "GET /api/v1/skills/garden-plan HTTP/1.1",
        None,
        "200 OK",
        r##"{"skill":{"name":"garden-plan","content":"# Seasonal plan\n- List planting dates.","category":"planning","description":"Draft a seasonal plan."}}"##,
    )
    .await;
    let skill = client.download_skill("garden-plan").await.unwrap();
    server.await.unwrap();
    assert_eq!(
        skill,
        Skill {
            name: "garden-plan".into(),
            content: "# Seasonal plan\n- List planting dates.".into(),
            category: Some("planning".into()),
            description: Some("Draft a seasonal plan.".into()),
        }
    );
}

#[tokio::test]
async fn download_distinguishes_status_errors_and_invalid_responses() {
    for (status, body, message) in [
        (
            "404 Not Found",
            "Skill not found",
            "Download failed (404 Not Found): Skill not found",
        ),
        ("200 OK", "{}", "Failed to parse download response:"),
    ] {
        let (client, server) = stub_skill_hub(
            "GET /api/v1/skills/garden-plan HTTP/1.1",
            None,
            status,
            body,
        )
        .await;
        let result = client.download_skill("garden-plan").await;
        server.await.unwrap();
        assert!(
            matches!(&result, Err(SkillError::HubError(detail)) if detail.starts_with(message)),
            "unexpected download result: {result:?}"
        );
    }
}
