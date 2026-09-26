# Hermix 稳定底座设计（参考 pi / Pico5）

> ⚠️ **已被 `docs/FOUNDATION_REWRITE_DESIGN.md`（v2）取代**。
> 主人 2026-09-24 决定改为"底座推倒重写 + 全面对齐 PI + 纳入网络安全"。
> 本文的**分析部分（症状归因 / 状态权威三方割裂 / 六条不变量 / 对抗式审查）在 v2 中全部继承且仍然有效**，
> 仅"绞杀者渐进迁移"的执行路线被重写路线取代。保留此文档作对照。
>
> 提案状态：**待主人评审**，未动一行代码。
> 参考项目：https://github.com/earendil-works/pi （`pi-agent-core` / `pi-durable` / `pi-protocol`）
> 参考文档：`packages/durable/docs/pico-v5.md`（规范设计文档）
> 诊断依据：本仓库代码实读 + 子 agent 全量架构探查（证据均带 `文件:行号`）

---

## 0. 结论摘要

**现在不稳定的根因不是"某个 bug 没修"，而是缺一层东西：状态变更没有唯一权威。**

pi 的全部稳定性都建立在一句设计原则之上：

> **Session 原子地提交不可变条目、完整任务记录与文档。只有已提交的状态才可观测。**
> ——`pico-v5.md`，硬性不变量 #1 / #3

Hermix 现在是反过来的：**状态先发到传输层（WS/stdio 帧），落盘是后置副作用**。所以断线 = 状态丢，崩溃 = 状态丢，重连 = 靠补发/重跑猜。凡是"卡住/丢失/对不上"的症状，最后都追到这条。

三个可直接验证的物理证据：

| 证据 | 位置 | 说明 |
|---|---|---|
| 15 个 `Arc<Mutex<…>>` 内存状态容器 | `crates/hermix-server/src/state.rs:22-152` | `active_sessions` / `active_handles` / `pending_grace` / `completed_keys` / `approval_registry` / `session_bypass`… 全部重启即失忆 |
| 事件流被当状态载体 | `crates/hermix-server/src/ws.rs:28` | `SESSION_BROADCASTS` 是进程内 `OnceLock` 静态 map，注释自承"events emitted BEFORE a subscriber attaches are NOT replayed" |
| 非事务双写 | `ws.rs:872`（写 DB）vs `:2068`（resolve 内存 oneshot） | 审批的内存 registry 与 DB `role='approval'` 行**各写各的**，中间任何一步失败即分叉 |

而 `docs/STABILITY_ENGINEERING_PLAN.md`（2026-07-25 起草）**零执行**：无连接池、41 处裸 `Connection::open` 原封不动、0 soak test。那份文档的价值在于它准确列出了病灶，但它治的是"症"（加锁、加超时、加 lint），没治"因"（状态权威缺失）。

本方案要做的是补上缺失的那一层，而不是再加一轮补丁。

---

## 1. 诊断：症状 → 根因 → 证据

### 1.1 症状归因表

| 你看到的现象 | 直接原因 | 架构根因 |
|---|---|---|
| 写入文件后半天转圈 | agent loop 停在 `AwaitApproval`，UI 靠事件流判断"还在跑" | 任务运行态只在内存，且 UI 用事件流推状态 |
| 审批卡看不见但 agent 在等 | `task_in_flight` 的 `Done` 关掉旧连接后，在途审批帧没重投 | 审批只有"帧"这一个载体，没有可重建的权威 |
| agent 已停止，UI 还显示"处理中" | 前端 `streamingSessionId` 与后端任务状态无对账机制 | 状态权威三分（内存 / DB / 前端），无一致性协议 |
| LLM 重复回复 | dedup key 是 `dedup_key(session, text)` 哈希（`ws.rs:522`） | **同会话不同文本可并发**，会话级串行语义缺失 |
| 偶发随机错误 / fd 抖动 | 41 处裸 `rusqlite::Connection::open`，三方共写同一 `sessions.db` | 无统一持久化抽象，无连接池，无事务边界 |
| 重启后行为不一致 | 内存 registry 全丢，靠启动时 `supersede_pending_approvals`（`state.rs:465`）批量清 | 内存是权威，DB 是副本，重启即分叉 |

### 1.2 状态权威的现状（三方割裂）

