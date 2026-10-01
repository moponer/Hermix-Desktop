//! Web tools: web_search and web_extract
//!
//! All backends are **keyless by design** — they work out of the box
//! without any API key. Multi-engine fallback: DuckDuckGo → Baidu → Bing.
//!
//! `web_extract` uses SimpleExtractBackend (keyless reqwest HTML fetch).

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, traits::RiskLevel, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::tools::url_safety::SsrfGuard;
use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Backend traits (injectable by the caller)
// ---------------------------------------------------------------------------

/// Backend for web search operations.
#[async_trait]
pub trait WebSearchBackend: Send + Sync {
    /// Search the web and return results as JSON string.
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
    ) -> Result<String, ToolError>;

    /// Search with domain filtering. Default implementation ignores domains.
    async fn search_with_domains(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
        _allowed_domains: Option<Vec<String>>,
    ) -> Result<String, ToolError> {
        self.search(query, num_results, category).await
    }
}

/// Backend for web extraction operations (e.g. Firecrawl API).
#[async_trait]
pub trait WebExtractBackend: Send + Sync {
    /// Extract content from a URL and return results as a string.
    async fn extract(&self, url: &str, include_links: bool) -> Result<String, ToolError>;

    /// Extract content from a URL, filtered by a question. When the
    /// backend supports question-aware extraction, it should return
    /// only the paragraphs / sections relevant to the question —
    /// dramatically reducing context bloat (50KB page → ~2-5KB
    /// relevant excerpt). Default: ignores the question and returns
    /// full content (backward compatible).
    async fn extract_with_question(
        &self,
        url: &str,
        include_links: bool,
        question: &str,
    ) -> Result<String, ToolError> {
        // Default: ignore the question, fall back to full extract.
        let _ = question;
        self.extract(url, include_links).await
    }
}

// ---------------------------------------------------------------------------
// WebSearchHandler
// ---------------------------------------------------------------------------

