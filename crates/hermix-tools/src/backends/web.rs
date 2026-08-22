//! Real web tool backends: multi-engine keyless search + local extract.
//!
//! All backends are **keyless by design** — they work out of the box
//! without any API key. The search backend tries multiple engines with
//! fallback: DuckDuckGo → Baidu → Bing.

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::web::{WebExtractBackend, WebSearchBackend};
use hermix_core::ToolError;

// ---------------------------------------------------------------------------
// Domain allowlist helper (shared by Baidu / DuckDuckGo / Bing backends)
// ---------------------------------------------------------------------------

/// Returns `true` when `url`'s host matches at least one entry in `allowed`.
/// Match rules (case-insensitive on both sides):
/// - exact host equality (e.g. `"baidu.com"` matches `https://baidu.com/x`)
/// - wildcard `*.suffix` matches `suffix` itself and any subdomain
/// (e.g. `"*.baidu.com"` matches `www.baidu.com`, `tieba.baidu.com`, ...)
/// Returns `false` for unparseable URLs or URLs without a host.
/// This intentionally replaces the previous `url.contains(d)` substring
/// match, which permitted trivially bypassing an allowlist with e.g.
/// `https://evil.com/?redirect=https://baidu.com/x`.
fn host_in_allowlist(url: &str, allowed: &[String]) -> bool {
    let host = match reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_lowercase()))
    {
        Some(h) => h,
        None => return false,
    };
    allowed.iter().any(|d| {
        let d = d.to_lowercase();
        if d == host {
            return true;
        }
        if let Some(suffix) = d.strip_prefix("*.") {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            false
        }
    })
}

// ---------------------------------------------------------------------------
// BaiduHtmlSearchBackend (no API key, scrapes www.baidu.com/s)
// ---------------------------------------------------------------------------

const BAIDU_MAX_BYTES: usize = 1_048_576; // 1 MB

/// A web search backend that scrapes the public Baidu HTML search endpoint.
/// Baidu does not require authentication for the `GET /s?wd=...` HTML page.
/// We parse the result list with a tolerant regex (Baidu does not expose a
/// stable JSON API without a key).
pub struct BaiduHtmlSearchBackend {
    client: Client,
}

impl BaiduHtmlSearchBackend {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .user_agent(concat!(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
                    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36"
                ))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

impl Default for BaiduHtmlSearchBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebSearchBackend for BaiduHtmlSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        self.search_with_domains(query, num_results, _category, None)
            .await
    }

    async fn search_with_domains(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<String, ToolError> {
        let resp = self
            .client
            .get("https://www.baidu.com/s")
            .query(&[("wd", query), ("rn", &num_results.min(50).to_string())])
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Baidu search request failed: {}", e))
            })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Baidu search HTTP {}",
                status
            )));
        }

        let bytes = resp.bytes().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Baidu response: {}", e))
        })?;

        if bytes.len() > BAIDU_MAX_BYTES {
            return Err(ToolError::ExecutionFailed(format!(
                "Baidu response too large ({} bytes)",
                bytes.len()
            )));
        }

        let html = String::from_utf8_lossy(&bytes).to_string();
        let mut results = parse_baidu_results(&html, num_results);

        // Apply domain filtering if specified
        if let Some(ref domains) = allowed_domains {
            results.retain(|r| {
                if let Some(url) = r.get("url").and_then(|v| v.as_str()) {
                    host_in_allowlist(url, domains)
                } else {
                    false
                }
            });
        }

        if results.is_empty() {
            return Ok(json!({
                "engine": "baidu",
                "query": query,
                "results": [],
                "note": "Baidu returned no parseable results; the page layout may have changed."
            })
            .to_string());
        }

        Ok(json!({
            "engine": "baidu",
            "query": query,
            "results": results,
        })
        .to_string())
    }
}

