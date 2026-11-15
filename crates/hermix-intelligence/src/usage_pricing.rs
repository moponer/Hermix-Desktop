//! Usage pricing — per-model pricing data, cost calculation, and formatting.
//!
//! Ported from `agent/usage_pricing.py`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Canonical usage breakdown from a single API call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub request_count: u64,
}

impl CanonicalUsage {
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens() + self.output_tokens
    }
}

/// Billing routing information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingRoute {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub billing_mode: BillingMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingMode {
    OfficialDocsSnapshot,
    ProviderModelsApi,
    SubscriptionIncluded,
    Unknown,
}

/// Per-model pricing entry (costs per 1M tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingEntry {
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
    pub cache_read_cost_per_million: Option<f64>,
    pub cache_write_cost_per_million: Option<f64>,
    pub request_cost: Option<f64>,
    pub source: CostSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostSource {
    OfficialDocsSnapshot,
    ProviderModelsApi,
    UserOverride,
    None,
}

/// Result of a cost estimation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostResult {
    pub amount_usd: Option<f64>,
    pub status: CostStatus,
    pub source: CostSource,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostStatus {
    Actual,
    Estimated,
    Included,
    Unknown,
}

// ---------------------------------------------------------------------------
// Official docs pricing database
// ---------------------------------------------------------------------------

/// Build the official pricing database.
fn official_pricing_db() -> HashMap<(&'static str, &'static str), PricingEntry> {
    let mut db = HashMap::new();

    let add = |db: &mut HashMap<(&str, &str), PricingEntry>,
               provider: &'static str,
               model: &'static str,
               input: f64,
               output: f64,
               cache_read: Option<f64>,
               cache_write: Option<f64>| {
        db.insert(
            (provider, model),
            PricingEntry {
                input_cost_per_million: Some(input),
                output_cost_per_million: Some(output),
                cache_read_cost_per_million: cache_read,
                cache_write_cost_per_million: cache_write,
                request_cost: None,
                source: CostSource::OfficialDocsSnapshot,
            },
        );
    };

    // Anthropic
    add(
        &mut db,
        "anthropic",
        "claude-opus-4-20250514",
        15.0,
        75.0,
        Some(1.50),
        Some(18.75),
    );
    add(
        &mut db,
        "anthropic",
        "claude-sonnet-4-20250514",
        3.0,
        15.0,
        Some(0.30),
        Some(3.75),
    );
    add(
        &mut db,
        "anthropic",
        "claude-3-5-sonnet-20241022",
        3.0,
        15.0,
        Some(0.30),
        Some(3.75),
    );
    add(
        &mut db,
        "anthropic",
        "claude-3-5-haiku-20241022",
        0.80,
        4.0,
        Some(0.08),
        Some(1.00),
    );
    add(
        &mut db,
        "anthropic",
        "claude-3-opus-20240229",
        15.0,
        75.0,
        Some(1.50),
        Some(18.75),
    );
    add(
        &mut db,
        "anthropic",
        "claude-3-haiku-20240307",
        0.25,
        1.25,
        Some(0.03),
        Some(0.30),
    );

    // OpenAI
    add(&mut db, "openai", "gpt-4o", 2.50, 10.0, Some(1.25), None);
    add(
        &mut db,
        "openai",
        "gpt-4o-mini",
        0.15,
        0.60,
        Some(0.075),
        None,
    );
    add(&mut db, "openai", "gpt-4.1", 2.00, 8.0, Some(0.50), None);
    add(
        &mut db,
        "openai",
        "gpt-4.1-mini",
        0.40,
        1.60,
        Some(0.10),
        None,
    );
    add(
        &mut db,
        "openai",
        "gpt-4.1-nano",
        0.10,
        0.40,
        Some(0.025),
        None,
    );
    add(&mut db, "openai", "o3", 10.0, 40.0, Some(2.50), None);
    add(&mut db, "openai", "o3-mini", 1.10, 4.40, Some(0.55), None);

    // Google Gemini
    add(&mut db, "google", "gemini-2.5-pro", 1.25, 10.0, None, None);
    add(
        &mut db,
        "google",
        "gemini-2.5-flash",
        0.15,
        0.60,
        None,
        None,
    );
    add(
        &mut db,
        "google",
        "gemini-2.0-flash",
        0.10,
        0.40,
        None,
        None,
    );

    // DeepSeek — official pricing snapshot (api-docs.deepseek.com,
    // 2026-08-17 峰谷调价后). `input` below is the PEAK cache-miss rate
    // (upper bound for an estimate; off-peak is half). Cache-hit input
    // goes into cache_read_cost_per_million. `deepseek-chat` /
    // `deepseek-reasoner` are legacy aliases routed to
    // DeepSeek-V4.1-Flash by the provider — priced the same.
    add(
        &mut db,
        "deepseek",
        "deepseek-flash",
        0.30,
        1.20,
        Some(0.006),
        None,
    );
    add(
        &mut db,
        "deepseek",
        "deepseek-v4-flash",
        0.30,
        1.20,
        Some(0.006),
        None,
    );
    add(
        &mut db,
        "deepseek",
        "deepseek-chat",
        0.30,
        1.20,
        Some(0.006),
        None,
    );
    add(
        &mut db,
        "deepseek",
        "deepseek-reasoner",
        0.30,
        1.20,
        Some(0.006),
        None,
    );
    add(
        &mut db,
        "deepseek",
        "deepseek-v4-pro",
        1.32,
        3.96,
        Some(0.044),
        None,
    );

    // MiniMax — official/cross-platform snapshot (pricepertoken.com,
    // 2026-10-26; cross-checked against MiniMax 官方按量页 2026-10).
    // Cache write is not billed separately (None). M2.7 deliberately
    // omitted: its listed input/output price is $0.000 (promo or data
    // anomaly) — guessing would be worse than showing "—".
    add(
        &mut db,
        "minimax",
        "minimax-m3",
        0.224,
        0.88,
        Some(0.0448),
        None,
    );
    add(
        &mut db,
        "minimax",
        "minimax-m3.1-flash",
        0.10,
        0.55,
        Some(0.01),
        None,
    );
    add(
        &mut db,
        "minimax",
        "minimax-m2.5",
        0.27,
        0.95,
        Some(0.027),
        None,
    );

    db
}

