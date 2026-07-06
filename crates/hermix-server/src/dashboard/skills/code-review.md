---
name: "code-review"
display_name: "代码审查"
category: "software-development"
description: "提交前的静态安全扫描、代码质量检查、测试覆盖率验证。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# 代码审查 Skill

## 触发条件
当用户要求以下操作时使用此 Skill:
- "审查代码" / "code review"
- "检查代码质量" / "check code quality"
- "安全审计" / "security audit"
- 提交前的代码检查

## 执行步骤
1. 读取目标文件或代码块
2. 按检查清单逐项审查
3. 输出审查报告，包含问题分类（严重/中等/建议）和具体位置
4. 如果发现问题，提供修复代码

## 检查清单

### 安全性（必须检查）
- SQL 注入防护
- XSS 防护
- 敏感信息泄露检查
- 权限验证

### 性能（建议检查）
- 数据库查询优化（索引、N+1）
- 缓存使用（重复计算）
- 异步处理（I/O 操作）

### 可维护性（建议检查）
- 代码可读性（命名、注释）
- 模块化程度（单一职责）
- 错误处理（异常捕获）

## 输出格式
```
## 审查报告
### 严重问题
- [问题描述] - [文件:行号] - [修复建议]

### 中等问题
- [问题描述] - [文件:行号] - [修复建议]

### 建议改进
- [改进建议]
```

## 工具
- Clippy (Rust)
- ESLint (JavaScript)
- SonarQube (多语言)
