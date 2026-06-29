---
name: "api-design"
display_name: "API 设计模式"
category: "software-development"
description: "RESTful、GraphQL、gRPC 的选型对比、错误处理、认证授权、版本控制等最佳实践。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# API 设计模式 Skill

## 概述
API 设计是系统间通信的关键，需要考虑易用性、性能、安全性。

## RESTful 设计原则
1. 资源导向：用名词表示资源
2. HTTP 方法：GET/POST/PUT/DELETE
3. 状态码：正确使用 HTTP 状态码
4. 版本控制：URL 或 Header 版本

## 错误处理
- 统一错误格式
- 详细的错误信息
- 错误码体系

## 认证授权
- JWT Token
- OAuth 2.0
- API Key

## 性能优化
- 分页
- 缓存
- 限流

