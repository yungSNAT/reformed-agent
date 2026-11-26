use hermes_eval::{RunRecord, TaskStatus};
use serde_json::{json, Value};
use std::{fs, net::Ipv4Addr, path::Path, process::Output, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::Command,
    time::timeout,
};

fn smoke_command(home: &Path) -> Command {
    let dataset = home.join("fixture.json");
    fs::write(
        &dataset,
        serde_json::to_vec(&json!({
            "benchmark": {"id": "fixture-smoke", "name": "Fixture", "source": "local", "version": "1"},
            "tasks": [{
                "task_id": "fixture-task",
                "instruction": "Report the fixture result",
                "expected_contains": ["deterministic smoke rollout"],
                "judge_prompt": "Assess only the fixture result"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let managed = home.join("managed");
    fs::create_dir_all(&managed).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hermes-bench"));
    cmd.env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("HERMES_HOME", home)
        .env("HERMES_MANAGED_DIR", managed)
        .current_dir(home)
        .kill_on_drop(true)
        .arg("--benchmark")
        .arg(dataset)
        .args([
            "--rollout",
            "smoke",
            "--model",
            "fixture-rollout",
            "--seed",
            "42",
        ]);
    if let Some(value) = std::env::var_os("SystemRoot") {
        cmd.env("SystemRoot", value);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        let profile = std::path::PathBuf::from(profile);
        let profile = if profile.starts_with("%t") {
            profile
        } else {
            std::path::absolute(profile).unwrap()
        };
        cmd.env("LLVM_PROFILE_FILE", profile);
    }
    cmd
}

async fn bench_output(cmd: &mut Command) -> Output {
    timeout(Duration::from_secs(15), cmd.output())
        .await
        .expect("fixture benchmark must exit")
        .unwrap()
}

fn success_text(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout).unwrap()
}

#[tokio::test]
async fn smoke_run_writes_the_default_report_and_rejects_an_empty_filter() {
    let home = tempfile::tempdir().unwrap();
    let run = bench_output(&mut smoke_command(home.path())).await;
    assert!(success_text(&run).contains("benchmark=fixture-smoke total=1 passed=1 failed=0"));

    let mut reports = fs::read_dir(home.path().join("evals")).unwrap();
    let report = reports.next().unwrap().unwrap().path();
    assert!(reports.next().is_none());
    let record: RunRecord = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(record.model, "fixture-rollout");
    assert_eq!(record.seed, 42);
    assert_eq!(record.tasks[0].task_id, "fixture-task");
    assert_eq!(record.tasks[0].status, TaskStatus::Passed);

    let failed = bench_output(smoke_command(home.path()).args([
        "--task-filter",
        "unmatched",
        "--output",
        "rejected.json",
    ]))
    .await;
    assert_eq!(failed.status.code(), Some(1));
    let error = String::from_utf8_lossy(&failed.stderr);
    assert!(
        error.contains("hermes-bench error") && error.contains("no tasks"),
        "{error}"
    );
    assert!(!home.path().join("rejected.json").exists());
}

async fn serve_one_judge_request(listener: &TcpListener, status: &str, response: &Value) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = BufReader::new(stream.take(16_384));
    let mut line = String::new();
    socket.read_line(&mut line).await.unwrap();
    assert_eq!(line.trim_end(), "POST /v1/chat/completions HTTP/1.1");
    let mut content_length = None;
    let mut auth_matches = false;
    loop {
        line.clear();
        assert!(socket.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
        if name.eq_ignore_ascii_case("authorization") {
            auth_matches = value.trim() == "Bearer fixture-judge";
        }
    }
    assert!(auth_matches);
    let length = content_length.unwrap();
    assert!(length <= 8_192);
    let mut payload = vec![0; length];
    socket.read_exact(&mut payload).await.unwrap();
    let request: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(request["model"], "fixture-judge-model");
    let prompt = request["messages"][1]["content"].as_str().unwrap();
    assert!(prompt.contains("Judge policy: Assess only the fixture result"));
    assert!(prompt.contains("deterministic smoke rollout for task_id=fixture-task"));

    let response = response.to_string();
    let wire = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
        response.len()
    );
    socket
        .get_mut()
        .get_mut()
        .write_all(wire.as_bytes())
        .await
        .unwrap();
}

#[tokio::test]
async fn smoke_run_uses_the_judge_verdict_or_falls_back_with_the_error() {
    let home = tempfile::tempdir().unwrap();
    for (status, response, reason) in [
        (
            "200 OK",
            json!({"choices": [{"message": {"content": "{\"score\":0.25,\"passed\":false,\"detail\":\"judge rejected fixture\"}"}}]}),
            None,
        ),
        (
            "503 Service Unavailable",
            json!({"error": "fixture unavailable"}),
            Some("fixture unavailable"),
        ),
        (
            "200 OK",
            json!({"choices": []}),
            Some("missing choices[0].message.content"),
        ),
    ] {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut cmd = smoke_command(home.path());
        cmd.env("HERMES_EVAL_LLM_JUDGE", "true")
            .env("HERMES_EVAL_JUDGE_API_KEY", "fixture-judge")
            .env("HERMES_EVAL_JUDGE_MODEL", "fixture-judge-model")
            .env(
                "HERMES_EVAL_JUDGE_BASE_URL",
                format!("http://{}/v1/", listener.local_addr().unwrap()),
            )
            .env("NO_PROXY", "127.0.0.1")
            .args(["--output", "judge.json", "--print-json"]);

        let (run, ()) = timeout(Duration::from_secs(15), async {
            tokio::join!(
                cmd.output(),
                serve_one_judge_request(&listener, status, &response)
            )
        })
        .await
        .expect("the local judge and benchmark must finish");
        let run = run.unwrap();
        let printed = success_text(&run);
        let stored = fs::read_to_string(home.path().join("judge.json")).unwrap();
        assert_eq!(printed.trim_end(), stored.trim_end());
        let record: RunRecord = serde_json::from_str(printed).unwrap();
        let task = &record.tasks[0];
        let detail = task.outcome.detail.as_deref().unwrap();
        if let Some(reason) = reason {
            assert_eq!(task.status, TaskStatus::Passed);
            assert_eq!(task.outcome.metadata["judge"], "fallback_heuristic");
            assert!(
                detail.contains("llm-judge-unavailable") && detail.contains(reason),
                "{detail}"
            );
        } else {
            assert_eq!(task.status, TaskStatus::Failed);
            assert!((task.outcome.score - 0.25).abs() < f64::EPSILON);
            assert_eq!(task.outcome.metadata["judge"], "llm");
            assert_eq!(detail, "judge rejected fixture");
        }
    }
}