/// Parse Baidu's `h3.a` result blocks. Falls back to a loose link pattern
/// when Baidu's class names have rotated.
fn parse_baidu_results(html: &str, limit: usize) -> Vec<Value> {
    // Primary pattern: <h3 class="..."><a href="...">title</a></h3>
    let re_h3 = Regex::new(r#"(?s)<h3[^>]*>\s*<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>\s*</h3>"#)
        .expect("static regex");
    // Secondary pattern: any <a href="http..."> inside the result container.
    let re_a = Regex::new(r#"(?s)<a[^>]+href="(https?://[^"]+)"[^>]*>(.*?)</a>"#)
        .expect("static regex");

    let mut out: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for cap in re_h3.captures_iter(html) {
        let url = decode_html_entities(&cap[1]);
        let title = strip_html_tags(&cap[2]);
        if !url.is_empty() && seen.insert(url.clone()) {
            out.push(json!({
                "title": title,
                "url": url,
            }));
            if out.len() >= limit {
                break;
            }
        }
    }

    if out.is_empty() {
        for cap in re_a.captures_iter(html) {
            let url = decode_html_entities(&cap[1]);
            let title = strip_html_tags(&cap[2]);
            if title.is_empty() || title.len() > 200 {
                continue;
            }
            if url.contains("baidu.com") || url.contains("bdimg.com") {
                continue;
            }
            if seen.insert(url.clone()) {
                out.push(json!({
                    "title": title,
                    "url": url,
                }));
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    out
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

// ---------------------------------------------------------------------------
// SimpleExtractBackend (uses reqwest, no API key needed)
// ---------------------------------------------------------------------------

const MAX_EXTRACT_BYTES: usize = 512_000; // 500 KB

/// A web extraction backend that fetches HTML via reqwest with no external API dependency.
pub struct SimpleExtractBackend {
    client: Client,
}

impl SimpleExtractBackend {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
                )
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

impl Default for SimpleExtractBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebExtractBackend for SimpleExtractBackend {
    async fn extract(&self, url: &str, _include_links: bool) -> Result<String, ToolError> {
        let resp = self.client.get(url).send().await.map_err(|e| {
            let mut msg = format!("Failed to fetch '{}': {}", url, e);
            // 超时时明确提示 LLM 勿重试, 避免对同一慢/挂 URL 无意义重试累积
            if e.is_timeout() {
                msg.push_str(
                    "\n\n[请勿重试] 该 URL 响应超时, 重复请求大概率仍会超时。请改用其他方式 \
                     (如换数据源/换工具) 或告知用户该服务暂不可用。",
                );
            }
            ToolError::ExecutionFailed(msg)
        })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "HTTP {} when fetching '{}'",
                status, url
            )));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = resp.bytes().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read response body: {}", e))
        })?;

        if bytes.len() > MAX_EXTRACT_BYTES {
            let text = String::from_utf8_lossy(&bytes[..MAX_EXTRACT_BYTES]);
            let result = json!({
                "url": url,
                "content_type": content_type,
                "content": text,
                "truncated": true,
                "original_size": bytes.len(),
            });
            return serde_json::to_string_pretty(&result)
                .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {}", e)));
        }

        let text = String::from_utf8_lossy(&bytes);

        let content = strip_html_tags(&text);

        let result = json!({
            "url": url,
            "content_type": content_type,
            "content": content,
            "truncated": false,
            "size": bytes.len(),
        });

        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {}", e)))
    }

    /// Question-aware extraction: fetch the page, strip HTML, then
    /// return only paragraphs that contain keywords from the question.
    /// This is a "poor man's sub-model" — no LLM call needed, but
    /// reduces a 50KB page to ~2-5KB of relevant content.
    ///
    /// Algorithm:
    /// 1. Fetch + strip HTML (same as `extract`)
    /// 2. Split into paragraphs (double-newline separated)
    /// 3. Extract keywords from the question (alphanumeric tokens ≥ 3 chars)
    /// 4. Score each paragraph by keyword hit count
    /// 5. Return top N paragraphs (up to 5KB total)
    /// 6. If no paragraph matches, return the first 5KB (fallback)
    async fn extract_with_question(
        &self,
        url: &str,
        _include_links: bool,
        question: &str,
    ) -> Result<String, ToolError> {
        // Fetch the page (same logic as extract).
        let resp = self.client.get(url).send().await.map_err(|e| {
            let mut msg = format!("Failed to fetch '{}': {}", url, e);
            // 超时时明确提示 LLM 勿重试, 避免对同一慢/挂 URL 无意义重试累积
            if e.is_timeout() {
                msg.push_str(
                    "\n\n[请勿重试] 该 URL 响应超时, 重复请求大概率仍会超时。请改用其他方式 \
                     (如换数据源/换工具) 或告知用户该服务暂不可用。",
                );
            }
            ToolError::ExecutionFailed(msg)
        })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "HTTP {} when fetching '{}'",
                status, url
            )));
        }

        let bytes = resp.bytes().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read response body: {}", e))
        })?;

        // Cap at MAX_EXTRACT_BYTES even for question-aware path.
        let bytes = if bytes.len() > MAX_EXTRACT_BYTES {
            &bytes[..MAX_EXTRACT_BYTES]
        } else {
            &bytes[..]
        };

        let text = String::from_utf8_lossy(bytes);
        let content = strip_html_tags(&text);

        // Extract keywords from the question.
        let keywords: Vec<String> = question
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| s.len() >= 3)
            .map(|s| s.to_lowercase())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Split content into paragraphs.
        let paragraphs: Vec<&str> = content.split("\n\n").collect();

        // Score each paragraph by keyword hits (case-insensitive).
        let mut scored: Vec<(usize, &str)> = paragraphs
            .iter()
            .map(|p| {
                let lower = p.to_lowercase();
                let score = keywords
                    .iter()
                    .filter(|kw| lower.contains(kw.as_str()))
                    .count();
                (score, *p)
            })
            .filter(|(score, p)| *score > 0 && p.len() > 20)
            .collect();

        // Sort by score descending, take top paragraphs up to 5KB.
        scored.sort_by(|a, b| b.0.cmp(&a.0));

        const MAX_QUESTION_RESULT_BYTES: usize = 5_000;
        let mut result_content = String::with_capacity(MAX_QUESTION_RESULT_BYTES);
        let mut paragraphs_used = 0;

        for (_, para) in &scored {
            if result_content.len() + para.len() + 2 > MAX_QUESTION_RESULT_BYTES {
                break;
            }
            if !result_content.is_empty() {
                result_content.push_str("\n\n");
            }
            result_content.push_str(para);
            paragraphs_used += 1;
        }

        // Fallback: if no paragraphs matched keywords, return the first
        // 5KB of content so the LLM at least gets something.
        if result_content.is_empty() {
            result_content = content.chars().take(MAX_QUESTION_RESULT_BYTES).collect();
        }

        let result = json!({
            "url": url,
            "question": question,
            "content": result_content,
            "paragraphs_returned": paragraphs_used,
            "paragraphs_total": paragraphs.len(),
            "keywords_matched": keywords.len(),
            "truncated": content.len() > MAX_QUESTION_RESULT_BYTES,
            "original_size": content.len(),
        });

        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// DuckDuckGoHtmlSearchBackend (keyless, scrapes DuckDuckGo HTML)
