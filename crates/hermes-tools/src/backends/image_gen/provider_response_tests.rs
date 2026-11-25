use super::*;
use wiremock::matchers::{body_json, body_partial_json, header, method, path};
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

fn xai_backend(server: &MockServer, cache: PathBuf) -> XaiImageGenBackend {
    let mut backend = XaiImageGenBackend::new(XaiImageGenConfig {
        api_key: Some("local-image-fixture".into()),
        base_url: format!("{}/v1/", server.uri()),
        source: "env".into(),
        model: "fixture-image-model".into(),
        resolution: "2k".into(),
        output_dir: cache,
    });
    backend.client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    backend
}

#[tokio::test]
async fn xai_generation_returns_provider_url_and_edit_saves_the_base64_result() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("cache");
    let backend = xai_backend(&server, cache.clone());
    let public_url = format!("{}/public/image.png", server.uri());
    Mock::given(method("POST"))
        .and(path("/v1/images/generations"))
        .and(header("authorization", "Bearer local-image-fixture"))
        .and(body_json(json!({
            "model": "fixture-image-model",
            "prompt": "Fixture landscape",
            "aspect_ratio": "16:9",
            "resolution": "2k"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"file_output": {"public_url": public_url}}],
            "usage": {"images": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut generation = request("  Fixture landscape  ");
    generation.size = Some("Landscape".into());
    let output: Value = serde_json::from_str(&backend.generate(generation).await.unwrap()).unwrap();
    assert_eq!(output["image"], public_url);
    assert_eq!(output["public_url"], public_url);
    assert_eq!(output["usage"]["images"], 1);
    assert_eq!(output["resolution"], "2k");
    assert_eq!(output["transport"], "direct");
    assert_eq!(output["modality"], "text");
    assert!(!cache.exists());

    let local_reference = directory.path().join("reference.png");
    std::fs::write(&local_reference, b"reference").unwrap();
    Mock::given(method("POST"))
        .and(path("/v1/images/edits"))
        .and(header("authorization", "Bearer local-image-fixture"))
        .and(body_json(json!({
            "model": DEFAULT_XAI_IMAGE_EDIT_MODEL,
            "prompt": "Edit the fixture",
            "image": {"type": "image_url", "url": "data:image/png;base64,cmVmZXJlbmNl"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"b64_json": "aW1hZ2U="}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut edit = request("Edit the fixture");
    edit.image_url = Some(local_reference.to_string_lossy().into_owned());
    let output: Value = serde_json::from_str(&backend.generate(edit).await.unwrap()).unwrap();
    assert_eq!(output["modality"], "image");
    assert_eq!(output["source_images"], 1);
    assert_eq!(output["model"], DEFAULT_XAI_IMAGE_EDIT_MODEL);
    assert!(output["resolution"].is_null());
    let saved = Path::new(output["image"].as_str().unwrap());
    assert_eq!(saved.parent(), Some(cache.as_path()));
    assert_eq!(std::fs::read(saved).unwrap(), b"image");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn xai_validation_and_bad_responses_leave_no_cached_image() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("cache");
    let mut backend = xai_backend(&server, cache.clone());
    assert!(matches!(
        backend.generate(request(" ")).await,
        Err(ToolError::InvalidParams(reason)) if reason.contains("Prompt is required")
    ));
    let fixture_key = backend.config.api_key.take();
    assert!(matches!(
        backend.generate(request("Fixture image")).await,
        Err(ToolError::ExecutionFailed(reason)) if reason.contains("No xAI credentials")
    ));
    backend.config.api_key = fixture_key;
    let mut too_many = request("Fixture edit");
    too_many.reference_image_urls = (0..=XAI_MAX_SOURCE_IMAGES)
        .map(|index| format!("{}/fixture-{index}.png", server.uri()))
        .collect();
    assert!(matches!(
        backend.generate(too_many).await,
        Err(ToolError::InvalidParams(reason)) if reason.contains("source images")
    ));
    assert!(server.received_requests().await.unwrap().is_empty());

    let rejection = format!(
        "{}): fixture rejected",
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
    for (index, (response, expected)) in [
        (
            ResponseTemplate::new(422)
                .set_body_json(json!({"error": {"message": "fixture rejected"}})),
            rejection.as_str(),
        ),
        (
            ResponseTemplate::new(200).set_body_string("invalid json"),
            "returned invalid JSON",
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"data": []})),
            "returned no image data",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let prompt = format!("Bad provider fixture {index}");
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .and(body_partial_json(json!({"prompt": prompt})))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            backend.generate(request(&prompt)).await,
            Err(ToolError::ExecutionFailed(reason)) if reason.contains(expected)
        ));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    assert!(!cache.exists());
}
