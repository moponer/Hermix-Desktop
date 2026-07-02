# BaiLongma 启发方案: hermes-agent-rs 下一阶段规划

> **调研日期**: 2026-06-07
> **修正日期**: 2026-06-08(主人纠正"不适用"判断后)
> **⚠️ 2026-06-09 主决策**: L2(多平台社交分发) + L5(资源自动感知) 永久不做(主人原话"做好自己,不要去感知外部")。详见 `PROGRESS_TRACKER.md` §三 L 系列。
> **信息源**:
> 1. [BaiLongma 博客 - 一种让 Agent 产生时间感的通用方法](https://bailongma.top/blog/agent-time-sense-runtime-theory)
> 2. [BaiLongma 博客 - 预判注入机制,一种超级快的响应过程](https://bailongma.top/blog/speculative-injection-mechanism-agent-context-prefetch)
> 3. [BaiLongma 博客 - 一切皆记忆:一种新的 Agent 设计思路](https://bailongma.top/blog/memory-as-agent-architecture)
> 4. [GitHub: xiaoyuanda666-ship-it/BaiLongma](https://github.com/xiaoyuanda666-ship-it/BaiLongma) (297⭐, v2.1.343, 18 crate 同形态)
> 5. [腾讯云: 别让 Skill 列表烧光你的 Token](https://cloud.tencent.com/developer/article/2648902)
>
> **对比对象**: hermes-agent-rs(18 crate, 146,598 行 Rust, 完整 monorepo)

---

## ⚠️ 修正说明(2026-06-08 主人纠正后)

**主人原话**:
> "我需要你能够将刚开始的方案的能力慎重思考下,我觉得都可以的,为啥有那么多不行了"

**之前错误的判断**:
- ❌ "工具路由对桌面端价值小" → **错的**,30+ 工具仍是 ~4500 token/turn 浪费
- ❌ "统一记忆层对桌面端过重" → **错的**,抽象是给插件作者用的
- ❌ "TickLoop 不适合桌面端" → **部分对**,主循环不需要但各组件仍有价值
- ❌ "ACUI Web Components 不适合" → **错的**,能力(AI 推 UI)与实现(技术栈)是两码事
- ❌ "本地向量嵌入对桌面端意义小" → **错的**,离线+隐私+速度是绝对价值
- ❌ "启动自检 = 扫描 SSH/Git/桌面/其他 AI Agent" → **错的**,启动自检是健康检查

**修正原则**:
1. **"延后" ≠ "取消"** — 任何能力在它对应场景下都有价值,只是优先级
2. **"产品定位" ≠ "技术能力约束"** — 纯桌面端是产品定位,工具路由/记忆层是技术能力
3. **每项能力都要找到自己的位置** — 不轻易砍,认真想"它在哪一阶段、用什么形式"

**修正后的完整方案见** [`PROPOSAL_PORTABLE_DESKTOP.md` §十](./PROPOSAL_PORTABLE_DESKTOP.md),包含 12 项能力(无遗漏)和 13 个 Phase。

---

## 一、TL;DR

| 维度 | BaiLongma(白龙马) | 我们 hermes-agent-rs | 差距 & 机会 |
|------|-------------------|---------------------|------------|
| **运行模式** | TICK 心脏跳动,持续运行,抢占式 | 请求-响应 + Cron 触发,passive | 缺 TICK 主循环,缺"持续存在"感 |
| **记忆体系** | SQLite + FTS5 + 向量嵌入双路召回,焦点栈,salience 重排 | 8 个 memory provider plugin(mem0/hindsight/byterover/openviking...),各自独立 | 缺统一抽象、缺焦点栈、缺 salience 调度 |
| **意图预判** | 3 种方式:语义记忆预取 / 工作模式识别 / 定时预热缓存,fan-out 并行 | 串行:模型决定 → 工具调用 → 等待 → 模型决定 | 缺并行 fan-out、缺预热缓存层 |
| **Skills 路由** | 全量注入(被文章 #5 批评:95 skill → 5238 token) | 工具 schema 全量注入 system prompt(schemas()) | 缺按需检索 + 路由,Token 浪费严重 |
| **UI 哲学** | Brain UI 实时可视化"意识流"(SSE 推送) | hermix-server dashboard(4494 行,静态路由) | 有底子,但缺"心电图"实时事件流 |
| **TTS/ASR** | 本地 Whisper + 4 个 TTS provider + 凭证面板 | hermix-tools/voice_mode.rs 已有但分散 | 有基础,需整合 |
| **可观测性** | 启动自检 / 看门狗 / 限流降级 | hermix-telemetry(MetricsRegistry + Prometheus) | 有基础,可加深 |
| **多 Agent 协调** | 双层思考 L1/L2 / 焦点栈压缩 | delegation / sub_agent_orchestrator(617 行) | 雏形,未做"焦点压缩" |

**核心结论**: BaiLongma 走的是**"持续存在 + 按需记忆"**的认知哲学路线,我们的 `hermes-agent-rs` 走的是**"工具完备 + 多平台 + 全静态二进制"**的工程完备路线。**两者不冲突,可融合**。

---

## 二、详细对比

### 2.1 运行模式: TICK vs Request-Response

| 维度 | BaiLongma | 我们 |
|------|-----------|------|
| 主循环 | `src/index.js` 自适应 TICK 心跳,忙时秒级、闲时 20 分钟 | agent_loop.rs 是单次请求循环,无"持续存在" |
| 中断机制 | 高优先级消息 abort 当前 LLM + 自动重试 | `InterruptController` 已有,但只接 cron 唤醒 |
| 自我唤醒 | 启动 10 个 TICK "觉醒期" 主动探索 | 无,只能等用户消息或 cron 触发 |
| 限流降级 | 429/配额按 quota_interval 自动调度 | `rate_limit.rs` 有,但未与主循环联动 |
| 看门狗 | `runTurn` 超 180s 强制 abort | turn budget 有,但无 watchdog |

**借鉴点**:
- 引入 `TickLoop` 概念:在 hermix-agent / hermix-server 中加一个 `RuntimeLoop` trait,让"持续运行"成为可选项
- 借鉴"觉醒期":首次启动时跑一段自检 + ~~资源感知(SSH/Git/桌面/其他 AI Agent)~~ [❌ **2026-06-09 主人决策永久不做**,改为只做系统健康检查(FS/内存/磁盘/LLM连通性),不扫描外部资源]
- `hermix-cron` 已经是 tick 模式(每 N 秒扫一次),但只触发 cron job,没接入 agent loop 的"主动思考"

### 2.2 记忆体系: 焦点栈 + 统一抽象

BaiLongma 的核心数据结构:

```
焦点栈(FocusStack)
├── frame_0: created (刚创建)
├── frame_1: pushed (子任务进入)
├── frame_2: kept (保持)
├── frame_3: returned (子任务返回)
└── frame_4: cleared (清理为结论)
```

**每帧 pop 时自动压缩为 `focus_conclusion`,挂回新栈顶 + 沉淀为长期记忆。**

我们的记忆体系:

```
crates/hermix-agent/src/memory_manager.rs       (657 行, MemoryManager 协调)
crates/hermix-agent/src/memory_plugins/
├── hindsight.rs      (835 行)
├── holographic.rs    (777 行)
├── openviking.rs     (483 行)
├── retaindb.rs       (484 行)
├── byterover.rs      (404 行)
├── supermemory.rs    (381 行)
├── honcho.rs         (430 行)
└── mem0.rs           (317 行)
```

每个 plugin 独立、各有 schema,缺统一抽象。

**借鉴点(3 个等级)**:

1. **L0 - 立即做(无风险)**: 定义 `MemoryLayer` trait,8 个 plugin 都实现,统一接口 `recognize(intent) → MemoryHit[]` 和 `inject(hits) → SystemPromptBlock`
2. **L1 - 1-2 周**: 引入 `FocusStack<T>`,作为 `MemoryManager` 的内部结构(栈底 = 长期记忆,栈顶 = 当前焦点),借鉴 5 种状态(kept/pushed/returned/cleared)
3. **L2 - 1 个月**: 加 `salience` 打分,把 BaiLongma 的"星标 + 时间近因"算法移植到 Rust,用 `redb` 或 `fjall` 做嵌入式 K-V,本地计算 cosine

**核心收益**: 把"8 个外部 memory provider"降级为可选 backend,统一为"焦点栈 + 注入器"。

### 2.3 工具/技能路由: 5238 Token 烧光问题

腾讯云文章 #5 提供的核心数据:

| 指标 | 优化前 | 优化后 | 提升 |
|------|--------|--------|------|
| System Prompt Token | 5,238 | 391 | **-92.5%** |
| 单轮成本 | $0.0053 | $0.0004 | **-92.5%** |
| Top-1 命中率 | N/A | 80%+ | — |

**核心方案**: 同步 Plugin Hook(`before_prompt_build`)在 Agent Loop 内部完成向量检索,当轮生效。

我们的现状:

```rust
// crates/hermix-tools/src/registry.rs
impl ToolRegistry {
    pub fn schemas(&self) -> Vec<ToolSchema> { ... }  // 全量返回
    pub fn names(&self) -> Vec<String> { ... }
}
```

```rust
// crates/hermix-skills/src/skill.rs
pub struct SkillManager {
    hub_client: Option<SkillsHubClient>,
    store: Arc<dyn SkillStore>,
}
```

工具 schema + skill 全量注入到 system prompt,跟 BaiLongma 一样的问题。

**借鉴点**:

1. **L0 - 立即做(同步 Hook)**: 在 `hermix-intelligence::prompt.rs` 加 `tool_router.rs`,实现:
   - `ToolSchemaRouter` trait: `route(turn_context) → Vec<ToolSchema>`(只返回相关 N 个)
   - 第一版用关键词 + LRU,后期接 `instant-distance` 或 `usearch` 向量路由
2. **L1 - 1-2 周**: 复用 hermix-telemetry 的 `prompt_cache_hits/misses`,加 `tool_router_hits` 统计
3. **L2 - 1 个月**: 支持 Jina v5 / OpenAI text-embedding-3-small Embedding,本地 ONNX 推理,30+ 工具降到 top-5

### 2.4 上下文预取: 预判注入机制

BaiLongma 文章 #2 的核心模式:

```
用户说"帮我写今日早报"
  ↓ 系统识别"写早报"意图(语义分类,非关键词)
  ↓ 并行 fan-out:
  ├─ 查天气
  ├─ 查新闻
  └─ 看日程
  ↓ 结果 fan-in 注入上下文
  ↓ 模型一开口就看到所有信息,直接写
```

**铁律**: 只能预判"查"的事,不能预判"改"的事。

我们的 `agent_loop.rs` 当前模式(7,694 行):

```rust
// 1. Send messages + tools to the LLM
// 2. If the LLM responds with tool calls, execute them (in parallel)  ← 这里已经并行!
// 3. Append results to conversation history
// 4. Repeat until the model finishes naturally
```

**好消息**: 我们**已经支持**并行工具调用(fan-out 阶段),但**没有**预判/预取阶段。

**借鉴点**:

1. **L0 - 立即做**: 在 agent_loop.rs 现有 `runTurn` 前加 `prefetch_phase`,识别高频 intent(早报/日报/天气/日程)后并行调用只读工具
2. **L1 - 1-2 周**: `PrefetchIntent` enum + 模式学习器(从历史 tool_call 序列提取模式)
3. **L2 - 1 个月**: 预热缓存层(`hermix-cache` crate),用 `moka` 做 TTL 缓存,启动时后台异步预热

### 2.5 UI 哲学: Brain UI 实时意识流

BaiLongma 的 Brain UI 设计哲学:**SSE 实时推送"意识流"**,用户能看到:
- 工具调用实时
- 记忆注入实时
- 焦点变化实时
- 思考流实时

我们的 desktop-tauri 现状:

```
apps/desktop-tauri/src/
├── App.tsx (37913 字节)
├── api.ts (36105 字节)
├── components/ChatView.tsx
├── components/MessageBubble.tsx        ← 已有 LiveTerminal(本轮刚加)
├── components/ExecutionTimeline.tsx    ← timeline 已经存在
├── components/RightSidebar.tsx
└── ... 20 个组件
```

已经做了 ExecutionTimeline(在 `hermix-server::handle_ws_stream` 推送),但 UI 渲染还在"折线 + 文字"层面,缺"心电图"风格的可视化。

**借鉴点**:

1. **L0 - 立即做**: 在 `MessageBubble.tsx` 加 `ThoughtStreamCard` 组件,显示本轮所有 chunk(已有 `ToolChunk` event 基础设施)
2. **L1 - 1-2 周**: 引入"思考流"实时时间线,3D 高度(过去/现在/未来),参考 BaiLongma 的 `agent-message-flow.html` 设计
3. **L2 - 1 个月**: 全屏"意识流"模式,横向时间轴 + 多通道(记忆/工具/焦点/思考)

---

## 三、可借鉴的功能点(按 ROI 排序)

### 高 ROI(立即可做,1 周内)

| # | 功能 | 来源 | 价值 | 估时 | 复杂度 |
|---|------|------|------|------|--------|
| **H1** | **工具 schema 按需路由**: 30+ 工具 → top-5,~92% token 节省 | 腾讯云 #5 | 极高(每轮省钱) | 3-5 天 | 中 |
| **H2** | **意图预判预取**: 识别"写早报"等高频模式 → 并行预取只读工具 | BaiLongma #2 | 高(降低延迟) | 3-5 天 | 中 |
| **H3** | **运行时事件流 → UI 实时意识流**: ToolStart/Chunk/Complete 已打通,加 LiveTimeline 组件 | BaiLongma Brain UI | 中(体验提升) | 1-2 天 | 低 |
| **H4** | **时间词召回**: 检测"昨天/上周",按时间窗口拉取历史 | BaiLongma #1 | 中(实用) | 1-2 天 | 低 |
| **H5** | **统一 MemoryLayer trait**: 8 个 plugin 统一接口,加 salience 打分 | BaiLongma #1 焦点栈 | 高(架构统一) | 2-3 天 | 中 |

### 中 ROI(1-2 周)

| # | 功能 | 来源 | 价值 | 估时 | 复杂度 |
|---|------|------|------|------|--------|
| **M1** | **FocusStack 抽象**: 5 状态帧栈(kept/pushed/returned/cleared) | BaiLongma #1 | 高(认知连续) | 1 周 | 高 |
| **M2** | **TICK 主循环**: hermix-runtime 持续运行 + 自适应节奏 | BaiLongma TICK | 中(场景扩展) | 1 周 | 高 |
| **M3** | **启动自检 + 觉醒期**: 文件读写/热点/视频 + 10 个 TICK 探索 | BaiLongma 启动 | 中(可观测) | 3-5 天 | 中 |
| **M4** | **预热缓存层**: moka + TTL,启动后台预热 | BaiLongma #2 缓存 | 中(性能) | 3-5 天 | 中 |
| **M5** | **ACUI 组件系统**: Agent 主动推送结构化 UI(Web Components 风格) | BaiLongma ACUI | 中(展示) | 1 周 | 中 |

### 低 ROI(1 个月+)

| # | 功能 | 来源 | 价值 | 估时 | 复杂度 |
|---|------|------|------|------|--------|
| **L1** | **本地向量嵌入**: ONNX + usearch/jina-v5 | BaiLongma FTS5+向量 | 高(但可后置) | 2 周 | 高 |
| **L2** | ~~**多平台社交分发**: 微信/Discord/飞书深度对接~~ | ~~BaiLongma 社交~~ | ❌ **2026-06-09 永久不做** | — | — |
| **L3** | **语音全流程**: Whisper 管理器 + 4 个 TTS 整合 | BaiLongma voice | 中(分散已有) | 1 周 | 中 |
| **L4** | **双层思考 L1/L2**: 快速响应 / 深度处理 | BaiLongma 双层 | 中(架构) | 2 周 | 高 |
| **L5** | ~~**资源自动感知**: 启动扫描 SSH/Git/桌面/其他 AI Agent~~ | ~~BaiLongma 资源~~ | ❌ **2026-06-09 永久不做** | — | — |

---

## 四、架构对比图

```
┌────────────────────────────────────────────────────────────────────────────┐
│                            hermes-agent-rs 当前架构                          │
├────────────────────────────────────────────────────────────────────────────┤
│                                                                            │
│  apps/desktop-tauri (React 19 + Tauri v2)                                   │
│       │                                                                    │
│       │  WebSocket /v1/ws-stream/{id}                                       │
│       ▼                                                                    │
│  hermix-server (HTTP+WS, 4494 行 dashboard)                                 │
│       │                                                                    │
│       │  LocalAgentService::send_message_stream                            │
│       ▼                                                                    │
│  hermix-agent::agent_loop.rs (7,694 行)                                     │
│       │                                                                    │
│       │  ──→ LLM Provider(10 个)                                            │
│       │  ──→ Tool Registry(30+ 工具, 全量注入)                                │
│       │  ──→ Memory Manager → 8 个 plugin(独立抽象)                           │
│       │  ──→ Skill Orchestrator → hermix-skills                             │
│       │  ──→ Cron 调度(每 N 秒扫一次)                                        │
│       │  ──→ Bus 抽象(hermix-bus,18 crate 新增)                              │
│       │                                                                    │
│       ▼                                                                    │
│  hermix-config::env_compat + hermix-intelligence::router                    │
│                                                                            │
└────────────────────────────────────────────────────────────────────────────┘

                                ⬇️ 融合 BaiLongma 启发

┌────────────────────────────────────────────────────────────────────────────┐
│                          融合后的目标架构(Phase 7-9)                        │
├────────────────────────────────────────────────────────────────────────────┤
│                                                                            │
│  apps/desktop-tauri (React 19 + Tauri v2)                                   │
│       │                                                                    │
│       │  + LiveTimeline(意识流实时)                                          │
│       │  + ACUI Components(Web Components 化)                                │
│       ▼                                                                    │
│  hermix-server (HTTP+WS)                                                    │
│       │                                                                    │
│       │  + ToolSchemaRouter(按需路由,top-5)                                   │
│       │  + PrefetchFanout(并行预取只读工具)                                   │
│       ▼                                                                    │
│  hermix-agent::agent_loop.rs                                                │
│       │                                                                    │
│       │  ──→ LLM Provider(10 个,不变)                                       │
│       │  ──→ Tool Registry(30+,接 Router)                                   │
│       │  ──→ FocusStack<T>(5 状态,salience)                                  │
│       │  ──→ MemoryLayer trait(unified 抽象)                                │
│       │  ├─→ Embedded(fjall/redb,本地)                                       │
│       │  ├─→ Hindsight / Mem0 / Honcho(8 个 plugin 仍可选)                  │
│       │  └─→ Time-aware Recall(时间词)                                       │
│       │  ──→ PrefetchCache(moka,TTL)                                        │
│       │  ──→ TickLoop(可选,后台持续运行)                                     │
│       │  ──→ Skill Orchestrator(接 Hub + Router)                             │
│       │  ──→ Bus 抽象(不变)                                                  │
│       │                                                                    │
│       ▼                                                                    │
│  hermix-runtime (新 crate, 持续运行 + 觉醒期 + 自检)                          │
│  hermix-cache (新 crate, moka 缓存 + 预热)                                  │
│  hermix-config + hermix-intelligence(不变)                                  │
│                                                                            │
└────────────────────────────────────────────────────────────────────────────┘
```

---

## 五、详细方案: 3 个 Phase

### Phase 7: 工具路由 + 上下文预取(2-3 周)

**目标**: 把 30+ 工具的"全量注入"问题解决,实现 BaiLongma 文章 #5 的"按需加载"模式

**新增 crate**: 无,只修改 `hermix-tools` + `hermix-intelligence`

**改动清单**:
1. `hermix-intelligence/src/tool_router.rs`(新文件,~400 行)
   - `ToolSchemaRouter` trait + `KeywordRouter` 实现(L0)+ `VectorRouter` 实现(L1)
   - `route(turn_context: &TurnContext) -> Vec<ToolSchema>`(返回 top-5)
2. `hermix-agent/src/context.rs` 修改 `SystemPromptBuilder`:
   - `add_tools_schemas()` 改为 `add_tools_schemas_via_router(&router)`
3. `hermix-telemetry/src/lib.rs` 加 `tool_router_hits/misses` 计数
4. `hermix-agent/src/agent_loop.rs` 加 `prefetch_phase`:
   - 识别高频 intent pattern("写早报"/"查天气"/"看日程"等)
   - 并行 fan-out 只读工具(L2 预热缓存接 moka)

**验收标准**:
- 单元测试:`KeywordRouter::route("帮我看天气")` 返回 `[weather_tool, calendar_tool]`,不返回 `terminal`
- Token 节省率:30+ 工具全量 schema ≈ 4500 token → 路由后 5 个 ≈ 800 token,节省 ~82%
- E2E:在 hermes-agent-rs 跑 e2e 任务"写今日早报",记录 token 消耗对比

### Phase 8: 统一记忆层 + 焦点栈(3-4 周)

**目标**: 把 8 个 memory plugin 抽象为统一 `MemoryLayer` trait,引入 `FocusStack` 借鉴 BaiLongma 文章 #1

**新增 crate**: `hermix-focus`(焦点栈,纯 Rust 内存结构 + 持久化到 sqlite)

**改动清单**:
1. `hermix-core/src/traits.rs` 加 `MemoryLayer` trait:
   ```rust
   #[async_trait]
   pub trait MemoryLayer: Send + Sync {
       async fn recognize(&self, intent: &Intent) -> Result<Vec<MemoryHit>>;
       async fn inject(&self, hits: &[MemoryHit]) -> Result<SystemPromptBlock>;
       async fn salience(&self, hit: &MemoryHit) -> f32;
   }
   ```
2. `hermix-focus/src/lib.rs`(新 crate,~600 行)
   - `FocusStack<T>` 泛型,5 状态(kept/pushed/returned/cleared/created)
   - `FocusFrame { id, content, salience, created_at, parent_id }`
   - `pop_compress()` → 自动 `focus_conclusion` 持久化
3. `hermix-agent/src/memory_manager.rs` 重构:
   - 引入 `MemoryRouter`,把 8 个 plugin 适配为 `MemoryLayer`
   - 加 `TimeAwareRecall`:解析"昨天/上周",按时间窗口拉 `focus_conclusion`
4. `hermix-skills/src/hub.rs` `SkillsHubClient` 加 `search_skill_by_intent()`(复用 router)

**验收标准**:
- 单元测试:`FocusStack` push/pop/keep 5 状态机
- 集成测试:8 个 plugin 都能适配 `MemoryLayer`
- E2E:说"昨天我们做了什么",自动召回昨天 24h 的 `focus_conclusion`

### Phase 9: TickLoop + 持续存在 + ACUI(2-3 周)

**目标**: 让 hermix-server 拥有"持续存在"模式,借鉴 BaiLongma 心脏跳动 TICK + ACUI 组件

**新增 crate**: `hermix-runtime`(持续运行 + 觉醒期 + 自检)

**改动清单**:
1. `hermix-runtime/src/lib.rs`(新 crate,~500 行)
   - `TickLoop` 结构(自适应节奏 20min 空闲 / 30s 任务中)
   - `AwakeningPhase`(启动 10 个 TICK 自动探索)
   - `SelfCheck`(文件读写 / 热点面板 / 视频播放 三项)
2. `hermix-runtime/src/watchdog.rs`(新文件,~150 行)
   - 借鉴 BaiLongma `runTurn` 180s 强制 abort
3. `apps/desktop-tauri/src/components/` 加 ACUI 组件
   - `WeatherCard.tsx`、`SelfCheckStepCard.tsx`、`AwakeningCard.tsx`
   - 复用 BaiLongma `ui_show` / `ui_update` / `ui_hide` 协议
4. `hermix-server/src/lib.rs` 集成 TickLoop(可选启动,默认 off)
5. `hermix-server/src/dashboard/` 加 `/consciousness` 路由(可视化心电图)

**验收标准**:
- 单元测试:`TickLoop::adjust_interval(状态)` 测试 4 种状态转换
- E2E:启动 `hermix-server` → 觉醒期 → 自检 → 进入"持续运行"模式 → desktop-tauri 显示心电图
- 性能:后台 TICK 不应让 P50 延迟退化 > 5%

---

## 六、给主人的问题(决策点)

| 决策点 | 选项 | 我的推荐 |
|--------|------|----------|
| **1. 优先级** | 先做 H1 (工具路由,省钱) 还是 H2 (意图预取,降延迟) 还是 H5 (统一记忆层) | **H1**(最直接 ROI,token 节省立竿见影) |
| **2. 工具路由实现** | L0 关键词 + LRU(Lightning) / L1 + usearch 向量(快 1 周) / L2 + Jina v5(本地 ONNX,稳) | **L0 先行**(参考 ES-skill-router 模式,先用关键词,数据驱动后升级向量) |
| **3. 记忆层抽象** | 8 个 plugin 直接弃用 / 8 个 plugin 适配统一 trait / 1 个本地 embedded + 7 个 plugin 适配 | **统一 trait + 本地优先**(高可用,7 个 plugin 仍可作为 backend) |
| **4. TICK 主循环** | 加进 hermix-server(影响范围大) / 新建 hermix-runtime 独立(干净) / 暂缓,先做工具路由 | **新建 hermix-runtime 独立**(不污染 server,baiLongma 的 TICK 是 1 个独立模块) |
| **5. ACUI 组件** | 用 Web Components(轻量,BaiLongma 同款) / 用 React 组件(跟我们栈一致) / 暂缓 | **Web Components**(BaiLongma 验证过,且不绑死前端栈) |

---

## 七、参考资料

1. **白龙马博客** (3 篇方法论):
   - [一种让 Agent 产生时间感的通用方法](https://bailongma.top/blog/agent-time-sense-runtime-theory) - TICK/焦点栈/时间戳
   - [预判注入机制](https://bailongma.top/blog/speculative-injection-mechanism-agent-context-prefetch) - 预取/fan-out/缓存
   - [一切皆记忆](https://bailongma.top/blog/memory-as-agent-architecture) - 记忆架构/识别器/注入器

2. **白龙马 GitHub**: [xiaoyuanda666-ship-it/BaiLongma](https://github.com/xiaoyuanda666-ship-it/BaiLongma) - 297⭐, v2.1.343, 18 crate 形态对比

3. **腾讯云**: [别让 Skill 列表烧光你的 Token](https://cloud.tencent.com/developer/article/2648902) - ES 语义搜索 + OpenClaw 同步 Plugin,token 节省 92.5%

4. **我们项目**: `<repo-parent>/hermes-agent-rs/` (18 crate, 146,598 行 Rust, 完整 monorepo)

5. **BaiLongma 独立 HTML 演示页** (直接打开看):
   - `agent-message-flow.html` - 消息流
   - `architecture-comparison.html` - 架构对比
   - `brain-ui.html` - Brain UI 演示
   - `focus-banner.html` - 焦点栈 banner
   - `turn-trace.html` - 单轮 trace

---

**状态**: 📋 调研完成,等主人拍板优先级
**作者**: Senior Developer (hermix-agent-rs 项目组)
**下一步**: 根据第 6 节决策点 → 进入实施
