//! Memory subsystem helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No behavior
//! change** — pure relocation.
//!
//! Items living here:
//! - `memory_prefetch` — recall memory hits via `MemoryRouter`, dedup,
//!   filter by salience floor, sort, and render a markdown block
//!   respecting the char budget.
//! - `memory_sync` — route per-turn sync to the router + write
//!   user-style / fact-extraction rows to the embedded backend.
//!
//! ## `recall_seeded` is per-`AgentLoop`, not global
//!
//! `memory_prefetch` takes `recall_seeded: &AtomicBool` so the first
//! turn of an `AgentLoop` uses `memory_recall_limit_first` (default 50)
//! and subsequent turns drop to `memory_recall_limit_subsequent`
//! (default 5). The flag is **owned by `AgentLoop`** — see
//! `AgentLoop::memory_recall_seeded` in `agent_loop.rs` — and is
//! initialised fresh per `AgentLoop::new`. Each session / user
//! conversation gets its own `AgentLoop` instance, so the flag cannot
//! leak between sessions in the desktop process model (one process
//! per session).
//!
//! Multi-tenant server deployments that share one process across
//! sessions would still see per-`AgentLoop` isolation as long as
//! each session spins up its own `AgentLoop`. If a future deployment
//! pattern ever reuses one `AgentLoop` across sessions (e.g.
//! pooling), the recall budget would degrade after the first
//! session's first turn — at that point switch `recall_seeded` to
//! a `HashMap<SessionId, AtomicBool>` keyed by session id. Documented
//! here so the assumption is visible at the recall site, not buried
//! in `AgentLoop`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hermix_focus::{
    extract_time_window, fact_extractor, user_style, FocusConclusion, FocusStack, MemoryHit,
    MemoryLayer, MemoryQuery, MemoryRouter, SessionFrame,
};
use hermix_intelligence::embedding::EmbeddingProvider;

use crate::agent_config::AgentConfig;
use crate::agent_helpers::AgentCallbacks;
use crate::consciousness::ConsciousnessTracker;
use crate::focus_stack_ops::{current_focus_depth, note_delegation_end};
use crate::memory_hooks::{build_session_summary, delegation_event_from_tool_result, memory_write_event_from_tool_call};
use hermix_core::{Message, MessageRole, ToolCall, ToolResult};
use hermix_focus::SessionFrameStatus;
use serde_json::Value;

//── Memory-operation surfacing ──────────────────────
// These types describe a single memory mutation so the agent can
// emit a `StreamEvent::MemoryUpdate` (bridged via the
// `status_callback` "memory_update" event) and the front-end can
// render it inline in the conversation. Keeping them here — at the
// `memory_*` function layer — means every call site that performs a
// mutation can return what it did without threading event plumbing
// through the call stack.

/// What kind of mutation happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryAction {
    /// New knowledge stored (fact extraction, `memory` add).
    Write,
    /// Existing entry overwritten (`memory` replace).
    Update,
    /// Entry removed (`memory` remove, stale-frame cleanup).
    Delete,
    /// A summary / conclusion was persisted (delegation, session end).
    Summarize,
    /// Old / low-salience frames were pruned from the table.
    Cleanup,
    /// A periodic checkpoint snapshot was frozen.
    Snapshot,
}

impl MemoryAction {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryAction::Write => "write",
            MemoryAction::Update => "update",
            MemoryAction::Delete => "delete",
            MemoryAction::Summarize => "summarize",
            MemoryAction::Cleanup => "cleanup",
            MemoryAction::Snapshot => "snapshot",
        }
    }
}

/// Which memory store the mutation targeted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    /// A user-asserted / extracted fact row.
    Fact,
    /// An inferred user-style fact (`user.style:<x>`).
    Style,
    /// A session / delegation summary / conclusion.
    Summary,
    /// An explicit `memory` tool operation.
    Tool,
    /// A frozen session snapshot.
    Snapshot,
    /// (E): a session-end reflection (template-driven,
    /// not LLM-generated). Surfaced as a `MemoryUpdate` so the user
    /// sees "what the agent thinks it learned".
    Reflection,
    /// A line appended to `daily_memory[今天].conclusions` at
    /// session end (deterministic, no LLM).
    Daily,
    /// Session-scope `MEMORY.md`/`USER.md` entries promoted into
    /// memory.db facts at session end, so explicit `memory` writes
    /// survive across sessions despite the per-session file scope.
    Promotion,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Fact => "fact",
            MemoryKind::Style => "style",
            MemoryKind::Summary => "summary",
            MemoryKind::Tool => "tool",
            MemoryKind::Snapshot => "snapshot",
            MemoryKind::Reflection => "reflection",
            MemoryKind::Daily => "daily",
            MemoryKind::Promotion => "promotion",
        }
    }
}

/// One memory mutation, returned by the `memory_*` helpers so the
/// caller (an `AgentLoop` method with `status_callback` access) can
/// emit a `MemoryUpdate` event.
#[derive(Debug, Clone)]
pub struct MemoryWriteRecord {
    pub action: MemoryAction,
    pub memory_type: MemoryKind,
    /// Canonical key (fact `canonical_key`, `memory` `target`, or
    /// `"session"` for session-level summaries).
    pub key: String,
    /// One-line human-readable description of what changed.
    pub summary: String,
    /// Batch size when several operations fold into one record
    /// (e.g. facts extracted from a single user turn).
    pub count: Option<u64>,
}

