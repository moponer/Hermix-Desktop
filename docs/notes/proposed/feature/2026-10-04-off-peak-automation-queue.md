# Decision Note: 闲时任务队列（off-peak）与保活联动

Status: proposed — 2026-10-04 评审：保活联动部分已单独立项（见 [批次评审](../process/2026-10-04-proposed-batch-review.md) 排序第 2）；闲时队列推迟到调度模型升级落地之后——有幂等 run 记录才谈得上排队。

## 背景事实（读源码所得）

- `desktopCronScheduler.ts` 除 cron 外还有整套 off-peak 协议：`OffPeakRunResultPayload { offPeakTaskId, ok, conversationId?, sessionId?, error?, failureKind? }`、`offpeak-active-count` 消息、`CUSTOMIZE_TEMPLATE` 兜底模板。
- 保活与闲时任务耦合：调度器回报"闲时任务执行中计数"，main 进程据计数 + 用户设置切 Electron `powerSaveBlocker`（注释原文："keep-awake：main 据此 + 设置切 powerSaveBlocker"）。即**保活的真实触发条件是"有后台任务在跑"，不是"前台在流式"**。
- 模板目录里 off-peak 模板独立成套（`OffPeakAutomationTemplate`，带 `customize` 本地兜底）。

Hermix 现状：keep-awake 开关（`cecebc3`）只在**会话流式**时调用 `set_keep_awake`（`App.tsx` 监听 `streamingSessionId`），后台定时任务在执行时**不保活**——笔记本合盖/休眠会让到点任务错过窗口。

## Decision（提案）

1. **闲时任务（off-peak）**：任务增加 `off_peak: bool`（缺省 false）。开启后不要求精确时刻：有预算约束时优先排进低峰时段（可配"最迟必须完成"作为硬截止，`endAt` 复用调度模型升级里的字段）。执行语义与普通任务一致（同一 dispatch），区别只在**触发时机**——由队列在高负载/低优先级时段消费。
2. **保活联动**：`set_keep_awake` 的触发条件从"会话流式"扩展为"会话流式 **或** 任一任务（含闲时）在跑"。后端在 `spawn_run` 的进出（run 状态回写处）经事件/计数通知前端，前端合并两个信号驱动开关——与 ZCode 的 `offpeak-active-count` 消息同构。
3. 闲时任务的 UI：任务卡一个"闲时"标记 + 队列深度/下次可跑时段的可见性；保活开关文案与触发条件同步更新（"Hermix 有任务在跑时保持电脑唤醒"）。

## Alternatives considered

- **不做闲时队列** — 否决（针对"笔记本合盖错过任务"这个真实失败模式）：保活联动（2）就能解决大部分，而闲时排队是"算力便宜时再跑"的成本优化，可独立于队列先上保活。
- **用"有限次 + 排期"模拟闲时** — 部分否决：`max_runs` 管次数、`endAt` 管截止，都不表达"低峰再跑"；没有优先级与预算约束。
- **保活继续只绑流式** — 否决：定时任务恰恰是无人盯着、最需要保活的场景。

## Consequences

- 保活语义与文案变化（可能影响已有用户预期，需在设置里写清触发条件）。
- 闲时队列需要排队/优先级/预算展示，一次完整功能；与 [调度模型升级](2026-10-04-automation-schedule-model-upgrade.md) 共享 `endAt` 与调度器改造，先做调度模型升级再上队列。
- 保活联动可独立先做（改动小、直击休眠漏跑）。
