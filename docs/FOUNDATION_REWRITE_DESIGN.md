# Hermix 底座重写方案 v2（PI 全面对齐 + 网络安全）

> 提案状态：**待主人评审，未动一行代码**。
> 取代：`docs/STABLE_FOUNDATION_DESIGN.md`（v1 渐进路线，其分析仍然有效，作为对照保留）
> 参考项目：https://github.com/earendil-works/pi
> 参考规范：`packages/durable/docs/pico-v5.md`、`packages/protocol/README.md`、`packages/server/README.md`、`packages/agent/README.md`
> 主人决策（2026-09-24）：要砍的砍，可推倒重写；底座全部参考 PI；网络安全先做；前端不大动。

---

## 0. 三条前置声明

在动手之前必须把三件事说清楚，否则方案会建在误解上。

### 声明 1：PI 是 TypeScript，Hermix 是 Rust。"全部参考 PI"的可行解释

PI 是 TS monorepo（12 个 npm 包）。**逐行翻译是不可能的，也是没意义的。**
本方案采用的解释是：

> **架构模式、不变量、状态机语义、协议契约全面对齐 PI；实现用 Rust 原生化完成。**

凡是 PI 有的机制，Hermix 必须有等价物（名字可不同，语义必须一致）。
凡是 PI 明确列为 Non-goal 的，Hermix 也不做。

如果主人要的是"逐行移植 TS 代码"，那等于用 Rust 重写一遍 TS 运行时，这在工程上是自毁——需要当场叫停。

### 声明 2：PI 没有网络安全，这一块它帮不上

PI 的网络相关原文（诚实摘录）：

| 出处 | 原文 |
|---|---|
| `packages/protocol/README.md` | `Peer authentication and authenticated service contexts are not implemented by the experimental transport.` |
| `packages/protocol/README.md` | `The protocol is experimental and has no compatibility guarantees.` |
| `packages/server/README.md` | `Server and worker lifecycle is intentionally outside this public protocol.` |
| `packages/server/README.md` | `peer authentication remains application policy and is not implemented by the experimental Unix transport.` |
| 根 `README.md` | `Pi does not include a built-in permission system... By default, it runs with the permissions of the user and process that launched it.` |

**PI 的 server 是 Unix domain socket —— 纯本地 IPC，它从未暴露过网络面，因此从不需要认证。**
主人要的"手机端连桌面端"是 PI 完全没走过的路。所以这一块**不能抄 PI**，只能：

- **抄 PI 的结构**：多展示端挂载（`attachmentId` 路由模型）——这个恰好就是多端需求的正解；
- **自己设计安全**：TLS + 设备配对 + 凭证层次 + 来源分级（见 §5）。

### 声明 3：Chord 划掉，不抄

`packages/chord` 的 README 第一段自己声明：

> `It is developed as a standalone package in the Pi monorepo, but it is not a Pi package: it does not depend on any other Pi workspace package and can be used by unrelated applications.`

Chord 是为"**同一功能需要在多个环境分片运行**（agent worker / TUI / remote WebUI）+ 插件跨进程加载"设计的组合运行时，含 facet bundler（esbuild 打包 + `node:vm` 沙箱加载 + SHA-256 校验）。

Hermix 没有这个需求。抄它 = 引入一套插件沙箱 + 复制状态机，纯负担。**明确划掉。**

---

## 1. 边界：重写 / 重构 / 砍掉 / 保留

这是本方案最重要的一节。主人说"哪怕推倒重写"，但 26.5 万行全推倒不现实也不必要——**要推倒的是底座，不是业务能力**。

### 1.1 完全重写（底座内核，约 2.3 万行被替换）

| 目标 | 现状 | 为什么必须重写 |
|---|---|---|
| **`hermix-store`**（新 crate） | 无。41 处裸 `Connection::open`，8 处独立 SQLite 访问 | 缺存储契约层，无从谈原子性与一致性 |
| **`hermix-session`**（新 crate） | 无。会话状态散在 `state.rs` 的 15 个 `Arc<Mutex>` + `ws.rs` | 缺会话内核，状态无唯一权威 |
| **`hermix-protocol`**（重写 `hermix-transport` 的协议部分） | `hermix-transport` 1248 行纯类型 + `server/transport.rs` 234 行 trait（两套并存） | 缺信封/seq/订阅模型，帧即状态 |
| **`hermix-server` 的会话/传输/状态部分** | `ws.rs` 3136 + `state.rs` 855 + `approval.rs` 1145 + `ws_grace.rs` 438 + `coalescer.rs` 315 ≈ 5900 行 | 这五块是同一条状态链，拆开重写没有意义 |
| **任务运行时** | `active_handles` 内存登记 + `hermix-task` 仓储 | 任务不是持久状态机，崩溃即孤儿 |

### 1.2 重构对齐（接口按 PI 调整，逻辑保留）

