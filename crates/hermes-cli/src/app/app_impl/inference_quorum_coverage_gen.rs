use super::*;

fn test_policy(models: &[&str], voters: usize) -> QuorumPolicy {
    QuorumPolicy {
        enabled: true,
        voters,
        models: models.iter().copied().map(str::to_owned).collect(),
        mode: "coverage-mode".to_string(),
        updated_at: "2025-01-01T00:00:00Z".to_string(),
    }
}

#[tokio::test]
async fn quorum_resolves_catalog_aliases_pinned_models_and_fallbacks() {
    let catalog = async |provider: &str| {
        if provider == "lab" {
            vec!["catalog/base".into(), "catalog/amber".into()]
        } else {
            assert_eq!(provider, "unlisted");
            Vec::new()
        }
    };
    let policy = test_policy(
        &[
            "LAB : amber",
            "lab:catalog/amber",
            "lab:ancient-20250101",
            "lab:zzzzzz",
            ":stray",
            "lab:",
            "unlisted:variant",
        ],
        8,
    );
    let (models, notes) = App::resolve_quorum_models("unlisted:variant", &policy, &catalog).await;
    assert_eq!(
        models,
        [
            "lab:catalog/amber",
            "lab:ancient-20250101",
            "lab:catalog/base",
            "unlisted:variant",
        ]
    );
    assert_eq!(
        notes,
        [
            "quorum model remapped via catalog: lab:amber -> lab:catalog/amber",
            "quorum model preserved despite catalog miss: lab:ancient-20250101",
            "quorum model not in provider catalog: lab:zzzzzz ; fallback -> lab:catalog/base ; close matches: (none)",
        ]
    );

    let empty = test_policy(&[" "], 2);
    let (models, notes) = App::resolve_quorum_models("", &empty, &catalog).await;
    assert!(models.is_empty());
    assert!(notes.is_empty());
}

#[test]
fn compose_quorum_messages_merges_nonempty_sections_in_role_order() {
    let mut absent_system = hermes_core::Message::system("unused".to_string());
    absent_system.content = None;
    let messages = App::compose_quorum_messages(
        [" first control ", " \n ", "second control"]
            .map(str::to_owned)
            .to_vec(),
        vec![
            hermes_core::Message::system(" first system ".to_string()),
            hermes_core::Message::user("  original question  ".to_string()),
            absent_system,
            hermes_core::Message::system(" \t ".to_string()),
            hermes_core::Message::system("second system\n".to_string()),
        ],
        Some("  follow-up context  ".to_string()),
    );

    use hermes_core::MessageRole::{System, User};
    let actual: Vec<_> = messages
        .iter()
        .map(|message| (&message.role, message.content.as_deref()))
        .collect();
    assert_eq!(
        actual,
        [
            (&System, Some("first system\n\nsecond system")),
            (
                &User,
                Some("[QUORUM_CONTROL]\nfirst control\n\nsecond control")
            ),
            (&User, Some("  original question  ")),
            (&User, Some("follow-up context")),
        ]
    );
}

#[test]
fn moa_empty_model_is_rejected_but_empty_preset_uses_default() {
    assert_eq!(App::moa_preset_name_for_model(" \t\n "), None);
    let model = format!(" {MOA_PROVIDER} : \t ");
    assert_eq!(
        App::moa_preset_name_for_model(&model).as_deref(),
        Some(MOA_DEFAULT_PRESET)
    );
    assert_eq!(
        App::moa_virtual_model_name(&model),
        Some(format!("{MOA_PROVIDER}:{MOA_DEFAULT_PRESET}"))
    );
}

#[test]
fn collect_quorum_models_filters_blanks_fills_slots_and_truncates() {
    let empty_policy = test_policy(&["", " \t "], 3);
    assert_eq!(
        App::collect_quorum_models(&empty_policy, " openai:current "),
        ["openai:current"]
    );
    assert!(App::collect_quorum_models(&empty_policy, " \t ").is_empty());

    let partial_policy = test_policy(&[" test:first ", "test:first", ""], 3);
    assert_eq!(
        App::collect_quorum_models(&partial_policy, " test:current "),
        ["test:first", "test:current"]
    );
    assert_eq!(
        App::collect_quorum_models(&partial_policy, " test:first "),
        ["test:first"]
    );

    let oversubscribed = test_policy(&["test:first", "test:second", "test:third"], 1);
    assert_eq!(
        App::collect_quorum_models(&oversubscribed, "test:current"),
        ["test:first", "test:second"]
    );
}

