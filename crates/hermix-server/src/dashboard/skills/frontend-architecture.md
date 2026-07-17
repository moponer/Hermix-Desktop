---
name: "frontend-architecture"
display_name: "前端架构"
category: "software-development"
description: "用于：前端框架选型（React/Vue 等）、渲染策略、状态管理、性能优化。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# 前端架构 Skill

## 概述
现代前端开发需要考虑架构设计、性能优化、用户体验。

## 框架选型
- React：生态丰富，适合大型应用
- Vue：易学易用，适合快速开发
- Angular：企业级，适合复杂应用

## 渲染策略
- CSR：客户端渲染
- SSR：服务端渲染
- SSG：静态生成
- ISR：增量静态再生

## 状态管理
- 本地状态：useState
- 全局状态：Redux, Zustand
- 服务端状态：React Query, SWR

## 性能优化
- 代码分割
- 懒加载
- 虚拟列表
- 缓存策略

## 何时激活
- 新项目选型框架 / 渲染模式时
- 首屏慢、包体大、交互卡顿时
- 设计状态管理方案时

## 核心概念
- CSR / SSR / SSG / ISR 的取舍：SEO 与首屏 vs 复杂度
- 状态分层：本地 / 全局 / 服务端状态分开管
- 渲染瓶颈常在"主线程长任务"与"大列表"

## 实践指南
1. 先定渲染策略（内容型 SSR，应用型 CSR）
2. 状态按来源分层，别全塞全局 store
3. 用代码分割 + 懒加载砍首屏体积
4. 长列表虚拟滚动，大计算搬 Web Worker

## 注意事项
- 别为小项目上过度架构
- SSR 注意服务端状态与 hydration 成本
- 缓存要处理失效

## 集成
- 与 `architecture-design` 衔接：前端也是架构的一环
- 与 `api-design` 配合：前端消费后端接口

## 参考
- React / Vue 官方文档
- web.dev 性能指南（Google）
- Next.js / Nuxt 渲染模式文档

