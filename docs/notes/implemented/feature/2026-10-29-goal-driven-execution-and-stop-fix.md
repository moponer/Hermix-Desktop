# Decision Note: 目标驱动执行 + 停止即时生效

Status: implemented — 2026-10-29 落地

2026-10-29。两个用户可感知缺陷同源修复：「长任务每做完一阶段就写总结并问『要我接着往下推，还是这轮先到这？』」与「任务执行中点停止按钮没反应」。

## Decision

### 1. 一轮的结束条件从「说完了」改成「目标达成了」

`run_loop` 的 `should_autonomous_nudge` 旧判据含 `!history_has_tool`：只有「本次运行还没用过任何工具」才把模型推回去干活。长任务一旦动过工具，该判据永久为假，于是「干一段 → 写总结 → 交回用户」成为唯一出口，用户必须每轮手动打「继续」。

改为 `should_goal_continue`：

- 条件只剩「本轮工具链之后还没推过」（`autonomous_nudge_count == 0`）+ `autonomous_mode` + 有工具 + 预算。
- 模型每次重新调用工具，`autonomous_nudge_count` 清零（与 `codex_ack_continuations = 0` 同一位置），所以「干活 → 收尾 → 被推回 → 再干活」可以一路循环到目标完成。
- 推过一次仍然只说话 → 计数停在 1 → 放行收尾，不空转。
- `MAX_GOAL_CONTINUATIONS = 40` 是全程硬上限（`max_turns` 默认 0 = 不限，需要自己的安全阀）。

nudge 文本同步改成「目标自检」：先判断**整个目标**是否达成（判据只能是实据：文件已写入 / 命令已执行 / 测试已通过 / 页面已回预期），未达成直接继续做，达成给结论，并明令禁止用「要不要继续 / 这轮先到这」收尾。`StoppedSilently`（工具跑完但一个字没说）不走这条路，仍交给 post-tool summarize。

提示词层同步收口：`AUTONOMOUS_EXECUTION_GUIDANCE` 新增 `## Ending a turn`；`REPORT_SECTIONING` 的收尾段限定为「只写已验证的事实与剩余风险」；`USER_LABEL_REPLY_GUIDANCE` 加「选项只用于用户才能做的决定，不要把『要不要继续』做成二选一菜单」。

### 1b. 身份槽是互斥的：SOUL.md 对阿黎无效

`prompt_assembly::build_stable_layer` 的身份槽二选一：

```
let mut builder = match named_profile.as_deref() {
    Some(profile) => SystemPromptBuilder::new().with_personality(Some(profile)),
    None           => SystemPromptBuilder::new().with_personality(soul.as_deref()),
};
```

`twin:ali` 会话在 `ws/inbound.rs` 固定注入 `personality = Some("ali")` → 走 `Some(profile)` 分支 → **`~/.hermix/SOUL.md` 完全不进入阿黎的提示词**。所以「改 SOUL.md 让阿黎别再停下来问」是无效的，纪律必须写进真正被读到的那一层：

- `BUILTIN_PERSONALITY_ALI`（身份槽正文）：「汇报风格」原为「再说做了什么、**还差什么**」——这句直接生产出「## 尚未完成」的收尾结构；改为「证据是什么」，另加「一次做完」「只有花钱/删数据/对外发送/不可逆二选一才停下来问」「禁止用『要我接着往下推，还是这轮先到这？』收尾」。
- `builtin-skills/hermix/twin/SKILL.md` 的 hard rule 2「Silence is a valid answer」补一句「这说的是**不必要的话**，不是**没做完的事**」。
- `SOUL.md` 仍然要改：它管的是「没有 named personality」的会话（默认 Hermix 身份）。但**不要**用「删掉 `~/.hermix/SOUL.md` 让程序重新种盘」来解决阿黎的问题 —— 对阿黎无效。

其余 named personality（`coder` / `writer` / `analyst` / `fengpo` / `duzi`）同样不读 SOUL.md，但它们是风格人格，没有「还差什么 / 要不要继续」这类措辞；目标驱动的共性条款放在每轮注入的 `directions.rs` 层即可，不必逐个人格复制。


### 2. 停止是四个真断点，不是一个标志位

