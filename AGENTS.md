# AGENTS.md

## What this is

Hermix Desktop — a portable, zero-install AI assistant. Windows-only (MSVC). Rust backend (18 crates, ~200K lines) + React 19 + Tauri v2 frontend.

## Build & verify

**Windows-only**: source MSVC env before any cargo command in Git Bash:

```bash
. ./cargo-env.sh && cargo check --workspace --all-targets
```

Without this, Git Bash's `link.exe` shadows MSVC's and you get opaque linker errors.

Full CI gate (compile + tests + TS typecheck):

```bash
. ./cargo-env.sh && ./scripts/ci-gate.sh
```

Quick commands:

```bash
cargo test --workspace --lib          # ~1,834 tests
cargo test -p hermix-agent --lib      # single crate
cargo check --workspace --all-targets # compile check
cd apps/desktop-tauri && pnpm tsc --noEmit  # frontend types
```

## Structure

- `crates/` — 21 Rust crates. Core dependency chain: `hermix-core` → `hermix-config` → `hermix-agent` → `hermix-server` / `hermix-cli`
- `apps/desktop-tauri/` — Tauri v2 desktop app (React 19 + Tailwind 4 + Vite)
- `scripts/` — CI gate, smoke tests, fixture recording
- `crates/hermix-parity-tests/` — Python↔Rust golden fixtures (run: `cargo test -p hermix-parity-tests`)

Key crates:
- `hermix-cli` — binary entrypoint (`hermix`)
- `hermix-server` — HTTP/WebSocket API + Dashboard
- `hermix-agent` — agent loop, LLM providers, memory
- `hermix-tools` — 30+ tool backends, approval engine
- `hermix-config` — config loading, path management
- `hermix-intelligence` — tool router, thinking modes
- `hermix-focus` — FocusStack, memory salience scoring

## Conventions

- **Windows MSVC only** — no cross-compilation to macOS/Linux
- **PR template required** — must include test evidence (`cargo check`, `cargo test`, `pnpm tsc`), pre-existing cleanup status, and `ci-gate.sh` passing
- **Pre-existing bug markers** — scan for `TODO: pre-existing` / `FIXME: pre-existing` in CI gate; don't introduce them
- **fastembed is optional** — `hermix-intelligence` has `fastembed` feature for local embeddings; `cdn.pyke.io` is geo-blocked in some regions, so default build doesn't require it
- **Parity fixtures** — `hermix-parity-tests/fixtures/` has golden JSON; `pending/` is excluded from test runs
- **Desktop app frontend** — `apps/desktop-tauri/src/`, uses `@` alias for `src/`

## Core principles (2026-07-23, applies to all work — code, design, bug fix, doc)

1. **First principles** — every design decision must derive from physical / architectural constraints, not analogies or "we always did it this way". Examples:
   - "LLM 单次响应能发几个 tool_call" → derive L1 batch capacity
   - "每个 read 进 ctx 都要在每轮付 token 费" → derive 派单原则
   - 问"在什么物理约束下我的方案会崩"比"参考别人怎么做"更重要

2. **Adversarial review** — every plan must explicitly answer "in what scenarios will this break?". If you can't enumerate failure modes, the plan is incomplete. Examples:
   - LLM fan-out 100 个 → 第 4 轮 LLM 偷懒？第 8 轮静默丢失？
   - Sub-agent 拿到 task 字符串含 50KB → 子 agent ctx 爆？
   - 主 agent 自己 read 50MB 文件 → parent ctx 永久膨胀？

3. **Zero-context-bloat is the core constraint** (主人 2026-07-23 explicit): ANY data operation that could inflate context MUST go through sub-agent isolation. This is NOT image-specific — it applies to:
   - Images (attachments) ✅ (already enforced)
   - PDFs > 5 pages (read_pdf in main ctx → 30K+ tokens)
   - Files > 50 KB / 1500 lines (read_file whole → ctx bloat)
   - Long command output (`cat huge.log`, `dir /s`, `find /`)
   - Web extraction at scale (3+ URLs)
   - Code exploration across 20+ files
   - Any task where output > ~4,000 chars and you don't branch on intermediates
   
   Mechanism: sub-agent isolation — sub-agent pays the read cost once, returns concise conclusion, parent context stays clean. See `directions.rs::TOOL_USAGE_GUIDANCE` (Context-bloat prevention — absolute rules) and `sub_agent_orchestrator.rs::sub_agent_guidance` (Context-bloat self-defense) for the operational rules.

4. **delegate_task is the universal fan-out tool** — it is NOT image-specific. The `attachments` field is just one (image) data path; `task` + `context` paths + sub-agent reading files itself cover everything else. Architecture: see `docs/ARCHITECTURE_REFACTOR_PLAN.md §7` for L1/L2/L3 batch scaling plan.

## Gotchas

- `.cargo/config.toml` hardcodes MSVC linker path — don't override
- `cargo-env.sh` strips Git's `link.exe` from PATH — critical for linking
- Tauri v2.9.2 doesn't support standalone zip bundle — NSIS produces portable archive
- `hermix-server` has `test-helpers` feature for integration tests
- PR禁区: no multi-platform distribution, no external resource sensing, no cross-platform targets
- **Agent Loop 正常流程**: LLM 思考 → `<think>` 进折叠块；LLM 输出可见文本 → Markdown 渲染给用户；LLM 调工具 → 结果写入 ctx → 下轮 LLM 自然总结。系统提示已含 `TOOL_EXECUTION_SUMMARY_GUIDANCE` + `STOPPING_SUMMARY_GUIDANCE` + `THINKING_DISCIPLINE_GUIDANCE`，信任 LLM 遵循。
- **Finalize 路径 (简化 2026-07-09)**: `process_post_response` 返回 `Finalize` 时，有可见文本 → 直接结束；无可见文本（thinking-only）→ 落入 `decide_empty_response` 处理边缘情况（Codex ack / 自治模式预工具 nudge）。**不再有多轮 nudge，不再有 PatchLastAssistant** — 信任 LLM + 系统提示自然工作，不伪造 LLM 输出。
- **架构改进计划**: 见 `docs/ARCHITECTURE_REFACTOR_PLAN.md` §6 — Agent Loop 核心引擎修复进度跟踪
- **显式状态机 (AL-6)**: `LoopPhase` 枚举在 `agent_loop/types.rs` 中定义 9 个阶段，通过 `tracing::debug!` 在 `run()` / `run_stream()` 每个阶段边界发射
- **统一工具验证 (AL-7)**: `tool_call_validator.rs` 模块集中了无效名称 + 无效 JSON 验证逻辑，返回 `ValidationOutcome` + `ApplyResult` 枚举
- **收敛检测 (AL-8, 修订 2026-07-09)**: `convergence_detector.rs` — `NoProgressWithTools` 和 `SimilarContent` 改为 **advisory warning**（注入系统提示，LLM 自己决定是否停止）；**只有 `ErrorLoop` 才 hard stop**（连续 N 轮所有工具全部报错）。之前 NoProgressWithTools/SimilarContent 会直接杀 Loop，太激进，短任务都自己中断。
- **上下文分层 (AL-9)**: `context.rs` 模块文档定义 5 层上下文模型（System Prompt / Tool Definitions / Conversation History / Memory / Dynamic Suffix）
- **Turn Report (AL-10)**: `turn_report.rs` 模块提供可序列化的 `TurnReport` 结构体用于每轮可观测性
