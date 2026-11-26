use super::*;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FAL_NOT_CONFIGURED: &str =
    "FAL_KEY not set and Nous-managed fal-queue gateway is not configured.";

fn request(model: &str) -> VideoGenerateRequest {
    VideoGenerateRequest {
        prompt: "  A train crossing a bridge  ".into(),
        image_url: None,
        reference_image_urls: Vec::new(),
        model: Some(model.into()),
        model_explicit: false,
        aspect_ratio: "16:9".into(),
        resolution: "720p".into(),
        duration: None,
        seed: None,
        audio: None,
        negative_prompt: None,
    }
}

fn execution_error<T: std::fmt::Debug>(result: Result<T, ToolError>) -> String {
    match result.unwrap_err() {
        ToolError::ExecutionFailed(message) => message,
        other => panic!("expected an execution error, got {other:?}"),
    }
}

// Keep home and provider settings out of the environment shared by parallel tests.
fn isolated_home(test_name: &str, settings: &[(&str, &str)]) -> Option<PathBuf> {
    const MARKER: &str = "HERMES_VIDEO_COVERAGE_CHILD_HOME";
    if let Some(home) = std::env::var_os(MARKER) {
        return Some(PathBuf::from(home));
    }

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.env_clear();
    for key in ["PATH", "SystemRoot", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }
    // Resolve relative profile paths against the parent working directory.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        child.env(
            "LLVM_PROFILE_FILE",
            std::env::current_dir().unwrap().join(profile),
        );
    }
    let output = child
        .args([
            "--exact",
            &format!("{module}::{test_name}"),
            "--nocapture",
            "--color=never",
        ])
        .current_dir(&home)
        .env(MARKER, &home)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("HERMES_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("APPDATA", home.join("config"))
        .env("LOCALAPPDATA", home.join("local"))
        .env(
            "HERMES_AUTH_FILE",
            home.join(".hermes-agent-ultra/auth.json"),
        )
        .envs(settings.iter().copied())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "isolated test {test_name} ({})\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

#[test]
fn video_coverage_config_in_isolated_home() {
    let Some(home) = isolated_home(
        "video_coverage_config_in_isolated_home",
        &[
            ("FAL_KEY", "   "),
            ("TEST_VIDEO_INTEGER", " 42 "),
            ("TEST_VIDEO_INVALID_INTEGER", " -1 "),
        ],
    ) else {
        return;
    };

    assert_eq!(env_u64("TEST_VIDEO_INTEGER"), Some(42));
    assert_eq!(env_u64("TEST_VIDEO_INVALID_INTEGER"), None);
    assert_eq!(env_u64("TEST_VIDEO_UNSET_INTEGER"), None);
    assert_eq!(expand_user_path("~"), home);
    assert_eq!(
        expand_user_path("~/pictures/frame.png"),
        home.join("pictures/frame.png")
    );

    assert!(hermes_config::paths::auth_json_path().starts_with(&home));
    assert!(hermes_config::cli_config_path().starts_with(&home));
    assert!(hermes_config::config_path().starts_with(&home));
    assert_eq!(
        execution_error(FalVideoGenBackend::from_env_or_managed()),
        FAL_NOT_CONFIGURED
    );
    assert!(auth_store_candidates().is_empty());
    assert_eq!(
        execution_error(XaiVideoGenBackend::from_env_or_auth_store()),
        "No xAI credentials found. Sign in via `hermes auth add xai-oauth` or set XAI_API_KEY."
    );

    let preferred = home.join(".hermes-agent-ultra/auth.json");
    let legacy = home.join(".hermes/auth.json");
    for file in [&preferred, &legacy] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    }
    std::fs::write(
        &preferred,
        r#"{"providers":{"xai-oauth":{"access_token":"  preferred  ","api_base_url":"https://example.invalid/v1///"}}}"#,
    )
    .unwrap();
    std::fs::write(&legacy, r#"{"providers":{"xai":{"api_key":"legacy"}}}"#).unwrap();

    assert_eq!(auth_store_candidates(), vec![preferred.clone(), legacy]);
    let backend = XaiVideoGenBackend::from_env_or_auth_store().unwrap();
    assert_eq!(
        backend.credentials(),
        Some(&XaiVideoCredentials {
            api_key: "preferred".into(),
            base_url: "https://example.invalid/v1".into(),
            source: preferred.display().to_string(),
        })
    );
    assert_eq!(backend.transport_label(), "auth-store");

    std::fs::write(preferred, "{").unwrap();
    let fallback = resolve_xai_video_credentials().unwrap();
    assert_eq!(fallback.api_key, "legacy");
    assert_eq!(fallback.base_url, "https://api.x.ai/v1");
}

#[test]
fn invalid_or_unrelated_yaml_preserves_video_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.yaml");
    let mut models = vec!["existing-model".to_string()];
    let mut providers = vec!["existing-provider".to_string()];
    for yaml in ["video_gen: [", "unrelated: true"] {
        std::fs::write(&config, yaml).unwrap();
        collect_video_model_candidates(&config, &mut models);
        collect_video_provider_candidates(&config, &mut providers);
        assert_eq!(models, ["existing-model"]);
        assert_eq!(providers, ["existing-provider"]);
    }

    std::fs::write(
        &config,
        "video_gen:\n  fal:\n    model: veo3.1\n  model: happy-horse\n  provider: FAL-AI\n  backend: grok\n",
    )
    .unwrap();
    collect_video_model_candidates(&config, &mut models);
    collect_video_provider_candidates(&config, &mut providers);
    assert_eq!(models, ["existing-model", "veo3.1", "happy-horse"]);
    assert_eq!(providers, ["existing-provider", "FAL-AI", "grok"]);

    for alias in ["fal", " FAL-AI ", "fal_ai", "Fal-Queue"] {
        assert_eq!(normalize_video_provider(alias), Some("fal"));
    }
    assert_eq!(normalize_video_provider("custom-provider"), None);
}