- **工具执行期**：`tool_executor` 全文此前 0 处 interrupt，工具只被 `tokio::time::timeout` 界住。现在串行（状态变更 / interactive）与并行（JoinSet 收集循环）两条路都和 `wait_for_interrupt()` 竞速：串行分支 `abort()` 后返回取消信封，并行分支 `abort_all()` 并把未出结果的槽位填成取消信封（不能留空 —— 缺结果就是 dangling tool_call，provider 直接拒）。
- **`Notify` 单 permit 竞争**：server 每个 send 一个 `Arc<Notify>`，`notify_one()` 只唤醒一个 waiter，而消费方有两个（停止转发任务 + 审批等待 select）。审批先醒会把它消费成 `ChannelClosed`，转发任务永不唤醒 → `InterruptController` 从未置位 → 这一轮照旧跑完。改成 `notify_waiters()` + 补一个 `notify_one()`（给此刻还没开始等的消费者）。同理 `InterruptController::interrupt` 自身从 `notify_one` 改 `notify_waiters`（LLM race / 工具 select / 流式 loop 现在有多个并发 waiter）。
- **流式静默期**：`stream_collect` 只在 chunk 到达后才 `take_interrupt_graceful`，provider 不发数据时点停止最坏要等满 `stream_idle_timeout_secs`（120s）。两个 collect loop 的 `select!` 都加上 interrupt 臂（`biased` 在最前，停止优先）。
- **子代理**：`delegate_task` 的父层裸 `await orch.execute_async`，唯一出口是子代理自己的 timeout。改成父 interrupt 与 timeout 竞速，命中即 `child_interrupt.interrupt()` + `abort()`。

前端三处「按钮不在 / 点了没反应」：`Composer` 的 `showStopButton` 去掉 `!canQueue`（用户一打字停止按钮就消失）；首轮欢迎页分支硬编码 `isStreaming={false}`；`cancel` 的 stop 确认超时从 3s 提到 15s（工具 abort 后还有 finalize：持久化、memory review、终态帧，3s 会提前把界面翻回「可发送」而后端仍在收尾）。

## Alternatives considered

- **等到目标完成才允许任何文本输出**（拒绝一切非工具轮）：会把正常的「阶段性交代」也压掉，且纯问答永远出不来。否决。
- **靠 `max_turns` 兜底、不加 `MAX_GOAL_CONTINUATIONS`**：`max_turns` 默认 0（不限），磨蹭型任务会一直「调一个工具 → 写一段 → 被推回」。否决。
- **把 `Notify` 换成 `CancellationToken`**：语义更正确，但要改 transport 消息与 server 全部构造点；`notify_waiters` + flag 已能达到同效果（`wait_for_interrupt` 先查 flag）。留作后续。
- **SOUL.md 做版本迁移覆盖已种盘文件**：`~/.hermix/SOUL.md` 是用户可改的身份层，按内容特征覆写会毁掉自定义。改为只更新内置模板（新装生效），存量用户靠机制层 + `directions.rs` 每轮注入。
- **流式轮次之外收到 Stop 用 registry 兜底 interrupt**：当前实现是显式 no-op，语义正确（确实没有在飞任务）。先不做，YAGNI。

## Consequences

- 长任务不再需要用户打「继续」；代价是每次收尾多一轮 LLM 往返（nudge 驱动的自检）。推过仍不干活即停，所以最多多一轮。
- 停止在工具执行中生效：工具会被 abort，`[状态未知]` 前缀提示模型副作用可能已发生 —— 同步工具（`terminal` 的阻塞调用）abort 只是不再等待，其线程仍可能跑完，这是不可消除的约束。
- 停止会中断子代理（`SubAgentError::Cancelled`），父 turn 立即返回而不是等子代理超时。
- 存量用户的 `~/.hermix/SOUL.md` 仍是旧文案（含「要我接着处理吗？」正例）；它只影响「没有 named personality」的会话。阿黎的纪律写在 `BUILTIN_PERSONALITY_ALI` 里，随二进制走，不需要用户操作。
- 身份槽内容（`BUILTIN_PERSONALITY_ALI` / SOUL.md）**不在** stable layer 的 cache key 里（key 只有 personality slug），所以改完要重启进程才生效。
- 基线：`cargo check --workspace --all-targets` 零错误零警告；`hermix-agent --lib` 1220 passed / 0 failed（新增 `goal_continuation_fires_after_tools_were_used`、`goal_continuation_stops_after_one_nudge_without_progress`，重写三个旧 nudge 断言）；`tsc --noEmit` exit 0；`check-design-guard` clean。
