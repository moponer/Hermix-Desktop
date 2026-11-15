---
name: "self-improvement"
display_name: "复盘与经验沉淀"
category: "productivity"
description: "任务结束或被用户纠正时做结构化复盘，把结论落到 skill patch 或 memory，让同类任务下次不再犯错。用于：任务完成后复盘、用户纠正了错误做法、发现更优路径、工具调用踩坑后。不适用于：任务执行中的临时调整（那是对话内容，不是经验）。"
version: "0.1.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["productivity", "builtin", "复盘", "改进", "经验", "self-improving"]
---

# 复盘与经验沉淀

核心原则：**只复盘不落盘 = 没复盘**。结论必须写进 skill patch 或 memory，
否则换个会话同样的错再犯一遍。

## 触发时机（满足其一即复盘）

- 任务完成，过程中有过失败/返工/用户纠正
- 用户明确说"以后要…/别再…/记住…"
- 发现了可复用的流程或工具参数组合（下次直接能用）
- 工具/命令踩坑后找到了正确用法

## 工具接线

1. **沉淀为技能改进**：`skill_manage` 的 `patch`——向相关技能追加
   "Improvement Feedback"节。适用于：流程类、纪律类、某技能描述有误/
   有遗漏的修正。patch 是追加不是重写（质量门强制）。
2. **沉淀为记忆**：`memory` 工具存**静态事实**——用户偏好、环境特性、
   工具怪癖。遵循系统提示 MEMORY_GUIDANCE 排除清单：可从代码/文件推出的
   信息**不存**（代码是活的，存了会烂）。
3. **跨会话可复跑的流程**：优先落成 skill（`auto_extract_from_conversation`）
   或 `workflow`（用户要"一键重跑"时）。

## 复盘格式（三段）

1. **发生了什么**：我做了什么动作，哪一步出了问题（或哪一步特别顺利）
2. **根因**：为什么错——判据写清楚（"没先 snapshot 就点击"而非"我大意了"）
3. **下次怎么办**：一句话可执行的规则 + 落盘位置（patch 到哪个技能 /
   存哪条 memory），当场落盘

## 反例（不要这样做）

- ❌ 复盘说"下次注意"但不落盘——下次照样错
- ❌ 把一次性任务细节存成 memory（MEMORY_GUIDANCE 排除清单）
- ❌ 把可从代码推导的信息存进记忆——代码变了记忆就成了"权威的错误"
- ❌ 复盘写成检讨书——用户要的是规则改进，不是道歉

## 输出契约

复盘简短：问题 → 根因 → 规则 → 已落盘位置（patch id / memory key）。
两三句话说完，不展开长文。

## 参考

- OpenClaw self-improving-agent（31.1 万下载，#1）：错题本范式
