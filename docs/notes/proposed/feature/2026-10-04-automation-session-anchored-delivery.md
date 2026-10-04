# Decision Note: 会话锚定投递（自动化结果回投原对话）

Status: proposed — 2026-10-04 逐层核实 cronjob 工具 → 宿主后端 → Automation 表 → 调度器全链路后的重写。**修 1（`_session_id` 注入）与修 2（`anchor_session_id` 字段）已实施**（见下方"实施进度"）；修 3/4/5 仍待 [调度模型升级](2026-10-04-automation-schedule-model-upgrade.md) 落地。核实推翻了原提案的两处前提：hermix 用的不是 ZCode 的 `targetTaskId` 而是 `session_id`；且提案设想的"对话内创建"链路需要先补两个断点才成立。裁决见 [批次评审](../process/2026-10-04-proposed-batch-review.md)。

## 核实结论

### 事实 1：链路是通的，但终点是一个固定字符串

对话内创建定时任务的完整路径（逐层核实）：

```
cronjob 工具（hermix-tools/src/tools/cronjob.rs）
  → 默认挂 SignalCronjobBackend（占位，:20）
  → 宿主在 state.rs:1110 用 DashboardCronjobBackend 覆盖（override_existing=true）
  → 写入 automations 表
  → AutomationScheduler 每 30s tick（scheduler.rs:43）
  → trigger（automation_trigger.rs:95）
  → session_id = format!("automation:{}", auto.id)   ← automation_trigger.rs:165
```

**所以"对话内创建"确实存在**（不是空头支票，`state.rs:1105-1109` 的注释明确说了这件事），**且结果落在一个硬编码的独立会话里**。

这确认了原提案的核心判断：**闭环断裂是真的**。用户在对话里说"每天九点汇总"，结果散在 `automation:{id}` 会话里，用户不会去看。

### 事实 2：hermix 用 `session_id`，不是 ZCode 的 `targetTaskId`

原提案直接照抄 ZCode 的 `targetTaskId`（ZCode 那是 `Task` 概念）。hermix 的 `Automation` 结构体（`lib.rs:67-108`）**没有任何 session/task 锚点字段**，`automation_trigger.rs:165` 硬编码 `automation:{id}`。

`target_task_id` 在 hermix 全仓不存在（`grep -rn "target_task_id" crates/ apps/` 无结果）。

**这不是术语差异，是模型差异**：hermix 的 `hermix_task::Task` 是**任务面板实体**（工作区/模式/状态机），chat session 是**对话历史**。ZCode 的 cron 绑 Task 合理，hermix 绑 Task 会绑错东西——自动化结果要进的是对话流，不是任务卡。

**决策：用 `session_id`，不用 `target_task_id`。**

### 事实 3（断点 1）：`cronjob` 工具拿不到当前 session

`cronjob.rs:125-135` 只读 `toolset` / `schedule` / `task` 三个参数，**没有任何 session 上下文**。

对比 `cua.rs:199-205` 的 `extract_session_id`：

```rust
fn extract_session_id(params: &Value) -> String {
    params.get("_session_id").and_then(|v| v.as_str()).unwrap_or("").to_string()
}
```

**但全仓搜不到任何 `_session_id` 注入点**（`grep -rn '"_session_id"' crates/` 排除 tools 目录后为空）。且 `agent_config.rs:373` 的 `pass_session_id` 语义是"系统提示时间戳块里带 session_id"，**与 tool params 无关**。

**结论：CU 的 `extract_session_id` 是死代码，永远返回空串。** 它让代码看起来有 session 追踪能力，实际没有。

要实现锚定，必须**新建**参数注入机制。这是原提案完全没考虑的成本。

> **已解决（修 1）**：注入点已建于 `ToolRegistry::dispatch_with_context`。本节保留作为"为什么需要这个机制"的依据——注入前的状态确实是死代码。**注意注入点不是本节暗示的 `dispatch_async`**：那两条路径（`dispatch` / `dispatch_async`）没有 `ToolCallContext`，凭空造 session 是错的。详见"实施进度"。

### 事实 4（断点 2）：`AutomationTriggered` 事件没有前端消费者