| 目标 | 现状 | 动作 |
|---|---|---|
| `hermix-agent` 的 loop 部分 | `run_loop.rs` 2698 + `agent_loop/` | 对齐 `pi-agent-core` 的 turn 生命周期（`prepareRequest` / `finishTurn` 钩子）、工具执行模式（parallel/sequential）、steering/followUp 队列语义；**状态读写改走 `hermix-session`** |
| `provider` 抽象 | `agent/provider.rs` 3202 行平铺 + `api_bridge.rs` + `intelligence/anthropic_adapter.rs` + 4 个工具各自实现重试 | 收敛为单一 provider 抽象（参考 `pi-ai`），删除重复重试/超时实现 |
| `hermix-telemetry` | 893 行，已能用 | 对齐 `pi-telemetry` 的 vendor-neutral 契约 |

### 1.3 砍掉（净省约 3.2 万行，零业务损失）

| 砍掉 | 行数 | 证据 |
|---|---|---|
| **`hermix-gateway` 整体** | **27451** | 全仓 Rust **零引用**（仅 2 条注释提及）；20 个平台适配器全 `cfg(feature=...)`，`default = []`，`server/Cargo.toml:29` 声明依赖但未启用任何 feature → 根本不参与编译 |
| **`hermix-acp`** | 2276 | lib-only、全仓 0 引用，且与 `hermix-agent/src/copilot_acp.rs` 重复实现 |
| `dashboard/stubs.rs` 大部分 | ~600 | 自述 "Stub endpoints for APIs the frontend expects but not yet fully implemented"，前端只用到 3 个 `/api/dashboard/plugins*` 端点 |
| `dashboard/consciousness.rs` + `researcher.rs` | 313 | 前端无任何调用 |
| 调度三套 → 一套 | ~1500 | `hermix-cron` / `hermix-automation` / `server/dashboard/automation.rs`（后者自述 "intentionally separate" 但存储与 tick 逻辑重复） |
| 任务/自动化双 REST 面 | — | `/v1/tasks` + `/v1/automations` 与 `/api/automation/tasks` 同域重复 |
| 传输两套抽象 | ~200 | `hermix-transport`（类型）vs `server/transport.rs`（trait），合并 |

### 1.4 保留（业务外壳，约 15 万行不动）

`hermix-tools`（52K）、`hermix-intelligence`（16.7K）、`hermix-plugin`（7.7K）、`hermix-focus`（8.5K）、`hermix-config`（7.3K）、`hermix-core`（7.2K）、`hermix-cua`（5.2K）、`hermix-environments`（4.6K）、`hermix-mcp`（3.6K）、`hermix-skills`（2.7K）、`hermix-voice`、`hermix-coreutils`、`hermix-shell-parser`、`hermix-telemetry`。

**这些 crate 的对外接口不变**，只把"读写状态"的路径从旧机制换成新的存储/会话内核。

---

## 2. PI 包 → Hermix 映射表

主人要求"底座代码必须全部参考 PI"。逐包交代抄什么、不抄什么。

| PI 包 | 职责 | Hermix 落点 | 策略 |
|---|---|---|---|
| `pi-durable`（Pico5） | Session / Entry / Task / Document / Storage 契约 / 崩溃恢复 | `hermix-store` + `hermix-session` | **核心参考**。八条硬性不变量逐条落地（见 §3.3） |
| `pi-protocol` | 信封、CBOR 编码、byte-stream framing、版本握手、路由、订阅 | `hermix-protocol` | **参考结构，不抄 CBOR**（见声明·选型） |
| `pi-server` | 多展示端挂载、Session 路由、worker 生命周期 | `hermix-server` 的会话部分 | **参考 attachment 模型**（正是多端需求的解） |
| `pi-agent-core` | agent loop、状态、工具调度、流式、steering | `hermix-agent` 的 loop | **参考接口语义**，逻辑保留 |
| `pi-ai` | 统一多 provider LLM API | `hermix-agent/provider.rs` + `hermix-intelligence` | **参考收敛**，消除平铺 |
| `pi-telemetry` | vendor-neutral 遥测契约 + conformance | `hermix-telemetry` | **参考契约** |
| `pi-client` | 客户端 SDK | 前端 TS 客户端 | 参考（前端不大动，仅加 seq 对账，见 §6） |
| `pi-evals` | 评测套件 | `scripts/` 稳定性套件 | 后续可选 |
| `pi-tui` | 终端 UI 库 | — | **不适用**（Tauri GUI，无 TUI 需求） |
| `chord` | 插件组合 / replicated state / facet bundler | — | **明确不抄**（见声明 3） |

### 选型偏离说明（2 处，必须主人知情）

| PI 的做法 | 本方案 | 理由 |
|---|---|---|
| CBOR 编码 + 4 字节大端长度前缀 framing | **保持 JSON 信封**，沿用现有 `WsEnvelope` 形状 | 前端零大动是硬约束；CBOR 换来的是调试痛苦和全栈改造。JSON 在桌面/手机局域网带宽下不是瓶颈 |
| Unix domain socket 传输 | **可插拔传输**：stdio（现状）/ Unix socket / TCP+TLS | 手机端必须走网络；stdio 要保留（桌面端现状不能破） |

