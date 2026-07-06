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

## 触发条件
当用户要求以下操作时使用此 Skill:
- "重构" / "refactor"
- "优化代码结构" / "improve code structure"
- "清理重复代码" / "clean up duplication"
- "改善可读性" / "improve readability"

## 执行步骤
1. 确认重构目标和范围
2. 运行测试，确保现有测试通过
3. 小步重构，每步完成后运行测试
4. 提交前再次运行完整测试

## 重构原则
1. **小步前进** — 每次只做一个小的、可验证的改动
2. **先测试** — 重构前确保有测试覆盖
3. **频繁提交** — 每完成一步就提交，方便回滚
4. **保持行为** — 重构不改变功能，只改善结构

## 常见重构模式
### 提取函数
- 将重复代码提取为独立函数
- 函数名应清晰表达意图

### 提取类
- 当一个类承担过多职责时

### 简化条件
- 使用 early return 减少嵌套
- 合并相似条件

## 检查清单
- [ ] 重构前运行测试，确保通过
- [ ] 每次只做一个小改动
- [ ] 每步完成后运行测试
- [ ] 提交前再次运行完整测试

## 输出格式
```
## 重构报告
### 变更摘要
- [文件:行号] — [变更描述]

### 验证结果
- 测试状态: [通过/失败]
```