`orchestration_bus.rs:117` 把它转成 `StreamEvent::AutomationTriggered`（`hermix-transport/src/lib.rs:789`），但前端唯一提及是 `App.tsx:573` 的一句注释：

> `// AutomationTriggered 事件当前没有前端消费者，且保活信号丢一次`

**即：结果回投没有任何现成通道。** 要做锚定投递，需要同时做前端消费。

## 实施进度

### 已完成：修 1（`_session_id` 注入）+ 修 2（`anchor_session_id` 字段）

这两个是纯数据地基，**零行为变化**——锚点被记录但调度侧不消费，回投路径与今天完全一致。

| 层 | 位置 | 改动 |
|---|---|---|
| 注入 | `registry.rs:600` | `dispatch_with_context` 在 `coerce_args` 之后调 `inject_session_id` |
| 注入实现 | `registry/validation.rs` | 空 session 不注入 / 非 object 原样返回 / 不覆盖已有值 |
| 工具读值 | `tools/cronjob.rs` | `create` 读 `_session_id`，空串过滤为无锚点 |
| trait | `tools/cronjob.rs` + `backends/cronjob.rs` | `CronjobBackend::create` 增 `anchor_session_id` 参数 |
| 领域模型 | `hermix-automation/src/lib.rs` | `Automation` / `CreateAutomationParams` 增字段 |
| 存储 | `hermix-automation/src/repository.rs` | 建表加列 + `ensure_column` 幂等 ALTER + INSERT/SELECT/row 映射 |
| 宿主 | `hermix-server/src/dashboard/automation.rs` | `AutomationTask` 增字段，`DashboardCronjobBackend` 透传 |

**注入点与原方案的偏差**：`dispatch_async` → `dispatch_with_context`。原因见"修 1"小节。

**顺序约束**（有测试钉住）：注入必须在 `coerce_args` **之后**。反序时若某 schema 恰好声明了同名键，`coerce_args` 会把注入的会话 id 强转成声明类型。

**顺带生效**：`cua.rs` 的 `extract_session_id` 不再是死代码——18 处 `TraceEvent` 现在带真实 session id（此前恒为空串），按会话分组审计日志有了意义。

**文案未承诺回投**：`DashboardCronjobBackend::create` 的返回文案**没有**写"结果会回到本对话"。锚定分支（修 3）尚未落地，此时这么写等于让模型向用户转述一个不存在的行为。

**风险核查**：注入的 `_session_id` 会进入所有工具的 params。已 grep 确认无工具把 params 整体回显进结果或落库（`custom_app_manager` / `vendor` 是反序列化成结构体；`url_safety` 的 `params_json` 拼接只出现在测试代码里，生产路径只做 allow/block 判定）。

### 未完成：修 3 / 修 4 / 修 5

修 3（运行期分流）依赖调度模型升级的回退路径稳定（`session_id` 要带 `run_id` 后缀）；修 4+5 同批，见"实施顺序"。

## Decision

### 修 1：`_session_id` 注入机制（前置，其余全部依赖它）

**问题**：见事实 3。

**方案**：在 agent loop 派发工具调用的地方（`ToolRegistry::dispatch_with_context`），把当前 session_id 注入到 params 的 `_session_id` 键。

**为什么是这个函数（实施时修正）**：`registry.rs` 里有三处 `coerce_args` 调用点，分属三条派发路径——`dispatch`（同步）、`dispatch_async`、`dispatch_with_context`。只有 `dispatch_with_context` 持有 `ToolCallContext`；`bridge_tool_registry`（`agent_builder.rs:232`）把 ctx 捕获进闭包后走的就是它。**前两条路径没有 session 概念**，注入等于凭空编造。`session_stamper.rs` 的文档曾声称 `agent_loop.execute_tool_calls` 会调 `set_active_session_for_tool`，实际调用方只有 `routes.rs` 的 `patch_todo` / `delete_todo`——文档漂移，不能作为注入点依据。

**为什么用下划线前缀**：不与 LLM 生成的实际参数冲突——模型不生成 `_` 开头的键（各 provider 的 schema 校验惯例）。CU 已经这么用了，**沿用同一约定而非另立**。