/// Fire a `memory_update` status event from inside a `memory_*`
/// helper that has `AgentCallbacks` access. Honors `quiet_mode` via
/// the `config` flag passed by the caller. No-op when `callbacks`
/// is `None` (e.g. tests / non-interactive runs).
pub fn emit_memory_update_event(
    callbacks: Option<&AgentCallbacks>,
    record: &MemoryWriteRecord,
) {
    let Some(cb) = callbacks else { return };
    let payload = serde_json::json!({
        "action": record.action.as_str(),
        "memory_type": record.memory_type.as_str(),
        "key": record.key,
        "summary": record.summary,
        "count": record.count,
    });
    if let Some(scb) = cb.status_callback.as_ref() {
        scb("memory_update", &payload.to_string());
    }
}


/// Run the memory prefetch pipeline and return a markdown block
/// (`"## Memory\n\n..."`) sized to the configured char budget.
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
    messages: Option<&[Message]>,
) -> String {
    if config.skip_memory {
        return String::new();
    }

    //: recall gating + budget pulled from config.
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

    //Time-aware recall via MemoryLayer path ----------
    let time_window = extract_time_window(query);

    // Extract task context from recent messages for better recall
    let task_context = messages.map(|msgs| extract_task_context(msgs));

    let recall_start = std::time::Instant::now();
    let structured_hits: Vec<MemoryHit> = if let Some(router) = router {
        // Enhance query with task context for better recall
        let enhanced_query = if let Some(ref tc) = task_context {
            let mut q = query.to_string();
            if !tc.active_files.is_empty() {
                q.push_str(&format!(" files: {}", tc.active_files.join(", ")));
            }
            if !tc.recent_errors.is_empty() {
                q.push_str(&format!(" error: {}", tc.recent_errors[0]));
            }
            q
        } else {
            query.to_string()
        };

        let mut q = MemoryQuery::recall(&enhanced_query)
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

    //: record recall_audit row — gives operators a
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

    //: dedup hits by content (recurring facts
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
    //char budget + salience floor come from config (defaults
    // 4000 chars / 0.30 salience — see AgentConfig).
    let mut block = String::from("## Memory\n\n");
    let mut total_chars = 0;

    // Include the agent's scoped MEMORY.md as a high-priority hit
    // (file-based memory). `config.resolved_memory_dir()` is this agent's
    // isolated memory directory — either the per-session dir (private 1:1
    // chat) or the per-room dir (group chat). For the auto-derived private
    // scope only, fall back to the legacy global `<home>/memories` when the
    // scoped dir has no MEMORY.md yet, so pre-existing private memories stay
    // visible during migration. Group-room scopes must never read global.
    let scope_dir = config.resolved_memory_dir();
    let memory_path = scope_dir.join("MEMORY.md");
    let global_path = hermix_home.map(|h| h.join("memories").join("MEMORY.md"));
    let content = std::fs::read_to_string(&memory_path)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            if config.memory_reads_global_fallback() {
                global_path
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .filter(|s| !s.trim().is_empty())
            } else {
                None
            }
        });
    if let Some(content) = content {
        let trimmed = content.trim();
        let line = format!("- [source=memo] {}\n", trimmed);
        if total_chars + line.len() <= char_budget {
            block.push_str(&line);
            total_chars += line.len();
        }
    }

    for h in &deduped {
        //salience noise floor — drop low-signal fragments
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

    //(reflection → 行为改写闭环): append a `## Prior
    // Reflections` section listing the most recent cross-session
    // reflections so the agent's next turn starts with the lessons
    // learned last session. Disabled when config.prior_reflection_max_items
    // is 0 or no embedded backend is wired.
    // The block clamps itself to char_budget so we never exceed the
    // total budget. Errors are swallowed — the rest of the `## Memory`
    // block stays intact and the user sees noise-free degradation.
    if config.prior_reflection_max_items > 0 {
        if let Some(embedded) = router.and_then(|r| r.find_embedded_backend()) {
            match embedded.list_cross_session_reflections(
                session_id,
                config.prior_reflection_max_items as u32,
            ) {
                Ok(prior) if !prior.is_empty() => {
                    // Adapt `hermix_focus::Reflection` to the
                    // generator-side `Reflection` (kind/content/salience).
                    // We drop id/session_id — they're metadata for
                    // tracing, not for the prompt body. The
                    // `kind: &'static str` requirement forces us to
                    // own the discriminator and compare eagerly.
                    let adapted: Vec<crate::reflection_generator::Reflection> = prior
                        .into_iter()
                        .map(|r| {
                            let k: &'static str = match r.kind.as_str() {
                                "what_went_well" => "what_went_well",
                                "what_to_improve" => "what_to_improve",
                                "user_preferences_observed" => "user_preferences_observed",
                                "next_session_hint" => "next_session_hint",
                                _ => "reflection",
                            };
                            crate::reflection_generator::Reflection {
                                kind: k,
                                content: r.content,
                                salience: r.salience,
                            }
                        })
                        .collect();
                    if let Some(section) =
                        crate::reflection_generator::build_prior_session_reflection_block(
                            &adapted,
                            config.prior_reflection_per_item_chars,
                            config.prior_reflection_max_items,
                        )
                    {
                        let header = "\n\n## Prior Reflections\n\n";
                        let with_header = format!("{header}{section}\n");
                        if total_chars + with_header.len() <= char_budget {
                            block.push_str(&with_header);
                            // Note: total_chars not updated here because this is
                            // the last budget check in the function — no subsequent
                            // reads need the updated value.
                        }
                    }
                }
                Ok(_) => { /* empty — nothing to surface */ }
                Err(e) => {
                    tracing::debug!(
                        target: "hermix.agent",
                        session_id = %session_id,
                        error = %e,
                        "memory_prefetch: list_cross_session_reflections failed (non-fatal)"
                    );
                }
            }
        }
    }
    block
}

