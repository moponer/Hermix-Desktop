# Decision Note: 自动化调度模型升级（结构化 scheduleRule + 派发重试状态机）

Status: proposed — 对齐 ZCode 调度权威与派发韧性两处：cron 字符串降为展示格式、编辑权威改为结构化 scheduleRule（含 endAt）；run 派发引入状态机与 transient/permanent 分类退避重试。2026-10-04 采纳为调度幂等的最终方案（**取代**已归档的 `../../archived/architecture/2026-10-04-automation-schedule-idempotency.md`，其五项实施要点已移植入本篇下节）。

## 背景事实（读源码所得）

`automation-types.ts`：

- **调度权威是结构化规则**：`ZCodeAutomationScheduleRule { unit, interval, hour, minute, anchorAt, weekdays?, monthDays?, months?, monthlyMode }`；`cronExpr` 注释写明"保留为兼容展示，调度以本字段为权威"；配套 `endAt`（截止）、`recurring/maxRuns`（有限次）。
- **派发状态机**：`ZCodeAutomationDispatchStatus = idle | claimed | dispatched | failed_to_dispatch`；单次 run 另有 `claimed | dispatched | failed_to_dispatch | skipped`；`dispatchAttempts / retryAt` 驱动退避；host 回报区分 `failureKind: "transient" | "permanent"`（`desktopCronScheduler.ts` 的 `CronRunResultPayload`）。
- **runId 契约**：内嵌 `automationId + trigger(schedule|manual) + scheduledAt`，是去重/重放的唯一解析入口。

Hermix 现状：`spawn_run` fire-and-record，判触发靠 `next_run_at <= now && last_run_at < next_run_at` 的时间戳比较；无 run 表、无状态机、无重试；编辑器直接暴露 cron 5 段字符串；`endAt` 缺失（`max_runs` 已于 `fa131ed7` 落地）。

## Decision（提案）

1. **结构化 scheduleRule 为调度权威**：`AutomationTask.schedule_rule`（可选）+ `end_at`，cron 降为编辑器的"高级输入"与既有数据的兼容字段。编辑器默认给结构化表单（每 N 分钟/小时、每天、工作日、每周几、每月 N 日/第 N 个周几、每年），用户切 cron 模式时反向解析成 scheduleRule。存量 `automation_tasks.json` 在读取时一次性转换（cron 解析失败的任务保持原样并在卡上标记）。
2. **run 表与派发状态机**：`automation_runs` 记录每次触发的 `run_id / scheduled_at / trigger / claimed_at / dispatched_at / outcome / error / attempts / retry_at`；调度 tick 先 claim 后派发（`dispatchStatus` 同步），按 runId 幂等——时钟跳变、进程重启、重复 tick 都不漏跑不重跑。**run 状态机为 `claimed → dispatched → {succeeded | failed | failed_to_dispatch | skipped | abandoned}`**，各终态的产生条件：`succeeded`/`failed` 是 agent 跑完的成败（`failed` 不重试，见第 3 条）；`failed_to_dispatch` 是派发层失败且已耗尽重试次数；`skipped` 是**成功 claim 但主动不执行**（命中 `max_runs` 上限、`end_at` 已过、另一 run 在途）；`abandoned` 是进程重启时收割的在途行（见实施要点 2）。**`skipped` 不是失败**——它是"这次不该跑"，UI 上不应标红。
3. **退避重试**：派发失败（不是 agent 执行失败）按 `transient`（网络/宿主忙）指数退避重试，`permanent`（配置非法/凭据缺失）直接失败并把原因写到任务卡；agent 运行失败仍按现状记 `last_status=failed`，不自动重试（重试业务内容需要用户判断）。分类标准按**失败性质**而非 `Once`/`Recurring`——调度类型与失败性质正交，`Once` 也会遇瞬时故障，`Recurring` 也会因配置非法而重试纯属浪费。
4. 与阶段一工作流（日志在会话）不冲突：定时任务日志进 run 表，工作流日志在会话，两套各自闭环。

## 实施要点（2026-10-04 逐行核实 hermix 调度路径后补入）

上文 Decision 是照 ZCode 的类型定义写的，以下是把每条落到 hermix 实际代码时**必须知道的事**。核实对象：`hermix-automation/src/scheduler.rs`、`hermix-server/src/automation_trigger.rs`、`repository.rs`。

### 要点 1：现状的重复触发是一个确定的竞态，不是"时间戳碰运气"

`automation_trigger.rs:95-105` 的 trigger 回调：

```rust
Arc::new(move |auto: Automation| {
    tokio::spawn(async move {
        execute_automation_run(&auto, &svc, &agent, &bus, tasks.as_deref()).await;
    });
})
```

而 `record_run_start` 在 `execute_automation_run` **第一行**（`:120`），即在那个 `tokio::spawn` **里面**。`trigger()` 返回时 `automation_runs` 表是空的、`last_run_at` 还是旧值。若 spawned task 未被及时调度（runtime 繁忙、进程刚启动），30s 后的 tick 判定 `should_fire` 仍为 true → 同一自动化触发两次。