**影响面**：所有工具的 params 会多一个键。**风险**——若某个工具把 params 整体回显给用户或原样落库，会泄漏内部 session_id。**已核实** `cronjob` 只读三个具名键，安全；**实施时需 grep 全部工具确认没有 `params` 整体落库的**。

**这一步修完，CU 的 `extract_session_id` 才真正开始工作**——顺带修掉一个既有的死代码。

### 修 2：`Automation` 加 `anchor_session_id`

**问题**：见事实 2。

**方案**：`Option<String>`，语义是"本自动化创建时的对话 session"。

**三种取值**：
- `Some(s)` — 对话内创建（cronjob 工具带上修 1 注入的 `_session_id`）
- `None` — 设置页/UI 创建（无对话上下文）
- 运行期覆写 — 用户可在 UI 上改绑/解绑（后话，不在首批）

**明确不用 `target_task_id`**：见事实 2。task 是工作区实体，session 是对话历史，绑错。

### 修 3：运行期分流 —— 锚定 / 回退

`automation_trigger.rs:165` 的 `session_id` 计算改为：

```
anchor_session_id 存在且目标会话仍在 → 用它
否则 → 保持 automation:{id}（现有回退路径，行为不变）
```

**目标会话"仍在"的判定**：查会话是否存在。**不判定"是否活跃"**——用户可能开着会话但几天没说话，结果照样该投进去。

**为什么不静默回退**：若锚定失败（会话被删）而静默落到独立会话，用户看到的是"结果没出现"，且不知道为什么。**回退必须可见**——UI 上任务卡标"已转独立会话"。原提案已提出这一点，本篇确认它是必需项而非可选项。

**与调度模型升级方案的交互（runId 契约让这一条更顺）**：该方案第 2 条把 `run_id` 定为 `automationId + trigger + scheduledAt` 的复合契约。锚定场景下 `session_id` 就是用户 session（无 run 后缀），**回退路径上加 `run_id` 后缀即可**（`automation:{id}:{run_id}`），而 `run_id` 本来就是那张 run 表的一等字段——不需要额外设计。**实现顺序上仍需先落调度模型升级**（回退路径稳定后再新增锚定分支）。

### 修 4：消息在对话流里的一等表示

**问题**：结果要出现在用户对话里，得有个"这是定时任务的结果"的呈现方式。

**方案**：`StreamEvent` 加一个消息类型，带 `{ task_name, anchor_session_id, run_id, body }`。

前端 `ChatView` 认这个类型：带分组头（"定时任务：<name>"）+ 可折叠 + 来源标注。

**明确不做**：不做成用户消息（带 assistant/user 角色）。理由——它不是用户说的，也不是模型自主对话产生的，是**外部注入**。混进角色流里会污染后续轮次的上下文归属（模型会当成对话的一部分继续接话）。

**上下文膨胀是真实的**：一个每天九点的任务，一年 365 次注入用户的主对话。**护栏必须有**（见修 5）。

### 修 5：注入次数护栏（不做这条例束就没有修 4）

**方案**：同一自动化向目标会话注入 N 次后（或累计 token 超阈值），改为回退独立会话，并在卡片上标记。

**N 取值**：**先不定**。原提案说"超过 N 次"但没给数。**理由——N 取决于用户主对话的日均消息量，没有基线数据拍不出来。决策记入 `Reintroduction conditions`，首批实现只做"计数 + 可配阈值"，默认值等观察后再定。**

**当前能否拿到这个计数**：`automation_runs` 里有全部运行历史，`list_runs` 已存在。**[调度模型升级](2026-10-04-automation-schedule-model-upgrade.md) 落地后 run 表还会多出 `outcome` / `attempts` / `trigger` 等列**，注入计数与"连续失败次数"都可直接从 run 表现算，无需另加表——这也是原提案 Status 里说的"依赖 run 记录"，现在成立了。

## 明确不做

