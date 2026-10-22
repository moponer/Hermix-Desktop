# Decision Note: Token 消耗结构分析与缓存命中率优化方案

Status: proposed — 2026-10-21 完成全量盘点 + provider 级对照实验；**2026-10-22 复核后重心更正**（见「事实 6」：真正的头号浪费不是 prompt 层，而是会话历史无界累积）。方案待拍板后实施

## ⚠️ 复核更正（2026-10-22）

本篇初稿把「prompt 层的冗余与 churn」当成主项。复核后**这个重心是错的**：

- 初稿写「单次请求真实输入 ≈ 43K token / 每章 ≈ 320K」—— **错了**，那是拿
  `prompt_cache` 日志里 `chars÷4` 的估算值当成真实 token。实测 provider 报的
  单次请求是 **~1M token**，比系统提示大两个数量级。
- 真正的构成：**会话历史占了 99%**，系统提示只占 1%。所以 P1 档（prompt 瘦身）
  最多省 1%，**降级为锦上添花**；头号措施换成「有界历史窗口」（已实施，见下）。

下面「事实 6」是复核时补的，「Decision」一节已按新重心重排。

## 事实 6（复核补，最重要）：会话历史无界累积，占请求的 99%

### 6.1 provider 的 `prompt_tokens` 是「单次请求大小」，不是累计计数

初稿怀疑它是累计计数器（会话第 7 分钟就报 2.8M）。实测排除：

| 请求 | system 字符 | provider 报的 `prompt_tokens` |
|---|---|---|
| 1 | 12K | 3,816 |
| 2 | 24K | 7,416 |
| 3 | 36K | 11,016 |

随 system 线性变化（比值 2.89 ≈ 设计值 3.0）→ **单次请求大小**。
于是生产里的 `prompt_tokens = 8,027,881` 是**真的**：那一次请求带了 800 万 token。

### 6.2 请求里 99% 是累积的会话历史

该会话（41 轮）的 `messages` 表：

| 列 | 字符 | 是否进 LLM 请求 |
|---|---|---|
| `content` | 561,460 | ✅ |
| `tool_calls`（模型自己发起的工具调用的 JSON） | 2,051,896 | ✅ |
| `extra`（自动化投递元数据：`automation_name` + 完整提示词） | 3,525,402 | ❌（`load_session` 过滤 `role='automation'`） |
| 系统提示 | 68,796 | ✅ |
| 工具 schema | 31,381 | ✅ |

模型窗口（`minimax/minimax-m3.1-flash:free`）= **1,000,000 token**，实测请求 ~1M → **顶满整个窗口**。
系统提示在其中占 **1%**。

### 6.3 为什么压缩一直没救场

压缩阈值 = `(max_context − output_reserve) × 0.8` ≈ **380K token**。
压缩器是接了的（`local_agent_service.rs:1165`），`archive_context_section` 也无条件执行
—— 但它只把中间内容**写到磁盘**（`ctx.get_messages()` 是不可变引用，**并不缩短活上下文**），
真正会缩短的 LLM 压缩器要等到逼近阈值才动手。

结果：**前 40 轮历史无界增长，第 41 轮才刚够到阈值**。压缩在这个任务上等于没有。

### 6.4 「89.21% 命中率」的真实含义

它是「**一个巨大的请求**里 89.21% 被缓存了」，不是「小请求被缓存了 89%」。
新鲜部分 ~10.8% ≈ **800K token/轮** —— 这才是每轮真正在付的钱。

## Decision（按复核后的重心重排）

**先做「有界历史窗口」（已实施），再做「让字节稳定」，最后才是 prompt 层瘦身。**

核心判断（provider 级对照实验，见下节）：**缓存是块级的，只有变了的字节重新计费。
所以提命中率靠「让字节别变」，不靠「把 prompt 变小」；变小是为了省绝对量。**
而绝对量的 99% 在会话历史里 → **先把历史管住，再谈别的。**

### P0-新（头号，已实施）：自动化每轮的有界历史窗口

- 新增 `AgentOverrides::history_token_budget`（`hermix-core`）+ `history_window` 模块
  （`hermix-agent`）：按**估算 token**（CJK 感知口径，不是 `chars/4`）从尾部保留，
  切点不落在 tool 结果上、末尾悬空的 tool_call 会被丢掉（两者都会让上游拒收）。
