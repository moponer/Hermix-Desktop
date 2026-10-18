# AGENTS.md

## What this is

Hermix Desktop — a portable, zero-install AI assistant. Windows-only (MSVC). Rust backend (23 crates, ~200K lines) + React 19 + Tauri v2 frontend.

## Build & verify

**Windows-only (MSVC)**: linker (rust-lld.exe + explicit LIBPATH rustflags) is configured in `.cargo/config.toml`, so plain `cargo` commands work in any terminal (Git Bash / PowerShell / IDE) with no env sourcing. Rust is installed at `C:\Program Files\Rust stable MSVC 1.92`.

```bash
cargo check --workspace --all-targets
```

Full CI gate (compile + tests + TS typecheck):

```bash
./scripts/ci-gate.sh
```

Quick commands:

```bash
cargo test --workspace --lib          # ~1,834 tests
cargo test -p hermix-agent --lib      # single crate
cargo check --workspace --all-targets # compile check
cd apps/desktop-tauri && ./node_modules/.bin/tsc --noEmit  # frontend types (~25s)
```

`tsc` 必须直接调二进制。`pnpm tsc --noEmit` 不会真正执行类型检查——它只跑 pnpm
自己的 install 流程就返回（~0.8s、输出 `Already up to date`、exit 0），是一个
看起来全绿的假信号。

Build artifacts default to repo-internal `target/` dirs (gitignored). `CARGO_TARGET_DIR` env var overrides the location if a unified path is needed (e.g. CI packaging).

## Structure

- `crates/` — 23 Rust crates. Core dependency chain: `hermix-core` → `hermix-config` → `hermix-agent` → `hermix-server` / `hermix-cli`
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
- **PR template required** — must include test evidence (`cargo check`, `cargo test`, `tsc --noEmit` via `node_modules/.bin`), pre-existing cleanup status, and `ci-gate.sh` passing
- **Pre-existing bug markers** — scan for `TODO: pre-existing` / `FIXME: pre-existing` in CI gate; don't introduce them
- **fastembed is optional** — `hermix-intelligence` has `fastembed` feature for local embeddings; `cdn.pyke.io` is geo-blocked in some regions, so default build doesn't require it
- **Parity fixtures** — `hermix-parity-tests/fixtures/` has golden JSON; `pending/` is excluded from test runs
- **Desktop app frontend** — `apps/desktop-tauri/src/`, uses `@` alias for `src/`
- **界面改动必读内置设计 skill** — `crates/hermix-server/src/dashboard/skills/impeccable-design.md`（仓库里唯一的界面设计规范）。改完 UI 必跑 `npx -y impeccable@4.1.0 detect <路径>`，退出码 0 才算干净；报告「已修好」之前检测结果就是证据。令牌层规范另见 `docs/TYPOGRAPHY_SYSTEM.md` + `npm run check:tokens`。注意 detect 对**解析不了的文件会静默返回 0**，所以先跑 `tsc --noEmit` 再信它的绿灯。
- **前端 UI 纪律（DesignGuard，2026-10-18）** — `scripts/check-design-guard.mjs`（`npm run check:design`，进 ci-gate）把 impeccable/typography 的硬约束做成 grep 门禁：TSX 禁字面色值/禁 `dark:` 变体/图标按钮必须 `aria-label`/视图文件禁 `加载中…` 字面量与 `window.confirm`。此前同类规则只写在文档里，CSS 层与既有组件的违规从未被检出。新增约束优先写进门禁而不是靠自觉。

## Core principles (2026-07-23, applies to all work — code, design, bug fix, doc)

1. **First principles** — every design decision must derive from physical / architectural constraints, not analogies or "we always did it this way". Examples:
   - "LLM 单次响应能发几个 tool_call" → derive L1 batch capacity
   - "每个 read 进 ctx 都要在每轮付 token 费" → derive 派单原则
   - 问"在什么物理约束下我的方案会崩"比"参考别人怎么做"更重要

2. **Adversarial review** — every plan must explicitly answer "in what scenarios will this break?". If you can't enumerate failure modes, the plan is incomplete. Examples:
   - LLM fan-out 100 个 → 第 4 轮 LLM 偷懒？第 8 轮静默丢失？
   - Sub-agent 拿到 task 字符串含 50KB → 子 agent ctx 爆？
   - 主 agent 自己 read 50MB 文件 → parent ctx 永久膨胀？

