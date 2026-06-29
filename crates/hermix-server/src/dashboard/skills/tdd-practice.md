---
name: "tdd-practice"
display_name: "测试驱动开发"
category: "software-development"
description: "RED-GREEN-REFACTOR 循环，强制执行测试优先的开发流程。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# 测试驱动开发 Skill

## 概述
测试驱动开发（TDD）是一种软件开发方法，先写测试，再写实现代码。

## 核心循环
1. **RED**：写一个失败的测试
2. **GREEN**：写最少的代码让测试通过
3. **REFACTOR**：重构代码，保持测试通过

## 最佳实践
- 测试应该小而专注
- 每个测试只验证一个行为
- 测试应该独立，不依赖其他测试
- 保持测试快速运行

## 工具
- Rust: cargo test
- JavaScript: Jest, Vitest
- Python: pytest