- 两个自动化引擎（JSON 引擎 `dashboard/automation.rs`、SQLite 引擎
  `automation_trigger.rs`）都设 `60,000` token 预算 ≈ 最近 3~4 轮。
- 磁盘上的会话**照旧完整**（UI 时间线 / 归档 / 检索不受影响），只有「送进模型
  的那一份」有界。
- 预期：单次请求 ~1M → **≤100K token**（~10x），每轮 ~7M → **≤700K**。

### P0：让字节稳定（命中率 → 98%+）


对 `https://api.kilo.ai/api/gateway`（`minimax/minimax-m3.1-flash:free`）直接发请求，
唯一变量是「请求体里哪一段的字节变了」：

| 实验 | 命中率 | 相对基线损失 |
|---|---|---|
| 完全相同的请求 | 100.0% | — |
| system 尾部时间戳变（~12 token） | 99.7% | **仅 12 token** |
| 剥离**靠前**那条 assistant 的 `reasoning_content` | **97.5%** | **833 token** |
| 剥离**最后**那条 assistant 的 `reasoning_content` | 99.9% | 297 token |

**结论三条**：

1. **provider 的前缀缓存是块级的**：只有「变了的字节」重新计费，尾部改 12 token 就只损失
   12 token 命中 —— **不是**整个前缀作废。
2. **从某条消息开始改字节 → 从那里往后全部变成新鲜 token**。位置决定代价。
3. 因此优化方向不是「把 prompt 变小」，而是 **「让字节别变」**；把 prompt 变小是为了省
   绝对量，不是为了提命中率。

## 二、问题定位

### P0：让字节变来变去（直接压命中率）

| # | 位置 | 量级 | 为什么是浪费 | 证据 |
|---|---|---|---|---|
| P0-1 | `prompt_assembly.rs:442-454` | 每调用改写一条历史 assistant 消息 | **追溯性剥离** `reasoning_content`：保留「最后一条带 reasoning 的」，把它之前的全置 None。「最后一条」每轮前移 → 上一条被**就地**改写 → 从该位置往后全部失去缓存 | 实验 [B]：-833 token |
| P0-2 | `hermix-tools/src/registry.rs:145` + `:211` | 109 个工具的顺序 | `self.tools.values()` 直接吐 HashMap 迭代序 → **跨进程不稳定** | 实测：重启后 109 个工具里 **107 个位置变了**（集合相同、纯顺序差异） |
| P0-3 | `agent_loop/runtime.rs:246-252` → `agent_loop/config.rs:939` | 每次用户请求重算工具集合 | `route_tools` 的 `TurnContext` 含「最近 6 条消息」→ 集合会变 → **tools 一变，整个前缀作废** | 代码路径 + `tool_router.rs:321` 按分数截断 50 |
| P0-4 | `local_agent_service.rs:1864` / `:2321` | 188 字符 | `session_started_at = now()` 写在**每请求**入口（`send_message` / `send_message_stream`），注释却写着「会话内恒定…避免每 turn 变化打断 provider 前缀缓存」 | 代码与注释自相矛盾；本该冻结它的 `hydrate_stored_system_prompt_from_hermes_home` **无生产调用点** |
| P0-5 | `agent_helpers.rs:212-225` | stable 层 | stable 层 cache key **不含工具集** → 工具集变了仍命中缓存 → 缓存里的 `<tools>` 清单是**旧的**，与实际发出的 tools 数组不一致（正确性 bug） | 代码 |

> P0-4 的代价经实测**只有 12 token/次**（块级缓存），所以它是 P0 里最便宜的一条，
> 但注释与实现相反这件事本身要修 —— 一旦哪天 provider 换成严格前缀缓存（Anthropic 式
> `cache_control` 断点），它会立刻变成最大的坑。

### P1：固定前缀里的冗余（压绝对量）