// ---------------------------------------------------------------------------

const DDG_MAX_BYTES: usize = 1_048_576; // 1 MB

/// A web search backend that scrapes DuckDuckGo's HTML search page.
pub struct DuckDuckGoHtmlSearchBackend {
    client: Client,
}

impl DuckDuckGoHtmlSearchBackend {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .user_agent(concat!(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
                    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36"
                ))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

impl Default for DuckDuckGoHtmlSearchBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebSearchBackend for DuckDuckGoHtmlSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        self.search_with_domains(query, num_results, _category, None)
            .await
    }

    async fn search_with_domains(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<String, ToolError> {
        let resp = self
            .client
            .get("https://html.duckduckgo.com/html/")
            .query(&[("q", query)])
            .header("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8")
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("DuckDuckGo search request failed: {}", e))
            })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "DuckDuckGo search HTTP {}",
                status
            )));
        }

        let bytes = resp.bytes().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read DuckDuckGo response: {}", e))
        })?;

        if bytes.len() > DDG_MAX_BYTES {
            return Err(ToolError::ExecutionFailed(format!(
                "DuckDuckGo response too large ({} bytes)",
                bytes.len()
            )));
        }

        let html = String::from_utf8_lossy(&bytes).to_string();
        let mut results = parse_ddg_results(&html, num_results);

        // Apply domain filtering
        if let Some(ref domains) = allowed_domains {
            results.retain(|r| {
                if let Some(url) = r.get("url").and_then(|v| v.as_str()) {
                    host_in_allowlist(url, domains)
                } else {
                    false
                }
            });
        }

        Ok(json!({
            "engine": "duckduckgo",
            "query": query,
            "results": results,
        })
        .to_string())
    }
}

