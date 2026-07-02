//! Day 2 smoke verification: print per-model context resolution for known
//! + unknown + custom-provider model names. Run via:
//!
//! ```bash
//! cargo run --example resolve_context -p hermix-agent
//! ```
//!
//! This is a no-provider-config smoke test: it goes through
//! `hermix_intelligence::model_metadata::get_model_context_length` (the
//! static catalog → fuzzy fallback → DEFAULT_FALLBACK_CONTEXT chain)
//! and prints the resolved `max_context_tokens` for each model, then
//! computes the 80% pressure threshold via the new
//! `ContextManager::pressure_threshold_tokens` method. Day 3 added the
//! `output_reserve_tokens` column so the printed pressure value is
//! honest about the headroom kept free for the LLM response.
//!
//! Pre-fix (Day 1): every model got `200_000`.
//! Post-fix (Day 2): each model resolves from its real catalog entry.
//! Post-fix (Day 3): columns renamed chars→tokens; reserve subtracted
//! before the ratio, so pressure = (max - reserve) * 0.8.

use hermix_agent::context::{ContextManager, DEFAULT_OUTPUT_RESERVE_TOKENS};

fn main() {
    let models: &[&str] = &[
        // Known catalog hits
        "gpt-4o",
        "gpt-5",
        "gpt-4.1",
        "claude-3-5-sonnet",
        "claude-opus-4-6",
        "gemini-1.5-pro",
        "deepseek-chat",
        "grok-4",
        "kimi-k2",
        "glm-4.6",
        "qwen3-coder-plus",
        // Fuzzy fallback hits
        "llama-3.3-70b",
        "minimax-text-01",
        // Unknown / custom provider
        "custom-agnes:agnes-2.0-flash",
        "my-custom-model",
        "",
    ];

    println!(
        "{:<40} {:>15} {:>15} {:>15} {:>15}",
        "model", "max_ctx (tok)", "reserve", "pressure (80%)", "warn @95%"
    );
    println!("{}", "-".repeat(105));

    for m in models {
        let cm = ContextManager::for_model(m);
        let max = cm.max_context_tokens();
        let reserve = cm.output_reserve_tokens();
        let pressure = cm.pressure_threshold_tokens();
        // 95% of pressure threshold is where the high-tier warn fires.
        // Bypass the reserve subtraction when computing the warn line so
        // the column reads "tokens used at which we warn", not "tokens
        // at which we trigger pressure".
        let warn95 = ((max.saturating_sub(reserve)) as f64 * 0.95) as usize;
        let label = if m.is_empty() { "(empty)" } else { m };
        println!(
            "{:<40} {:>15} {:>15} {:>15} {:>15}",
            label, max, reserve, pressure, warn95
        );
    }

    eprintln!(
        "\nDay 3 note: DEFAULT_OUTPUT_RESERVE_TOKENS = {} (subtracted before 80% ratio)",
        DEFAULT_OUTPUT_RESERVE_TOKENS
    );
}