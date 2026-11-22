use super::*;

fn benchmark_fixture() -> Value {
    json!({
        "id": "coverage-bench",
        "name": "Coverage benchmark",
        "source": "local",
        "version": "1"
    })
}

fn task_with_context(context: Value) -> TaskSpec {
    TaskSpec {
        task_id: "coverage-task".into(),
        category: None,
        instruction: "Report the result".into(),
        context,
        timeout: Duration::from_secs(10),
    }
}

fn dataset_error(path: &Path) -> String {
    match ConfiguredBenchmarkAdapter::from_path(path) {
        Err(EvalError::DatasetLoad(message)) => message,
        other => panic!("expected a dataset load error, got {other:?}"),
    }
}

#[test]
fn reports_malformed_unsupported_and_empty_benchmark_files() {
    let dir = tempfile::tempdir().expect("create benchmark directory");

    let malformed = dir.path().join("malformed.JSON");
    fs::write(&malformed, "{").expect("write malformed benchmark");
    let message = dataset_error(&malformed);
    assert!(message.starts_with(&format!("{}: ", malformed.display())));
    assert!(message.contains("EOF"), "{message}");

    let unsupported = dir.path().join("benchmark.csv");
    fs::write(&unsupported, "task_id,instruction\n").expect("write unsupported benchmark");
    assert_eq!(
        dataset_error(&unsupported),
        format!(
            "unsupported benchmark file extension for {} (expected .toml or .json)",
            unsupported.display()
        )
    );

    let empty = dir.path().join("empty.json");
    fs::write(
        &empty,
        json!({"benchmark": benchmark_fixture()}).to_string(),
    )
    .expect("write benchmark without tasks");
    assert_eq!(
        dataset_error(&empty),
        "benchmark coverage-bench has no tasks"
    );
}

#[tokio::test]
async fn json_adapter_loads_task_context_and_provides_a_verifier() {
    let dir = tempfile::tempdir().expect("create benchmark directory");
    let path = dir.path().join("benchmark.JSON");
    let fixture = json!({
        "benchmark": benchmark_fixture(),
        "tasks": [
            {
                "task_id": "first",
                "category": "formatting",
                "instruction": "Write a greeting",
                "context": {"audience": "team"},
                "timeout_secs": 0,
                "expected_contains": ["hello"],
                "expected_regex": ["^Hello"],
                "expected_any": ["team", "everyone"],
                "min_length": 5,
                "judge_prompt": "Check the greeting"
            },
            {
                "task_id": "second",
                "instruction": "Say done",
                "expected_contains": ["done"]
            }
        ]
    });
    fs::write(&path, fixture.to_string()).expect("write benchmark");

    let adapter = ConfiguredBenchmarkAdapter::from_path(path).expect("load JSON benchmark");
    let metadata = adapter.metadata();
    assert_eq!(metadata.id, "coverage-bench");
    assert_eq!(metadata.name, "Coverage benchmark");

    let tasks = adapter.load_tasks().await.expect("load tasks");
    assert_eq!(tasks.len(), 2);
    let first = &tasks[0];
    assert_eq!(first.task_id, "first");
    assert_eq!(first.category.as_deref(), Some("formatting"));
    assert_eq!(first.instruction, "Write a greeting");
    assert_eq!(first.timeout, Duration::from_secs(1));
    assert_eq!(
        first.context,
        json!({
            "context": {"audience": "team"},
            "expected_contains": ["hello"],
            "expected_regex": ["^Hello"],
            "expected_any": ["team", "everyone"],
            "min_length": 5,
            "judge_prompt": "Check the greeting"
        })
    );

    let second = &tasks[1];
    assert_eq!(second.task_id, "second");
    assert_eq!(second.category, None);
    assert_eq!(second.timeout, Duration::from_secs(300));
    assert_eq!(
        second.context,
        json!({
            "context": null,
            "expected_contains": ["done"],
            "expected_regex": [],
            "expected_any": [],
            "min_length": null,
            "judge_prompt": null
        })
    );

    let verdict = adapter
        .verifier()
        .verify(second, &json!({"response": "still working"}))
        .await
        .expect("verify without a judge prompt");
    assert!(!verdict.passed);
    assert_eq!(
        verdict.detail.as_deref(),
        Some("missing expected_contains token: done")
    );
    assert_eq!(
        verdict.metadata,
        json!({"judge": "heuristic", "failures": ["missing expected_contains token: done"]})
    );
}

