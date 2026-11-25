use super::*;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn backend(server: &MockServer, cache: PathBuf) -> OpenAiImageGenBackend {
    OpenAiImageGenBackend {
        client: Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap(),
        config: OpenAiImageGenConfig {
            api_key: Some("local-image-fixture".into()),
            base_url: server.uri(),
            tier_id: "gpt-image-2-low".into(),
            quality: "low".into(),
            output_dir: cache,
        },
    }
}

fn request(prompt: &str) -> ImageGenerateRequest {
    ImageGenerateRequest {
        prompt: prompt.into(),
        size: None,
        style: None,
        n: None,
        image_url: None,
        reference_image_urls: Vec::new(),
    }
}

#[tokio::test]
async fn malformed_provider_responses_do_not_create_an_image() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("cache");
    let backend = backend(&server, cache.clone());

    for (prompt, response, expected) in [
        (
            "fixture rejected",
            ResponseTemplate::new(422)
                .set_body_json(json!({"error": {"message": "fixture validation failure"}})),
            "fixture validation failure",
        ),
        (
            "fixture invalid JSON",
            ResponseTemplate::new(200).set_body_string("invalid json"),
            "returned invalid JSON",
        ),
        (
            "fixture no image",
            ResponseTemplate::new(200).set_body_json(json!({"data": []})),
            "returned no image data",
        ),
        (
            "fixture no data",
            ResponseTemplate::new(200).set_body_json(json!({"data": [{}]})),
            "contained neither b64_json nor URL",
        ),
        (
            "fixture malformed base64",
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"b64_json": "%"}]})),
            "was not valid base64",
        ),
    ] {
        Mock::given(method("POST"))
            .and(path("/images/generations"))
            .and(header("authorization", "Bearer local-image-fixture"))
            .and(body_partial_json(json!({"prompt": prompt})))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let error = backend.generate(request(prompt)).await.unwrap_err();
        assert!(matches!(error, ToolError::ExecutionFailed(_)));
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!cache.exists());
    }
    server.verify().await;
}

#[tokio::test]
async fn multipart_edit_loads_local_and_remote_input_and_saves_the_result() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let local_reference = directory.path().join("reference-local.png");
    let output_dir = directory.path().join("cache");
    std::fs::write(&local_reference, b"local-reference-data").unwrap();

    Mock::given(method("GET"))
        .and(path("/reference-remote.jpg"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("remote-reference-data", "image/jpeg"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/result"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("fixture-result-data", "image/webp"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/images/edits"))
        .and(header("authorization", "Bearer local-image-fixture"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"file_output": {"public_url": format!("{}/result", server.uri())}}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let backend = backend(&server, output_dir.clone());
    let mut edit = request("fixture multipart edit");
    edit.image_url = Some(format!("{}/reference-remote.jpg", server.uri()));
    edit.reference_image_urls = vec![local_reference.to_string_lossy().into_owned()];
    let output: Value = serde_json::from_str(&backend.generate(edit).await.unwrap()).unwrap();
    assert_eq!(output["source_images"], 2);
    assert_eq!(output["modality"], "image");
    let saved = Path::new(output["image"].as_str().unwrap());
    assert_eq!(saved.parent(), Some(output_dir.as_path()));
    assert_eq!(saved.extension(), Some(std::ffi::OsStr::new("webp")));
    assert_eq!(std::fs::read(saved).unwrap(), b"fixture-result-data");

    let requests = server.received_requests().await.unwrap();
    let multipart = requests
        .iter()
        .find(|request| request.url.path() == "/images/edits")
        .unwrap();
    let body = std::str::from_utf8(&multipart.body).unwrap();
    assert_eq!(body.matches("name=\"image[]\"").count(), 2);
    let media_type = multipart
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    let (_, boundary) = media_type
        .split_once("boundary=")
        .expect("multipart boundary");
    let delimiter = format!("--{boundary}");
    for (filename, mime, data) in [
        (
            "reference-remote.jpg",
            "image/jpeg",
            "remote-reference-data",
        ),
        ("reference-local.png", "image/png", "local-reference-data"),
    ] {
        let filename_field = format!("filename=\"{filename}\"");
        let part = body
            .split(&delimiter)
            .find(|part| part.contains(&filename_field))
            .expect("multipart file");
        let (headers, payload) = part.split_once("\r\n\r\n").expect("multipart headers");
        assert!(headers.contains("name=\"image[]\""), "{headers}");
        assert!(headers.contains(mime), "{headers}");
        assert_eq!(payload.trim_end_matches("\r\n"), data);
    }
    server.verify().await;
}
