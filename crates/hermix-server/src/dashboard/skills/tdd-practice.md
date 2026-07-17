---
name: "tdd-practice"
display_name: "测试驱动开发"
category: "software-development"
description: "用于：RED-GREEN-REFACTOR 循环，强制执行测试优先的 TDD 开发流程。"
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

## 何时激活
- 开始写新功能 / 新模块，想先锁行为时
- 修复 bug，先补失败测试复现再修时
- 用户说"用 TDD / 测试优先写"

## 核心概念
- 测试即规格：先写测试等于先定义预期行为
- 最小步进：红→绿→重构循环转得快
- 可测性：难测往往意味着耦合过重

## 实践指南
1. 先写一条会失败的测试（RED）
2. 写最少实现让它变绿（GREEN）
3. 在测试保护下重构（REFACTOR）
4. 每个测试只验证一个行为，保持独立快速

## 注意事项
- 不要为凑覆盖率写无意义的测试
- 测试脆弱（依赖顺序 / 时间 / 网络）要及时修
- 遗留代码先用性格测试（characterization）兜底

## 集成
- 与 `debugging-methodology` 配合：测试失败是定位起点
- 与 `code-review` 衔接：测试覆盖是审查项

## 参考
- 《测试驱动开发》（Kent Beck）
- 各语言测试框架官方文档

