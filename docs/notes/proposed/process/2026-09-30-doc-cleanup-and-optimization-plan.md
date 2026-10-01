# Decision Note: 文档清理与新决策记录制度全面落地 + 代码优化路线图

Status: proposed — 待评审后分批执行；执行主体可以是其他 agent 会话

## Decision

三件事分三个批次落地：

1. **文档清理**：按本文迁移表处置 docs/ 现存 56 篇 + 根目录杂件——有效的迁入新体系或保留原位，过期/杂件/重复的移入 `docs/notes/archived/`（或直接删除纯快照类），杜绝"过期设计文档被 agent 当现行依据"的问题。
2. **制度落地补全**：`docs/notes/` 骨架、README、`scripts/check-notes.sh` 门禁与 AGENTS.md 规范节已于 2026-09-30 建立并挂进 ci-gate [10/10]；本提案批准后即成为全项目文档的唯一规则源。
3. **代码优化路线图**：把对 deepseek-harness 525 篇 Agent Notes 的分析结论沉淀为四批可执行工程批次（见文末），供后续会话逐批认领。

## Alternatives considered

- **维持现状 + 手工清理**：否决——文档会继续以每次 PR 新增一篇 PLAN 的速度劣化，根因是没有"新文档往哪放、旧文档何时退场"的制度。
- **全量翻译 dsh 的 Agent Notes 目录树到本仓库**：否决——201+ 篇架构笔记对应的是 microkernel+事件溯源+40 包 monorepo 的复杂度；hermix 18 crate 的规模只需要三态生命周期 + 6 类闭集 + 强制 Alternatives 节，其余照搬是仪式而非信息。
- **只立制度不清旧账**：否决——门禁只管新文件，56 篇旧文档（14 篇过期、16 篇杂件、1 篇重复）继续污染检索与 agent 上下文；清账与立制必须同批。

## Consequences

- 放弃：`docs/` 顶层"一篇 PLAN 一个方向"的自由命名惯例；今后规划一律走 notes 三态。
- 放弃：过期文档的可检索性（archived 件移出主视线；确实还要读的，git 历史与 archived 目录都在）。
- 代价：一次性迁移成本约半天（本表可机械执行）；此后每篇决策文档多写 3 个强制节（收益是决策不再被反复重新争论）。
- 风险与缓解：agent 可能仍往 docs/ 顶层丢新 PLAN → AGENTS.md 已加禁止条款 + ci-gate 提示；后续可在 check-notes.sh 加"docs/ 顶层新增 .md 即警告"（本批未做，见 TODO 节）。

## TODO 节（执行清单）

以下为可分派给执行 agent 的具体批次。每批完成的标准：迁移/修复落地 + 验证命令通过 + 本提案状态更新。

### 批次 A：文档清理（半天）

按 §迁移表 逐行执行。执行方式：
- 【归档】→ `git mv docs/<file> docs/notes/archived/<class>/`，文件头 Status 行下补 `Archived: 2026-09-30`（archived 件不受 check-notes 强制节约束，但 keep 原文冻结）
- 【删除】→ `git rm`（纯快照/报告类，git 历史即存档）
- 【保留】→ 不动；【迁 notes】→ 见批次 B
- 完成后 `bash scripts/check-notes.sh` 必须通过，`docs/` 顶层 .md 只允许 §迁移表"保留"行的文件

### 批次 B：有效规划文档迁入 notes（2 小时）

把 §迁移表中标"迁 notes"的文件改写为标准格式（补齐 Status/Alternatives/Consequences 三节，内容不改写只重组）：
- 现行规划 → `docs/notes/implemented/`（已部分落地的）或 `proposed/`（还没动的）
- design/ 下 3 篇有效 → `docs/notes/implemented/architecture/`
- 原文件 git rm，文档间引用链修复（grep 引用文件名）

### 批次 C：代码止血三件（2026-10-01 状态更新）

> **⚠️ 执行前必读**：另一 agent 会话（worktree `.worktrees/feat-auto-20261001-c09d7e4f`，基于 f469b2ae）已于 2026-10-01 完成本批 3 项中的 2 项，方案与本文不同且**以其实现为准**。已核实存在：`ApprovalFallback` 策略枚举（hermix-core::approval + agent_helpers.rs:387）、ApprovalDrawer Enter 守卫改用 `closest("button, a[href], summary, [role='button']")`（比本文原方案更宽）。**不得按本文 C.1/C.3 原文重做**，否则与已有设计冲突。

