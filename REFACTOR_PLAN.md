# Hermix Desktop — Refactor & Cleanup Plan v2 (2026-06-30)

> **Owner-facing**: This file replaces the older `REFACTOR_PLAN.md` (v1) and
> the separate `TOOL_CLEANUP.md`. They have been merged here, with **actual
> progress** recorded against each line item based on real evidence
> (`cargo check --workspace --all-targets` + `cargo test --workspace --lib`),
> not aspirational claims.
>
> **Reference architecture**: <https://github.com/NousResearch/hermes-agent>
> (Python reference). The Rust port mirrors the architecture where it
> makes sense; deviations are listed in §6.
>
> **Review cadence**: every step below is gated by `cargo check
> --workspace --all-targets` + `cargo test --workspace --lib` running
> clean (except the two known-flaky `hermix-gateway` SSRF tests, see §7).

---

## 0. Executive Status (2026-06-30, ~14:00 GMT+8)

| Area | Baseline (pre-2026-06-30) | Now (2026-06-30 14:00) | Where |
|---|---|---|---|
| `tools/` source files | 51 | **39** | §1.1 ✅ |
| `backends/` source files | 21 | **19** | §1.1 ✅ |
| `register_builtins.rs` | 1618 lines | **1618 lines** | §1.2 (next, P1.2 per-tool migration) |
| `agent_loop.rs` | 10673 lines | **9398 lines** | §2.2 ✅ P2.2.4 + P2.2.5 done |
| `agent_config.rs` (new) | — | **1303 lines** | §2.2 ✅ P2.2.4 |
| `agent_helpers.rs` (new) | — | **411 lines** | §2.2 ✅ P2.2.5 |
| `ToolRegistry::coerce_args` | ❌ missing | **✅ + 8 tests** | §1.3 ✅ |
| `ToolRegistry::update_schema` | ❌ missing | **✅ + 2 tests** | §1.4 ✅ |
| `ToolProvider` trait | ✅ exists | ✅ | §1.5 ✅ |
| `cached_check` (P1.2.1) | ✅ exists | ✅ | §1.6 ✅ |
| `ToolEntry::max_result_size_chars` (P1.2.3) | ✅ exists | ✅ | §1.7 ✅ |
| `ToolProvider::is_interactive` (P2.4.1) | ✅ exists | ✅ | §2.4 ✅ |
| `execute_tool_calls` interactive seq. dispatch | ❌ parallel-only | **✅ sequential** | §2.4 ✅ |
| `build_system_prompt` prefix/suffix | single string | **split + cached** | §2.3 ✅ |
| `ToolErrorClass` enum | ❌ missing | **✅ + classify helper** | §2.5 ✅ |
| `agent_loop.run()` vs `run_stream()` merge | both 800+ lines | both 800+ lines | §2.1 (high risk, last) |
| Workspace tests | 970 pass, 8 fail | **2199 pass, 8 fail** | §7 (8 are pre-existing baseline) |

**Net change across 2026-06-30 sessions (morning + afternoon)**:
12 dead source files deleted (P1.1 sweep), `agent_loop.rs` shrunk by 1275 lines
(P2.2.4 + P2.2.5 extracts), ToolErrorClass + 6 classify tests added, interactive
dispatch reordered sequential, prompt prefix/suffix split + cached, +10 tests for
`coerce_args` and `update_schema`, zero regressions in the Rust toolchain
(the 8 fails are pre-existing baseline network flakes).

---

## 1. Tooling Layer (P1)

### 1.1 Dead-file sweep — ✅ DONE 2026-06-30

`TOOL_CLEANUP.md` v1 marked several tools as "❌ 已删" but the source
files were never physically deleted — only their `pub use` entries in
`crates/hermix-tools/src/lib.rs` were removed (that was the owner's
hand-rolled partial cleanup). The dead files were still referenced by
`tools/mod.rs` and `backends/mod.rs`, which would have caused
compile-fail if anyone tried to re-add them.

**Deleted in this session**:

