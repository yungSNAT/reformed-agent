use super::*;
use std::process::Command;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use wiremock::matchers::{any, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_KEY: &str = "unit-test-key";
const DOCUMENT_URL: &str = "https://docs.rs/example";

fn execution_message<T>(result: Result<T, ToolError>) -> String {
    match result {
        Err(ToolError::ExecutionFailed(message)) => message,
        Err(error) => panic!("expected an execution failure, got {error}"),
        Ok(_) => panic!("expected an execution failure, got success"),
    }
}

fn parse_output(result: Result<String, ToolError>) -> Value {
    serde_json::from_str(&result.unwrap()).unwrap()
}

#[tokio::test]
#[allow(clippy::default_constructed_unit_structs)]
async fn fallback_defaults_explain_which_configuration_is_missing() {
    let search = FallbackSearchBackend::default();
    let output = parse_output(search.search("async Rust", 2, None).await);
    assert_eq!(output["error"], "no_api_key");
    assert_eq!(output["query"], "async Rust");
    let message = output["message"].as_str().unwrap();
    assert!(message.contains("EXA_API_KEY"));
    assert!(message.contains("Query was: async Rust"));

    let crawl = FallbackCrawlBackend::default();
    let output = parse_output(
        crawl
            .crawl(DOCUMENT_URL, Some("read docs"), "basic", 2)
            .await,
    );
    assert_eq!(output["success"], false);
    assert_eq!(output["error"], "no_api_key");
    assert_eq!(
        output["results"],
        json!([{
            "url": DOCUMENT_URL,
            "title": "",
            "content": "",
            "error": "TAVILY_API_KEY environment variable not set"
        }])
    );
}

#[tokio::test]
async fn search_only_extract_reports_provider_and_source_quality() {
    let backend = SearchOnlyExtractBackend::new("example-search");
    for (url, label, score, reason) in [
        (" \t", "secondary", 0.20, "missing URL"),
        ("not a URL", "secondary", 0.35, "unparseable URL"),
        (
            "https://github.com/example/project/issues/7",
            "community",
            0.78,
            "repository discussion",
        ),
    ] {
        let output = parse_output(backend.extract(url, true).await);
        assert_eq!(
            output,
            json!({
                "success": false,
                "error": "example-search is a search-only web backend and cannot extract URLs",
                "url": url,
                "provider": "example-search",
                "source_quality": label,
                "source_quality_score": score,
                "source_quality_reason": reason,
            })
        );
    }
}

#[tokio::test]
async fn extractors_reject_secret_query_parameters_before_sending() {
    let url = "https://example.invalid/?api%5Fkey=fixture-secret";
    let simple = SimpleExtractBackend::default();
    let search_only = SearchOnlyExtractBackend::new("example-search");

    for result in [
        simple.extract(url, false).await,
        search_only.extract(url, false).await,
    ] {
        assert!(
            matches!(
                &result,
                Err(ToolError::InvalidParams(message))
                    if message.contains("query parameter 'api_key'")
                        && !message.contains("fixture-secret")
            ),
            "{result:?}"
        );
    }
}

#[test]
fn html_stripping_removes_script_and_style_bodies_and_collapses_spaces() {
    assert_eq!(
        strip_html_tags(
            " \n<p>Read <b>the docs</b></p>\t<SCRIPT type='text/javascript'>ignore()</SCRIPT>\
             <STYLE>.hidden { color: red; }</STYLE>\n <a href='/guide'>café guide</a>  "
        ),
        "Read the docs café guide"
    );
    assert_eq!(strip_html_tags("<style>nothing visible</style>"), "");
}

#[test]
fn provider_constructors_default_blank_bases_and_preserve_explicit_endpoints() {
    assert_eq!(
        ExaSearchBackend::new(TEST_KEY.into()).base_url(),
        "https://api.exa.ai"
    );
    assert_eq!(
        ExaSearchBackend::with_base_url(TEST_KEY.into(), " /// ".into()).base_url(),
        "https://api.exa.ai"
    );
    assert_eq!(
        ExaExtractBackend::new(TEST_KEY.into()).base_url(),
        "https://api.exa.ai"
    );
    assert_eq!(
        ExaExtractBackend::with_base_url(TEST_KEY.into(), " /// ".into()).base_url(),
        "https://api.exa.ai"
    );

    assert_eq!(
        TavilySearchBackend::new(TEST_KEY.into(), " /// ".into()).base_url(),
        "https://api.tavily.com"
    );
    assert_eq!(
        TavilyExtractBackend::new(TEST_KEY.into(), " /// ".into()).base_url(),
        "https://api.tavily.com"
    );
    assert_eq!(
        TavilyCrawlBackend::new(TEST_KEY.into(), " /// ".into()).base_url(),
        "https://api.tavily.com"
    );

    let searxng = SearxngSearchBackend::new(" https://searxng.invalid/root/// ".into());
    assert_eq!(searxng.endpoint(), "https://searxng.invalid/root/search");

    let brave = BraveFreeSearchBackend::new(TEST_KEY.into(), "https://brave.invalid/custom".into());
    assert_eq!(brave.endpoint(), "https://brave.invalid/custom");
    let ddg = DuckDuckGoSearchBackend::new("https://ddg.invalid/custom".into());
    assert_eq!(ddg.endpoint(), "https://ddg.invalid/custom");
    assert_eq!(ddg.timeout(), Duration::from_secs(15));
}

fn assert_provider_env_errors(problem: &str, searxng_message: &str) {
    for (message, variable) in [
        (
            execution_message(ExaSearchBackend::from_env()),
            "EXA_API_KEY",
        ),
        (
            execution_message(ExaExtractBackend::from_env()),
            "EXA_API_KEY",
        ),
        (
            execution_message(TavilySearchBackend::from_env()),
            "TAVILY_API_KEY",
        ),
        (
            execution_message(TavilyExtractBackend::from_env()),
            "TAVILY_API_KEY",
        ),
        (
            execution_message(TavilyCrawlBackend::from_env()),
            "TAVILY_API_KEY",
        ),
        (
            execution_message(BraveFreeSearchBackend::from_env()),
            "BRAVE_SEARCH_API_KEY",
        ),
    ] {
        assert_eq!(
            message,
            format!("{variable} environment variable {problem}")
        );
    }
    assert_eq!(
        execution_message(SearxngSearchBackend::from_env()),
        searxng_message
    );
}

#[test]
#[ignore = "run by provider_environment_cases in an isolated process"]
fn provider_env_fixture() {
    let case = std::env::var("HERMES_WEB_COVERAGE_CASE").unwrap();
    let (exa_url, tavily_url, searxng_url, brave_url) = match case.as_str() {
        "missing" => {
            assert_provider_env_errors(
                "not set",
                "SEARXNG_BASE_URL or SEARXNG_URL environment variable not set",
            );
            return;
        }
        "blank" => {
            assert_provider_env_errors(
                "is empty",
                "SEARXNG_BASE_URL/SEARXNG_URL environment variable is empty",
            );
            return;
        }
        "defaults" => (
            "https://api.exa.ai",
            "https://api.tavily.com",
            "https://alternate.invalid/root",
            "https://api.search.brave.com/res/v1/web/search",
        ),
        "overrides" => (
            "https://exa.invalid/root",
            "https://tavily.invalid/root",
            "https://searxng.invalid/root",
            "https://brave.invalid/custom",
        ),
        _ => panic!("unexpected environment test case: {case}"),
    };

    let exa_search = ExaSearchBackend::from_env().unwrap();
    let exa_extract = ExaExtractBackend::from_env().unwrap();
    let tavily_search = TavilySearchBackend::from_env().unwrap();
    let tavily_extract = TavilyExtractBackend::from_env().unwrap();
    let tavily_crawl = TavilyCrawlBackend::from_env().unwrap();
    let searxng = SearxngSearchBackend::from_env().unwrap();
    let brave = BraveFreeSearchBackend::from_env().unwrap();

    assert_eq!(exa_search.base_url(), exa_url);
    assert_eq!(exa_extract.base_url(), exa_url);
    assert_eq!(tavily_search.base_url(), tavily_url);
    assert_eq!(tavily_extract.base_url(), tavily_url);
    assert_eq!(tavily_crawl.base_url(), tavily_url);
    assert_eq!(searxng.base_url(), searxng_url);
    assert_eq!(brave.endpoint(), brave_url);
    for key in [
        exa_search.api_key,
        exa_extract.api_key,
        tavily_search.api_key,
        tavily_extract.api_key,
        tavily_crawl.api_key,
        brave.api_key,
    ] {
        assert_eq!(key, TEST_KEY);
    }
}

#[test]
fn provider_environment_cases() {
    let executable = std::env::current_exe().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let fixture = format!("{module}::provider_env_fixture");
    let home = tempfile::tempdir().unwrap();
    let key_variables = ["EXA_API_KEY", "TAVILY_API_KEY", "BRAVE_SEARCH_API_KEY"];

    for case in ["missing", "blank", "defaults", "overrides"] {
        let mut command = Command::new(&executable);
        command
            .args(["--ignored", "--exact", fixture.as_str()])
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("HERMES_WEB_COVERAGE_CASE", case);
        for variable in key_variables.into_iter().chain([
            "EXA_BASE_URL",
            "TAVILY_BASE_URL",
            "SEARXNG_BASE_URL",
            "SEARXNG_URL",
            "BRAVE_SEARCH_URL",
        ]) {
            command.env_remove(variable);
        }
        if case == "blank" {
            for variable in key_variables.into_iter().chain(["SEARXNG_BASE_URL"]) {
                command.env(variable, " \t ");
            }
        } else if matches!(case, "defaults" | "overrides") {
            for variable in key_variables {
                command.env(variable, format!("  {TEST_KEY}\t"));
            }
            command.env("SEARXNG_URL", " https://alternate.invalid/root/// ");
            if case == "overrides" {
                command
                    .env("EXA_BASE_URL", " https://exa.invalid/root/// ")
                    .env("TAVILY_BASE_URL", " https://tavily.invalid/root/// ")
                    .env("SEARXNG_BASE_URL", " https://searxng.invalid/root/// ")
                    .env("BRAVE_SEARCH_URL", "https://brave.invalid/custom");
            }
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "environment case {case} did not pass:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn fixture_client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

struct ProviderFixtures {
    exa_search: ExaSearchBackend,
    exa_extract: ExaExtractBackend,
    tavily_search: TavilySearchBackend,
    tavily_extract: TavilyExtractBackend,
    tavily_crawl: TavilyCrawlBackend,
    searxng: SearxngSearchBackend,
    brave: BraveFreeSearchBackend,
    ddg: DuckDuckGoSearchBackend,
}

impl ProviderFixtures {
    fn new(base: &str) -> Self {
        let client = fixture_client();
        macro_rules! keyed_backend {
            ($backend:ident, $suffix:literal) => {
                $backend {
                    client: client.clone(),
                    api_key: TEST_KEY.into(),
                    base_url: format!("{base}/{}", $suffix),
                }
            };
        }
        Self {
            exa_search: keyed_backend!(ExaSearchBackend, "exa"),
            exa_extract: keyed_backend!(ExaExtractBackend, "exa"),
            tavily_search: keyed_backend!(TavilySearchBackend, "tavily"),
            tavily_extract: keyed_backend!(TavilyExtractBackend, "tavily"),
            tavily_crawl: keyed_backend!(TavilyCrawlBackend, "tavily"),
            searxng: SearxngSearchBackend {
                client: client.clone(),
                base_url: format!("{base}/searxng"),
            },
            brave: BraveFreeSearchBackend {
                client: client.clone(),
                api_key: TEST_KEY.into(),
                endpoint: format!("{base}/brave"),
            },
            ddg: DuckDuckGoSearchBackend {
                client,
                endpoint: format!("{base}/ddg"),
                timeout: Duration::from_secs(5),
            },
        }
    }

    async fn assert_errors(&self, prefixes: [&str; 8]) {
        let results = [
            self.exa_search.search("guide", 2, Some("research")).await,
            self.exa_extract.extract(DOCUMENT_URL, false).await,
            self.tavily_search.search("guide", 2, Some(" News ")).await,
            self.tavily_extract.extract(DOCUMENT_URL, false).await,
            self.tavily_crawl
                .crawl(DOCUMENT_URL, None, "basic", 2)
                .await,
            self.searxng.search("guide", 2, Some(" science ")).await,
            self.brave.search("guide", 2, None).await,
            self.ddg.search("guide", 2, None).await,
        ];
        for (result, prefix) in results.into_iter().zip(prefixes) {
            let message = execution_message(result);
            assert!(
                message.starts_with(prefix),
                "expected {prefix:?}, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn providers_report_request_errors_for_unsupported_url_schemes() {
    ProviderFixtures::new("file:///offline-fixture")
        .assert_errors([
            "Exa API request failed:",
            "Exa extract request failed:",
            "Tavily API request failed:",
            "Tavily extract request failed:",
            "Tavily crawl request failed:",
            "SearXNG API request failed:",
            "Brave Search request failed:",
            "DuckDuckGo search request failed:",
        ])
        .await;
}

#[tokio::test]
async fn providers_report_invalid_json_from_successful_http_responses() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
        .expect(8)
        .mount(&server)
        .await;
    ProviderFixtures::new(&server.uri())
        .assert_errors([
            "Failed to parse Exa response:",
            "Failed to parse Exa extract response:",
            "Failed to parse Tavily response:",
            "Failed to parse Tavily extract response:",
            "Failed to parse Tavily crawl response:",
            "Failed to parse SearXNG response:",
            "Failed to parse Brave Search response:",
            "Failed to parse DuckDuckGo response:",
        ])
        .await;
}

#[tokio::test]
async fn providers_distinguish_incomplete_response_bodies_from_parse_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..8 {
            let (socket, _) = listener.accept().await.unwrap();
            let mut request = BufReader::new(socket);
            let mut body_length = 0;
            let mut line = String::new();
            loop {
                line.clear();
                assert_ne!(request.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        body_length = value.trim().parse().unwrap();
                    }
                }
            }
            // Drain the request before closing, so the client sees a short body rather
            // than a connection reset while it is still sending its request.
            request.read_exact(&mut vec![0; body_length]).await.unwrap();
            request
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
            request.get_mut().shutdown().await.unwrap();
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        ProviderFixtures::new(&base)
            .assert_errors([
                "Failed to read Exa response:",
                "Failed to read Exa extract response:",
                "Failed to read Tavily response:",
                "Failed to read Tavily extract response:",
                "Failed to read Tavily crawl response:",
                "Failed to read SearXNG response:",
                "Failed to read Brave Search response:",
                "Failed to read DuckDuckGo response:",
            ])
            .await;
        server.await.unwrap();
    })
    .await
    .expect("the loopback fixture should finish without waiting for a request timeout");
}

#[tokio::test]
async fn searches_map_provider_descriptions_and_rank_primary_sources_first() {
    let server = MockServer::start().await;
    for (verb, route, description_field) in [
        ("POST", "/exa/search", "text"),
        ("POST", "/tavily/search", "raw_content"),
        ("GET", "/searxng/search", "snippet"),
    ] {
        let response = json!({
            "results": [
                {"title": "Article", "url": "https://example.invalid/blog/guide", "score": 0.99},
                {
                    "title": "Reference",
                    "url": DOCUMENT_URL,
                    "score": 0.25,
                    (description_field): "The API reference"
                }
            ]
        });
        let mut mock = Mock::given(method(verb)).and(path(route));
        if verb == "GET" {
            mock = mock.and(query_param("categories", "science"));
        }
        mock.respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
    }

    let providers = ProviderFixtures::new(&server.uri());
    for result in [
        providers.exa_search.search("guide", 2, None).await,
        providers.tavily_search.search("guide", 2, None).await,
        providers
            .searxng
            .search("guide", 2, Some(" science "))
            .await,
    ] {
        let output = parse_output(result);
        let rows = output["results"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["title"], "Reference");
        assert_eq!(rows[0]["url"], DOCUMENT_URL);
        assert_eq!(rows[0]["description"], "The API reference");
        assert_eq!(rows[0]["score"], json!(0.25));
        assert_eq!(rows[0]["position"], 1);
        assert_eq!(rows[0]["original_position"], 2);
        assert_eq!(rows[1]["title"], "Article");
        assert_eq!(rows[1]["position"], 2);
        assert_eq!(
            output["source_quality_summary"],
            json!({"primary": 1, "community": 0, "secondary": 1})
        );
    }
}

#[tokio::test]
async fn extracts_and_crawl_preserve_content_and_provider_failures() {
    let server = MockServer::start().await;
    let tavily_response = json!({
        "results": [{"title": "Guide", "raw_content": "Tavily document"}],
        "failed_results": [{"url": "https://example.invalid/missing", "error": "not found"}],
        "failed_urls": ["https://example.invalid/unavailable"]
    });
    for (route, response) in [
        (
            "/exa/contents",
            json!({"results": [{"url": DOCUMENT_URL, "title": "Guide", "text": "Exa document"}]}),
        ),
        ("/tavily/extract", tavily_response.clone()),
        ("/tavily/crawl", tavily_response),
    ] {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
    }

    let providers = ProviderFixtures::new(&server.uri());
    let exa = parse_output(providers.exa_extract.extract(DOCUMENT_URL, false).await);
    assert_eq!(exa["provider"], "exa");
    assert_eq!(exa["content"], "Exa document");
    assert_eq!(exa["results"][0]["metadata"]["sourceURL"], DOCUMENT_URL);

    let extract = parse_output(providers.tavily_extract.extract(DOCUMENT_URL, true).await);
    let crawl = parse_output(
        providers
            .tavily_crawl
            .crawl(DOCUMENT_URL, Some("follow guides"), "advanced", 3)
            .await,
    );
    assert_eq!(extract["content"], "Tavily document");
    for output in [extract, crawl] {
        assert_eq!(output["provider"], "tavily");
        assert_eq!(output["url"], DOCUMENT_URL);
        let rows = output["results"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["url"], DOCUMENT_URL);
        assert_eq!(rows[0]["content"], "Tavily document");
        assert_eq!(rows[1]["url"], "https://example.invalid/missing");
        assert_eq!(rows[1]["error"], "not found");
        assert_eq!(rows[2]["url"], "https://example.invalid/unavailable");
        assert_eq!(rows[2]["error"], "extraction failed");
        assert_eq!(
            output["source_quality_summary"],
            json!({"primary": 1, "community": 0, "secondary": 2})
        );
    }
}

#[tokio::test]
async fn brave_and_duckduckgo_return_ranked_results_with_provider_names() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/brave"))
        .and(query_param("count", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "web": {"results": [
                {"title": "Article", "url": "https://example.invalid/blog/guide"},
                {"title": "Reference", "url": DOCUMENT_URL, "description": "Brave description"}
            ]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ddg"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "AbstractURL": DOCUMENT_URL,
            "Heading": "Reference",
            "AbstractText": "DuckDuckGo abstract",
            "RelatedTopics": [
                {"Text": "No URL"},
                {"Topics": null},
                {"Topics": [{"FirstURL": "https://example.invalid/blog/related", "Text": "Related - an article"}]}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let providers = ProviderFixtures::new(&server.uri());
    let brave = parse_output(providers.brave.search("guide", 1, None).await);
    assert_eq!(brave["provider"], "brave-free");
    let rows = brave["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["url"], DOCUMENT_URL);
    assert_eq!(rows[0]["description"], "Brave description");

    let ddg = parse_output(providers.ddg.search("guide", 5, None).await);
    assert_eq!(ddg["provider"], "ddgs");
    let rows = ddg["results"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["url"], DOCUMENT_URL);
    assert_eq!(rows[0]["title"], "Reference");
    assert_eq!(rows[0]["description"], "DuckDuckGo abstract");
    assert_eq!(rows[1]["title"], "Related");
    assert_eq!(rows[1]["description"], "Related - an article");
}

#[tokio::test]
async fn checked_redirects_report_request_errors_without_connecting() {
    let url = "file:///offline-fixture";
    let error = execution_message(
        get_with_checked_redirects(
            &fixture_client(),
            url,
            tokio::time::Instant::now() + EXTRACT_TIMEOUT,
            |checked_url| async move {
                assert_eq!(checked_url, url);
                Ok(())
            },
        )
        .await,
    );
    assert!(
        error.starts_with(&format!("Failed to fetch '{url}':")),
        "{error}"
    );
}

#[tokio::test]
async fn checked_redirects_distinguish_missing_and_invalid_locations() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(302).set_body_string("no location"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/invalid"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "http://["))
        .expect(1)
        .mount(&server)
        .await;

    let client = fixture_client();
    let base = server.uri();
    let response = get_with_checked_redirects(
        &client,
        &format!("{base}/missing"),
        tokio::time::Instant::now() + EXTRACT_TIMEOUT,
        |_| async { Ok(()) },
    )
    .await
    .unwrap();
    assert_eq!(response.status().as_u16(), 302);
    assert_eq!(response.text().await.unwrap(), "no location");

    let invalid_url = format!("{base}/invalid");
    let error = execution_message(
        get_with_checked_redirects(
            &client,
            &invalid_url,
            tokio::time::Instant::now() + EXTRACT_TIMEOUT,
            |_| async { Ok(()) },
        )
        .await,
    );
    assert!(
        error.starts_with(&format!(
            "Invalid redirect from '{invalid_url}' to 'http://[':"
        )),
        "{error}"
    );
}
