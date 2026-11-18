# Decision Note: 自动化调度模型升级（结构化 scheduleRule + 派发重试状态机）

Status: implemented — 对齐 ZCode 调度权威与派发韧性两处：cron 字符串降为展示格式、编辑权威改为结构化 scheduleRule（含 endAt）；run 派发引入状态机与 transient/permanent 分类退避重试。2026-10-04 采纳为调度幂等的最终方案（**取代**已归档的 `../../archived/architecture/自动化调度幂等性（已删除）.md`，其五项实施要点已移植入本篇下节）。**实施要点 1-6 与 Decision 2/3 已全部落地**；**Decision 1（结构化 scheduleRule 表单）未做**，是独立的 UI 工作量。实施中修正了本篇 4 处与实际代码不符的前提，并发现两处既有缺陷（执行层失败被记成成功、手动触发 runId 恒定），见「实施时的方案偏差更正」与「实施过程中发现的更大问题」。

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

## 实施时的方案偏差更正（2026-10-04）

本篇的 Decision 与 Consequences 是**照 ZCode 的类型定义 + 对旧代码的推测**写的。逐条核实后有 4 处前提在 hermix 不成立，处置如下。**保留原文并在此更正**，因为原文的偏差本身就是"照抄别处方案时该查什么"的记录。

### 偏差 1：失败分类不能复用 `classifyStreamErrorKind`（Consequences 末条作废）

原文写「复用 `hermix-core` 已有的 `classifyStreamErrorKind`」。实际它在 **`app/streamEvents.ts`（前端 TypeScript）**，只吃字符串，Rust crate 无法访问。

改为按 `AgentError` 的**判别式枚举**匹配——判别式枚举本就是分类器，比字符串匹配更强。

**但更关键的是：核实后发现 `AgentLoop::run()` 全文零个 `return Err`。** LLM/传输失败被转成 `Ok(graceful_interrupt_result_errored(..))`（`run_loop.rs` 中注释 "treating as graceful interrupt for persistence"），因为返回 `Err` 会让 `local_agent_service` 的 `?` 跳过收尾持久化、session 重载为空。

推论有两条：

1. **`send_message` 返回 `Err` 时，agent 一定还没跑过任何工具**（工具执行在首轮 LLM 成功返回之后）。这意味着只按错误变体分类就足够——**退避重试天然是安全的**。
2. **但这条不变量是脆弱的，且没有任何测试锁定**。任何人把那个 `Ok(..)` 改成 `Err(..)`（这是个诱人的"修复"），不变量立刻破裂，自动化就会开始重试带副作用的 run。

**第 2 条推论直接推翻了原方案的分类前提**：真正需要区分的不是"错误变体"，而是"**失败发生在第几轮之后**"。`last_error` 只有错误文本，没有轮次信息。详见下方「实施过程中发现的更大问题」。

### 偏差 2：`TriggerFn` 改签名不是"公开 API 破坏性变更"

原文 Consequences 写「`hermix-automation` 是可发布 crate，需保留默认适配方法」。实际 `TriggerFn` 在同 crate 内**只有一个调用方**（`hermix-server/src/automation_trigger.rs`），且该 crate 未独立发布。直接改签名，无需适配层。

### 偏差 3：`max_runs` 不在 `hermix_automation::Automation` 上

原文 Reintroduction conditions 称「hermix 的 `max_runs` 已于 `fa131ed7` 落地」。实际该字段**只在 dashboard 的 `AutomationTask` 上**，调度器读的 `hermix_automation::Automation` 没有这一列——**两套模型字段不通用**。

处置：`try_begin_run` 不做 `max_runs` 检查，保留 `valid_until` 检查（`skipped` 的两个来源少了一个）。真要加上限，得先给 `automations` 表加列。

### 偏差 4：`count_active_runs` 的状态名确实会漏（此条原文正确，已按此实施）

原文指出改名期间该查询会漏。核实属实：`AutomationRun.status` 原是 `String` 而非枚举，全仓无 `RunStatus`。已把状态枚举化并**在同一批**更新查询为 `status IN ('claimed','dispatched') AND finished_at IS NULL`。

## 实施过程中发现的更大问题：执行层失败被记成"成功"

上面偏差 1 的第 2 条推论落地时，暴露了一个比本方案全部内容都严重的缺陷。

### 症状

provider 挂掉数小时，期间每次自动化触发都被记为 `Succeeded`，UI 上一片绿。

### 两层抹除

