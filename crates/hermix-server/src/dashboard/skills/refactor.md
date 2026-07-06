---
name: "refactor"
display_name: "代码重构"
category: "software-development"
description: "安全地重构代码，改善结构、可读性和可维护性。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "caution"
tags: ["software-development", "builtin"]
---

# 代码重构 Skill

## 概述
重构是在不改变外部行为的前提下改善代码内部结构的过程。

## 重构原则
1. **小步前进** — 每次只做一个小的、可验证的改动
2. **先测试** — 重构前确保有测试覆盖
3. **频繁提交** — 每完成一步就提交，方便回滚
4. **保持行为** — 重构不改变功能，只改善结构

## 常见重构模式
### 提取函数
- 将重复代码提取为独立函数
- 函数名应清晰表达意图
- 每个函数只做一件事

### 提取类
- 当一个类承担过多职责时
- 相关功能应组织在一起

### 重命名
- 使用有意义的名称
- 保持一致性

### 简化条件
- 使用 early return 减少嵌套
- 使用 guard clause
- 合并相似条件

## 工具
- Clippy (Rust) - 自动检测可改进的模式
- ESLint (JavaScript) - 代码质量检查
- IDE 重构功能

## 检查清单
- [ ] 重构前运行测试，确保通过
- [ ] 每次只做一个小改动
- [ ] 每步完成后运行测试
- [ ] 提交前再次运行完整测试
- [ ] 更新相关文档
