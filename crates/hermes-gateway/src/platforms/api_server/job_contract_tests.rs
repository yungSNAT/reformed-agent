use hermes_cron::{FileJobPersistence, JobPersistence};

#[test]
fn api_job_delivery_targets_roundtrip_with_canonical_names() {
    for (wire_name, target, alias) in [
        ("origin", DeliverTarget::Origin, None),
        ("local", DeliverTarget::Local, None),
        ("telegram", DeliverTarget::Telegram, None),
        ("discord", DeliverTarget::Discord, None),
        ("slack", DeliverTarget::Slack, None),
        ("email", DeliverTarget::Email, None),
        ("whatsapp", DeliverTarget::WhatsApp, None),
        ("signal", DeliverTarget::Signal, None),
        ("matrix", DeliverTarget::Matrix, None),
        ("mattermost", DeliverTarget::Mattermost, None),
        ("dingtalk", DeliverTarget::DingTalk, None),
        ("feishu", DeliverTarget::Feishu, None),
        ("wecom", DeliverTarget::WeCom, None),
        ("weixin", DeliverTarget::Weixin, Some("wechat")),
        (
            "bluebubbles",
            DeliverTarget::BlueBubbles,
            Some("blue_bubbles"),
        ),
        ("sms", DeliverTarget::Sms, None),
        (
            "homeassistant",
            DeliverTarget::HomeAssistant,
            Some("home_assistant"),
        ),
        ("ntfy", DeliverTarget::Ntfy, None),
    ] {
        assert_eq!(deliver_target_name(&target), wire_name);
        for name in std::iter::once(wire_name).chain(alias) {
            let input = serde_json::json!(format!(" {} ", name.to_ascii_uppercase()));
            let deliver = parse_api_deliver_config(Some(&input))
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .expect("delivery target");
            assert_eq!(deliver.target, target, "{name}");
            assert!(deliver.platform.is_none());
        }
    }
}

#[tokio::test]
async fn api_job_routes_report_invalid_bodies_and_missing_jobs() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let missing_path = "/api/jobs/aabbccddeeff";

    for (method, path) in [("POST", "/api/jobs"), ("PATCH", missing_path)] {
        let raw = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 1\r\n\r\n{{"
        );
        let response = state.roundtrip(raw, None).await;
        assert!(
            response.starts_with("HTTP/1.1 400 Bad Request"),
            "{response}"
        );
        let error = &json_body(&response)["error"];
        assert_eq!(error["type"], "invalid_request_error");
        assert!(error["message"]
            .as_str()
            .unwrap()
            .starts_with("Invalid request:"));
    }

    let response = state
        .roundtrip(
            json_request("PATCH", missing_path, serde_json::json!([])),
            None,
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "{response}"
    );
    assert_eq!(
        json_body(&response)["error"]["message"],
        "Request body must be an object"
    );

    let response = state
        .roundtrip(
            json_request(
                "POST",
                "/api/jobs",
                serde_json::json!({
                    "name": "Invalid fixture schedule",
                    "schedule": "invalid fixture schedule",
                    "prompt": "Fixture only"
                }),
            ),
            None,
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "{response}"
    );
    assert_eq!(
        json_body(&response)["error"]["type"],
        "invalid_request_error"
    );

    for (method, path) in [
        ("DELETE", missing_path),
        ("POST", "/api/jobs/aabbccddeeff/pause"),
        ("POST", "/api/jobs/aabbccddeeff/resume"),
        ("POST", "/api/jobs/aabbccddeeff/run"),
    ] {
        let response = state.roundtrip(empty_request(method, path), None).await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
        assert_eq!(
            json_body(&response)["error"]["message"],
            "Job not found: aabbccddeeff"
        );
    }

    let store = FileJobPersistence::with_dir(state._cron_dir.path().join("cron"));
    assert!(store.load_jobs().await.unwrap().is_empty());
}

