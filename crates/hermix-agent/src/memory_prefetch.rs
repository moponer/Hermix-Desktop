//! Memory subsystem helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, 2026-07-04) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No behavior
//! change** — pure relocation.
//!
//! Items living here:
//! - `memory_prefetch` — recall memory hits via `MemoryRouter`, dedup,
//!   filter by salience floor, sort, and render a markdown block
//!   respecting the char budget.
//! - `memory_sync` — route per-turn sync to the router + write
//!   user-style / fact-extraction rows to the embedded backend.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use hermix_focus::{extract_time_window, fact_extractor, user_style, MemoryHit, MemoryLayer, MemoryQuery};
use hermix_intelligence::embedding::EmbeddingProvider;

use crate::agent_config::AgentConfig;

/// Run the memory prefetch pipeline and return a markdown block
/// (`"## Memory\n\n..."`) sized to the configured char budget.
///
/// Honours `config.skip_memory` (returns empty), `config.memory_min_salience`
/// (drops low-signal hits), `config.memory_char_budget` (clamps total chars),
/// and `config.memory_recall_limit_{first,subsequent}` (caps hit count).
pub async fn memory_prefetch(
    config: &AgentConfig,
    router: Option<&hermix_focus::MemoryRouter>,
    embedding_provider: Option<&dyn EmbeddingProvider>,
    recall_seeded: &AtomicBool,
    hermix_home: Option<&Path>,
    query: &str,
    session_id: &str,
) -> String {
    if config.skip_memory {
        return String::new();
    }

    // M2 (2026-07-03): recall gating + budget pulled from config.
    // Defaults are 50 first / 5 subsequent recall, 4000 char block
    // cap, 0.30 salience floor. Tunables were previously hardcoded.
    let is_seeded = recall_seeded.load(Ordering::Relaxed);
    let limit = if is_seeded {
        config.memory_recall_limit_subsequent
    } else {
        config.memory_recall_limit_first
    };
    let char_budget = config.memory_char_budget;
    let min_salience = config.memory_min_salience;

    // --- Phase 15: Time-aware recall via MemoryLayer path ----------
    let time_window = extract_time_window(query);

    let recall_start = std::time::Instant::now();
    let structured_hits: Vec<MemoryHit> = if let Some(router) = router {
        let mut q = MemoryQuery::recall(query)
            .with_limit(limit)
            .with_session_id(session_id.to_string());
        if let Some(tw) = time_window {
            q = q.with_time_window(tw);
        }
        // Generate embedding for vector recall if provider is available
        if let Some(ep) = embedding_provider {
            match ep.embed(query).await {
                Ok(embedding) => {
                    q.embedding = Some(embedding);
                }
                Err(e) => tracing::debug!("embedding failed (falling back to text recall): {e}"),
            }
        }
        match router.recognize(&q).await {
            Ok(hits) => hits,
            Err(e) => {
                tracing::warn!("time-aware memory recall failed: {e}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let recall_latency_us = recall_start.elapsed().as_micros() as i64;

    // Mark seeded after the first full recall so subsequent turns
    // use a smaller limit.
    if !is_seeded {
        recall_seeded.store(true, Ordering::Release);
    }

    // Phase 18 (P1-1): record recall_audit row — gives operators a
    // window into recall quality without inspecting logs. Skipped
    // silently if the embedded backend isn't wired (e.g. server
    // mode with remote backends only).
    if let Some(router) = router {
        if let Some(embedded) = router.find_embedded_backend() {
            let query_hash = {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                query.hash(&mut h);
                format!("{:016x}", h.finish())
            };
            if let Err(e) = embedded.record_recall_audit(
                session_id,
                query,
                &query_hash,
                structured_hits.len() as i64,
                recall_latency_us,
            ) {
                tracing::debug!(
                    target: "hermix.agent",
                    error = %e,
                    "recall_audit insert failed (non-fatal)"
                );
            }
        }
    }

    // Phase 18 (P2-2): dedup hits by content (recurring facts
    // surface from frames + facts + memory_tool). Keep the
    // highest-salience copy and merge access_count if visible.
    let mut deduped: Vec<MemoryHit> = Vec::with_capacity(structured_hits.len());
    for h in structured_hits {
        if let Some(existing) = deduped.iter_mut().find(|e| e.content == h.content) {
            // Boost: take max salience; bump access_count via the
            // embedded backend so frequently-shown hits naturally
            // float up.
            existing.salience = existing.salience.max(h.salience);
            if let Some(router) = router {
                if let Some(embedded) = router.find_embedded_backend() {
                    if let Some(fact_key) = h.metadata.get("canonical_key").and_then(|v| v.as_str())
                    {
                        let _ = embedded.boost_fact_access(fact_key);
                    }
                }
            }
        } else {
            deduped.push(h);
        }
    }
    // Sort by salience desc so the most relevant hits land in the
    // limited-char budget first.
    deduped.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Inject structured hits as a markdown block with token budget.
    // M2: char budget + salience floor come from config (defaults
    // 4000 chars / 0.30 salience — see AgentConfig).
    let mut block = String::from("## Memory\n\n");
    let mut total_chars = 0;

    // Include MEMORY.md content as high-priority hits (file-based memory)
    if let Some(hh) = hermix_home {
        let memory_path = hh.join("memories").join("MEMORY.md");
        if let Ok(content) = std::fs::read_to_string(&memory_path) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                let line = format!("- [source=memo] {}\n", trimmed);
                if total_chars + line.len() <= char_budget {
                    block.push_str(&line);
                    total_chars += line.len();
                }
            }
        }
    }

    for h in &deduped {
        // M2: salience noise floor — drop low-signal fragments
        // before they consume char_budget.
        if h.salience < min_salience {
            continue;
        }
        // Source-aware label: facts get `[fact]`, memory_write
        // gets `[memo]`, turns get `[turn]`, otherwise generic.
        let source_tag = match h.source.as_str() {
            "curation" | "fact_extractor" => "[fact]",
            "memory_tool" => "[memo]",
            "turn" => "[turn]",
            "session" => "[session]",
            other => {
                // unknown source — generic tag, but include the
                // raw value so debugging is easy.
                if other.is_empty() {
                    ""
                } else {
                    "[src]"
                }
            }
        };
        let line = format!("- {} [salience={:.2}] {}\n", source_tag, h.salience, h.content);
        if total_chars + line.len() > char_budget {
            break;
        }
        block.push_str(&line);
        total_chars += line.len();
    }
    block
}

/// Run the memory sync side-effects for one turn:
/// 1. Route to `MemoryRouter::sync_turn`.
/// 2. Extract facts from the user turn and upsert them into the embedded
///    backend's `facts` table.
/// 3. Run `user_style::detect_user_style` and write a `user.style:<style>`
///    fact if a style was detected with sufficient confidence.
pub fn memory_sync(
    config: &AgentConfig,
    router: Option<&hermix_focus::MemoryRouter>,
    user: &str,
    assistant: &str,
    session_id: &str,
) {
    if config.skip_memory {
        return;
    }
    // Route to MemoryRouter (all backends) — replaces legacy
    // `PluginManager::memory_providers` fan-out.
    if let Some(router) = router {
        router.sync_turn(user, assistant, session_id);
    }
    // Phase 18 (P0-3): facts 表 caller — 从 user turn 抽取关键陈述
    // 并写入 SQLite `facts` 表，让跨 session 召回（memory_prefetch
    // 里的 `MemoryRouter::recognize`）能命中 user-asserted facts。
    // 仅对 embedded backend 调用（其他 backend 没 facts 表）。
    if let Some(router) = router {
        if let Some(embedded) = router.find_embedded_backend() {
            let facts = fact_extractor::extract_facts_from_turn(user, session_id, 0);
            for fact in facts {
                if let Err(e) = embedded.upsert_fact(
                    &fact.canonical_key,
                    fact.payload.clone(),
                    0.6, // user_asserted facts: slightly below memory_write (0.7)
                    &fact.tags,
                    "fact_extractor",
                ) {
                    tracing::debug!(
                        target: "hermix.agent",
                        key = %fact.canonical_key,
                        error = %e,
                        "fact_extractor: upsert_fact failed"
                    );
                }
            }
        }
    }

    // P2-6 (2026-06-27): dialectic user-style detection. We write
    // ONE fact per turn with `kind = user.style:<style>` so the
    // tag dedups across sessions via `canonical_key UNIQUE`. The
    // LLM never has to ask the user "are you direct or
    // exploratory?" — we infer it from the prompt shape and
    // surface it as a fact row that future turns can pull in.
    //
    // Best-effort: a `detect_user_style` miss (no rule hits
    // threshold) just means we don't write anything, which is
    // safer than a noisy tag competing for salience.
    if let Some(router) = router {
        if let Some(embedded) = router.find_embedded_backend() {
            if let Some(detected) = user_style::detect_user_style(user) {
                let key = format!("user.style:{}", detected.style);
                let payload = serde_json::json!({
                    "kind": "user.style",
                    "style": detected.style,
                    "confidence": detected.confidence,
                    "evidence": detected.evidence,
                    "session_id": session_id,
                    "turn_number": 0,
                });
                let tags = vec![
                    format!("session:{session_id}"),
                    "user.style".to_string(),
                    "dialectic".to_string(),
                ];
                if let Err(e) = embedded.upsert_fact(
                    &key,
                    payload,
                    // Style facts sit at 0.5 — below preferences
                    // (0.6) but above ephemeral session metadata.
                    0.5,
                    &tags,
                    "user_style_detector",
                ) {
                    tracing::debug!(
                        target: "hermix.agent",
                        key = %key,
                        error = %e,
                        "user_style: upsert_fact failed"
                    );
                }
            }
        }
    }
}