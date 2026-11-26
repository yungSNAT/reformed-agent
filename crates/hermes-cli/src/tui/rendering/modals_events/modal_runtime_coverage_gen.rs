use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

#[test]
fn ctrl_c_detection() {
    assert!(is_ctrl_c(&key(KeyCode::Char('\u{3}'), KeyModifiers::NONE)));
    assert!(is_ctrl_c(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    assert!(is_ctrl_c(&key(KeyCode::Char('C'), KeyModifiers::CONTROL)));
    assert!(!is_ctrl_c(&key(KeyCode::Char('c'), KeyModifiers::NONE)));
    assert!(!is_ctrl_c(&key(KeyCode::Char('x'), KeyModifiers::CONTROL)));
}

#[test]
fn submit_shortcut_variants() {
    assert!(is_submit_shortcut(
        &key(KeyCode::Char('\r'), KeyModifiers::NONE),
        ""
    ));
    assert!(is_submit_shortcut(
        &key(KeyCode::Char('\n'), KeyModifiers::NONE),
        ""
    ));
    assert!(is_submit_shortcut(
        &key(KeyCode::Enter, KeyModifiers::NONE),
        ""
    ));
    assert!(is_submit_shortcut(
        &key(KeyCode::Enter, KeyModifiers::CONTROL),
        ""
    ));
    assert!(is_submit_shortcut(
        &key(KeyCode::Enter, KeyModifiers::ALT),
        ""
    ));
    assert!(!is_submit_shortcut(
        &key(KeyCode::Enter, KeyModifiers::SHIFT),
        ""
    ));
    // Enter with an unrelated modifier falls through to the Ctrl+M fallback.
    assert!(!is_submit_shortcut(
        &key(KeyCode::Enter, KeyModifiers::SUPER),
        ""
    ));
    assert!(is_submit_shortcut(
        &key(KeyCode::Char('m'), KeyModifiers::CONTROL),
        ""
    ));
    assert!(!is_submit_shortcut(
        &key(KeyCode::Char('m'), KeyModifiers::NONE),
        ""
    ));
}

#[test]
fn slash_parts_parsing() {
    let (cmd, args) = parse_slash_parts("  /model openai gpt-4  ").expect("slash command");
    assert_eq!(cmd, "/model");
    assert_eq!(args, vec!["openai".to_string(), "gpt-4".to_string()]);

    let (cmd, args) = parse_slash_parts("/help").expect("slash command");
    assert_eq!(cmd, "/help");
    assert!(args.is_empty());

    assert!(parse_slash_parts("hello world").is_none());
    assert!(parse_slash_parts("   ").is_none());
}

#[test]
fn strip_markers() {
    assert_eq!(strip_question_option_marker("  - alpha "), "alpha");
    assert_eq!(strip_question_option_marker("* beta"), "beta");
    assert_eq!(strip_question_option_marker("+ gamma"), "gamma");
    assert_eq!(strip_question_option_marker("2. delta"), "delta");
    assert_eq!(strip_question_option_marker("  plain  "), "plain");
}

#[test]
fn question_option_parsing() {
    let item = parse_question_option(" fast :: quick mode ");
    assert_eq!(item.label, "fast");
    assert_eq!(item.detail, "quick mode");
    assert_eq!(item.value, "fast");

    let item = parse_question_option(" safe ");
    assert_eq!(item.label, "safe");
    assert!(item.detail.is_empty());
    assert_eq!(item.value, "safe");
}

#[test]
fn interactive_question_errors() {
    let err = parse_interactive_question_request("/foo bar").unwrap_err();
    assert_eq!(err, "not an interactive question command");

    let err = parse_interactive_question_request("/ask").unwrap_err();
    assert!(err.starts_with("Usage:"));
    let err = parse_interactive_question_request("/ask HELP").unwrap_err();
    assert!(err.starts_with("Usage:"));

    let err = parse_interactive_question_request("/ask |").unwrap_err();
    assert!(err.starts_with("Question prompt is empty"));

    let err = parse_interactive_question_request("/ask Pick | only").unwrap_err();
    assert!(err.starts_with("Provide at least 2 options"));
}

#[test]
fn interactive_question_demo() {
    let req = parse_interactive_question_request("/ask demo").expect("demo");
    assert_eq!(req.prompt, "How should we proceed?");
    assert_eq!(req.options.len(), 3);
    assert_eq!(req.options[0].value, "Continue implementation");
    assert_eq!(req.options[1].label, "Pause for diagnosis");
    assert_eq!(req.options[2].detail, "Try a different runtime profile.");
}

#[test]
fn interactive_question_pipe_and_multiline() {
    let req =
        parse_interactive_question_request("/question Pick mode | safe | fast::quick | - slow")
            .expect("pipe format");
    assert_eq!(req.prompt, "Pick mode");
    let values: Vec<&str> = req.options.iter().map(|o| o.value.as_str()).collect();
    assert_eq!(values, vec!["safe", "fast", "slow"]);
    assert_eq!(req.options[1].detail, "quick");

    let req = parse_interactive_question_request("/ask\nWhich one?\n- first\n* second\n\n+ third")
        .expect("multiline format");
    assert_eq!(req.prompt, "Which one?");
    let values: Vec<&str> = req.options.iter().map(|o| o.value.as_str()).collect();
    assert_eq!(values, vec!["first", "second", "third"]);
}

#[test]
fn interactive_question_truncates_to_twelve() {
    let mut input = String::from("/ask Q");
    for i in 0..15 {
        input.push_str(&format!(" | opt{i}"));
    }
    let req = parse_interactive_question_request(&input).expect("many options");
    assert_eq!(req.options.len(), 12);
    assert_eq!(req.options[11].value, "opt11");
}

fn expected_hints(provider: &str, fallback: &'static [&'static str]) -> &'static [&'static str] {
    let provider = provider.trim().to_ascii_lowercase();
    let run = hermes_core::providers::provider_api_key_env_vars(
        &provider,
        &hermes_core::providers::canonical_provider_id(&provider),
    );
    if run.is_empty() {
        fallback
    } else {
        run
    }
}

