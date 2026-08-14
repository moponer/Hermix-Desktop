---
name: "api-design"
display_name: "API 设计模式"
category: "software-development"
description: "用于：RESTful、GraphQL、gRPC 的选型对比、错误处理、认证授权、版本控制等 API 最佳实践。"
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

## 何时激活
- 设计新接口或评估现有接口时
- 选型 REST / GraphQL / gRPC 拿不准时
- 处理认证、错误码、版本兼容问题时

## 核心概念
- 资源 vs 行为：REST 面向资源，gRPC 面向服务，GraphQL 面向查询
- 契约优先：先定 Schema / OpenAPI 再实现
- 幂等：写操作要考虑重试安全

## 实践指南
1. 先画调用关系与数据流，再选协议
2. 错误用统一结构 + 稳定错误码，别把堆栈抛给调用方
3. 认证走标准（OAuth2 / JWT），密钥不进代码
4. 版本用 Header 或路径，避免破坏老客户端

## 注意事项
- 别为小项目上重协议（GraphQL 也有成本）
- 分页 / 限流是线上稳定性底线
- 不要在 URL 里放敏感信息

## 集成
- 与 `architecture-design` 衔接：API 是架构边界的体现
- 与 `database-design` 配合：接口背后的数据模型

## 参考
- OpenAPI Specification
- Google API Design Guide
- REST API 设计最佳实践（Microsoft / AWS）

## 代码样例

```http
GET /api/v1/users/{id}
Authorization: Bearer <token>
200 OK {"id":1,"name":"..."}  # 用名词复数作资源、状态码表语义、版本前缀 /v1
```

