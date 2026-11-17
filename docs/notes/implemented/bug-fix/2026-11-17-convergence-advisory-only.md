# Decision Note: 收敛检测不再终止 run，只注入诊断

Status: implemented — 三个建议性信号改为可重复注入的诊断，只有 ErrorLoop 仍可硬停

## Decision

`convergence_detector.rs` 里只有 **ErrorLoop**（连续 N 轮工具调用全部失败）能终止
run。其余三个信号——`NoProgressWithTools`（连续静默）、累计静默轮数、`SimilarContent`
（重复输出）——一律返回 `ConvergenceSignal::Warn`，由 `run_loop/tool_turn.rs` 注入一条
system 诊断并继续跑；同时 `emit_status("lifecycle", …)` 给用户一行可见说明。

配套的三条改动：

1. **删掉三个 `*_hard_stop_turns` 配置字段**（`max_no_progress_hard_stop_turns`、
   `max_silent_tool_hard_stop_turns`、`max_similar_content_hard_stop_turns`）。留着它们
   等于把「要不要停」做成可配项，而结论是**不该停**，可配项只会被重新打开。
2. **诊断按倍数重复**：`no_progress_reported_at` / `silent_tool_reported_at` /
   `similar_content_reported_at` 记录「已报告到第几个阈值倍数」，而不是一次性布尔。
   一次 run 静默 60 轮要说两次，不是一次。
3. **`Warn` 接上消费者**。此前 `run_loop` 里是 `_ => {}`——检测器算出诊断、
   日志里没有、模型不知道、用户也看不到，等于该信号从初始快照起就不存在。

## Alternatives considered

- **提高阈值**（10 → 15/20/25）。模块自己的文档已经否掉这条路：
  「Raising the threshold to 10 wasn't enough」。阈值只是把失败推后，代价是每次
  失败前多烧十几轮完整 prompt。
- **保留硬停但只放宽到某个「安全值」**。没有安全值可言：2026-11-17 那次 run 的
  18 轮里 `context_pct` 始终 16%（1048576 上限），上下文根本没用完，真正的损失是
  延迟——`api_ms` 从 47 s 涨到 163 s、`tool_ms` 从 1.4 s 涨到 828 s。硬停发生在
  run **正在收敛**的时候，砍掉的是接近完成的活。
- **只把硬停改成「先问用户」**。用户已经在截图里给出答案了：连续出现「继续」
  按钮，每次都要人推一下。把决策权交回用户等于把同一个问题变成手动操作。
- **让 `ProgressLedger` 取代整个收敛检测器**。方向对，但 `ProgressLedger` 依赖
  `tool_state::signature` 的世界状态指纹，只覆盖文件/shell/浏览器三类工具族；
  其余工具返回 `None`（「宁可少判，不可误判」），于是整批无签名时它什么也看不到。
  两者判据不同（一个是世界状态、一个是对话形状），并存比替换更稳。

## Consequences

- 坏循环不再被自动切断。兜底只剩：`ErrorLoop`（全错）、`RepeatGuard`（工具结果
  逐字重复 → 停用工具）、成本护栏、用户手动停止。这是有意的取舍：**沉默地跑偏
  比停下来更便宜**，因为停下来会丢掉已经做完的部分，而跑偏最多多花几轮 token。
- 三个配置字段从 `ConvergenceConfig` 消失。任何外部 yaml/环境变量试图设置它们
  都会变成未知键——目前没有读取方，因此无迁移成本。
- `convergence_wrap_up` 现在只有 `ErrorLoop` 会走到；`NoProgressWithTools` /
  `SimilarContent` 分支保留是为了枚举完整性，不是活路径。
- 用户可见行为变化：连续静默时会看到一行「已经连续 N 轮没有汇报了，提醒它先说清
  进展再继续（任务没有被中断）」，而不再是几十分钟安静之后任务被砍。

## 为什么这不是「删掉保护」

保护的目的从来不是「让 run 停下来」，而是「不要让 run 白烧」。这两件事在实现上
分开了：白烧由 `RepeatGuard`（结果逐字重复）和成本护栏处理，而「模型不知道该
换哪条路」只能靠告诉它——它持有完整对话，是唯一能区分「长读取链」和「空转」的
一方。`progress_ledger.rs` 的模块文档早就写了这个结论
（「命中后的反应是**注入诊断，不停止 run**」），本笔记只是把旧的那套按同一原则
对齐，并让信号真正送达。