3. **Zero-context-bloat is the core constraint**: ANY data operation that could inflate context MUST go through sub-agent isolation. This is NOT image-specific — it applies to:
   - Images (attachments) ✅ (already enforced)
   - PDFs > 5 pages (read_pdf in main ctx → 30K+ tokens)
   - Files > 50 KB / 1500 lines (read_file whole → ctx bloat)
   - Long command output (`cat huge.log`, `dir /s`, `find /`)
   - Web extraction at scale (3+ URLs)
   - Code exploration across 20+ files
   - Any task where output > ~4,000 chars and you don't branch on intermediates
   
   Mechanism: sub-agent isolation — sub-agent pays the read cost once, returns concise conclusion, parent context stays clean. See `directions.rs::TOOL_USAGE_GUIDANCE` (Context-bloat prevention — absolute rules) and `sub_agent_orchestrator.rs::sub_agent_guidance` (Context-bloat self-defense) for the operational rules.

4. **delegate_task is the universal fan-out tool** — it is NOT image-specific. The `attachments` field is just one (image) data path; `task` + `context` paths + sub-agent reading files itself cover everything else. Architecture: see `docs/ARCHITECTURE_REFACTOR_PLAN.md §7` for L1/L2/L3 batch scaling plan.

## Code simplification discipline (六步决策阶梯, 2026-08-15 起常设)

所有在本项目落地的代码（Rust / TS / 配置 / skill）默认走六步决策阶梯，**质量、安全、统一为前提，不可妥协**：

1. **YAGNI** — 不需要就跳过，一句话说明即可。
2. **标准库** — 优先用语言/运行时自带能力，不引依赖。
3. **原生平台** — 浏览器/OS/DB 原生特性优先于库（`<input type="date">` > 日期库；CSS > JS；DB 约束 > 应用代码）。
4. **现有依赖** — 已安装的 crate/npm 包优先；不为几行逻辑加新依赖。
5. **单行代码** — 能一行解决的绝不写多行。
6. **最小实现** — 只有走到这步才写"能跑的最小代码"。

**不可削减项**：信任边界校验、防数据丢失的错误处理、安全措施、无障碍基础、用户明确要求的功能。注释从简，只写功能/技术根因，杜绝过度工程与"为以后预留"的脚手架。参考 WorkBuddy `ponytail` skill（上游 DietrichGebert/ponytail）的七级阶梯，二者等价。

**量化目标**：
- 长期把全项目源码总量控制在 **10 万行以内**（当前基线 ≈14 万行，含 node_modules/target 已排除）。
- `scripts/ci-gate.sh` 增加单文件 **> 2500 行即告警**，防止上帝文件复发。
- 拆分/重写时优先砍重复（平行复制的平台/provider/工具）与搬测试到独立文件，而非单纯切文件。

## 文档制度（2026-09-30 起）

- **决策记录唯一入口是 `docs/notes/`**（三态：proposed/implemented/rejected，闭集 class，格式与规则见 `docs/notes/README.md`）。影响代码库的持久决策（为什么做、放弃了什么、击败了哪些方案）写 Decision Note；机械性/局部改动豁免。
- **方案/规划不得散落**：新规划写成 `docs/notes/proposed/{class}/yyyy-mm-dd-topic.md`，拍板后移入 implemented 同名路径；被否决的写进 rejected 留论证。**禁止再新建** `docs/XXX_PLAN.md` 式顶层文档。
- **每篇笔记强制节**：`## Decision` / `## Alternatives considered` / `## Consequences`；simplification 类加 `## Reintroduction conditions`。`scripts/check-notes.sh` 在 CI gate 中强制。
- **写新笔记时当场检查是否取代旧笔记**，是则同一 PR 内归档旧笔记（移入 `archived/{class}/` 并加 `Archived:` 行）。归档件冻结，不再编辑、不作为现行依据。
- **事实单一归属**：一个事实只有一个"家"（AGENTS.md 只放每会话必读的 standing orders，1-3 行 + 链接；机制细节归笔记；操作步骤归 README/ cookbook）。在其他地方提到事实时链接过去，不复述。
- **文档只描述现状**：历史留在提交记录与笔记里；正文不写"已实现！/ 未来将…"式的状态标注。implemented 笔记用现在时描述已交付现实。

## Gotchas

- **Rust 里没有正则字面量**（Rust 1.92 起，edition 2021 也不再解析 `/…/` 为正则）：
  写 `if !/^[0-9a-f]{64}$/.test(s)` 会报 `expected expression, found '/'`，
  看起来像括号配平错误，其实是语言特性没了。一律用 `regex::Regex::new(&pat)?`
  （见 `crates/hermix-server/src/util.rs:35`）或手写 `chars().all(...)`。
