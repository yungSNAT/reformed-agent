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

#[test]
fn local_image_references_distinguish_missing_files_from_read_errors() {
    let fixture = tempfile::tempdir().unwrap();
    let missing = fixture.path().join("missing.gif");
    let unreadable = fixture.path().join("directory.gif");
    std::fs::create_dir(&unreadable).unwrap();
    let image = fixture.path().join("fixture.GIF");
    std::fs::write(&image, b"GIF89a").unwrap();
    let openrouter = openrouter_compat_image_url_part;
    let codex = codex_image_reference_part;
    for (resolve, expected_error) in [
        (
            openrouter as fn(&str) -> Result<Option<String>, ToolError>,
            "Could not read reference image",
        ),
        (codex, "Could not read Codex source image"),
    ] {
        assert!(resolve("  ").unwrap().is_none());
        assert!(resolve(missing.to_str().unwrap()).unwrap().is_none());
        assert!(matches!(
            resolve(unreadable.to_str().unwrap()),
            Err(ToolError::ExecutionFailed(detail)) if detail.contains(expected_error)
        ));
        assert_eq!(
            resolve(image.to_str().unwrap()).unwrap().as_deref(),
            Some("data:image/gif;base64,R0lGODlh")
        );
    }
    assert!(matches!(
        codex("data:;base64,R0lGODlh"),
        Err(ToolError::InvalidParams(detail)) if detail.contains("must include a raster image media type")
    ));
}