---

## 3. 底座新架构

### 3.1 总览

```
┌────────────────────────────────────────────────────────────────────┐
│ 业务外壳（保留，接口不变）                                            │
│  tools · intelligence · skills · cua · plugin · focus · mcp · voice │
├────────────────────────────────────────────────────────────────────┤
│ L4  hermix-server                                                   │
│     会话宿主 + 多展示端挂载(attachment) + 路由                        │
│     + 网络安全层（§5）                                               │
├────────────────────────────────────────────────────────────────────┤
│ L3  hermix-protocol                                                 │
│     信封 {kind, route, seq, payload} · 版本握手 · 订阅 · 错误码        │
├────────────────────────────────────────────────────────────────────┤
│ L2  任务运行时（hermix-runtime 扩展）                                 │
│     TaskRecord 持久状态机 · Effect Sandwich · 崩溃恢复                │
├────────────────────────────────────────────────────────────────────┤
│ L1  hermix-session  ★ 会话内核                                      │
│     Session{变更线, entries, tasks, docs} · commit 协议              │
├────────────────────────────────────────────────────────────────────┤
│ L0  hermix-store  ★ 存储契约                                        │
│     trait Storage { commit, mint_id, find*, scan* }                 │
│     SqliteStore（生产） / MemoryStore（测试）                        │
└────────────────────────────────────────────────────────────────────┘
```

### 3.2 L0 `hermix-store`：存储契约

对齐 `pico-v5.md` 的 Storage 接口，Rust 化：

```rust
#[async_trait]
pub trait Storage: Send + Sync {
    /// 原子提交一批写入，返回提交序号。失败则什么都不持久化。
    async fn commit(&self, writes: Vec<StorageWrite>) -> Result<Seq, StorageError>;
    /// 全局唯一 ID 分配（永不复用）
    async fn mint_id(&self) -> Result<Id, StorageError>;

    // 精确身份查找
    async fn conversation(&self, id: Id) -> Result<Option<ConversationRecord>, StorageError>;
    async fn entry(&self, id: Id) -> Result<Option<EntryRecord>, StorageError>;
    async fn task(&self, id: Id) -> Result<Option<TaskRecord>, StorageError>;

    // 有序扫描（cursor 分页）
    async fn scan_entries(&self, conv: Id, cursor: Option<Cursor>, limit: usize)
        -> Result<Page<EntryRecord, Cursor>, StorageError>;
    // ...
}
```

**关键约束（来自 Pico5）**：

1. **一个 SQL 事务 = 一次 commit**（SQLite 后端）。部分成功 = 不存在。
2. **ID 永不复用**，已提交的写永不覆盖。
3. **扫描用 cursor，不做偏移分页**（偏移分页在并发提交下会跳项/重项）。
4. **读取永远返回 detach 过的值**（不改动调用者看到的快照）。
5. **存储失败分类**：准备阶段失败 → 正常回滚；不确定失败 → 毒化当前 Session（不发布任何内容）。

**同时兑现 v1 的 P0-1**：连接池（`rusqlite` + `deadpool-sqlite`），41 处裸连接归零，8 处独立 SQLite 访问收敛到此。

### 3.3 L1 `hermix-session`：会话内核（核心）

#### 八条硬性不变量（逐条落地 PI 的，Hermix 版）

| # | PI 原文 | Hermix 落地 | CI 守卫 |
|---|---|---|---|
| I1 | 一次 Session commit 在所有记录与文档写入上是原子的 | 变更线 + 单事务提交 | 并发测试：1000 并发写无交错 |
| I2 | 文档更新只在其存储提交成功后才发布 | 发布路径必须在 `commit()` 返回之后 | 静态扫描发布调用点 |
| I3 | 所有可见进度都是持久的，不存在易失发布路径 | **唯一例外**：流式展示 delta（显式标注，见 F1） | 例外清单白名单 |
| I4 | 外部副作用不在 Session 变更事务内运行 | Effect Sandwich | 注入副作用失败测试 |
| I5 | 条目与 ID 不可变，已提交写后永不复用 | `EntryRecord` 无 `mut`，ID 单调 | 类型系统保证 |
| I6 | 事务回调结算时草稿被撤销；赋值进入草稿的值按值复制 | `Draft` 生命周期绑定 raft callback | 借用检查器 |
| I7 | 变更线在存储结算与采纳期间保持持有；监听器稍后在线外运行 | `MutationLine` guard 跨 commit，通知在 drop 后 | 并发测试 |
| I8 | 不确定的存储失败对打开的 Session 是致命的（poisoned） | `SessionState::Poisoned`，重开可恢复 | 故障注入测试 |

#### 提交协议

