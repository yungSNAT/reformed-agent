use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

use super::*;

const CHILD_HOME: &str = "HERMES_CRON_FIRE_TEST_CHILD_HOME";
const ISSUER: &str = "https://portal.example.invalid";
const AUDIENCE: &str = "fixture-cron";

#[tokio::test]
async fn cron_fire_verifies_claims_and_dispatches_only_a_current_job() {
    let Some(home) = std::env::var_os(CHILD_HOME) else {
        let profile = tempfile::tempdir().expect("fixture home");
        let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"));
        child
            .env_clear()
            .env(CHILD_HOME, profile.path())
            .env("HOME", profile.path())
            .env("USERPROFILE", profile.path())
            .env("HERMES_HOME", profile.path())
            .env("HERMES_MANAGED_DIR", profile.path().join("managed"))
            .current_dir(profile.path())
            .kill_on_drop(true)
            .args([
                "--exact",
                "platforms::api_server::tests::cron_fire_contract::cron_fire_verifies_claims_and_dispatches_only_a_current_job",
            ]);
        if let Some(root) = std::env::var_os("SystemRoot") {
            child.env("SystemRoot", root);
        }
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            let path = std::path::PathBuf::from(profile);
            let path = if path.starts_with("%t") {
                path
            } else {
                std::path::absolute(path).expect("absolute LLVM profile path")
            };
            child.env("LLVM_PROFILE_FILE", path);
        }
        let output = tokio::time::timeout(Duration::from_secs(15), child.output())
            .await
            .expect("isolated API test timeout")
            .expect("isolated API test");
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
            "{output:?}"
        );
        return;
    };

    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
            .expect("generate fixture signing key");
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref())
        .expect("read fixture signing key");
    let point = pair.public_key().as_ref();
    let jwks = json!({"keys": [{
        "kty": "EC", "crv": "P-256",
        "x": URL_SAFE_NO_PAD.encode(&point[1..33]), "y": URL_SAFE_NO_PAD.encode(&point[33..65])
    }]})
    .to_string();
    std::fs::write(
        std::path::PathBuf::from(home).join("config.yaml"),
        serde_yaml::to_string(&json!({
            "cron": {
                "provider": "chronos",
                "chronos": {
                    "portal_url": ISSUER,
                    "expected_audience": AUDIENCE,
                    "nas_jwks_url": jwks
                }
            }
        }))
        .expect("serialize fixture config"),
    )
    .expect("write fixture config");
    let key = EncodingKey::from_ec_der(pkcs8.as_ref());
    let sign = |audience: &str| {
        let expires = jiff::Timestamp::now().as_second() + 300;
        encode(
            &Header::new(Algorithm::ES256),
            &json!({"purpose": "cron_fire", "iss": ISSUER, "aud": audience, "exp": expires}),
            &key,
        )
        .expect("sign fixture claim")
    };
    let valid = sign(AUDIENCE);
    let wrong_audience = sign("another-fixture");
    let (tx, _rx) = mpsc::channel(1);
    let mut state = ApiTestState::new(tx);
    let (completion, mut events) = tokio::sync::broadcast::channel(2);
    Arc::get_mut(&mut state.cron_scheduler)
        .expect("fixture scheduler has one owner")
        .set_completion_broadcast(completion);
    let job_id = state
        .cron_scheduler
        .create_job(hermes_cron::CronJob::new("0 * * * *", "summarise fixture"))
        .await
        .expect("create local job");
    let scheduled = state
        .cron_scheduler
        .get_job(&job_id)
        .await
        .expect("load local job")
        .next_run
        .expect("scheduled time");
    let stale_job_id = state
        .cron_scheduler
        .create_job(hermes_cron::CronJob::new("15 * * * *", "ignore stale fire"))
        .await
        .expect("create second local job");

    for (token, body, status, message) in [
        (&wrong_audience, "{".to_string(), 401, "Unauthorized"),
        (&valid, "{".to_string(), 400, "Invalid request:"),
        (
            &valid,
            json!({"job_id": "   "}).to_string(),
            400,
            "job_id is required",
        ),
        (
            &valid,
            json!({"job_id": job_id, "fire_at": "yesterday"}).to_string(),
            400,
            "fire_at must be RFC3339",
        ),
        (
            &valid,
            json!({"job_id": "missing"}).to_string(),
            404,
            "Job not found",
        ),
    ] {
        let (actual_status, reply) = fire_request(&state, token, &body).await;
        assert_eq!(actual_status, status);
        let actual_message = reply["error"]["message"].as_str().expect("error message");
        assert!(actual_message.starts_with(message), "{actual_message}");
    }
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    for (id, fire_at, dispatched) in [
        (&stale_job_id, "2001-01-01T00:00:00Z".to_string(), false),
        (&job_id, scheduled.to_string(), true),
    ] {
        let data = json!({"job_id": format!(" {id} "), "fire_at": format!(" {fire_at} ")});
        assert_eq!(
            fire_request(&state, &valid, &data.to_string()).await,
            (
                202,
                json!({"status": "accepted", "job_id": id, "dispatched": dispatched})
            )
        );
    }
    let completed = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("job completion deadline")
        .expect("completion event");
    assert_eq!(completed.job_id, job_id);
    assert!(completed.ok, "{:?}", completed.error);
    let stale = state
        .cron_scheduler
        .get_job(&stale_job_id)
        .await
        .expect("load stale job");
    assert_eq!(stale.run_count, 0);
}

async fn fire_request(state: &ApiTestState, token: &str, body: &str) -> (u16, Value) {
    let request = format!(
        "POST /api/cron/fire HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    );
    let response = state
        .roundtrip(request, Some("fixture-api-value".into()))
        .await;
    let status = response
        .split_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse()
        .expect("numeric HTTP status");
    (status, json_body(&response))
}