#[tokio::test]
async fn api_job_invalid_creates_do_not_leave_a_job_record() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);

    for (field, value, expected) in [
        ("name", serde_json::json!(" "), "name is required"),
        ("name", serde_json::json!("é".repeat(201)), "200 characters"),
        ("schedule", serde_json::json!(" "), "schedule is required"),
        ("prompt", serde_json::json!(" "), "prompt is required"),
        (
            "prompt",
            serde_json::json!("é".repeat(5001)),
            "5000 characters",
        ),
        (
            "repeat",
            serde_json::json!(0),
            "repeat must be greater than zero",
        ),
        (
            "deliver",
            serde_json::json!(44),
            "deliver must be a string or object",
        ),
        (
            "deliver",
            serde_json::json!({}),
            "deliver.target is required",
        ),
        (
            "deliver",
            serde_json::json!({"target": "other"}),
            "Unknown deliver target",
        ),
    ] {
        let mut body = serde_json::json!({
            "name": "fixture task",
            "schedule": "0 * * * *",
            "prompt": "Collect the fixture report.",
            "enabled": false
        });
        body[field] = value;
        let response = state
            .roundtrip(json_request("POST", "/api/jobs", body), None)
            .await;
        assert!(
            response.starts_with("HTTP/1.1 400 Bad Request"),
            "{response}"
        );
        let body = json_body(&response);
        let message = body["error"]["message"]
            .as_str()
            .expect("reported validation error");
        assert!(message.contains(expected), "{field}: {message}");
    }
    let response = state
        .roundtrip(empty_request("GET", "/api/jobs?include_disabled=yes"), None)
        .await;
    assert_eq!(json_body(&response)["jobs"], serde_json::json!([]));
    let store = FileJobPersistence::with_dir(state._cron_dir.path().join("cron"));
    assert!(store.load_jobs().await.unwrap().is_empty());
}

#[tokio::test]
async fn api_job_rejected_updates_leave_the_saved_configuration_intact() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let created = state
        .roundtrip(
            json_request(
                "POST",
                "/api/jobs",
                serde_json::json!({
                    "name": " Local fixture ",
                    "schedule": "0 * * * *",
                    "prompt": "Fixture only",
                    "deliver": {"target": " WeChat ", "recipient": " fixture-recipient "},
                    "enabled": false,
                    "repeat": 3,
                    "skills": [" fixture-skill ", " "],
                    "script": " fixture-script ",
                    "no_agent": true
                }),
            ),
            None,
        )
        .await;
    assert!(created.starts_with("HTTP/1.1 200 OK"), "{created}");
    let created = json_body(&created);
    let job = &created["job"];
    assert_eq!(job["name"], "Local fixture");
    assert_eq!(
        job["deliver"],
        serde_json::json!({"target": "weixin", "platform": "fixture-recipient"})
    );
    assert_eq!(job["skills"], serde_json::json!(["fixture-skill"]));
    assert_eq!(job["script"], "fixture-script");
    assert_eq!(job["no_agent"], true);
    assert_eq!(job["enabled"], false);
    let path = format!("/api/jobs/{}", job["id"].as_str().unwrap());

    for (field, value, expected) in [
        (
            "prompt",
            serde_json::json!("x".repeat(5001)),
            "5000 characters",
        ),
        ("schedule", serde_json::json!(" "), "schedule is required"),
        (
            "repeat",
            serde_json::json!("two"),
            "repeat must be an integer",
        ),
        (
            "repeat",
            serde_json::json!(0),
            "repeat must be greater than zero",
        ),
        (
            "repeat",
            serde_json::json!(u64::from(u32::MAX) + 1),
            "repeat is too large",
        ),
        (
            "skills",
            serde_json::json!("fixture-skill"),
            "skills must be an array",
        ),
    ] {
        let mut body = serde_json::json!({"name": "Must not persist"});
        body[field] = value;
        let response = state
            .roundtrip(json_request("PATCH", &path, body), None)
            .await;
        assert!(
            response.starts_with("HTTP/1.1 400 Bad Request"),
            "{response}"
        );
        let error = json_body(&response);
        let message = error["error"]["message"]
            .as_str()
            .expect("reported validation error");
        assert!(message.contains(expected), "{field}: {message}");
    }
    let saved = state.roundtrip(empty_request("GET", &path), None).await;
    assert_eq!(json_body(&saved)["job"], *job);
    let store = FileJobPersistence::with_dir(state._cron_dir.path().join("cron"));
    let persisted = store.load_jobs().await.unwrap();
    let [saved_job] = persisted.as_slice() else {
        panic!("expected one persisted job");
    };
    assert_eq!(api_cron_job_body(saved_job), *job);

    let response = state
        .roundtrip(
            json_request(
                "PATCH",
                &path,
                serde_json::json!({
                    "schedule": "15 * * * *",
                    "prompt": " Updated fixture ",
                    "deliver": "local",
                    "skills": null,
                    "repeat": null,
                    "enabled": false,
                    "script": null,
                    "no_agent": false
                }),
            ),
            None,
        )
        .await;
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    let patched = json_body(&response);
    assert_eq!(patched["job"]["schedule"], "15 * * * *");
    assert_eq!(patched["job"]["prompt"], "Updated fixture");
    assert_eq!(patched["job"]["deliver"], "local");
    assert_eq!(patched["job"]["enabled"], false);
    assert_eq!(patched["job"]["no_agent"], false);
    for field in ["skills", "repeat", "script"] {
        assert!(patched["job"][field].is_null());
    }
}