1. ~~审批 fail-open~~ → **已完成**（ApprovalFallback 显式策略，方案见 worktree 实现）。
2. **失败轮补录 tool result（唯一保留待做）**：失败收尾前为所有未回答 tool call 填合成结果（"tool not started / outcome unknown, do not blindly retry — side effects possible"），对齐 dsh `failed-step-tool-results`。位置：run_loop.rs 收尾路径。注意 worktree 若已合入，先 grep `TOOL_OUTCOME\|not started` 确认未做再动手。
3. ~~ApprovalDrawer Enter 守卫~~ → **已完成**（closest 方案）。

### 批次 D：可靠性批次（2026-10-01 状态更新）

> ⚠️ D.1/D.2/D.3 已由同 worktree 完成（`join.abort()` 已加 :838、task_id→call_id 映射已加、llm_retry.rs 早有重试循环——**D.1 原文基于错误前提，已推翻**）。保留待做：

4. 批超时不 drop JoinSet：改为 drain + 给运行中任务有界 grace，写工具避免半写。（先核实 worktree 是否已覆盖）
5. tool_dispatch 多审批并行发出（先全审批后执行），消除第 N 张卡串行等待。

### 批次 E：安全批次（2026-10-01 状态更新）

> ⚠️ E.2/E.4 已完成（`save_stored` 0o600 Unix 权限、key.pem rename 前收紧）；E.3 **已否决**（分桶会降低安全性，尊重执行 agent 判断）。保留待做：

1. `security.rs:331` 移除 `?token=` query 通道，仅 Authorization 头。（先核实 worktree）


### 批次 F：结构升级（季度级，单独立项再评审）

