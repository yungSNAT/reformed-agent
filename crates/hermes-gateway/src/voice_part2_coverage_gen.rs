#[tokio::test]
async fn voice_output_blank_text_returns_empty() {
    if run_isolated("voice_output_blank_text_returns_empty") {
        return;
    }

    let mgr = VoiceManager::new(VoiceConfig::default());
    assert!(mgr
        .handle_voice_output("   ", None)
        .await
        .unwrap()
        .is_empty());
}

#[test]
fn deepgram_validates_key_format_and_model_before_request() {
    if run_isolated("deepgram_validates_key_format_and_model_before_request") {
        return;
    }

    let _env = ScopedEnv::new(&["DEEPGRAM_API_KEY", "DEEPGRAM_MODEL"]);
    let mgr = VoiceManager::new(VoiceConfig {
        stt_provider: SttProvider::DeepgramNova,
        ..Default::default()
    });
    let err = block_on(mgr.transcribe(&[1, 2, 3], "wav")).unwrap_err();
    assert!(err.to_string().contains("DEEPGRAM_API_KEY not set"));

    std::env::set_var("DEEPGRAM_API_KEY", "dg-key");
    let err = block_on(mgr.transcribe(&[1, 2, 3], "aiff")).unwrap_err();
    assert!(err
        .to_string()
        .contains("Unsupported audio format for Deepgram: 'aiff'"));

    std::env::set_var("DEEPGRAM_MODEL", "nova 2!");
    for fmt in ["WAV", "mp3", "ogg", "oga", "webm", "flac"] {
        let err = block_on(mgr.transcribe(&[1, 2, 3], fmt)).unwrap_err();
        assert!(
            err.to_string()
                .contains("DEEPGRAM_MODEL must be alphanumeric"),
            "{fmt}"
        );
    }
}

#[test]
fn elevenlabs_validates_key_voice_and_model_before_request() {
    if run_isolated("elevenlabs_validates_key_voice_and_model_before_request") {
        return;
    }

    let _env = ScopedEnv::new(&[
        "ELEVENLABS_API_KEY",
        "ELEVENLABS_VOICE_ID",
        "ELEVENLABS_MODEL_ID",
    ]);
    let mgr = VoiceManager::new(VoiceConfig {
        tts_provider: TtsProvider::ElevenLabs,
        ..Default::default()
    });
    let err = block_on(mgr.synthesize("hello")).unwrap_err();
    assert!(err.to_string().contains("ELEVENLABS_API_KEY not set"));

    std::env::set_var("ELEVENLABS_API_KEY", "el-key");
    std::env::set_var("ELEVENLABS_VOICE_ID", "bad/voice");
    let err = block_on(mgr.synthesize("hello")).unwrap_err();
    assert!(err
        .to_string()
        .contains("ELEVENLABS_VOICE_ID must be alphanumeric"));

    std::env::remove_var("ELEVENLABS_VOICE_ID");
    std::env::set_var("ELEVENLABS_MODEL_ID", "bad model");
    let err = block_on(mgr.synthesize("hello")).unwrap_err();
    assert!(err
        .to_string()
        .contains("ELEVENLABS_MODEL_ID must be alphanumeric"));

    let default_mgr = VoiceManager::new(VoiceConfig::default());
    let err = block_on(default_mgr.handle_voice_output("hello", Some(&TtsProvider::ElevenLabs)))
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("ELEVENLABS_MODEL_ID must be alphanumeric"));
}

