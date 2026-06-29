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

## 概述
代码审查是确保代码质量的重要环节，包括安全检查、性能优化、可维护性评估。

## 检查清单
### 安全性
- SQL 注入防护
- XSS 防护
- 敏感信息泄露检查
- 权限验证

### 性能
- 数据库查询优化
- 缓存使用
- 异步处理
- 资源释放

### 可维护性
- 代码可读性
- 命名规范
- 注释完整性
- 模块化程度

## 工具
- Clippy (Rust)
- ESLint (JavaScript)
- SonarQube (多语言)

