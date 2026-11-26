use super::*;
use std::collections::HashMap;

fn map_env(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn prometheus_text_includes_counters() {
    record_http_request();
    record_llm_request();
    record_http_reject();
    let t = prometheus_text();
    assert!(t.contains("hermes_http_requests_total"));
    assert!(t.contains("hermes_llm_requests_total"));
    assert!(t.contains("hermes_http_rejects_total"));
}

#[test]
fn langfuse_config_resolves_endpoint_headers_and_resource_attrs() {
    let env = map_env(&[
        ("HERMES_LANGFUSE_PUBLIC_KEY", "pk-lf-public"),
        ("HERMES_LANGFUSE_SECRET_KEY", "sk-lf-secret"),
        ("HERMES_LANGFUSE_BASE_URL", "https://us.cloud.langfuse.com/"),
        ("HERMES_LANGFUSE_ENV", "local"),
        ("HERMES_LANGFUSE_RELEASE", "v1.2.3"),
        ("HERMES_LANGFUSE_SAMPLE_RATE", "0.25"),
    ]);
    let cfg = resolve_langfuse_trace_config(|name| env.get(name).cloned())
        .expect("valid langfuse config")
        .expect("configured");

    assert_eq!(
        cfg.endpoint,
        "https://us.cloud.langfuse.com/api/public/otel/v1/traces"
    );
    assert_eq!(cfg.sample_rate, Some(0.25));
    let headers = cfg.otlp_headers();
    assert!(headers
        .iter()
        .any(|(k, v)| k == "Authorization" && v.starts_with("Basic ")));
    assert!(headers
        .iter()
        .any(|(k, v)| { k == "x-langfuse-ingestion-version" && v == LANGFUSE_INGESTION_VERSION }));
    assert!(cfg
        .resource_attributes()
        .contains(&("deployment.environment.name".into(), "local".into())));
    assert!(cfg
        .resource_attributes()
        .contains(&("service.version".into(), "v1.2.3".into())));
}

#[test]
fn langfuse_config_rejects_missing_or_placeholder_credentials() {
    let env = map_env(&[
        ("HERMES_LANGFUSE_PUBLIC_KEY", "placeholder"),
        ("HERMES_LANGFUSE_SECRET_KEY", "test-secret"),
    ]);
    let err = resolve_langfuse_trace_config(|name| env.get(name).cloned())
        .expect_err("placeholder credentials rejected");
    assert!(err
        .issues
        .iter()
        .any(|issue| issue.contains("expected \"pk-lf-\" prefix")));
    assert!(err
        .issues
        .iter()
        .any(|issue| issue.contains("expected \"sk-lf-\" prefix")));

    let env = map_env(&[("HERMES_LANGFUSE_PUBLIC_KEY", "pk-lf-public")]);
    let err = resolve_langfuse_trace_config(|name| env.get(name).cloned())
        .expect_err("half config rejected");
    assert!(err
        .to_string()
        .contains("missing HERMES_LANGFUSE_SECRET_KEY"));
}

#[test]
fn langfuse_endpoint_normalizes_full_otel_paths() {
    assert_eq!(
        langfuse_otlp_traces_endpoint("https://cloud.langfuse.com"),
        "https://cloud.langfuse.com/api/public/otel/v1/traces"
    );
    assert_eq!(
        langfuse_otlp_traces_endpoint("https://cloud.langfuse.com/api/public/otel"),
        "https://cloud.langfuse.com/api/public/otel/v1/traces"
    );
    assert_eq!(
        langfuse_otlp_traces_endpoint("https://cloud.langfuse.com/api/public/otel/v1/traces"),
        "https://cloud.langfuse.com/api/public/otel/v1/traces"
    );
}

#[test]
fn sample_rate_and_otlp_header_parsing_are_tolerant() {
    assert_eq!(parse_sample_rate("1.5"), Some(1.0));
    assert_eq!(parse_sample_rate("-2"), Some(0.0));
    assert_eq!(parse_sample_rate("nan"), None);
    assert_eq!(parse_sample_rate("garbage"), None);

    assert_eq!(
        parse_otlp_headers("Authorization=Basic abc, x-langfuse-ingestion-version=4,broken"),
        vec![
            ("Authorization".into(), "Basic abc".into()),
            ("x-langfuse-ingestion-version".into(), "4".into())
        ]
    );
}

fn assert_explicit_otlp_endpoint() {
    let export = trace_export_from_env();
    assert_eq!(
        export.endpoint.as_deref(),
        Some("http://collector.test:4318")
    );
    assert_eq!(
        export.headers,
        vec![("X-Telemetry-Test".into(), "fixture".into())]
    );
    assert!(export.resource_attributes.is_empty());
}

#[test]
fn telemetry_environment_controls_export_and_logging() {
    const CASE: &str = "HERMES_TEST_TELEMETRY_CASE";
    if let Some(case) = std::env::var_os(CASE) {
        match case.to_str().unwrap() {
            "explicit" => {
                // An explicit collector takes precedence even if Langfuse is set.
                assert_explicit_otlp_endpoint();
                #[cfg(not(feature = "otlp"))]
                init_telemetry_from_env("telemetry-test", "info");
            }
            "langfuse" => {
                let export = trace_export_from_env();
                assert_eq!(
                    export.endpoint.as_deref(),
                    Some("https://traces.example.test/api/public/otel/v1/traces")
                );
                assert_eq!(export.sample_rate, Some(0.25));
                assert!(export
                    .resource_attributes
                    .contains(&("deployment.environment.name".into(), "local".into())));
            }
            _ => {
                let export = trace_export_from_env();
                assert_eq!(export.endpoint, None);
                assert!(export.headers.is_empty());
                init_telemetry_from_env("telemetry-test", "info");
                tracing::info!("isolated logging test");
            }
        }
        return;
    }

    let valid_langfuse = [
        ("HERMES_LANGFUSE_PUBLIC_KEY", "pk-lf-fixture-public"),
        ("HERMES_LANGFUSE_SECRET_KEY", "sk-lf-fixture-secret"),
        ("HERMES_LANGFUSE_BASE_URL", "https://traces.example.test"),
        ("HERMES_LANGFUSE_ENV", "local"),
        ("HERMES_LANGFUSE_SAMPLE_RATE", "0.25"),
    ];
    let explicit = [
        ("HERMES_OTLP_ENDPOINT", "http://collector.test:4318"),
        ("HERMES_OTLP_HEADERS", "X-Telemetry-Test=fixture"),
    ];
    let invalid = [("HERMES_LANGFUSE_SECRET_KEY", "sk-lf-fixture-secret")];
    for (case, vars, additional) in [
        ("explicit", &valid_langfuse[..], &explicit[..]),
        ("langfuse", &valid_langfuse[..], &[][..]),
        ("invalid", &invalid[..], &[("HERMES_LOG_JSON", "1")][..]),
        ("unconfigured", &[][..], &[][..]),
    ] {
        // Re-run this test binary with only synthetic environment variables.
        let mut command = std::process::Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
        command
            .env_clear()
            .env(CASE, case)
            .envs(vars.iter().chain(additional).copied())
            .args([
                "--exact",
                "tests::telemetry_environment_controls_export_and_logging",
                "--nocapture",
            ]);
        for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let output = command.output().expect("run isolated telemetry test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("1 passed;"),
            "telemetry case {case} failed:\n{stdout}\n{stderr}"
        );
        if case == "invalid" {
            assert!(
                stderr.contains("missing HERMES_LANGFUSE_PUBLIC_KEY"),
                "{stderr}"
            );
            assert!(
                stderr.contains(r#""message":"isolated logging test""#),
                "{stderr}"
            );
        }
        #[cfg(not(feature = "otlp"))]
        if case == "explicit" {
            assert!(
                stderr.contains("OTLP endpoint is set but this build lacks"),
                "{stderr}"
            );
        }
    }
}

#[test]
fn recorded_tool_and_cache_counters_are_observable() {
    let before = snapshot();
    record_tool_call(Duration::from_millis(137));
    record_error();
    record_prompt_cache_hit();
    record_prompt_cache_miss();
    let after = snapshot();
    assert_eq!(after.tool_calls_total, before.tool_calls_total + 1);
    assert_eq!(after.tool_time_ms_total, before.tool_time_ms_total + 137);
    assert_eq!(after.errors_total, before.errors_total + 1);
    assert_eq!(after.prompt_cache_hits, before.prompt_cache_hits + 1);
    assert_eq!(after.prompt_cache_misses, before.prompt_cache_misses + 1);
}

#[test]
fn defaults_disable_export_and_debug_output_redacts_credentials() {
    let defaults = TelemetryConfig::default();
    assert_eq!(defaults.service_name, "hermes");
    assert_eq!(defaults.level, "info");
    assert!(!defaults.json);
    assert!(defaults.otlp_endpoint.is_none());
    assert!(defaults.otlp_headers.is_empty());
    assert!(defaults.otlp_sample_rate.is_none());
    assert!(defaults.resource_attributes.is_empty());

    let env = map_env(&[
        ("HERMES_LANGFUSE_PUBLIC_KEY", "pk-lf-fixture-public"),
        ("HERMES_LANGFUSE_SECRET_KEY", "sk-lf-fixture-secret"),
    ]);
    let mut cfg = resolve_langfuse_trace_config(|name| env.get(name).cloned())
        .expect("valid Langfuse config")
        .expect("configured");
    let debug = format!("{cfg:?}");
    assert!(debug.contains("pk-lf-..."), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
    assert!(!debug.contains("fixture-public"));
    assert!(!debug.contains("fixture-secret"));
    cfg.public_key.clear();
    assert!(format!("{cfg:?}").contains("<empty>"));
}