- **不绑 `hermix_task::Task`** — 见事实 2，概念不同。
- **不改 `Once` vs `Recurring` 的投递差异** — 两者都用同一套锚定逻辑。语义上一致（都是"这条任务的结果给我"）。
- **不做结果的双写（锚定 + 独立会话都放）** — 同一件事两处可见，状态口径分裂。原提案已否决，确认。
- **不把注入伪装成用户消息** — 见修 4。
- **不实现"用户主动解绑/改绑"的 UI** — 首批自动锚定就够了，手动改绑是独立需求。
- **不注入 IM 远程会话的特判** — hermix 的 IM 会话 id 格式待查，但**锚定机制对任何固定 session id 都成立**，不需要特判。

## Alternatives considered

- **照抄 ZCode 的 `targetTaskId`** — 否决：见事实 2，hermix 的 task 与 session 是不同概念，绑错。
- **不锚定，只在 UI 上给个"查看结果"的跳转链接** — 否决：这就是现状（`AutomationView` 能看到 run 列表）。用户抱怨的不是"找不到结果"，是"闭环断了"。**降级方案解决不了真问题。**
- **把结果以用户消息身份注入** — 否决：见修 4。污染上下文归属，模型会当成对话继续接话。
- **用 workspace 目录代替 session 作为锚点** — 否决：workspace 是工作区，不是一段对话。同一个 workspace 里用户可能有 5 段无关对话。
- **N 定为 10 次** — 否决：无基线数据。拍一个数出来再改，不如先做计数。
- **先不做护栏，只做锚定** — 否决：护栏不做，锚定就是给用户埋一个"一年后对话被 365 条任务消息撑爆"的雷。**修 4 与修 5 同批，不可拆。**

## Consequences

- **获得**：对话内创建的定时任务结果回到用户正在看的那段对话；通用结果走独立会话的路径**完全不变**（`anchor_session_id` 为 `None` 时行为与今天一致）。
- **前置依赖**：修 2-5 全部依赖修 1。**`_session_id` 注入是整条链的地基**。
- **顺带修掉**：`cua.rs:199` 的 `extract_session_id` 死代码（修 1 完成后它才真正有效）。
- **风险（最大）**：**上下文膨胀**。护栏的 N 值没有基线，首批做"可配 + 默认待定"意味着**首批上线时 N 可能是个随意值**。这是本方案最诚实的短板——已列入 Consequences 而非藏起来。
- **风险**：锚定到用户主对话意味着任务结果会**影响后续对话的上下文**。一个每天报错的定时任务，往用户主对话里注入 365 条错误消息，可能干扰后续正常对话的质量。护栏不能只护 token，**也应护"连续失败"**——连续失败 N 次即停投并告警。
- **范围**：跨 `hermix-tools`（参数注入）+ `hermix-automation`（新字段）+ `hermix-server`（分流）+ `hermix-transport`（新事件）+ 前端（消费 + 呈现）。**这是本批次里跨面最大的一条。**
- **实施顺序**：修 1 → 修 2 → 修 4+5 → 修 3 的分流。修 3 放最后，因为它要依赖 [调度模型升级](2026-10-04-automation-schedule-model-upgrade.md) 的回退路径稳定（`session_id` 要带 `run_id` 后缀）。
- **方法论**：原提案的四条决策全部照抄 ZCode 的字段与语义，没有一层落到 hermix 的实际代码上。**结果是四条里有两条的前提在 hermix 根本不成立**（`targetTaskId` 不存在、注入通道不存在）。跨项目借鉴时，**先核实对方的概念在自己这边的对应物是否存在**，再决定抄不抄。

## Reintroduction conditions

- **注入次数阈值 N 的默认值**：积累一段真实使用数据（不同频率的自动化在主对话里注入多少条仍不影响体验）后再定。当前实现只提供计数与可配。
- **`target_task_id` 兼容字段**：若将来 hermix 的 task 体系确实需要承载"任务结果归档"，再评估双锚点（session 进对话 + task 进任务卡）。当前二者语义不重叠，不硬凑。
- **锚定消息的回复能力**：若用户希望对任务结果直接追问（"这个汇总里为什么少了两条？"），需要一个"以任务消息为起点开子对话"的入口。当前设计下用户只能在原对话里追问，会把上下文搅在一起。
