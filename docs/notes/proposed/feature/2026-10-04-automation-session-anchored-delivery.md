# Decision Note: 会话锚定投递（自动化任务结果回投原对话）

Status: proposed — 对齐 ZCode `ZCodeAutomation.targetTaskId` 语义：会话里创建的定时/周期任务，到点结果投递回**创建它的那条对话**，而不是永远落进独立的 `automation:{id}` 会话。

## 背景事实（读源码所得）

`automation-types.ts`：

```
/** 会话内创建的 cron 绑定到当前 task；后续触发都投递回该 task，不再新建 session。 */
targetTaskId?: string;
```

配 `packages/desktop/src/host/cronBotDelivery.ts`（投递通道）与 `cronRunLifecycle.ts`（run 生命周期回写）。ZCode 明确把"会话内创建"和"设置页创建"分成两种投递语义。Hermix 现状：所有运行落 `automation:{id}` 独立会话（`automation_trigger.rs` / `spawn_run`），会话内创建的任务（`03d297ad` 接通的 cronjob 工具）也走这一条——用户在对话里说"每天九点汇总"，结果散在一个不相关的独立会话里。

## Decision（提案）

1. `AutomationTask` / `WorkflowEntry` 增加 `target_task_id: Option<String>`：任务在**对话内**被创建（cronjob 工具 / workflow 工具）时写入当前 session id；设置页/工具在无会话上下文时创建则留空。
2. 运行时：`spawn_run` 检测 `target_task_id`——存在且目标会话仍在时，结果作为该会话的**任务消息**（带"定时任务：<task name>"分组头，可折叠）注入，run 状态同样回写到任务表但不再新建 `automation:{id}` 会话；目标会话已删除/不可用时**回退**到独立会话（不丢结果）。
3. IM 远程会话天然符合（`im:{platform}:{user}` 固定会话），无需特判。
4. 上下文膨胀护栏：同一任务向目标会话注入超过 N 次（或累计超过 token 阈值）后自动改为独立会话续跑，并在任务卡上标记"已转独立会话"——否则长期定时任务会把用户的主对话历史撑爆。

## Alternatives considered

- **保持现状（永远独立会话）** — 否决：用户在对话里下指令、结果却去别处，闭环断裂；ZCode 专门为这个语义留了字段。
- **把结果复制进目标会话且保留独立会话** — 否决：双份结果，同一件事两处可见，状态口径分裂。
- **定时任务在目标会话里"插队发言"且不带分组标记** — 否决：任务消息与用户对话混流，用户无法分辨哪条是机器人自发。

## Consequences

- 需要任务消息在对话流里的一等表示（分组头 + 可折叠 + 来源标注），`ChatView` 要认这个新行类型。
- 上下文护栏是必需的：不做会退化出"对话被定时任务淹没"的新问题。
- 独立会话回退路径要保留并可诊断（`target_task_id` 悬空要在卡片上可见）。
- IM 场景行为不变（本来就是固定会话），收益在桌面端。
