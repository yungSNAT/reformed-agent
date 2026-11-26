use std::process::Command;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::*;

const CHILD: &str = "HERMES_CHRONOS_PROVISION_TEST_CHILD";
const FIXTURE_TOKEN: &str = "chronos-loopback-test";
const CALLBACK: &str = "https://example.invalid/cron/fire";
const FIRE_AT: &str = "2030-01-02T03:04:05+00:00";

#[test]
fn reconciles_managed_jobs_over_loopback_and_reports_rejections() {
    if std::env::var_os(CHILD).is_none() {
        let profile = tempfile::tempdir().unwrap();
        let managed = profile.path().join("managed");
        std::fs::create_dir(&managed).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("HOME", profile.path())
            .env("HERMES_HOME", profile.path())
            .env("HERMES_MANAGED_DIR", managed)
            .env(CHILD, "1")
            .current_dir(profile.path())
            .args([
                "--exact",
                "chronos::provision_contract::reconciles_managed_jobs_over_loopback_and_reports_rejections",
                "--color",
                "never",
            ]);
        if let Some(path) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", path);
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "isolated Chronos fixture exited {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(verify_reconciliation());
}

fn job(id: &str, status: JobStatus, time: Option<&str>) -> CronJob {
    let mut job = CronJob::new("0 4 * * *", "Synthetic job");
    job.id = id.into();
    job.status = status;
    job.next_run = time.map(|value| value.parse().unwrap());
    job
}

fn provision_body(id: &str) -> Value {
    json!({
        "job_id": id,
        "fire_at": FIRE_AT,
        "agent_callback_url": CALLBACK,
        "dedup_key": format!("{id}:{FIRE_AT}"),
    })
}

async fn verify_reconciliation() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let provider = ChronosNasCronProvider::new(ChronosConfig {
        provider: "chronos".into(),
        portal_url: format!("http://{}/", listener.local_addr().unwrap()),
        callback_url: CALLBACK.into(),
        request_timeout_seconds: 2,
        ..ChronosConfig::default()
    });
    assert_eq!(provider.name(), "chronos");
    assert!(!provider.is_available());
    provider.reconcile(Vec::new()).await.unwrap();
    provider.cancel("unused").await.unwrap();

    let auth_path = hermes_config::paths::auth_json_path();
    assert_eq!(
        auth_path,
        std::env::current_dir().unwrap().join("auth.json")
    );
    std::fs::write(
        auth_path,
        json!({"providers":{"nous":{"access_token":FIXTURE_TOKEN}}}).to_string(),
    )
    .unwrap();
    assert!(provider.is_available());
    let observed = json!({"armed":[
        {"job_id":"same","fire_at":FIRE_AT},
        {"job_id":"reschedule","fire_at":"2030-01-01T00:00:00+00:00"},
        {"job_id":"paused","fire_at":FIRE_AT},
        {"job_id":"stale"},
        {"fire_at":FIRE_AT}
    ]});
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(8), async {
            for (request, body, status, response) in [
                ("GET /api/agent-cron/list HTTP/1.1", Value::Null, "200 OK", observed.to_string()),
                ("POST /api/agent-cron/provision HTTP/1.1", provision_body("new"), "200 OK", "{}".into()),
                ("POST /api/agent-cron/provision HTTP/1.1", provision_body("reschedule"), "200 OK", "{}".into()),
                ("POST /api/agent-cron/cancel HTTP/1.1", json!({"job_id":"paused"}), "200 OK", "{}".into()),
                ("POST /api/agent-cron/cancel HTTP/1.1", json!({"job_id":"stale"}), "200 OK", "{}".into()),
                ("GET /api/agent-cron/list HTTP/1.1", Value::Null, "200 OK", "invalid JSON".into()),
                ("POST /api/agent-cron/provision HTTP/1.1", provision_body("fallback"), "200 OK", "{}".into()),
                ("POST /api/agent-cron/cancel HTTP/1.1", json!({"job_id":"rejected"}), "409 Conflict", "{}".into()),
            ] {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                assert_eq!(line.trim_end(), request);
                let mut authenticated = false;
                let mut length = 0;
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    let (key, value) = line.split_once(':').unwrap();
                    if key.eq_ignore_ascii_case("authorization") {
                        authenticated = value.trim() == format!("Bearer {FIXTURE_TOKEN}");
                    } else if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
                assert!(authenticated, "missing fixture bearer header");
                assert!(length <= 1024, "unexpected request body length");
                let mut data = vec![0; length];
                reader.read_exact(&mut data).await.unwrap();
                let received = if data.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&data).unwrap()
                };
                assert_eq!(received, body);
                reader.get_mut().write_all(format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                ).as_bytes()).await.unwrap();
            }
        })
        .await
        .expect("fixture server timed out");
    });
    provider
        .reconcile(vec![
            job("same", JobStatus::Active, Some(FIRE_AT)),
            job("new", JobStatus::Active, Some(FIRE_AT)),
            job("reschedule", JobStatus::Active, Some(FIRE_AT)),
            job("paused", JobStatus::Paused, Some(FIRE_AT)),
            job("unscheduled", JobStatus::Active, None),
        ])
        .await
        .unwrap();
    provider
        .reconcile(vec![job("fallback", JobStatus::Active, Some(FIRE_AT))])
        .await
        .unwrap();
    let error = provider.cancel("rejected").await.unwrap_err();
    assert!(matches!(
        error,
        CronError::Scheduler(message) if message.contains("Chronos cancel rejected:") && message.contains("409")
    ));
    server.await.unwrap();
}
