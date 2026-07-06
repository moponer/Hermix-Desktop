---
name: "security-audit"
display_name: "安全审计"
category: "software-development"
description: "检查代码安全漏洞，包括注入攻击、权限问题、敏感信息泄露。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# 安全审计 Skill

## 概述
识别和修复代码中的安全漏洞。

## 常见安全问题
### 注入攻击
- SQL 注入
- XSS（跨站脚本）
- 命令注入
- 路径遍历

### 认证与授权
- 弱密码
- 权限提升
- 会话固定

### 数据安全
- 敏感信息泄露
- 不安全的存储
- 传输加密

### 依赖安全
- 已知漏洞的依赖
- 未更新的库

## 审计检查清单
- [ ] 输入验证
- [ ] 输出编码
- [ ] 参数化查询
- [ ] 权限检查
- [ ] 错误处理
- [ ] 日志记录
- [ ] 依赖扫描

## 工具
- cargo-audit (Rust)
- npm audit (Node.js)
- Bandit (Python)
- Snyk (多语言)