**这直接决定了 claim 必须同步做**：`automation_trigger.rs:120` 的 `record_run_start` 要提到 `trigger()` **之前**，把 run_id 作为参数传进去。`Tick 异步 spawn` 与 `claim 同步落库` 的分离，是本方案第 2 条的最小实现形态。

### 要点 2：runId 幂等解决不了跨重启的孤儿行

第 2 条说「按 runId 幂等——时钟跳变、进程重启、重复 tick 都不漏跑不重跑」。**幂等键只能防重复，不能回收已死进程留下的行。**

进程在 claim 之后、dispatch 之前崩溃，留下一条 `claimed` 状态的 run，永远不会再有人推进它。**必须配一条启动收割**：

```sql
UPDATE automation_runs
SET status = 'abandoned', finished_at = <now>,
    error_text = 'process restarted while in flight'
WHERE status IN ('claimed', 'dispatched') AND finished_at IS NULL;
```

**这不是可选的加固**——Task 1 刚加的保活信号（`count_active_runs`，查 `status='running' AND finished_at IS NULL`）会因为这条永久行**让机器永远不休眠**。用户在 UI 上只会看到"合上盖子它不睡"，查不出原因。

**`abandoned` 是第 2 条状态机里缺的那个终态**，应补进去：`claimed | dispatched | failed_to_dispatch | skipped | abandoned`。

**`status` 在 Rust 侧用枚举 + 双向转换，不加 DB `CHECK` 约束。** 现状 `repository.rs:102` 的 `status TEXT NOT NULL` 无约束，而测试（如 `automation_trigger.rs:418`）断言的是字面量 `"success"` 而非枚举——加状态时容易漏改这类断言。**加 `CHECK` 需要重建表**（`ALTER TABLE` 不支持给已有表加约束），存量数据迁移是额外风险；收益只是"挡住手滑"，而类型系统已在编译期挡住。**六步阶梯第 5 条：单行能解决的不写多行。**

### 要点 3：单机单飞用状态查重，不必等唯一索引

「不漏跑不重跑」在本项目（单进程桌面应用）不需要 DB 唯一约束：`record_run_start` 前先查该 automation 有无在途 run，有则跳过本次触发即可。

更强的做法是部分唯一索引 `UNIQUE(automation_id) WHERE status IN ('claimed','dispatched')`——**技术可行且能下沉到 DB 层**，但存量脏数据会让建索引失败，且需迁移。**首批用状态查重，索引记入 Reintroduction conditions**。理由与第 2 条 Alternatives 里"不引入独立调度进程"同源：单进程场景没有第二个写入者，别为一个不存在的并发源建机制。

### 要点 4：`should_fire` 的判据必须重读后再动 `update_after_run`

退避重试要改 `update_after_run`（当前无条件 `run_count + 1` 并重算 `next_run_at`）。但 `scheduler.rs:102-118` 的 `should_fire` 依赖 `last_run_at` 与 `next_run_at` 的**特定组合**：

```rust
ScheduleType::Recurring => match state.next_run_at {
    Some(next) if next <= now => match state.last_run_at {
        Some(last) => last < next,   // ← 动了 last_run_at 就不会重试
        None => true,
    },
    _ => false,
}
```

派发失败退避时必须 **`next_run_at` 前移 + `last_run_at` 不动**，判据才仍为 true。**这是整个重试机制最脆弱的一处，改的时候必须同时重读这个函数并补测试**，否则表现是"永远不重试"或"无限重试"两种之一。

### 要点 5：手动触发与调度触发共用同一个 claim

`trigger_automation_manually`（`:286`）目前直接 `await execute_automation_run`，与 scheduler 无互斥，且共用 `session_id = "automation:{id}"` → 并发写同一会话历史。

**claim 判据必须放在 service 层**（如 `try_begin_run`），让两条路径都走它。放在 repository 层或调度器层都挡不住手动触发。

### 要点 6：回退路径的 `session_id` 要带 `run_id`，且前端没有过滤

多次 run 打到同一个 `session_id = "automation:{id}"`（`automation_trigger.rs:165` 硬编码）→ 上下文互相污染，且无法追溯"某次 run 的上下文"。**回退路径应改为 `automation:{id}:{run_id}`**——`run_id` 本来就是本方案第 2 条定义的一等字段，不需要额外设计。

**已核实（实施时必查）**：前端**完全没有** automation 会话过滤。`grep -rn "automation:" apps/desktop-tauri/src/` 只命中 `api/polish.ts:68` 的一个 prompt 常量，与会话列表无关；会话列表侧无任何 `startsWith("automation:")` 之类的筛选。

**所以改 `session_id` 与加过滤必须同一个 PR**——只改不加等于往用户会话列表里塞垃圾。

**若过滤改动面过大，可先保持 `session_id` 不变**：本方案落地后 claim 机制已保证同一 automation 不会并发跑，串行情况下共用会话的污染大幅减轻（只是历史累积，不会交叉）。**这是个有意识的降级选项，不是遗漏。**

（锚定投递方案落地后，回退路径的 session_id 才会退居次要——主路径会走用户自己的 session。届时过滤需求也随之消失。见 [会话锚定投递](2026-10-04-automation-session-anchored-delivery.md)。）