#[test]
fn quorum_target_normalization_inherits_provider_for_bare_model() {
    let current = "openai:gpt-4o";
    assert_eq!(
        App::normalize_quorum_model_target(" openai:gpt-4o ", " \n "),
        current
    );
    assert_eq!(
        App::normalize_quorum_model_target(current, " gpt-4o-mini "),
        "openai:gpt-4o-mini"
    );
    assert_eq!(
        App::normalize_quorum_model_target(current, " OpenRouter : Lab/Model "),
        "openrouter:Lab/Model"
    );
    assert_eq!(
        App::split_provider_model("Lab/Model:variant"),
        ("Lab/Model", "variant")
    );
    assert_eq!(App::split_provider_model("Lab/Model"), ("", "Lab/Model"));
}

#[test]
fn quorum_catalog_resolution_prefers_exact_then_namespaced_matches() {
    let catalog = ["lab/quartz", "  QuArTz  ", "vendor/Nova", "nova-preview"].map(str::to_owned);
    assert_eq!(App::resolve_quorum_catalog_candidate("quartz", &[]), None);
    assert_eq!(
        App::resolve_quorum_catalog_candidate(" \t ", &catalog).as_deref(),
        Some("lab/quartz")
    );
    assert_eq!(
        App::resolve_quorum_catalog_candidate(" QUARTZ ", &catalog).as_deref(),
        Some("  QuArTz  ")
    );
    assert_eq!(
        App::resolve_quorum_catalog_candidate("nOvA", &catalog).as_deref(),
        Some("vendor/Nova")
    );
    assert_eq!(
        App::resolve_quorum_catalog_candidate("nova-20250130", &catalog),
        None
    );
}

#[test]
fn quorum_catalog_ranking_rewards_exact_and_tail_matches() {
    let exact_catalog = ["vendor/atlas", " AtLaS "].map(str::to_owned);
    assert_eq!(
        App::rank_catalog_candidates("atlas", &exact_catalog, 2),
        ["AtLaS", "vendor/atlas"]
    );

    let suffix_match = "zzzzzzzzzzzzzzzzzzzzzzzz/atlas";
    let catalog = ["vendor/atlas-preview", "atlas", suffix_match, " \t ", "zzz"].map(str::to_owned);
    assert_eq!(
        App::rank_catalog_candidates("vendor/atlas", &catalog, 5),
        [suffix_match, "atlas", "vendor/atlas-preview"]
    );
    assert_eq!(
        App::rank_catalog_candidates(" \t ", &catalog, 2),
        ["vendor/atlas-preview", "atlas"]
    );
    assert!(App::rank_catalog_candidates("atlas", &catalog, 0).is_empty());
    assert!(App::rank_catalog_candidates("atlas", &[], 2).is_empty());
}

