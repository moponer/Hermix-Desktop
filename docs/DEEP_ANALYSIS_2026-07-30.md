# Hermix Desktop 深度分析报告：代码质量 · 架构质量 · 任务执行逻辑

> 分析日期：2026-07-30
> 分析方法：第一性原理推导 + 对抗性审查 + 源码静态分析
> 分析范围：`crates/` (21 Rust crate) + `apps/desktop-tauri/` (React 19 前端) + `apps/custom-app-loader/`
> 与已有报告的关系：本文聚焦已有报告（`CODEBASE_AUDIT_REPORT.md`、`ARCHITECTURE_ISSUES_REPORT.md`、`STABILITY_ENGINEERING_PLAN.md`、`ARCHITECTURE_REFACTOR_PLAN.md`）**未覆盖或未深入**的系统性问题。已有报告中的 P0-P2 条目不再重复，仅在交叉引用时提及。

---

## 目录

1. [方法论](#1-方法论)
2. [架构质量问题](#2-架构质量问题)
3. [代码质量问题](#3-代码质量问题)
4. [任务执行逻辑问题](#4-任务执行逻辑问题)
5. [解决方案：分阶段路线图](#5-解决方案分阶段路线图)
6. [对抗性自检](#6-对抗性自检)

---

## 1. 方法论

### 1.1 第一性原理

三个物理约束决定了一个 AI Agent 桌面应用的可维护性：

| 约束 | 推导 |
|------|------|
| **编译时间是开发反馈延迟的下界** | 90+ 模块的单一 crate → 任何改动触发全量重编 → 开发者被迫减少迭代频率 → 代码质量下降 |
| **认知复杂度决定缺陷引入率** | 单文件 8000 行 / 单结构体 25+ 字段 → 任何修改的"爆炸半径"不可控 → 防御性编程 → 代码膨胀 |
| **LLM 行为不可预测性 × 循环复杂度 = 系统不可预测性** | Agent Loop 每多一条隐式状态转换路径，LLM 的行为空间就乘以一个因子 → 调试不可能 |

### 1.2 对抗性审查原则

对每个发现追问三个问题：
1. **在什么场景下这个问题会导致系统崩溃或用户体验降级？**
2. **这个问题在现有测试体系下能被发现吗？**
3. **如果我不修这个问题，它在 6 个月后会导致什么？**

---

## 2. 架构质量问题

### A1. hermix-agent 是"上帝 Crate"——90+ 模块混合 6 层关注点

**现状**：

`crates/hermix-agent/src/lib.rs` 声明了 **90+ 个 `pub mod`**，混合了至少 6 个不同关注层：

| 关注层 | 模块（示例） | 应属层级（按 ARCHITECTURE_REFACTOR_PLAN.md §1） |
|--------|-------------|------------------------------------------------|
| Agent 循环核心 | `agent_loop/`, `run_loop.rs`, `tool_dispatch.rs`, `tool_executor.rs` | L3 业务层 |
| 上下文压缩 | `compression.rs` (4380行), `compression_health.rs`, `compression_ops.rs`, `compression_pipeline.rs` | L3 或独立 crate |
| 认知/意识实验 | `consciousness.rs`, `cognition_arch.rs`, `bdi.rs`, `reasoning.rs` | **不应在生产代码中** |
| 记忆/技能 | `memory_hooks.rs`, `memory_prefetch.rs`, `memory_layer_adapter.rs`, `skill_loader.rs`, `skill_orchestrator.rs`, `skill_auto_create.rs`, `skill_patch.rs`, `skill_health.rs` | L4 智能层 |
| 会话持久化 | `session_persistence.rs` (4428行), `session_lifecycle.rs` | L5 基础设施层 |
| 自我改进/反馈 | `self_improvement.rs`, `reflection_generator.rs`, `feedback_loop.rs` | **不应在生产代码中** |

**对抗性场景**：
- 修改 `consciousness.rs` 中的一个 ECG 波形参数 → 整个 hermix-agent crate 重编译 → 等待 3-5 分钟 → 开发者放弃迭代
- 新开发者克隆仓库，打开 `hermix-agent/src/`，看到 90 个文件 → 无法定位"Agent Loop 到底在哪"
- CI 编译 hermix-agent 耗时占整体编译时间的 40%+ → CI 反馈延迟 → PR 合并速度下降

**根因**：从 Python Hermes 单体移植时，将所有功能放入一个 Rust crate 以保持"parity"。但 Python 的模块系统（`import`）和 Rust 的 crate 系统有本质区别：Python 模块边界是运行时的，Rust crate 边界是编译期的。一个 Python 模块修改不会触发其他模块重载，但 Rust crate 内任何文件修改都触发全 crate 重编。

**解决方案**：

```
crates/hermix-agent/          → 仅保留 agent_loop/ + run_loop.rs + tool_dispatch.rs + tool_executor.rs
crates/hermix-compression/    → compression.rs + compression_health + compression_ops + compression_pipeline
crates/hermix-memory/         → memory_hooks + memory_prefetch + memory_layer_adapter + recall_scheduler
crates/hermix-skills-agent/   → skill_loader + skill_orchestrator + skill_auto_create + skill_patch + skill_health
crates/hermix-session/        → session_persistence + session_lifecycle + archive
crates/hermix-cognition/      → consciousness + cognition_arch + bdi + reasoning (feature-gated, default off)
crates/hermix-self-improvement/ → self_improvement + reflection_generator + feedback_loop (feature-gated, default off)
```

**预计**：2-3 周（渐进式拆分，每步保证编译通过）

**风险**：crate 拆分可能引入循环依赖。需要先画依赖图确认无环。

---

### A2. 实验性认知架构代码混入生产路径

**现状**：

以下模块存在于生产代码中，但属于实验性/研究性质：

| 模块 | 行数 | 性质 | 生产使用情况 |
|------|------|------|-------------|
| `consciousness.rs` | ~540 | ECG 心电图可视化——把 agent 状态映射为心跳波形 | UI 装饰性，非核心功能 |
| `cognition_arch.rs` | ~480 | BDI（Beliefs/Desires/Intentions）认知模型 | 结构体已定义但**无证据表明被 agent loop 使用** |
| `bdi.rs` | — | 独立 BDI 模块（与 cognition_arch.rs 重复？） | 待确认 |
| `self_improvement.rs` | — | 自我改进逻辑 | 高风险——agent 修改自身行为 |
| `reflection_generator.rs` | — | 反思生成 | 实验性 |
| `feedback_loop.rs` | — | 反馈循环 | 实验性 |
| `long_horizon.rs` | — | 长期规划 | 实验性 |
| `gatherer.rs` | — | 信息收集器 | 实验性 |

**对抗性场景**：
- `self_improvement.rs` 如果被 LLM 触发，agent 可能修改自身配置导致行为不可预测
- `consciousness.rs` 的 `ConsciousnessTracker` 通过 `Arc` 持有在 `AgentLoop` 上——如果 tracker 的 broadcast channel 积压，会增加 agent loop 的内存压力
- `cognition_arch.rs` 和 `bdi.rs` 如果同时存在且接口不一致，开发者不知道该用哪个

**根因**：快速迭代中将所有"可能有用的"实验代码提交到了主分支，没有用 feature gate 隔离。

**解决方案**：
1. 将实验性模块移入 `crates/hermix-cognition/`，用 `feature = "cognition"` 门控
2. 默认编译不包含这些模块
3. `self_improvement.rs` 必须有明确的安全边界（不允许修改 agent loop 核心配置）
4. 删除 `cognition_arch.rs` 和 `bdi.rs` 中重复的那个

**预计**：1 周

---

### A3. 工作区级 Lint 抑制掩盖技术债务

**现状**：

`Cargo.toml` 工作区配置：

```toml
[workspace.lints.rust]
unused_variables = "allow"
unused_imports = "allow"
dead_code = "allow"
```

`hermix-agent/src/lib.rs` crate 级抑制：

```rust
#![allow(
    clippy::collapsible_match,
    clippy::collapsible_if,
    clippy::doc_nested_refdefs,
    clippy::if_same_then_else,
    clippy::manual_clamp,
    clippy::needless_range_loop,
    clippy::ptr_arg,
    clippy::too_many_arguments,
    clippy::type_complexity
)]
```

**对抗性场景**：
- `dead_code = "allow"` → 函数被废弃后不告警 → 6 个月后代码库有 500+ 个 zombie 函数 → 编译时间增加 30% → 新开发者无法区分活代码和死代码
- `unused_imports = "allow"` → import 块膨胀 → 每个 `use` 语句都可能是过期的 → 开发者不敢删除 import → 恶性循环
- `clippy::too_many_arguments = "allow"` → 函数签名有 10+ 参数 → 没人重构 → 接口腐烂

**根因**：在从 Python 移植过程中，大量代码产生 lint 噪音，于是全量 allow。但移植完成后从未清理。

**解决方案**：

分三步推进，每步独立可交付：

1. **第一步（1 天）**：将 `unused_imports` 和 `unused_variables` 从 `allow` 改为 `warn`。用 `cargo fix` 自动修复。
2. **第二步（1 周）**：将 `dead_code` 从 `allow` 改为 `warn`。逐文件审查，确认是否真的死代码后删除。
3. **第三步（2 周）**：移除 `hermix-agent/src/lib.rs` 的 crate 级 `#![allow(...)]`，逐函数修复 clippy 警告。

**预计**：3 周（可与其他工作并行）

---

### A4. 系统提示词硬编码在编译产物中

**现状**：

`directions.rs` 是 **800+ 行的系统提示词字符串常量**，直接编译进二进制文件。包括：

- `MEMORY_GUIDANCE` — 记忆使用指导
- `SESSION_SEARCH_GUIDANCE` — 会话搜索指导
- `SKILLS_GUIDANCE` — 技能使用指导
- `CONTEXT_BUDGET_GUIDANCE` — 上下文预算指导
- `TOOL_USAGE_GUIDANCE` — 工具使用指导（含上下文膨胀防护规则）
- 以及 10+ 个其他指导块

**对抗性场景**：
- 修改一个提示词中的措辞 → 需要重新编译整个 hermix-agent crate → 3-5 分钟反馈延迟
- 无法 A/B 测试不同提示词版本——提示词与代码版本绑定
- 无法在不发布新版本的情况下热更新提示词
- 提示词中的业务逻辑（如"图片走 delegate_task"）与代码逻辑耦合——修改提示词需要理解代码结构

**根因**：提示词从 Python 代码直接移植，Python 中字符串也是内联的。但 Python 可以热重载，Rust 不行。

**解决方案**：

1. 将提示词移到 `$HERMIX_HOME/prompts/` 目录下的 `.md` 文件
2. `directions.rs` 改为从文件系统加载（启动时读取 + 内存缓存 + 文件变更监听）
3. 保留 Rust 中的默认值作为 fallback（文件不存在时使用）
4. 这样修改提示词只需要重启应用，不需要重新编译

**预计**：1 周

---

### A5. hermix-server 依赖 14 个内部 crate——它是单体而非微服务

**现状**：

`hermix-server/Cargo.toml` 的 `[dependencies]` 包含：

```toml
hermix-core, hermix-transport, hermix-agent, hermix-tools, hermix-plugin,
hermix-config, hermix-cron, hermix-gateway, hermix-skills, hermix-environments,
hermix-telemetry, hermix-runtime, hermix-voice, hermix-mcp, hermix-cua,
hermix-task, hermix-automation
```

这意味着 `hermix-server` 依赖了**所有其他 crate**。任何 crate 的改动都可能影响 server 编译。

**对抗性场景**：
- 修改 `hermix-cua` 中的一个 UI Automation FFI → hermix-server 重编译 → 即使 server 代码完全没变
- 修改 `hermix-voice` 的 TTS 逻辑 → 同样触发 server 重编译
- CI 中 `cargo check --workspace` 的编译时间随 crate 数量二次增长

**根因**：hermix-server 承担了 HTTP 路由 + WebSocket + Dashboard + 侧边栏管理 + 插件加载 + 语音 + CUA + 定时任务 + 自动化触发器的所有职责。它不是一个"server"，而是一个"把所有东西粘在一起"的胶水层。

**解决方案**：

不需要立即拆分 server，但需要建立边界纪律：

1. **短期**：在 `lib.rs` 中按子系统分 `mod`（已有 `dashboard/` 模块），但确保每个 `mod` 只依赖自己需要的 crate
2. **中期**：将 `hermix-voice`、`hermix-cua` 等非核心依赖改为 `optional` feature：
   ```toml
   [features]
   voice = ["dep:hermix-voice"]
   cua = ["dep:hermix-cua"]
   ```
   默认编译不包含，需要时 `cargo build -p hermix-server --features voice,cua`
3. **长期**：将 Dashboard 前端服务和 API 服务分离（不同的二进制或不同的 axum Router）

**预计**：短期 3 天，中期 1 周

---

### A6. ~~自定义应用加载器无路径沙箱~~ (已移除)

> **2026-07-30 更新**：自定义应用是 LLM 根据用户需求生成的可信代码，不是第三方不可信代码。且自定义应用需要在任意电脑环境中运行，不能设置沙箱限制。此条目已移除，不做修改。

---

## 3. 代码质量问题

### C1. AgentLoop 是"上帝对象"——25+ 字段混合 10 个关注点

**现状**：

`AgentLoop` 结构体有 **25+ 个字段**，按关注点分组：

| 关注点 | 字段 | 锁类型 |
|--------|------|--------|
| 配置 | `config`, `delegate_depth` | 无锁 |
| 工具 | `tool_registry`, `tool_router`, `always_include` | `Arc<RwLock>` |
| LLM | `llm_provider`, `primary_credential_pool`, `oauth_refresh` | `Arc<dyn>`, `Arc<Mutex>` |
| 插件/回调 | `plugin_manager`, `callbacks` | `Arc<Mutex>`, `Arc` |
| 子代理 | `sub_agent_orchestrator` | `Option<Arc>` |
| 记忆/技能 | `memory_router`, `prefetcher`, `learned_prefetch_stats`, `memory_recall_seeded` | 混合 |
| 焦点/意识 | `focus_stack`, `consciousness_tracker` | `Arc<Mutex>`, `Option<Arc>` |
| 压缩 | `compression`, `latest_compressed_summary`, `latest_compression_archived_turns`, `latest_compression_metrics`, `last_archive_refs`, `compression_in_flight` | 混合 |
| 提示缓存 | `prompt_cache`, `context_epoch`, `stored_prompt_epoch` | `Mutex`, `AtomicU64` |
| 错误/计数 | `evolution_counters`, `tool_error_counter`, `pending_steer` | `Arc<Mutex>` |

**对抗性场景**：
- 测试任何一个子系统（如压缩）都需要构造完整的 25+ 字段结构体 → 单元测试变成集成测试
- 锁顺序注释（`// 1. tool_registry → 2. plugin_manager → ...`）是建议性的，编译器不强制 → 任何 PR 都可能引入锁顺序违规
- `std::sync::Mutex`（用于 `latest_compressed_summary` 等）和 `tokio::sync::Mutex`（用于 `compression`）混用 → 如果 `std::sync::MutexGuard` 被持有跨 `.await`，clippy 能检测到；但如果开发者用了 `#[allow(clippy::await_holding_lock)]`，就静默死锁

**根因**：AgentLoop 从 Python 的 `AIAgent` 类直接移植，Python 的 `self` 可以挂任意属性，Rust 的 struct 需要显式声明。移植时没有做关注点分离。

**解决方案**：

将 AgentLoop 拆分为组合结构：

```rust
pub struct AgentLoop {
    /// 核心循环状态——只有这些字段参与 run()/run_stream()
    core: AgentLoopCore,
    /// 压缩子系统——独立持有自己的状态
    compression: CompressionSubsystem,
    /// 记忆子系统
    memory: MemorySubsystem,
    /// 预取子系统
    prefetch: PrefetchSubsystem,
    /// 工具/插件
    tools: ToolSubsystem,
}

pub struct CompressionSubsystem {
    engine: Option<Arc<tokio::sync::Mutex<dyn CompressionEngine + Send>>>,
    latest_summary: std::sync::Mutex<Option<String>>,
    latest_archived_turns: std::sync::Mutex<Option<Vec<Message>>>,
    latest_metrics: std::sync::Mutex<Option<CompressionMetrics>>,
    last_archive_refs: std::sync::Mutex<Vec<ArchiveRef>>,
    in_flight: std::sync::Mutex<Option<CompressionInFlight>>,
}
```

这样：
- 测试压缩逻辑只需要构造 `CompressionSubsystem`
- 锁的生命周期限定在子系统内部
- 新增子系统不需要修改 `AgentLoop` struct 定义

**预计**：2 周（渐进式提取，每步保证测试通过）

---

### C2. run() 与 run_stream() 60% 代码重复——已知但未修

**现状**：

`ARCHITECTURE_REFACTOR_PLAN.md` §6 的待实施项 P0："合并 run() 和 run_stream()"。`local_agent_service.rs:1199-1203` 的 TODO 注释确认：

```rust
// TODO(L8): send_message and send_message_stream share ~300 lines of
// duplicated setup code (session loading, gateway safety net, image
// handling, model resolution, etc.). Consider extracting a shared
// `prepare_and_run` helper that both methods call with a
// streaming/non-streaming mode flag.
```

`run_loop.rs` 中虽然提取了 `do_pre_turn_setup`、`do_tool_turn` 等 helper，但 `run()` 和 `run_stream()` 的主循环骨架仍然各自独立实现。

**对抗性场景**：
- 在 `run()` 中修复了一个上下文截断 bug → 忘记同步到 `run_stream()` → 流式模式 50 轮后上下文溢出
- 在 `run_stream()` 中增加了新的 LoopPhase → 忘记同步到 `run()` → 非流式模式跳过该阶段
- 已有报告的 AL-5 根因分析就是这种重复导致的：Finalize 路径在两个方法中行为不一致

**根因**：流式和非流式的控制流确实有差异（流式需要 `StreamCollect` 状态机），但 60% 的代码是相同的。提取 helper 需要仔细设计泛型参数，工作量大但非困难。

**解决方案**：

引入 `LoopMode` 枚举 + `run_engine` 统一入口：

```rust
enum LoopMode {
    NonStreaming,
    Streaming { tx: mpsc::Sender<StreamChunk> },
}

impl AgentLoop {
    async fn run_engine(&self, ctx: &mut ContextManager, mode: LoopMode) -> AgentResult {
        // 统一主循环
        loop {
            let pre = self.do_pre_turn_setup(ctx, ...).await?;
            let response = match &mode {
                LoopMode::NonStreaming => self.call_llm(ctx).await?,
                LoopMode::Streaming { tx } => self.call_llm_stream(ctx, tx).await?,
            };
            let action = self.process_post_response(ctx, &response).await?;
            // ... 统一的后续处理
        }
    }

    pub async fn run(&self, ...) -> AgentResult {
        self.run_engine(ctx, LoopMode::NonStreaming).await
    }

    pub async fn run_stream(&self, ..., tx: ...) -> AgentResult {
        self.run_engine(ctx, LoopMode::Streaming { tx }).await
    }
}
```

**预计**：1 周

---

### C3. 压缩系统四模块 + 三 Mutex 字段散落在 AgentLoop 上

**现状**：

压缩系统由 4 个源文件组成：
- `compression.rs` (4380 行) — 核心压缩逻辑
- `compression_health.rs` — 压缩健康检查
- `compression_ops.rs` — 压缩操作
- `compression_pipeline.rs` — 压缩管线

此外，`AgentLoop` struct 上有 **5 个压缩相关字段**：
- `compression: Option<Arc<tokio::sync::Mutex<dyn CompressionEngine + Send>>>`
- `latest_compressed_summary: std::sync::Mutex<Option<String>>`
- `latest_compression_archived_turns: std::sync::Mutex<Option<Vec<Message>>>`
- `latest_compression_metrics: std::sync::Mutex<Option<CompressionMetrics>>`
- `last_archive_refs: std::sync::Mutex<Vec<ArchiveRef>>`
- `compression_in_flight: std::sync::Mutex<Option<CompressionInFlight>>`

**对抗性场景**：
- 压缩状态的 6 个 Mutex 字段分散在 AgentLoop 上 → 任何修改 AgentLoop 的代码都可能意外触碰压缩状态 → 竞争条件
- `compression_in_flight` 的设计注释说"spawned task keeps running in the background but its outcome is discarded" → 如果 spawn 的压缩任务与主循环并发修改 `ctx.messages`，会数据竞争
- `compression.rs` 4380 行 → 修改压缩逻辑需要在巨型文件中定位 → 引入 bug 的概率高

**解决方案**：

1. 将 6 个压缩字段提取为 `CompressionSubsystem` struct（见 C1 方案）
2. 将 `compression.rs` 按阶段拆分：`prune.rs`（Phase 1）、`protect.rs`（Phase 2）、`summarize.rs`（Phase 3-4）、`sanitize.rs`（Phase 5）
3. `compression_in_flight` 的 spawn 任务必须使用 `ctx.messages` 的 **snapshot（clone）**，不允许并发修改

**预计**：2 周

---

### C4. session_persistence.rs 4428 行——持久化层缺乏抽象

**现状**：

`session_persistence.rs` 是项目第二大 Rust 文件（4428 行），包含：
- SQLite 连接管理
- 会话 CRUD
- 消息持久化
- 审批持久化
- 归档管理
- 压缩历史记录
- 子代理 lineage 记录
- 会话搜索
- 统计查询

所有操作直接使用 `rusqlite::Connection`，无 repository trait 抽象。

**对抗性场景**：
- 任何持久化逻辑的修改都可能影响其他持久化逻辑（如修改消息存储格式忘记更新搜索索引）
- 无法替换存储后端（如改用 PostgreSQL 或内存数据库做测试）
- 4428 行的文件中任何一行 SQL 注入漏洞都难以发现

**解决方案**：

1. 按 entity 拆分子模块：`session_repo.rs`、`message_repo.rs`、`approval_repo.rs`、`archive_repo.rs`、`lineage_repo.rs`、`search_repo.rs`
2. 定义 `trait SessionRepository` 等接口
3. 当前 rusqlite 实现作为 `SqliteSessionRepository`
4. 测试可使用 `InMemorySessionRepository`

**预计**：2-3 周

---

### C5. 前端组件规模失控——ChatView 2856 行 + App 2815 行

**现状**（与 `CODEBASE_AUDIT_REPORT.md` P2-2 一致，但需补充更深层分析）：

| 组件 | 行数 | 承担的职责数 | 问题 |
|------|------|-------------|------|
| `ChatView.tsx` | 2856 | 消息渲染、输入框、滚动管理、拖放、模型选择、专家切换、附件处理、审批 UI | 7+ |
| `App.tsx` | 2815 | 路由、全局状态、副作用编排、WS 连接、UI 布局、错误处理、启动逻辑 | 8+ |
| `MessageBubble.tsx` | 2632 | Markdown 渲染、工具时间线、审批卡片、图片预览、代码块、语音播放 | 6+ |
| `RightSidebar.tsx` | 1905 | 文件浏览器、产品列表、浏览器预览、标签管理、文件解析 | 5+ |

**对抗性场景**：
- `ChatView` 中输入框的 `onChange` 触发 re-render → 整个 2856 行组件重新执行 → 消息列表不必要的重渲染 → 输入卡顿
- `App.tsx` 的任何 state 变化都导致所有子组件 re-render（除非 `React.memo`）→ 快速切换会话时 UI 冻结
- 修改 `MessageBubble` 的工具时间线样式 → 可能意外影响图片预览逻辑 → 回归 bug

**根因**：React 组件按"页面"而非"职责"组织。`ChatView` 是一个"页面"所以什么都往里放。

**解决方案**：

按职责拆分，而非按页面拆分：

```
ChatView/
  ├── index.tsx              (容器，~100 行)
  ├── ChatMessageList.tsx    (消息列表 + 虚拟滚动)
  ├── ChatInput.tsx          (输入框 + 附件 + 发送)
  ├── ChatModelSelector.tsx  (模型选择器)
  ├── ChatExpertPicker.tsx   (专家选择器)
  └── ChatDragDrop.tsx       (拖放逻辑)
```

**预计**：2 周

---

### C6. 前后端类型手动同步——三端结构体漂移风险

**现状**：

`CODEBASE_AUDIT_REPORT.md` P7-4 已指出 `TodoSnapshotItem` 三端手动同步。但问题比报告中更广泛：

| 类型 | Rust (transport) | TypeScript | 同步方式 |
|------|-----------------|------------|---------|
| `TodoSnapshotItem` | `hermix-transport/src/lib.rs` | `useStreamChat.ts` | 手动复制 |
| `SendMessageRequest` | `hermix-transport/src/lib.rs` | `useStreamChat.ts:ChatOverrides` | 手动 + `[k: string]: unknown` |
| `WireMessage` | `hermix-transport/src/lib.rs` | `protocolAdapter.ts` | 手动 + `[k: string]: unknown` |
| `SessionSummary` | `hermix-transport/src/lib.rs` | `types.ts` | 手动 |
| `StreamEvent` | `hermix-transport/src/lib.rs` | `useStreamChat.ts` | 手动 |
| `ToolCall` | `hermix-core/src/` | `types.ts` | 手动 |

**对抗性场景**：
- 后端给 `SendMessageRequest` 新增 `reasoning_effort` 字段 → 前端 `ChatOverrides` 不报编译错误 → 用户设置 reasoning_effort 被静默忽略
- 后端 `StreamEvent` 新增一个 `SubAgentProgress` 变体 → 前端 switch 语句没有 default 分支 → 事件被静默丢弃

**解决方案**：

引入 `ts-rs` 或 `specta` 从 Rust 自动生成 TypeScript 类型：

```rust
// In hermix-transport/src/lib.rs
#[derive(ts_rs::TS)]
#[ts(export, export_to = "../apps/desktop-tauri/src/types/generated/")]
pub struct SendMessageRequest { ... }
```

CI 中运行 `cargo test --features export-types` 生成类型文件，前端 `import type { SendMessageRequest } from "./types/generated"`。

**预计**：1 周

---

## 4. 任务执行逻辑问题

### T1. Agent Loop 状态机仅日志级——无编译期强制

**现状**：

AL-6 引入了 `LoopPhase` 枚举（9 个阶段），但阶段转换仅通过 `tracing::debug!` 日志记录。`run_loop.rs` 中的控制流是命令式的 `if/else` + `match`，没有状态机约束：

```rust
// run_loop.rs 中的实际模式（简化）
loop {
    let pre = self.do_pre_turn_setup(ctx, state, ...).await?;
    match pre {
        PreTurnAction::Stop(result) => return result,
        PreTurnAction::Continue { .. } => {}
    }
    let response = self.call_llm(ctx, ...).await?;
    let action = self.process_post_response(ctx, &response).await?;
    match action {
        PostResponseAction::Finalize => { /* ... */ }
        PostResponseAction::Continue => { /* ... */ }
    }
    // 没有任何机制保证阶段顺序
}
```

**对抗性场景**：
- 一个 bug 导致 `do_pre_turn_setup` 返回 `Continue` 但跳过了 interrupt 检查 → agent 无法被中断 → 用户只能强杀进程
- `process_post_response` 返回 `Finalize` 但 `do_tool_turn` 的结果还没写入 ctx → LLM 下一轮看到的上下文缺少工具结果 → 重复执行

**根因**：AL-6 定义了 `LoopPhase` 枚举但没有将其作为状态机的核心驱动。枚举只用于日志，不用于控制流。

**解决方案**：

将 `LoopPhase` 作为状态机的核心驱动：

```rust
enum LoopState {
    Init,
    PreTurnSetup { /* ... */ },
    LlmCall { route: TurnRuntimeRoute, model: String },
    PostLlmCall { response: LlmResponse },
    ToolValidation { calls: Vec<ToolCall> },
    ToolExecution { results: Vec<ToolResult> },
    PostToolExec { /* ... */ },
    Finalize { result: AgentResult },
    Stopped,
}

impl AgentLoop {
    async fn run_engine(&self, ctx: &mut ContextManager) -> AgentResult {
        let mut state = LoopState::Init;
        loop {
            state = match state {
                LoopState::Init => self.transition_to_pre_turn(ctx).await?,
                LoopState::PreTurnSetup { .. } => self.transition_to_llm_call(ctx).await?,
                LoopState::LlmCall { .. } => self.transition_to_post_llm(ctx).await?,
                // ... 每个状态只能转换到合法的下一个状态
                LoopState::Finalize { result } => return result,
                LoopState::Stopped => return Ok(/* ... */),
            }
        }
    }
}
```

**预计**：2-3 周（需要仔细映射所有合法的状态转换）

---

### T2. 收敛检测设计依赖 LLM 自我纠正——但 LLM 在循环中自我纠正能力差

**现状**：

AL-8（修订 2026-07-09）将 `NoProgressWithTools` 和 `SimilarContent` 从硬停止改为**建议性警告**（注入系统提示，LLM 自己决定是否停止）。只有 `ErrorLoop`（连续 N 轮所有工具全部报错）才硬停止。

**对抗性场景**：
- Agent 连续 8 轮调用 `read_file` 读不同文件但每次都返回相似的分析 → `SimilarContent` 触发建议性警告 → LLM 看到警告后"决定继续"（因为还没找到用户要的东西）→ 继续读更多文件 → 上下文膨胀 → 最终 API 400
- Agent 连续 5 轮调用 `web_search` 但搜索结果越来越不相关 → `NoProgressWithTools` 触发建议 → LLM 换了关键词但仍然没进展 → 继续循环 → token 预算耗尽

**根因**：LLM 在循环中的自我纠正能力是有限的。研究表明，LLM 倾向于"坚持当前策略"而非"放弃当前策略"。将收敛检测改为建议性等于放弃了一个重要的安全阀。

**解决方案**：

采用**分层收敛策略**：

| 检测模式 | 当前行为 | 建议行为 |
|---------|---------|---------|
| `ErrorLoop` (连续 N 轮全工具报错) | 硬停止 | 保持硬停止 ✅ |
| `NoProgressWithTools` (连续 N 轮工具调用但无可见文本) | 建议性警告 | **硬停止 + 注入总结提示** |
| `SimilarContent` (连续 N 轮可见文本 >80% 相似) | 建议性警告 | **建议性警告 + 递增紧迫度**：第一次 warn，第二次 error，第三次硬停止 |
| `ContextBudgetExceeded` (上下文接近模型限制) | 无检测 | **硬停止 + 触发压缩** |

**预计**：3 天

---

### T3. ~~全局环境变量修改导致多会话数据竞争~~ (已修复)

> **2026-07-30 确认**：`local_agent_service.rs:1065-1069` 已移除 `std::env::set_var("TERMINAL_CWD", ws)`，改为注释说明工作区通过 `ToolRegistry::set_terminal_workspace` 路径传递。此问题已修复。

---

### T4. 工具并发执行无背压机制

**现状**：

`tool_executor.rs` 使用 `futures::join_all` 并发执行工具调用。LLM 可以在一轮中发出 N 个 tool_call，所有工具同时执行。

**对抗性场景**：
- LLM 一轮发出 10 个 `web_search` 调用 → 10 个并发 HTTP 请求 → 目标站点 rate limit → 10 个请求全部 429 → 10 个错误结果塞回 ctx → 下一轮 LLM 困惑
- LLM 一轮发出 8 个 `read_file` 调用，每个文件 5MB → 40MB 数据同时读入内存 → 加上 base64 编码膨胀 → 内存峰值 120MB+
- LLM 一轮发出 5 个 `delegate_task` 调用 → 5 个子 agent 同时启动 → 5 个并发 LLM API 调用 → API rate limit → 子 agent 全部失败

**根因**：`futures::join_all` 是"全并发"语义，没有信号量限制并发度。`AgentConfig::max_concurrent_delegates` 只限制 `delegate_task` 的并发数，不限制其他工具。

**解决方案**：

引入全局工具执行信号量：

```rust
pub struct AgentLoop {
    /// 全局工具并发上限。默认 = 5。
    tool_concurrency_semaphore: Arc<tokio::sync::Semaphore>,
}

// 在 execute_tool_calls 中：
let permits = (0..calls.len())
    .map(|_| self.tool_concurrency_semaphore.acquire())
    .collect::<Vec<_>>();
let _permits = futures::future::try_join_all(permits).await?;
// 然后执行工具
```

另外，按工具类型设置不同的并发限制：
- I/O 密集型（`read_file`, `web_search`）：max 5
- CPU 密集型（`code_execution`）：max 2
- 网络/API 密集型（`delegate_task`, `web_search`）：max 3

**预计**：3 天

---

### T5. 降级链路多层级叠加——行为不可预测

**现状**：

Agent Loop 的"LLM 输出质量不佳"降级链路：

```
LLM 输出
  → stream_collect 过滤空名工具调用 (AL-4)
  → process_post_response 判断有无工具调用
    → 有工具调用 → 正常执行
    → 无工具调用 + 有可见文本 → Finalize (AL-5 修正)
    → 无工具调用 + 无可见文本 (thinking-only) → decide_empty_response
      → 注入总结提示 → 下一轮 LLM
      → 如果仍然空 → empty_response_handler
        → 如果连续 N 轮空 → fallback 消息注入
          → local_agent_service 检测空可见文本 → fallback "已完成。"
```

**对抗性场景**：
- LLM 连续 3 轮返回 thinking-only → 每轮注入不同的"总结提示" → ctx 中积累 3 个系统消息 + 3 个空 assistant 消息 → 第 4 轮 LLM 困惑，不知道该总结什么
- `decide_empty_response` 和 `empty_response_handler` 的边界不清晰——前者是"决定是否注入提示"，后者是"处理空响应"——但两者都修改 ctx，可能重复注入
- fallback 消息 "已完成。" 被注入后，LLM 下一轮看到这条消息可能认为任务真的完成了 → 停止执行 → 用户任务未完成

**根因**：每一层降级都是独立添加的（AL-1 到 AL-5 逐步引入），没有从整体设计降级链路。每层都"信任 LLM 会自己纠正"，但多层叠加后 LLM 的上下文被污染。

**解决方案**：

1. **合并降级路径**：`decide_empty_response` 和 `empty_response_handler` 合并为单一决策点
2. **限制降级轮数**：连续 2 轮 thinking-only 后直接触发总结提示（而非无限重试）
3. **移除 fallback 消息注入**：AL-3 将 fallback 简化为 "已完成。"，但这仍然是一条伪造的 LLM 输出。应该改为向用户显示"Agent 未能生成回复，请重试或调整指令"的错误提示
4. **降级路径可视化**：在 TurnReport 中记录降级路径，方便调试

**预计**：1 周

---

### T6. 子代理递归防护仅靠 prompt——无编译期保证

**现状**：

`sub_agent_safety.rs` 通过 `BLOCKED_TOOLS_FOR_SUBAGENT` 在工具列表中物理移除 `delegate_task`，防止子代理递归委托。`sub_agent_guidance` 在子代理的 system prompt 中注入 HARD OVERRIDE。

**对抗性场景**：
- `BLOCKED_TOOLS_FOR_SUBAGENT` 是一个 `&[&str]` 数组，通过字符串匹配移除工具。如果 `delegate_task` 被重命名为 `delegate` 或 `task_delegate`，防护失效
- 子代理虽然没有 `delegate_task` 工具，但可以通过 `terminal` 工具调用 `hermix cli delegate` 命令绕过
- 子代理可以通过 `code_execution` 工具编写代码调用 hermix API 绕过

**根因**：工具级别的防护是"黑名单"模式（移除特定工具），而非"白名单"模式（只允许特定工具）。

**解决方案**：

1. 子代理使用**白名单模式**：`SubAgentConfig` 携带 `allowed_tools: Vec<String>`，只有列出的工具可用
2. `delegate_depth` 检查从 prompt 层移到 `tool_executor` 层：在 `execute_tool_calls` 入口检查 `if self.delegate_depth >= MAX_DEPTH && tc.function.name == "delegate_task" { return error }`
3. `code_execution` 和 `terminal` 工具在子代理模式下自动沙箱化（禁止网络、限制文件系统访问）

**预计**：1 周

---

## 5. 解决方案：分阶段路线图

### Phase 1：安全与正确性（1-2 周）

| # | 问题 | 动作 | 预计 | 优先级 |
|---|------|------|------|--------|
| S1 | T3 全局环境变量竞争 | ~~移除 `set_var`~~ **已修复**（代码已用 ToolRegistry 路径替代） | — | ✅ |
| S3 | T2 收敛检测分层策略 | NoProgressWithTools 改为硬停止 | 3d | P0 |
| S4 | C2 run/run_stream 合并 | 提取 `run_engine` + `LoopMode` | 1w | P1 |
| S5 | T4 工具并发背压 | 引入 Semaphore + 按类型限流 | 3d | P1 |

### Phase 2：架构治理（2-4 周）

| # | 问题 | 动作 | 预计 | 优先级 |
|---|------|------|------|--------|
| S6 | A1 hermix-agent 拆分 | 拆出 hermix-compression + hermix-session | 2w | P1 |
| S7 | A2 实验代码隔离 | cognition/self-improvement feature gate | 1w | P1 |
| S8 | A3 lint 清理 | unused → warn → 自动修复 | 1w | P1 |
| S9 | C1 AgentLoop 拆分 | 提取 CompressionSubsystem + MemorySubsystem | 2w | P1 |
| S10 | A4 提示词外部化 | directions.rs → 文件系统 | 1w | P2 |

### Phase 3：代码质量（4-8 周）

| # | 问题 | 动作 | 预计 | 优先级 |
|---|------|------|------|--------|
| S11 | C3 压缩系统整理 | 拆分 compression.rs + 提取子系统 | 2w | P2 |
| S12 | C4 持久化层重构 | session_persistence.rs → 多 repo 模块 | 3w | P2 |
| S13 | C5 前端组件拆分 | ChatView/App/MessageBubble 按职责拆 | 2w | P2 |
| S14 | C6 类型自动生成 | 引入 ts-rs/specta | 1w | P2 |
| S15 | T1 状态机强制化 | LoopPhase 驱动状态机 | 2w | P2 |

### Phase 4：执行逻辑加固（持续）

| # | 问题 | 动作 | 预计 | 优先级 |
|---|------|------|------|--------|
| S16 | T5 降级链路简化 | 合并决策点 + 限制降级轮数 | 1w | P2 |
| S17 | T6 子代理白名单 | allowed_tools + depth 检查移到执行层 | 1w | P2 |
| S18 | A5 server 依赖收敛 | 非核心依赖改 optional feature | 1w | P3 |

---

## 6. 对抗性自检

### F1: crate 拆分引入循环依赖

**场景**：`hermix-compression` 需要调用 `hermix-agent` 的 `ContextManager`，而 `hermix-agent` 需要调用 `hermix-compression` 的 `CompressionEngine`。

**应对**：
- `CompressionEngine` trait 定义在 `hermix-core`（L5 基础设施层），不在 `hermix-compression`
- `hermix-compression` 依赖 `hermix-core`（获取 trait + Message 类型）
- `hermix-agent` 依赖 `hermix-compression`（获取实现）
- 无循环

### F2: 状态机强制化导致无法处理边缘情况

**场景**：某些边缘情况需要跳过某个阶段（如用户 interrupt 后直接 Finalize，跳过 ToolExecution）。

**应对**：
- 状态机允许"快捷转换"：`PreTurnSetup → Finalize`（interrupt 时）
- 每个状态有一个 `fn next_states() -> &[LoopState]` 方法定义合法转换
- 非法转换在 debug build 中 panic，在 release build中 log + 降级到 `Finalize`

### F3: 提示词外部化后首次启动找不到文件

**场景**：用户从旧版本升级，`$HERMIX_HOME/prompts/` 目录不存在。

**应对**：
- `directions.rs` 保留编译期默认值作为 `const`
- 运行时先检查文件系统，不存在则用 `const` 默认值 + `tracing::warn!`
- 首次启动时将默认值写入 `$HERMIX_HOME/prompts/`（类似 config 迁移）

### F4: 工具并发限制导致长任务执行变慢

**场景**：用户需要并发处理 20 个文件，但信号量限制为 5。

**应对**：
- 信号量限制是**每轮**的，不是**每会话**的
- LLM 可以在多轮中分批处理（每轮 5 个，4 轮完成 20 个）
- `delegate_batch`（L2 批量处理，ARCHITECTURE_REFACTOR_PLAN.md §7.5）有自己的并发控制，不受此限制

### F5: 本报告遗漏了什么

**已知未覆盖**：
- `hermix-gateway` 的 IM 平台集成质量（微信/钉钉/企微回调）——需要实际运行环境验证
- `hermix-cua` 的 UI Automation 稳定性——需要实际 Windows 环境测试
- `hermix-cron` 的定时任务可靠性——需要长时间运行验证
- 前端 Tauri 命令的性能分析——需要 profiling
- 已有报告（CODEBASE_AUDIT_REPORT 等）中的 P0-P9 条目——本文不重复

**建议**：本报告应与 `STABILITY_ENGINEERING_PLAN.md` 和 `CODEBASE_AUDIT_REPORT.md` 结合阅读，三者互补覆盖了代码质量、架构质量、稳定性工程、任务执行逻辑四个维度。

---

## 附录：已有报告交叉索引

| 本报告条目 | 关联已有报告条目 | 关系 |
|-----------|----------------|------|
| A1 (God Crate) | ARCHITECTURE_REFACTOR_PLAN.md P0-3 | 深化——P0-3 只提 agent_loop.rs 单文件，本文提整个 crate |
| A3 (Lint 抑制) | 无 | **全新发现** |
| A4 (提示词硬编码) | 无 | **全新发现** |
| A6 (无沙箱) | memory: P0 issue bridge.rs | 代码级确认 |
| C1 (God Object) | ARCHITECTURE_ISSUES_REPORT.md L1 | 深化——L1 只提字段多，本文分析关注点混合 |
| C2 (run/run_stream) | ARCHITECTURE_REFACTOR_PLAN.md §6 P0 | 确认仍未修复 |
| T1 (状态机) | ARCHITECTURE_REFACTOR_PLAN.md §6 AL-6 | 深化——AL-6 引入枚举但未驱动控制流 |
| T2 (收敛检测) | ARCHITECTURE_REFACTOR_PLAN.md §6 AL-8 | 对抗——质疑建议性策略的安全性 |
| T3 (环境变量) | ARCHITECTURE_ISSUES_REPORT.md H1 | 确认仍未修复 |

---

*本报告基于 2026-07-30 的代码快照生成。所有问题均通过源码阅读 + 模式搜索 + 第一性原理推导发现。*