#[test]
fn extracts_latest_usable_assistant_message_after_trying_top_level_fields() {
    let mut state = json!({
        "last_assistant_preview": "preview",
        "response": "response",
        "messages": [
            {"role": "assistant", "content": "older answer"},
            {"role": "ASSISTANT", "content": "latest usable answer"},
            {"role": "assistant", "content": ["not a string"]},
            {"role": "user", "content": "ignore this"},
            {"content": "no role"}
        ]
    });
    assert_eq!(extract_agent_text(&state), "preview");

    state["last_assistant_preview"] = json!(123);
    assert_eq!(extract_agent_text(&state), "response");

    state["response"] = Value::Null;
    assert_eq!(extract_agent_text(&state), "latest usable answer");

    state["messages"] = json!([
        {"role": "user", "content": "only a user message"},
        {"role": "assistant", "content": null}
    ]);
    assert_eq!(extract_agent_text(&state), state.to_string());
}

#[test]
fn heuristic_checks_count_characters_and_report_failed_expectations() {
    let task = task_with_context(json!({
        "min_length": 4,
        "expected_contains": [false, "Alpha"],
        "expected_regex": [null, "^ALPHA"],
        "expected_any": ["omega", 123, "BETA"]
    }));

    let ok = heuristic_verify(&task, "ALPHA beta").expect("run passing checks");
    assert!(ok.passed);
    assert!((ok.score - 1.0).abs() < f64::EPSILON);
    assert_eq!(ok.detail.as_deref(), Some("heuristic checks passed"));
    assert_eq!(ok.metadata, json!({"judge": "heuristic"}));

    let failed = heuristic_verify(&task, "Aé!").expect("run failing checks");
    let failures = [
        "output shorter than min_length=4",
        "missing expected_contains token: Alpha",
        "regex did not match: ^ALPHA",
        "none of expected_any tokens matched (omega, BETA)",
    ];
    assert!(!failed.passed);
    assert!(failed.score.abs() < f64::EPSILON);
    assert_eq!(failed.detail, Some(failures.join("; ")));
    assert_eq!(
        failed.metadata,
        json!({"judge": "heuristic", "failures": failures})
    );
}

#[test]
fn heuristic_ignores_expected_any_without_string_tokens() {
    let task = task_with_context(json!({"expected_any": [null, 42]}));
    let verdict = heuristic_verify(&task, "").expect("run checks");
    assert!(verdict.passed);
    assert_eq!(verdict.detail.as_deref(), Some("heuristic checks passed"));
}

#[test]
fn heuristic_rejects_invalid_regex_as_a_verification_error() {
    let task = task_with_context(json!({"expected_regex": ["^answer", "["]}));
    let Err(EvalError::Verification(message)) = heuristic_verify(&task, "answer") else {
        panic!("invalid regex should produce a verification error");
    };
    assert!(
        message.starts_with("invalid expected_regex `[`: "),
        "{message}"
    );
}

