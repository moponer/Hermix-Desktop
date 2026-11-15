# Decision Note: Skill 渐进披露 —— 索引给入口，正文按体积决定内联还是按需加载

Status: implemented — 2026-10-30

## Decision

**skill 的默认形态是「头部 + 指针」，正文只在两种情况下进上下文：小到不值得一次往返，或者模型主动
`skill_view` 去取。**

| 位置 | 改动 |
|---|---|
| `skill_loader::build_injection_and_tools` | 自动匹配到的 skill：正文 ≤ `SKILL_INLINE_MAX_CHARS`（2 000）→ 内联全文；超过 → **只输出指针**（名字 + description + 「先 `skill_view(name=…)` 再动手」+ 正文字符数） |
| `acui_dispatch::default_tool_router` | 白名单加 `skill_view` / `skills_list` |
| `result_budget::model_visible_cap` | `skill_view` 单独给 16 K（它是全文读取通道），`skills_list` / `skill_manage` 保持 6 K |
| `skill_orchestrator::build_preloaded_skills_prompt` | 接收 `char_budget`（调用方传 `config.skill_injection_char_budget`）：超预算的正文转指针，勾选的技能不丢 |

## 问题

技能系统本来就是渐进披露的：system prompt 里只有 `<available_skills>` 索引（名字 + 描述），
正文要 `skill_view`。但**自动匹配这条路绕过了它**——命中即把 SKILL.md 正文塞进 ctx：

```
## Auto-Matched Skills (system-injected)
### {name} (score: 0.72, matched on: keyword)
{整篇正文，单个 ≤500 行}
```

三处后果：

1. 体量差两个数量级：`workflow-tdd` 正文 ≈ 2.5 K 字符，`custom-app-development` SKILL.md 是
   **33.8 K**，`storage-analyzer` 13.1 K。命中 3 个大技能就把 16 K 预算吃满。
2. 这条注入是 **system 角色消息，进了 ctx 就留在历史里**，此后每轮重发（去重只防同一轮重复追加）。
3. 索引本身是便宜的：本机 87 个 skill 的 `description:` 首行合计 12,367 字符，索引整块
   上限 20 K、降级形态 2 K。真正的成本从来不在索引。

还有一条隐性的：`skills_system_prompt` 只在 `skills_list` / `skill_view` / `skill_manage`
出现在本轮路由结果里时才渲染索引；而它们**不在** `default_tool_router` 的白名单里。
路由把它们裁掉 → 索引不出现 + 指针无处可点 → 技能系统对该轮完全不可见。

## Alternatives considered

- **一律只给指针**（最严格的渐进披露）：2.5 K 的正文也要多跑一次 `skill_view` 往返，而模型
  本来就会跳过「请先读取」这类指令——这正是当初改成全文注入的原因。省的是 token，赔的是
  「技能有没有生效」。
- **一律内联**（现状）：一个大技能 = 16 K / 轮，且进历史后永久重发。
- **按匹配分数决定**：分数是启发式（关键词 0.3 起、向量按语种另设阈值），用它决定「要不要
  花 16 K」会让同一条消息的上下文体积随匹配算法抖动；体积是确定的、可解释的判据。
- **把 `skill_view` 结果也压到 6 K**：那等于全文读取通道读不全 33 K 的技能，指针就失去意义。
  改为 16 K + spill（超出部分落盘并给分页指针）。

## Consequences

- 大技能命中后的注入从「≤16 K 正文」降到「~200 字符指针」；代价是模型可能多一次工具调用
  （`skill_view`），这一次往返才真正把正文带进来，且带进来的是它**选择**要读的那份。
- 小技能（≤2 K）行为不变：仍然是命中即内联，不增加往返。
- `skill_view` / `skills_list` 进白名单后，工具数组固定多两个 schema（合计约 0.6 K 字符），
  换来的是「技能索引一定存在、指针一定可点」。在会话冻结工具数组之后，这是每会话一次的固定成本。
- 回归锁：`skill_loader::tests` 的注入用例（小正文仍断言内联内容）；大正文只断言指针形态。
  `skill_orchestrator::tests::preloaded_skill_over_budget_becomes_a_pointer` 锁住同一规则在
  预加载路径上的形态。
- **预加载（UI 技能选择器）同规则**：`build_preloaded_skills_prompt` 现在接收
  `char_budget`（调用方传 `config.skill_injection_char_budget`）。超预算的技能不丢
  （`loaded` 里仍有名字），正文换成指针并打一条 `hermix.skills_index` 日志。此前这条路
  无任何上限——勾两个大技能就能把 47 K 字符压进 system prompt，而且进会话快照后一直重发。
