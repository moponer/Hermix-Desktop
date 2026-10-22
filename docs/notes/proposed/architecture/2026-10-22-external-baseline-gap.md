# Decision Note: 外部底座对照 —— 我们的 token 优化发力点错了

Status: proposed — 2026-10-22 外部底座对照调研（Anthropic / aider）+ 自建决定性实验

## 背景

`2026-10-21-token-consumption-and-cache-efficiency.md` 把成本归因到「窗口大小 ×
未命中率 × 调用次数」，并据此做了一整轮优化（历史窗口、字节稳定、存根治理、长等待）。
本篇把**外部成熟底座的做法**拿来对照，并用**自建实验**测我们 provider 的真实缓存行为，
结论是：**发力点错了 —— 决定成本的不是「窗口多大」，而是「载荷里每一块字节稳不稳」。**

## Decision

**放弃「调窗口」作为主手段，转向三条按实测收益排序的动作**：

### D1（P0，最贵的一处）：system 提示必须逐字节稳定 —— 改一处 = 整份作废

自建实验（`.git/vtest/hit-curve.py`，真实载荷形态 `[tools][system][messages]`）：

| 变动 | 首发命中 | 恢复 |
|---|---|---|
| 只有尾部增长（我们的常态） | 94.0% → 91.6%（5 发降 2.4pt，cached 恒定 4,864） | — |
| **中途改 system 一处** | **2.1%**（cached=128） | 下一发 100% |
| 中途改 tools 一处 | 67.0% | 下一发 90.9% |

**改 system 的一处字，连改动点之前的部分也一起作废** → 一次 system 变化 = 整轮全价。
这条直接决定优先级：

- **技能正文改为按需注入**（现在是无条件注入 16,000 字符）→ 不只是省 token，
  更让**技能索引不再随 agent 建 skill 而变**（`~/.hermix/skills/auto/` 的 16 个
  自动 skill 每次增删都改索引 → 每建一个 skill 作废一轮）。外部同构做法见
  Anthropic《Agent Skills》的**渐进披露**：启动只预载 name+description，正文按需读。
- 工具集合的字节稳定性同权重（M2 已做，别退）。

### D2（P0）：工具表按 token 预算动态收缩，别用「白名单 + 固定 50」

aider 的 repo map 用**图排序**挑最相关部分，默认 `--map-tokens 1k`，并按会话状态
动态增减。我们的工具表 26,272~53,654 字符（≈7~13K token），是 aider 整个 repo map
的 **10 倍量级**。方向：按「当前任务相关度」排序 + 预算封顶，而不是白名单硬撑。

### D3（P1）：轮内旧工具组直接丢弃（不是换存根）

Anthropic 明确写：*"One of the safest lightest touch forms of compaction is tool
result clearing"* —— 清掉历史里的工具调用与结果。我们只做了「换成一行存根」，
**存根本身不清除**（1,118 条 × 81 字符 = 窗口的 38%）。这与 M22 的空操作
（事实 15）是同一个洞的两面。

## Alternatives considered

- **保活 ping（aider 的做法）—— 实测无效，不做**。aider 靠它防 Anthropic 的 5 分钟
  缓存过期；我们自建 TTL 实验（`.git/vtest/cache-ttl-exp.py`，双对照）显示
  **同一前缀 6 分钟后仍 99.8% 命中** → 我们这个 provider **没有 5 分钟 TTL**，
  保活无意义。（附带观测：某个前缀块被逐出时，同一请求命中会**整份归零**，
  不是「只损失那一块」—— 逐出机制待查，但这不影响上面的动作选择。）
- **把历史窗口从 60K 调到 25K —— 收益极小，不做**。窗口里绝大部分是
  `role='automation'` 的孤儿 tool 结果（事实 13），发请求前就被 provider 丢掉；
  裁它们不省 token，只是让丢弃这件事便宜。