```
1. 持线（acquire mutation line）        ← 串行化，同 session 第二个提交排队
2. 起草（Draft）                        ← 可撤销，失败即弃
3. 冻结（prepare → 不可变写入集）        ← 此后再改草稿无效
4. 提交（store.commit → 原子落盘）      ← 一个 SQL 事务
5. 采纳 + 释放线
6. 发布（通知监听器）                    ← 在线外运行
```

**任一步失败 → 回滚草稿，不采纳、不发布**：对外状态与提交前完全一致。

#### 数据模型（对齐 Pico5）

```rust
pub struct ConversationRecord {
    pub id: Id,
    pub parent: Option<(Id, Id)>,   // 历史继承（parent）与授权中止（owner）分离
    pub owner: Option<(Id, Id)>,
}

pub struct EntryRecord {
    pub id: Id,
    pub conversation_id: Id,
    pub kind: EntryKind,            // user / assistant / tool / system / approval / approval_decision
    pub model: Option<Vec<Message>>,
    pub data: Option<JsonValue>,
    pub head: Option<Id>,
    pub edits: Vec<ContextEdit>,
    pub by_task_id: Option<Id>,
}

pub enum TaskState {
    Pending  { checkpoint: Checkpoint },
    Running  { checkpoint: Checkpoint },
    Terminal { outcome: TaskOutcome },
}

pub enum TaskOutcome {
    Completed { result: JsonValue },
    Failed    { error: String, result: Option<JsonValue> },
    Aborted   { reason: Option<String>, result: Option<JsonValue> },
    Orphaned  { reason: String },     // 崩溃后无法归属
    Faulted   { error: String },      // 内部错误
}
```

**这个模型直接治好当下的三个症状**：

| 症状 | 为什么好了 |
|---|---|
| 审批卡看不见 | 审批本身是一条 `EntryRecord{kind: Approval}`，提交即持久 → 重连时从 entry 日志**重建**，不依赖任何帧投递 |
| UI 还转圈 | 任务状态是 `TaskRecord` 的一部分，UI 读权威字段，不推断 |
| LLM 重复回复 | 变更线串行：同会话第二个提交**排队**，不是并发跑第二个 agent |

### 3.4 L2 任务运行时：Effect Sandwich + 崩溃恢复

工具执行按 Pico5 的三明治：

```
commit(intent:  ToolIntent { tool, args, task_id })    ← 持久：要跑这个工具了
       ↓（事务外，副作用）
执行工具
       ↓
commit(outcome: ToolOutcome { status, result })        ← 持久：结果是什么
```

**崩溃恢复协议（open 时，一个原子提交内完成）**：

```
扫 tasks where status = 'running'
  ├─ 有 intent 无 outcome → 效果可能已发生
  │    ├─ 工具声明幂等 → 按幂等语义重放
  │    └─ 非幂等 → 标记 needs_review，交用户裁决（不自动重放）
  ├─ 可迁移 → 降级 Pending，保留 checkpoint
  └─ 未知/不可迁移 → Orphaned 终态（含通知条目）
```

这消灭现在靠 `ws.rs:439-457` zombie takeover + `ws_grace.rs` 兜的孤儿问题。

### 3.5 L3 `hermix-protocol`：信封 + 订阅

对齐 `pi-protocol` 的**结构**（CBOR → JSON，见选型偏离）：

```rust
pub struct Envelope {
    pub kind: EventKind,
    pub route: Route,        // Server{server_id} | Session{server_id, session_id, attachment_id}
    pub seq: u64,            // 严格递增，允许间隙（客户端只做大小比较）
    pub payload: JsonValue,  // 语义由 kind 决定
}

pub struct Route {
    pub server_id: String,
    pub session_id: Option<SessionId>,
    pub attachment_id: Option<AttachmentId>,   // 多端挂载的核心
}
```

**订阅模型**（替代现在的"帧 + 补发镜像"）：

```
客户端连接 → subscribe(route, since_seq)
           ← 先回快照：entries where seq > since_seq（cursor 分页）
           ← 之后推增量：{ seq, kind, payload }

重连 = 用最后收到的 seq 重新 subscribe（幂等）
```

`seq` 是**对账锚**：

- 客户端发现 seq 跳号 → 重新拉快照
- 服务端发现 `since_seq` 太旧（已被压缩）→ 回退到全量快照

→ `SESSION_BROADCASTS` / `completed_keys` / `pending_grace` 三个补丁机制**全部下线**。

### 3.6 L4 `hermix-server`：多展示端挂载模型

这是从 `pi-server` 抄来的、**恰好命中主人多端需求**的设计。PI 原文：

> `A Session may have multiple presentation attachments. Repeating attach from one connection is idempotent; every successful attachment has a server-generated attachmentId... Session requests carry { serverId, sessionId, attachmentId }, and the server rejects stale or mismatched routes. Losing a connection rejects its local responses but releases its attachment only after admitted service calls settle.`

Hermix 落地：

