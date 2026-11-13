//! Web tools: web_search, web_extract, and web_crawl.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// Backend traits (injectable by the caller)
// ---------------------------------------------------------------------------

/// Backend for web search operations (e.g. Exa API).
#[async_trait]
pub trait WebSearchBackend: Send + Sync {
    /// Search the web and return results as JSON string.
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
    ) -> Result<String, ToolError>;
}

/// Backend for web extraction operations (e.g. Firecrawl API).
#[async_trait]
pub trait WebExtractBackend: Send + Sync {
    /// Extract content from a URL and return results as a string.
    async fn extract(&self, url: &str, include_links: bool) -> Result<String, ToolError>;
}

/// Backend for web crawl operations.
#[async_trait]
pub trait WebCrawlBackend: Send + Sync {
    /// Crawl a seed URL and return results as a JSON string.
    async fn crawl(
        &self,
        url: &str,
        instructions: Option<&str>,
        depth: &str,
        limit: usize,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// WebSearchHandler
// ---------------------------------------------------------------------------

/// Web search tool using the configured backend (Exa/Tavily/SearXNG).
pub struct WebSearchHandler {
    backend: Box<dyn WebSearchBackend>,
}

impl WebSearchHandler {
    pub fn new(backend: Box<dyn WebSearchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WebSearchHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'query' parameter".into()))?;

        let num_results = params
            .get("num_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let category = params.get("category").and_then(|v| v.as_str());

        self.backend.search(query, num_results, category).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "query".into(),
            json!({
                "type": "string",
                "description": "The search query. Prefer precise queries that target official docs, source repositories, primary data, standards, papers, or high-signal expert/community threads before generic summaries."
            }),
        );
        props.insert(
            "num_results".into(),
            json!({
                "type": "integer",
                "description": "Number of results to return (default: 10)",
                "default": 10
            }),
        );
        props.insert("category".into(), json!({
            "type": "string",
            "description": "Optional category filter (e.g. 'research paper', 'news', 'github')",
            "enum": ["research paper", "news", "github", "tweet", "movie", "song", "personal site", "pdf"]
        }));

        tool_schema(
            "web_search",
            "Search the web using the configured provider (Exa/Tavily/SearXNG). Returns titles, URLs, and snippets. For research tasks, gather primary/official/repository/community sources first and follow the best results with web_extract when detail matters.",
            JsonSchema::object(props, vec!["query".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// WebCrawlHandler
// ---------------------------------------------------------------------------

/// Web crawl tool using the configured backend.
pub struct WebCrawlHandler {
    backend: Box<dyn WebCrawlBackend>,
}

impl WebCrawlHandler {
    pub fn new(backend: Box<dyn WebCrawlBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WebCrawlHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'url' parameter".into()))?;

        let instructions = params.get("instructions").and_then(|v| v.as_str());
        let depth = params
            .get("depth")
            .and_then(|v| v.as_str())
            .unwrap_or("basic");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        crate::tools::url_safety::ensure_url_fetch_allowed(url).await?;
        let output = self.backend.crawl(url, instructions, depth, limit).await?;
        withhold_blocklisted_pages(output)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "The seed URL to crawl"
            }),
        );
        props.insert(
            "instructions".into(),
            json!({
                "type": "string",
                "description": "Optional natural-language crawl guidance. Prefer instructions that keep crawl scope on official docs, repositories, standards, papers, or expert/community pages relevant to the claim being verified."
            }),
        );
        props.insert(
            "depth".into(),
            json!({
                "type": "string",
                "description": "Extraction depth for providers that support it",
                "enum": ["basic", "advanced"],
                "default": "basic"
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "description": "Maximum number of pages to crawl (default: 20)",
                "default": 20
            }),
        );

        tool_schema(
            "web_crawl",
            "Crawl a seed URL using the configured provider. Returns normalized page documents and per-page errors. Use for source-grounded research when one high-value source has linked subpages worth verifying.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// WebExtractHandler
// ---------------------------------------------------------------------------

/// Web extraction tool using Firecrawl API.
pub struct WebExtractHandler {
    backend: Box<dyn WebExtractBackend>,
}

impl WebExtractHandler {
    pub fn new(backend: Box<dyn WebExtractBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WebExtractHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'url' parameter".into()))?;

        let include_links = params
            .get("include_links")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        crate::tools::url_safety::ensure_url_fetch_allowed(url).await?;
        let output = self.backend.extract(url, include_links).await?;
        withhold_blocklisted_pages(output)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "The URL to extract content from. Prefer the highest-value primary, official, repository, paper, or expert/community result rather than SEO summaries."
            }),
        );
        props.insert(
            "include_links".into(),
            json!({
                "type": "boolean",
                "description": "Whether to include links found on the page (default: true)",
                "default": true
            }),
        );

        tool_schema(
            "web_extract",
            "Extract clean content from a web page using Firecrawl. Returns page text and optional links; use this to verify details from primary/official/repository/community sources before final synthesis.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

/// Remote providers follow redirects and crawl links themselves, so only the
/// seed URL is checked before the call. With the website blocklist on, each
/// page in `results` that it refuses, or whose URLs it cannot judge, is
/// replaced by an error entry, as upstream does. The top-level `content` and
/// `source_quality_summary` that the extract backends copy from `results` are
/// then recomputed. Output without `results` is a single page (Firecrawl, the
/// built-in extractor) and is judged the same way as a whole.
fn withhold_blocklisted_pages(output: String) -> Result<String, ToolError> {
    let Ok(mut value) = serde_json::from_str::<Value>(&output) else {
        return Ok(output);
    };
    if !value.is_object() {
        return Ok(output);
    }
    let policy = crate::website_policy::load_website_blocklist(None).map_err(|e| {
        ToolError::ExecutionFailed(format!("Blocked: website blocklist could not be read: {e}"))
    })?;
    if !policy.enabled {
        return Ok(output);
    }
    let Some(pages) = value.get_mut("results").and_then(Value::as_array_mut) else {
        return match withheld_page_error(&value, &policy) {
            Some(error) => to_json(&error),
            None => Ok(output),
        };
    };
    let mut kept = Vec::with_capacity(pages.len());
    let mut withheld = false;
    for page in pages.iter_mut() {
        match withheld_page_error(page, &policy) {
            Some(error) => {
                *page = error;
                withheld = true;
            }
            None => kept.push(page.clone()),
        }
    }
    if !withheld {
        return Ok(output);
    }
    let first_content = pages
        .first()
        .and_then(|page| page.get("content"))
        .cloned()
        .unwrap_or_else(|| json!(""));
    let top = value.as_object_mut().expect("checked to be an object");
    if top.contains_key("content") {
        top.insert("content".into(), first_content);
    }
    if top.contains_key("source_quality_summary") {
        top.insert(
            "source_quality_summary".into(),
            crate::backends::web::source_quality_summary(&kept),
        );
    }
    to_json(&value)
}

/// The non-empty URLs a provider page names: its own and, for Firecrawl, the
/// final and source URLs in its metadata.
fn page_urls(page: &Value) -> impl Iterator<Item = &str> + '_ {
    ["/url", "/metadata/url", "/metadata/sourceURL"]
        .into_iter()
        .filter_map(|pointer| page.pointer(pointer)?.as_str())
        .filter(|url| !url.is_empty())
}

/// The error entry that replaces `page`, or `None` to keep it: a page is
/// withheld when it names no URL, names one without a host to judge, or names
/// one the blocklist refuses.
fn withheld_page_error(
    page: &Value,
    policy: &crate::website_policy::WebsiteBlocklistPolicy,
) -> Option<Value> {
    if page_urls(page).next().is_none() {
        return Some(json!({
            "error": "Withheld: the page has no URL to check against the website blocklist",
        }));
    }
    if let Some(url) = page_urls(page).find(|url| {
        url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::is_empty))
            .unwrap_or(true)
    }) {
        return Some(json!({
            "url": url,
            "error": format!("Withheld: '{url}' has no host to check against the website blocklist"),
        }));
    }
    let block = page_urls(page)
        .find_map(|url| crate::website_policy::check_website_access_with_policy(url, policy))?;
    Some(json!({
        "url": block.url,
        "error": format!("Blocked: '{}' matches website blocklist rule '{}'", block.url, block.rule),
    }))
}

fn to_json(value: &Value) -> Result<String, ToolError> {
    serde_json::to_string_pretty(value)
        .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSearchBackend;
    #[async_trait]
    impl WebSearchBackend for MockSearchBackend {
        async fn search(
            &self,
            query: &str,
            num_results: usize,
            category: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!(
                "Results for '{}' (count: {}, category: {})",
                query,
                num_results,
                category.unwrap_or("")
            ))
        }
    }

    struct MockExtractBackend;
    #[async_trait]
    impl WebExtractBackend for MockExtractBackend {
        async fn extract(&self, url: &str, include_links: bool) -> Result<String, ToolError> {
            Ok(format!("Content from {} links={}", url, include_links))
        }
    }

    struct MockCrawlBackend;
    #[async_trait]
    impl WebCrawlBackend for MockCrawlBackend {
        async fn crawl(
            &self,
            url: &str,
            instructions: Option<&str>,
            depth: &str,
            limit: usize,
        ) -> Result<String, ToolError> {
            Ok(format!(
                "Crawl {} depth={} limit={} instructions={}",
                url,
                depth,
                limit,
                instructions.unwrap_or("")
            ))
        }
    }

    #[tokio::test]
    async fn test_web_search_schema() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "web_search");
        assert!(schema.parameters.properties.is_some());
        assert!(schema.description.contains("primary/official"));
    }

    #[tokio::test]
    async fn test_web_search_execute() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let result = handler
            .execute(json!({"query": "rust async", "num_results": 3, "category": "news"}))
            .await
            .unwrap();
        assert!(result.contains("rust async"));
        assert!(result.contains("count: 3"));
        assert!(result.contains("category: news"));
    }

