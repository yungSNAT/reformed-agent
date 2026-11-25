use super::*;
use std::time::Duration;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn configured_speech_routes_preserve_provider_response_formats() {
    const CHILD: &str = "HERMES_TEST_VOICE_TRANSPORT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        for (provider, expected) in [
            (SttProvider::GroqWhisper, "Park survey"),
            (SttProvider::MistralVoxtral, "Survey notes"),
        ] {
            let voice = VoiceManager::new(VoiceConfig {
                stt_provider: provider,
                language: Some("en".into()),
                ..VoiceConfig::default()
            });
            assert_eq!(
                voice.transcribe(b"fixture audio", "wav").await.unwrap(),
                expected
            );
        }
        let voice = VoiceManager::new(VoiceConfig::default());
        let error = voice.transcribe(b"fixture audio", "wav").await.unwrap_err();
        assert!(
            matches!(error, AgentError::LlmApi(message) if message == "OpenAI Whisper error: fixture unavailable")
        );
        return;
    }

    let server = MockServer::start().await;
    assert!(server.address().ip().is_loopback());
    Mock::given(method("POST"))
        .and(path("/groq/v1/audio/transcriptions"))
        .and(body_string_contains("name=\"response_format\"\r\n\r\ntext"))
        .and(body_string_contains("name=\"language\"\r\n\r\nen"))
        .and(body_string_contains("whisper-large-v3"))
        .respond_with(ResponseTemplate::new(200).set_body_string(" Park survey \n"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/mistral/v1/audio/transcriptions"))
        .and(body_string_contains("voxtral-mini-latest"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "Survey notes"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/whisper/v1/audio/transcriptions"))
        .and(body_string_contains("name=\"response_format\"\r\n\r\njson"))
        .and(body_string_contains("gpt-4o-mini-transcribe"))
        .respond_with(ResponseTemplate::new(503).set_body_string("fixture unavailable"))
        .expect(1)
        .mount(&server)
        .await;

    let home = tempfile::tempdir().unwrap();
    let cwd = std::env::current_dir().unwrap();
    let executable = cwd.join(std::env::args_os().next().expect("test executable"));
    let mut child = tokio::process::Command::new(executable);
    let fixture_value = ["voice", "fixture", "placeholder"].join("-");
    let endpoint_root = server.uri();
    child
        .env_clear()
        .env(CHILD, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("HERMES_HOME", home.path())
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "127.0.0.1,::1")
        .env("GROQ_API_KEY", &fixture_value)
        .env("MISTRAL_API_KEY", &fixture_value)
        .env("VOICE_TOOLS_OPENAI_KEY", &fixture_value)
        .env("STT_GROQ_MODEL", "whisper-large-v3")
        .env("STT_MISTRAL_MODEL", "voxtral-mini-latest")
        .env("STT_OPENAI_MODEL", "gpt-4o-mini-transcribe")
        .env("STT_GROQ_BASE_URL", format!("{endpoint_root}/groq/v1/"))
        .env(
            "STT_MISTRAL_BASE_URL",
            format!("{endpoint_root}/mistral/v1/audio/transcriptions"),
        )
        .env("STT_OPENAI_BASE_URL", format!("{endpoint_root}/whisper/v1"))
        .current_dir(home.path())
        .kill_on_drop(true)
        .args([
            "--exact",
            "voice::transport_tests::configured_speech_routes_preserve_provider_response_formats",
            "--nocapture",
        ]);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        child.env("LLVM_PROFILE_FILE", cwd.join(profile));
    }
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        child.env("SystemRoot", system_root);
    }
    let output = tokio::time::timeout(Duration::from_secs(15), child.output())
        .await
        .expect("voice transport child timed out")
        .expect("start voice transport child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let (authorization, expected) = ("authorization", format!("Bearer {fixture_value}"));
    for request in &requests {
        assert_eq!(request.headers[authorization].to_str().unwrap(), expected);
    }
    let mistral = requests
        .iter()
        .find(|request| request.url.path().starts_with("/mistral/"))
        .unwrap();
    assert!(!String::from_utf8_lossy(&mistral.body).contains("name=\"response_format\""));
}