```
一个 Session ──┬── attachment A（桌面端 UI）
               ├── attachment B（手机端 UI）
               └── attachment C（另一台电脑 / WebUI）

每个 attachment 有 server 生成的 attachmentId
断开只释放自己的 attachment（且要等在途调用结算完）
Session 本体与任务不受影响
```

**这解决了当前架构的一个硬伤**：现在 session↔连接是"channel 生命周期内一对一"（`stdio_session.rs:122-129`），多端根本无从接入。改成 attachment 模型后，手机端接进来不会踢掉桌面端。

---

## 4. 重写后的状态流转（端到端）

```
① 用户在桌面端发消息
        ↓
② server 收到 → 路由到 Session（带 attachmentId）
        ↓
③ Session.commit：写入 user entry（持久，拿 seq）
        ↓ 提交成功
④ 通知所有 attachment：{ seq, kind: EntryCommitted }
        ↓
⑤ 任务运行时：commit(TaskRecord{pending→running})
        ↓
⑥ agent loop 跑（从 Session 派生上下文）
        ├─ 流式 token → 易失通道直推 attachment（纯展示，不落盘）
        └─ 工具调用 → Effect Sandwich（intent → 执行 → outcome）
        ↓
⑦ 完工：Session.commit(assistant entry) + commit(TaskRecord{Terminal})
        ↓
⑧ 通知所有 attachment（含 seq）

【手机端在 ⑦ 之前接进来】
        → subscribe(session, since_seq=0) → 拿到全量快照 → 立刻看到同一会话的完整状态
```

**关键**：任何时刻有客户端接进来，都能从已提交状态复原出**同一个视图**。这是"稳定"的定义。

---

## 5. 网络安全层（PI 没有，全新设计）

主人需求原文：**"外部网络安全也先做进去，因为后面我们要做手机端与桌面端的连接。"**

### 5.1 现状：门没锁

`crates/hermix-server/src/security.rs`（343 行）现在只有：

- 可选静态 Bearer API key（环境变量）
- IP 白名单（环境变量）
- 每 IP 内存限流
- policy admin token

**缺失且对手机端致命的部分**：

| 缺口 | 后果 |
|---|---|
| 无 TLS | 明文传输，局域网内可被嗅探/篡改 |
| 无设备身份 | 所有客户端共用一个静态 token，无法单独撤销 |
| 无配对流程 | 只能手工抄写密钥 |
| 无短期凭证 | token 永久有效，泄漏即永久失守 |
| **无来源区分** | 远程请求与本地请求在代码里**无差别对待** → 手机端可执行本地命令 |
| 无失败锁定 | 限流是每分钟计数，可无限慢速爆破 |

### 5.2 威胁模型

| 威胁 | 场景 | 对策 |
|---|---|---|
| T1 未授权访问 | 同局域网攻击者扫描到端口 | 设备配对 + 强制认证，默认不监听 |
| T2 中间人 | 局域网 ARP 欺骗 | TLS + 证书指纹固定（配对时通过二维码带外传递） |
| T3 凭证泄漏 | 手机丢失 / token 被窃 | 短期 access token + 设备可撤销 + 设备级密钥 |
| T4 重放 | 抓包重发请求 | 请求带 nonce + 时间窗 + 单调计数器 |
| T5 远程越权执行 | 手机端遥控桌面跑 `rm -rf` | **来源感知权限**：远程来源高危工具强制桌面端确认 |
| T6 暴力破解 | 反复试配对码 | 配对码 TTL 120s + 一次性 + 失败指数退避 + 锁定 |
| T7 资源耗尽 | 大量连接/超大请求 | 连接数上限 + 请求大小上限（参考 PI 的 16MiB/1M/64 层）+ 空闲超时 |
| T8 提权 | 手机端伪造为本地来源 | 来源由**服务端**根据连接属性判定，客户端无法声明 |

### 5.3 设计：五层防护

#### 第 1 层：网络暴露面默认关闭

```
默认：仅监听 127.0.0.1（手机端连不上）
用户显式开启："允许设备连接"
  ├─ 仅本机        （现状，最安全）
  ├─ 局域网        （mDNS 广播，方便发现）
  └─ 自定义端口转发 （高级用户）
```

**默认关闭是硬要求**：桌面单机用户不该因为一个自己不知道的开关而暴露。

#### 第 2 层：传输加密（TLS 1.3）

```
选型：rustls + rcgen（自签证书）
证书指纹在配对时通过二维码带外传递 → 证书固定（pinning），免装根证书

不选的方案与理由：
  ❌ 公网 CA 证书      —— 局域网 IP 无法签发，且要联网
  ❌ 让用户装根证书     —— 手机端体验灾难
  ❌ Noise Protocol   —— 更轻但要自己实现握手状态机，rustls 更成熟
  ❌ 明文 + 应用层加密  —— 元数据（谁连谁、连多久）仍泄漏
```

**指纹固定是关键**：配对二维码里带 `cert_fingerprint`，手机首次连接时校验；不匹配即拒绝 → 中间人无法伪装。