#[test]
fn judge_json_clamps_scores_and_respects_explicit_verdicts() {
    let cases = [
        (
            r#"{"score":1.75,"passed":false,"detail":"explicit failure"}"#,
            1.0,
            false,
            "explicit failure",
        ),
        (r#"{"score":-0.3}"#, 0.0, false, "llm judge"),
        (r#"{"score":0.5}"#, 0.5, true, "llm judge"),
        (r#"{"score":0.49}"#, 0.49, false, "llm judge"),
        (
            r#"{"score":"unknown","passed":true,"detail":42}"#,
            0.0,
            true,
            "llm judge",
        ),
        ("{}", 0.0, false, "llm judge"),
    ];

    for (input, score, passed, detail) in cases {
        let verdict = parse_judge_json(input).expect("parse judge verdict");
        assert!((verdict.score - score).abs() < f64::EPSILON, "{input}");
        assert_eq!(verdict.passed, passed, "{input}");
        assert_eq!(verdict.detail.as_deref(), Some(detail), "{input}");
        assert_eq!(verdict.metadata, json!({"judge": "llm"}), "{input}");
    }
}

#[test]
fn judge_json_reports_the_raw_content_when_it_cannot_parse_it() {
    let Err(EvalError::Verification(message)) = parse_judge_json("not JSON") else {
        panic!("invalid JSON should produce a verification error");
    };
    assert!(
        message.starts_with("judge content is not valid JSON: not JSON ("),
        "{message}"
    );
}

#[tokio::test]
async fn judge_configuration_without_network() {
    const CHILD_CASE: &str = "HERMES_EVAL_CONFIGURED_COVERAGE_CASE";
    const COMPLETED: &str = "configured judge coverage case completed";
    const INVALID_BASE_URL: &str = ":invalid/";

    match std::env::var(CHILD_CASE).as_deref() {
        Ok("missing-key") => {
            assert!(llm_judge_enabled());
            assert!(std::env::var_os("HERMES_EVAL_JUDGE_API_KEY").is_none());
            assert!(std::env::var_os("OPENAI_API_KEY").is_none());

            let verifier = ConfiguredVerifier;
            let state = json!({"response": "green"});
            for prompt in [json!(" \t\n "), json!(17)] {
                let task = task_with_context(json!({
                    "expected_any": ["GREEN"],
                    "judge_prompt": prompt
                }));
                let verdict = verifier
                    .verify(&task, &state)
                    .await
                    .expect("use heuristics");
                assert!(verdict.passed);
                assert_eq!(verdict.detail.as_deref(), Some("heuristic checks passed"));
                assert_eq!(verdict.metadata, json!({"judge": "heuristic"}));
            }

            let task = task_with_context(json!({
                "expected_contains": ["gold"],
                "judge_prompt": "  Check the result  "
            }));
            let verdict = verifier
                .verify(&task, &state)
                .await
                .expect("fall back to heuristics without an API key");
            assert!(!verdict.passed);
            assert!(verdict.score.abs() < f64::EPSILON);
            assert_eq!(verdict.metadata, json!({"judge": "fallback_heuristic"}));
            let detail = verdict.detail.expect("fallback explains both results");
            assert!(detail
                .starts_with("missing expected_contains token: gold; llm-judge-unavailable: "));
            assert!(detail.contains("missing HERMES_EVAL_JUDGE_API_KEY/OPENAI_API_KEY"));
        }
        Ok("malformed-url") => {
            assert_eq!(
                std::env::var("HERMES_EVAL_JUDGE_BASE_URL").as_deref(),
                Ok(INVALID_BASE_URL)
            );
            let task = task_with_context(json!({}));
            let Err(error) = llm_judge(&task, "green", "Check the result").await else {
                panic!("an invalid endpoint must fail before making a request");
            };
            assert!(error.to_string().contains("builder error"), "{error}");
        }
        _ => {
            // Isolate judge settings from the other tests running in the parent.
            let (_, module) = module_path!().split_once("::").expect("test module path");
            let test_name = format!("{module}::judge_configuration_without_network");
            let executable = std::env::current_exe().expect("test executable");
            let home = tempfile::tempdir().expect("create temporary home");
            for case in ["missing-key", "malformed-url"] {
                let mut command = std::process::Command::new(&executable);
                command
                    .args(["--exact", test_name.as_str(), "--nocapture"])
                    .env(CHILD_CASE, case)
                    .env("HOME", home.path())
                    .env("USERPROFILE", home.path())
                    .env("HERMES_EVAL_LLM_JUDGE", " \tYeS ")
                    .env("HERMES_EVAL_JUDGE_BASE_URL", INVALID_BASE_URL)
                    .env_remove("HERMES_EVAL_JUDGE_MODEL")
                    .env_remove("HERMES_EVAL_JUDGE_API_KEY")
                    .env_remove("OPENAI_API_KEY");
                if case == "malformed-url" {
                    command.env("OPENAI_API_KEY", "not-a-real-key");
                }
                let output = command.output().expect("run isolated judge test");
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    output.status.success() && stdout.contains(COMPLETED),
                    "{case} did not finish successfully:\n{stdout}\n{stderr}"
                );
            }
            return;
        }
    }
    println!("{COMPLETED}");
}