#[tokio::test]
async fn api_job_patch_enables_a_paused_job_and_persists_delivery() {
    let (tx, mut rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let created = state
        .roundtrip(
            json_request(
                "POST",
                "/api/jobs",
                serde_json::json!({
                    "name": "Paused fixture",
                    "schedule": "0 * * * *",
                    "prompt": "Summarise the fixture",
                    "enabled": false,
                }),
            ),
            None,
        )
        .await;
    assert!(created.starts_with("HTTP/1.1 200 OK"), "{created}");
    let created = json_body(&created);
    let job = &created["job"];
    assert_eq!(job["enabled"], false);
    assert!(job["next_run"].is_null());
    let id = job["id"].as_str().expect("job id");
    let path = format!("/api/jobs/{id}");

    let updated = state
        .roundtrip(
            json_request(
                "PATCH",
                &path,
                serde_json::json!({
                    "enabled": true,
                    "schedule": "15 * * * *",
                    "deliver": {"type": " Matrix ", "chat_id": " fixture-room "},
                    "repeat": 4,
                    "skills": [" daily-summary ", " "],
                }),
            ),
            None,
        )
        .await;
    assert!(updated.starts_with("HTTP/1.1 200 OK"), "{updated}");
    let updated = json_body(&updated);
    let job = &updated["job"];
    assert_eq!(job["id"], id);
    assert_eq!(job["enabled"], true);
    assert_eq!(job["schedule"], "15 * * * *");
    assert!(job["next_run"].is_string());
    assert_eq!(job["repeat"], 4);
    assert_eq!(job["skills"], serde_json::json!(["daily-summary"]));
    assert_eq!(
        job["deliver"],
        serde_json::json!({"target": "matrix", "platform": "fixture-room"})
    );
    let saved = state.roundtrip(empty_request("GET", &path), None).await;
    assert!(saved.starts_with("HTTP/1.1 200 OK"), "{saved}");
    assert_eq!(json_body(&saved)["job"], *job);
    let store = FileJobPersistence::with_dir(state._cron_dir.path().join("cron"));
    let persisted = store.load_jobs().await.expect("persisted job");
    let [saved_job] = persisted.as_slice() else {
        panic!("expected one persisted job");
    };
    assert_eq!(api_cron_job_body(saved_job), *job);
    assert_eq!(rx.try_recv().unwrap_err(), mpsc::error::TryRecvError::Empty);
}
