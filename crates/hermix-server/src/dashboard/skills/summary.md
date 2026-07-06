---
name: "summary"
display_name: "内容摘要"
category: "office"
description: "从长文本中提取关键信息，生成简洁的摘要。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["office", "builtin"]
---

# 内容摘要 Skill

## 触发条件
当用户要求以下操作时使用此 Skill:
- "总结" / "摘要" / "summary"
- "提炼要点" / "extract key points"
- "概括" / "概要"
- "帮我看看这篇文章讲了什么"

## 执行步骤
1. 阅读全文，理解主题和结构
2. 提取 3-5 个核心要点
3. 生成简洁摘要

## 摘要类型
- 文档摘要（技术文档、论文、报告）
- 会议摘要（讨论要点、决策结果）
- 对话摘要（关键信息、待办事项）

## 输出格式
```
## 摘要
### 主题
[文档/对话的主题]

### 核心要点
1. [要点 1]
2. [要点 2]
3. [要点 3]

### 关键结论
[结论或决定]

### 行动项（如有）
- [待办事项]
```