#### 第 3 层：设备配对（防未授权接入）

```
桌面端：用户点"添加设备"
   ↓
生成一次性配对会话（TTL 120s，一次性）
   ↓
显示二维码，内容：
   { server_id, host, port, cert_fingerprint, pairing_token, proto_version }
   ↓
手机扫码
   ↓
① 校验证书指纹（防中间人）
② 用 pairing_token 完成配对握手
③ 手机生成设备密钥对，公钥上送
   ↓
桌面端签发设备凭证：{ device_id, device_name, device_pubkey, issued_at }
   ↓
手机保存 device_secret 到 OS keychain（iOS Keychain / Android Keystore）
```

配对是一次性、带外（二维码即带外信道）的信任建立。之后所有连接走凭证。

#### 第 4 层：凭证层次与请求签名

```
长寿命（配对时签发）
  device_secret  ── 存手机 keychain，永不过网
        ↓ 换取（首次连接时）
短寿命（TTL 1h，可刷新）
  access_token   ── 绑定 { server_id, device_id, attachment_id, scopes }
        ↓ 用于
每个请求
  Bearer <access_token> + 请求签名（device_secret 签 nonce+时间戳）
```

**为什么要请求签名**：access_token 在 TLS 内传输，理论上安全；但签名让"token 泄漏"不等于"可无限使用"——签名含时间窗 + 单调计数器，重放无效（对策 T4）。

**服务端维护设备注册表**（持久）：

```sql
CREATE TABLE paired_devices (
  device_id      TEXT PRIMARY KEY,
  device_name    TEXT NOT NULL,
  device_pubkey  BLOB NOT NULL,
  scopes         TEXT NOT NULL,      -- JSON 数组
  issued_at      INTEGER NOT NULL,
  last_seen_at   INTEGER,
  revoked_at     INTEGER             -- 非空即已撤销
);
```

用户可以随时在桌面端"设备管理"里撤销任意设备 → 立即断连 + 该设备凭证失效。

#### 第 5 层：来源感知权限（**最重要的一层**）

这是"手机端能做什么"的答案，也是唯一能防 T5 的机制。

```rust
pub enum Origin {
    Local,           // 桌面端自身发起的请求（用户直接操作）
    PairedDevice { device_id: DeviceId, scopes: Scopes },
    Unpaired,        // 只能访问 /health 和配对端点
}
```

**来源由服务端根据连接属性判定，客户端无法声明**（对策 T8）。

权限矩阵（默认值，桌面端可调）：

| 能力 | Local | PairedDevice（默认 scopes） |
|---|---|---|
| 发消息 / 看会话 | ✅ | ✅ |
| 审批工具调用 | ✅ | ⚠️ 可配置（默认允许看、不允许"always allow"） |
| 执行 shell / terminal | ✅ | ❌ **强制桌面端确认** |
| 删除文件 | ✅ | ❌ **强制桌面端确认** |
| 修改配置 / 模型 | ✅ | ❌ |
| 设备管理（配对/撤销） | ✅ | ❌ |
| 访问工作台 / 自定义 App | ✅ | ❌（默认） |

**远程来源下，高危工具的审批策略强制收紧**：

- `Origin::PairedDevice` 发起的 turn，其工具调用打标 `origin = remote`
- 高危工具（terminal / 删除类 / 系统级）在 `origin = remote` 下：
  - **不能**被 `session` 级"本次会话总是允许"覆盖
  - **必须**在桌面端（`Origin::Local`）由用户人工确认
- 即：手机可以指挥助手做事，但不能借助手的手在桌面上执行危险操作

**这条与已完成的一轮审批收窄（`engine.rs` 的 CONFIRM_PATTERNS）并不冲突**：那轮解决的是"本地操作弹太多卡"，这一层解决的是"远程操作必须弹卡"。两者是不同来源的不同策略。

#### 附：防滥用（全局）

| 措施 | 参数 |
|---|---|
| 认证失败退避 | 指数退避，5 次失败锁定该设备/IP 15 分钟 |
| 连接数上限 | 每设备 4 条，全局 64 条 |
| 请求大小上限 | 16 MiB payload（对齐 PI 默认）；数组/对象元素 1,000,000；嵌套 64 层 |
| 空闲超时 | 无活动 5 分钟断开（可配） |
| 审计日志 | 配对/认证/授权/撤销 全部落盘，含时间、device_id、来源 IP、决策 |

### 5.4 明确不做（防止安全模块膨胀）

- ❌ 不做用户账号体系（这是个人设备互联，不是多租户 SaaS）
- ❌ 不做公网中继服务器（超出本期；局域网 + 用户自建端口转发足够）
- ❌ 不做 OAuth/OIDC 集成（无第三方身份提供者）
- ❌ 不做端到端加密（服务端要解密才能执行工具，E2EE 在此无意义）
- ❌ 不做手机端 App 本身（本期只做**服务端侧的网络安全基础设施**）

