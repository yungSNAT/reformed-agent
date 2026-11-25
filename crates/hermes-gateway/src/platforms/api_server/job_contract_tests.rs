use hermes_cron::{FileJobPersistence, JobPersistence};

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
