//! Web tools: web_search (Exa) and web_extract (Firecrawl)

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::tools::url_safety::SsrfGuard;

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

        self.backend.search(query, num_results, category).await
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

        tool_schema(
            "web_search",
            "Search the web using Exa API. Returns relevant results with titles, URLs, and snippets.",
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

        let include_links = params
            .get("include_links")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

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

        tool_schema(
            "web_extract",
            "Extract clean content from a web page using Firecrawl. Returns the page text and optional links.",
            JsonSchema::object(props, vec!["url".into()]),
        )
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
    //
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