#[tokio::test]
async fn unconfigured_video_backends_report_errors_without_sending() {
    let fal = FalVideoGenBackend::unconfigured();
    assert_eq!(fal.transport_label(), "unconfigured");
    assert_eq!(
        execution_error(fal.transport.auth_header()),
        FAL_NOT_CONFIGURED
    );
    assert_eq!(
        execution_error(fal.submit_managed("unused", &json!({})).await),
        FAL_NOT_CONFIGURED
    );
    assert_eq!(
        execution_error(fal.submit_direct_queue("unused", &json!({})).await),
        FAL_NOT_CONFIGURED
    );
    let fal = VideoGenBackend::from(fal);
    let mut input = request("pixverse-v6");
    input.image_url = Some("  ".into());
    assert_eq!(
        execution_error(fal.generate_video(input).await),
        FAL_NOT_CONFIGURED
    );

    let xai = XaiVideoGenBackend::unconfigured();
    assert_eq!(xai.transport_label(), "unconfigured");
    assert!(xai.credentials().is_none());
    let error = execution_error(
        VideoGenBackend::from(xai)
            .generate_video(request("unused"))
            .await,
    );
    assert!(error.contains("set XAI_API_KEY"), "{error}");
}

#[test]
fn fal_payload_respects_optional_parameters_and_family_capabilities() {
    let mut input = request("kling-v3-4k");
    input.image_url = Some(" https://example.invalid/frame.png ".into());
    input.seed = Some(17);
    input.duration = Some(1);
    input.audio = Some(false);
    input.negative_prompt = Some("flicker".into());

    let kling = build_payload(family_by_id("kling-v3-4k").unwrap(), &input);
    assert_eq!(kling.duration, Some(3));
    assert_eq!(kling.aspect_ratio.as_deref(), Some("16:9"));
    assert_eq!(
        Value::Object(kling.payload),
        json!({
            "prompt": "A train crossing a bridge",
            "start_image_url": "https://example.invalid/frame.png",
            "seed": 17,
            "aspect_ratio": "16:9",
            "duration": "3",
            "generate_audio": false,
            "negative_prompt": "flicker"
        })
    );

    let horse = build_payload(family_by_id("happy-horse").unwrap(), &input);
    assert_eq!(horse.duration, None);
    assert_eq!(horse.aspect_ratio, None);
    assert_eq!(
        Value::Object(horse.payload),
        json!({
            "prompt": "A train crossing a bridge",
            "image_url": "https://example.invalid/frame.png",
            "seed": 17
        })
    );

    input.audio = None;
    let kling = build_payload(family_by_id("kling-v3-4k").unwrap(), &input);
    assert!(!kling.payload.contains_key("generate_audio"));
}