#[test]
fn quorum_output_cap_uses_environment_overrides() {
    const CHILD_CASE: &str = "HERMES_QUORUM_COVERAGE_OUTPUT_CAP_CASE";
    const CAP_VAR: &str = "HERMES_QUORUM_MAX_VOTER_OUTPUT_CHARS";
    const CHECKED: &str = "quorum output cap assertion ran";

    if let Ok(case) = std::env::var(CHILD_CASE) {
        let expected = match case.as_str() {
            "unbounded" => None,
            "positive" => Some(321),
            "invalid" | "missing" => Some(QUORUM_MAX_VOTER_OUTPUT_CHARS),
            other => panic!("unexpected output cap case: {other}"),
        };
        assert_eq!(App::quorum_output_char_cap(), expected, "case: {case}");
        println!("{CHECKED}");
        return;
    }

    // Use the runtime's token parser to select an accepted spelling, then check
    // that the output cap honors that token in the child.
    let unbounded = [
        "unlimited",
        "infinite",
        "infinity",
        "inf",
        "none",
        "off",
        "no-limit",
        "no_limit",
        "nolimit",
        "max",
        "0",
        "-1",
        "∞",
    ]
    .into_iter()
    .find(|raw| App::is_unbounded_token(raw));
    let cases = [
        ("positive", Some(" 321 ")),
        ("invalid", Some("not-a-cap")),
        ("missing", None),
    ];

    // Run each case in a child test harness so parallel tests never see its environment.
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let test_name = format!("{module}::quorum_output_cap_uses_environment_overrides");
    let executable = std::env::current_exe().expect("test executable"); // nosemgrep: rust.lang.security.current-exe.current-exe
    for (case, raw) in cases
        .into_iter()
        .chain(unbounded.map(|raw| ("unbounded", Some(raw))))
    {
        let mut command = std::process::Command::new(&executable);
        command
            .args(["--exact", test_name.as_str(), "--nocapture"])
            .env(CHILD_CASE, case);
        if let Some(raw) = raw {
            command.env(CAP_VAR, raw);
        } else {
            command.env_remove(CAP_VAR);
        }
        let output = command.output().expect("run output cap test case");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(CHECKED),
            "child case {case} did not pass its assertion:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn quorum_voter_prompt_switches_to_review_after_first_pass() {
    let first = App::build_quorum_voter_prompt(0, 3, "test:voter");
    assert!(first.starts_with("[QUORUM_VOTER] model=test:voter\n"));
    assert!(first.contains("exists_now=true/false"));
    assert!(first.ends_with("This is pass 1/3."));

    let review = App::build_quorum_voter_prompt(1, 3, "test:voter");
    assert!(review.starts_with("[QUORUM_VOTER_REVIEW] pass 2/3\n"));
    assert!(review.contains("Critique and strengthen your prior answer."));
    assert!(review.contains("Remove any unverified file names/modules/metrics."));
    assert!(review.ends_with("Keep objective truth over optimism."));
    assert!(!review.contains("[QUORUM_VOTER]"));
}

fn assistant_message(content: Option<&str>, reasoning: Option<&str>) -> hermes_core::Message {
    let mut message = hermes_core::Message::user(String::new());
    message.role = hermes_core::MessageRole::Assistant;
    message.content = content.map(str::to_owned);
    message.reasoning_content = reasoning.map(str::to_owned);
    message
}

#[test]
fn quorum_extracts_latest_nonempty_assistant_output_or_reasoning() {
    let mut messages = vec![
        assistant_message(Some("older response"), None),
        assistant_message(Some(" \t "), Some("  useful reasoning\n")),
        assistant_message(None, Some(" \n ")),
        hermes_core::Message::user("not an assistant response".to_string()),
    ];
    assert_eq!(
        App::extract_last_assistant_output(&messages),
        "useful reasoning"
    );

    messages.push(assistant_message(
        Some("  final answer "),
        Some("do not select this"),
    ));
    assert_eq!(
        App::extract_last_assistant_output(&messages),
        "final answer"
    );
    assert!(App::extract_last_assistant_output(&messages[2..4]).is_empty());
}

#[test]
fn quorum_truncation_counts_unicode_characters_and_reserves_ellipsis() {
    let text = "é界🦀!";
    assert_eq!(App::truncate_for_quorum(text, None), text);
    assert_eq!(App::truncate_for_quorum(text, Some(0)), text);
    assert_eq!(App::truncate_for_quorum(text, Some(4)), text);
    assert_eq!(App::truncate_for_quorum(text, Some(3)), "é界…");
    assert_eq!(App::truncate_for_quorum(text, Some(1)), "…");
}

#[test]
fn quorum_synthesis_prompt_includes_successful_and_failed_voter_evidence() {
    let policy = test_policy(&["test:first", "test:second"], 7);
    let outcomes = [
        QuorumVoterOutcome {
            model: "test:first".to_string(),
            status: "ok".to_string(),
            duration_ms: 23,
            total_turns: 2,
            tool_errors: 0,
            error: None,
            output: "The first voter found evidence.".to_string(),
        },
        QuorumVoterOutcome {
            model: "test:second".to_string(),
            status: "failed".to_string(),
            duration_ms: 41,
            total_turns: 1,
            tool_errors: 1,
            error: Some("voter stopped before completion".to_string()),
            output: "Partial observation.".to_string(),
        },
    ];

    let prompt = App::build_quorum_synthesis_prompt(&policy, &outcomes);
    let required_success = App::required_quorum_success(outcomes.len());
    assert!(prompt.starts_with("[QUORUM_SYNTHESIS]"));
    assert!(prompt.contains("Call out disagreements explicitly."));
    assert!(prompt.contains("Only cite a command/file/result if it appears verbatim"));
    assert!(prompt.ends_with(&format!(
        "Configured voters: 7 | mode=coverage-mode | enabled=true | required_success={required_success}\n\n\
         === VOTER 1 ===\nmodel: test:first\nstatus: ok\nduration_ms: 23\nturns: 2\ntool_errors: 0\n\
         output:\nThe first voter found evidence.\n\n\
         === VOTER 2 ===\nmodel: test:second\nstatus: failed\nduration_ms: 41\nturns: 1\ntool_errors: 1\n\
         error:\nvoter stopped before completion\noutput:\nPartial observation.\n\n"
    )));
}

#[test]
fn quorum_artifact_update_preserves_metadata_and_trims_synthesis() {
    let dir = tempfile::tempdir().expect("temporary artifact directory");
    let path = dir.path().join("artifact.json");
    std::fs::write(
        &path,
        r#"{"session_id":"test-session","voters":[{"model":"test:first"}],"synthesis":"old"}"#,
    )
    .expect("write initial artifact");

    App::update_quorum_artifact_with_synthesis(&path, " \n new synthesis \t ")
        .expect("update artifact");

    let raw = std::fs::read_to_string(&path).expect("read updated artifact");
    let mut updated: serde_json::Value =
        serde_json::from_str(&raw).expect("parse updated artifact");
    let saved_at = updated
        .as_object_mut()
        .expect("artifact object")
        .remove("synthesis_saved_at")
        .expect("synthesis timestamp");
    assert!(saved_at
        .as_str()
        .expect("timestamp string")
        .parse::<jiff::Timestamp>()
        .is_ok());
    assert_eq!(
        updated,
        serde_json::json!({
            "session_id": "test-session",
            "voters": [{"model": "test:first"}],
            "synthesis": "new synthesis",
        })
    );
}

#[test]
fn quorum_artifact_update_reports_read_and_parse_failures() {
    let dir = tempfile::tempdir().expect("temporary artifact directory");
    let path = dir.path().join("artifact.json");
    let read_prefix = format!("Failed to read quorum artifact {}:", path.display());
    let error = App::update_quorum_artifact_with_synthesis(&path, "new")
        .expect_err("a missing artifact cannot be updated");
    assert!(
        matches!(&error, AgentError::Io(detail) if detail.starts_with(read_prefix.as_str())),
        "{error:?}"
    );
    assert!(!path.exists());

    let invalid = "{invalid JSON";
    std::fs::write(&path, invalid).expect("write invalid artifact");
    let parse_prefix = format!("Failed to parse quorum artifact {}:", path.display());
    let error = App::update_quorum_artifact_with_synthesis(&path, "new")
        .expect_err("an invalid artifact cannot be updated");
    assert!(
        matches!(&error, AgentError::Config(detail) if detail.starts_with(parse_prefix.as_str())),
        "{error:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read artifact"),
        invalid
    );
}

#[cfg(target_os = "linux")]
#[test]
fn quorum_artifact_update_reports_write_failure_for_sealed_file() {
    use rustix::fs::{fcntl_add_seals, memfd_create, MemfdFlags, SealFlags};
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;

    // Seals make this readable but unwritable even when tests run as root.
    let fd = memfd_create(
        "quorum-artifact",
        MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC,
    )
    .expect("create anonymous artifact");
    let mut file = std::fs::File::from(fd);
    let original = br#"{"session_id":"sealed"}"#;
    file.write_all(original).expect("write initial artifact");
    fcntl_add_seals(
        &file,
        SealFlags::WRITE | SealFlags::SHRINK | SealFlags::GROW,
    )
    .expect("seal artifact");

    let path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
    let prefix = format!(
        "Failed to write quorum synthesis artifact {}:",
        path.display()
    );
    let error = App::update_quorum_artifact_with_synthesis(&path, "new")
        .expect_err("a sealed artifact cannot be rewritten");
    assert!(
        matches!(&error, AgentError::Io(detail) if detail.starts_with(prefix.as_str())),
        "{error:?}"
    );
    assert_eq!(
        std::fs::read(path)
            .expect("read sealed artifact")
            .as_slice(),
        original
    );
}