// ---------------------------------------------------------------------------
// Core functions
// ---------------------------------------------------------------------------

/// Resolve billing route from model/provider/base_url.
///
/// Model strings arrive in several shapes depending on how the agent
/// was configured: bare (`MiniMax-M3`), provider-prefixed
/// (`minimax:MiniMax-M3`), or proxy-prefixed
/// (`kilo:minimax/minimax-m3`). The prefix before the first `:` (when
/// present) is treated as the provider when none was given explicitly,
/// and any remaining `vendor/` segment is stripped before the DB
/// lookup — so all three shapes resolve to the same pricing entry.
pub fn resolve_billing_route(
    model_name: &str,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> BillingRoute {
    let provider_name = provider.unwrap_or("").trim().to_lowercase();
    let base = base_url.unwrap_or("").trim().to_lowercase();
    let raw_model = model_name.trim();

    // Strip a `prefix:` provider tag; adopt it as the provider when the
    // caller didn't supply one.
    let (effective_provider, model_body) = match raw_model.split_once(':') {
        Some((prefix, rest)) if !prefix.is_empty() && !rest.is_empty() => (
            if provider_name.is_empty() {
                prefix.to_lowercase()
            } else {
                provider_name
            },
            rest,
        ),
        _ => (provider_name, raw_model),
    };

    // Free proxies bill nothing to the user ("custom-free:…" hermix
    // model ids, OpenRouter `…:free` variants). Checked after prefix
    // extraction so the marker inside the prefix isn't hidden.
    if effective_provider.contains("free") || raw_model.to_lowercase().contains(":free") {
        return BillingRoute {
            provider: if effective_provider.is_empty() {
                "free-proxy".into()
            } else {
                effective_provider
            },
            model: raw_model.to_string(),
            base_url: base,
            billing_mode: BillingMode::SubscriptionIncluded,
        };
    }

    let model = model_body
        .split('/')
        .next_back()
        .unwrap_or(model_body)
        .to_string();

    let provider_name = effective_provider;
    if provider_name == "openai-codex" {
        return BillingRoute {
            provider: "openai-codex".into(),
            model,
            base_url: base,
            billing_mode: BillingMode::SubscriptionIncluded,
        };
    }
    if provider_name == "openrouter" || base.contains("openrouter.ai") {
        return BillingRoute {
            provider: "openrouter".into(),
            model,
            base_url: base,
            billing_mode: BillingMode::ProviderModelsApi,
        };
    }
    if provider_name == "anthropic" {
        let bare = model.split('/').next_back().unwrap_or(&model).to_string();
        return BillingRoute {
            provider: "anthropic".into(),
            model: bare,
            base_url: base,
            billing_mode: BillingMode::OfficialDocsSnapshot,
        };
    }
    if provider_name == "openai" {
        let bare = model.split('/').next_back().unwrap_or(&model).to_string();
        return BillingRoute {
            provider: "openai".into(),
            model: bare,
            base_url: base,
            billing_mode: BillingMode::OfficialDocsSnapshot,
        };
    }

    BillingRoute {
        provider: if provider_name.is_empty() {
            "unknown".into()
        } else {
            provider_name
        },
        model: model.split('/').next_back().unwrap_or(&model).to_string(),
        base_url: base,
        billing_mode: BillingMode::Unknown,
    }
}

/// Look up pricing for a model.
pub fn get_pricing_entry(
    model_name: &str,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> Option<PricingEntry> {
    let route = resolve_billing_route(model_name, provider, base_url);

    if route.billing_mode == BillingMode::SubscriptionIncluded {
        return Some(PricingEntry {
            input_cost_per_million: Some(0.0),
            output_cost_per_million: Some(0.0),
            cache_read_cost_per_million: Some(0.0),
            cache_write_cost_per_million: Some(0.0),
            request_cost: None,
            source: CostSource::None,
        });
    }

    let db = official_pricing_db();
    let model_lower = route.model.to_lowercase();

    // Exact match
    if let Some(entry) = db.get(&(route.provider.as_str(), model_lower.as_str())) {
        return Some(entry.clone());
    }

    // Substring match (longest key first)
    let mut candidates: Vec<_> = db
        .iter()
        .filter(|((p, m), _)| *p == route.provider.as_str() && model_lower.contains(*m))
        .collect();
    candidates.sort_by_key(|b| std::cmp::Reverse(b.0 .1.len()));
    if let Some(entry) = candidates.first() {
        return Some((*entry).1.clone());
    }

    // Global substring fallback: the model string may carry a proxy
    // tag we couldn't attribute to a provider ("kilo:minimax/…") or
    // the route resolution may have produced an unknown provider. The
    // model name itself is usually distinctive enough to find the
    // entry anyway.
    let mut candidates: Vec<_> = db
        .iter()
        .filter(|((_, m), _)| model_lower.contains(*m))
        .collect();
    candidates.sort_by_key(|b| std::cmp::Reverse(b.0 .1.len()));
    candidates.first().map(|(_, entry)| (*entry).clone())
}

/// Calculate cost for given usage.
pub fn calculate_cost(
    model_name: &str,
    usage: &CanonicalUsage,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> CostResult {
    let route = resolve_billing_route(model_name, provider, base_url);

    if route.billing_mode == BillingMode::SubscriptionIncluded {
        return CostResult {
            amount_usd: Some(0.0),
            status: CostStatus::Included,
            source: CostSource::None,
            label: "included".into(),
        };
    }

    let entry = match get_pricing_entry(model_name, provider, base_url) {
        Some(e) => e,
        None => {
            return CostResult {
                amount_usd: None,
                status: CostStatus::Unknown,
                source: CostSource::None,
                label: "n/a".into(),
            }
        }
    };

    let million = 1_000_000.0_f64;
    let mut amount = 0.0_f64;

    if let Some(input_cost) = entry.input_cost_per_million {
        amount += (usage.input_tokens as f64) * input_cost / million;
    }
    if let Some(output_cost) = entry.output_cost_per_million {
        amount += (usage.output_tokens as f64) * output_cost / million;
    }
    if let Some(cache_read_cost) = entry.cache_read_cost_per_million {
        amount += (usage.cache_read_tokens as f64) * cache_read_cost / million;
    }
    if let Some(cache_write_cost) = entry.cache_write_cost_per_million {
        amount += (usage.cache_write_tokens as f64) * cache_write_cost / million;
    }
    if let Some(request_cost) = entry.request_cost {
        amount += (usage.request_count as f64) * request_cost;
    }

    CostResult {
        amount_usd: Some(amount),
        status: CostStatus::Estimated,
        source: entry.source,
        label: format_cost(amount),
    }
}

/// Format a cost in USD for human display.
pub fn format_cost(usd: f64) -> String {
    if usd == 0.0 {
        "included".to_string()
    } else if usd < 0.01 {
        format!("~${:.4}", usd)
    } else {
        format!("~${:.2}", usd)
    }
}

/// Check whether we have pricing data for a model+route.
pub fn has_known_pricing(model_name: &str, provider: Option<&str>, base_url: Option<&str>) -> bool {
    let route = resolve_billing_route(model_name, provider, base_url);
    if route.billing_mode == BillingMode::SubscriptionIncluded {
        return true;
    }
    get_pricing_entry(model_name, provider, base_url).is_some()
}

/// Backward-compatible thin wrapper returning input/output pricing.
pub fn get_pricing(model_name: &str, provider: Option<&str>, base_url: Option<&str>) -> (f64, f64) {
    match get_pricing_entry(model_name, provider, base_url) {
        Some(entry) => (
            entry.input_cost_per_million.unwrap_or(0.0),
            entry.output_cost_per_million.unwrap_or(0.0),
        ),
        None => (0.0, 0.0),
    }
}

/// Format a token count compactly (e.g. 1.5K, 2.3M).
pub fn format_token_count_compact(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    if value >= 1_000_000_000 {
        return format!("{:.1}B", value as f64 / 1_000_000_000.0);
    }
    if value >= 1_000_000 {
        return format!("{:.1}M", value as f64 / 1_000_000.0);
    }
    let scaled = value as f64 / 1_000.0;
    if scaled < 10.0 {
        format!("{:.2}K", scaled)
    } else if scaled < 100.0 {
        format!("{:.1}K", scaled)
    } else {
        format!("{:.0}K", scaled)
    }
}

/// Format a duration compactly.
pub fn format_duration_compact(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{:.0}s", seconds)
    } else if seconds < 3600.0 {
        format!("{:.0}m", seconds / 60.0)
    } else if seconds < 86400.0 {
        let hours = (seconds / 3600.0) as u32;
        let remaining_min = ((seconds % 3600.0) / 60.0) as u32;
        if remaining_min > 0 {
            format!("{}h {}m", hours, remaining_min)
        } else {
            format!("{}h", hours)
        }
    } else {
        format!("{:.1}d", seconds / 86400.0)
    }
}

// ---------------------------------------------------------------------------
// Normalize raw API usage
// ---------------------------------------------------------------------------

/// Normalize raw API response usage into canonical token buckets.
pub fn normalize_usage(
    raw_usage: &serde_json::Value,
    provider: Option<&str>,
    api_mode: Option<&str>,
) -> CanonicalUsage {
    let provider_name = provider.unwrap_or("").trim().to_lowercase();
    let mode = api_mode.unwrap_or("").trim().to_lowercase();

    let get_u64 = |obj: &serde_json::Value, key: &str| -> u64 {
        obj.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
    };

    if mode == "anthropic_messages" || provider_name == "anthropic" {
        let input_tokens = get_u64(raw_usage, "input_tokens");
        let output_tokens = get_u64(raw_usage, "output_tokens");
        let cache_read = get_u64(raw_usage, "cache_read_input_tokens");
        let cache_write = get_u64(raw_usage, "cache_creation_input_tokens");
        return CanonicalUsage {
            input_tokens,
            output_tokens,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            reasoning_tokens: 0,
            request_count: 1,
        };
    }

    if mode == "codex_responses" {
        let input_total = get_u64(raw_usage, "input_tokens");
        let output_tokens = get_u64(raw_usage, "output_tokens");
        let details = raw_usage.get("input_tokens_details");
        let cache_read = details
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_write = details
            .and_then(|d| d.get("cache_creation_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return CanonicalUsage {
            input_tokens: input_total.saturating_sub(cache_read + cache_write),
            output_tokens,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            reasoning_tokens: 0,
            request_count: 1,
        };
    }

    // OpenAI Chat Completions (default)
    let prompt_total = get_u64(raw_usage, "prompt_tokens");
    let completion_tokens = get_u64(raw_usage, "completion_tokens");
    let details = raw_usage.get("prompt_tokens_details");
    let cache_read = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_write = details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let reasoning_tokens = raw_usage
        .get("output_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    CanonicalUsage {
        input_tokens: prompt_total.saturating_sub(cache_read + cache_write),
        output_tokens: completion_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens,
        request_count: 1,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_cost_openai() {
        let usage = CanonicalUsage {
            input_tokens: 1000,
            output_tokens: 500,
            request_count: 1,
            ..Default::default()
        };
        let result = calculate_cost("gpt-4o", &usage, Some("openai"), None);
        assert_eq!(result.status, CostStatus::Estimated);
        assert!(result.amount_usd.unwrap() > 0.0);
    }

    #[test]
    fn test_subscription_included() {
        let usage = CanonicalUsage {
            input_tokens: 1000,
            output_tokens: 500,
            request_count: 1,
            ..Default::default()
        };
        let result = calculate_cost("gpt-4o", &usage, Some("openai-codex"), None);
        assert_eq!(result.status, CostStatus::Included);
        assert_eq!(result.amount_usd, Some(0.0));
    }

    #[test]
    fn free_proxy_prefix_routes_as_included() {
        // hermix custom free proxy model id
        let route = resolve_billing_route("custom-free:kilo:minimax/minimax-m3", None, None);
        assert_eq!(route.billing_mode, BillingMode::SubscriptionIncluded);
        let usage = CanonicalUsage {
            input_tokens: 1_000_000.0 as u64,
            output_tokens: 100_000,
            ..Default::default()
        };
        let result = calculate_cost("custom-free:kilo:minimax/minimax-m3", &usage, None, None);
        assert_eq!(result.status, CostStatus::Included);
        assert_eq!(result.amount_usd, Some(0.0));
    }

    #[test]
    fn provider_prefix_is_stripped_and_adopted() {
        // "minimax:MiniMax-M3" → provider "minimax", model "minimax-m3"
        let route = resolve_billing_route("minimax:MiniMax-M3", None, None);
        assert_eq!(route.provider, "minimax");
        let entry = get_pricing_entry("minimax:MiniMax-M3", None, None).expect("minimax-m3 entry");
        assert_eq!(entry.input_cost_per_million, Some(0.224));
        assert_eq!(entry.output_cost_per_million, Some(0.88));
        assert_eq!(entry.cache_read_cost_per_million, Some(0.0448));
    }

    #[test]
    fn global_substring_fallback_finds_proxy_tagged_model() {
        // provider tag "kilo" is not in the DB; the model name is.
        let entry = get_pricing_entry("kilo:minimax/minimax-m3.1-flash", None, None)
            .expect("global fallback finds minimax-m3.1-flash");
        assert_eq!(entry.input_cost_per_million, Some(0.10));
    }

    #[test]
    fn deepseek_2026_pricing_with_cache_hit() {
        let usage = CanonicalUsage {
            input_tokens: 400_000, // cache miss
            cache_read_tokens: 600_000,
            output_tokens: 100_000,
            ..Default::default()
        };
        let result = calculate_cost("deepseek:deepseek-chat", &usage, None, None);
        assert_eq!(result.status, CostStatus::Estimated);
        // 0.4 × $0.30 + 0.6 × $0.006 + 0.1 × $1.20
        let expected = 0.4 * 0.30 + 0.6 * 0.006 + 0.1 * 1.20;
        assert!((result.amount_usd.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_stays_unknown() {
        let usage = CanonicalUsage::default();
        let result = calculate_cost("totally-unknown-model-xyz", &usage, None, None);
        assert_eq!(result.status, CostStatus::Unknown);
        assert_eq!(result.amount_usd, None);
    }

    #[test]
    fn test_unknown_model() {
        let usage = CanonicalUsage::default();
        let result = calculate_cost("unknown-model", &usage, Some("unknown"), None);
        assert_eq!(result.status, CostStatus::Unknown);
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.0), "included");
        assert_eq!(format_cost(0.005), "~$0.0050");
        assert_eq!(format_cost(1.50), "~$1.50");
    }

    #[test]
    fn test_format_token_count() {
        assert_eq!(format_token_count_compact(500), "500");
        assert_eq!(format_token_count_compact(1500), "1.50K");
        assert_eq!(format_token_count_compact(1_500_000), "1.5M");
    }

    #[test]
    fn test_normalize_usage_anthropic() {
        let raw = serde_json::json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 50,
        });
        let usage = normalize_usage(&raw, Some("anthropic"), None);
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.cache_write_tokens, 50);
    }

    #[test]
    fn test_has_known_pricing() {
        assert!(has_known_pricing("gpt-4o", Some("openai"), None));
        assert!(has_known_pricing(
            "claude-sonnet-4-20250514",
            Some("anthropic"),
            None
        ));
        assert!(!has_known_pricing("unknown-model", Some("unknown"), None));
    }
}
