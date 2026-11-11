//! Text-to-speech tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::credential_guard::CredentialGuard;

// ---------------------------------------------------------------------------
// TtsBackend trait
// ---------------------------------------------------------------------------

/// Backend for text-to-speech operations.
#[async_trait]
pub trait TtsBackend: Send + Sync {
    /// Convert text to speech.
    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        provider: Option<&str>,
        output_path: Option<&str>,
        speed: Option<f64>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// TextToSpeechHandler
// ---------------------------------------------------------------------------

/// Tool for converting text to speech using various TTS providers.
pub struct TextToSpeechHandler {
    backend: Arc<dyn TtsBackend>,
}

impl TextToSpeechHandler {
    pub fn new(backend: Arc<dyn TtsBackend>) -> Self {
        Self { backend }
    }
}

/// Refuse an `output_path` that the file tools would not write to.
///
/// The backends write the path as given, relative to the working directory, so
/// the guard is shown that absolute form. The guard compares paths as text: a
/// path containing `..` is refused outright, and the rest is rebuilt from its
/// components so that `//etc/passwd` and `/etc/./passwd` read as `/etc/passwd`.
/// Links are not resolved, which is the guard's limit for the file tools too.
fn check_output_path(raw: &str) -> Result<(), ToolError> {
    let path = Path::new(raw);
    if path.components().any(|part| part == Component::ParentDir) {
        return Err(ToolError::InvalidParams(format!(
            "output_path must not contain '..': {raw}"
        )));
    }
    let absolute: PathBuf = std::path::absolute(path)
        .map_err(|e| ToolError::InvalidParams(format!("Invalid output_path '{raw}': {e}")))?
        .components()
        .collect();
    CredentialGuard::path_only().check_write_access(&absolute, "")
}

#[async_trait]
impl ToolHandler for TextToSpeechHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'text' parameter".into()))?;

        let voice = params.get("voice").and_then(|v| v.as_str());
        let provider = params.get("provider").and_then(|v| v.as_str());
        let output_path = params.get("output_path").and_then(|v| v.as_str());
        if let Some(path) = output_path {
            check_output_path(path)?;
        }
        let speed = params.get("speed").and_then(|v| v.as_f64());

        self.backend
            .synthesize(text, voice, provider, output_path, speed)
            .await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "text".into(),
            json!({
                "type": "string",
                "description": "Text to convert to speech"
            }),
        );
        props.insert(
            "voice".into(),
            json!({
                "type": "string",
                "description": "Voice to use (provider-specific)"
            }),
        );
        props.insert(
            "provider".into(),
            json!({
                "type": "string",
                "description": "TTS provider to use",
                "enum": ["elevenlabs", "openai", "minimax", "piper"],
                "default": "openai"
            }),
        );
        props.insert(
            "output_path".into(),
            json!({
                "type": "string",
                "description": "Optional output file path. Defaults to a temporary audio file."
            }),
        );
        props.insert(
            "speed".into(),
            json!({
                "type": "number",
                "description": "Optional provider-specific speech speed multiplier."
            }),
        );

        tool_schema(
            "text_to_speech",
            "Convert text to speech audio using ElevenLabs, OpenAI, MiniMax, or local Piper. HTTP providers use direct API calls; Piper uses a local binary (no Python runtime required).",
            JsonSchema::object(props, vec!["text".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTtsBackend;
    #[async_trait]
    impl TtsBackend for MockTtsBackend {
        async fn synthesize(
            &self,
            text: &str,
            _voice: Option<&str>,
            _provider: Option<&str>,
            _output_path: Option<&str>,
            _speed: Option<f64>,
        ) -> Result<String, ToolError> {
            Ok(format!("Audio for: {}", text))
        }
    }

    #[tokio::test]
    async fn test_tts_schema() {
        let handler = TextToSpeechHandler::new(Arc::new(MockTtsBackend));
        assert_eq!(handler.schema().name, "text_to_speech");
    }

    #[tokio::test]
    async fn test_tts_execute() {
        let handler = TextToSpeechHandler::new(Arc::new(MockTtsBackend));
        let result = handler
            .execute(json!({"text": "Hello world"}))
            .await
            .unwrap();
        assert!(result.contains("Hello world"));
    }

    /// Records whether synthesis ran; writes nothing.
    #[derive(Default)]
    struct RecordingTtsBackend {
        called: std::sync::atomic::AtomicBool,
    }
    #[async_trait]
    impl TtsBackend for RecordingTtsBackend {
        async fn synthesize(
            &self,
            _text: &str,
            _voice: Option<&str>,
            _provider: Option<&str>,
            _output_path: Option<&str>,
            _speed: Option<f64>,
        ) -> Result<String, ToolError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("synthesized".into())
        }
    }

    /// Runs the tool with `output_path`; returns its result and whether synthesis ran.
    async fn run_with_output_path(output_path: &str) -> (Result<String, ToolError>, bool) {
        let backend = Arc::new(RecordingTtsBackend::default());
        let handler = TextToSpeechHandler::new(backend.clone());
        let result = handler
            .execute(json!({"text": "hello", "output_path": output_path}))
            .await;
        let called = backend.called.load(std::sync::atomic::Ordering::SeqCst);
        (result, called)
    }

    #[tokio::test]
    async fn output_path_on_a_protected_path_is_refused_before_synthesis() {
        let temp = std::env::temp_dir();
        let protected = [
            temp.join(".ssh").join("speech.mp3"),
            temp.join(".aws").join("credentials"),
            temp.join(".env"),
            // Relative paths are judged where the backend would write them.
            PathBuf::from(".env"),
        ];
        for path in protected {
            let (result, called) = run_with_output_path(&path.to_string_lossy()).await;
            assert!(
                matches!(&result, Err(ToolError::ExecutionFailed(message)) if message.contains("Write denied")),
                "{} was not refused: {result:?}",
                path.display()
            );
            assert!(!called, "synthesis ran for {}", path.display());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_path_on_a_system_path_is_refused_before_synthesis() {
        for path in [
            "/etc/passwd",
            "/etc/sudoers.d/speech.mp3",
            "//etc/passwd",
            "/etc/./passwd",
        ] {
            let (result, called) = run_with_output_path(path).await;
            assert!(
                matches!(&result, Err(ToolError::ExecutionFailed(message)) if message.contains("Write denied")),
                "{path} was not refused: {result:?}"
            );
            assert!(!called, "synthesis ran for {path}");
        }
    }

    #[tokio::test]
    async fn output_path_with_a_parent_component_is_refused_before_synthesis() {
        let (result, called) = run_with_output_path("audio/../../etc/passwd").await;
        assert!(
            matches!(&result, Err(ToolError::InvalidParams(message)) if message.contains("'..'")),
            "{result:?}"
        );
        assert!(!called);
    }

    #[tokio::test]
    async fn an_ordinary_output_path_reaches_the_backend() {
        let path = std::env::temp_dir()
            .join("hermes-tts-test")
            .join("speech.mp3");
        let (result, called) = run_with_output_path(&path.to_string_lossy()).await;
        assert_eq!(result.unwrap(), "synthesized");
        assert!(called);
    }
}
