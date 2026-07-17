---
name: "database-design"
display_name: "数据库设计"
category: "software-development"
description: "用于：关系型 / NoSQL 选型、Schema 设计、索引优化、Redis 缓存模式等数据库设计。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# 数据库设计 Skill

## 概述
数据库设计是应用开发的基础，影响性能、可维护性和扩展性。

## 设计步骤
1. 需求分析：确定数据实体和关系
2. 概念设计：ER 图
3. 逻辑设计：表结构设计
4. 物理设计：索引和分区

## 关系型数据库
- MySQL / PostgreSQL
- 事务支持
- ACID 特性
- 适合结构化数据

## NoSQL 数据库
- MongoDB：文档型
- Redis：键值对
- Cassandra：列式存储
- 适合非结构化数据

## 优化
- 索引优化
- 查询优化
- 缓存策略

## 何时激活
- 新建表 / 重新设计 Schema 时
- 查询慢、索引失效、N+1 时
- 选关系型还是 NoSQL 纠结时

## 核心概念
- 范式与反范式：规范化代价是 JOIN，反范式换读性能
- 索引：加速读但拖慢写，选择性高的列优先
- 事务边界：ACID 与隔离级别
- 缓存一致性：Cache-Aside / 写穿透

## 实践指南
1. 先画 ER 图，明确实体与关系
2. 热字段建索引，避免对低选择性列建索引
3. 大表加分区 / 分表，热数据进缓存
4. 写操作放进事务，控制锁粒度

## 注意事项
- 不要无脑加索引（写放大）
- 慎用 SELECT * 与深分页
- Redis 缓存要处理失效 / 穿透 / 雪崩

## 集成
- 与 `api-design` 配合：接口背后的数据模型
- 与 `architecture-design` 衔接：存储是架构的一部分

## 参考
- 《数据库系统概念》（Silberschatz）
- PostgreSQL / MySQL 官方文档
- Redis 官方最佳实践

