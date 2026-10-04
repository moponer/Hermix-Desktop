# Decision Note: 自动化调度幂等（run 状态机 + 退避重试）

Status: proposed — 2026-10-04 逐行核实 `scheduler.rs` / `automation_trigger.rs` / `repository.rs` 后的设计。**只写方案，未改任何代码。** 核实发现的是**一个确定的重复触发竞态**与**一处状态机缺口**，不是提案标题里笼统的"幂等"。

Archived: 2026-10-04

本篇被 [调度模型升级](../../../proposed/feature/2026-10-04-automation-schedule-model-upgrade.md) 取代。后者的 runId 契约 + 派发状态机是本篇的完备形态（覆盖时钟跳变，本篇只解决并发竞态）。本篇独有的五项实施要点已全部移植进那篇的「实施要点」节。**本篇冻结，不再编辑，不作为现行依据。**
## 核实结论：现有代码有一个确定的竞态

### 事实 1：`trigger()` 返回时数据库还是空的

`automation_trigger.rs:95-105` 的 trigger 回调：

```rust
Arc::new(move |auto: Automation| {
    tokio::spawn(async move {
        execute_automation_run(&auto, &svc, &agent, &bus, tasks.as_deref()).await;
    });
})
```

而 `record_run_start`（唯一会推进运行态的动作）在 `execute_automation_run` **第一行**（`automation_trigger.rs:120`），也就是在那个 `tokio::spawn` **里面**。

时间线：

```
t=0.0s  tick_once() 判定 should_fire = true
t=0.0s  trigger(auto) → tokio::spawn(...) → 立刻返回
        ← 此时 automation_runs 表里没有任何 'running' 行
        ← 此时 last_run_at 仍是旧值
t=0.1s  spawned task 才开始跑，record_run_start 落库
t=30s   下一次 tick
```

**若 spawned task 在 t=0.1s 之前没被调度到**（runtime 繁忙、进程刚启动、tick 与 spawn 竞态），t=30s 的 `should_fire` 仍为 true → **同一个自动化被触发两次**。

`should_fire` 的判据（`scheduler.rs:102-118`）是 `state.last_run_at < next_run_at`，而 `last_run_at` 只在 `update_after_run`（`service.rs:186`）里写，也就是**整个 run 跑完才写**。

### 事实 2：手动触发与调度触发无互斥

`trigger_automation_manually`（`automation_trigger.rs:286`）直接 `await execute_automation_run(...)`，与 scheduler 完全无协调。用户点"立即运行"的同时调度器也在 tick → 同一个自动化两个 run 并发跑。

两者用的是**同一个 `session_id`**（`automation:{}`），`agent_service.send_message` 打到同一个会话 → 并发写同一会话历史。

### 事实 3：`automation_runs.status` 是自由文本，无约束

`repository.rs:102` 的 `status TEXT NOT NULL`，无 `CHECK` 约束。现有代码只写入 `"running"` / `"success"` / `"failed"`（`automation_trigger.rs:201/210`），但没有任何机制阻止拼错的字符串进来。

`AutomationRun.status` 在 `automation_trigger.rs:418` 被断言为 `== "success"`——**断言的是字面量，不是枚举**。加状态时容易漏改这类断言。

### 事实 4：零重试机制

`grep -rn "retry|backoff|attempt|max_attempt" crates/hermix-automation/` → **无结果**。run 失败即终态，不重试。

这未必是缺陷（自动化重复执行可能比失败更糟），**但当前用户无法表达"这个任务失败请再试"**。`Once` 类型失败后 `last_run_at` 被写、`next_run_at` 变 `None` → 永久不再触发，**用户只能手动重建**。

## Decision

### 修 1：调度器在触发前预占（消除竞态）

**方案**：`tick_once` 在 `trigger()` **之前**同步调用 `record_run_start`，把 run_id 作为参数传给 trigger。

改动：`TriggerFn` 签名从 `Fn(Automation)` 改为 `Fn(Automation, String)`（`run_id`）。`execute_automation_run` 接收外部传入的 run_id，不再自己生成。

**为什么这样最优**：
- `record_run_start` 是 SQLite 同步写（`rusqlite` 阻塞调用，在 async 上下文里但很快）——放在 `tick_once` 里能保证**返回前已落库**。
- 竞态窗口从"一个 tick 周期"缩到"一次同步 SQLite 写"。
- run 创建与调度决策变成同一个原子决策点。