/// Run the memory sync side-effects for one turn:
/// 1. Route to `MemoryRouter::sync_turn`.
/// 2. Extract facts from the user turn and upsert them into the embedded
/// backend's `facts` table.
/// 3. Run `user_style::detect_user_style` and write a `user.style:<style>`
/// fact if a style was detected with sufficient confidence.
#[tracing::instrument(
    name = "memory_sync",
    level = "debug",
    skip_all,
    fields(
        session_id = %session_id,
        user_chars = user.chars().count(),
        assistant_chars = assistant.chars().count(),
        fact_writes = tracing::field::Empty,
    )
)]
pub fn memory_sync(
    config: &AgentConfig,
    router: Option<&hermix_focus::MemoryRouter>,
    user: &str,
    assistant: &str,
    session_id: &str,
) -> Vec<MemoryWriteRecord> {
    let mut records: Vec<MemoryWriteRecord> = Vec::new();
    if config.skip_memory {
        return records;
    }
    // Route to MemoryRouter (all backends) — replaces legacy
    // `PluginManager::memory_providers` fan-out.
    if let Some(router) = router {
        router.sync_turn(user, assistant, session_id);
    }
    //: facts 表 caller — 从 user turn 抽取关键陈述
    // 并写入 SQLite `facts` 表，让跨 session 召回（memory_prefetch
    // 里的 `MemoryRouter::recognize`）能命中 user-asserted facts。
    // 仅对 embedded backend 调用（其他 backend 没 facts 表）。
    if let Some(router) = router {
        if let Some(embedded) = router.find_embedded_backend() {
            let facts = fact_extractor::extract_facts_from_turn(user, session_id, 0);
            for fact in facts {
                let fact_text = fact
                    .payload
                    .get("text")
                    .or_else(|| fact.payload.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&fact.canonical_key)
                    .to_string();
                if embedded
                    .upsert_fact(
                        &fact.canonical_key,
                        fact.payload.clone(),
                        0.6, // user_asserted facts: slightly below memory_write (0.7)
                        &fact.tags,
                        "fact_extractor",
                    )
                    .is_ok()
                {
                    records.push(MemoryWriteRecord {
                        action: MemoryAction::Write,
                        memory_type: MemoryKind::Fact,
                        key: fact.canonical_key.clone(),
                        summary: fact_text,
                        count: None,
                    });
                } else {
                    tracing::debug!(
                        target: "hermix.agent",
                        key = %fact.canonical_key,
                        "fact_extractor: upsert_fact failed"
                    );
                }
            }
        }
    }

    //: dialectic user-style detection. We write
    // ONE fact per turn with `kind = user.style:<style>` so the
    // tag dedups across sessions via `canonical_key UNIQUE`. The
    // LLM never has to ask the user "are you direct or
    // exploratory?" — we infer it from the prompt shape and
    // surface it as a fact row that future turns can pull in.
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
                if embedded
                    .upsert_fact(
                        &key,
                        payload,
                        // Style facts sit at 0.5 — below preferences
                        // (0.6) but above ephemeral session metadata.
                        0.5,
                        &tags,
                        "user_style_detector",
                    )
                    .is_ok()
                {
                    // latest-wins 互斥：此前 `user.style:{style}` 每种
                    // 风格各占一行，direct/technical/mixed/exploratory
                    // 四条互相矛盾的探测并存。隐藏其余可见风格行后，
                    // 召回与 `most_frequent_user_style` 只见最新推断。
                    let _ = embedded.soft_hide_other_user_styles(&key);
                    records.push(MemoryWriteRecord {
                        action: MemoryAction::Write,
                        memory_type: MemoryKind::Style,
                        key,
                        summary: format!("用户风格推断：{}", detected.style),
                        count: None,
                    });
                } else {
                    tracing::debug!(
                        target: "hermix.agent",
                        key = %key,
                        "user_style: upsert_fact failed"
                    );
                }
            }
        }
    }
    records
}
/// (架构-1): freeze the current session state into a
/// `session_snapshots` row. Called every N turns (default 50)
/// and on `on_session_end`. The `frame_ids` and `summary` are
/// extracted from the router's embedded backend so this method
/// is a no-op when the embedded backend is absent.
/// Designed to be cheap: the embedded SQLite path is O(visible
/// frames) on `total_turns.is_multiple_of(N)` boundary calls,
/// and O(1) on all other turns.
pub fn maybe_freeze_snapshot(
    config: &AgentConfig,
    router: Option<&hermix_focus::MemoryRouter>,
    session_id: &str,
    turn_number: u32,
) {
    if config.skip_memory {
        return;
    }
    let Some(router) = router else {
        return;
    };
    let Some(embedded) = router.find_embedded_backend() else {
        return;
    };
    // Snapshot at every 50 turns + at the final turn. Avoid
    // snapshotting every turn (would saturate the table).
    const SNAPSHOT_INTERVAL: u32 = 50;
    let should_snapshot = turn_number > 0
        && (turn_number.is_multiple_of(SNAPSHOT_INTERVAL) || turn_number == config.max_turns);
    if !should_snapshot {
        return;
    }
    // Build a short summary of the most recent turn. We pull
    // the user message verbatim from the in-process message
    // context if available; otherwise we fall back to a
    // terse placeholder.
    let summary = format!("Frozen at turn {turn_number} of session {session_id}");
    let frame_ids: Vec<String> = Vec::new();
    match embedded.record_session_snapshot(session_id, turn_number, &summary, &frame_ids) {
        Ok(_) => tracing::debug!(
            target: "hermix.agent",
            session_id,
            turn_number,
            "snapshot frozen"
        ),
        Err(e) => tracing::warn!(
            target: "hermix.agent",
            session_id,
            turn_number,
            error = %e,
            "snapshot freeze failed"
        ),
    }
}