/// Parse DuckDuckGo HTML search results.
fn parse_ddg_results(html: &str, limit: usize) -> Vec<Value> {
    // DuckDuckGo result pattern: <a rel="nofollow" class="result__a" href="...">title</a>
    let re_a = Regex::new(
        r#"(?s)<a[^>]+class="result__a"[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#,
    )
    .expect("static regex");
    // Fallback: any link in result container
    let re_fallback = Regex::new(
        r#"(?s)<a[^>]+href="(https?://[^"]+)"[^>]*class="result__a"[^>]*>(.*?)</a>"#,
    )
    .expect("static regex");
    // Snippet pattern
    let re_snippet =
        Regex::new(r#"(?s)<a[^>]+class="result__snippet"[^>]*>(.*?)</a>"#).expect("static regex");

    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Try primary pattern
    for cap in re_a.captures_iter(html) {
        let url = decode_html_entities(&cap[1]);
        let title = strip_html_tags(&cap[2]);
        if !url.is_empty() && title.len() < 200 && seen.insert(url.clone()) {
            out.push(json!({
                "title": title,
                "url": url,
            }));
            if out.len() >= limit {
                break;
            }
        }
    }

    // Try fallback pattern
    if out.is_empty() {
        for cap in re_fallback.captures_iter(html) {
            let url = decode_html_entities(&cap[1]);
            let title = strip_html_tags(&cap[2]);
            if !url.is_empty() && title.len() < 200 && seen.insert(url.clone()) {
                out.push(json!({
                    "title": title,
                    "url": url,
                }));
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    // Extract snippets if available
    let snippets: Vec<String> = re_snippet
        .captures_iter(html)
        .map(|cap| strip_html_tags(&cap[1]))
        .filter(|s| !s.is_empty() && s.len() < 500)
        .take(limit)
        .collect();

    // Attach snippets to results
    for (i, result) in out.iter_mut().enumerate() {
        if let Some(snippet) = snippets.get(i) {
            result["snippet"] = json!(snippet);
        }
    }

    out
}

// ---------------------------------------------------------------------------
// BingHtmlSearchBackend (keyless, scrapes Bing HTML)
// ---------------------------------------------------------------------------

const BING_MAX_BYTES: usize = 1_048_576; // 1 MB

/// A web search backend that scrapes Bing's HTML search page.
pub struct BingHtmlSearchBackend {
    client: Client,
}

impl BingHtmlSearchBackend {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .user_agent(concat!(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
                    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36"
                ))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

impl Default for BingHtmlSearchBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebSearchBackend for BingHtmlSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        self.search_with_domains(query, num_results, _category, None)
            .await
    }

    async fn search_with_domains(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<String, ToolError> {
        let resp = self
            .client
            .get("https://www.bing.com/search")
            .query(&[("q", query), ("count", &num_results.min(50).to_string())])
            .header("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8")
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Bing search request failed: {}", e))
            })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Bing search HTTP {}",
                status
            )));
        }

        let bytes = resp.bytes().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Bing response: {}", e))
        })?;

        if bytes.len() > BING_MAX_BYTES {
            return Err(ToolError::ExecutionFailed(format!(
                "Bing response too large ({} bytes)",
                bytes.len()
            )));
        }

        let html = String::from_utf8_lossy(&bytes).to_string();
        let mut results = parse_bing_results(&html, num_results);

        // Apply domain filtering
        if let Some(ref domains) = allowed_domains {
            results.retain(|r| {
                if let Some(url) = r.get("url").and_then(|v| v.as_str()) {
                    host_in_allowlist(url, domains)
                } else {
                    false
                }
            });
        }

        Ok(json!({
            "engine": "bing",
            "query": query,
            "results": results,
        })
        .to_string())
    }
}

