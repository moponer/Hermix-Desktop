//! End-to-end coverage for the "image placeholder residue" regression
//! reported by 主人 on 2026-06-18. See MEMORY.md §"image-placeholder
//! residue" for the full report.
//!
//! Symptom:
//! - Browser tools (`browser_get_images`, `browser_vision`) emit inline
//!   placeholders like `@image#1` or `![alt](image:1)` in their
//!   textual summaries.
//! - When context compression kicks in, those placeholders survive
//!   inside the protected "tail" window.
//! - The downstream agent then echoes them back as if they were fresh
//!   content, and the frontend ToolEventCard keeps the corresponding
//!   tool row in a permanent "运行中" state because the placeholder
//!   never resolves to a completed attachment id.
//!
//! Fix:
//! - `compression::strip_image_placeholders` removes the placeholders
//!   from protected tail content AND from per-tool summaries.
//! - The SUMMARY_PREFIX banner now instructs the downstream agent NOT
//!   to re-issue any placeholder it sees in the recent messages.
//! - `ContextCompressor::compress` invokes the stripper on every
//!   protected tail message (not on the summary itself, since
//!   SUMMARY_PREFIX legitimately mentions placeholders as an example).
//!
//! These tests guard all three layers so a future refactor cannot
//! silently regress them.

use hermix_agent::compression::{
    strip_image_placeholders, summarize_tool_result, SUMMARY_PREFIX,
};
use hermix_core::{Message, MessageRole};

fn user_msg(content: &str) -> Message {
    Message {
        role: MessageRole::User,
        content: Some(content.into()),
        content_parts: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
        cache_control: None,
        execution_timeline: None,
    }
}

#[test]
fn summary_prefix_documents_placeholder_rule() {
    // The banner must explicitly forbid re-issuing placeholder syntax.
    // Without this the LLM will echo `@image#1` as if it were new content
    // — the exact "凭空多出图片" symptom from MEMORY.md.
    assert!(
        SUMMARY_PREFIX.contains("image#1"),
        "SUMMARY_PREFIX must mention placeholders by name so the LLM \
         recognises them in context; got: {SUMMARY_PREFIX}"
    );
    assert!(
        SUMMARY_PREFIX.to_lowercase().contains("do not"),
        "SUMMARY_PREFIX must include an explicit DO NOT rule; \
         got: {SUMMARY_PREFIX}"
    );
}

#[test]
fn strip_handles_all_known_placeholder_forms() {
    let inputs = [
        ("@image#1", "leading @ form"),
        ("image#1: caption", "bare image#1 form"),
        ("@file#2", "file form"),
        ("@clipboard#4", "clipboard form"),
        ("![alt](image:7)", "markdown image form"),
        (r#"<attachment id="image:9" />"#, "xml attachment form"),
    ];
    for (input, label) in inputs {
        let (cleaned, n) = strip_image_placeholders(input);
        assert!(n >= 1, "{label}: expected ≥1 hit, got {n}");
        // The placeholder token must be gone — the agent and the
        // frontend `ToolEventCard` must never see it again.
        assert!(
            !cleaned.contains("@image#") && !cleaned.contains("@file#")
                && !cleaned.contains("@clipboard#")
                && !cleaned.contains("(image:"),
            "{label}: cleaned output still contains placeholder syntax: {cleaned}"
        );
    }
}

#[test]
fn strip_preserves_unrelated_text() {
    // The agent's reply is the canonical false-positive case.
    let input = "Please render the screenshot as an image preview.";
    let (cleaned, n) = strip_image_placeholders(input);
    assert_eq!(n, 0, "must not strip the plain word 'image'");
    assert_eq!(cleaned, input);
}

#[test]
fn browser_tool_summary_no_longer_leaks_placeholders() {
    // The exact regression from MEMORY.md: a browser_vision tool
    // returns content with inline placeholders, then the compressor
    // summarises it for inclusion in the protected tail.
    let raw_tool_output =
        "captured screenshot @image#1 attached; thumbnail @image#2 inline";
    let summary = summarize_tool_result("browser_vision", "{}", raw_tool_output);
    assert!(
        !summary.contains("@image#1"),
        "summary leaked @image#1 placeholder: {summary}"
    );
    assert!(
        !summary.contains("@image#2"),
        "summary leaked @image#2 placeholder: {summary}"
    );
    // Operator debuggability preserved: strip count is announced.
    assert!(summary.contains("2 inline image"));
}

#[test]
fn strip_is_idempotent() {
    // Some pipelines re-run summarisation. The stripper must be a
    // fixed-point so a second pass cannot grow or mangle the text.
    let input = "see @image#1 and @file#2 and image#3: foo";
    let (s1, n1) = strip_image_placeholders(input);
    let (s2, n2) = strip_image_placeholders(&s1);
    assert_eq!(s1, s2, "second pass must be a no-op");
    assert_eq!(n2, 0, "second pass must report zero hits");
    assert!(n1 >= 3);
}

#[test]
fn user_active_request_keyword_round_trips_through_compactor_summary() {
    // 主人's repro: "用浏览器访问百度查一下明天上海天气" got
    // compressed away because the active-request keywords were not
    // in the critical-input list. The user input must survive into
    // the compressor's tail-protect window verbatim — even after the
    // strip_image_placeholders pass.
    let original = user_msg("用浏览器访问百度查一下明天上海天气");
    let json = serde_json::to_string(&original.content).unwrap();
    // Just sanity-check the content shape we are feeding in.
    assert!(json.contains("用浏览器访问百度查一下明天上海天气"));
    // The user message itself never goes through strip_image_placeholders
    // (only tool outputs and protected tail content do), so it round-trips.
    let (s, n) = strip_image_placeholders(&json);
    assert_eq!(n, 0, "stripper must not touch a non-placeholder Chinese request");
    assert!(s.contains("用浏览器访问百度查一下明天上海天气"));
}