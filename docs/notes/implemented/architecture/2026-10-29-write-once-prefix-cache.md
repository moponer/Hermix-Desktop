# Decision Note: 发过即冻结（write-once prefix）——把 provider 前缀缓存命中率从 77% 拉回来

Status: implemented — 2026-10-29 交付

## Decision

**已经进入过请求体的内容，此后永不改写。** 上下文瘦身只允许两个入口：① 插入时定型
（工具结果预算、thinking 剥离、hook 注入）；② 显式压缩事件（本来就付一次全量失效）。
除此之外，任何"按距尾部距离"之类的滑窗式回溯改写一律禁止。

### 问题（实测，2026-10-29 本机 `~/.hermix/data/sessions.db`）

| 口径 | 调用数 | prompt | 命中 | 命中率 |
|---|---|---|---|---|
| 近 30 天 | 299 | 36,020,301 | 27,685,475 | **76.9%** |
| 单会话 `72b180fa…` | 60 | 5,182,750 | 3,999,025 | 77.2% |

逐次比对 `cache_read[n] − prompt[n-1]`：60 次调用里只有 2 次等于 0（理想增量命中），
其余每次少付 **9K~43K token**。命中率被钉死的机理是代数式的：
`命中率 = 1 − 窗口/prompt`，窗口是固定的「尾部 N 条消息」，prompt 越长比值越稳，
所以不管聊多久都停在 77%。

两个回溯改写点：

1. **每轮 rollup**（`run_loop/tool_turn.rs::do_tool_turn` 原第 347 行）：
   `rollup_tool_results(ctx, 30, 8_000)` 判据是 `dist_from_tail > 30`。每新增 2 条
   消息就有 1 条**已发过**的工具结果被压成一行 `[ALREADY EXECUTED]`，该位置之后的
   整个尾部（实测 60,005 字符 ≈ 25~35K token）重新计费。该会话 rollup 归档 405KB、
   2~4 行/秒持续触发。
2. **thinking 回传**（`prompt_assembly::messages_for_api_call`）：
   `reasoning_content` 只保留"最后一条 assistant"。第 N 次请求带 thinking 发出去，
   第 N+1 次它已不是最后一条 → 被清空 → 同位置字节变化。该会话 60 条 assistant 的
   thinking 合计 **189,733 字符**（单条最高 10,590）。

### 修法

| 位置 | 改动 |
|---|---|
| `run_loop/tool_turn.rs` | 删除每轮 `rollup_tool_results`（保留压缩路径 `compression_ops.rs:552` 那次） |
| `prompt_assembly::messages_for_api_call` | 所有消息的 `reasoning_content` 一律置 `None`（发送期投影，`ctx` 与持久化不动） |
| `prompt_assembly::observe_prefix_reuse` | 新增：按 session 保存上一次 payload 的逐条指纹，每次调用 log `shared_msgs / rewritten / predicted_hit_pct` |
| `run_loop/pre_turn.rs` | skills 块、intent prefetch 块在近 8 条内已存在则跳过（同一 turn 多轮循环不再重复追加） |
| `hook_dispatch::inject_hook_context` | 不再往"最后一条 user 消息"append（那是在改已发送字节），改为在尾部 push 新 system 消息，并对相同文本去重 |

thinking 选择"全丢"而不是"全留"：两者都可做到字节稳定，但全留会把 19 万字符
thinking 塞进每一次请求（更快触发压缩，而每次压缩都是一次全量缓存失效）。

## Alternatives considered

- **保留"最后一条 thinking"、只把清理动作提前到入栈**：不成立。一条消息必然会先被
  发出去一次（它是"最后一条"），下一次被清理时仍是一次回溯改写，只是把破坏点从
  第 N 位挪到第 N-1 位。
- **rollup 改成"每个 turn 只推进一次"**：破坏次数减到 1/turn，但每次仍丢整个尾部窗口，
  长任务里 turn 数与调用数同量级，收益不足。
- **完全不做 rollup、靠压缩兜底**（采纳）：压缩是显式事件、频率低一个数量级，且本就要
  重写历史；代价是压缩触发比原来早。
- **把图片 base64 也做"发过即冻结"**：做不到——图片必须原样送达一次才能被模型看到，
  之后必然有一次变化。现状 `strip_previous_image_urls` 保留（一次性代价），不属本次范围。
- **改 provider 侧**（Anthropic `cache_control` 断点、MiniMax 自动缓存调参）：前提是
  我们自己的 payload 先稳定；否则断点打在哪里都没用。

## Consequences

- 单次调用的未命中部分 ≈ 本轮新增内容（assistant 正文 + tool_calls + 工具结果），
  实测级长会话预期落到 **95%~97%**；`>98%` 需要"每轮新增 ≤ prompt 的 2%"，
  即工具输出本身要更小（预算 16K 字符/条已是 2026-10-27 收紧后的值）。
- 首轮永远不可能命中，短会话（prompt < 50K）的命中率天然低，看总量时要把这两点扣掉。
- 上下文压力增长变快 → 压缩触发更早。压缩仍会重写历史（一次全量失效），这是
  唯一允许的例外。
- 观测口径：`server.log` 的 `prompt_cache: payload prefix reuse` 每调用一条，
  `rewritten=false` 表示零回溯；`predicted_hit_pct` 是**消息体**的可复用比例
  （不含 native tools schema），比 provider 报的数略高，趋势对齐即可。
- 回归锁：`prompt_assembly::tests::payload_never_rewrites_already_sent_rows`
  跑 6 轮工具循环，逐条比对相邻两次 payload 的已发送部分，任何回溯改写即红。