/// Web search tool using Exa API.
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

        // Extract optional domain filter
        let allowed_domains = params
            .get("allowed_domains")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            });

        if allowed_domains.is_some() {
            self.backend
                .search_with_domains(query, num_results, category, allowed_domains)
                .await
        } else {
            self.backend.search(query, num_results, category).await
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "query".into(),
            json!({
                "type": "string",
                "description": "The search query"
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
        props.insert("allowed_domains".into(), json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "Optional list of domains to restrict search results to (e.g. ['rust-lang.org', 'docs.rs'])"
        }));

        tool_schema(
            "web_search",
            "Search the web. Returns relevant results with titles, URLs, and snippets. Supports domain filtering.",
            JsonSchema::object(props, vec!["query".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// WebExtractHandler
// ---------------------------------------------------------------------------

/// Web extraction tool using Firecrawl API.
pub struct WebExtractHandler {
    backend: Box<dyn WebExtractBackend>,
    /// SSRF guard. `Some(_)` enables the pre-check (default in
    /// [`Self::new`]); `None` skips it ([`Self::disabled`], for
    /// tests or trusted internal callers that intentionally want
    /// to fetch from a private address).
    ssrf_guard: Option<SsrfGuard>,
}

impl WebExtractHandler {
    /// Create a handler with the default SSRF blocklist enabled.
    /// This is the right constructor for production: it refuses to
    /// dispatch `http://169.254.169.254/` (cloud metadata) and
    /// friends before the backend ever sees the URL.
    pub fn new(backend: Box<dyn WebExtractBackend>) -> Self {
        Self {
            backend,
            ssrf_guard: Some(SsrfGuard::new()),
        }
    }

    /// Create a handler that skips the SSRF pre-check. Use only in
    /// tests or trusted internal contexts (e.g. extracting from
    /// a known localhost dev server).
    pub fn disabled(backend: Box<dyn WebExtractBackend>) -> Self {
        Self {
            backend,
            ssrf_guard: None,
        }
    }
}

#[async_trait]
impl ToolHandler for WebExtractHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'url' parameter".into()))?;

        // SSRF pre-check: refuse URLs that point at loopback,
        // link-local (incl. 169.254.169.254 cloud metadata), RFC
        // 1918, IPv6 ULA, IPv4-mapped IPv6, or non-http(s)
        // schemes. Hostname DNS resolution is the backend's
        // responsibility; the guard catches IP literals directly
        // and blocks well-known mDNS / internal hostnames by
        // name. See `crate::tools::url_safety::SsrfGuard`.
        if let Some(guard) = &self.ssrf_guard {
            if let Err(v) = guard.check(url) {
                return Err(ToolError::ExecutionFailed(v.to_string()));
            }
        }

        let include_links = crate::param_coercion::bool_param(&params, "include_links", true);

        // Question-aware extraction: when `question` is provided,
        // delegate to `extract_with_question` so the backend can return
        // only the paragraphs relevant to the question instead of the
        // full page content. This is the zero-context-bloat path —
        // a 50KB page becomes ~2-5KB of relevant excerpt.
        if let Some(question) = params.get("question").and_then(|v| v.as_str()) {
            if !question.trim().is_empty() {
                return self
                    .backend
                    .extract_with_question(url, include_links, question)
                    .await;
            }
        }

        self.backend.extract(url, include_links).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "The URL to extract content from"
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
        props.insert(
            "question".into(),
            json!({
                "type": "string",
                "description": "Optional question about the page content. When provided, the tool returns only the paragraphs relevant to the question instead of the full page — reducing context bloat from ~50KB to ~2-5KB. STRONGLY RECOMMENDED: always pass a question when you know what you're looking for. Example: question='What is the default timeout value?' instead of dumping the entire page into context."
            }),
        );

        tool_schema(
            "web_extract",
            "Extract clean content from a web page. Returns the page text and optional links. When 'question' is provided, returns only relevant paragraphs (context-efficient). Always pass 'question' when you know what you're looking for — a 50KB page becomes ~2-5KB.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register both `web_search` and `web_extract` with the registry.
/// Backends are keyless — no API key required. Uses multi-engine fallback:
/// DuckDuckGo → Baidu → Bing.
pub fn register(registry: &ToolRegistry) {
    // web_search — multi-engine keyless backend
    {
        let search = Arc::new(WebSearchHandler::new(Box::new(
            crate::backends::web::MultiEngineSearchBackend::new(),
        )));
        let schema = search.schema();
        registry.register_with_risk(
            "web_search",
            "web",
            schema,
            search,
            Arc::new(|| true),
            vec![],
            false,
            "Search the web (keyless, multi-engine: DuckDuckGo → Baidu → Bing). Returns results with titles, URLs, and snippets. Supports domain filtering.",
            "🔍",
            None,
            RiskLevel::Safe,
            false,
        );
    }
    // web_extract (with SSRF pre-check enabled by default)
    {
        let extract = Arc::new(WebExtractHandler::new(Box::new(
            crate::backends::web::SimpleExtractBackend::new(),
        )));
        let schema = extract.schema();
        registry.register_with_risk(
            "web_extract",
            "web",
            schema,
            extract,
            Arc::new(|| true),
            vec![],
            false,
            "Extract clean content from a URL. Includes an SSRF pre-check that blocks loopback / link-local / RFC1918 / IPv6 ULA addresses.",
            "📄",
            None,
            RiskLevel::Safe,
            false,
        );
    }
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
            _category: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Results for '{}' (count: {})", query, num_results))
        }
    }

    struct MockExtractBackend;
    #[async_trait]
    impl WebExtractBackend for MockExtractBackend {
        async fn extract(&self, url: &str, _include_links: bool) -> Result<String, ToolError> {
            Ok(format!("Content from {}", url))
        }
    }

    #[tokio::test]
    async fn test_web_search_schema() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "web_search");
        assert!(schema.parameters.properties.is_some());
    }

    #[tokio::test]
    async fn test_web_search_execute() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let result = handler
            .execute(json!({"query": "rust async"}))
            .await
            .unwrap();
        assert!(result.contains("rust async"));
    }

    #[tokio::test]
    async fn test_web_extract_schema() {
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "web_extract");
    }

    #[tokio::test]
    async fn test_web_extract_execute() {
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let result = handler
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(result.contains("example.com"));
    }

    // -- SSRF pre-check -------------------------------------------------
    // Pins the wiring: the SSRF blocklist sits in the handler, not
    // the backend, so even if a future backend swap forgets to
    // re-add the check the LLM-facing tool will still refuse to
    // dispatch. The default `new()` enables the guard; `disabled()`
    // is for tests that need to feed a private URL on purpose.

    #[tokio::test]
    async fn test_web_extract_blocks_aws_metadata() {
        // The whole reason this check exists: an LLM on an EC2
        // instance being tricked into fetching IAM credentials.
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let err = handler
            .execute(json!({"url": "http://169.254.169.254/latest/meta-data/"}))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("link_local") || msg.contains("169.254"),
            "expected SSRF block message, got: {}",
            msg
        );
    }

    #[tokio::test]
    async fn test_web_extract_blocks_rfc1918() {
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        for url in [
            "http://10.0.0.1/admin",
            "http://192.168.1.1/router",
            "http://172.16.0.1/internal",
        ] {
            let err = handler
                .execute(json!({"url": url}))
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("SSRF block"),
                "expected SSRF block for {}, got: {}",
                url,
                err
            );
        }
    }

    #[tokio::test]
    async fn test_web_extract_blocks_loopback_and_file_scheme() {
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let err = handler
            .execute(json!({"url": "http://127.0.0.1:6379/"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("loopback"));

        let err = handler
            .execute(json!({"url": "file:///etc/passwd"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("forbidden_scheme"));
    }

    #[tokio::test]
    async fn test_web_extract_blocks_ipv4_mapped_ipv6_bypass() {
        // The naive v4-only check would let ::ffff:10.0.0.1
        // through. The `url::Host` typed dispatch catches it.
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let err = handler
            .execute(json!({"url": "http://[::ffff:10.0.0.1]/admin"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ipv4_mapped_ipv6"));
    }

    #[tokio::test]
    async fn test_web_extract_disabled_bypasses_ssrf() {
        // The opt-out path — needed for tests / internal callers
        // that intentionally want to fetch from a private address.
        // The mock backend just echoes the URL, so we can confirm
        // the call went through.
        let handler = WebExtractHandler::disabled(Box::new(MockExtractBackend));
        let result = handler
            .execute(json!({"url": "http://10.0.0.1/admin"}))
            .await
            .unwrap();
        assert!(result.contains("10.0.0.1"));
    }

    #[tokio::test]
    async fn test_web_search_missing_params() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let result = handler.execute(json!({})).await;
        assert!(result.is_err());
    }
}