- **打包必须含最新后端与资源（每次打包前强制）**: 单 workspace 后产物一律在仓库根 `target/`；sidecar 刷新已自动化——`npm run build`（dev.mjs）与 `cargo tauri build` 的 `beforeBuildCommand` 都会跑 `npm run ensure-sidecar`（编译 hermix-server + hermix-indexer → 拷 binaries/ → sha256 逐字节校验，任一步失败即断）。**禁止绕过该脚本手工 copy**；build.rs 在 release 缺源时直接 panic。打包前仍跑 `./scripts/check-sidecar-freshness.sh`（exit 1 = 过期，禁止打包）：
  ```bash
  npm run build                # dev.mjs 全链路（含 ensure-sidecar）
  # 或最小路径：cargo tauri build 的 beforeBuildCommand 自带 ensure-sidecar
  ./scripts/check-sidecar-freshness.sh   # 必须 exit 0
  ```
  教训：2026-10-06 与 2026-10-08 两次"修了但新装的包里没有"——测试/交付跑在旧二进制上。根因（手工 copy 链 + 三处产物目录分裂）已于 2026-10-09 根治，决策记录：`docs/notes/implemented/architecture/2026-10-09-single-workspace-target-dir.md`。
- `.cargo/config.toml` hardcodes MSVC linker path — don't override
- Linker is `rust-lld.exe` (`.cargo/config.toml`), which bypasses Git Bash's `link.exe` shadow entirely — no env script needed
- **sccache 缓存必须 H 盘 + server 在跑**：`[profile.*] incremental` 禁止改回 `true`（否则 sccache 命中率静默归零、无报错），server 掉则静默回退 C 盘。依据与运维见 `docs/notes/implemented/process/2026-10-15-sccache-h-shared-cache.md`；起 server 用 `scripts/start-sccache.ps1`（已建开机任务 `HermixSccacheServer`）
- Tauri v2.9.2 doesn't support standalone zip bundle — NSIS produces portable archive
- `hermix-server` has `test-helpers` feature for integration tests
- PR禁区: no multi-platform distribution, no external resource sensing, no cross-platform targets
- **Agent Loop 正常流程**: LLM 思考 → `<think>` 进折叠块；LLM 输出可见文本 → Markdown 渲染给用户；LLM 调工具 → 结果写入 ctx → 下轮 LLM 自然总结。系统提示含 `THINKING_DISCIPLINE_GUIDANCE` + `REPORT_SECTIONING`（单一真相源，think.full 与 format.short 运行时拼接）+ `TOOL_FAILURE_GUIDANCE` 等（directions.rs）。`TOOL_EXECUTION_SUMMARY_GUIDANCE` / `STOPPING_SUMMARY_GUIDANCE` 已删除（曾导致 filler 压力），勿再引用。
- **Finalize 路径 (简化 2026-07-09)**: `process_post_response` 返回 `Finalize` 时，有可见文本 → 直接结束；无可见文本（thinking-only）→ 落入 `decide_empty_response` 处理边缘情况（Codex ack / 自治模式预工具 nudge）。**不再有多轮 nudge，不再有 PatchLastAssistant** — 信任 LLM + 系统提示自然工作，不伪造 LLM 输出。**例外（2026-09-29）**：自治模式下可见文本 + 尚未用过任何工具 + nudge 额度在（`nudge_eligible`）时仍会发**一次**预工具 nudge——该豁免位于 run_loop.rs 空名过滤 finalize 处，勿删。
- **架构改进计划**: 见 `docs/ARCHITECTURE_REFACTOR_PLAN.md` §6 — Agent Loop 核心引擎修复进度跟踪
- **显式状态机 (AL-6)**: `LoopPhase` 枚举在 `agent_loop/types.rs` 中定义 9 个阶段，通过 `tracing::debug!` 在 `run()` / `run_stream()` 每个阶段边界发射
- **统一工具验证 (AL-7)**: `tool_call_validator.rs` 模块集中了无效名称 + 无效 JSON 验证逻辑，返回 `ValidationOutcome` + `ApplyResult` 枚举
- **收敛检测 (AL-8)**: `convergence_detector.rs` — 双阈值：`NoProgressWithTools` 15 轮 / `SimilarContent` 5 轮先 **advisory WARN**，到 25 轮 / 10 轮升级 **hard stop**（曾短暂改纯 advisory，但 LLM 倾向固守当前策略，commit d4d7bc9f 回退升级）；`ErrorLoop`（连续 3 轮全错）hard stop。阈值见 `convergence_detector.rs:95-121`。
- **上下文分层 (AL-9)**: `context.rs` 模块文档定义 5 层上下文模型（System Prompt / Tool Definitions / Conversation History / Memory / Dynamic Suffix）
- **Turn Report (AL-10)**: `turn_report.rs` 模块提供可序列化的 `TurnReport` 结构体用于每轮可观测性