| # | 位置 | 量级 | 为什么是浪费 |
|---|---|---|---|
| P1-1 | `prompt_assembly.rs:125` `SKILL_METADATA_BUDGET_CHARS` | **20,000 字符**预算 | `<available_skills>` 索引是 context 层最大单项；典型技能目录远用不到这个额度 |
| P1-2 | `directions.rs:706-880` | **10,313 字符**（10 条 always-on） | 其中 `file_lifecycle`(2,342)+`env_shell`(2,002)+`tool_failure`(1,952)=**6,296 字符**是纯行为规约，与本会话有没有这些工具无关，照挂 |
| P1-3 | `directions.rs:626-704` | 首轮 5,497 vs 次轮 1,610 | 首轮比后续多 3,887 字符，且与 short 块语义重复 → 首轮前缀与后续轮**不共享** |
| P1-4 | `prompt_assembly.rs:974-991` vs `directions.rs:370` | 996 + 2,342 | 「临时文件放哪、该删」同一套规则讲两遍 |
| P1-5 | `agent_config.rs:809` | **16,000 字符**预算 | 实测两个 skill 合计 **16,008 字符** —— **正好顶满**；再加一个 skill 就把前一个截断。且全文注入，不管这一轮用不用得上 |
| P1-6 | `registry.rs:145` + `agent_loop/config.rs:968` | 31,381 字符 | 109 个工具的 schema 全量进请求体，但路由后只暴露 ~63 个（top-50 + 13 白名单） |
| P1-7 | `context.rs:71-72` / `memory_prefetch.rs:186` / `prompt_assembly.rs:1127` | 2,200+1,375+4,000 | **三处独立注入**同一类记忆内容 |
| P1-8 | `prompt_assembly.rs:125` vs `skill_loader` | — | skill 索引走 20,000 预算、正文走 16,000 预算，**两个预算互不知情** |

### P2：调用次数（压墙钟）

- 每章 **7.4 次调用**（304 次 / 41 轮）。skill 里已有「6 次调用做完一套题」的最优打法
  （`browser-automation` §3.8），但实测没稳定走到那条路径 —— 大概率是每章按章节类型
  （纯视频 / 视频+测验 / 纯测验）重新决策流程，而不是套固定剧本。

## Decision

**分三档做：P0 让字节稳定（提命中率到 98%+）、P1 砍固定前缀里的冗余（压绝对量）、P2 减少每章调用次数（压墙钟）。先做「可观测地基」，再按 P0 → P1 → P2 推进。**

核心判断（由 provider 级对照实验得出，见上节）：**缓存是块级的，只有变了的字节重新计费。
所以提命中率靠「让字节别变」，不靠「把 prompt 变小」；变小是为了省绝对量。**

### P0 档：让字节稳定（命中率 → 98%+）

| 措施 | 做法 | 预期 |
|---|---|---|
| **M1 剥离改为单调** | `reasoning_content` 的剥离改成「**按本轮起点**切」：只剥离**最后一个 user 消息之前**的 assistant 消息；本轮内产生的 assistant 保留 reasoning | 字节只在一轮内第一次调用变一次 → 从「每调用变」降到「每轮变一次」 |
| **M2 工具顺序确定化** | `registry.schemas()` 与 `names()` 按名字排序后输出；`AlwaysInclude` 追加也按名字排 | 重启不再作废前缀（实测 107/109 位置变化消失） |
| **M3 工具集合稳定化** | 同一会话内对路由结果做**迟滞**（hysteresis）：集合未显著变化就不重算；或路由结果按会话缓存，仅在用户显式换话题时失效 | 集合不变 → tools 不变 → 前缀整体保住 |
| **M4 时间戳冻结** | `session_started_at` 改为**每会话一次**（从 session 行 `created_at` 取，或按 session_id 缓存），并把 `hydrate_stored_system_prompt_from_hermes_home` 真正接上 | 与注释对齐；块级缓存下省 ~12 token/次，但为将来换 provider 兜底 |
| **M5 stable 层 key 纳入工具集** | `compute_prompt_cache_key` 加入工具集哈希 | 修 P0-5 的正确性 bug（清单与实际不一致） |
| **M6 directions 分层** | 把 always-on 的 10,313 字符移进 **stable 层**（它们与 turn 无关），只把真正 per-turn 的（domain.directive / tool-gated / project.conventions / workbench）留在变动区 | 19,628 字符的变动区缩到几百字符 |