1. `AgentLoop::run()` 把 LLM 失败表达成 `Ok` + `finish_reason = "error"`（保住 transcript）。
2. **`local_agent_service.rs` 的 sync 路径把这三个字段全部硬编码丢弃**：

```rust
Ok(AgentReply {
    text: reply_text,
    message_count: messages.len(),
    finish_reason: None,      // ← 抹掉 "error"
    last_error: None,         // ← 抹掉原始错误
    interrupted: false,       // ← 抹掉中断标记
})
```

紧邻的 stream 路径（`send_message_stream` 收尾处）三个字段**全是透传**。同步路径这三行是历史遗留，自动化走的正是同步路径。

### 连带影响：原方案的重试机制无法实现

原方案第 3 条要求"派发失败按 transient 退避重试"。但如果按偏差 1 的原计划（只判错误变体），实施时会发现：

- `send_message` 的 `Err` 只有三个出口（`resolve_effective_model` / `resolve_provider` / `agent.run()`），前两个都是 `Config`（permanent）；
- `LlmApi` / `Gateway` / `RateLimited` / `Timeout` **在自动化路径上永不可达**——它们全在 `call_llm_with_retry` 内部，被抹成 `Ok` 了。

**即：只按错误变体分类，`failed_to_dispatch` 永远不会被记录，`MAX_DISPATCH_ATTEMPTS` 和 `backoff_delay_ms` 全是死代码。** 表面"实现完成"，实际一行都不会执行。

### 解法：用轮次数替代错误变体作为判据

`AgentLoop` 在**每次 LLM 调用之前**自增 `state.total_turns`（`run_loop.rs`，`state.total_turns += 1`）。该值已写入 `AgentResult`，只是没透出到 `AgentReply`。补一个字段即可：

- `total_turns == 1` → 首轮调用就失败 → **没有任何工具执行过** → 可安全重试
- `total_turns >= 2` → 至少有一个响应回来过 → 工具可能已执行 → **绝不重试**

据此新增 `hermix_automation::dispatch_outcome`，输入 `AgentReply` 输出 `Succeeded` / `Failed{retryable}` / `DispatchFailed{retryable}`。瞬时性只能从 `last_error` 文本判（`is_transient_error_text`），**刻意保守**：宁判永久（漏一次重试）也不判瞬时（重放带副作用的 run）。

**这是个应该早点想到的判据**：整个方案纠结"错误变体够不够细"，而真正的信息（跑了几轮）一直在手边，只是没被暴露出来。

## 实施进度

| 实施要点 | 状态 | 落点 |
|---|---|---|
| 1 同步 claim | ✅ | `scheduler.rs::tick_once_at` 同步 `try_begin_run` 后才 `trigger`；`record_run_start` 删除 |
| 2 启动收割 | ✅ | `reap_abandoned_runs`，`state.rs` 在 scheduler 启动**之前**调用 |
| 3 单飞状态查重 | ✅ | `try_begin_run` 内 `count_in_flight`，有测试逐状态对比枚举判据与 SQL |
| 4 退避不动 `last_run_at` | ✅ | `defer_for_retry`；`should_fire` 抽成纯函数 `is_due` 以便钉边界 |
| 5 手动触发共用 claim | ✅ | `trigger_automation_manually` 走同一个 `try_begin_run` |
| 6 回退 session_id 带 run_id | ✅ | `automation:{id}:{run_id}`；有锚点时用 `anchor_session_id`；**前端会话列表同步过滤 `automation:` 前缀**（`App.tsx` 的 `chatSessions`，与既有的 `!s.workbench_id` 同一模式：列表出口过滤、按 id 查全量） |
| Decision 1 结构化 scheduleRule | ❌ 未做 | 独立 UI 工作量，本批明确不做 |
| Decision 2 状态机 | ✅ | `RunStatus` 七态 + 旧字面量兼容（`running`→`Dispatched`、`success`→`Succeeded`） |
| Decision 3 退避重试 | ✅ | `MAX_DISPATCH_ATTEMPTS=3`、`backoff_delay_ms`（30s 起指数、10min 封顶） |

### 实施中额外修掉的既有缺陷

- **执行层失败被记成"成功"**（见上节，最高优先级）。
- **手动触发的 runId 是恒定值**：`compose_run_id` 对 `scheduled_at = None` 返回 `{automation}:manual`，**不含任何区分信息**。`run_exists` 去重因此对手动触发完全失效——同一天第二次点"立即运行"被当作重复槽位静默丢弃，点了按钮什么都没发生也没有报错。
  手动触发**没有槽位可折叠**：每次点击都是独立意图。改为 `None` 时也带墙钟时间戳，单飞（而非去重键）才是那条路径的防线。
  *这个 bug 是被新写的 reap 测试顺带暴露的*——它断言"收割后能再手动触发"却失败，而失败原因不是收割，是去重键。
