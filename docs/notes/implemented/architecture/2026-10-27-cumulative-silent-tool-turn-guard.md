# Decision Note: 累计「有工具无正文」守卫——补上交替空转的缺口

Status: implemented — `ConvergenceDetector` 新增累计沉默工具轮计数，警告 10 / 硬停 18

## Decision

起因是一次完整请求实测（2026-10-27 00:44，`agent.log` 的 `turn report` 序列）：模型从
turn=19 起陷入 `terminal ↔ write_file` 的严格交替循环——`terminal` 轮次 `has_visible_text=false`
（completion 只有 85~90 token，全是工具调用参数），`write_file` 轮次有一句"写入结果"的说明。
整个 run 跑到 turn=29 被下一轮打断，期间 13 轮无正文、prompt 从 64K 涨到 81K，用户等满全程
却没有任何面向人的进度。

这条循环**穿透了所有既有守卫**：

| 守卫 | 为什么没拦住 |
|---|---|
| `NoProgressWithTools`（连续 15/25） | 判据是**连续**计数；`write_file` 那半句正文每轮把计数清零，交替模式连续值永远 ≤1 |
| `RepeatGuard`（连续 2 批重复结果） | 指纹看**结果正文**；`terminal` 每次观察的对象在变，结果逐字不同 |
| `ErrorLoop`（连续 3 轮全错） | 工具都成功 |

据此给 `ConvergenceDetector` 增加一个**累计**维度的计数：

- `silent_tool_turns_total`：整个 run 里「有工具调用、但无可见正文」的轮次总数。
  **不因有正文的轮次而重置**——因为 `write_file` 那一句"写入文件"不是面向用户的进度汇报，
  它不该抹掉 `terminal` 的沉默。
- 累计到 `max_silent_tool_turns`（默认 10）→ 发一次 advisory 警告（`NoProgressWithTools`），
  让模型停下来给用户总结。
- 累计到 `max_silent_tool_hard_stop_turns`（默认 18）→ 硬停（`Converge`），与连续 no-progress
  的 25 轮硬停同源，只是累计维度更早兜底。

连续计数保留原样，两个维度互补：连续空转由 `no_progress_count` 拦（15 警告 / 25 硬停），
交替空转由 `silent_tool_turns_total` 拦（10 警告 / 18 硬停）。两者同用一个
`ConvergenceReason::NoProgressWithTools`，语义本来就是「有工具、无正文」。

## Alternatives considered

- **降低连续 `max_no_progress_turns` 阈值** — 否决。交替模式连阈值的一半都到不了（连续值恒
  ≤1），降多少都没用；且 10+ 连续工具轮是合法长工具链（search → read 20 个文件 → 总结），
  团队已明确"不能杀"。
- **滑动窗口比例**（最近 N 轮里无正文占比 > 阈值）— 否决。案例是 50% 交替，要拦它阈值必须
  ≤50%，而"正常工具链偶尔无正文"也常在 30~50%，误伤面大且引入环形缓冲复杂度，违背简化纪律。
- **让累计计数在有正文时衰减（而非不重置）** — 否决。纯交替 +1/-1 净增长为零，永远拦不住；
  且"有正文"≠"给了用户反馈"，衰减会重蹈连续计数的覆辙。
- **扩展现有 `RepeatGuard` 加"工具名模式重复"指纹** — 否决。案例里 `terminal` 单工具轮与
  `write_file` 单工具轮交替，工具名集合也不连续重复；且维护"模式"比维护"结果"更难定义边界。
- **只发警告、不硬停** — 部分采纳。硬停阈值 18 刻意高于警告 10，给模型 8 轮机会开始汇报；
  与"只有 ErrorLoop 才硬停"的旧哲学不同，但延续了 no-progress 25 轮硬停的既有先例（LLM 会
  persist 而非 self-correct）。

## Consequences

- 「`terminal` 观察 → `write_file` 记录」这类交替空转，在第 10 轮沉默工具轮时收到一条
  "别再观察不汇报，给用户总结"的警告；无视警告继续到 18 轮则硬停，不再一路烧到 `max_turns`。
- 累计计数**只增不减**，是刻意的：它度量的是"整个 run 有多少轮没对用户说话"，这正是
  `turn report` 里 `has_visible_text=false` 的累计口径。代价是超长会话里迟早会碰到 10 的警告
  线——但一个正常会话本就不该有 10 轮纯工具零正文，这条警告是提醒而非误杀；硬停 18 更难碰到。
- 现有连续 no-progress 的行为与测试不变（只新增字段，未改 `no_progress_count` 语义）；唯一
  更新的测试是 `test_no_progress_hard_stop_disabled_when_zero`——它循环 20 次沉默工具轮，
  现在会触达累计硬停，故显式 `max_silent_tool_hard_stop_turns: 0` 以隔离它被测的连续路径。
- 阈值（10/18）是经验值，非推导值。若将来批量操作（Operate 域）出现"多轮工具零正文"的合法
  误伤，优先在 `ConvergenceConfig::for_domain` 里按域放宽，而不是动全局默认值。