/// (架构-2): enforce a hard cap on visible frames so
/// memory.db doesn't grow unbounded. Called every 100 turns as a
/// background cleanup. Default cap is 10000 — enough for ~3
/// months of heavy chat, soft-hidden rows stay on disk for the
/// audit trail and the 30-day janitor.
pub fn maybe_enforce_frame_cap(
    config: &AgentConfig,
    router: Option<&hermix_focus::MemoryRouter>,
    turn_number: u32,
) -> usize {
    if config.skip_memory {
        return 0;
    }
    const CAP_INTERVAL: u32 = 100;
    const FRAME_CAP: usize = 10_000;
    if !turn_number.is_multiple_of(CAP_INTERVAL) || turn_number == 0 {
        return 0;
    }
    let Some(router) = router else {
        return 0;
    };
    let Some(embedded) = router.find_embedded_backend() else {
        return 0;
    };
    match embedded.enforce_frame_cap(FRAME_CAP) {
        Ok(0) => 0,
        Ok(n) => {
            tracing::info!(
                target: "hermix.agent",
                hidden = n,
                cap = FRAME_CAP,
                "frame cap enforced (oldest/lowest-salience hidden)"
            );
            n
        }
        Err(e) => {
            tracing::warn!(
                target: "hermix.agent",
                error = %e,
                "frame cap enforcement failed"
            );
            0
        }
    }
}

/// Route `memory` tool calls (action=add/replace/remove) through
/// `MemoryRouter::on_memory_write` — replaces legacy
/// `PluginManager::memory_providers` fan-out.
pub fn notify_memory_writes(
    config: &AgentConfig,
    router: Option<&hermix_focus::MemoryRouter>,
    tool_calls: &[ToolCall],
    results: &[ToolResult],
) -> Vec<MemoryWriteRecord> {
    let mut records: Vec<MemoryWriteRecord> = Vec::new();
    if config.skip_memory {
        return records;
    }
    let Some(router) = router else {
        return records;
    };
    for result in results {
        if result.is_error {
            continue;
        }
        let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
            continue;
        };
        let Some((action, target, content)) = memory_write_event_from_tool_call(tc) else {
            continue;
        };
        router.on_memory_write(&action, &target, &content);
        // Map the `memory` tool action verb to our surfacing action.
        let mem_action = match action.as_str() {
            "add" => MemoryAction::Write,
            "replace" => MemoryAction::Update,
            "remove" => MemoryAction::Delete,
            _ => MemoryAction::Write,
        };
        let summary = if content.trim().is_empty() {
            format!("记忆 {action}：{target}")
        } else {
            // Keep the inline summary short so the chat row stays readable.
            let truncated: String = content
                .chars()
                .take(80)
                .collect();
            let ellipsis = if content.chars().count() > 80 { "…" } else { "" };
            format!("记忆 {action}「{target}」：{truncated}{ellipsis}")
        };
        records.push(MemoryWriteRecord {
            action: mem_action,
            memory_type: MemoryKind::Tool,
            key: target,
            summary,
            count: None,
        });
    }
    records
}

/// Route a `on_turn_start` notification through `MemoryRouter`.
/// No-op when `skip_memory` is set or no router is attached.
#[tracing::instrument(
    name = "memory_on_turn_start",
    level = "debug",
    skip_all,
    fields(
        turn = turn,
        router_attached = router.is_some(),
    )
)]
pub fn memory_on_turn_start(
    config: &AgentConfig,
    router: Option<&MemoryRouter>,
    turn: u32,
    message: &str,
) {
    if config.skip_memory {
        return;
    }
    if let Some(router) = router {
        router.on_turn_start(turn, message);
    }
}

/// Ask `MemoryRouter` for a pre-compression note to splice into the
/// compression summary. Returns `None` when `skip_memory` is set,
/// no router is attached, or the router produced an empty string.
pub fn memory_pre_compress_note(
    config: &AgentConfig,
    router: Option<&MemoryRouter>,
    messages: &[Message],
) -> Option<String> {
    if config.skip_memory {
        return None;
    }
    let router = router?;
    let as_values: Vec<Value> = messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    let combined = router.on_pre_compress(&as_values);
    if combined.trim().is_empty() {
        None
    } else {
        Some(combined)
    }
}