---

## 6. 前端零大动的兼容策略

主人硬约束：**前端不要大动**。

### 6.1 冻结的契约（不许破）

| 契约 | 位置 | 内容 |
|---|---|---|
| 出站帧 | `hermix-transport/src/lib.rs:347` | `StreamEvent` 35 个 discriminator（`text`/`thinking`/`tool_start`/`request_approval`/`done`/…） |
| 入站帧 | `hermix-transport/src/lib.rs:855` | `WsClientMessage` 5 个（`send`/`approval_response`/`approval_responses_batch`/`clarify_response`/`ping`） |
| 信封 | `WsEnvelope` | `{version, request_id, trace_id, event}` |
| REST | `rest.ts` | `/v1/*` 与 `/api/*` 全部端点路径 + 响应形状 |
| 会话历史 | `GET /v1/sessions/{id}/messages` | `{ messages, context_usage }` |
| 审批补偿 | `GET /v1/sessions/{id}/approvals/pending` | `{ approvals: [{request_id, tool, arguments, expires_at}] }` |

### 6.2 适配层设计

新底座内部用 `Envelope{kind, route, seq, payload}`，**在服务端边界投影成前端认识的 `StreamEvent` 序列**：

```
内部：commit(EntryRecord{kind: Assistant, ...}) → 通知 { seq: 42, kind: EntryCommitted }
                                                          ↓ 边界适配（服务端）
前端：收到 {"type":"text","delta":"..."} / {"type":"done", ...}    ← 形状完全不变
```

**seq 对账对新前端是增量能力**：适配层在每帧里附带 `seq`（放在现有 `WsEnvelope` 的可选字段里 → 老前端忽略不报错）。

### 6.3 前端改动上限（明确量化）

| 允许改 | 上限 |
|---|---|
| `api/rest.ts` 新增函数调用 | +80 行 |
| `hooks/useStreamChat.ts` 接纳 seq 对账 + 重连改 subscribe | +120 行 |
| `hooks/useStreamParser.ts` 读取可选的 seq 字段 | +20 行 |
| **组件（任何 `.tsx` UI 文件）** | **0 行** |
| **视觉 / 布局 / 样式** | **0 行** |
| **总计** | **≤ 300 行，且 0 个组件改动** |

超过这个上限，说明适配层设计有问题，要回头改适配层而不是改前端。

### 6.4 手机端（后续，不在本期）

本期交付的"网络安全 + attachment 模型"是手机端的地基。手机端 App 需要：一个新 UI（独立项目）、复用本期的协议与安全层。**本期不写手机端。**

---

## 7. 执行阶段

重写版的分期。**每期独立可交付、可验证、可回退。**

| 期 | 内容 | 验收 |
|---|---|---|
| **P0 砍除与收敛** | 砍 `gateway`（27451）/ `acp`（2276）/ stubs 大部分 / consciousness / researcher；调度三套合一；传输两套合一 | `cargo check --workspace` 绿 + 测试全绿 + **净减 ≥ 3 万行** |
| **P1 存储层** | 新建 `hermix-store`：`trait Storage` + `SqliteStore`（连接池）+ `MemoryStore`；41 处裸连接归零 | grep 裸 `Connection::open` = 0；单测：并发提交无交错、失败无部分写入 |
| **P2 会话内核** | 新建 `hermix-session`：Session + 变更线 + commit 协议 + Entry 模型；消息读写切换 | 八条不变量逐条有测试；注入存储失败 → 状态不变 |
| **P3 任务运行时** | TaskRecord 持久状态机 + Effect Sandwich + 崩溃恢复 | 注入 kill -9 → 重启后状态可判定、无孤儿、无重复执行 |
| **P4 协议层** | `hermix-protocol` 信封 + seq + 订阅；旧补发机制（`SESSION_BROADCASTS`/`completed_keys`/`pending_grace`）下线 | 断线重连 100 次：UI 状态 == 后端快照，无丢帧重帧 |
| **P5 服务端重写** | `hermix-server` 会话/传输/状态部分重写；attachment 多端模型 | 两端（桌面 + 模拟第二端）同时挂载同一 Session，互不干扰 |
| **P6 网络安全** | TLS + 配对 + 凭证 + 来源分级 + 防滥用 + 审计（§5 全部） | 安全测试：中间人拒绝、重放拒绝、未配对拒绝、远程高危工具强制本地确认 |
| **P7 前端适配** | seq 对账（≤300 行，0 组件改动） | `tsc` 绿；UI 行为与现在一致 |
| **P8 稳定性验证** | 8h soak + 故障注入 + crash dump | SLO：内存增长 ≤ 200MB、fd 不增长、task 数稳定、0 未处理 panic |

**P0 可以立刻做且零风险**（砍的全是死代码）。

---

## 8. 对抗式审查

