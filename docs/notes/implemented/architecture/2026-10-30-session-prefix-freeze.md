# Decision Note: 会话级前缀冻结 —— 把冷启动从 0.1% 命中拉回 99%

Status: implemented — 2026-10-30

## Decision

**provider 看到的前两段（`tools` 数组、system 消息）在一个会话内逐字节不变。**

落点三处：

| 位置 | 改动 |
|---|---|
| `agent_loop::config::route_tools` | 首轮路由结果按 session 冻结；后续 run 直接返回同一份数组（含 `tool_search` 桥的 deferred 计数文本） |
| `prompt_assembly::resolve_initial_system_prompt_with_tool_errors` | 系统提示按 session 存进程内快照；命中即复用**同一串字节**（SectionGate 补丁后的字节） |
| `turn_report` / `observe_prefix_reuse` | 补观测：`cache_read_tokens` / `cache_hit_pct` / `context_threshold_tokens`，以及前缀归因三元组 `tools_changed` / `system_changed` / `tools_fp` |

冻结只有三种失效条件，且都是「真的变了」：

1. **缓存键变** —— 模型 / 人格 / 自定义 system_prompt / workspace / 领域模式 / 工作台 / 记忆文件 `(mtime, len)`；
2. **身份锚点不在快照里** —— SOUL.md 或人格卡换了名字（沿用 2026-10-27 的 stale-identity 规则）；
3. **记忆 epoch 前进** —— `memory` 工具写库后要求重建（`force_rebuild`）。

记忆为什么用文件元数据当键而不是只靠 epoch：桌面端每次用户消息都会新建一个 `AgentLoop`，
`context_epoch` 随之归零，「本会话内改过记忆」这件事跨不过 run 边界。`MEMORY.md` / `USER.md`
的 mtime+len 是跑不掉的事实，且只有真的写了才变。

注册表规模变化（MCP 新工具、插件热载）时重新冻结 tools，并同时丢弃系统提示快照，避免提示里列着数组里没有的工具。

## 问题（实测，本机 `~/.hermix/data/sessions.db`，近 7 天）

| 口径 | 调用数 | prompt | 命中 | 命中率 |
|---|---|---|---|---|
| 全部（prompt > 1K） | 483 | 56,070,200 | 47,816,908 | **86.2%** |
| 命中 ≥ 90% | 275 | 36,979,869 | — | 98.0% |
| 命中 50–90% | 172 | 15,488,371 | — | 76.0% |
| 命中 < 50% | 36 | 3,601,960 | — | **8.4%** |

- 那 36 次冷启动吃掉了**全部未命中 token 的 44%**（3.39M / 7.72M）。
- 冷启动的 `cache_read_tokens` 恒为 **184**，而 prompt 是 22 万 —— 共享前缀只剩身份块开头。
- 同一会话内（相邻两次工具循环）`predicted_hit_pct` 稳定在 96.8%–99.8%，`rewritten=false`：**消息体本身没有问题**（2026-10-29 的 write-once 修复有效）。
- 冷启动那一次 `shared_msgs=0 total_msgs=471 rewritten=true`。

### 机理

provider 的前缀缓存只认「从 position 0 开始的字节」。OpenAI 兼容模板把 `tools` 数组渲染在 system 之前，所以：

- `tools` 集合随**当前用户消息**变（路由打分输入是最后一条 user message，`max_routed_tools` 截断到 50，超出部分再被折叠进 `tool_search` 桥，实测同一会话内 `deferred=24/25/26/30`）；
- system 消息里的 `directions` 也随当前用户消息变（`resolve_domain_mode`），工具清单文本随路由结果变。

两者叠加 ⇒ 每条新用户消息都从字节 0 重算。184 这个数字是判据：若分歧点在 system 内部的 directions 块，stable 层（实测 6,892 token）应该仍然命中。

### 附带发现的死路径

`config.stored_system_prompt` 这条「prefix cache continuity」路径在桌面端从未被填：

- `hermix-server` 里没有任何 `hydrate_stored_system_prompt_from_hermes_home` 调用；
- `sessions.system_prompt` 实测为 NULL（该列只有压缩子会话会写）；
- `stored_prompt_epoch` 自初始化后**从未被写过**，即便存在冻结提示，`force_rebuild` 也会永久为真。

本次把快照放在进程内（不依赖上述死路径），并把 `stored_prompt_epoch` 在真正重建后写入。

## Alternatives considered

- **只排序不冻结**（2026-10-23 已做）：解决的是「同一集合换顺序」，解决不了「集合本身每轮都换」。
- **把 `directions` 从 system 移到当前 user 消息尾部**：语义上最干净（system 只剩会话常量），但要改 `build_system_prompt` 的产出契约，牵连十余处断言 `prompt.contains(...)` 的测试，且 SectionGate 会把缺失的 `### Active tools` 补回 system —— 一次改动同时动契约、门禁和缓存，风险不成比例；先做冻结。
- **用 DB 的 `sessions.system_prompt` 做跨进程复用**：需要 server 显式 hydrate + 每次 run 写回，还要处理多进程并发；进程内快照零 schema 改动即可拿到同样效果（重启代价是每会话一次冷启动）。
- **并集增长（union-grow）**：每次并集变化仍是一次全量失效，单用户消息数少的会话里退化成今天的行为，等于没修。
- **关掉路由 / 放宽 `max_routed_tools`**：能让数组从第一天就与消息无关，代价是常驻工具数固定变大（约 +5K token，且这部分会被缓存）。留到有实测数据后再定。

## Consequences

- **收益**：冷启动从「每条用户消息一次」降到「每会话一次」。按上面分布，7.7M 未命中 token 里约 3.4M 可消掉，整体命中率 86.2% → 约 92%；会话内继续维持 99%+。
- **代价 1**：`directions` 冻结在会话首条消息的形态。用户切换领域模式 / 工作台会触发重建（已计入快照键），但「自动检测」的漂移不再逐条反映。
- **代价 2**：`[Tool Failure Hints]`（工具连续失败的降权提示）挂在 system 尾部，冻结后只在首次构建时出现 —— 该提示在会话中不再更新。降权本身（`classify_tool_error`）不受影响。
- **代价 3**：会话首个问题的路由决定整套工具；后续话题需要的新工具只能经 `tool_search` → `tool_call` 到达（三个桥在 `always_include` 里，永远在数组中）。已加 debug 计数 `tool set: this run's route wanted tools outside the frozen array`，用来量化这一牺牲。
- **观测**：`agent.log` 的 turn report 现在带 `cache_read_tokens` / `cache_hit_pct` / `context_threshold`；`prompt_cache: payload prefix reuse` 现在带 `tools_changed` / `system_changed` / `tools_fp`，能直接回答「这次没命中的是工具还是提示」。
- **回归锁**：`agent_loop::config::tool_freeze_tests`（冻结命中 / 注册表变化重冻 / 无 session 不冻）。
- **上下文膨胀另见** [`2026-10-30-tool-result-shaping.md`](2026-10-30-tool-result-shaping.md)：工具结果的分档上限、`read_file` 分页与投递量对齐、旧结果的高水位回收。请求体积与窗口大小无关，判据是工作负载（单请求目标 ≤ ~50 K token），不是「窗口占了几成」。