### P1 档：固定前缀瘦身（**降级**：绝对量里只占 1%，做完 P0 再看）

| 措施 | 做法 | 省下 |
|---|---|---|
| **M7 skill 索引预算下调** | 20,000 → 4,000（只留名字+一行摘要，命中后再取全文） | ~16,000 字符 |
| **M8 skill 正文按需** | 正文注入改为「命中即注入，但每个 skill 只注入**当前任务相关**的章节」；或索引里带 `when_to_use`，由模型主动 `skill_read` 取 | ~8,000 字符/请求 |
| **M9 skill 索引与正文共用一个预算** | 20,000 与 16,000 合并成一个（索引优先，正文按剩余额度） | 消除互相挤占 |
| **M10 directions 按工具可用性裁剪** | `file_lifecycle`/`env_shell`/`tool_failure` 只在对应工具在集合里时才注入 | ~4,000 字符 |
| **M11 去掉重复段** | 删 `Temp File Hygiene`(996)，保留 `file_lifecycle.always` | 996 字符 |
| **M12 首轮 full 块** | 首轮也用 short 块；full 块按需（模型问再给） | 首轮 3,887 字符 + 首轮前缀不再与后续不共享 |
| **M13 工具 schema 瘦身** | 未路由到的工具用 `tool_search` 的渐进披露桥（阈值 25 现在永远触发不了，因为 50-39=11 < 25）→ 调低阈值或改成「只发路由子集 + 一个检索入口」 | ~10,000 字符 |
| **M14 记忆单点注入** | 三处记忆合并成一处（选 context 层那份，它有 epoch 缓存） | ~3,000 字符 + 少一次前缀变动 |

### P2 档：减少调用次数（压墙钟）

| 措施 | 做法 |
|---|---|
| **M15 固定剧本** | 把「视频节 / 视频+测验 / 纯测验」三种章节类型的调用序列写进 `chaoxing-learning` skill，每章照剧本走（目标 7.4 → 5 次/章） |
| **M16 批量化** | 一次 eval 完成「量坐标 + 点完全部题」，一次 eval 完成「读隐藏域 + 核对」，合并调用 |

## 四、可衡量目标指标

| 指标 | 现状 | 目标 | 怎么测 |
|---|---|---|---|
| **单次请求真实 token** | **~1M**（顶满模型窗口） | **≤100K**（~10x） | `prefix_probe` 日志（见下）+ provider 报的 `prompt_tokens` |
| **每轮 token** | **~6.94M** | **≤700K** | `token_usage` 按轮聚合 |
| **缓存命中率** | 89.21% | **≥98%** | `token_usage` 的 `cache_read_tokens/prompt_tokens`（provider 口径） |
| 每轮未命中占比（中位） | 10.5% | **≤2%** | 同上 |
| 历史进模型的 token | 无界（累积 3,718 条消息 / 6.1M 字符） | **≤60,000** | `bounded history window applied to the LLM payload` 日志行 |
| 每章调用次数 | 7.4 | **≤5** | `messages` 表 assistant 行数 / 轮数 |
| tools 数组跨重启稳定性 | 107/109 位置变化 | **0 变化** | `/v1/tools` 顺序哈希跨重启比对（`.git/vtest/probe-order.py` 可复用） |
| 系统提示字符数 | 68,796 | ≤48,000（**P1 档，可选**） | `prefix_probe` 的 `system=` |
| skill 正文注入量 | 16,008/请求 | ≤6,000 且按需（**P1 档，可选**） | 同上 |

**验收方式（已实施）**：`prefix_probe` 日志行，每次 API 调用前打出三段的**真实字符数**
与「相对同会话上次变了哪几段」：

```text
prefix_probe: #8 session=a04cab07 system=67900chars tools=33977chars
              messages=236289chars | 变了: messages
```

「变了」那一列就是**缓存失效的起点** —— 从该段往后，之前建立的缓存全部失效。
有了它，每条优化都能**当场验证**，而不是靠推断。

### 实测结果（2026-10-22 07:51，一轮真跑）

探针输出（`~/.hermix/logs/agent.log`）：

