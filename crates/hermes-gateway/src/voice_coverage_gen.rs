use super::*;
use std::sync::{Mutex as StdMutex, MutexGuard, OnceLock};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static GEN_ENV_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

struct ScopedEnv {
    saved: Vec<(&'static str, Option<String>)>,
    _g: MutexGuard<'static, ()>,
}

impl ScopedEnv {
    fn new(keys: &[&'static str]) -> Self {
        let g = GEN_ENV_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        for key in keys {
            std::env::remove_var(key);
        }
        Self { saved, _g: g }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

fn poison<T>(m: &std::sync::Mutex<T>) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _g = m.lock().unwrap();
        panic!("intentional poison for coverage test");
    }));
    assert!(r.is_err());
    assert!(m.is_poisoned());
}

fn full_duplex() -> VoiceConfig {
    VoiceConfig {
        state: VoiceState::FullDuplex,
        ..Default::default()
    }
}

#[test]
fn response_format_depends_on_provider_and_model() {
    if run_isolated("response_format_depends_on_provider_and_model") {
        return;
    }

    assert_eq!(
        gateway_transcription_response_format("groq", "whisper-1"),
        "text"
    );
    assert_eq!(
        gateway_transcription_response_format("mistral", "whisper-1"),
        "json"
    );
    assert_eq!(
        gateway_transcription_response_format("openai", "whisper-1"),
        "text"
    );
    assert_eq!(
        gateway_transcription_response_format("openai", "WHISPER-1"),
        "text"
    );
    assert_eq!(
        gateway_transcription_response_format("openai", "gpt-4o-transcribe"),
        "json"
    );
}

#[test]
fn normalize_model_covers_mistral_openai_and_groq_defaults() {
    if run_isolated("normalize_model_covers_mistral_openai_and_groq_defaults") {
        return;
    }

    assert_eq!(
        normalize_gateway_stt_model("mistral", Some("voxtral-custom")),
        "voxtral-custom"
    );
    assert_eq!(
        normalize_gateway_stt_model("mistral", None),
        env_or_default(&["STT_MISTRAL_MODEL"], "voxtral-mini-latest")
    );
    assert_eq!(
        normalize_gateway_stt_model("openai", Some("whisper-large-v3")),
        env_or_default(&["STT_OPENAI_MODEL"], "whisper-1")
    );
    assert_eq!(
        normalize_gateway_stt_model("openai", Some("voxtral-mini-latest")),
        env_or_default(&["STT_OPENAI_MODEL"], "whisper-1")
    );
    assert_eq!(
        normalize_gateway_stt_model("groq", Some("my-groq-model")),
        "my-groq-model"
    );
    let groq_default = normalize_gateway_stt_model("groq", None);
    assert!(!groq_default.is_empty());
    assert_eq!(
        groq_default,
        normalize_gateway_stt_model("groq", Some("gpt-4o-transcribe"))
    );
}

#[test]
fn configured_model_for_mistral_and_openai_falls_back_to_defaults() {
    if run_isolated("configured_model_for_mistral_and_openai_falls_back_to_defaults") {
        return;
    }

    assert_eq!(
        configured_gateway_stt_model("mistral"),
        normalize_gateway_stt_model("mistral", None)
    );
    assert_eq!(
        configured_gateway_stt_model("openai"),
        normalize_gateway_stt_model("openai", None)
    );
}

#[test]
fn toggle_from_listen_only_disables() {
    if run_isolated("toggle_from_listen_only_disables") {
        return;
    }

    let mut mgr = VoiceManager::new(VoiceConfig {
        state: VoiceState::ListenOnly,
        ..Default::default()
    });
    assert!(mgr.is_enabled());
    assert_eq!(mgr.toggle(), VoiceState::Disabled);
    assert!(!mgr.is_enabled());
}

#[test]
fn normalize_identifier_rejects_empty_and_too_long() {
    if run_isolated("normalize_identifier_rejects_empty_and_too_long") {
        return;
    }

    let err = VoiceManager::normalize_identifier("   ", "platform").unwrap_err();
    assert!(err.to_string().contains("Voice platform must be non-empty"));
    let long = "x".repeat(300);
    let err = VoiceManager::normalize_identifier(&long, "channel_id").unwrap_err();
    assert!(err
        .to_string()
        .contains("Voice channel_id exceeds maximum length"));
    assert_eq!(
        VoiceManager::normalize_identifier("  ok ", "platform").unwrap(),
        "ok"
    );
}

#[test]
fn mixer_queries_without_active_mixer() {
    if run_isolated("mixer_queries_without_active_mixer") {
        return;
    }

    let mgr = VoiceManager::new(VoiceConfig::default());
    let err = mgr.read_voice_mixer_frame("discord", "nope").unwrap_err();
    assert!(err
        .to_string()
        .contains("Voice mixer is not active for 'discord' channel 'nope'"));
    assert!(!mgr.stop_voice_speech("discord", "nope"));
    assert!(!mgr.voice_speech_active("discord", "nope"));
    let err = mgr.play_voice_speech_pcm("", "nope", &[0, 0]).unwrap_err();
    assert!(err.to_string().contains("must be non-empty"));
}