#[test]
fn xai_payload_distinguishes_text_image_and_explicit_model() {
    let mut input = request("   ");
    input.model_explicit = true;
    input.prompt = " \n ".into();
    assert!(matches!(
        build_xai_payload(&input),
        Err(ToolError::InvalidParams(message))
            if message == "prompt is required for xAI video generation."
    ));

    input.prompt = " City lights ".into();
    input.image_url = Some(" \t ".into());
    input.aspect_ratio = " 3:2 ".into();
    input.resolution = " 480p ".into();
    let text = build_xai_payload(&input).unwrap();
    assert_eq!(text.modality, "text");
    assert_eq!(
        Value::Object(text.payload),
        json!({
            "model": "grok-imagine-video",
            "prompt": "City lights",
            "aspect_ratio": "3:2",
            "resolution": "480p",
            "duration": 8
        })
    );

    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("frame.GIF");
    std::fs::write(&file, b"GIF89a").unwrap();
    input.model = Some(" custom-model ".into());
    input.image_url = Some(file.to_str().unwrap().into());
    let image = build_xai_payload(&input).unwrap();
    assert_eq!(image.model, "custom-model");
    assert_eq!(image.modality, "image");
    assert_eq!(
        image.payload["image"],
        json!({"url": "data:image/gif;base64,R0lGODlh"})
    );

    input.image_url = None;
    input.model_explicit = false;
    assert_eq!(build_xai_payload(&input).unwrap().model, "custom-model");
}

#[test]
fn local_image_references_preserve_unsupported_or_missing_paths() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.png");
    let unsupported = temp.path().join("frame.txt");
    std::fs::write(&unsupported, b"not an image").unwrap();

    for file in [missing, unsupported] {
        let value = file.to_str().unwrap();
        assert_eq!(image_ref_to_xai_url(&format!(" {value} ")), value);
    }
    assert!(image_ref_to_xai_url(" \t ").is_empty());

    for (filename, mime) in [
        ("frame.GIF", Some("image/gif")),
        ("frame.webp", Some("image/webp")),
        ("frame.bmp", Some("image/bmp")),
        ("frame.avif", Some("image/avif")),
        ("frame.svg", Some("image/svg+xml")),
        ("frame.txt", None),
        ("frame", None),
    ] {
        assert_eq!(image_mime_for_path(Path::new(filename)), mime);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn unreadable_image_reference_falls_back_to_the_path() {
    // This optional procfs entry looks like a regular file but has no read operation.
    let write_only = Path::new("/proc/self/clear_refs");
    if !write_only.is_file() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let image = temp.path().join("unreadable.png");
    std::os::unix::fs::symlink(write_only, &image).unwrap();
    assert!(image.is_file());
    assert!(std::fs::read(&image).is_err());
    assert_eq!(
        image_ref_to_xai_url(image.to_str().unwrap()),
        image.to_str().unwrap()
    );
}

#[test]
fn video_response_extractors_handle_fallbacks_and_empty_fields() {
    for (body, expected) in [
        (
            json!({"error": {"message": "  nested  "}, "message": "outer"}),
            Some("nested"),
        ),
        (json!({"error": "  rejected  "}), Some("rejected")),
        (json!({"error": {}, "message": "  outer  "}), Some("outer")),
        (json!({"error": " \t "}), None),
        (json!({}), None),
    ] {
        assert_eq!(extract_xai_failure_message(&body).as_deref(), expected);
    }

    assert_eq!(
        extract_video(&json!({
            "data": {"video": {"url": "  https://example.invalid/nested.mp4  ", "file_size": 42}},
            "video": "https://example.invalid/outer.mp4"
        })),
        Some(VideoArtifact {
            url: "https://example.invalid/nested.mp4".into(),
            file_size: Some(42),
            content_type: None,
        })
    );
    assert_eq!(
        extract_video(&json!({"data": {"video": {"url": " "}}, "video": "outer"}))
            .unwrap()
            .url,
        "outer"
    );
    assert!(extract_video(&json!({"video": {"url": " \n "}})).is_none());
}

fn loopback_client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0)
        .build()
        .unwrap()
}

fn fal_at(origin: String) -> FalVideoGenBackend {
    FalVideoGenBackend {
        client: loopback_client(),
        transport: FalVideoTransport::Managed {
            gateway_origin: origin,
            nous_token: "test-fal-token".into(),
        },
    }
}

fn xai_at(base_url: String) -> XaiVideoGenBackend {
    XaiVideoGenBackend {
        client: loopback_client(),
        credentials: Some(XaiVideoCredentials {
            api_key: "test-xai-token".into(),
            base_url,
            source: "env".into(),
        }),
    }
}

