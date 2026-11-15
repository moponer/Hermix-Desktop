# Decision Note: 工具结果进上下文的形态 —— 分档上限、分页对齐、高水位回收

Status: implemented — 2026-10-30

## Decision

**工具结果必须进上下文，但进的是「这一次决策需要的视图」，不是「工具的完整产出」。**
三条落地：

| 位置 | 改动 |
|---|---|
| `hermix-tools/src/result_budget.rs` | 新增 `model_visible_cap(tool)`：每条结果进上下文前的字符上限**按工具分档**（terminal/read 16 K、search 4 K、list 3 K、元工具 6 K、子代理 12 K、默认 16 K） |
| `hermix-agent/src/budget.rs` | `enforce_budget_with_spill` 接受 `tool_calls`（用于 `tool_call_id → 工具名`），按分档上限裁剪；落盘阈值 = 本工具上限；落盘过的结果跳过裁剪（否则指针被切掉）；聚合裁剪也先落盘再裁 |
| `hermix-agent/src/tool_result_reclaim.rs` | 新增高水位回收：历史里工具结果合计 > 150 K 字符时，把最旧的一批换成一行占位符，保留最近 40 条原文 |

配套两处修正：

- `file.rs` 的 `read_file` 页大小从 `READ_FILE_CHARS`（80 K）改为 **`model_visible_cap("read_file") − 512`**。
- `terminal.rs` 的 `spill_or_truncate` 内联预览从 `SPILL_INLINE_CHARS`（20 K）改为同一档上限。
- `BudgetConfig::max_aggregate_chars` 默认 120 K → **48 K**。

## 为什么（实测，`~/.hermix/data/sessions.db`，会话 `efd064e9…`）

那次请求 880,670 字符 / 222,209 token 的构成：

| 组成 | 字符 | 占比 |
|---|---|---|
| 工具结果（209 条） | 339,808 | 38.6% |
| 工具调用参数（write_file 40 次 228,329） | 334,879 | 38.0% |
| 每轮注入的 system 块 | ~123,067 | 14.0% |
| system prompt | 59,995 | 6.8% |
| assistant 正文 | 22,773 | 2.6% |

工具结果内部：`terminal` 98 次 167,557、`read_file` 32 次 54,509、`search_files` 13 次
49,366（均值 3.8 K）、**`list_files` 5 次 43,572（均值 8.7 K）**。

两个必须修的缺陷：

1. **`list_files` / `search_files` 这类输出可再生的工具，和 `terminal` 共用一个上限。**
   一次目录列举 8.7 K 字符进上下文并被此后每轮重发，而重跑一次的成本几乎为零。
2. **`read_file` 的分页提示与实际投递量不一致 —— 这是静默丢数据，不是省 token。**
   工具按 80 K 分页并给出 `next_offset`，agent 层再在 16 K 处做中间截断：模型拿到
   「前 8 K + 后 8 K + 请从 offset=N 续读」，而 N 是按 80 K 算的，中间那 60 多 K
   **再也不会被读到**。

### 开源对照（同一次调研，逐条有源码位置）

| 维度 | Codex | ZCode | hermix 改前 |
|---|---|---|---|
| 单条预算 | 默认 `Bytes(10_000)` ≈ 2.5 K token | 逐工具声明：bash 30 K、grep 20 K、read 256 K + 内部 25 K token 降级 | 所有工具 16 K 字符，无分档 |
| 超限处理 | middle 截断 + marker，无指针 | artifact 落盘 + 预览信封 + 取回路径 | 落盘 + 指针（已对齐） |
| 部分视图 | 只有 marker | `isPartialView` → 后续编辑拒绝 | 已有（`file_read_state`，仅 patch 校验） |
| 旧结果回收 | 压缩时整体丢弃工具输出 | `microcompact` 留最近 5 组、其余占位，默认关闭 | **无** |
| per-turn 聚合 | 无 | 无 | 有字段（120 K），实际等于没有 |
| 调用参数 | 原样保留 | 原样保留 | 原样保留（228 K 来自 `write_file` 全文） |

结论：两家都把工具参数原样保留，所以 `write_file` 的 228 K 不是「我们做错了」，
而是「让模型用全文写入而不是增量补丁」的后果 —— 本次不动参数，改由 patch 优先
（后续）。

## Alternatives considered

- **按距尾部条数做滑窗回收**（ZCode microcompact 的形状）：滑窗会让**每次**请求都
  改写一条已发过的结果 → 前缀缓存永久为零。2026-10-29 拆掉的 `rollup_tool_results`
  正是这个形状（命中率被钉死在 77%）。因此回收只在 run 开始做、且高水位批量触发。
- **截断工具调用参数**：Codex / ZCode 都不做。截断点只匹配输出类型，参数一律原样
  保留。抄了它就是**偏离两家**且会让模型回看自己写的内容时要重新 read_file；
  真正该改的是让 `write_file` 不再是 40 次/会话的首选。
- **把 `read_file` 的 `READ_FILE_CHARS` 直接改成 16 K**：会连带把注册表的全局上限
  （`DEFAULT_MAX_RESULT_CHARS = READ_FILE_CHARS`）压到 16 K，破坏「注册表是外层安全网、
  不能先于工具自己的上限触发」这条不变量。改为新增一张 `model_visible_cap` 表。
- **聚合裁剪保持纯截断**：会把中间段丢成不可取回（注册表注释里抱怨的正是这种丢法），
  改为先落盘再裁。

## Consequences

- 单次工具输出对上下文的贡献从「最多 16 K 字符 / 任意工具」降到**按信息量分档**，
  `list_files` 一类从 8.7 K 降到 3 K，`read_file` 不再出现「提示跳过了正文」。
- 落盘比改前更频繁（阈值从固定 20 K 变成各档上限，最小 3 K）：spill 目录有条数
  上限（200，按 mtime 淘汰），且只在结果真的超过本档上限时才写。
- 回收是一次**全量缓存失效**。门槛 150 K 字符意味着只有严重膨胀的会话会付这一次；
  回收后被清的行带 `[old tool result cleared…]` 前缀，重复调用幂等，不会每轮反复失效。
  代价是这些历史输出被持久化成占位符（与 ZCode 的 microcompact 同性质），需要原文
  时重跑该调用。
- `BudgetConfig::max_aggregate_chars` 48 K：一轮 3 条 16 K 不裁，第 4 条起按比例压，
  且压之前落盘，模型拿得到取回路径。
- 回归锁：`tool_result_reclaim::tests`（水位 no-op / 保留最近 N 条 / 二次调用幂等）；
  `budget::tests` 覆盖分档与聚合路径。
- 未处理：`write_file` 全量参数（228 K，占请求 26%）—— 对策是 patch 优先，需要
  单独一次改动与实测。
