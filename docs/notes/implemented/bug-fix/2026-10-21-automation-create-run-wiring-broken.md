# Decision Note: 定时任务的「创建」与「执行」两半没接上 —— 面板/对话里建的任务永远不会跑

Status: implemented — 2026-10-21 落地并端到端验证：触发判定重写 + 单飞 + 锚点认领 + 提示词不再以 user 身份落库 + 示例任务改纯读公开信息；
一条每 10 分钟的定时任务自行跑完一轮（超星 6.1 测验 83.4 → 100 分），全程无外部脚本

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

`dashboard/automation.rs` 的 `tick_once` 旧实现：

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
（`automation_trigger.rs` 的 `decide_delivery` → `DeliveryTarget::Anchor`），
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

### 事实 5：引擎 A 没有单飞，两轮会并发

引擎 A 的去重只有「同一个槽 60 秒内不重复」（`last_fired` map，key = 任务 + 槽）。
**运行时间超过调度间隔时，下一槽会再起一轮** —— 超星刷课实测：一节 12 分钟配
15 分钟的槽，中途的 tick 就会再触发一次，两个 agent 同时驱动同一个查看器页面。
引擎 B 有 run 表 + 单飞（`try_begin_run` / `is_in_flight`）挡这一下，引擎 A 没有。

### 对旧笔记的事实更正

`docs/notes/proposed/feature/2026-10-04-automation-session-anchored-delivery.md`
写「『对话内创建』确实存在（不是空头支票）」——**端到端不成立**：
数据地基（`_session_id` 注入 + `anchor_session_id` 字段）与运行期分流都真的落地了，
但**没有任何写入方把面板/工具创建的任务写进引擎 B 读的那张表**，
而引擎 A 自己的触发条件恒为假。

## Decision

**就地修好引擎 A（面板/工具真正在用的那条路），把「合并成一台引擎」记为后续项。**

1. **修触发判定**：判「最近一次**已经过去**的发生时刻」，而不是「下一个未来
   时刻」。实现为纯函数 `due_slot(schedule, last_run_at, now)`：在
   `[now - 2min, now]` 窗口里找第一次发生，它比 `last_run_at` 新就说明这一槽
   还没跑过。附带收益：进程停机期间错过的槽会在下次 tick 补跑。
2. **加单飞**：新增 `AutomationTask.running_since`（JSON 内字段，不动 SQLite
   schema）。`tick_once` 跳过仍在跑的任务；`running_since` 超过 120 分钟没清
   就当那次进程死了、允许重跑。
3. **让锚点真正生效**：`spawn_run` 不再写死 `automation:{id}`，而是把
   `anchor_session_id` 交给与引擎 B **同一套**护栏
   （`hermix_automation::decide_delivery_target`）判定 —— 无锚点 / 锚点已删 /
   连续失败 3 次 / 注入次数达 50 次 → 独立会话（带可见原因）。
   判定规则不重写，两台引擎共用一份。
4. **按自动化身份落库**：派发时带 `ExternalDelivery`，这一轮的提示词与回复都以
   `MessageRole::Automation` 落库 —— 前端渲染成自动化卡片，而**不进 LLM 上下文**，
   模型不会在下一轮看到「用户说过这句话」的假象。
5. **内置示例换成纯读公开信息的任务**（天气 / 要闻 / 名词 / 开源 / 公开课 /
   常识），并给每条提示词加「不要读取或修改任何本地文件、会话历史或配置」的约束。
   旧示例（读会话历史、扫工作目录、看 git 状态）即使被启用也会去翻用户的数据；
   示例只该演示「定时任务能跑」，不该演示「能翻你的私房」。

## Alternatives considered

- **把面板/工具的写入改为落到引擎 B 读的那张表（合并成一台引擎）**：
  这是终态，但**不能现在做**：引擎 B 的领域模型比面板的 `AutomationTask` **窄**
  ——没有 `skills` / `max_runs` / `description` / `category` / `icon`，
  合并要么改 `automations` 表（加列 + `ensure_column` 迁移），要么把这些字段
  搬进 SQLite 领域模型；前者要动 `hermix-automation/src/repository.rs`，
  而并行线此刻正在同一个 crate 上改（`open_with_wal` 换共享 opener）。
  在共享 target 的锁被占、磁盘又在涨的时候插进去改同一个 crate 风险太高。
  → 记为后续项，本篇先把功能恢复。
- **只修触发条件，不加单飞**：跑一轮的时长可能超过调度间隔 → 下一槽再起一轮，
  两个 agent 同时驱动同一个目标。**实测踩过**（12 分钟的活配 15 分钟的槽）。
  否决。
- **让 `spawn_run` 认锚点但不接护栏**：注入次数/连续失败没有闸，
  一个每天报错的任务会一直往用户主对话里灌错误信息。护栏是现成的
  （`decide_delivery_target`），没有理由不用。否决。
- **继续用「会话 HTTP 接口 + 外部脚本」驱动长任务**（2026-10-21 超星刷课就是这么跑的）：
  能跑，但那是**产品外面**的手工接线，不解决「产品自己能不能无人值守」。
  作为**本轮的超星任务**继续用它（它是唯一现在就通的路径），但**不作为长期方案**。

## Consequences

- 修完后，产品**原生**支持无人值守长任务：对话里一句「每 15 分钟继续刷下一节」，
  定时任务锚定到本会话 → 调度器认领 → 续跑原会话 → 结果以自动化卡片回投，
  护栏在注入 50 次 / 连续失败 3 次时自动回退独立会话。
- **端到端验证**（2026-10-21 16:00）：一条每 10 分钟的定时任务（锚定一个真实
  会话，强制注入 `chaoxing-learning` + `browser-automation` 两个 skill）自行
  跑完一轮 —— 认领 → 派发 → 驱动查看器做完超星《管理信息系统》6.1 系统规划
  （视频+测验 83.4 → 重做 100 分）→ 结果以 `MessageRole::Automation` 回投到
  锚定会话 → 下一槽自动再起一轮。全程没有外部脚本参与。
- 超星刷课的驱动已从「外部 PowerShell 循环」迁到**产品内建机制**：外部脚本是
  产品外面手工接线（客户端断开会连带中止服务端那一轮），内建机制带护栏、
  带 run 记账、且不依赖任何外部进程活着。
- 引擎 A 仍缺 run 表那套幂等/退避/`abandoned` 收割；它的去重是
  「槽 + 60 秒」+ 这次的 `running_since` 单飞，够用但比引擎 B 弱。
  **后续合并到引擎 B 时要把这套护栏一起带过去**，别只搬调度。
- 引擎 B（SQLite 那套）在本篇之后仍是**无写入方**的空转引擎。
  合并时它才是终态落点（护栏 / run 表 / 锚定都在那边）。
- 内置示例的语义变了：从「演示自动化面板长什么样」变成「演示一个真的能跑的
  公开信息任务」。用户可随手删；**它们默认停用**，启用也只是查公开信息。
  存量 JSON 里的旧示例按「旧提示词 → 新示例 id」映射自动刷新（用户自己改过
  提示词的示例原样保留，开关状态也保留）。
- 旧笔记的「不是空头支票」结论被本篇更正；实施时应同时归档那篇
  （或在其中标注端到端未通）。