- **自动化 Task 永远停在 `Pending`**：`execute_automation_run` 建 Task 后立即跑 agent，run 失败时 Task 仍是 `Pending`；而 `TaskService::fail` 只接受 `Planning | InProgress` → 迁移被拒 → 调用方只 `warn!` → 错误被吞、UI 上 Task 永远"未完成"，看不出这次运行失败了。
  改为接受 `Pending`，并同步放开 `validate_transition` 的 `Pending → Failed`（否则 `fail()` 与 `transition()` 两套判定分叉）。
  *原代码就有此问题*（`result.is_ok() ? complete : fail`），只是没人写过"run 失败时 Task 状态是什么"的测试。
  顺带说明：`Pending → Completed` 一直在迁移表里，所以成功路径从没暴露过这个问题。
- **`should_fire` 抽成纯函数 `is_due` + 注入时钟 `tick_once_at`**。原测试用"设未来几十毫秒 + sleep"验证退避是否到期，实测**每次仓储调用都 `open_with_wal`（新连接 + PRAGMA），几十毫秒的窗口在慢盘上会被写操作整个吃掉**——测的是机器快慢，不是退避逻辑。注入时钟后零 sleep。
- **`AgentReply` 新增 `total_turns`**。
- **`skipped` 不占单飞槽**：否则一次被拒的 claim 会连带挡住下一次正常触发。
- **退避分支跳过 `update_after_run`**：它会重算 `next_run_at` 覆盖刚设的退避时刻，还会把 `last_run_at` 推前（`is_due` 读作"这槽已跑过"）→ 重试永不触发。
- **`skipped` 不是失败**：UI 不应标红。

### 关键不变量（有测试钉住，勿改）

1. **`Err(AgentError)` ⇒ 零副作用**。依赖 `run_loop.rs` 那个 `Ok(..)`。若有人改成 `Err(..)`，须同时改 `dispatch_outcome` 的判据。
2. **`defer_for_retry` 不动 `last_run_at`**。动了重试永不触发（表现为"永远不重试"，无报错）。
3. **`is_in_flight()` 与 SQL 谓词一致**。单飞检查与保活信号共用同一谓词，不一致会让机器永不休眠而 UI 查不出原因。
4. **调度触发才用槽位去重**。手动触发的 runId 必须每次唯一——`{automation}:manual` 是恒定值，会让第二次点击静默失效。

### 三条踩过的测试陷阱（写自动化测试时适用）

1. **别用"未来几十毫秒 + sleep"验证时间窗口到期**。每次仓储调用都开连接 + PRAGMA，窗口会被写操作吃掉。注入时钟（`tick_once_at`）+ 判据抽成纯函数（`is_due`）。
2. **别靠数据库摆位造对照组**。曾试着用 `defer_for_retry` + `update_after_run` 的调用顺序造"last_run_at 被改动"，失败——`update_after_run` 内部会重算 `next_run_at`，顺序颠倒就失效。判据逻辑抽成纯函数后对照组直接喂数字。
3. **"新写的测试失败"要先分清是实现错还是断言错**。本轮两次失败：一次是实现错（Task 迁移规则），一次是断言错（`count_active_runs` 我没查、run_id 去重是另一个原因）。**先追根因再改代码**——把 reap 测试直接改松就会掩盖真 bug。

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
- **`maxRuns` 语义**：ZCode 有 `recurring/maxRuns`（有限次）。**hermix 的 `hermix_automation::Automation` 上没有这一列**——`max_runs` 只存在于 dashboard 的 `AutomationTask` 上，两套模型字段不通用（见偏差 3）。若要支持，先给 `automations` 表加列，并明确它与 `skipped` 终态的关系。
- **重试策略可配置**：若不同自动化的失败容忍度差异大（有的能重试 10 次、有的必须一次过），把退避参数从常量提到 `automations` 表的列。当前不做——六步阶梯第 1 条。
- **时钟跳变的显式处理**：runId 契约（`automationId + trigger + scheduledAt`）理论上能覆盖时钟回跳，但具体行为（回跳后应补跑还是跳过）需产品定。**当前方案未展开这一条。**