/// Drain pending `FocusConclusion`s from the focus stack and forward
/// them to the long-term memory backend. Returns the number of
/// conclusions processed.
/// No-op when no router is attached — conclusions are still drained
/// from the focus stack so they don't pile up. Each conclusion is
/// persisted via `router.upsert_conclusion` (async, fan-out to all
/// backends) and, when the embedded backend is present, a
/// deterministic compression fact row is backfilled into the `facts`
/// table for cross-session recall.
pub fn drain_focus_conclusions(
    focus_stack: &Mutex<FocusStack<SessionFrame>>,
    router: Option<&Arc<MemoryRouter>>,
    session_id: &str,
) -> usize {
    let conclusions: Vec<FocusConclusion> = {
        let Ok(mut stack) = focus_stack.lock() else {
            return 0;
        };
        stack.drain_conclusions()
    };
    if conclusions.is_empty() {
        return 0;
    }
    let Some(router_ref) = router else {
        tracing::debug!(
            target: "hermix.agent",
            count = conclusions.len(),
            "drain_focus_conclusions: no memory_router attached, dropping conclusions"
        );
        return 0;
    };
    let session_id = session_id.to_string();
    // Clone the Arc so the async task owns its handle — keeps the
    // caller's borrow free after we return.
    let router: Arc<MemoryRouter> = Arc::clone(router_ref);
    let count = conclusions.len();
    //(BaiLongma compression backfill): deterministic
    // fact extraction. Each conclusion becomes a row in the
    // `facts` table with a canonical_key derived from
    // (session_id, source_frame_id) so re-running is idempotent.
    // Sync (SQLite is sub-ms) — done before spawning the async
    // persistence so the fact row lands in the same SQLite
    // connection window as the conclusion row.
    // Only on the embedded backend (the only one that owns the
    // `facts` table); the router-level `upsert_conclusion` already
    // fans out to every backend.
    for c in &conclusions {
        if let Some(embedded) = router.find_embedded_backend() {
            if let Err(e) = embedded.backfill_compression_fact(&session_id, c) {
                tracing::warn!(
                    target: "hermix.agent",
                    session_id = %session_id,
                    conclusion_id = %c.source_frame_id,
                    error = %e,
                    "drain_focus_conclusions: backfill_compression_fact failed"
                );
            }
        }
    }
    // Persist asynchronously — don't block the agent loop on SQLite
    // writes. Errors are logged via the router's tolerate_errors
    // policy (default: log + continue).
    tokio::spawn(async move {
        for c in &conclusions {
            if let Err(e) = router.upsert_conclusion(c).await {
                tracing::warn!(
                    target: "hermix.agent",
                    session_id = %session_id,
                    conclusion_id = %c.source_frame_id,
                    error = %e,
                    "drain_focus_conclusions: router.upsert_conclusion failed"
                );
            }
        }
    });
    count
}

/// Retention windows for the per-message frame janitor. Long-term
/// memory is untouched: it lives in `facts`（自带 TTL 梯度）、
/// MEMORY.md / USER.md、daily memory.
const TURN_FRAME_RETENTION_DAYS: i64 = 14;
const SESSION_FRAME_RETENTION_DAYS: i64 = 30;

