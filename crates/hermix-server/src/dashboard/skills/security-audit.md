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

## 触发条件
当用户要求以下操作时使用此 Skill:
- "安全审计" / "security audit"
- "安全检查" / "security check"
- "漏洞扫描" / "vulnerability scan"
- "检查安全漏洞"

## 执行步骤
1. 确定审计范围（文件/模块/项目）
2. 按检查清单逐项审查
3. 输出审计报告，包含漏洞分级和修复建议

## 常见安全问题
### 注入攻击
- SQL 注入、XSS、命令注入、路径遍历

### 认证与授权
- 弱密码、权限提升、会话固定

### 数据安全
- 敏感信息泄露、不安全的存储、传输加密

### 依赖安全
- 已知漏洞的依赖、未更新的库

## 审计检查清单
- [ ] 输入验证
- [ ] 输出编码
- [ ] 参数化查询
- [ ] 权限检查
- [ ] 错误处理（不泄露信息）
- [ ] 日志记录（不记录敏感数据）
- [ ] 依赖扫描

## 输出格式
```
## 安全审计报告
### 严重漏洞
- [漏洞描述] — [文件:行号] — [修复建议]

### 中等风险
- [漏洞描述] — [文件:行号] — [修复建议]

### 低风险/建议
- [建议描述]
```