#[test]
fn krea_error_bodies_keep_messages_and_bound_unstructured_fallbacks() {
    for (body, expected) in [
        (
            r#"{"error":{"message":" fixture problem "},"detail":"fallback"}"#,
            "fixture problem",
        ),
        (r#"{"error":" fixture rejection "}"#, "fixture rejection"),
        (r#"{"message":" fixture message "}"#, "fixture message"),
        (r#"{"detail":" fixture detail "}"#, "fixture detail"),
        (r#"{"detail":" "}"#, r#"{"detail":" "}"#),
    ] {
        assert_eq!(krea_error_message(body), expected);
    }
    let long = "é".repeat(501);
    assert_eq!(krea_error_message(&long), "é".repeat(500));
}

fn compatible_backend(server: &MockServer, cache: PathBuf) -> OpenRouterCompatImageGenBackend {
    OpenRouterCompatImageGenBackend {
        client: Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap(),
        config: OpenRouterCompatImageGenConfig {
            provider: OpenRouterCompatImageProviderKind::Nous,
            api_key: Some("local-compatible-fixture".into()),
            base_url: server.uri(),
            model: "fixture-image-model".into(),
            output_dir: cache,
        },
    }
}

#[tokio::test]
async fn compatible_image_failures_leave_the_cache_untouched() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("compatible-cache");
    let mut backend = compatible_backend(&server, cache.clone());
    assert!(matches!(
        backend.generate(request("  ")).await,
        Err(ToolError::InvalidParams(_))
    ));
    assert!(!cache.exists());
    let key = backend.config.api_key.take();
    assert!(matches!(
        backend.generate(request("missing credentials")).await,
        Err(ToolError::ExecutionFailed(detail)) if detail.contains("requires credentials")
    ));
    assert!(!cache.exists());
    backend.config.api_key = key;
    assert!(server.received_requests().await.unwrap().is_empty());

    for (prompt, reply, expected) in [
        (
            "rejected fixture",
            ResponseTemplate::new(422)
                .set_body_json(json!({"error": {"message": "fixture validation failed"}})),
            "image generation failed (422 Unprocessable Entity): fixture validation failed",
        ),
        (
            "malformed fixture response",
            ResponseTemplate::new(200).set_body_string("invalid fixture JSON"),
            "returned invalid JSON",
        ),
        (
            "text-only fixture response",
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "text only", "images": []}}]
            })),
            "returned no image. Ensure the model 'fixture-image-model' supports image output",
        ),
    ] {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer local-compatible-fixture"))
            .and(body_partial_json(json!({"messages": [
                {"role": "user", "content": [{"type": "text", "text": prompt}]}
            ]})))
            .respond_with(reply)
            .expect(1)
            .mount(&server)
            .await;
        let error = backend.generate(request(prompt)).await.unwrap_err();
        assert!(matches!(error, ToolError::ExecutionFailed(_)));
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!cache.exists());
    }
    server.verify().await;
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn compatible_image_edits_forward_the_reference_and_cache_the_download() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("compatible-cache");
    let reference = format!("data:image/png;base64,{}", STANDARD.encode(b"reference"));
    let generated = format!("{}/generated", server.uri());
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer local-compatible-fixture"))
        .and(body_partial_json(json!({
            "model": "fixture-image-model",
            "modalities": ["image", "text"],
            "image_config": {"aspect_ratio": "9:16"},
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "Edit the fixture"},
                {"type": "image_url", "image_url": {"url": &reference}}
            ]}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"images": [
                {"type": "image_url", "image_url": {"url": &generated}}
            ]}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/generated"))
        .and(|req: &wiremock::Request| !req.headers.contains_key("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("generated fixture", "image/jpeg"))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ImageGenRuntimeBackend::from(compatible_backend(&server, cache.clone()));
    assert_eq!(backend.capabilities().provider.as_deref(), Some("nous"));
    let mut edit = request("  Edit the fixture  ");
    edit.size = Some("portrait".into());
    edit.image_url = Some(reference);
    let output: Value = serde_json::from_str(&backend.generate(edit).await.unwrap()).unwrap();
    assert_eq!(output["modality"], "image");
    assert_eq!(output["source_images"], 1);
    assert_eq!(output["images"][0]["source_url"], generated);
    let image = Path::new(output["image"].as_str().unwrap());
    assert_eq!(image.parent(), Some(cache.as_path()));
    assert_eq!(image.extension().unwrap(), "jpg");
    assert_eq!(std::fs::read(image).unwrap(), b"generated fixture");
    server.verify().await;
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

fn krea_backend(transport: KreaTransport, cache: PathBuf) -> KreaImageGenBackend {
    KreaImageGenBackend {
        client: Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap(),
        config: KreaImageGenConfig {
            transport,
            model: "krea-2-medium".into(),
            creativity: "medium".into(),
            output_dir: cache,
            poll_initial_interval: Duration::ZERO,
            poll_max_interval: Duration::ZERO,
            poll_timeout: Duration::from_secs(5),
        },
    }
}

#[tokio::test]
async fn krea_submit_errors_preserve_transport_context_and_stop_before_polling() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("krea-cache");
    let direct = krea_backend(
        KreaTransport::Direct {
            api_key: "local-krea-fixture".into(),
            base_url: server.uri(),
        },
        cache.clone(),
    );
    let managed = krea_backend(
        KreaTransport::Managed {
            gateway_origin: server.uri(),
            nous_token: "local-managed-fixture".into(),
        },
        cache.clone(),
    );
    for (backend, bearer, prompt, response, expected) in [
        (
            &direct,
            "Bearer local-krea-fixture",
            "direct rejection",
            ResponseTemplate::new(422).set_body_json(json!({"error": "fixture rejected"})),
            "Krea image generation failed (422 Unprocessable Entity): fixture rejected",
        ),
        (
            &managed,
            "Bearer local-managed-fixture",
            "managed capacity",
            ResponseTemplate::new(429).set_body_json(json!({"error": "fixture busy"})),
            "concurrency cap was hit",
        ),
        (
            &managed,
            "Bearer local-managed-fixture",
            "managed model disabled",
            ResponseTemplate::new(403).set_body_json(json!({"error": "fixture model disabled"})),
            "krea-2-medium' may not be enabled",
        ),
        (
            &direct,
            "Bearer local-krea-fixture",
            "malformed submit response",
            ResponseTemplate::new(200).set_body_string("invalid fixture JSON"),
            "Krea returned invalid JSON on submit",
        ),
        (
            &direct,
            "Bearer local-krea-fixture",
            "empty job identifier",
            ResponseTemplate::new(200).set_body_json(json!({"job_id": "  "})),
            "Krea submit response missing job_id",
        ),
    ] {
        Mock::given(method("POST"))
            .and(path("/generate/image/krea/krea-2/medium"))
            .and(header("authorization", bearer))
            .and(body_partial_json(json!({"prompt": prompt})))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let error = backend.generate(request(prompt)).await.unwrap_err();
        assert!(matches!(error, ToolError::ExecutionFailed(_)));
        assert!(error.to_string().contains(expected), "{error}");
    }
    server.verify().await;
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
    assert!(!cache.exists());
}

#[tokio::test]
async fn krea_poll_reports_terminal_jobs_and_keeps_an_unavailable_image_url() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("krea-cache");
    let backend = krea_backend(
        KreaTransport::Direct {
            api_key: "local-krea-fixture".into(),
            base_url: server.uri(),
        },
        cache.clone(),
    );
    let image_url = format!("{}/fixture.png", server.uri());
    let job = "cache-fallback";
    Mock::given(method("GET"))
        .and(path(format!("/jobs/{job}")))
        .and(header("authorization", "Bearer local-krea-fixture"))
        .respond_with(ResponseTemplate::new(200).set_body_string("invalid fixture JSON"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/fixture.png"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    for (job, result, expected_error) in [
        (
            "fixture-failed",
            json!({"status": "failed", "result": {"error": "fixture rejected"}}),
            Some("Krea job fixture-failed failed: fixture rejected"),
        ),
        (
            "fixture-cancelled",
            json!({"status": "cancelled"}),
            Some("Krea job fixture-cancelled was cancelled"),
        ),
        (
            "missing-image",
            json!({"status": "completed", "result": {"urls": []}}),
            Some("Krea result contained no image URL"),
        ),
        (
            job,
            json!({"completed_at": "2026-01-01T00:00:00Z", "result": {"urls": [&image_url]}}),
            None,
        ),
    ] {
        Mock::given(method("POST"))
            .and(path("/generate/image/krea/krea-2/medium"))
            .and(header("authorization", "Bearer local-krea-fixture"))
            .and(body_partial_json(json!({"prompt": job})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"job_id": job})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/jobs/{job}")))
            .and(header("authorization", "Bearer local-krea-fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(result))
            .expect(1)
            .mount(&server)
            .await;
        let result = backend.generate(request(job)).await;
        if let Some(expected) = expected_error {
            let error = result.unwrap_err();
            assert!(matches!(error, ToolError::ExecutionFailed(_)));
            assert!(error.to_string().contains(expected), "{error}");
        } else {
            let payload: Value = serde_json::from_str(&result.unwrap()).unwrap();
            assert_eq!(payload["job_id"], job);
            assert_eq!(payload["image"], image_url);
            assert_eq!(payload["images"][0]["source_url"], image_url);
        }
    }
    server.verify().await;
    assert_eq!(server.received_requests().await.unwrap().len(), 10);
    assert!(!cache.exists() || cache.read_dir().unwrap().next().is_none());
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