    #[tokio::test]
    async fn test_web_extract_schema() {
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "web_extract");
        assert!(schema.description.contains("verify details"));
    }

    #[tokio::test]
    async fn test_web_extract_execute() {
        let _home = IsolatedHome::new();
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let result = handler
            .execute(json!({"url": "https://93.184.215.14/page", "include_links": false}))
            .await
            .unwrap();
        assert!(result.contains("93.184.215.14"));
        assert!(result.contains("links=false"));
    }

    #[tokio::test]
    async fn test_web_crawl_schema() {
        let handler = WebCrawlHandler::new(Box::new(MockCrawlBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "web_crawl");
    }

    #[tokio::test]
    async fn test_web_crawl_execute() {
        let _home = IsolatedHome::new();
        let handler = WebCrawlHandler::new(Box::new(MockCrawlBackend));
        let result = handler
            .execute(json!({
                "url": "https://93.184.215.14/",
                "instructions": "docs only",
                "depth": "advanced",
                "limit": 7
            }))
            .await
            .unwrap();
        assert!(result.contains("https://93.184.215.14/"));
        assert!(result.contains("depth=advanced"));
        assert!(result.contains("limit=7"));
    }

    #[tokio::test]
    async fn test_web_crawl_missing_params() {
        let handler = WebCrawlHandler::new(Box::new(MockCrawlBackend));
        let result = handler.execute(json!({})).await;
        assert!(result.is_err());
    }

    /// Points `HERMES_HOME` at an empty directory under the shared env lock,
    /// so the URL check reads no real `config.yaml`.
    struct IsolatedHome {
        _dir: tempfile::TempDir,
        original: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl IsolatedHome {
        fn new() -> Self {
            let lock = hermes_config::managed_gateway::test_lock::lock();
            let dir = tempfile::tempdir().unwrap();
            let original = std::env::var("HERMES_HOME").ok();
            std::env::set_var("HERMES_HOME", dir.path());
            Self {
                _dir: dir,
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    struct RecordingBackend(std::sync::Arc<std::sync::atomic::AtomicBool>);
    #[async_trait]
    impl WebExtractBackend for RecordingBackend {
        async fn extract(&self, _url: &str, _include_links: bool) -> Result<String, ToolError> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("fetched".into())
        }
    }
    #[async_trait]
    impl WebCrawlBackend for RecordingBackend {
        async fn crawl(
            &self,
            _url: &str,
            _instructions: Option<&str>,
            _depth: &str,
            _limit: usize,
        ) -> Result<String, ToolError> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("crawled".into())
        }
    }

    #[tokio::test]
    async fn web_extract_and_crawl_refuse_metadata_address_before_any_backend() {
        let metadata = "http://169.254.169.254/latest/meta-data/";
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let extract = WebExtractHandler::new(Box::new(RecordingBackend(called.clone())));
        let err = extract
            .execute(json!({"url": metadata}))
            .await
            .expect_err("metadata address must be refused");
        assert!(err.to_string().contains("Blocked"), "{err}");
        let crawl = WebCrawlHandler::new(Box::new(RecordingBackend(called.clone())));
        let err = crawl
            .execute(json!({"url": metadata}))
            .await
            .expect_err("metadata address must be refused");
        assert!(err.to_string().contains("Blocked"), "{err}");
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "no backend may run for a refused URL"
        );
    }

    /// A remote provider that followed a redirect or crawled a link onto a
    /// blocklisted site, in the shape the Tavily, Exa and Parallel extract
    /// backends return: the first page's text and a quality count are copied
    /// to the top level.
    struct ProviderPagesBackend;
    const PROVIDER_PAGES: &str = r#"{
        "url": "https://1.1.1.1/",
        "content": "blocklisted page",
        "results": [
            {"url": "https://blocked.example/landing", "content": "blocklisted page", "source_quality": "primary"},
            {"url": "https://1.1.1.1/docs", "content": "allowed page", "source_quality": "secondary"},
            {"content": "page without a url", "source_quality": "community"}
        ],
        "provider": "tavily",
        "source_quality_summary": {"primary": 1, "community": 1, "secondary": 1}
    }"#;
    #[async_trait]
    impl WebExtractBackend for ProviderPagesBackend {
        async fn extract(&self, _url: &str, _include_links: bool) -> Result<String, ToolError> {
            Ok(PROVIDER_PAGES.into())
        }
    }
    #[async_trait]
    impl WebCrawlBackend for ProviderPagesBackend {
        async fn crawl(
            &self,
            _url: &str,
            _instructions: Option<&str>,
            _depth: &str,
            _limit: usize,
        ) -> Result<String, ToolError> {
            Ok(PROVIDER_PAGES.into())
        }
    }

    #[tokio::test]
    async fn provider_pages_on_blocklisted_sites_are_withheld() {
        let _home = IsolatedHome::new();
        std::fs::write(
            hermes_config::config_path(),
            "security:\n  website_blocklist:\n    enabled: true\n    domains: [blocked.example]\n",
        )
        .unwrap();
        let seed = json!({"url": "https://1.1.1.1/"});
        let outputs = [
            WebExtractHandler::new(Box::new(ProviderPagesBackend))
                .execute(seed.clone())
                .await
                .expect("extract"),
            WebCrawlHandler::new(Box::new(ProviderPagesBackend))
                .execute(seed)
                .await
                .expect("crawl"),
        ];
        for output in outputs {
            let value: Value = serde_json::from_str(&output).expect("JSON results");
            let pages = value["results"].as_array().expect("results");
            assert_eq!(pages[0]["url"], "https://blocked.example/landing");
            assert!(
                pages[0]["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("blocked.example")),
                "{output}"
            );
            assert_eq!(pages[1]["content"], "allowed page");
            assert!(pages[2]["error"].is_string(), "{output}");
            assert_eq!(value["content"], "", "{output}");
            assert_eq!(
                value["source_quality_summary"],
                json!({"primary": 0, "community": 0, "secondary": 1})
            );
            assert!(!output.contains("blocklisted page"), "{output}");
            assert!(!output.contains("page without a url"), "{output}");
        }
    }

    /// Firecrawl's extract output: one page, no `results`, with the final URL
    /// in `metadata`.
    struct FirecrawlShapeBackend(String);
    #[async_trait]
    impl WebExtractBackend for FirecrawlShapeBackend {
        async fn extract(&self, _url: &str, _include_links: bool) -> Result<String, ToolError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn a_single_page_redirected_onto_a_blocklisted_site_is_withheld() {
        let _home = IsolatedHome::new();
        std::fs::write(
            hermes_config::config_path(),
            "security:\n  website_blocklist:\n    enabled: true\n    domains: [blocked.example]\n",
        )
        .unwrap();
        let seed = json!({"url": "https://1.1.1.1/"});
        for metadata in [
            r#"{"url": "https://blocked.example/after-redirect"}"#,
            r#"{"sourceURL": "https://blocked.example/after-redirect"}"#,
        ] {
            let page = format!(
                r#"{{"content": "redirected page", "metadata": {metadata}, "links": ["https://blocked.example/more"]}}"#
            );
            let output = WebExtractHandler::new(Box::new(FirecrawlShapeBackend(page)))
                .execute(seed.clone())
                .await
                .expect("extract");
            let value: Value = serde_json::from_str(&output).expect("JSON");
            assert!(
                value["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("blocked.example")),
                "{output}"
            );
            assert!(!output.contains("redirected page"), "{output}");
        }
        let allowed =
            r#"{"content": "allowed page", "metadata": {"sourceURL": "https://1.1.1.1/docs"}}"#;
        let output = WebExtractHandler::new(Box::new(FirecrawlShapeBackend(allowed.into())))
            .execute(seed)
            .await
            .expect("extract");
        assert_eq!(output, allowed);
    }

    #[tokio::test]
    async fn pages_whose_urls_cannot_be_judged_are_withheld() {
        let _home = IsolatedHome::new();
        std::fs::write(
            hermes_config::config_path(),
            "security:\n  website_blocklist:\n    enabled: true\n    domains: [blocked.example]\n",
        )
        .unwrap();
        async fn extract(output: &str) -> Result<String, ToolError> {
            WebExtractHandler::new(Box::new(FirecrawlShapeBackend(output.into())))
                .execute(json!({"url": "https://1.1.1.1/"}))
                .await
        }
        // A single page that names no URL, or one without a host.
        for page in [
            r#"{"content": "unjudged page", "metadata": {}}"#,
            r#"{"content": "unjudged page", "metadata": {"url": "blocked.example/after-redirect"}}"#,
        ] {
            let output = extract(page).await.expect("extract");
            assert!(!output.contains("unjudged page"), "{output}");
        }
        // In `results`, the first page's text stays at the top level only
        // while that page is kept.
        let pages = r#"{
            "content": "allowed page",
            "results": [
                {"url": "https://1.1.1.1/docs", "content": "allowed page"},
                {"url": "blocked.example/landing", "content": "unjudged page"}
            ]
        }"#;
        let output = extract(pages).await.expect("extract");
        let value: Value = serde_json::from_str(&output).expect("JSON");
        assert_eq!(value["content"], "allowed page", "{output}");
        assert!(value["results"][1]["error"].is_string(), "{output}");
        assert!(!output.contains("unjudged page"), "{output}");
    }

    #[tokio::test]
    async fn test_web_search_missing_params() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let result = handler.execute(json!({})).await;
        assert!(result.is_err());
    }
}