- **换 provider 之前先测**：要 98% 平均命中，需要带**显式缓存断点 + 长 TTL** 的
  provider（Anthropic 支持 1h TTL，写入价 2×），或自建（vLLM/SGLang 块级前缀缓存
  + 明确的容量与逐出策略）。这是**采购/架构决策**，不在本轮代码范围内。

## 外部底座对照表（七条共识 vs 我们的现状）

| # | 外部做法 | 来源 | 我们的现状 | 判定 |
|---|---|---|---|---|
| 1 | **渐进披露**：启动只预载 skill 的 name+description，正文**按需**读进上下文 | Anthropic《Agent Skills》 | 索引 20,000 + **正文 16,000 无条件注入** | ❌ 偏离最大的一处 → D1 |
| 2 | **just-in-time 检索**：只给轻量标识符（路径/查询），运行时用工具取数据，不预先灌全量 | Anthropic《Effective context engineering》 | read_file 整文件、webview DOM dump 整段进 ctx；有 spill/rollup 但不是「按需」 | ⚠️ 做了一半 → 与 D3 同批 |
| 3 | **tool result clearing**：清掉历史里的工具调用与结果，「最安全的压缩形式」 | Anthropic 同上 | 只换成存根，**存根不清除**（1,118 条 = 窗口 38%） | ❌ → D3 |
| 4 | **结构化笔记**：把状态写到上下文外，之后再拉回 | Anthropic 同上 | MEMORY.md **写不进去**（冒号路径 bug，M21 已修） | ✅ 本轮修好，是「每轮不重新摸状态」的前提 |
| 5 | **子 agent 隔离**：子 agent 烧几万 token，只回 1~2K 摘要 | Anthropic《multi-agent research》：agent ≈4×、多 agent ≈15× token，但 token 用量单独解释 80% 的性能方差 | delegate_task 已有 | ⚠️ 机制有，但「摸状态/找线索」类工作没走子 agent |
| 6 | **固定缓存顺序**：system → 只读文件 → repo map → 可编辑文件 | aider《Prompt caching》 | 我们的顺序天然是 tools→system→messages（与 Anthropic 文档同序） | ✅ 顺序对，**字节稳定性**是 D1 |
| 7 | **repo map 预算**：图排序挑最相关，默认 1k token，按会话动态增减 | aider《Repository map》 | 工具表 26,272~53,654 字符（≈7~13K token），白名单 + 固定 top-50 | ❌ 量级差 10 倍 → D2 |

**两个不要做的**（外部做法里对我们**不适用**的）：

- **保活 ping**（aider 防 5 分钟过期）：我们自建双对照实验证明本 provider **没有 5 分钟
  TTL**（6 分钟后 99.8% 命中）→ 照抄只会白花钱。
- **把窗口调小**：我们窗口里绝大部分是发请求前就被丢掉的孤儿（事实 13），裁它们
  不省 token。

## Consequences

- 本篇**推翻**同族笔记里「绝对量的 99% 在会话历史里 → 先把历史管住」的判断
  （已在那篇的 Decision 段标注更正）。正确的表述是：
  **成本 = 载荷中「不新鲜」的字节量 × 未命中率，而载荷的稳定性比载荷的大小更贵。**
- 外部七条共识里，我们原本**只完整做到两条**（字节稳定、有界窗口），其余五条
  要么只做了一半，要么方向错了 —— 这就是本篇的差距清单。
- 代价：技能正文改为按需后，agent 可能「该读没读」→ 需要在 skill 索引里写清
  何时必须读（Anthropic 同一篇文章也强调 description 决定触发质量）。
- 风险：工具表按相关度收缩 → 冷门工具可能摸不到 → 保留 `list_tools` 兜底。

## 复现脚本

- `.git/vtest/hit-curve.py` —— 真实载荷形态下的命中曲线（尾部增长 / 改 system / 改 tools）
- `.git/vtest/cache-ttl-exp.py` —— TTL 双对照实验（无保活 vs 有保活）
- `.git/vtest/cache-exp.py` —— 字节稳定对照实验（既有）
