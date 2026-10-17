# Decision Note: 技能匹配叠加语义检索

Status: proposed — 从 [2026-10-17-prompt-asset-quality-upgrade.md](2026-10-17-prompt-asset-quality-upgrade.md) 批次 4 拆出的独立决策

## 背景

`SkillLoader`（`crates/hermix-agent/src/skill_loader.rs`）目前用关键词匹配
用户消息与技能（name/description/tags 字面命中，阈值 0.3，每轮 ≤ 3 个）。
同义表述匹配不上：技能写"视觉设计"，用户说"美化一下界面"就漏。Anthropic
官方对 Agent Skills 的建议也是用模型自选/语义路由替代字面关键词。

## Decision（提案）

在关键词匹配之上叠加一层向量召回，两路结果合并去重后按相关度排序：

1. **索引**：技能安装/更新时，把 name + description + tags 拼成一段文本
   做嵌入，存技能向量索引（落 `~/.hermix/skills/embeddings/`）。
2. **召回**：每轮用户消息做一次嵌入（单条，毫秒级），取 top-3 相似技能，
   与关键词匹配结果合并。
3. **嵌入模型**：复用 `hermix-intelligence` 既有 fastembed 通道（feature
   门控）；fastembed 未启用时静默降级为纯关键词匹配——语义检索是增强，
   不是依赖。
4. **截断修复**：现 `max_body_lines=500` 超长静默截断，改为：超长时注入
   技能骨架（标题 + 每节首行）+ 提示用 `skill_view` 按节读全文，禁止
   注入半篇残文。

## Alternatives considered

- **全靠 LLM 自选**（把技能列表全量塞给系统提示让模型挑）：技能多时
  系统提示膨胀且每轮付费，与零上下文膨胀原则冲突。渐进加载（摘要表 +
  按需取全文）仍是骨架，语义检索只优化"摘要表怎么排"。否决替换。
- **关键词权重调优**：词表永远追不完同义表达，维护成本线性增长。
  否决。
- **升级为 LLM 打分路由**（每轮用小模型给技能打分）：多一次 LLM 调用，
  延迟与成本高于本地嵌入。否决，作为 fastembed 路径不可用时的备选。

## Consequences

- **fastembed 依赖的地域风险**：cdn.pyke.io 部分地区被墙（AGENTS.md
  已记录），故语义层必须保持 feature 门控 + 静默降级，绝不做成硬依赖。
- **索引失效管理**：技能文件被外部编辑（直接改 SKILL.md）后索引过期；
  用文件 mtime 做懒重建（加载时 diff mtime），不引入文件监听。
- **放弃的东西**：放弃"匹配行为完全可预测可复现"——向量召回有非确定性，
  调试匹配问题多一层。缓解：匹配结果日志记 score 与来源（keyword/vector）。