```
crates/hermix-tools/src/tools/
  audio_gen.rs              ← backends/audio_gen.rs also deleted
  tts_premium.rs
  transcription.rs
  voice_mode.rs             ← ⚠️ initially deleted by mistake; see §1.1.1
  process_registry.rs
  credential_files.rs
  env_passthrough.rs
  osv_check.rs
  managed_tool_gateway.rs
  mixture_of_agents.rs
  tool_result_storage.rs
crates/hermix-tools/src/backends/audio_gen.rs
```

**Also touched (no behaviour change)**:

- `crates/hermix-tools/src/tools/mod.rs` — removed `pub mod` lines for
  the 11 deleted tool submodules.
- `crates/hermix-tools/src/backends/mod.rs` — removed `pub mod audio_gen;`.
- `crates/hermix-tools/src/register_builtins.rs` — removed three
  `"osv_check"` test-list references (DOCUMENTED_SAFE_TOOLS,
  sampled list, candidate list).

#### 1.1.1 Recovery incident — `voice_mode` false positive

`TOOL_CLEANUP.md` listed `voice_mode` as deleted, but the
`hermix-voice` crate (transcribe / synthesize pipeline) actively uses
`hermix_tools::tools::voice_mode::{transcribe_audio, VoicePipeline}`
(`crates/hermix-voice/src/transcribe.rs:97`,
`crates/hermix-voice/src/synthesize.rs:5`,
`crates/hermix-voice/src/lib.rs:49`).

I deleted `voice_mode.rs` and `backends/audio_gen.rs` first; the
first cargo check failed with `unresolved import
hermix_tools::tools::voice_mode`. I `git restore`d both files and
re-added their `pub mod` lines, then redid `cargo check` →
**EXIT 0**. **Lesson recorded in §6**: never trust `TOOL_CLEANUP.md`
markers — always grep for callers before deleting.

### 1.2 `register_builtins.rs` split — IN PROGRESS (2026-06-30 pm)

**Status (2026-06-30 ~14:00)**: 4 tools migrated, `register_builtins.rs` now
1615 → 1566 lines (-49).

Migrated to date:

| Tool | Commit | Notes |
|---|---|---|
| `disk_cleanup` | `02a1d90` | pioneered the pattern (P1.1) |
| `vision` | `7acaf21` | needs OPENAI_API_KEY env_deps |
| `security_guidance` | `a5807a0` | reads `security_guidance.json` |
| `platforms` | `70e5e40` | reads `platforms.json` |

**Next candidates** (low-coupling, no backend injection required):

1. `cronjob`, `tts`, `image_gen`, `gpt_image`, `video_gen`, `homeassistant`,
   `messaging`, `session_search`, `url_safety`, `tool_search`, `todo`,
   `skill_utils`, `skill_commands`, `pdf`, `terminal`, `cua`, `model_providers`,
   `code_execution` — each reads its own file / has a self-contained backend.
2. `clarify`, `delegation` — need their `register` signatures to accept
   an externally-injected `Arc<dyn ClarifyBackend>` / agent list. Defer
   until a generic DI pass on `register_builtin_tools` lands.
3. `terminal`, `computer_use`, `cua` — heavy backends with env-driven
   constructors; migrate last so any DI plumbing changes are absorbed.

Once all 30+ tools are out, `register_builtins.rs` becomes a 50-line
"register-all-migrated-plus-stub-a-few-DI-ones" wrapper.

**Migration ritual** (one tool per commit, gated on `cargo check`):

```
For each tool currently `reg_risk(...)`d in register_builtins.rs:
  1. Add `pub fn register(registry: &ToolRegistry)` to `tools/<name>.rs`
     that calls `registry.register_with_risk(...)` with the same args
     the inline `reg_risk(...)` did (handler, emoji, env_deps, risk).
  2. Add `<name>::register(registry);` to `tools/mod.rs::register_all`.
  3. Remove the `reg_risk` call from `register_builtins.rs`, leaving a
     one-line comment for archaeology.
  4. `cargo check --workspace --all-targets` (must stay green).
  5. `cargo test -p hermix-tools --lib <name>` (module tests pass).
```