async fn mock_json(server: &MockServer, verb: &str, route: &str, response: Value) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn managed_fal_reports_video_metadata_and_rejects_missing_video() {
    let server = MockServer::start().await;
    let backend = fal_at(format!("{}/", server.uri()));
    let artifact = json!({"video": {
        "url": "https://example.invalid/clip.mp4",
        "file_size": 42,
        "content_type": "video/mp4"
    }});

    Mock::given(method("POST"))
        .and(path("/run/fal-ai/pixverse/v6/image-to-video"))
        .and(header("authorization", "Bearer test-fal-token"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({
            "prompt": "A train crossing a bridge",
            "image_url": "https://example.invalid/frame.png",
            "duration": "15",
            "resolution": "720p"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(&artifact))
        .expect(1)
        .mount(&server)
        .await;
    let mut image = request("pixverse-v6");
    image.image_url = Some(" https://example.invalid/frame.png ".into());
    image.duration = Some(99);
    let out: Value = serde_json::from_str(&backend.generate_video(image).await.unwrap()).unwrap();
    assert_eq!(
        out,
        json!({
            "success": true,
            "video": "https://example.invalid/clip.mp4",
            "model": "pixverse-v6",
            "prompt": "A train crossing a bridge",
            "modality": "image",
            "aspect_ratio": "",
            "duration": 15,
            "provider": "fal",
            "endpoint": "fal-ai/pixverse/v6/image-to-video",
            "transport": "managed",
            "file_size": 42,
            "content_type": "video/mp4"
        })
    );

    mock_json(
        &server,
        "POST",
        "/run/fal-ai/happy-horse/text-to-video",
        artifact,
    )
    .await;
    let mut text = request("happy-horse");
    text.image_url = Some(" ".into());
    let out: Value = serde_json::from_str(&backend.generate_video(text).await.unwrap()).unwrap();
    assert_eq!(out["modality"], "text");
    assert_eq!(out["duration"], 0);

    mock_json(&server, "POST", "/run/fal-ai/veo3.1", json!({"data": {}})).await;
    assert_eq!(
        execution_error(backend.generate_video(request("veo3.1")).await),
        "FAL returned no video URL in response"
    );
    let error = execution_error(
        fal_at("://invalid".into())
            .submit_managed("test", &json!({}))
            .await,
    );
    assert!(error.starts_with("FAL video request failed:"), "{error}");
}

#[tokio::test]
async fn video_coverage_queue_and_xai_on_loopback() {
    let Some(_home) = isolated_home(
        "video_coverage_queue_and_xai_on_loopback",
        &[
            ("FAL_VIDEO_POLL_INTERVAL_SECONDS", "600"),
            ("XAI_VIDEO_POLL_INTERVAL_SECONDS", "240"),
        ],
    ) else {
        return;
    };
    let server = MockServer::start().await;
    let origin = server.uri();
    // The managed URL builder lets the queue algorithm use loopback instead of queue.fal.run.
    let fal = fal_at(origin.clone());
    let xai = xai_at(origin.clone());
    let credentials = xai.credentials().unwrap();
    assert_eq!(xai.transport_label(), "direct");
    let payload = json!({"prompt": "test"});
    let artifact = json!({"data": {"video": "https://example.invalid/clip.mp4"}});

    mock_json(&server, "POST", "/run/immediate", artifact.clone()).await;
    assert_eq!(
        fal.submit_direct_queue("immediate", &payload)
            .await
            .unwrap(),
        artifact
    );

    for (endpoint, reply, error_prefix) in [
        (
            "missing",
            json!({"status_url": false, "response_url": null, "request_id": 17}),
            "FAL queue response omitted request URL",
        ),
        (
            "bad-status",
            json!({"status_url": "://invalid"}),
            "FAL queue status failed:",
        ),
        (
            "bad-result",
            json!({"response_url": "://invalid"}),
            "FAL queue response failed:",
        ),
    ] {
        mock_json(&server, "POST", &format!("/run/{endpoint}"), reply).await;
        let error = execution_error(fal.submit_direct_queue(endpoint, &payload).await);
        assert!(error.starts_with(error_prefix), "{error}");
    }

    mock_json(
        &server,
        "POST",
        "/run/complete",
        json!({"status_url": format!("{origin}/completed"), "response_url": format!("{origin}/result")}),
    )
    .await;
    mock_json(&server, "GET", "/completed", json!({"status": "COMPLETED"})).await;
    Mock::given(method("GET"))
        .and(path("/result"))
        .and(header("authorization", "Bearer test-fal-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&artifact))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        fal.submit_direct_queue("complete", &payload).await.unwrap(),
        artifact
    );

    let failed = json!({"status": "ERROR", "detail": "render rejected"});
    mock_json(&server, "GET", "/failed", failed.clone()).await;
    mock_json(
        &server,
        "POST",
        "/run/failure",
        json!({"status_url": format!("{origin}/failed")}),
    )
    .await;
    assert_eq!(
        execution_error(fal.submit_direct_queue("failure", &payload).await),
        format!("FAL video generation failed: {failed}")
    );
    let error = execution_error(
        fal_at("://invalid".into())
            .submit_direct_queue("test", &payload)
            .await,
    );
    assert!(error.starts_with("FAL queue submit failed:"), "{error}");

    Mock::given(method("POST"))
        .and(path("/videos/generations"))
        .and(header("authorization", "Bearer test-xai-token"))
        .and(header("user-agent", "hermes-agent/video_gen"))
        .and(header("content-type", "application/json"))
        .and(wiremock::matchers::header_exists("x-idempotency-key"))
        .and(body_json(payload.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "  alternate-id  "})))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        xai.submit_xai_generation(credentials, &payload)
            .await
            .unwrap(),
        "alternate-id"
    );

    mock_json(
        &server,
        "POST",
        "/invalid-id/videos/generations",
        json!({"request_id": " \t "}),
    )
    .await;
    let invalid_id = xai_at(format!("{origin}/invalid-id"));
    assert_eq!(
        execution_error(
            invalid_id
                .submit_xai_generation(invalid_id.credentials().unwrap(), &payload)
                .await
        ),
        "xAI video response did not include request_id"
    );

    let invalid_url = xai_at("://invalid".into());
    let invalid_credentials = invalid_url.credentials().unwrap();
    let error = execution_error(
        invalid_url
            .submit_xai_generation(invalid_credentials, &payload)
            .await,
    );
    assert!(error.starts_with("xAI video submit failed:"), "{error}");
    let error = execution_error(
        invalid_url
            .poll_xai_generation(invalid_credentials, "id")
            .await,
    );
    assert!(
        error.starts_with("xAI video status request failed:"),
        "{error}"
    );

    for (id, body, expected) in [
        (
            "expired",
            json!({"status": "EXPIRED"}),
            "xAI video generation failed: expired",
        ),
        (
            "cancelled",
            json!({"status": "Cancelled", "error": {"message": "  provider cancelled  "}}),
            "provider cancelled",
        ),
    ] {
        mock_json(&server, "GET", &format!("/videos/{id}"), body).await;
        assert_eq!(
            execution_error(xai.poll_xai_generation(credentials, id).await),
            expected
        );
    }

    mock_json(
        &server,
        "POST",
        "/missing-video/videos/generations",
        json!({"request_id": " job "}),
    )
    .await;
    mock_json(
        &server,
        "GET",
        "/missing-video/videos/job",
        json!({"status": "SuCcEeDeD"}),
    )
    .await;
    assert_eq!(
        execution_error(
            xai_at(format!("{origin}/missing-video"))
                .generate_video(request("grok-imagine-video"))
                .await
        ),
        "xAI returned no video URL in response"
    );

    mock_json(
        &server,
        "POST",
        "/run/timeout",
        json!({"status_url": format!("{origin}/pending")}),
    )
    .await;
    mock_json(&server, "GET", "/pending", json!({"status": "IN_PROGRESS"})).await;
    mock_json(
        &server,
        "GET",
        "/videos/pending",
        json!({"status": "Processing"}),
    )
    .await;

    // Poll intervals equal their deadlines, so each pending reply leads to one
    // virtual timer before the next iteration reports a timeout.
    tokio::time::pause();
    assert_eq!(
        execution_error(fal.submit_direct_queue("timeout", &payload).await),
        "Timed out waiting for FAL video generation after 600s"
    );
    assert_eq!(
        execution_error(xai.poll_xai_generation(credentials, "pending").await),
        "Timed out waiting for xAI video generation after 240s; last status: processing"
    );
    tokio::time::resume();
}

#[tokio::test]
async fn json_response_preserves_http_errors_and_labels_parse_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rejected"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limit"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/malformed"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
        .expect(1)
        .mount(&server)
        .await;

    let client = loopback_client();
    for (route, expected) in [
        (
            "/rejected",
            "test video error (429 Too Many Requests): rate limit",
        ),
        ("/malformed", "Failed to parse test video response:"),
    ] {
        let response = client
            .get(format!("{}{route}", server.uri()))
            .send()
            .await
            .unwrap();
        let error = execution_error(read_json_response(response, "test video").await);
        if route == "/rejected" {
            assert_eq!(error, expected);
        } else {
            assert!(error.starts_with(expected), "{error}");
        }
    }
}

#[tokio::test]
async fn json_response_reports_a_truncated_body() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = async {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(socket.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
        }
        socket
            .get_mut()
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
        socket.get_mut().shutdown().await.unwrap();
    };
    let fetch = async {
        let response = loopback_client()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        execution_error(read_json_response(response, "truncated video").await)
    };
    let ((), error) = tokio::join!(serve, fetch);
    assert!(
        error.starts_with("Failed to read truncated video response:"),
        "{error}"
    );
}