```text
prefix_probe: #1  system=67900chars tools=33977chars messages=236289chars | 变了: messages
prefix_probe: #19 system=67900chars tools=33977chars messages=310287chars | 变了: messages
```

| 指标 | 修复前 | 修复后 | 变化 |
|---|---|---|---|
| **单次请求真实 token** | ~1M | **~101K** | **~10x ↓** |
| 每轮 prompt（provider 口径） | 13,078,433 | **2,728,089** | 4.8x ↓ |
| 每调用新鲜 token | ~120K | **~16.5K** | 7.3x ↓ |
| **缓存命中率** | 88.0% | **83.6%** | **↓ 4.4 点** |

单次请求 101K = 窗口 60K + 系统提示 30K + 工具 10K —— **与设计完全吻合**。

**命中率为什么反而降了**（这是诚实要说清楚的）：请求小了 10 倍，「每轮新追加的消息」
这个**变量部分**在总请求里的占比就变大 —— 绝对消耗降了 10 倍，比率却被稀释。
这是「变小」与「变稳」两条路的此消彼长：**要回到 98%，得靠 P0 的字节稳定那一档**
（reasoning 剥离单调化 / 工具顺序确定化 / 工具集合稳定化 / 时间戳冻结），
那些改动在请求变小之后**相对更重要**。

## Alternatives considered

- **只调小 prompt 体积来提命中率**：否决。实验证明缓存是块级的，命中与否取决于**哪些字节变了**，
  不取决于 prompt 多大。把 prompt 砍小但保持每轮都变，命中率不会涨。
- **把 volatile 段挪到请求末尾**：可行但收益小（实测只值 12 token/次），且要改 provider 的
  system/messages 组装方式。**先做 M4（冻结），不够再考虑挪位置。**
- **把 `reasoning_content` 全清空（连本轮内的也不留）**：最省缓存，但会让模型在工具循环里
  看不到自己上一轮的思考 → 连续性下降。**M1 只清「本轮起点之前」的**，保留轮内连续性。
- **给每个 MCP 工具单独开缓存断点**：Anthropic 式缓存支持多断点，但本项目走的是 OpenAI 兼容
  路径，provider 侧是自动块级缓存，**前端加断点无效**。
- **把工具暴露从 109 个砍到路由子集**：P1-6/M13 的方向对，但会改变模型可见的能力面（可能
  够不到某些工具）→ **必须做成可配置开关**，默认保持现状，先在自动化任务这类窄场景开。

## Consequences

- **实施面**：M1/M4/M5 改 `crates/hermix-agent/src/{prompt_assembly.rs,agent_helpers.rs,local_agent_service.rs}`；
  M2/M13 改 `crates/hermix-tools/src/registry.rs`；M3 改 `crates/hermix-agent/src/agent_loop/`；
  M7~M12 改 `crates/hermix-agent/src/prompt_assembly.rs` + `crates/hermix-server/src/dashboard/skills.rs`。
  **这些 crate 此刻由并行线在改**（`crates/hermix-agent/*` 23 个文件、`crates/hermix-tools/*` 13 个文件
  未提交）→ 实施前需要等并行线落地或明确分工，否则会在同一批文件上打架。
- **风险 M3**：工具集合稳定化会降低「模型临时够到某个工具」的概率（路由不再每轮重算）。
  缓解：给路由加显式「换话题」触发条件，而不是纯按分数漂移。
- **风险 M8**：skill 正文按需后，模型可能不主动去取全文 → 站点知识缺失。缓解：索引里给
  「命中即自动注入该 skill 的哪一节」的白名单。
- **观测地基**：M1~M6 的验证都依赖「哪一段在变」可观测。在加哈希 span 字段之前，所有
  命中率改动都只能靠端到端对比验证，迭代成本高 —— **建议先做这一条**。
- **成本现实**：当前模型是 free 档，`estimated_cost` 全为 NULL，**省钱不是当下动机**；
  动机是①命中率高 → 延迟低 → 长任务跑得快；②若将来换付费模型，同样的请求体成本会按
  43K/次 计价，320K/章 × 630 节 = 200M token/门课 —— 那时这轮优化直接决定盈亏。
