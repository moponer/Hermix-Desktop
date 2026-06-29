---
name: "architecture-design"
display_name: "系统架构设计"
category: "software-development"
description: "单体 vs 微服务、领域驱动设计(DDD)、高可用与高扩展方案设计方法论。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# 系统架构设计 Skill

## 概述
提供系统架构设计的最佳实践，包括单体架构、微服务架构、事件驱动架构等。

## 核心原则
1. **单一职责**：每个模块只负责一个功能
2. **松耦合**：模块间依赖最小化
3. **高内聚**：相关功能集中在同一模块
4. **可扩展**：支持水平和垂直扩展

## 架构模式
- **单体架构**：适合小型项目，开发部署简单
- **微服务架构**：适合大型项目，独立部署和扩展
- **事件驱动**：适合异步处理和解耦
- **CQRS**：读写分离，优化查询性能

## 设计步骤
1. 需求分析和功能拆分
2. 选择架构模式
3. 定义服务边界
4. 设计数据模型
5. 规划部署架构