| # | 失败模式 | 触发场景 | 应对 |
|---|---|---|---|
| F1 | 变更线成瓶颈 | 流式 delta 逐字提交 → 每字一个事务 | **流式 delta 走易失通道**（纯展示），落定后提交一条 entry。这是 I3 的**唯一例外**，必须写在例外清单里，不许扩大 |
| F2 | 大对象撑爆事务 | 一个 commit 塞 10MB 压缩产物 | 大对象走 sidecar，提交只写引用；单提交字节上限 + 超限拒绝 |
| F3 | 重写期新旧并存不一致 | P1-P7 期间新旧路径各写一份 | 迁移期**单一写入方向**（新写 → 旧读兼容），禁止双向 |
| F4 | 非幂等工具重放 | 崩在 intent 后 outcome 前 | 工具声明幂等性；非幂等 + 无 outcome → `needs_review`，不自动重放 |
| F5 | Session 毒化卡死用户 | 存储不确定失败 | 毒化只针对打开的实例（重开可恢复）；UI 提示"会话需重开"+ 一键重开 |
| F6 | TLS 指纹固定导致升级困难 | 重装/换机后证书变化 | 支持"重新配对"流程；指纹变更加显著确认 |
| F7 | 配对二维码被拍屏 | 肩窥 | 配对码 TTL 120s + 一次性 + 配对时桌面端显示设备信息待确认 |
| F8 | 远程权限过严导致手机端难用 | 每个操作都要桌面确认 | scopes 可调；默认给出"会话内可见 + 非高危可执行"的实用默认值 |
| F9 | 性能回归 | 每次读都从 entry 派生上下文 | 派生缓存 + 增量失效；entry 数超阈值强制压缩提交（对齐 Pico5 的 checkpoint 思路） |
| F10 | 重写引入新并发 bug | 变更线 + 现有 `Arc<Mutex>` 混用 | 迁移期禁止新旧锁嵌套；`clippy::await_holding_lock` 进 CI；每期并发压测 |
| F11 | 方案变成第二个许愿清单 | 范围膨胀 | **冻结在 P0-P8**；新增项必须替换其中一项 |

---

## 9. 需要主人拍板的决策点

| # | 决策 | 选项 | 建议 |
|---|---|---|---|
| **D1** | 重写范围 | A. §1.1 全部重写 + §1.2 重构 + §1.3 砍掉<br>B. 只做 §1.1 重写 + §1.3 砍掉（不动 agent loop）<br>C. 先只做 §1.3 砍除（P0），验证后再定 | **C → B → A 递进**。P0 零风险可立刻做；agent loop 重构风险最高，放最后决定 |
| **D2** | 是否接受"架构对齐而非逐行移植" | 是 / 否（要我逐行翻译 TS） | **是**。见声明 1，逐行翻译是自毁 |
| **D3** | 网络暴露面默认策略 | A. 默认关闭，用户显式开启<br>B. 默认局域网可见 | **A**。单机用户不该被默认暴露 |
| **D4** | 传输加密选型 | A. TLS 1.3 (rustls) + 自签 + 指纹固定<br>B. Noise 协议<br>C. 暂不加密 | **A**。手机端最兼容（WS over TLS），指纹固定免装根证书 |
| **D5** | 远程设备默认权限 | A. 只读（看会话，不能发消息）<br>B. 会话级（可发消息、看会话，高危工具需本地确认）<br>C. 完全等同本地 | **B**。A 太束手束脚，C 等于把桌面交给手机 |
| **D6** | 手机端 App 是否在本期 | A. 不在（本期只做服务端安全基础设施）<br>B. 在 | **A**。本期交付地基，手机端另立项目 |

---

## 10. 与 v1 的关系

| | v1（`STABLE_FOUNDATION_DESIGN.md`） | v2（本文） |
|---|---|---|
| 路线 | 绞杀者渐进迁移（F0-F6） | **底座推倒重写**（P0-P8） |
| 砍代码 | 不砍 | **砍 3.2 万行死重** |
| 网络安全 | 明确划为 out of scope | **纳入核心（§5）** |
| PI 对齐程度 | 借鉴 9 个机制 | **逐包映射，全面对齐** |
| 适用场景 | 只想止血 | 要重做地基、且要做多端 |

v1 的分析（症状归因、状态权威三方割裂、六条不变量）**在 v2 中全部继承且仍然有效**，只是执行变为重写。

---

## 11. 不做什么（Non-goals）

- ❌ 不逐行移植 TS 代码（见声明 1）
- ❌ 不抄 Chord（见声明 3）
- ❌ 不抄 CBOR（见选型偏离）
- ❌ 不重写业务外壳（tools / skills / cua / plugin 等 15 万行保留）
- ❌ 不改前端组件与视觉（≤300 行，0 组件）
- ❌ 不做手机端 App（本期只做服务端安全基础设施）
- ❌ 不做公网中继 / 用户账号体系 / OAuth
- ❌ 不做跨平台（Windows-only 是产品边界）
- ❌ 不追求"零易失"（流式展示是唯一例外，显式标注）