/// Parse Bing HTML search results.
fn parse_bing_results(html: &str, limit: usize) -> Vec<Value> {
    // Bing result pattern: <h2><a href="...">title</a></h2>
    let re_h2 = Regex::new(
        r#"(?s)<h2[^>]*>\s*<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>\s*</h2>"#,
    )
    .expect("static regex");
    // Fallback pattern
    let re_a = Regex::new(
        r#"(?s)<a[^>]+href="(https?://[^"]+)"[^>]*class="b_algo"[^>]*>(.*?)</a>"#,
    )
    .expect("static regex");

    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for cap in re_h2.captures_iter(html) {
        let url = decode_html_entities(&cap[1]);
        let title = strip_html_tags(&cap[2]);
        if !url.is_empty()
            && title.len() < 200
            && !url.contains("bing.com")
            && seen.insert(url.clone())
        {
            out.push(json!({
                "title": title,
                "url": url,
            }));
            if out.len() >= limit {
                break;
            }
        }
    }

    if out.is_empty() {
        for cap in re_a.captures_iter(html) {
            let url = decode_html_entities(&cap[1]);
            let title = strip_html_tags(&cap[2]);
            if !url.is_empty()
                && title.len() < 200
                && !url.contains("bing.com")
                && seen.insert(url.clone())
            {
                out.push(json!({
                    "title": title,
                    "url": url,
                }));
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// MultiEngineSearchBackend (keyless, tries multiple engines with fallback)
// ---------------------------------------------------------------------------

/// A keyless search backend that tries multiple engines with automatic
/// fallback. Order: DuckDuckGo → Baidu → Bing.
pub struct MultiEngineSearchBackend {
    engines: Vec<Box<dyn WebSearchBackend>>,
}

impl MultiEngineSearchBackend {
    pub fn new() -> Self {
        Self {
            engines: vec![
                Box::new(DuckDuckGoHtmlSearchBackend::new()),
                Box::new(BaiduHtmlSearchBackend::new()),
                Box::new(BingHtmlSearchBackend::new()),
            ],
        }
    }
}

impl Default for MultiEngineSearchBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebSearchBackend for MultiEngineSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
    ) -> Result<String, ToolError> {
        self.search_with_domains(query, num_results, category, None)
            .await
    }

    async fn search_with_domains(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<String, ToolError> {
        let mut last_error = None;

        for engine in &self.engines {
            match engine
                .search_with_domains(query, num_results, category, allowed_domains.clone())
                .await
            {
                Ok(result) => {
                    // Check if the result has actual results
                    if let Ok(parsed) = serde_json::from_str::<Value>(&result) {
                        if let Some(results) = parsed.get("results").and_then(|v| v.as_array()) {
                            if !results.is_empty() {
                                return Ok(result);
                            }
                        }
                    }
                    // Empty results, try next engine
                    last_error = Some(ToolError::ExecutionFailed(
                        "Engine returned empty results".to_string(),
                    ));
                }
                Err(e) => {
                    last_error = Some(e);
                    // Try next engine
                }
            }
        }

        // All engines failed, return last error
        Err(last_error.unwrap_or_else(|| {
            ToolError::ExecutionFailed("No search engines available".to_string())
        }))
    }
}

/// Minimal HTML tag stripper for producing readable text from HTML.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_space = false;

    let lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        if !in_tag && chars[i] == '<' {
            in_tag = true;
            let remaining: String = lower_chars[i..].iter().take(20).collect();
            if remaining.starts_with("<script") {
                in_script = true;
            } else if remaining.starts_with("</script") {
                in_script = false;
            } else if remaining.starts_with("<style") {
                in_style = true;
            } else if remaining.starts_with("</style") {
                in_style = false;
            }
            i += 1;
            continue;
        }

        if in_tag {
            if chars[i] == '>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }

        if in_script || in_style {
            i += 1;
            continue;
        }

        let ch = chars[i];
        if ch.is_whitespace() {
            if !last_was_space && !result.is_empty() {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            result.push(ch);
            last_was_space = false;
        }
        i += 1;
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_handles_script_and_style() {
        let html =
            "<html><head><style>body{color:red}</style></head><body><script>x()</script>Hello<b>World</b>!</body></html>";
        let out = strip_html_tags(html);
        assert!(out.contains("Hello"));
        assert!(out.contains("World"));
        assert!(!out.contains("color:red"));
        assert!(!out.contains("x()"));
    }

    #[test]
    fn decode_entities_round_trip() {
        assert_eq!(decode_html_entities("a&amp;b"), "a&b");
        assert_eq!(decode_html_entities("&lt;tag&gt;"), "<tag>");
    }

    // -----------------------------------------------------------------------
    // host_in_allowlist — regression tests for the substring-bypass bug fixed
    // (audit of docs/GROK_BUILD_MIGRATION_REPORT.md P0 finding).
    // -----------------------------------------------------------------------

    #[test]
    fn host_in_allowlist_matches_exact_host() {
        let allowed = vec!["baidu.com".into()];
        assert!(host_in_allowlist("https://baidu.com/s?wd=hi", &allowed));
    }

    #[test]
    fn host_in_allowlist_is_case_insensitive_via_wildcard() {
        // The browser host parser already lowercases the URL host, so
        // case-insensitivity only matters when the URL host has an
        // uppercase *suffix* and the allowlist pattern uses a wildcard.
        let allowed = vec!["*.baidu.com".into()];
        assert!(host_in_allowlist("https://WWW.baidu.com/s?wd=hi", &allowed));
        assert!(host_in_allowlist("https://Tieba.BAIDU.com/x", &allowed));
    }

    #[test]
    fn host_in_allowlist_matches_wildcard_subdomain() {
        let allowed = vec!["*.baidu.com".into()];
        assert!(host_in_allowlist("https://www.baidu.com/x", &allowed));
        assert!(host_in_allowlist("https://tieba.baidu.com/y", &allowed));
        assert!(host_in_allowlist("https://baidu.com/z", &allowed)); // apex matches
        assert!(!host_in_allowlist("https://evil.com/w", &allowed));
    }

    #[test]
    fn host_in_allowlist_rejects_substring_bypass() {
        // The P0 bug: `url.contains(d)` on `evil.com/?u=baidu.com`
        // matched a "baidu.com" allowlist. Hostname extraction must
        // reject this — `evil.com` is not in the allowlist, regardless
        // of what the query string happens to contain.
        let allowed = vec!["baidu.com".into()];
        assert!(!host_in_allowlist("https://evil.com/?u=https://baidu.com/x", &allowed));
        assert!(!host_in_allowlist("https://attacker.com/path?redirect=baidu.com", &allowed));
        assert!(!host_in_allowlist("https://baidu.com.evil.com/x", &allowed));
    }

    #[test]
    fn host_in_allowlist_returns_false_for_unparseable_or_hostless() {
        let allowed = vec!["baidu.com".into()];
        assert!(!host_in_allowlist("not a url", &allowed));
        assert!(!host_in_allowlist("file:///etc/passwd", &allowed));
        assert!(!host_in_allowlist("", &allowed));
    }

    #[test]
    fn parse_baidu_results_extracts_h3_blocks() {
        let html = r#"
            <html><body>
              <h3 class="t"><a href="http://example.com/1">标题 1</a></h3>
              <h3 class="t"><a href="http://example.com/2">标题 2</a></h3>
              <h3 class="t"><a href="http://example.com/3">标题 3</a></h3>
            </body></html>
        "#;
        let out = parse_baidu_results(html, 10);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["url"], "http://example.com/1");
        assert_eq!(out[0]["title"], "标题 1");
    }

    #[test]
    fn parse_ddg_results_extracts_links() {
        let html = r#"
            <html><body>
              <a rel="nofollow" class="result__a" href="https://rust-lang.org/">Rust Programming</a>
              <a rel="nofollow" class="result__a" href="https://docs.rs/">Docs.rs</a>
            </body></html>
        "#;
        let out = parse_ddg_results(html, 10);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["url"], "https://rust-lang.org/");
        assert_eq!(out[0]["title"], "Rust Programming");
    }

    #[test]
    fn parse_bing_results_extracts_h2() {
        let html = r#"
            <html><body>
              <h2><a href="https://example.com/page1">Example Page 1</a></h2>
              <h2><a href="https://example.com/page2">Example Page 2</a></h2>
            </body></html>
        "#;
        let out = parse_bing_results(html, 10);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["url"], "https://example.com/page1");
        assert_eq!(out[0]["title"], "Example Page 1");
    }

    #[test]
    fn parse_bing_filters_bing_domains() {
        let html = r#"
            <html><body>
              <h2><a href="https://www.bing.com/account">Bing Account</a></h2>
              <h2><a href="https://example.com/real">Real Result</a></h2>
            </body></html>
        "#;
        let out = parse_bing_results(html, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["url"], "https://example.com/real");
    }

    #[test]
    fn parse_ddg_extracts_snippets() {
        let html = r#"
            <html><body>
              <a rel="nofollow" class="result__a" href="https://example.com/">Example</a>
              <a class="result__snippet">This is a snippet about the result.</a>
            </body></html>
        "#;
        let out = parse_ddg_results(html, 10);
        assert_eq!(out.len(), 1);
        assert!(out[0].get("snippet").is_some());
    }
}
