# Decision Note: 定时任务的「创建」与「执行」两半没接上 —— 面板/对话里建的任务永远不会跑

Status: proposed — 2026-10-21 实测证实：经 `POST /api/automation/tasks` 建的任务，
启用后 `next_run_at` 正常推进、`run_count` 恒为 0、会话里没有任何回投、页面无任何动作

## 事实（逐条核实，非推测）

### 事实 1：有两套自动化引擎、两套存储

| | 引擎 | 存储 | 谁写 | 谁读 |
|---|---|---|---|---|
| A | `dashboard/automation.rs` 的 `AutomationScheduler::start` → `tick_once`（30s tick） | `$HERMIX_HOME/automation_tasks.json` | 面板 CRUD、`cronjob` 工具（`DashboardCronjobBackend`） | 面板 CRUD、引擎 A 自己的 tick |
| B | `automation_trigger.rs::start_automation_scheduler` → `hermix_automation::AutomationScheduler`（30s tick） | `sessions.db` 的 `automations` / `automation_runtime_state` / `automation_runs` 三表 | **没有人** | 引擎 B |

`state.rs:1277 with_automation_scheduler()` 起的是**引擎 A**；
`state.rs:942-978`（`build_with_hermes_home`）起的是**引擎 B**。
两条路径都跑，但**引擎 B 的表里没有任何写入方** —— 面板和 `cronjob` 工具都只写 JSON。

### 事实 2：引擎 A 的触发条件恒为假

`dashboard/automation.rs:884-902`：

```rust
let next = sched.after(&now).next();          // 下一个**未来**时刻，恒 > now
...
let should_fire = match (next, t.last_run_at.as_ref()) {
    (Some(next_at), Some(last)) => next_at <= now && last_at < next_at,
    (Some(next_at), None) => next_at <= now,    // next_at 恒 > now → 恒 false
    _ => false,
};
```

`after(&now)` 取的是**严格晚于 now** 的下一次发生，所以 `next_at <= now` 永远不成立
→ **引擎 A 永远不会触发任何任务**（它只会不断把 `next_run_at` 往后推）。

### 事实 3：引擎 A 还忽略锚点

`spawn_run` 里写死 `let session_id = format!("automation:{}", task.id);` ——
`AutomationTask.anchor_session_id` 存了但没人读。引擎 B 才读锚点
（`automation_trigger.rs:246 decide_delivery` → `DeliveryTarget::Anchor`），
并且引擎 B 才有护栏（`decide_delivery_target`：注入上限 50 次、连续失败 3 次回退）
与 run 表（幂等 / 单飞 / 退避 / `abandoned` 收割）。

### 事实 4：实测

2026-10-21 13:26 经 `POST /api/automation/tasks` 建了一条任务（锚定当前会话，
`schedule = */15 * * * *`），13:26 启用：

- `next_run_at` 由 `05:30:00` 推进到 `05:45:00`（引擎 A 的 tick 在跑，也在推进时间）；
- `run_count` 恒为 0，`last_run_at` / `last_status` 恒空；
- 锚定会话里没有任何回投消息，查看器页面无任何动作（22 分钟无一次工具调用）；
- 该任务 id **只出现在 `~/.hermix/automation_tasks.json`**，任何 SQLite 库里都搜不到。

→ 面板/对话里建的定时任务是**空头支票**。

### 对旧笔记的事实更正

`docs/notes/proposed/feature/2026-10-04-automation-session-anchored-delivery.md`
写「『对话内创建』确实存在（不是空头支票）」——**端到端不成立**：
数据地基（`_session_id` 注入 + `anchor_session_id` 字段）与运行期分流都真的落地了，
但**没有任何写入方把面板/工具创建的任务写进引擎 B 读的那张表**，
而引擎 A 自己的触发条件恒为假。

## Decision

**把自动化收敛到引擎 B（SQLite 那套），让面板/工具的写入直接落到它读的地方。**

1. `dashboard/automation.rs` 的 create / update / delete / run **写穿**到
   `state.automation_service`（`AutomationService`）：面板与 `cronjob` 工具
   走的是同一批 handler，所以**一处改动，两处同时生效**。
2. **删掉引擎 A 的 tick 循环**（`AutomationScheduler::start` + `tick_once`），
   避免两台调度器抢同一个任务。
3. 面板的 `GET /api/automation/tasks` 仍返回 `AutomationTask` 形状（前端契约不动），
   但运行态字段（`run_count` / `last_run_at` / `last_status` / `next_run_at` / `history`）
   改为从 `automation_runtime_state` + `automation_runs` **投影**出来，
   不再由 JSON 自己记账。
4. `automation_tasks.json` 降级为**一次性导入源**：启动时把存量 JSON 里的任务
   迁进 `automations` 表，迁完不再作为权威存储。

## Alternatives considered

- **只修引擎 A 的触发条件（`including(&now)` 之类）+ 让 `spawn_run` 认锚点**：
  改动最小、也能让功能跑起来。但**留下两台引擎**——而两台引擎并存正是这个 bug 的成因
  （同一个概念两个权威存储，迟早再分叉）；且引擎 A 没有 run 表、没有幂等单飞、
  没有退避、没有连续失败护栏。否决。
- **继续用「会话 HTTP 接口 + 外部脚本」驱动长任务**（2026-10-21 超星刷课就是这么跑的）：
  能跑，但那是**产品外面**的手工接线，不解决「产品自己能不能无人值守」。
  作为**本轮的超星任务**继续用它（它是唯一现在就通的路径），但**不作为长期方案**。
- **什么都不做**：面板里那 6 条内置示例任务永远不触发，用户会以为是自己的配置问题。
  否决。

## Consequences

- 修完后，产品**原生**支持无人值守长任务：对话里一句「每 15 分钟继续刷下一节」，
  定时任务落到 `automations` 表 → 引擎 B 认领 → 有锚点就续跑原会话 →
  结果以 `MessageRole::Automation` 落库（前端有 `AutomationBubble` 卡片，
  且**不进 LLM 上下文**，不会把「用户说过的话」伪造进对话）→
  护栏在注入 50 次 / 连续失败 3 次时自动回退独立会话。
- 超星刷课的驱动可以从外部脚本迁到产品内建机制（本轮先继续用脚本，
  因为引擎 A/B 的接线要改 `hermix-server`，与并行线在同一 crate）。
- 前端契约不变（`AutomationTask` 形状），但「运行历史」的真值从 JSON 移到 run 表——
  面板上那 6 条内置示例会从「永远不触发」变成「真的会跑」，**这本身是行为变化**，
  种子示例的 schedule 需要复核（演示任务突然开始跑用户会措手不及）。
- 旧笔记的「不是空头支票」结论被本篇更正；实施时应同时归档那篇
  （或在其中标注端到端未通）。