1. 事件溯源会话改造（先写 notes/proposed 定事件域语义：session/* 事实 vs agent/* 信号，再动 session_persistence）——dsh 全部架构的根，越晚做迁移越贵。
2. Windows ACL 沙箱（SANDBOX_WINDOWS_ACL_DESIGN.md 迁入 notes/proposed 后认领）：WRITE_RESTRICTED token + 低完整性标签 + FILE_DELETE_CHILD deny ACE；fs/bash 统一策略根。
3. PTC（模型写代码组合工具调用）。
4. 子 agent 推式结算 + 模型自选路由（用户授权门控）。

### 批次 G：代码结构清理（2026-09-30 增补，半天 + 渐进）

盘点方法：crate 依赖图逐边核对、全仓库符号引用扫描、非代码资产引用计数。执行顺序按风险升序：

**G1 死 crate（立即可做）**
- `crates/hermix-protocol/`（127 行）：全仓库 `.rs` 源码零引用（grep hermix_protocol 无命中），hermix-server 的 Cargo.toml 依赖声明是死依赖。先删 server 依赖声明，编译通过后 `git rm -r` crate 并从 workspace members 移除。预期 -127 行 + 编译图瘦身。

**G2 死脚本（立即可做）**
- 以下 scripts/ 文件在 docs/ 与源码中零引用、非 CI gate 组成部分，属一次性调试残留：`probe-redrive.py`、`probe-stdio-approval.py`、`probe-stdio-frames.py`、`repro-stuck-send.py`、`clean-webview-cache.ps1`、`verify-fresh-model-config.py`、`verify-model-list-roundtrip.py`、`generate_hermix_icons.py`（图标已生成入库）。删除前跑一次 ci-gate 确认无隐藏依赖。保留：check-notes/check-theme-tokens/ci-gate/gen-workbench-skill/verify-*（gate 链）、vendor-mirror-probe（有 1 处文档引用）。

**G3 空/近空目录与原型（低风险）**
- `docs/prototype/`（1.3M，10 张截图 + index.html）：仅被 2 篇已判定归档的 UI 文档（V3/V4）引用，随批次 A 归档后一并删除。
- `docs/architecture/`（空目录仅 .gitkeep）：删除。
- `docs/compose/specs/` 两篇过期群聊设计：随批次 A 归档后目录即空，删除。

**G4 死代码候选（需逐个证明后删，不可只信 lint）**
- `hermix-cron/src/file_lock.rs` 被 agent_loop 引用需保留；但 crate 其余部分与 `hermix-automation` 的调度存在职责重叠，合并评估（2 crate → 1）列入批次 F 附带项。
- `#[allow(dead_code)]` 密集点逐一核实：`convergence_detector.rs`（5 处，含 apply_domain_max_stall——AL-8 回退后仅测试调用）、`workbench_lint.rs`（3 处）、`harness_self_edit.rs`（2 处）。规则：无 import 不构成死代码证据，须核对调用点/配置/生成物后按 simplification 类笔记流程删除并写 Reintroduction conditions。
- `hermix-environments` 的 ssh/docker feature 默认关闭，确认无产品路径启用后可裁（当前 server 全量依赖 environments，需先查启用面）。

**G5 上帝文件拆分（对齐已有 2500 行纪律，渐进）**
按 ci-gate 已告警的 6 个文件，拆分优先级（拆分原则：砍重复/搬测试，不单纯切文件）：
1. `local_agent_service.rs`（4625）— 审批/权限/工具路由/overrides 四块各自成模块；高危 fail-open 修复（批次 C）就在此文件，拆分前先修 bug 以免冲突。
2. `agent_loop/tests.rs`（3731）— 测试文件，按主题搬到独立测试文件，零运行时风险。
3. `App.tsx`（3479）— 页面级状态容器，抽 composables（键盘/缩放/审批 resolver 已有 hooks 雏形）。
4. `ws.rs`（3245）/ `provider.rs`（3202）/ `run_loop.rs`（2755）— 各按 handler/协议/阶段边界切。

**G6 资源体积（打包项，非代码）**
- `resources/loader/` 15M + `resources/icons/` 3.3M：随 NSIS 打包体积直接相关，暂无删除候选（loader 二进制现行），仅记录基线供后续对比。

## 迁移表

判定来源：2026-09-30 对 docs/ 全部 56 篇 + 根目录 3 篇杂件的逐篇审读。class 按笔记分类法标注（归档时入同名目录）。

| 文件 | 判定 | 处置 |
|---|---|---|
| docs/ARCHITECTURE_BENCHMARK.md | 杂件（06-19 行数快照） | 删除 |
| docs/ARCHITECTURE_ISSUES_REPORT.md | 杂件（07-17 问题清单） | 删除 |
| docs/ARCHITECTURE_REFACTOR_PLAN.md | 过期（被 PLUGIN_KERNEL 取代） | 归档 architecture |
| docs/CHANNEL_WORKSPACE_SCOPE.md | 有效（IM 通道现行规划） | 迁 notes/implemented/feature |
| docs/CODEBASE_AUDIT_REPORT.md | 杂件（07-24 审计，已消化） | 删除 |
| docs/COMPRESSION_NO_MEMORY_LOSS.md | 有效（现行机制说明） | 迁 notes/implemented/architecture |
| docs/CSS_NAMING_AUDIT.md | 杂件（自标不批量执行） | 删除 |
| docs/DEEPSEEK_HARNESS_BORROW.md | 重复（主=BORROWINGS） | 删除 |
| docs/DEEPSEEK_HARNESS_BORROWINGS.md | 有效（P1-P3 待做，现行规划） | 保留原位（本文来源文档），P0 落地部分可拆笔记 |
| docs/DEEP_ANALYSIS_2026-07-30.md | 杂件（与审计重叠） | 删除 |
| docs/DESIGN.md | 过期（被纸墨 UI 取代） | 归档 architecture |
| docs/FEATURE_ANALYSIS_SETTINGS_MODEL_SECURITY.md | 已完成 | 归档 feature |
| docs/FILE_PREVIEW_REFACTOR.md | 有效（09-27 现行方案） | 迁 notes/proposed/feature |
| docs/FOUNDATION_REWRITE_DESIGN.md | 过期（自标被取代） | 归档 architecture |
| docs/FRONTEND_AUDIT_2026-07-13.md | 杂件 | 删除 |
| docs/FRONTEND_AUDIT_TODO_2026-07-13.md | 杂件 | 删除 |
| docs/FRONTEND_DESIGN_AUDIT.md | 杂件（被字体系统接手） | 删除 |
| docs/GROK_BUILD_MIGRATION_REPORT.md | 杂件（执行报告） | 删除 |
| docs/HANDOFF_FOUNDATION_REWRITE.md | 有效（唯一交接入口，09-26 仍更新） | 保留原位 |
| docs/HERMIX_AGENT_SYSTEM_PROMPT.md | 有效（现行系统提示词设计） | 保留原位 |
| docs/MIGRATION_TODO.md | 已完成（06 重构收口） | 归档 architecture |
| docs/MODEL_CONFIG_REDESIGN.md | 有效（P3/P4 待做） | 迁 notes/implemented/feature |
| docs/OPTIMIZATION_DIRECTIONS_2026-07-26.md | 已完成（4/5 落地） | 归档 process |
| docs/OPTIMIZATION_PLAN.md | 过期（被 DIRECTIONS 覆盖） | 归档 process |
| docs/ORCHESTRATION_ENGINE_DEV_PLAN.md | 过期（路由已整体砍除，方向推翻） | 归档 architecture |
| docs/PLUGIN_KERNEL_DESIGN.md | 有效（现行内核方向） | 迁 notes/implemented/architecture |
| docs/PRODUCT_ZERO_FRICTION_DESIGN.md | 杂件（自标不开发） | 删除 |
| docs/PROPOSAL_PORTABLE_DESKTOP.md | 有效（产品定位铁律） | 迁 notes/implemented/process |
| docs/SANDBOX_WINDOWS_ACL_DESIGN.md | 有效（P3 待做，批次 F 认领） | 迁 notes/proposed/architecture |
| docs/SEAMS_DESIGN.md | 已完成（自标已落地） | 归档 architecture |
| docs/STABILITY_AND_REMOTE_PLAN.md | 有效（阶段 1-3 落地，5-8 执行中） | 迁 notes/implemented/process |
| docs/STABILITY_ENGINEERING_PLAN.md | 过期（路线被抛弃） | 归档 process |
| docs/STABILITY_PLAN_REVIEW.md | 杂件（一次性审查） | 删除 |
| docs/STABLE_FOUNDATION_DESIGN.md | 过期（自标被取代） | 归档 architecture |
| docs/TYPOGRAPHY_SYSTEM.md | 有效（现行字体规范） | 保留原位 |
| docs/UI_DESIGN_V2_AXIS.md | 过期（v3 逐条否决） | 归档 architecture |
| docs/UI_DESIGN_V3_REVISED.md | 过期（被 V4 取代） | 归档 architecture |
| docs/UI_DESIGN_V4_SCOPE.md | 已完成（四批当晚全落地） | 归档 feature |
| docs/UI_PROPOSAL_CC_HAHA_2026-09-29.md | 有效（现行 UI 方向） | 保留原位 |
| docs/UI_REDESIGN_PAPER_INK_2026-09-28.md | 有效（现行 UI 主张） | 保留原位 |
| docs/WORKBENCH_CHAT_REDESIGN_PLAN.md | 已完成（工作台已上线） | 归档 feature |
| docs/WORKBENCH_MODULE_DESIGN.md | 已完成（同上） | 归档 feature |
| docs/code-analysis-report-2026-08-15.md | 杂件 | 删除 |
| docs/compose/specs/2026-06-18-groupchat-collaborative-design.md | 过期（架构不符） | 归档 architecture |
| docs/compose/specs/2026-06-18-groupchat-implementation-plan.md | 过期（同上） | 归档 architecture |
| docs/custom-app-isolation-analysis.md | 杂件（修复已落地） | 删除 |
| docs/design/DISCUSSION_MODE_DESIGN.md | 有效（群聊现行） | 迁 notes/implemented/feature |
| docs/design/GROUPCHAT_UI_REDESIGN.md | 过期（被纸墨 UI 取代） | 归档 architecture |
| docs/design/THEME_SYSTEM.md | 过期（被纸墨双主题取代） | 归档 architecture |
| docs/design/custom-app-architecture-spec.md | 有效（custom-app 线现行） | 迁 notes/implemented/architecture |
| docs/design/hermix-design-spec.md | 过期（最早视觉规范，多轮被取代） | 归档 architecture |
| docs/god-file-split-report-2026-08-17.md | 已完成（自标全部完成） | 归档 process |
| docs/groupchat-silent-failure-analysis.md | 杂件（修复后历史记录） | 删除 |
| docs/groupchat-task-engine-design.md | 有效（段位机制现行依据） | 迁 notes/implemented/architecture |
| docs/prompt-comparison-report.md | 杂件（已被吸收） | 删除 |
| docs/transport-stdio-migration.md | 已完成（全量迁 stdio） | 归档 architecture |
| 根 windows_cmd_analysis.md | 杂件（一次性生效性分析） | 删除 |
| 根 SOUL.md | 有效（agent 人格文件，运行时读取） | 保留原位 |
| 根 README.md / AGENTS.md | 有效 | 保留原位 |

汇总：删除 22、归档 19、迁 notes 12、保留 6（+根 3）。
