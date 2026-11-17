# Decision Note: 长任务与 coding 的三处硬约束修正

Status: implemented — 子代理轮次/超时放宽并可覆盖、图片 base64 全路径剥离、读写比收敛规则写入提示

## Decision

三处独立修正，共同点是**它们都不是模型能力问题，而是脚手架把模型卡住了**。

### 1. 子代理：轮次 12 → 40，超时 180 s → 600 s，且可按请求覆盖

`SUB_AGENT_MAX_TURNS_CAP` 从 12 提到 40。这个上限此前与
`DEFAULT_DELEGATION_MAX_ITERATIONS = 50` **直接矛盾**：文档写的默认迭代预算是 50，
而 `.clamp(SUB_AGENT_MIN_TURNS, SUB_AGENT_MAX_TURNS_CAP)` 把每个请求都压到 12。
一个"读文件 → 改文件 → 跑验证"的子任务要 6–10 轮，跨两个文件就超了。

`DEFAULT_SUB_AGENT_TIMEOUT_SECS` 从 180 提到 600。2026-11-10 把它从 600 收到 180
的理由是"子代理只做短任务"，但实测每轮 provider 延迟 30–130 s（`agent.log` 的
`turn report`），12 轮根本装不进 180 s。2026-11-17 两次 `delegate_task` 全部
`reason=timeout`，就是这个预算算错的结果——子代理被砍在半路，父 agent 拿不到任何
部分成果。

超时同时改成**可按请求覆盖**（`SubAgentRequest::timeout_secs`，由
`delegate_task` 的 `timeout_secs` 参数传入）。理由：2 轮查证和 10 轮改码不该共用一个
预算，而知道任务大小的是调用方（父 agent），不是 crate 常量。

### 2. 图片 base64：剥离改为按 JSON 形状判定，不再按工具名

`push_tool_result` 原本只在 `tool_name == Some("read_image")` 时调用
`strip_image_base64`。而 `tool_name` 是从 `tool_calls` 里按 id 查出来的，
**重放批次、恢复的会话、重试的调用**里这个 id 可能不存在 → `None` → 不剥离 →
20–80 KB base64 直接进上下文，并在之后每一轮重新计费。

改成无条件调用 `strip_image_base64`。该函数本来就按 JSON 形状判定
（`_image_meta._image_data_b64` / `_image_data`），非图片内容原样返回，所以对
所有工具结果都安全。

子代理那条"保留原文供 UI 预览"的例外同时取消：UI 预览走 attachment，不依赖这段
文本；隔离边界决定的是"会不会泄漏到父上下文"，不是"同一个上下文里要不要付两次钱"。

### 3. 读写比：把"读够了就动手"写成规则

`TOOL_USAGE_GUIDANCE` 的 §1–§6 全是关于怎么**读**（orient、baseline、locate），
没有任何一条说读到哪里该停。实测读写比是 `read_file` 205 次 : `edit_file` 36 次
≈ 5.7:1——模型把轮次花在"看"上。

§3 增加三条：读是成本不是进展；知道改哪个函数、改什么之后立刻动手，剩下的交给
编译器和测试（它们更便宜也更诚实）；搜索连续三次无结果就换方法，不要细化同一个查询。

## Alternatives considered

- **只提高超时、不动轮次上限**：12 轮的上限会让"跨两个文件"的活必然失败，加时间只是
  让它更久地失败。两者必须一起改。
- **保留 180 s，改让子代理更快**：延迟是 provider 侧的，本地改不了。
- **给子代理加"超时前先返回部分结果"**：更复杂，且部分结果对父 agent 的用处远小于
  让子代理做完。
- **把 base64 留在子代理上下文（原设计）**：理由写的是"隔离边界、双份成本有界"。但
  同一张图在同一上下文里存两遍没有任何收益——模型只能看 `content_parts` 那份，
  文本那份它读不了。
- **只靠提示词让模型少读**：已经有一整节 `TOOL_USAGE_GUIDANCE` 讲怎么读，仍然
  5.7:1。所以这次加的是**可判定的停止条件**（"知道改哪了就动手"），不是再加一句
  "要高效"。
- **用 `max_turns` 逼它收敛**：轮次上限会砍掉合法长任务，而真正的问题是读写比。
  已在 2026-11-17 的收敛检测改动中确认 `max_turns` 从未触发过。

## Consequences

- 子代理可以跑 40 轮 / 10 分钟。坏处：一个卡住的子代理会占用更久的槽位。兜底是父
  interrupt 竞速（已有）与 `delegate_task` 的 `timeout_secs` 参数。
- 放宽后 `SUB_AGENT_MAX_TURNS_CAP` 与 `DEFAULT_DELEGATION_MAX_ITERATIONS` 不再矛盾
  （40 vs 50 仍然存在截断，但 50 是"默认请求值"、40 是"上限"，语义分层合理）。
- 图片 base64 现在在任何路径下都不进上下文。代价：`read_image` 的文本结果里
  `_image_data` 恒为占位符——依赖该字段做 base64 解析的旧调用方会拿到占位符。
  仓库内唯一这类调用方是测试，已更新为断言剥离后的形状。
- 读写比规则是提示词层面的，**不保证**生效。真正的验证是下一段 coding 会话里
  `edit_file` 的占比是否上升——这是后续要观测的指标，不是本次能声称的结果。
