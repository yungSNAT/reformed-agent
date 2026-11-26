use super::*;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ARTICLE_URL: &str = "https://docs.example.test/article";

fn local_client() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build test HTTP client")
}

fn parallel_at(root: &str) -> ParallelWebBackend {
    ParallelWebBackend {
        client: local_client(),
        api_key: Some("parallel-test-key".into()),
        rest_base_url: root.into(),
        search_mode: "basic".into(),
    }
}

fn xai_at(root: &str) -> XaiWebSearchBackend {
    XaiWebSearchBackend {
        client: local_client(),
        base_url: root.into(),
        ..XaiWebSearchBackend::new("xai-test-key".into())
    }
}

fn direct_at(root: &str) -> FirecrawlTransport {
    FirecrawlTransport::Direct {
        endpoint_root: root.into(),
        api_key: None,
    }
}

fn execution_error<T: std::fmt::Debug>(result: Result<T, ToolError>) -> String {
    match result {
        Err(ToolError::ExecutionFailed(message)) => message,
        other => panic!("expected ExecutionFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn parallel_extract_sends_full_content_request_and_joins_excerpts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .and(header("authorization", "Bearer parallel-test-key"))
        .and(body_partial_json(json!({
            "urls": [ARTICLE_URL],
            "advanced_settings": {"full_content": true}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{
                "url": ARTICLE_URL,
                "title": "Article",
                "excerpts": ["First paragraph.", {"not": "text"}, "Second paragraph."]
            }],
            "errors": [{
                "url": "https://docs.example.test/missing",
                "error_type": "not_found"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let result = parallel_at(&server.uri())
        .extract(ARTICLE_URL, false)
        .await
        .expect("extract from mock Parallel endpoint");
    let result: Value = serde_json::from_str(&result).expect("parse extract result");
    let content = "First paragraph.\n\nSecond paragraph.";
    assert_eq!(result["provider"], "parallel");
    assert_eq!(result["url"], ARTICLE_URL);
    assert_eq!(result["content"], content);
    let documents = result["results"].as_array().expect("extract documents");
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0]["content"], content);
    assert_eq!(documents[0]["raw_content"], content);
    assert_eq!(
        documents[0]["metadata"],
        json!({"sourceURL": ARTICLE_URL, "title": "Article"})
    );
    assert_eq!(documents[1]["content"], "");
    assert_eq!(documents[1]["error"], "not_found");
    assert_eq!(
        documents[1]["metadata"]["sourceURL"],
        "https://docs.example.test/missing"
    );
    assert_eq!(
        result["source_quality_summary"],
        json!(source_quality_summary(documents))
    );
}

#[test]
fn parallel_empty_extract_reports_each_requested_url() {
    let urls = [ARTICLE_URL, "https://docs.example.test/another"];
    let documents =
        normalize_parallel_extract_documents(&json!({"results": [], "errors": []}), &urls);
    assert_eq!(documents.len(), urls.len());
    for (document, url) in documents.iter().zip(urls) {
        assert_eq!(document["url"], url);
        assert_eq!(document["metadata"], json!({"sourceURL": url}));
        assert_eq!(document["title"], "");
        assert_eq!(document["content"], "");
        assert_eq!(document["error"], "extraction failed (no content returned)");
    }

    let rendered = parallel_extract_response(ARTICLE_URL, documents.clone())
        .expect("serialize the empty extraction response");
    let rendered: Value = serde_json::from_str(&rendered).expect("parse extraction response");
    assert_eq!(rendered["url"], ARTICLE_URL);
    assert_eq!(rendered["content"], "");
    assert_eq!(rendered["results"], json!(documents));
}

#[test]
fn xai_prompt_contains_query_limit_and_json_contract() {
    let prompt = XaiWebSearchBackend::build_prompt("marine weather forecasts", 7);
    assert!(prompt.contains("at most 7 results"));
    assert!(prompt.contains("absolute https:// URLs"));
    assert!(prompt.contains(r#"{"results": []}"#));
    assert!(prompt.contains(r#""description": "1-2 sentence summary""#));
    assert!(prompt.ends_with("Query: marine weather forecasts"));
}

#[test]
fn xai_prefers_usable_json_from_message_text_over_citations() {
    let no_url = r#"{"results":[{"title":"Missing URL"},{"url":"  "}]}"#;
    let fenced = "```json\n{\"results\":[{\"url\":\" https://docs.example.test/article \",\
                  \"title\":\" Article \",\"description\":\" A weather summary. \"}]}\n```";
    let annotation =
        json!({"type": "url_citation", "url": "https://other.example.test/annotation"});
    let data = json!({
        "output": [
            {"type": "reasoning", "content": [{"type": "output_text", "text": "ignore event"}]},
            {"type": "message", "content": {}},
            {"type": "message", "content": [
                {"type": "input_text", "text": "ignore input"},
                {"type": "output_text", "text": " \n ", "annotations": [annotation]},
                {"type": "output_text", "text": "{}"},
                {"type": "output_text", "text": no_url},
                {"type": "output_text", "text": fenced}
            ]}
        ],
        "citations": ["https://other.example.test/legacy"]
    });

    let (text_blocks, annotations) = collect_xai_output_text(&data);
    assert_eq!(text_blocks, ["{}", no_url, fenced]);
    assert_eq!(annotations, vec![annotation]);
    assert!(parse_xai_json_results(no_url, 1).is_empty());

    let results = XaiWebSearchBackend::parse_results(&data, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["url"], ARTICLE_URL);
    assert_eq!(results[0]["title"], "Article");
    assert_eq!(results[0]["description"], "A weather summary.");
}

#[test]
fn xai_annotation_fallback_deduplicates_urls_and_checks_utf8_boundaries() {
    let context = format!("{}é", "x".repeat(199));
    let text = format!("discard this prefix {context}");
    let second_url = "https://docs.example.test/second";
    let data = json!({
        "output": [{"type": "message", "content": [{
            "type": "output_text",
            "text": text,
            "annotations": [
                {"type": "other", "url": "https://docs.example.test/ignore"},
                {"type": "url_citation"},
                {"type": "url_citation", "url": " "},
                {"type": "url_citation", "url": format!(" {ARTICLE_URL} "), "start_index": text.len()},
                {"type": "url_citation", "url": ARTICLE_URL, "start_index": 0},
                {"type": "url_citation", "url": second_url, "start_index": text.len() - 1},
                {"type": "url_citation", "url": "https://docs.example.test/beyond-limit"}
            ]
        }]}],
        "citations": ["https://docs.example.test/legacy"]
    });

    let rows = XaiWebSearchBackend::parse_results(&data, 2);
    assert_eq!(rows.len(), 2);
    let article = rows
        .iter()
        .find(|row| row["url"] == ARTICLE_URL)
        .expect("first unique citation");
    assert_eq!(article["description"], context);
    let second = rows
        .iter()
        .find(|row| row["url"] == second_url)
        .expect("citation with an index inside a multibyte character");
    assert_eq!(second["description"], "");

    let invalid_annotations = json!({
        "output": [{"type": "message", "content": [{
            "type": "output_text",
            "annotations": [{"type": "url_citation", "url": " "}]
        }]}],
        "citations": [null, " ", ARTICLE_URL]
    });
    let rows = XaiWebSearchBackend::parse_results(&invalid_annotations, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["url"], ARTICLE_URL);
}

#[test]
fn firecrawl_explicit_keys_select_the_direct_cloud_transport() {
    let search = FirecrawlSearchBackend::new("search-test-key".into());
    let extract = FirecrawlExtractBackend::new("extract-test-key".into());
    assert_eq!(search.transport_label(), "direct");
    assert_eq!(extract.transport_label(), "direct");
    assert_eq!(search.transport.auth_bearer(), Some("search-test-key"));
    assert_eq!(extract.transport.auth_bearer(), Some("extract-test-key"));
    assert_eq!(
        search.transport.endpoint("/search"),
        format!("{FIRECRAWL_BASE_URL_DEFAULT}/v1/search")
    );
    assert_eq!(
        extract.transport.endpoint("scrape"),
        format!("{FIRECRAWL_BASE_URL_DEFAULT}/v1/scrape")
    );
    assert_eq!(
        normalize_firecrawl_endpoint_root(" /v1/// "),
        FIRECRAWL_BASE_URL_DEFAULT
    );
}

#[tokio::test]
async fn firecrawl_responses_expose_transport_and_preserve_scrape_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .and(header("authorization", "Bearer local-gateway-token"))
        .and(body_partial_json(json!({"query": "weather", "limit": 3})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"web": [{"url": ARTICLE_URL, "title": "Weather", "markdown": "Forecast"}]}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let metadata = json!({"title": "Weather", "sourceURL": ARTICLE_URL});
    let links = json!(["https://docs.example.test/forecast"]);
    Mock::given(method("POST"))
        .and(path("/v1/scrape"))
        .and(body_partial_json(json!({
            "url": ARTICLE_URL,
            "formats": ["markdown"],
            "onlyMainContent": true,
            "includeLinks": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"markdown": "# Forecast", "metadata": metadata, "links": links}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let search = FirecrawlSearchBackend {
        client: local_client(),
        transport: FirecrawlTransport::Managed {
            endpoint_root: server.uri(),
            nous_token: "local-gateway-token".into(),
        },
    };
    assert_eq!(search.transport_label(), "managed");
    let result = search
        .search("weather", 3, None)
        .await
        .expect("search result");
    let result: Value = serde_json::from_str(&result).expect("parse search result");
    assert_eq!(result["transport"], "managed");
    let rows = result["results"].as_array().expect("search rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["url"], ARTICLE_URL);
    assert_eq!(rows[0]["title"], "Weather");
    assert_eq!(rows[0]["description"], "Forecast");

    let extract = FirecrawlExtractBackend {
        client: local_client(),
        transport: direct_at(&server.uri()),
    };
    let result = extract
        .extract(ARTICLE_URL, true)
        .await
        .expect("scrape result");
    let result: Value = serde_json::from_str(&result).expect("parse scrape result");
    assert_eq!(
        result,
        json!({
            "content": "# Forecast",
            "metadata": metadata,
            "links": links,
            "transport": "direct"
        })
    );
}

#[tokio::test]
async fn provider_errors_identify_http_status_invalid_json_and_embedded_error_codes() {
    let server = MockServer::start().await;
    for (route, response) in [
        (
            "/status/v1/extract",
            ResponseTemplate::new(429).set_body_string("quota exhausted"),
        ),
        (
            "/v1/extract",
            ResponseTemplate::new(200).set_body_string("not JSON"),
        ),
        (
            "/responses",
            ResponseTemplate::new(200).set_body_string("not JSON"),
        ),
        (
            "/code/responses",
            ResponseTemplate::new(200).set_body_json(json!({"error": {"code": "model_inactive"}})),
        ),
        (
            "/v1/search",
            ResponseTemplate::new(200).set_body_string("not JSON"),
        ),
        (
            "/v1/scrape",
            ResponseTemplate::new(200).set_body_string("not JSON"),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
    }

    let root = server.uri();
    assert_eq!(
        execution_error(
            parallel_at(&format!("{root}/status"))
                .extract(ARTICLE_URL, false)
                .await
        ),
        "Parallel API error (429 Too Many Requests): quota exhausted"
    );
    assert!(
        execution_error(parallel_at(&root).extract(ARTICLE_URL, false).await)
            .starts_with("Failed to parse Parallel response:")
    );
    assert!(
        execution_error(xai_at(&root).search("weather", 1, None).await)
            .starts_with("Failed to parse xAI web search response:")
    );
    assert_eq!(
        execution_error(
            xai_at(&format!("{root}/code"))
                .search("weather", 1, None)
                .await
        ),
        "xAI returned an error: model_inactive"
    );

    let search = FirecrawlSearchBackend {
        client: local_client(),
        transport: direct_at(&root),
    };
    assert!(execution_error(search.search("weather", 1, None).await)
        .starts_with("Failed to parse Firecrawl search response:"));
    let extract = FirecrawlExtractBackend {
        client: local_client(),
        transport: direct_at(&root),
    };
    assert!(execution_error(extract.extract(ARTICLE_URL, false).await)
        .starts_with("Failed to parse Firecrawl response:"));
}

#[tokio::test]
async fn malformed_endpoints_report_provider_specific_request_failures() {
    let invalid = "http://[";
    assert!(
        execution_error(xai_at(invalid).search("weather", 1, None).await)
            .starts_with("xAI web search request failed:")
    );
    let search = FirecrawlSearchBackend {
        client: local_client(),
        transport: direct_at(invalid),
    };
    assert!(execution_error(search.search("weather", 1, None).await)
        .starts_with("Firecrawl search request failed:"));
    let extract = FirecrawlExtractBackend {
        client: local_client(),
        transport: direct_at(invalid),
    };
    assert!(execution_error(extract.extract(ARTICLE_URL, false).await)
        .starts_with("Firecrawl API request failed:"));
}

async fn serve_truncated_body(listener: TcpListener) {
    let (socket, _) = listener.accept().await.expect("accept local HTTP request");
    let mut reader = BufReader::new(socket);
    let mut content_length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(
            reader
                .read_line(&mut line)
                .await
                .expect("read request header"),
            0,
            "request ended before the header terminator"
        );
        if line == "\r\n" {
            break;
        }
        if let Some((_, value)) = line
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        {
            content_length = value.trim().parse().expect("parse request content length");
        }
    }
    let mut request_body = vec![0; content_length];
    reader
        .read_exact(&mut request_body)
        .await
        .expect("read full request before closing the socket");

    let mut socket = reader.into_inner();
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{")
        .await
        .expect("send response with an incomplete body");
    socket.shutdown().await.expect("close mock response");
}

async fn truncated_body_error<F>(call: impl FnOnce(String) -> F) -> String
where
    F: std::future::Future<Output = Result<String, ToolError>>,
{
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind an ephemeral HTTP port");
    let root = format!("http://{}", listener.local_addr().expect("mock address"));
    let ((), result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(serve_truncated_body(listener), call(root))
    })
    .await
    .expect("local request and response should complete");
    execution_error(result)
}

#[tokio::test]
async fn incomplete_http_bodies_are_reported_as_read_failures() {
    let message =
        truncated_body_error(
            |root| async move { parallel_at(&root).extract(ARTICLE_URL, false).await },
        )
        .await;
    assert!(message.starts_with("Failed to read Parallel response:"));

    let message =
        truncated_body_error(|root| async move { xai_at(&root).search("weather", 1, None).await })
            .await;
    assert!(message.starts_with("Failed to read xAI web search response:"));

    let message = truncated_body_error(|root| async move {
        FirecrawlSearchBackend {
            client: local_client(),
            transport: direct_at(&root),
        }
        .search("weather", 1, None)
        .await
    })
    .await;
    assert!(message.starts_with("Failed to read Firecrawl search response:"));

    let message = truncated_body_error(|root| async move {
        FirecrawlExtractBackend {
            client: local_client(),
            transport: direct_at(&root),
        }
        .extract(ARTICLE_URL, false)
        .await
    })
    .await;
    assert!(message.starts_with("Failed to read Firecrawl response:"));
}

const ENV_CASE: &str = "HERMES_TEST_PARALLEL_XAI_FIRECRAWL_ENV_CASE";

fn run_environment_case(case: &str, settings: &[(&str, &str)]) {
    // Give each fixture its own process so concurrent tests never see modified variables.
    let home = tempfile::tempdir().expect("temporary home");
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    command
        .arg("--exact")
        .arg(format!("{module}::isolated_environment_configuration"))
        .arg("--test-threads=1")
        .current_dir(home.path())
        .env_clear()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("XDG_DATA_HOME", home.path())
        .env("HERMES_HOME", home.path())
        .env("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "false")
        .env(ENV_CASE, case)
        .envs(settings.iter().copied());
    if let Some(root) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", root);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env(
            "LLVM_PROFILE_FILE",
            std::env::current_dir()
                .expect("current directory")
                .join(profile),
        );
    }

    let output = command.output().expect("run environment fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "environment fixture {case} ({settings:?}) failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn isolated_environment_configuration() {
    let Ok(case) = std::env::var(ENV_CASE) else {
        run_environment_case(
            "direct",
            &[
                ("HERMES_WEB_SEARCH_BACKEND", "unknown"),
                ("HERMES_WEB_EXTRACT_BACKEND", "unknown"),
                ("HERMES_WEB_CRAWL_BACKEND", "unknown"),
                ("HERMES_WEB_BACKEND", " SeArX "),
                ("PARALLEL_API_KEY", " parallel-env-key "),
                ("PARALLEL_BASE_URL", " https://parallel.example.test/// "),
                ("PARALLEL_SEARCH_MODE", " Fast "),
                (
                    "FIRECRAWL_API_URL",
                    " https://firecrawl.example.test/gateway/v1/// ",
                ),
            ],
        );
        run_environment_case(
            "xai",
            &[
                ("HERMES_WEB_BACKEND", " GrOk "),
                ("XAI_API_KEY", " xai-env-key "),
                ("XAI_BASE_URL", " http://[/// "),
                ("HERMES_WEB_XAI_MODEL", " test-model "),
                (
                    "HERMES_WEB_XAI_ALLOWED_DOMAINS",
                    "a.test, , b.test,c.test,d.test,e.test,f.test",
                ),
                ("HERMES_WEB_XAI_TIMEOUT", "not a number"),
            ],
        );
        run_environment_case(
            "conflicting-domains",
            &[
                ("HERMES_WEB_BACKEND", " disabled "),
                ("XAI_API_KEY", "xai-env-key"),
                ("HERMES_WEB_XAI_ALLOWED_DOMAINS", "a.test"),
                ("HERMES_WEB_XAI_EXCLUDED_DOMAINS", "b.test"),
            ],
        );
        run_environment_case(
            "firecrawl-factories",
            &[
                ("HERMES_WEB_SEARCH_BACKEND", "firecrawl"),
                ("HERMES_WEB_EXTRACT_BACKEND", "firecrawl"),
                ("FIRECRAWL_API_URL", "http://["),
            ],
        );
        for provider in ["exa", "tavily", "firecrawl", "xai", "searxng", "brave-free"] {
            run_environment_case("unconfigured", &[("HERMES_WEB_BACKEND", provider)]);
        }
        return;
    };

    match case.as_str() {
        "direct" => {
            let parallel = ParallelWebBackend::from_env();
            assert_eq!(parallel.api_key.as_deref(), Some("parallel-env-key"));
            assert_eq!(parallel.rest_base_url, "https://parallel.example.test");
            assert_eq!(parallel.search_mode, "basic");
            assert_eq!(search_backend_choice_from_env(), "parallel");
            assert_eq!(extract_backend_choice_from_env(), "search-only:searxng");
            assert_eq!(crawl_backend_choice_from_env(), "fallback");
            assert_eq!(normalize_search_backend_choice("unknown"), None);

            let search = FirecrawlSearchBackend::from_env_or_managed().expect("self-hosted search");
            let extract = FirecrawlExtractBackend::from_env().expect("self-hosted extract alias");
            assert_eq!(search.transport_label(), "direct");
            assert_eq!(extract.transport_label(), "direct");
            assert_eq!(search.transport.auth_bearer(), None);
            assert_eq!(extract.transport.auth_bearer(), None);
            assert_eq!(
                search.transport.endpoint("search"),
                "https://firecrawl.example.test/gateway/v1/search"
            );
            assert_eq!(
                extract.transport.endpoint("scrape"),
                "https://firecrawl.example.test/gateway/v1/scrape"
            );
        }
        "xai" => {
            let backend = XaiWebSearchBackend::from_env().expect("configured xAI backend");
            assert_eq!(backend.api_key, "xai-env-key");
            assert_eq!(backend.base_url(), "http://[");
            assert_eq!(backend.model(), "test-model");
            assert_eq!(
                backend.allowed_domains,
                ["a.test", "b.test", "c.test", "d.test", "e.test"]
            );
            assert!(backend.excluded_domains.is_empty());
            assert_eq!(backend.timeout_secs, XAI_WEB_TIMEOUT_SECS_DEFAULT);
            assert_eq!(search_backend_choice_from_env(), "xai");
            assert_eq!(extract_backend_choice_from_env(), "search-only:xai");
            assert!(execution_error(
                search_backend_from_env_or_fallback()
                    .search("weather", 1, None)
                    .await
            )
            .starts_with("xAI web search request failed:"));
        }
        "conflicting-domains" => {
            assert_eq!(extract_backend_choice_from_env(), "simple");
            assert_eq!(
                execution_error(XaiWebSearchBackend::from_env().map(|_| ())),
                "HERMES_WEB_XAI_ALLOWED_DOMAINS and HERMES_WEB_XAI_EXCLUDED_DOMAINS cannot both be set"
            );
        }
        "firecrawl-factories" => {
            assert!(execution_error(
                search_backend_from_env_or_fallback()
                    .search("weather", 1, None)
                    .await
            )
            .starts_with("Firecrawl search request failed:"));
            assert!(execution_error(
                extract_backend_from_env_or_fallback()
                    .extract(ARTICLE_URL, true)
                    .await
            )
            .starts_with("Firecrawl API request failed:"));
        }
        "unconfigured" => {
            let provider = std::env::var("HERMES_WEB_BACKEND").expect("legacy provider choice");
            assert_eq!(search_backend_choice_from_env(), provider);
            assert_eq!(
                crawl_backend_choice_from_env(),
                if provider == "tavily" {
                    "tavily"
                } else {
                    "fallback"
                }
            );
            let expected = FallbackSearchBackend::new()
                .search("weather", 1, None)
                .await;
            let actual = search_backend_from_env_or_fallback()
                .search("weather", 1, None)
                .await;
            assert_eq!(
                actual.map_err(|error| error.to_string()),
                expected.map_err(|error| error.to_string())
            );

            if matches!(provider.as_str(), "exa" | "tavily" | "firecrawl") {
                let invalid_url = "http://[";
                let expected = SimpleExtractBackend::new()
                    .extract(invalid_url, false)
                    .await;
                let actual = extract_backend_from_env_or_fallback()
                    .extract(invalid_url, false)
                    .await;
                assert_eq!(
                    actual.map_err(|error| error.to_string()),
                    expected.map_err(|error| error.to_string())
                );
            }
            if provider == "xai" {
                assert_eq!(
                    execution_error(XaiWebSearchBackend::from_env().map(|_| ())),
                    "XAI_API_KEY environment variable not set for xAI web search"
                );
                let parallel = ParallelWebBackend::from_env();
                assert!(parallel.api_key.is_none());
                assert_eq!(parallel.rest_base_url, PARALLEL_BASE_URL_DEFAULT);
                assert_eq!(parallel.search_mode, "advanced");
            }
        }
        other => panic!("unknown environment fixture: {other}"),
    }
}
