# Decision Note: 自动化调度模型升级（结构化 scheduleRule + 派发重试状态机）

Status: proposed — 对齐 ZCode 调度权威与派发韧性两处：cron 字符串降为展示格式、编辑权威改为结构化 scheduleRule（含 endAt）；run 派发引入状态机与 transient/permanent 分类退避重试。

## 背景事实（读源码所得）

`automation-types.ts`：

- **调度权威是结构化规则**：`ZCodeAutomationScheduleRule { unit, interval, hour, minute, anchorAt, weekdays?, monthDays?, months?, monthlyMode }`；`cronExpr` 注释写明"保留为兼容展示，调度以本字段为权威"；配套 `endAt`（截止）、`recurring/maxRuns`（有限次）。
- **派发状态机**：`ZCodeAutomationDispatchStatus = idle | claimed | dispatched | failed_to_dispatch`；单次 run 另有 `claimed | dispatched | failed_to_dispatch | skipped`；`dispatchAttempts / retryAt` 驱动退避；host 回报区分 `failureKind: "transient" | "permanent"`（`desktopCronScheduler.ts` 的 `CronRunResultPayload`）。
- **runId 契约**：内嵌 `automationId + trigger(schedule|manual) + scheduledAt`，是去重/重放的唯一解析入口。

Hermix 现状：`spawn_run` fire-and-record，判触发靠 `next_run_at <= now && last_run_at < next_run_at` 的时间戳比较；无 run 表、无状态机、无重试；编辑器直接暴露 cron 5 段字符串；`endAt` 缺失（`max_runs` 已于 `fa131ed7` 落地）。

## Decision（提案）

1. **结构化 scheduleRule 为调度权威**：`AutomationTask.schedule_rule`（可选）+ `end_at`，cron 降为编辑器的"高级输入"与既有数据的兼容字段。编辑器默认给结构化表单（每 N 分钟/小时、每天、工作日、每周几、每月 N 日/第 N 个周几、每年），用户切 cron 模式时反向解析成 scheduleRule。存量 `automation_tasks.json` 在读取时一次性转换（cron 解析失败的任务保持原样并在卡上标记）。
2. **run 表与派发状态机**：`automation_runs` 记录每次触发的 `run_id / scheduled_at / trigger / claimed_at / dispatched_at / outcome / error / attempts / retry_at`；调度 tick 先 claim 后派发（`dispatchStatus` 同步），按 runId 幂等——时钟跳变、进程重启、重复 tick 都不漏跑不重跑。
3. **退避重试**：派发失败（不是 agent 执行失败）按 `transient`（网络/宿主忙）指数退避重试，`permanent`（配置非法/凭据缺失）直接失败并把原因写到任务卡；agent 运行失败仍按现状记 `last_status=failed`，不自动重试（重试业务内容需要用户判断）。
4. 与阶段一工作流（日志在会话）不冲突：定时任务日志进 run 表，工作流日志在会话，两套各自闭环。

## Alternatives considered

- **继续只用 cron 字符串** — 否决：cron 无法自然表达"每月第 3 个周二""工作日跳过节假日"，且中文"每工作日 9 点"要用户手算 5 段。
- **引入独立调度进程（ZCode 的 utilityProcess 路线）** — 否决：我们的执行是进程内 spawn，不存在跨进程派发失败面，独立进程只增加 IPC 与生命周期复杂度；除非将来多进程/多机调度。
- **agent 执行失败也自动重试** — 否决：业务内容（发消息、写文件）重试可能重复副作用，ZCode 也没有这一层。

## Consequences

- 存量数据一次性转换 + 转换失败的可见标注。
- 调度器与存储复杂度显著上升（run 表 + 状态机 + 重试），换来确定性：漏跑/重跑从"时间戳碰运气"变成幂等契约。
- 结构化编辑器是一次完整表单设计工作；cron 高级模式保留给老用户，不强制迁移。