#[test]
fn vad_short_and_odd_length_inputs() {
    if run_isolated("vad_short_and_odd_length_inputs") {
        return;
    }

    let mgr = VoiceManager::new(VoiceConfig::default());
    assert!(!mgr.detect_voice_activity(&[0x40; 100]));
    assert!(mgr.detect_voice_activity(&[0x40; 321]));
    assert!(!mgr.detect_voice_activity(&[0u8; 321]));
}

#[tokio::test]
async fn leave_reports_unknown_platform_and_channel() {
    if run_isolated("leave_reports_unknown_platform_and_channel") {
        return;
    }

    let mgr = VoiceManager::new(full_duplex());
    let err = mgr.leave_voice_channel("discord", "x").await.unwrap_err();
    assert!(err
        .to_string()
        .contains("Voice channel is not joined for platform 'discord'"));

    mgr.join_voice_channel("discord", "a").await.unwrap();
    let err = mgr.leave_voice_channel("discord", "b").await.unwrap_err();
    assert!(err
        .to_string()
        .contains("Voice channel 'b' is not currently joined on 'discord'"));
    assert!(mgr.is_joined("discord", "a"));
}

#[tokio::test]
async fn ack_phrase_none_when_all_phrases_blank() {
    if run_isolated("ack_phrase_none_when_all_phrases_blank") {
        return;
    }

    let mut config = full_duplex();
    config.voice_fx.enabled = true;
    config.voice_fx.ambient_enabled = false;
    config.voice_fx.ack_phrases = vec![" ".to_string(), String::new()];
    let mgr = VoiceManager::new(config);
    mgr.join_voice_channel("discord", "v").await.unwrap();
    assert!(mgr.voice_mixer_active("discord", "v"));
    assert!(mgr.voice_ack_phrase("discord", "v").is_none());
}

#[tokio::test]
async fn poisoned_cursor_returns_first_phrase() {
    if run_isolated("poisoned_cursor_returns_first_phrase") {
        return;
    }

    let mut config = full_duplex();
    config.voice_fx.enabled = true;
    config.voice_fx.ambient_enabled = false;
    config.voice_fx.ack_phrases = vec!["First.".to_string(), "Second.".to_string()];
    let mgr = VoiceManager::new(config);
    mgr.join_voice_channel("discord", "v").await.unwrap();
    poison(&mgr.ack_phrase_cursor);
    assert_eq!(
        mgr.voice_ack_phrase("discord", "v").as_deref(),
        Some("First.")
    );
    assert_eq!(
        mgr.voice_ack_phrase("discord", "v").as_deref(),
        Some("First.")
    );
}

#[tokio::test]
async fn poisoned_mixer_lock_is_handled_gracefully() {
    if run_isolated("poisoned_mixer_lock_is_handled_gracefully") {
        return;
    }

    let mgr = VoiceManager::new(full_duplex());
    mgr.join_voice_channel("discord", "a").await.unwrap();
    mgr.join_voice_channel("discord", "b").await.unwrap();
    poison(&mgr.voice_mixers);
    assert!(!mgr.voice_mixer_active("discord", "a"));
    assert!(!mgr.voice_speech_active("discord", "a"));
    assert!(!mgr.stop_voice_speech("discord", "a"));
    mgr.leave_voice_channel("discord", "a")
        .await
        .expect("leave succeeds despite poisoned mixer lock");
    assert!(!mgr.is_joined("discord", "a"));
    assert_eq!(mgr.leave_all_channels(), 1);
    assert_eq!(mgr.joined_channel_count(), 0);
}

#[tokio::test]
async fn poisoned_join_lock_is_handled_gracefully() {
    if run_isolated("poisoned_join_lock_is_handled_gracefully") {
        return;
    }

    let mgr = VoiceManager::new(full_duplex());
    mgr.join_voice_channel("discord", "a").await.unwrap();
    poison(&mgr.joined_channels);
    assert!(!mgr.is_joined("discord", "a"));
    assert_eq!(mgr.joined_channel_count(), 0);
    assert_eq!(mgr.leave_all_channels(), 0);
    let err = mgr.join_voice_channel("discord", "b").await.unwrap_err();
    assert!(err.to_string().contains("voice join lock poisoned"));
}

#[tokio::test]
async fn voice_input_empty_and_vad_silence_return_empty() {
    if run_isolated("voice_input_empty_and_vad_silence_return_empty") {
        return;
    }

    let mgr = VoiceManager::new(VoiceConfig {
        auto_detect_voice: true,
        stt_provider: SttProvider::Custom("http://127.0.0.1:9/unused".to_string()),
        ..Default::default()
    });
    assert_eq!(mgr.handle_voice_input(&[], "wav").await.unwrap(), "");
    assert_eq!(
        mgr.handle_voice_input(&[0u8; 400], "wav").await.unwrap(),
        ""
    );
}

include!("voice_part2_coverage_gen.rs");
