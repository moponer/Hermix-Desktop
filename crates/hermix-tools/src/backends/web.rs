//! Real web tool backends: Baidu HTML search + local extract.
//!
//! All backends in this module are **keyless by design** — they work out of the
//! box without any API key. We deliberately avoid paid third-party services
//! (Exa, Firecrawl, Tavily, Serper, …) and use direct HTTP scraping of public
//! endpoints instead. To replace with another search engine, just point the
//! URL/parser to a different host.

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::web::{WebExtractBackend, WebSearchBackend};
use hermix_core::ToolError;

// ---------------------------------------------------------------------------
// BaiduHtmlSearchBackend (no API key, scrapes www.baidu.com/s)
// ---------------------------------------------------------------------------

const BAIDU_MAX_BYTES: usize = 1_048_576; // 1 MB

/// A web search backend that scrapes the public Baidu HTML search endpoint.
///
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
        let results = parse_baidu_results(&html, num_results);

        if results.is_empty() {
            // Surface an empty result set, not an error, so the LLM can adjust.
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
            ToolError::ExecutionFailed(format!("Failed to fetch '{}': {}", url, e))
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
}