#[test]
fn env_key_hints_for_local_backends() {
    let cases: Vec<(&str, &'static [&'static str])> = vec![
        ("ollama-local", &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"]),
        ("llama-cpp", &["LLAMA_CPP_API_KEY"]),
        ("vllm", &["VLLM_API_KEY"]),
        ("mlx", &["MLX_API_KEY"]),
        ("apple-ane", &["APPLE_ANE_API_KEY"]),
        ("sglang", &["SGLANG_API_KEY"]),
        ("tgi", &["TGI_API_KEY"]),
        ("lmstudio", &["LMSTUDIO_API_KEY"]),
        ("lm-studio", &["LMSTUDIO_API_KEY"]),
        ("lmdeploy", &["LMDEPLOY_API_KEY"]),
        ("lm-deploy", &["LMDEPLOY_API_KEY"]),
        ("localai", &["LOCALAI_API_KEY"]),
        ("local-ai", &["LOCALAI_API_KEY"]),
        ("koboldcpp", &["KOBOLDCPP_API_KEY"]),
        ("kobold-cpp", &["KOBOLDCPP_API_KEY"]),
        ("text-generation-webui", &["TEXT_GENERATION_WEBUI_API_KEY"]),
        ("oobabooga", &["TEXT_GENERATION_WEBUI_API_KEY"]),
        ("tabbyapi", &["TABBYAPI_API_KEY"]),
        ("tabby-api", &["TABBYAPI_API_KEY"]),
        ("exllama", &["TABBYAPI_API_KEY"]),
        ("exllamav2", &["TABBYAPI_API_KEY"]),
        ("zz-no-such-provider-xyz", &[]),
    ];
    for (provider, fallback) in cases {
        let hints = provider_env_key_hints(provider);
        assert_eq!(
            hints,
            expected_hints(provider, fallback),
            "provider {provider}"
        );
    }
    // Case and whitespace are normalised.
    assert_eq!(
        provider_env_key_hints("  VLLM "),
        expected_hints("vllm", &["VLLM_API_KEY"])
    );
}

#[test]
fn file_like_hints_extraction() {
    let text = "see src/main.rs, Cargo.toml and src/main.rs again (notes.md) a.rs x.py";
    let hints = extract_file_like_hints(text, 10);
    assert_eq!(
        hints,
        vec![
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
            "notes.md".to_string(),
            "a.rs".to_string(),
            "x.py".to_string(),
        ]
    );

    let limited = extract_file_like_hints(text, 2);
    assert_eq!(
        limited,
        vec!["src/main.rs".to_string(), "Cargo.toml".to_string()]
    );

    let more = extract_file_like_hints("'cfg.json' conf.yaml; app.yml word plain", 10);
    assert_eq!(
        more,
        vec![
            "cfg.json".to_string(),
            "conf.yaml".to_string(),
            "app.yml".to_string(),
        ]
    );

    assert!(extract_file_like_hints("nothing here at all", 5).is_empty());
    assert!(extract_file_like_hints("src/a.rs", 0).is_empty());
}