## Alternatives considered

- **继续只用 cron 字符串** — 否决：cron 无法自然表达"每月第 3 个周二""工作日跳过节假日"，且中文"每工作日 9 点"要用户手算 5 段。
- **引入独立调度进程（ZCode 的 utilityProcess 路线）** — 否决：我们的执行是进程内 spawn，不存在跨进程派发失败面，独立进程只增加 IPC 与生命周期复杂度；除非将来多进程/多机调度。
- **agent 执行失败也自动重试** — 否决：业务内容（发消息、写文件）重试可能重复副作用，ZCode 也没有这一层。**派发层重试安全（任务还没跑），执行层重试危险（副作用可能已发生）**——一个每天发日报的任务在 provider 故障期重试 4 次，用户会收到 4 份残缺日报。
- **只做并发去重、不做 runId 幂等**（原先的"触发前预占"最小方案）— 否决：它只解决并发竞态，**不解决时钟跳变**（系统时间回拨或前跳会导致漏跑/重跑）。本方案更完备，实现成本相近。
- **在进程内用 `Mutex<HashSet<id>>` 做单飞** — 否决：只护单进程，重启即失效；`try_begin_run` 里的状态查重跨进程有效，且不引入新的锁序。
- **给 run 表加 `UNIQUE(automation_id)` 全表唯一约束** — 否决：历史 run 表天然一对多，唯一约束会直接建不上。要防的是"同一时刻两条在途"，得用部分索引 `WHERE status IN ('claimed','dispatched')`——但存量脏数据会让建索引失败，故首批用状态查重（见 Reintroduction conditions）。
- **退避用固定间隔** — 否决：provider 503 之类的故障是持续性的，固定间隔会在故障期把请求量翻倍，加剧问题。
- **重试次数单独建表/加列** — 否决：`automation_runs` 里同一 automation 的在途与历史 run 数就是重试次数与连续失败数，**事实单一归属**——运行历史属于 runs 表。

## Consequences

- 存量数据一次性转换 + 转换失败的可见标注。
- 调度器与存储复杂度显著上升（run 表 + 状态机 + 重试），换来确定性：漏跑/重跑从"时间戳碰运气"变成幂等契约。
- 结构化编辑器是一次完整表单设计工作；cron 高级模式保留给老用户，不强制迁移。
- **hermix 实际路径的改动面**（实施要点逐条对应）：
  - `automation_runs` 表加列：`scheduled_at / trigger / claimed_at / dispatched_at / outcome / attempts / retry_at`（`repository.rs:97` 现有 6 列）
  - `automation_trigger.rs:120` 的 `record_run_start` 上提到 `trigger()` 之前，`TriggerFn` 签名从 `Fn(Automation)` 改为 `Fn(Automation, String)`（**公开 API 破坏性变更**，`hermix-automation` 是可发布 crate，需保留默认适配方法）
  - 启动路径加一次孤儿行收割
  - `service.rs` 加 `try_begin_run`（手动 + 调度共用）
  - `update_after_run` 改为按派发结果分流（退避 / 正常推进）
  - `status` 在 Rust 侧改为枚举（`RunStatus` + `as_str`/`parse`），**不加 DB `CHECK`**——加约束需重建表，而类型层已在编译期挡住拼错
- **保活联动的前置依赖**：`count_active_runs`（Task 1，`75f5f058`）的语义是「`status='running' AND finished_at IS NULL`」，本方案把状态扩为 `claimed/dispatched/abandoned` 后，**该查询需同步更新**，否则收割成 `abandoned` 的行不会被排除（虽已非 `running`，但中间态改名期间会漏）。**两项必须同 PR。**
- **失败分类复用既有机制**：`transient`/`permanent` 判定应复用 `hermix-core` 已有的 `classifyStreamErrorKind`（provider 层错误分类），**不在 automation 侧重新判断错误字符串**。若它不覆盖 `AgentError`，扩展它。

## Reintroduction conditions

- **部分唯一索引**（`UNIQUE(automation_id) WHERE status IN ('claimed','dispatched')`）：本方案落地、存量脏数据清理完毕后，把单飞保证从应用层查重下沉到数据库层，届时可移除 `try_begin_run` 里的 SELECT。
- **独立调度进程**：若自动化执行被拆到独立进程（`hermix-server` → 独立 worker），进程内保证失效，索引与 claim 协议必须重做。
- **`maxRuns` 语义**：ZCode 有 `recurring/maxRuns`（有限次），hermix 的 `max_runs` 已于 `fa131ed7` 落地但语义是否与 run 表的 `skipped` 终态对齐，**需实施时核实**。
- **重试策略可配置**：若不同自动化的失败容忍度差异大（有的能重试 10 次、有的必须一次过），把退避参数从常量提到 `automations` 表的列。当前不做——六步阶梯第 1 条。
- **时钟跳变的显式处理**：runId 契约（`automationId + trigger + scheduledAt`）理论上能覆盖时钟回跳，但具体行为（回跳后应补跑还是跳过）需产品定。**当前方案未展开这一条。**