**残留风险**：若 `record_run_start` 成功但 `trigger` 因进程崩溃未执行 → 留下一条永久 `running` 的孤儿记录。**修 2 顺带解决**（重启时收割孤儿）。

**明确不做**：不加进程内互斥锁（`Mutex<HashSet<id>>`）。理由——锁只护得住单进程，重启即失效，而 SQLite 唯一索引是跨进程的真约束。**六步阶梯第 3 条：能用原生 DB 约束就别写应用代码。**

### 修 2：run 状态机 + 启动时收割孤儿

**状态机**（`automation_runs.status`）：

```
running ──┬──> success     (终态)
          │
          ├──> failed      (终态，可重试 → 回到 pending)
          │
          └──> abandoned   (终态，进程死时被收割)
```

**`abandoned` 的产生**：进程启动时执行一次

```sql
UPDATE automation_runs
SET status = 'abandoned',
    finished_at = <now>,
    error_text = 'process restarted while running'
WHERE status = 'running';
```

**为什么这条必须有**：`running` 且 `finished_at IS NULL` 的行跨越了一次进程重启，意味着那个 run 永远不会再写结果——不收割的话 `count_active_runs()`（Task 1 刚加的保活信号）会**永久返回非零**，机器永远不休眠。

这不是假想——Task 1 的 `active-count` 端点上线后，这个 bug 会直接表现为"合上笔记本它不睡"。

**`status` 加 `CHECK` 约束**：新建表时加，但**存量表要迁移**（`ALTER TABLE` 无法加 CHECK 到已有表，需重建）。**决策：暂不加 CHECK**，改为在 Rust 侧引入 `RunStatus` 枚举 + `as_str()`/`parse()` 双向转换，让 `status` 在类型层就是闭集。理由——CHECK 约束要重建表，收益只是"挡住手滑"，而类型系统已经在编译期挡了。**六步阶梯第 5 条。**

**用 `status='running'` 做幂等键**：`record_run_start` 前先查"该 automation 有没有 `running` 的 run"，有就跳过本次触发。这同时挡住事实 1 和事实 2——**手动触发与调度触发共用这一个判据**。

注意：这条判据要放在 service 层（`try_begin_run`）而不是 repository 层，因为 scheduler 和 manual 都要走它。

### 修 3：失败退避重试（按失败性质分类，不按 Once/Recurring）

**本条与 [调度模型升级](../feature/2026-10-04-automation-schedule-model-upgrade.md) 的第 3 条曾有冲突**（那篇否决"agent 执行失败也自动重试"，理由是业务内容重试可能重复副作用）。**采纳那篇的分类标准**——按失败性质而非调度类型分类更准确，也与 ZCode 一致：

| 失败性质 | 判定 | 处理 |
|---|---|---|
| `transient` | 网络抖动、provider 503/超时、宿主忙、锁竞争 | 指数退避重试 `1min → 2min → 4min → 8min`（4 次封顶） |
| `permanent` | 配置非法、凭据缺失、prompt 为空、模型不存在 | 直接失败 + 原因写到任务卡，不重试 |

**为什么按性质而非 Once/Recurring 分**：`Once` 任务也可能遇到瞬时网络故障而不该丢；`Recurring` 任务也可能因配置非法而重试 4 次纯属浪费。**调度类型与失败性质正交**——那篇的判断是对的，本篇修正。

**关键区分：重试的是「派发」，不是「业务内容」。** ZCode 的 `dispatchAttempts` 重试的是"把任务送出去"这一步；agent 跑起来之后的失败是业务结果，重试它会重复副作用（发消息、下单）。

**所以 hermix 的分层是**：
- **派发失败**（spawn 失败、session 不可用、触发前置条件不满足）→ 退避重试
- **agent 执行失败**（`send_message` 返回 Err）→ 记 `failed`，**不重试**，等用户判断

**这与我最初提议的"Recurring 自动重试"不同。修正理由**：一个每天发日报的任务，provider 挂了两小时，重试 4 次会让用户收到 4 份残缺的日报。**派发层重试是安全的（任务还没跑），执行层重试是危险的（副作用可能已发生）。**

**为什么是 4 次封顶**：覆盖瞬时故障（一次 provider 重启、几分钟网络抖动），覆盖不了逻辑错误——逻辑错误重试也没用。