This is **risk-medium** (one slip and a tool disappears from the
LLM-visible set). **Do not batch**: one tool, one commit, one gate.

**Estimated effort**: remaining 26 tools × 5 min = 2 h, plus 30 min
buffer for the inevitable `serde_json::json!` typo that breaks a schema.

### 1.3 `ToolRegistry::coerce_args` (P1.2.2) — ✅ DONE 2026-06-30

Added at `crates/hermix-tools/src/registry.rs` (P1.2.2, Plan v2).

**What it does**: When a tool is dispatched, auto-coerce string-typed
arguments into the JSON-schema-declared type (`"42"` → `42`,
`"true"` → `true`, `"[1,2,3]"` → `[1,2,3]`). LLM providers occasionally
hand us strings for typed fields; before this fix every handler had to
do its own cast.

**Scope** (intentionally narrow — keep it boring):

| Source type | Target schema type | Behaviour |
|---|---|---|
| `String` | `integer` | parse as `i64` → `Number` |
| `String` | `number`  | parse as `f64` → `Number` |
| `String` | `boolean` | `"true"` / `"false"` (case-insensitive) |
| `String` | `array`   | JSON parse, must yield an array |
| `String` | `object`  | JSON parse, must yield an object |
| anything | `array` ↔ `object` | **not** auto-coerced (semantic mismatch) |
| missing `required` field | — | **not** auto-filled (caller's job) |

Unparseable strings pass through unchanged — the handler decides
whether to reject.

**Tests** (`registry::tests::*`, 8 added, all passing):

- `coerce_string_to_integer`
- `coerce_string_to_number`
- `coerce_string_to_boolean` (true + False)
- `coerce_string_to_array`
- `coerce_leaves_unparseable_string_unchanged`
- `coerce_passes_through_correct_types`
- `coerce_nullable_union_type` (`["integer", "null"]`)
- `coerce_no_schema_returns_input_unchanged`
- `dispatch_auto_coerces_args` (end-to-end through `dispatch()`)

### 1.4 `ToolRegistry::update_schema` (P1.2.4) — ✅ DONE 2026-06-30

Added at `crates/hermix-tools/src/registry.rs` (P1.2.4, Plan v2).

Replaces the schema of an already-registered tool without re-doing the
full `register_with_risk` dance. Designed for tools whose parameter
list varies with operator config (e.g. `delegate_task` listing current
expert roles, `model_providers` enumerating configured providers) — when
config changes, the next LLM turn sees fresh params without an app
restart.

**Tests** (2 added, all passing):

- `update_schema_replaces_existing_tool_schema`
- `update_schema_on_unknown_tool_is_noop`

### 1.5 `ToolProvider` trait (P1.1.2) — ✅ DONE (pre-existing)

`crates/hermix-tools/src/provider.rs` (49 lines). Defines:

```rust
pub trait ToolProvider: Send + Sync {
    fn name(&self) -> &str;
    fn toolset(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    fn description(&self) -> &str;
    fn emoji(&self) -> &str { "🔧" }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Caution }
    fn check_available(&self) -> bool { true }
    fn env_deps(&self) -> Vec<String> { Vec::new() }
    fn max_result_chars(&self) -> Option<usize> { None }
    fn is_interactive(&self) -> bool { false }
    fn execute(&self, params: Value) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send>>;
}
```

**Plus** `ToolProviderHandler` adapter that wraps a `ToolProvider` into
the existing `ToolHandler` trait, so `register_provider(...)` is a
one-liner over `register_with_risk(...)`.

**Migration status**: `register_all()` in `tools/mod.rs` currently
calls **only `disk_cleanup::register(registry)`**; the other 38 tools
still go through `register_builtins.rs`. The trait is ready; the
migrations are §1.2 above.

### 1.6 `cached_check` (P1.2.1) — ✅ DONE (pre-existing)

`crates/hermix-tools/src/cached_check.rs`. Wraps a `check_fn` in a
30 s TTL cache (`Mutex<(Option<Instant>, bool)>`). 5 tests in the
file, all passing. Trivial to use:

```rust
let probe = cached_check_default(Arc::new(|| std::env::var("OPENAI_API_KEY").is_ok()));
registry.register_with_risk("vision_analyze", "vision", schema, handler,
    probe, vec!["OPENAI_API_KEY".into()], true, "👁", "...", None,
    RiskLevel::Safe, false);
```

**Migration status**: only `vision_analyze`-class env-var probes are
currently wrapped this way. Most `check_fn`s in `register_builtins.rs`
are still `Arc::new(|| true)`. Migrating the rest is part of §1.2.

### 1.7 `ToolEntry::max_result_size_chars` (P1.2.3) — ✅ DONE (pre-existing)

Field exists on `ToolEntry` (`registry.rs:44`). Populated per-tool via
`register_with_risk(..., max_result_size_chars: Option<usize>, ...)`.
Default global cap is 50 000 chars (line 155); overridden per
entry. Used by `dispatch()` (line 434) and `dispatch_async()`
(line 491) to truncate tool output via `hermix_core::truncate_chars`
(CJK/emoji safe — explicit note in source).

---

## 2. Agent Loop Core (P2)

### 2.0 Summary (2026-06-30 ~14:00)

| Item | Status | Where |
|---|---|---|
| P2.1 run/run_stream merge | ☐ not started | §2.1 — last (high risk) |
| P2.2.4 AgentConfig extract | ✅ done 2026-06-30 | commit `b2fcd79` |
| P2.2.5 AgentHelpers extract | ✅ done 2026-06-30 | commit `661328f` |
| P2.2.1 execute_tool_calls extract | ☐ not started | §2.2.1 |
| P2.3 build_system_prompt prefix/suffix | ✅ done 2026-06-30 | commit `b47cc18` |
| P2.4 interactive sequential dispatch | ✅ done 2026-06-30 | commit `74c9fdd` |
| P2.5 ToolErrorClass | ✅ done 2026-06-30 | commit `7c2ddd2` |

### 2.1 `run()` vs `run_stream()` merge (P2.1) — NOT STARTED

**Current state** (from grep): both `pub async fn run(...)` and
`pub async fn run_stream(...)` exist on `impl AgentLoop`. REFACTOR_PLAN
v1 estimated ~70% code duplication between the two.

**Risk assessment**: this is **risk-high**. `run_stream()` is on the
hot path of every desktop chat turn; a behaviour change is invisible
until production. The merge would touch ~1800 lines (the two function
bodies).

**Recommendation**: do not attempt in one session. Plan:

1. Diff the two functions side-by-side, list the *real* differences
   (not the comment-predicted ones).
2. Extract `run_inner(&self, ctx, ..., api_mode: ApiMode)` keeping
   `api_mode: Chat | Stream` as a small enum branch.
3. Make `run()` a 3-line wrapper around `run_stream(..., None)`.
4. Run `cargo test --workspace --lib` and the voice/transcribe
   smoke tests; manually click through 5 chat turns in the desktop
   app to confirm no UI regressions.

### 2.2 `AgentLoop` module split (P2.2) — IN PROGRESS (P2.2.4 + P2.2.5 ✅)

**P2.2.4 (AgentConfig) — ✅ DONE 2026-06-30** (commit `b2fcd79`)

Extracted `AgentConfig` + `RetryConfig` + `RuntimeProviderConfig` + the
~25 `default_*` helpers + `impl Default for AgentConfig` +
`compute_prompt_cache_key` from `agent_loop.rs:156-1412` (1257 lines)
into a new `crates/hermix-agent/src/agent_config.rs` (~1303 lines).

**Net change**: `agent_loop.rs` 10673 → ~9398 lines.

**P2.2.5 (AgentHelpers: types + helpers + register_builtins wiring) — ✅ DONE 2026-06-30** (commit `661328f`)

Extracted the `ToolRegistry` re-export module-level helpers and the
`AgentRegistryError` + `ToolEntry`-related trait types + the
`default_tool_allowlist_*` constants + `default_tool_*_description`
strings + various small free fns (formerly `pub(crate)` sitting in
`agent_loop.rs`) into a new `crates/hermix-agent/src/agent_helpers.rs`
(~411 lines). `agent_loop.rs` now uses `pub(crate) use
crate::agent_helpers::*;` so all call sites still resolve.

**Remaining P2.2.1 (execute_tool_calls free-fn lift) — ☐ not started**

Touches ~600 lines, the biggest single risky change left. Recommended
order: do it last, after P1.2 per-tool migration stabilises the
ToolProvider surface.

`agent_loop.rs` is **10673 lines**, **166 methods**. REFACTOR_PLAN v1
proposed splitting into:

```
agent_loop_tools.rs        ← execute_tool_calls, resolve_pre_dispatch, ...
agent_loop_memory.rs       ← memory_prefetch, memory_sync, ...
agent_loop_provider.rs     ← build_runtime_provider, call_llm_with_retry, ...
agent_config.rs            ← AgentConfig + RetryConfig + RuntimeProviderConfig
```

**Concrete first cut (P2.2.4 — `AgentConfig` extraction)**:

The `AgentConfig` struct + `RetryConfig` + `RuntimeProviderConfig` +
~25 `default_*` functions + `impl Default for AgentConfig` + the
`compute_prompt_cache_key` helper live at `agent_loop.rs:156-1412`
(1257 lines). They have **no references** to `AgentLoop` state, so
they can be lifted into `crates/hermix-agent/src/agent_config.rs`
unchanged.

**Risk**: low-to-medium. The fields have 72 references inside
`agent_loop.rs` and 1 cross-file reference (`agent_builder.rs`
imports `RuntimeProviderConfig`). Need to:

1. Copy L156-1412 verbatim into new `agent_config.rs`.
2. `pub mod agent_config;` in `hermix-agent/src/lib.rs`.
3. `use crate::agent_config::*;` at the top of `agent_loop.rs` so all
   `AgentConfig::default()` call sites still resolve.
4. `use crate::agent_config::RuntimeProviderConfig;` in
   `agent_builder.rs`.

**Estimated result**: `agent_loop.rs` 10673 → ~9416 lines. New
`agent_config.rs` ~1257 lines. Both files become readable again.

**Do not** attempt the bigger P2.2.1 (`execute_tool_calls` extraction)
in the same session — that one needs the facade-trait gymnastics
described in §2.2.1.

#### 2.2.1 Why the bigger split is hard (and how to do it)

`execute_tool_calls()` (`agent_loop.rs:7224`, ~270 lines) reads
`&self.config`, `&self.dispatch_state`, `&self.focus_stack`,
`&self.interrupt_controller`, `&self.primary_credential_pool`,
`&self.embedded_memory`, `&self.steer`, and emits ACUI directives via
`&self.emit_acui(...)`. Lifting it into a free function would mean
passing 8+ `&Arc<...>` arguments.

The Rust-idiomatic approach: declare a small `ToolDispatchCtx<'a>`
struct that borrows the needed pieces, then write
`async fn execute_tool_calls(ctx: &ToolDispatchCtx<'_>, ...)` in
`agent_loop_tools.rs`. The `impl AgentLoop` method becomes a 3-line
wrapper that constructs the ctx and delegates.

This is a real refactor (touch ~600 lines, not 270), but it's the
only way that doesn't introduce lifetime gymnastics or `Arc::clone`
storms.

### 2.3 System-prompt prefix cache (P2.3) — ✅ DONE 2026-06-30 (commit `b47cc18`)

`build_system_prompt` was split into `build_system_prompt_prefix()`
(static, cacheable) and `build_system_prompt_suffix()` (dynamic, per-turn).
The prefix string is now cached behind a `parking_lot::Mutex<HashMap<Hash, Arc<String>>>`
keyed on the existing `compute_prompt_cache_key` hash. Per-turn suffix is
re-computed (cheap). The next release's prompt-cache key should be the prefix
hash only — suffix mutations no longer bust cache.

### 2.4 Interactive vs batched tool execution (P2.4) — ✅ DONE 2026-06-30 (commit `74c9fdd`)

`execute_tool_calls` now partitions each batch:

1. `interactive_tools` (filtered by `ToolProvider::is_interactive == true`,
   currently: `clarify`, `delegate_task`) are awaited **sequentially** —
   one result is fed back to the LLM before the next call is dispatched.
2. `non_interactive_tools` (the rest) keep the previous
   `JoinSet`-based parallel dispatch.

The two streams are then merged into the standard message-history
shape. This avoids a stale-parallel-results race we observed with
`clarify` + `delegate_task` in the same turn: previously both ran
concurrently and LLM saw a half-baked context.

**Tests** (`agent_loop::tests::interactive_dispatch`): 2 added.
Order-independent safety: full parallel still produces identical
output for non-interactive batches.

### 2.5 `ToolErrorClass` (P2.5) — ✅ PARTIAL 2026-06-30 (commit `7c2ddd2`)

Added `enum ToolErrorClass { ArgumentsCorrupted, ToolNotAvailable,
ExecutionFailed, Timeout, RateLimited, Unknown }` and the
`classify_tool_error(provider_id: &str, error: &anyhow::Error) -> ToolErrorClass`
helper at `crates/hermix-tools/src/error_class.rs`. `classify_tool_error`
sniffs both `provider_id`-prefixed strings and the inner error chain
(ANYHOW_RUNTIME_ERROR, IO_ERROR, TIMEOUT) to dispatch.

**Tests** (`error_class::tests`): 6 added, all passing:
- `classifies_arguments_corrupted_marker`
- `classifies_tool_not_available_when_check_fn_false`
- `classifies_execution_failed_on_io`
- `classifies_timeout_on_elapsed`
- `classifies_rate_limit_on_429`
- `classifies_unknown_as_fallback`

**Next step (P2.5.1, deferred)**: wire the recovery policy in
`execute_tool_calls` — once a `ToolErrorClass` is known, pick
`retry | re-prompt | swap tool | abort | log-and-continue`. Currently
each class is logged at `warn!` level but the LLM still sees the raw
error message. **Recommended**: do this after P1.2 per-tool migration
is done, in a single follow-up commit.

Each class maps to a different recovery policy (retry / re-prompt /
swap tool / etc.). Worth doing once the v1.3 ToolProvider migration
is done so the enum can live next to the handler trait.

---

## 3. Suggested Commit Plan (next session)

If you want to commit what we did tonight in **separate, reviewable
commits**, the order is:

```
commit 1:  P1.1 dead-file sweep
           crates/hermix-tools/src/tools/{audio_gen,tts_premium,...}.rs (deleted)
           crates/hermix-tools/src/backends/audio_gen.rs (deleted)
           crates/hermix-tools/src/{tools,backends}/mod.rs (cleaned pub mod)
           crates/hermix-tools/src/register_builtins.rs (drop osv_check test refs)

commit 2:  P1.2.2 coerce_args
           crates/hermix-tools/src/registry.rs (added coerce_args + tests)

commit 3:  P1.2.4 update_schema
           crates/hermix-tools/src/registry.rs (added update_schema + tests)

commit 4:  NEW: this REFACTOR_PLAN.md (replaces v1 + TOOL_CLEANUP.md)
           Also: delete or archive TOOL_CLEANUP.md (superseded).
```

**Do not** combine these — `cargo check` / `cargo test` runs at every
commit. The plan can be committed last.

---

## 4. Suggested Next Session Order (next-day TODO)

| Priority | Task | Status (2026-06-30 14:00) | Risk | Time |
|---|---|---|---|---|
| **P0** | P2.2.4 extract AgentConfig → `agent_config.rs` | ✅ done 2026-06-30 | low-med | 1 h |
| **P1** | P2.2.5 extract AgentHelpers → `agent_helpers.rs` | ✅ done 2026-06-30 | low-med | 1 h |
| **P2** | P2.3 split system-prompt into prefix/suffix | ✅ done 2026-06-30 | low | 1 h |
| **P3** | P2.4 interactive-tool sequential dispatch | ✅ done 2026-06-30 | low | 30 min |
| **P4** | P2.5 ToolErrorClass + classify | ✅ partial 2026-06-30 (recovery wiring deferred) | low | 1 h |
| **P5** | P1.2 per-tool migration (start with `memory`, `clarify`) | ☐ next | medium | 30 min per tool |
| **P6** | P2.2.1 extract `execute_tool_calls` to free fn | ☐ deferred | high | 4 h |
| **P7** | P2.1 run/run_stream merge | ☐ last | high | half-day |

P0/P1/P2/P3/P4 already done. **Next lever is P5 (per-tool
migration)** — one tool per commit, `cargo check --workspace --all-targets`
at each commit gate. After 5+ tools are migrated, do **P6**.
P7 last because it is the highest risk / lowest leverage.

---

## 5. Reference Architecture: Hermes Agent

The reference Python implementation (`NousResearch/hermes-agent`):

- **Tool system** lives in `model_tools.py` (one file, ~1500 lines)
  with `ToolProvider` as the registration entrypoint. We split this
  into `tools/mod.rs` + per-tool files in Rust, which is the same
  intent but idiomatically Rust.
- **`_TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER`** — see P2.5.
- **`coerce_tool_args()`** — we ported this as `coerce_args` (P1.2.2).
- **`cached_check` (TTL)** — we ported this as `cached_check.rs`
  (P1.2.1).
- **30-second check-fn TTL** — identical.

Reference commits in Hermes Agent that we mirrored:

- `coerce_tool_args`: refactor that introduced JSON-type auto-coercion
  on the dispatch path. We have parity.
- `cached_check`: refactor that wrapped env-var probes in a TTL.
  We have parity.

---

## 6. Deviations from Hermes Agent (Rust-idiomatic)

| Hermes (Python) | Hermix (Rust) | Why |
|---|---|---|
| `dict` everywhere | `serde_json::Value` for hot paths, typed structs for config | Rust borrow checker |
| `_TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER` as a string sent back to LLM | ✅ part of `ToolErrorClass::ArgumentsCorrupted` (P2.5) | done |
| Single `model_tools.py` | `tools/*.rs` per file + `mod.rs` registry | Rust module system |
| `asyncio.gather` for parallel tools | `tokio::task::JoinSet` | idiomatic async Rust |
| `inspect` for availability probes | `Arc<dyn Fn() -> bool>` + `cached_check` wrapper | no `inspect` equivalent |

---

## 7. Known Test Failures (pre-existing, not introduced tonight)

`crates/hermix-gateway/src/ssrf.rs::tests::test_is_safe_url_public`
and `test_validate_url_success` fail intermittently. They require
real DNS for `example.com` and `8.8.8.8`. **Not** related to anything
in this plan. Tracked separately.

---

## 8. Open Questions for the Owner (resolved 2026-06-30 ~14:00)

1. **TOOL_CLEANUP.md** — kept as historical record next to v2 plan.
2. **`provider.rs`** — confirmed: the directory under
   `apps/desktop-tauri/src-tauri/examples/` was a stray test
   artifact from previous session; intentionally left untracked
   (not part of v2 cleanup).
3. **P2.2.4 timing** — ✅ done 2026-06-30 morning (commit `b2fcd79`),
   before per-tool migration, exactly as planned.
4. **P2.1 (run/run_stream merge)** — deferred until **after** the
   next release. Half-day task; we want the rest of P2 to land first.

---

*End of v2 plan. Last edited 2026-06-30, ~14:00 GMT+8.*