/// Session-end side-effects:
/// 1. Persist a session-level `FocusConclusion` to the embedded
/// SQLite (memory.db) so cross-session recall works even when the
/// session never used `delegate_task`.
/// 2. Drain any pending focus conclusions from the focus stack.
/// 3. Route `on_session_end` through `MemoryRouter`.
/// No-op when `skip_memory` is set. The session-level conclusion is
/// spawned on the runtime so the agent loop doesn't block on SQLite
/// writes; errors are logged and swallowed.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "memory_on_session_end",
    level = "debug",
    skip_all,
    fields(
        msg_count = messages.len(),
        router_attached = router.is_some(),
    )
)]
pub fn memory_on_session_end(
    config: &AgentConfig,
    router: Option<&Arc<MemoryRouter>>,
    focus_stack: &Mutex<FocusStack<SessionFrame>>,
    messages: &[Message],
    callbacks: Option<&AgentCallbacks>,
) {
    if config.skip_memory {
        return;
    }
    let Some(router_ref) = router else {
        // Even with no router, drain so conclusions don't pile up.
        let _ = drain_focus_conclusions(focus_stack, None, "");
        return;
    };
    let router_owned: Arc<MemoryRouter> = Arc::clone(router_ref);
    //persist a session-level FocusConclusion to the
    // embedded SQLite (memory.db) so cross-session recall works
    // even when the session never used `delegate_task`. Without
    // this hook the only writes to memory.db came from
    // `note_delegation_end`, leaving single-shot sessions with
    // 0 rows — the symptom the user reported as "会话记忆不工作".
    //
    // 去重：`finalize_agent_result` 是每条用户消息触发的，而本轮的
    // user/assistant 对刚刚已被 `memory_sync` 存成 turn 帧——这里的
    // 「会话总结」逐字重复同一对对话（存量 867 条 focus 帧全是这种）。
    // 仅当 turn 帧路径没有刚覆盖同一文本时才写（真·会话收尾/旧数据
    // 兼容），查不到 turn 帧则维持原语义。
    let last_user_text = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)
        .map(|m| m.content.as_deref().unwrap_or("").trim().to_string())
        .filter(|s| !s.is_empty());
    let summary_already_covered = last_user_text
        .as_deref()
        .zip(router_owned.find_embedded_backend())
        .map(|(text, backend)| backend.has_recent_turn_frame(text, 10).unwrap_or(false))
        .unwrap_or(false);
    let wrote_session_summary = if summary_already_covered {
        None
    } else {
        build_session_summary(messages).map(|text| {
            let router = router_owned.clone();
            let session_id = config.session_id.clone().unwrap_or_default();
            let conclusion = FocusConclusion {
                source_frame_id: uuid::Uuid::new_v4(),
                text: text.clone(),
                salience: 0.5,
                tags: vec!["session".to_string()],
                created_at: chrono::Utc::now(),
            };
            tokio::spawn(async move {
                if let Err(e) = router.upsert_conclusion(&conclusion).await {
                    tracing::warn!(
                        target: "hermix.agent",
                        session_id = %session_id,
                        error = %e,
                        "memory_on_session_end: upsert_conclusion failed"
                    );
                }
            });
            text
        })
    };

    // Drain any pending focus conclusions.
    let session_id = config.session_id.clone().unwrap_or_default();
    let drained = drain_focus_conclusions(focus_stack, Some(&router_owned), &session_id);
    if drained > 0 {
        tracing::debug!(
            target: "hermix.agent",
            count = drained,
            "memory_on_session_end: drained leftover focus conclusions"
        );
    }

    // surface the session-end summary so the user can
    // see that the agent "remembered" this session. Emit before the
    // async persistence completes — we already know synchronously
    // whether a summary was produced and how many conclusions drained.
    if !config.quiet_mode {
        let total = drained + if wrote_session_summary.is_some() { 1 } else { 0 };
        if total > 0 {
            let summary = match wrote_session_summary {
                Some(ref t) => {
                    let truncated: String = t.chars().take(80).collect();
                    let ellipsis = if t.chars().count() > 80 { "…" } else { "" };
                    format!("保存会话总结：{truncated}{ellipsis}")
                }
                None => format!("归档 {drained} 条会话结论"),
            };
            emit_memory_update_event(
                callbacks,
                &MemoryWriteRecord {
                    action: MemoryAction::Summarize,
                    memory_type: MemoryKind::Summary,
                    key: "session".to_string(),
                    summary,
                    count: Some(total as u64),
                },
            );
        }
    }

    // Route session-end notification through MemoryRouter.
    let as_values: Vec<Value> = messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    router_owned.on_session_end(&as_values);

    // (晋升) Session-scope `MEMORY.md`/`USER.md` entries → memory.db
    // facts. The `memory` tool writes to the per-session scope by
    // design, which left cross-session recall blind to everything the
    // LLM explicitly remembered; promoting at session end closes that
    // gap. Idempotent: the canonical key is content-hashed, so a
    // re-promotion only bumps the fact's access_count.
    if let Some(backend) = router_owned.find_embedded_backend() {
        // (生命周期) 每消息级维护：衰减过期的 turn/session 帧 +
        // 清理过期 facts（facts TTL 此前只在 DB 打开时执行一次，
        // 长驻进程内从不生效）。turn 帧 14 天、session 尾注 30 天；
        // 置顶帧不衰减；软删可逆。长期记忆不受影响——它住在
        // facts（自带 TTL 梯度）/ MEMORY.md / USER.md / daily 里。
        let _ = backend.decay_stale_frames(TURN_FRAME_RETENTION_DAYS, SESSION_FRAME_RETENTION_DAYS);
        let _ = backend.cleanup_expired_facts();

        // Re-promoting surviving entries every turn end is a silent
        // salience bump; the chat event fires only for genuinely new
        // ones, otherwise the card would repeat on every exchange.
        let promoted = promote_session_memory_files(config, &backend, &session_id);
        if promoted > 0 && !config.quiet_mode {
            emit_memory_update_event(
                callbacks,
                &MemoryWriteRecord {
                    action: MemoryAction::Write,
                    memory_type: MemoryKind::Promotion,
                    key: "memory".to_string(),
                    summary: format!("晋升 {promoted} 条会话记忆到长期记忆"),
                    count: Some(promoted as u64),
                },
            );
        }

        // (每日) Deterministic daily-memory append: the
        // `daily_memory` tool content depends on the LLM opting in,
        // so unless we land the session summary here the day's row
        // stays an empty skeleton forever. Appends every session end
        // regardless of quiet_mode; the chat event fires only for
        // the day's first line — later entries are routine journal
        // lines the session-summary event already covers.
        if let Some(text) = wrote_session_summary.as_ref() {
            if let Some(true) = append_daily_memory_conclusion(&session_id, text) {
                if !config.quiet_mode {
                    emit_memory_update_event(
                        callbacks,
                        &MemoryWriteRecord {
                            action: MemoryAction::Write,
                            memory_type: MemoryKind::Daily,
                            key: "daily".to_string(),
                            summary: "已开启今日记忆".to_string(),
                            count: None,
                        },
                    );
                }
            }
        }
    }

    //emit per-session reflections
    // (what_went_well / what_to_improve / user_preferences_observed /
    // next_session_hint). No LLM — pure template aggregation over
    // facts + tool_errors + user.style. The user sees these in chat
    // via `StreamEvent::MemoryUpdate { kind: "reflection" }`.
    // We aggregate synchronously because the inputs are small and
    // we want the UI to render them in the same frame as the
    // session-end summary emit above.
    if !config.quiet_mode {
        if let Some(backend) = router_owned.find_embedded_backend() {
            let turn_count = messages
                .iter()
                .filter(|m| matches!(m.role, MessageRole::User))
                .count() as u32;
            let tool_error_count = backend.count_session_tool_errors(&session_id).unwrap_or(0);
            let session_fact_count = backend.count_session_facts(&session_id).unwrap_or(0);
            let dominant_user_style = backend.most_frequent_user_style().unwrap_or(None);
            let inputs = crate::reflection_generator::ReflectionInputs {
                session_id: session_id.clone(),
                turn_count,
                tool_error_count,
                session_fact_count,
                dominant_user_style,
            };
            let n = crate::reflection_generator::write_and_emit_reflections(
                &backend,
                callbacks,
                &inputs,
            );
            if n > 0 {
                tracing::debug!(
                    target: "hermix.agent",
                    session_id = %session_id,
                    count = n,
                    "memory_on_session_end: reflections written + emitted"
                );
            }
        }
    }
}