**落地位置**：`update_after_run` 现在无条件 `run_count + 1` 并算 `next_run_at`。派发失败时改为 `next_run_at = now + backoff`，**且不动 `last_run_at`**——因为 `last_run_at` 是 `should_fire` 的判据之一（`scheduler.rs:104-108`），动了它就不会重试了。

**这个交互是本条最难的地方**，必须钉测试：`last_run_at` 不动 + `next_run_at` 前移 → 下个 tick `should_fire` 仍为 true → 重试。语义正确。

**失败分类从哪来**：`send_message` 返回的 `AgentError` 需能区分 transient/permanent。**这需要 `hermix-core` 侧提供分类函数**——项目已有 `classifyStreamErrorKind`（provider 层错误分类），**复用它，不新建一套**。若它不覆盖 `AgentError`，扩展它而不是在 automation 侧重新判断错误字符串。

### 修 4：并发 session 隔离

事实 2 的另一半：两次 run 打到同一个 `session_id = "automation:{id}"`。

**方案**：`session_id` 改为 `automation:{id}:{run_id}`。

理由——两次 run 逻辑上就是两次独立对话，混在一个会话里会让上下文互相污染（B 轮的 prompt 带着 A 轮的结果）。这也让"某次 run 的上下文"可追溯。

**代价**：会话列表里会出现 `automation:{id}:{run_id}` 这样的碎会话。

**已核实（实施前必查项之一）**：前端**完全没有** automation 会话过滤。`grep -rn "automation:" apps/desktop-tauri/src/` 只命中 `api/polish.ts:68` 的一个 prompt 常量，与会话列表无关；会话列表侧无任何 `startsWith("automation:")` 之类的筛选。

**所以修 4 必须自带过滤**：改 `session_id` 的同时，会话列表要过滤掉 `automation:` 前缀（或后端在列会话时排除）。**两者必须同一个 PR 落地**——只改不改过滤等于给用户塞垃圾。

另一个方向（更保守）：**保持 `session_id` 不变**，靠修 2 的单飞保证同一 automation 不会并发跑。串行情况下共用会话的污染问题大幅减轻（只是历史累积，不会交叉）。**若会话列表过滤改动面较大，走这条**——六步阶梯第 1 条。

## 明确不做

- **不做分布式锁**——单进程桌面应用，没有第二个写入者。进程内唯一性由 SQLite 事务保证。
- **不引入新表存重试计数**——`automation_runs` 里同一 automation 的连续失败行数就是重试次数，加字段是重复存储。
- **不按 `Once` / `Recurring` 决定是否重试** — 见修 3。调度类型与失败性质正交，按性质分类才是 ZCode 的做法，也更准确。
- **不重试 agent 执行失败** — 业务内容重试会重复副作用（发消息、下单、写文件）。ZCode 也没有这一层。**重试的是派发，不是执行。**
- **不实现「同一自动化串行化」**（前一个 run 没跑完就不启下一个）。看起来合理，但 `Recurring` 间隔可能就是 1 分钟而 run 要跑 5 分钟——串行化会导致永远追不上。**积压是用户该看到的现象，不该被系统悄悄吞掉。**

## Alternatives considered

- **在 `tick_once` 里加进程内 `Mutex<HashSet<String>>` 守卫** — 否决：只护单进程，重启即失效；且 SQLite 唯一索引能给出跨进程的真约束。六步阶梯第 3 条。
- **给 `automation_runs` 加 `UNIQUE(automation_id) WHERE status='running'` 部分索引** — 否决：**技术上可行且最强**（数据库层强制单飞），但已有脏数据（现存 `running` 行）会让建索引失败，且引入迁移路径。**决策记入 `Reintroduction conditions`——修 2 落地、数据清干净后，这是更彻底的方案。**
- **用 `next_run_at` 推进代替 run 表判重** — 否决：`next_run_at` 语义是"下次该跑"，不是"正在跑"。混用会导致长 run 期间反复触发。
- **重试交给用户（UI 上放个"重试"按钮）** — 否决：这是最诚实的方案（自动化不该替用户决定重复执行），但用户不在电脑前的场景正是自动化存在的理由。**折中：修 3 只对 `Recurring` 自动重试，且次数封顶，UI 显示重试状态。**
- **退避用固定间隔** — 否决：provider 503 之类的故障是持续性的，固定间隔会在故障期把请求量翻倍，加剧问题。
- **重试次数存 `automation_runtime_state` 表** — 否决：新增列要迁移，且与 `automation_runs` 的行数重复。**事实单一归属**——运行历史属于 runs 表。
- **agent 执行失败也自动重试** — 否决（**与本篇初稿冲突，已裁决**）：初稿提议"`Recurring` 失败就重试"，理由是"持续性任务单次失败不该永久丢失"。但一个每天发日报的任务在 provider 故障期间重试 4 次，会让用户收到 4 份残缺日报。**派发层重试安全（任务还没跑），执行层重试危险（副作用可能已发生）。** 采纳调度模型升级那篇的分类：transient/permanent 按失败性质分，且只重试派发。