```
① 内存权威（进程内，易失）
   HttpServerState { active_sessions, active_handles, approval_registry,
                     pending_grace, completed_keys, session_bypass, ... }
   ↑ 运行时真相

② 持久权威（SQLite sessions.db）
   sessions / messages / tasks / automations / token_usage
   ↑ 事后记账，非实时同步；且被 3 个 crate 共写
       (session_persistence / hermix-task / hermix-automation)

③ 展示权威（前端 React state）
   sessions[] / streamPhase / streamErrors / runningToolCount
   ↑ 从事件流增量推导 + DB 快照叠加（两套来源混合）
```

三者之间**没有一致性协议**。①写②靠"agent 结束时调 `persist_session_with_compression`"（`local_agent_service.rs:2810`），①推③靠 WS 帧。任何一段断裂，三方就分叉，且没有"谁是对的"的仲裁机制。

### 1.3 依赖图（唯一的好消息）

子 agent 探查确认：**无循环依赖**。`server → agent → tools → core` 单向。这意味着加一层不会触发重构雪崩——底座是可以"插"进去的。

### 1.4 上帝文件（复杂度热点）

| 文件 | 行数 | 问题 |
|---|---|---|
| `crates/hermix-agent/src/local_agent_service.rs` | 4364 | agent 服务 + 审批判定 + 持久化调用的混合体 |
| `apps/desktop-tauri/src/App.tsx` | 3340 | 状态归约 + 事件流处理 + UI 渲染全在一个组件 |
| `crates/hermix-server/src/ws.rs` | 3136 | 连接管理 + 任务调度 + 审批 + 广播 + 持久化 |
| `crates/hermix-agent/src/provider.rs` | 3202 | 多 provider 实现平铺 |
| `crates/hermix-server/src/routes.rs` | 1831 | 所有 REST 端点平铺 |

`ws.rs` 一个文件同时管 6 件事，是"状态权威分散"的必然结果——因为没有内核，所有东西只能在借用层就地堆。

---

## 2. Pi 的内核：可借鉴的 8 个机制

只列**对 Hermix 有直接价值**的，不抄无关的（Chord 的 CRDT / replicated state 桌面单机用不上）。

### M1. 单一变更线 + 原子提交（Mutation Line）
一个 Session 拥有**一条**变更线，所有提交串行化，持有至"存储结算 + 状态采纳"完成。
→ **解决**：会话级并发（当前 dedup key 错位）、非事务双写。

### M2. 只有已提交状态可观测（No Volatile Publish）
不存在"先发布后落盘"的路径。提交成功才发布通知；提交失败则**什么都不发布**。
→ **解决**：事件流当状态载体、断线丢状态。

### M3. 持久任务状态机（Task State Machine）
```
pending(checkpoint) → running(checkpoint) → terminal(completed|failed|aborted|orphaned|faulted)
```
任务本身是**持久记录**，不是内存里的 `JoinHandle`。
→ **解决**：孤儿任务、僵尸接管、崩溃无恢复。

### M4. Effect Sandwich（副作用三明治）
```
commit intent  →  执行外部副作用  →  commit outcome
```
外部副作用**不放在状态事务内**，但两侧都有持久记录，重开时可判定"效果可能已发生"。
→ **解决**：审批/工具执行与状态不一致（当前 `ws.rs:872` / `:2068` 双写）。

### M5. 崩溃恢复协议
open 时：存活的 `running` → 降级 `pending`（保留 checkpoint）；未知/不可迁移的 → `orphaned` 终态。全部在**一个原子提交**内完成。
→ **解决**：重启后状态是猜的。

### M6. 存储契约抽象（Storage Trait）
`Storage.commit / mintId / 精确查找 / 有序扫描`，后端可换（Memory / JSONL / SQLite）。SQLite 后端：**一个 SQL 事务 = 一个 commit**。
→ **解决**：41 处裸连接、三方共写、无事务包装。

### M7. 上下文派生（Entry 日志 → 模型上下文）
上下文是**从不可变 entry 日志派生的**，且 "UI 归约与模型上下文归约是独立消费者"。
→ **解决**：前端从事件流推 UI 状态、压缩逻辑与展示逻辑纠缠。