/// Dispatch delegation tool-call results through the memory subsystem:
/// 1. Fan out `on_delegation` hooks through `MemoryRouter`.
/// 2. Pop the matching `Delegation` frame on the focus stack so a
/// `FocusConclusion` is buffered for the next drain.
/// 3. Drain pending focus conclusions to long-term memory.
/// 4. Broadcast the live focus depth to the consciousness tracker.
/// wires (1)–(3); added (4). The push side
/// (`note_delegation_start`) is invoked from the tool-dispatch
/// path; the pop side here is a safe no-op when no matching frame
/// is on the stack.
#[allow(clippy::too_many_arguments)]
pub fn notify_delegations(
    config: &AgentConfig,
    router: Option<&Arc<MemoryRouter>>,
    focus_stack: &Mutex<FocusStack<SessionFrame>>,
    tool_calls: &[ToolCall],
    results: &[ToolResult],
    consciousness_tracker: Option<&Arc<ConsciousnessTracker>>,
) -> usize {
    if config.skip_memory {
        return 0;
    }
    let router = match router {
        Some(r) => r,
        None => return 0,
    };
    let session_id = config.session_id.clone().unwrap_or_default();
    let router_owned: Arc<MemoryRouter> = Arc::clone(router);

    // Step 1: route on_delegation hooks through MemoryRouter.
    for result in results {
        let Some(tc) = tool_calls
            .iter()
            .find(|tc| tc.id == result.tool_call_id)
        else {
            continue;
        };
        let Some((task, sub_agent_id)) = delegation_event_from_tool_result(tc, result) else {
            continue;
        };
        router_owned.on_delegation(&task, &sub_agent_id);
    }

    // Step 2: pop the matching `Delegation` frame on every
    // delegate_task result so the next pop generates a
    // `FocusConclusion`. Best-effort second source for
    // sub_agent_id (args or result.content).
    for result in results {
        let Some(tc) = tool_calls
            .iter()
            .find(|tc| tc.id == result.tool_call_id)
        else {
            continue;
        };
        if tc.function.name != "delegate_task" {
            continue;
        }
        let args: Option<Value> = serde_json::from_str(&tc.function.arguments).ok();
        let sub_agent_id = args
            .as_ref()
            .and_then(|v| v.get("sub_agent_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();
        let result_sub = serde_json::from_str::<Value>(&result.content)
            .ok()
            .and_then(|v| {
                v.get("sub_agent_id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            });
        let effective_sub = result_sub
            .filter(|s| !s.is_empty())
            .or(if sub_agent_id.is_empty() {
                None
            } else {
                Some(sub_agent_id)
            })
            .unwrap_or_default();
        let status = if result.is_error {
            SessionFrameStatus::Failed
        } else {
            SessionFrameStatus::Completed
        };
        let _ = note_delegation_end(focus_stack, &effective_sub, status);
    }

    // Step 3: drain any pending focus conclusions.
    let drained = drain_focus_conclusions(focus_stack, Some(&router_owned), &session_id);

    // Step 4: broadcast the current focus depth to the consciousness
    // tracker (no-op when no tracker is attached).
    if let Some(tracker) = consciousness_tracker {
        tracker.set_focus_depth(current_focus_depth(focus_stack));
    }
    drained
}

/// Task context extracted from recent messages for better memory recall.
struct TaskContext {
    active_files: Vec<String>,
    recent_errors: Vec<String>,
}

/// Extract task context from recent messages to enhance memory recall.
/// Looks at the last 10 messages for file paths and error messages.
fn extract_task_context(messages: &[Message]) -> TaskContext {
    let mut active_files = Vec::new();
    let mut recent_errors = Vec::new();

    for msg in messages.iter().rev().take(10) {
        if let Some(tool_calls) = &msg.tool_calls {
            for tc in tool_calls {
                if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                    if let Some(path) = args.get("path").or(args.get("file_path")) {
                        if let Some(p) = path.as_str() {
                            if !active_files.contains(&p.to_string()) {
                                active_files.push(p.to_string());
                            }
                        }
                    }
                }
            }
        }
        if msg.role == MessageRole::Tool {
            if let Some(content) = &msg.content {
                if content.contains("Error") || content.contains("error") {
                    let truncated = content.chars().take(200).collect();
                    recent_errors.push(truncated);
                }
            }
        }
    }

    TaskContext {
        active_files,
        recent_errors,
    }
}

/// Promote entries from the session-scope `MEMORY.md` / `USER.md`
/// into memory.db facts so explicit `memory` writes survive across
/// sessions. Content-hashed canonical keys make this idempotent —
/// re-promoting an unchanged file only bumps `access_count` and
/// renews the TTL. Returns the number of **newly** promoted facts
/// (re-asserts of known keys are silent).
fn promote_session_memory_files(
    config: &AgentConfig,
    backend: &hermix_focus::EmbeddedMemoryBackend,
    session_id: &str,
) -> usize {
    let dir = config.resolved_memory_dir();
    let mut promoted = 0usize;
    for file in ["MEMORY.md", "USER.md"] {
        let Ok(content) = std::fs::read_to_string(dir.join(file)) else {
            continue;
        };
        for entry in hermix_tools::backends::memory::FileMemoryBackend::parse_entries(&content) {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let digest = md5::compute(entry);
            let key = format!("user.memory:{digest:x}");
            let already = backend.has_fact(&key).unwrap_or(false);
            let payload = serde_json::json!({ "text": entry, "from": file });
            let tags = vec![
                format!("session:{session_id}"),
                format!("scope:{}", file.trim_end_matches(".md")),
            ];
            match backend.upsert_fact(&key, payload, 0.6, &tags, "memory_tool_promotion") {
                Ok(Some(_)) => {
                    if !already {
                        promoted += 1;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        target: "hermix.agent",
                        session_id,
                        file,
                        error = %e,
                        "memory promotion: upsert_fact failed"
                    );
                }
            }
        }
    }
    promoted
}

/// Append the session summary to `daily_memory[今天].conclusions`.
/// Deterministic (no LLM), so the day's row stops being an empty
/// skeleton when the model never calls the `daily_memory` tool.
/// Returns `Some(first_line_of_day)` on success, `None` on failure.
/// Best-effort: failures are logged, never propagated.
fn append_daily_memory_conclusion(session_id: &str, summary: &str) -> Option<bool> {
    let Ok(store) = hermix_tools::tools::daily_memory::DailyMemoryStore::open() else {
        return None;
    };
    if let Err(e) = store.ensure_today() {
        tracing::warn!(target: "hermix.agent", error = %e, "daily memory: ensure_today failed");
        return None;
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let first_line_of_day = store
        .get(&today)
        .map(|e| e.conclusions.trim().is_empty())
        .unwrap_or(true);
    let sid8: String = session_id.chars().take(8).collect();
    let truncated: String = summary.chars().take(120).collect();
    let line = format!(
        "- {} [{}] {}",
        chrono::Local::now().format("%H:%M"),
        sid8,
        truncated
    );
    match store.append_section(&today, "conclusions", &line) {
        Ok(()) => Some(first_line_of_day),
        Err(e) => {
            tracing::warn!(target: "hermix.agent", error = %e, "daily memory: append failed");
            None
        }
    }
}

// (): integration tests covering the
// memory-recall → pre-compress → persist round-trip that the
// agent loop relies on. Previously only `compression::tests`
// had end-to-end coverage; memory_prefetch was completely
// uncovered. These tests pin the contract from the outside
// Integration tests for the boundary functions in this module
// live in `tests/memory_prefetch_integration.rs` ().

#[cfg(test)]
mod promotion_tests {
    use super::*;
    use crate::agent_config::AgentConfig;
    use hermix_focus::EmbeddedMemoryBackend;

    /// Build a config whose auto-derived memory scope points at a
    /// temp dir, with a scoped MEMORY.md containing two entries in
    /// the `\n§\n` delimiter format `FileMemoryBackend` writes.
    fn setup(dir: &std::path::Path, memory_content: &str) -> AgentConfig {
        let scoped = dir.join("sessions").join("sess-1").join("memories");
        std::fs::create_dir_all(&scoped).unwrap();
        std::fs::write(scoped.join("MEMORY.md"), memory_content).unwrap();
        AgentConfig {
            hermix_home: Some(dir.display().to_string()),
            session_id: Some("sess-1".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn promotion_upserts_entries_as_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let config = setup(
            tmp.path(),
            "记住的第一件事\n§\n记住的第二件事\n<!-- archived: 2026-10 -->\n",
        );
        let backend = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let n = promote_session_memory_files(&config, &backend, "sess-1");
        assert_eq!(n, 2, "both entries promoted, archive index ignored");
        let facts = backend.ui_list_facts(10).unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().all(|f| f.key.as_deref().unwrap_or("").starts_with("user.memory:")));
    }

    #[test]
    fn promotion_is_idempotent_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        let config = setup(tmp.path(), "稳定条目\n§\n另一条\n");
        let backend = EmbeddedMemoryBackend::open_in_memory().unwrap();
        assert_eq!(promote_session_memory_files(&config, &backend, "sess-1"), 2);
        // Re-run: same keys, no new facts — re-promotion is silent.
        assert_eq!(promote_session_memory_files(&config, &backend, "sess-1"), 0);
        assert_eq!(backend.ui_list_facts(10).unwrap().len(), 2);
        // An added entry promotes as new; existing ones stay quiet.
        let scoped = tmp.path().join("sessions").join("sess-1").join("memories");
        std::fs::write(
            scoped.join("MEMORY.md"),
            "稳定条目\n§\n另一条\n§\n第三条新增\n",
        )
        .unwrap();
        assert_eq!(promote_session_memory_files(&config, &backend, "sess-1"), 1);
        assert_eq!(backend.ui_list_facts(10).unwrap().len(), 3);
    }

    #[test]
    fn promotion_without_files_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let config = AgentConfig {
            hermix_home: Some(tmp.path().display().to_string()),
            session_id: Some("sess-2".to_string()),
            ..Default::default()
        };
        let backend = EmbeddedMemoryBackend::open_in_memory().unwrap();
        assert_eq!(promote_session_memory_files(&config, &backend, "sess-2"), 0);
    }

    #[test]
    fn daily_append_lands_in_conclusions() {
        let tmp = tempfile::tempdir().unwrap();
        // Open the store at a temp path first so the module-level
        // today date is derived from `open_at`, then append via the
        // production helper (which opens the global path) is not
        // test-safe, so this test drives `open_at` + append_section
        // — the exact calls append_daily_memory_conclusion makes.
        let store =
            hermix_tools::tools::daily_memory::DailyMemoryStore::open_at(
                tmp.path().join("daily_memory.db"),
            )
            .unwrap();
        store.ensure_today().unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        store
            .append_section(&today, "conclusions", "- 12:00 [sess-1] 测试总结")
            .unwrap();
        let entry = store.get(&today).unwrap();
        assert!(entry.conclusions.contains("[sess-1] 测试总结"));
        let days = store.list_days(10).unwrap();
        assert_eq!(days.len(), 1);
    }
}
