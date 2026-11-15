# Decision Note: 技能匹配叠加语义检索

Status: implemented — 2026-10-17 落地（与提示词改造同 PR）

## 背景

`SkillLoader`（`crates/hermix-agent/src/skill_loader.rs`）目前用关键词匹配
用户消息与技能（name/description/tags 字面命中，阈值 0.3，每轮 ≤ 3 个）。
同义表述匹配不上：技能写"视觉设计"，用户说"美化一下界面"就漏。Anthropic
官方对 Agent Skills 的建议也是用模型自选/语义路由替代字面关键词。

## Decision

关键词匹配之上叠加一层向量召回（`SkillLoader::match_message`），两路结果
去重合并按相关度排序。**落地事实与原提案的差异**（按实现修正）：

1. **嵌入模型**：默认用 `HashingEmbeddingProvider`（**常驻可用**——sklearn
   hashing trick，纯 CPU、零下载、CJK 友好），不存在降级问题；fastembed
   升级路径保留 feature 门控，与同一 cosine 契约兼容。提案原设想的
   "fastembed 未启用即整层降级"不成立——无需降级。
2. **只嵌 description，不嵌 name/tags**：name/tags 是字面触发面，已被
   关键词/CJK pass 服务；嵌入它们会让单个共享 bigram（如 tag 里的"存储"）
   在无关查询上误报（实测复现并因此修正）。description 才是改写信号所在。
3. **两侧滤停用词**：查询与描述都先滤 STOPWORDS 再嵌入——英文胶水词
   bigram（that/are 的 "at"/"er"）会压过真实信号（run_loop 回归测试
   `p2_t1_block_injects_matching_skills_into_injection` 实测复现并修正）。
   CJK 字符属 alphanumeric，滤词不破坏 CJK 连续性。
4. **阈值 0.18 + 操作员联动**：短 CJK 查询对长描述的余弦实测 ~0.20，
   无关查询 ~0.03-0.06，0.18 两侧留有余量；操作员把 `min_score` 抬到
   0.3 默认之上时语义阈值同步抬高（钉 0.99 的严格测试不得收到向量匹配）。
5. **召回为增量**：向量匹配只补充关键词层漏掉的技能（按 name 去重），
   `matched_on = "vector"` 可观测；关键词命中的分数不被覆盖。
6. **截断修复**：`max_body_lines` 超长不再静默截半篇，改为注入技能骨架
   （标题 + 每节首行）+ `skill_view` 指引（`load_body`）。

## Alternatives considered

- **全靠 LLM 自选**（把技能列表全量塞给系统提示让模型挑）：技能多时
  系统提示膨胀且每轮付费，与零上下文膨胀原则冲突。渐进加载（摘要表 +
  按需取全文）仍是骨架，语义检索只优化"摘要表怎么排"。否决替换。
- **关键词权重调优**：词表永远追不完同义表达，维护成本线性增长。
  否决。
- **升级为 LLM 打分路由**（每轮用小模型给技能打分）：多一次 LLM 调用，
  延迟与成本高于本地嵌入。否决，作为 fastembed 路径不可用时的备选。

## Consequences

- **fastembed 地域风险消除**：语义层默认跑常驻 hashing 嵌入器，不依赖
  cdn.pyke.io；fastembed 仅作为可选升级（同一 cosine 契约），地域被墙
  不再影响技能匹配。
- **无索引失效问题**：hashing 嵌入每次匹配即时计算（16 个技能 × 微秒级），
  不落向量索引、无 mtime 失效管理、无文件监听——比提案的索引方案更简单
  且天然反映磁盘最新状态。
- **放弃的东西**：①放弃"匹配行为完全可预测可复现"——向量召回有相似度
  灰区；缓解：`matched_on = "vector"` 可观测 + 阈值经三组实测校准
  （改写 0.20 / 误报 ≤0.06 / 单 bigram 0.19）。②放弃 hashing 嵌入的
  语义上限——它只能抓字符 n-gram 重叠，真同义改写（"美化"vs"视觉设计"）
  若无共字则召回不到；fastembed 升级路径为此保留。