## Consequences

- **获得**：重复触发竞态消除（修 1）；跨重启的孤儿 run 被收割，Task 1 的保活信号不会永久卡死（修 2）；`Recurring` 瞬时故障不再永久丢失（修 3）；多次 run 的会话不再互相污染（修 4）。
- **代价**：`TriggerFn` 是公开 API，改签名会波及所有实现方（全仓只有 `automation_trigger.rs` 一处，但 `hermix-automation` 是可发布 crate，外部消费者会 break）。**缓解：给新签名保留一个默认适配方法。**
- **代价**：agent 执行失败仍然不自动重试（与现状一致）。**这是有意的**——但用户要有办法重新触发：已核实 UI 已暴露「立即运行」按钮（`AutomationView.tsx:454`，调 `api.triggerAutomationTask`，`:190`），手动 API 亦可用。**自动化不替用户重复执行，但用户随时能自己来一次。**
- **与另一篇的关系**：调度模型升级那篇的第 2 条（run 表 + 派发状态机 + runId 幂等契约）与本篇修 1 / 修 2 **是同一件事的两种设计**。本篇更小（不引入 `scheduledAt` / `trigger` 字段，不做 runId 复合契约），那篇更全。**建议以本篇为最小落地路径，那篇的结构化 scheduleRule 作为独立后续**——若采纳那篇的 runId 契约，本篇修 1 的"触发前预占"应改为"按 runId claim"。**本篇已在 Reintroduction conditions 记录这一分歧。**
- **风险**：修 3 的 `last_run_at` / `next_run_at` 交互是本篇最脆弱的部分。`should_fire` 的判据（`scheduler.rs:102-118`）依赖这两个字段的特定组合，**改 `update_after_run` 时必须同时重读该函数并补测试**，否则会出现"永远不重试"或"无限重试"。
- **风险**：修 4 若不过滤会话列表，用户会看到大量 `automation:{id}:{run_id}` 碎会话。**已核实前端无任何过滤**（见修 4 节），故过滤必须与改 `session_id` 同 PR，或改走修 4 的保守方案（保持 session_id 不变）。
- **范围**：修 1 / 修 2 动 `hermix-automation` + `hermix-server`；修 3 另加 UI 展示；修 4 视走哪条方案，可能不涉及前端。
- **未验证**：本篇全部基于源码阅读。**修 1 的竞态窗口宽度是推断的**（依赖 tokio 调度时机），建议实施时先加一个"慢 agent"测试复现重复触发，再改——**先复现再修，否则无法证明修复有效**。

## Reintroduction conditions

- **部分唯一索引**（`UNIQUE(automation_id) WHERE status='running'`）：修 2 落地且存量脏数据清理完毕后，把单飞保证从应用层下沉到数据库层，届时可移除 service 层的查重逻辑。
- **`Once` 也重试**：若产品上确认 `Once` 自动化有幂等保障（如用户显式勾选"允许重试"），再纳入修 3 的范围。**注意——修 3 已改为按失败性质分类，`Once` 的瞬时失败本就会重试**；此条改为"是否给 `Once` 也开派发重试"，即本条实际已被修 3 覆盖，保留此项作为回溯索引。
- **改用 runId 复合契约**：若采纳调度模型升级那篇的 `runId = automationId + trigger + scheduledAt` 设计，本篇修 1 的"触发前预占"应改为"按 runId claim"（可同时解决时钟跳变导致的重复触发，而本篇修 1 只解决并发竞态）。**这是本篇与那篇的实质分歧点。**
- **跨进程单飞**：若将来自动化执行被拆到独立进程（`hermix-server` → 独立 worker），修 1 的进程内保证失效，必须改用部分唯一索引。
- **重试策略可配置**：若不同自动化的失败容忍度差异大（有的能重试 10 次，有的必须一次过），把退避参数从常量提到 `automations` 表的列。当前不做——六步阶梯第 1 条。