### M8. 访问驱动迁移 + 显式版本
```
stored == token → 直接用
stored <  token → migrate(value, version)
stored >  token → 拒绝访问
```
不扫描、不全量升级，访问时才迁移。
→ **解决**：schema 迁移与数据修复的被动性（当前 `schema.rs:241` 已有基础，但只管表结构）。

### M9. 明确 Non-goals
pi 把"不做什么"写进规范（不做 CRDT、不做自动检查点启发式、不做 session-scoped rewindable document…）。
→ **解决**：本方案防止变成第二个没人执行的许愿清单。

---

## 3. 目标架构

### 3.1 分层总览

```
┌─────────────────────────────────────────────────────────────┐
│ L5  前端投影层                                                │
│     apps/desktop-tauri/src/state/                            │
│     单一 reducer：已提交 entry[] → UI 状态                    │
│     ❌ 不再"事件流增量推导 + 快照叠加"两套                      │
├─────────────────────────────────────────────────────────────┤
│ L4  传输层（信封 + 订阅）                                      │
│     crates/hermix-transport/                                 │
│     统一信封 {kind, session, seq, payload}                    │
│     客户端订阅 session 的已提交变更流                           │
│     重连 = 拉快照(seq) + 订阅增量（天然幂等）                    │
├─────────────────────────────────────────────────────────────┤
│ L3  Agent Loop（复用现有 hermix-agent）                        │
│     - 从 Session 读取派生上下文                                │
│     - 所有写入走 Session.commit                               │
│     - 不再直接发帧当状态                                       │
├─────────────────────────────────────────────────────────────┤
│ L2  任务运行时（扩展 crates/hermix-runtime/）                   │
│     TaskRecord 持久状态机 + Effect Sandwich                   │
│     checkpoint / 崩溃恢复 / orphan 判定                       │
├─────────────────────────────────────────────────────────────┤
│ L1  会话内核（新 crate: hermix-session）  ★核心               │
│     Session { mutation_line, entries, tasks, docs }          │
│     commit 协议：prepare → storage → adopt/publish           │
├─────────────────────────────────────────────────────────────┤
│ L0  存储层（新 crate: hermix-store）                          │
│     trait Storage { commit, mint_id, find*, scan* }          │
│     SqliteStore（生产） / MemoryStore（测试）                  │
│     ★ 所有 SQLite 访问唯一入口                                 │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 六条硬性不变量（Hermix 版）

落进代码 + CI 守卫，不允许"这次先不算"：

| # | 不变量 | 验证方式 |
|---|---|---|
| **I1** | 一个 Session 的所有状态变更走**同一条变更线**，串行提交 | 并发测试：1000 次并发写同一 session 无交错 |
| **I2** | **只有已提交状态可观测**。提交前不发布任何通知 | 静态扫描：发布路径必须在 `storage.commit()` 返回之后 |
| **I3** | 外部副作用**不在**状态事务内，两侧有持久记录（Effect Sandwich） | 注入副作用失败：状态可判定、可恢复 |
| **I4** | 任务状态是**持久记录**，不是内存句柄 | `active_handles` 语义被 TaskRecord 取代 |
| **I5** | SQLite 访问**唯一入口**是 `hermix-store` | CI grep：`crates/**` 中裸 `Connection::open` 归零 |
| **I6** | 前端状态**只从已提交 entry 派生**，不从增量帧拼 | 断线重连测试：UI 状态 == 后端快照 |

### 3.3 L1 会话内核：提交协议（关键机制）

这是整个方案的心脏。伪代码：

```rust
// 一个 Session 独占一条变更线
pub struct Session {
    id: SessionId,
    line: MutationLine,          // 串行化所有提交
    store: Arc<dyn Storage>,
}

impl Session {
    /// 提交协议：任一阶段失败 → 什么都不发布
    pub async fn commit<F, T>(&self, f: F) -> Result<CommitResult<T>, CommitError>
    where F: FnOnce(&mut Draft) -> Result<T, DraftError>
    {
        let guard = self.line.acquire().await;        // 1. 持线（串行）
        let mut draft = Draft::new();                 // 2. 可撤销草稿
        let value = f(&mut draft)?;                   //    用户逻辑，失败即弃

        let writes = draft.prepare()?;                // 3. 冻结为不可变写入集
        let seq = self.store.commit(writes).await?;   // 4. 原子落盘（SQLite 事务）

        // 5. 只有到这里才采纳 + 发布
        self.adopt(value);
        drop(guard);                                  // 6. 释放线
        self.publish(seq);                            // 7. 监听器在线外运行
        Ok(CommitResult { seq, value })
    }
}
```

对照 pi 的术语：`prepare → storage settle → adopt → release line → notify listeners`。

**为什么这样能治好当下的病**：

- "审批卡丢了" → 审批**本身是一条 entry**，提交即持久，重连时从 entry 日志重建，不依赖任何帧投递。
- "UI 还转圈" → 任务状态是 entry 的一部分，UI 读权威值，不推断。
- "重复回复" → 变更线天然串行，同会话第二个 `commit` 排队而不是并发跑 agent。
- "非事务双写" → 内存态是 `adopt` 的产物，不是另一份真相源。

### 3.4 L2 任务运行时：状态机 + Effect Sandwich

```rust
pub enum TaskState {
    Pending   { checkpoint: Checkpoint },
    Running   { checkpoint: Checkpoint },
    Terminal  { outcome: TaskOutcome },
}

pub enum TaskOutcome {
    Completed { result: JsonValue },
    Failed    { error: String, result: Option<JsonValue> },
    Aborted   { reason: Option<String>, result: Option<JsonValue> },
    Orphaned  { reason: String },   // 崩溃后无法归属的任务
    Faulted   { error: String },    // 内部错误（毒化）
}
```

工具执行走三明治：

```
commit(intent: ToolIntent { tool, args, task_id })   ← 持久：我要跑这个工具了
        ↓  （事务外）
执行工具（可重入需幂等；不可重入需靠 intent 判定"可能已执行"）
        ↓
commit(outcome: ToolOutcome { status, result })      ← 持久：跑完了结果是什么
```

崩溃恢复（open 时）：

```
扫 tasks where status = 'running'
  ├─ 有 intent 无 outcome → 判定"效果可能已发生" → 标记 needs_review 或按工具幂等性重放
  ├─ 可迁移 → 降级 pending，保留 checkpoint
  └─ 未知/不可迁移 → orphaned（含通知条目，同一提交内完成）
```

这直接消灭"孤儿任务"和"僵尸 takeover"（现在靠 `ws.rs:439-457` 兜）。

### 3.5 L4 传输层：从"帧"到"订阅"

现状：`ws.rs` 手写 `biased select` + `SESSION_BROADCASTS` 补发镜像，注释自承不 replay 历史。

目标：

```
客户端连接 → subscribe(session_id, since_seq) 
           ← 服务端先回快照：entries where seq > since_seq（分页）
           ← 之后推送增量：{ seq, kind, payload }

重连 = 用最后收到的 seq 重新 subscribe（幂等，不重复不丢失）
```

帧格式统一信封（借鉴 `pi-protocol` 的信封/载荷分离，不抄 CBOR）：

```rust
struct Envelope {
    kind: EventKind,        // EntryCommitted / TaskStateChanged / ApprovalPending / Error
    session: SessionId,
    seq: u64,               // 单调递增，客户端用于对账
    payload: JsonValue,     // 语义由 kind 决定
}
```

`seq` 是"对账锚"：客户端发现 `seq` 跳号 → 重新拉快照；服务端发现客户端 `since_seq` 太旧 → 回退到快照。不再需要 `completed_keys` / `pending_grace` / `SESSION_BROADCASTS` 这些补丁。

### 3.6 L5 前端：单一投影

现状：`App.tsx` 3340 行，状态从两套来源混合推导（`useStreamParser` 事件增量 + `getSessionMessages` 快照）。

目标：

```ts
// 唯一归约入口
function reduce(state: UiState, event: CommittedEvent): UiState
// state 从权威快照初始化，之后只由已提交事件驱动
```

判定标准变成**读字段**而不是**猜流程**：
- "任务在跑" = `taskState.status === 'running'`（不是 `streamingSessionId === activeSessionId`）
- "任务结束" = 收到 `TaskStateChanged{terminal}`（不是"我等到 done 帧了"）
- "有卡可点" = `entries` 里有 pending approval（不是"我收没收到那一帧"）

---

## 4. 迁移路径（绞杀者模式，不推倒重来）

原则：**新旧并存，逐条路径切换，每一步都可回退**。不搞大爆炸重写。

### F0 —— 收口写入 + 立不变量（不改行为）
- 建 `crates/hermix-store`：`trait Storage` + `SqliteStore`（WAL + 连接池，兑现 P0-1）
- 把 41 处裸 `Connection::open` 迁到 store 入口（纯搬家，行为不变）
- 三方共写 `sessions.db` 收敛到 store 内部
- 加 CI 守卫：裸连接归零、发布路径静态检查
- **验收**：`cargo check --workspace` 绿 + grep 归零 + 现有 1834 单测全绿（行为未变）

### F1 —— 会话内核上线（消息读写先切）
- 建 `crates/hermix-session`：`Session` + `Draft` + commit 协议
- Entry 模型（user / assistant / tool / system / approval 统一为 entry）
- 消息读路径切到 Session（`getSessionMessages` 变成"读 entry 派生"）
- 变更线接管会话级串行（修掉 dedup key 错位）
- **验收**：并发写同一 session 无交错；消息往返一致；旧路径未删

### F2 —— 任务持久状态机 + 崩溃恢复
- `TaskRecord` 落 `tasks` 表（表已存在，`schema.rs:547`），扩展状态机字段
- Agent loop 的 spawn 从"登记 JoinHandle"改为"提交 TaskRecord(pending→running)"
- 工具执行走 Effect Sandwich
- 实现 open 时恢复协议（running → pending / orphaned）
- **验收**：注入 kill -9，重启后任务状态可判定、无孤儿、无重复执行

### F3 —— 审批改造成 entry
- 审批请求 = 提交一条 `approval` entry（持久即真相）
- 用户决策 = 提交 `approval_decision` entry（Effect Sandwich 收尾）
- 删除 `ApprovalRegistry` 内存 oneshot 作为**权威**（可保留为唤醒通道）
- 前端的 `getPendingApprovals` 兜底逻辑下线（不再需要）
- **验收**：断线任意时刻重连，审批卡必然出现在 UI；不存在"点不到"状态

### F4 —— 传输层信封 + 订阅式投影
- 统一 Envelope + `seq` 对账
- `SESSION_BROADCASTS` / `completed_keys` / `pending_grace` 下线
- 前端重连改为 `subscribe(since_seq)`
- **验收**：断线重连 100 次，UI 状态 == 后端快照；无丢帧无重帧

### F5 —— 前端单 reducer + 清理
- `App.tsx` 拆解，状态归约独立
- 删除旧路径（`useStreamChat` 的本地 settle 推断、`useStreamParser` 的状态推导职责）
- **验收**：`tsc` 绿；`App.tsx` 显著瘦身；所有状态判定来自字段

### F6 —— 稳定性验证（兑现 P2）
- 8h soak test（SLO：内存增长 ≤ 200MB、fd 不增长、task 数稳定）
- fault injection：LLM 503 / tool panic / WS 断连 / 存储失败
- crash dump

**每阶段独立可交付、可回退**，不存在"做到一半停了就全废"的风险。

---

## 5. 对抗式审查：这套设计在什么情况下会崩

按 AGENTS.md 的对抗式审查要求，逐条列出失败模式与应对。**这一节不通过，方案就不算完整。**

| # | 失败模式 | 触发场景 | 应对 |
|---|---|---|---|
| F1 | 变更线成为瓶颈 | 单 session 高频小提交（流式 delta 逐字提交）→ 吞吐崩 | **流式 delta 不逐字提交**：流式内容走易失通道（纯展示），**落定后**提交一条 assistant entry。这是唯一的"易失例外"，且例外必须显式标注 |
| F2 | 提交粒度过大 | 一个 commit 里塞 10MB 压缩产物 → SQLite 事务长事务锁库 | 大对象走 sidecar（借鉴 JSONL sidecar 思路），提交只写引用；单提交字节数上限 + 超限拒绝 |
| F3 | 迁移期双写不一致 | F1-F4 期间新旧路径并存，两条路各写一份 | 迁移期**单一写入方向**（新写入 → 旧读取兼容），禁止双向；每阶段有"旧路径是否只读"的显式检查 |
| F4 | Effect Sandwich 重放非幂等工具 | 崩在 intent 之后 outcome 之前，恢复时重跑 shell 命令 | 工具声明幂等性；非幂等且 intent 无 outcome → 标记 `needs_review` 交用户裁决，**不自动重放** |
| F5 | seq 对账被前端误用 | 前端把 seq 当"必须连续"，服务端批量提交跳号 → 误判丢帧 | seq 语义定为**严格递增但允许间隙**（pi 同款），前端只做"比大小"不做"连续性断言" |
| F6 | Session 毒化后无法恢复 | 存储不确定失败 → Session poisoned，用户会话卡死 | 毒化只针对**打开的 Session 实例**（重开可恢复）；持久层本身不毒化；UI 明确提示"会话需重开"并提供一键重开 |
| F7 | 迁移引入新并发 bug | 变更线 + 现有 `Arc<Mutex>` 混用 → 锁序倒置 | 迁移期禁止新旧锁嵌套；`clippy::await_holding_lock` 进 CI；每个阶段跑并发压测 |
| F8 | 性能回归 | 每次读都从 entry 派生上下文（长会话派生成本 O(n)） | 派生结果缓存 + 增量失效；上限保护：entry 数超阈值强制压缩提交（对齐 pi 的 checkpoint 思路） |
| F9 | 方案本身变成第二个许愿清单 | 范围膨胀，F5 之后还有 F6' F7'…… | 本方案**冻结在 F0-F6**；新增项必须替换掉其中一项，不允许追加 |

---

## 6. 不做什么（Non-goals）

明确划掉，防止范围膨胀：

- ❌ **不推倒重写**。新内核是插入式的，现有 265K 行 Rust 的绝大部分（工具后端、provider、skills、focus、cua、gateway）不动。
- ❌ **不抄 Chord 的 replicated state / CRDT**。桌面单机单写者，多写者合并是纯负担。
- ❌ **不抄 CBOR 协议**。JSON 信封够用，换编码只换来调试痛苦。
- ❌ **不改前端设计系统 / UI 结构**。本方案只管状态建模，不碰视觉。
- ❌ **不做跨平台**。Windows-only 是产品边界（AGENTS.md 铁律）。
- ❌ **不做外部网络安全**（DDoS/TLS/鉴权）——另一个范围。
- ❌ **不在本方案里删审批**。审批频率是独立议题（已在 `engine.rs` 完成一轮收窄），本方案只保证审批**不丢、可达、可重建**。
- ❌ **不追求"零易失"**。唯一的例外是流式展示 delta（见 F1），且必须显式标注、有界。

---

## 7. 需要主人拍板的决策点

| # | 决策 | 选项 | 我的建议 |
|---|---|---|---|
| D1 | 是否启动这次底座改造 | A. 全量做 F0-F6 ／ B. 只做 F0-F2（收口写入+内核+任务状态机）／ C. 不做，继续打补丁 | **B**。F0-F2 解决你实际遇到的 80% 症状（卡住/孤儿/重复），F3-F5 是长期收益，可后续再决定 |
| D2 | 会话内核的持久表示 | A. 事件溯源（entry 日志派生一切）／ B. 快照 + 增量（当前路线的加固版） | **A**。B 就是在现有架构上再加补丁，治不了根因 |
| D3 | 流式 delta 的定位 | A. 易失（例外，需显式标注）／ B. 也持久化 | **A**。逐字落盘是性能自杀；但例外必须写进不变量文档，不许偷偷扩大 |
| D4 | `hermix-store` 的库选型 | A. `rusqlite` + `deadpool-sqlite` ／ B. `sqlx` ／ C. 自己写连接池 | **A**。改动最小，同步 API 与现有代码同构，`spawn_blocking` 包裹即可 |
| D5 | 迁移期并行多久 | A. 每阶段完成即删旧路径 ／ B. 保留到 F6 统一清理 | **A**。双路径存留越久，"双写不一致"风险越大（见 F3） |

---

## 8. 附：与既有计划的关系

- `docs/STABILITY_ENGINEERING_PLAN.md`（2026-07-25）：**病灶清单有效，方案无效**（零执行证明"加锁+加超时+加 lint"治不了根因）。本方案吸收它的 P0-1/P0-2/P1-1/P1-5/P2-2 作为 F0/F2/F6 的具体子项，其余降级为可选。
- 本方案与它的根本差别：**它是"在现有结构上加约束"，本方案是"补上缺失的结构层"**。约束会过期，结构不会。