#[tokio::test]
async fn multipart_text_response_is_trimmed_and_sends_language() {
    if run_isolated("multipart_text_response_is_trimmed_and_sends_language") {
        return;
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .and(header("Authorization", "Bearer secret"))
        .and(body_string_contains("name=\"language\""))
        .and(body_string_contains("name=\"response_format\""))
        .respond_with(ResponseTemplate::new(200).set_body_string("  hello world \n"))
        .mount(&server)
        .await;
    let mgr = VoiceManager::new(VoiceConfig {
        language: Some("en".to_string()),
        ..Default::default()
    });
    let endpoint = format!("{}/audio/transcriptions", server.uri());
    let text = mgr
        .transcribe_multipart_endpoint(
            "Test STT",
            &endpoint,
            "secret",
            &[1, 2, 3, 4],
            "wav",
            "whisper-1",
            "text",
            true,
        )
        .await
        .unwrap();
    assert_eq!(text, "hello world");
}

#[tokio::test]
async fn multipart_json_response_and_errors() {
    if run_isolated("multipart_json_response_and_errors") {
        return;
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ok/audio/transcriptions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "json text"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/notext/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"other": 1})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/bad/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/fail/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let mgr = VoiceManager::new(VoiceConfig::default());
    let call = |p: &'static str| {
        let endpoint = format!("{}/{}/audio/transcriptions", server.uri(), p);
        let mgr = &mgr;
        async move {
            mgr.transcribe_multipart_endpoint(
                "Test STT",
                &endpoint,
                "k",
                &[1, 2, 3, 4],
                "ogg",
                "voxtral-mini-latest",
                "json",
                false,
            )
            .await
        }
    };

    assert_eq!(call("ok").await.unwrap(), "json text");
    assert_eq!(call("notext").await.unwrap(), "");
    let err = call("bad").await.unwrap_err();
    assert!(err.to_string().contains("Test STT parse error"));
    let err = call("fail").await.unwrap_err();
    assert!(err.to_string().contains("Test STT error: boom"));
}

#[tokio::test]
async fn custom_stt_handles_json_plain_and_error_responses() {
    if run_isolated("custom_stt_handles_json_plain_and_error_responses") {
        return;
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/json-text"))
        .and(header("content-type", "audio/wav"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "from text"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/json-transcript"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"transcript": "from transcript"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string("plain words"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/fail"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .mount(&server)
        .await;

    let mgr_for = |p: &str| {
        VoiceManager::new(VoiceConfig {
            stt_provider: SttProvider::Custom(format!("{}/{}", server.uri(), p)),
            ..Default::default()
        })
    };

    assert_eq!(
        mgr_for("json-text")
            .handle_voice_input(&[1, 2, 3], "wav")
            .await
            .unwrap(),
        "from text"
    );
    assert_eq!(
        mgr_for("json-transcript")
            .transcribe(&[1, 2, 3], "wav")
            .await
            .unwrap(),
        "from transcript"
    );
    assert_eq!(
        mgr_for("plain")
            .transcribe(&[1, 2, 3], "wav")
            .await
            .unwrap(),
        "plain words"
    );
    let err = mgr_for("fail")
        .transcribe(&[1, 2, 3], "wav")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Custom STT error: down"));
}

#[tokio::test]
async fn custom_tts_returns_bytes_and_reports_errors() {
    if run_isolated("custom_tts_returns_bytes_and_reports_errors") {
        return;
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/tts"))
        .and(body_string_contains("\"input\""))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![9u8, 8, 7]))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/tts-fail"))
        .respond_with(ResponseTemplate::new(500).set_body_string("nope"))
        .mount(&server)
        .await;

    let ok_url = format!("{}/tts", server.uri());
    let mgr = VoiceManager::new(VoiceConfig {
        tts_provider: TtsProvider::Custom(ok_url.clone()),
        ..Default::default()
    });
    assert_eq!(mgr.synthesize("hi there").await.unwrap(), vec![9u8, 8, 7]);

    let default_mgr = VoiceManager::new(VoiceConfig::default());
    let custom = TtsProvider::Custom(ok_url);
    assert_eq!(
        default_mgr
            .handle_voice_output("hi there", Some(&custom))
            .await
            .unwrap(),
        vec![9u8, 8, 7]
    );

    let fail_mgr = VoiceManager::new(VoiceConfig {
        tts_provider: TtsProvider::Custom(format!("{}/tts-fail", server.uri())),
        ..Default::default()
    });
    let err = fail_mgr.handle_voice_output("hi", None).await.unwrap_err();
    assert!(err.to_string().contains("Custom TTS error: nope"));
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path
        .split_once("::")
        .map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary")) // nosemgrep: rust.lang.security.current-exe.current-exe
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
        .output()
        .expect("run the isolated test");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}
