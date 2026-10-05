---
name: "frontend-architecture"
display_name: "前端架构"
category: "software-development"
description: "用于设计前端架构、选择框架（React / Vue / Angular）、规划渲染策略（CSR / SSR / SSG），或优化前端性能。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# 前端架构 Skill

## When to Activate

Use this skill when:
- 需要做框架选型（React / Vue / Angular）或评估迁移方案
- 需要设计渲染策略（CSR / SSR / SSG / ISR）
- 需要选择状态管理方案（本地 / 全局 / 服务端状态）
- 需要进行前端性能优化（代码分割、懒加载、虚拟列表、缓存）

Do not activate for:
- 纯后端 API 设计（用 api-design skill）
- 移动端原生开发（React Native / Flutter 有独立上下文）
- 纯 CSS 样式调整（不需要架构级决策）

## Core Concepts

### 框架选型

| 框架 | 适用场景 | 核心优势 |
|------|---------|---------|
| React | 大型应用、生态丰富 | 组件化、社区庞大 |
| Vue | 快速开发、中小项目 | 易学易用、渐进式 |
| Angular | 企业级、复杂应用 | 全家桶、强约定 |

### 渲染策略

- **CSR**（客户端渲染）：首屏慢、交互快，适合后台管理
- **SSR**（服务端渲染）：首屏快、SEO 好，适合内容站
- **SSG**（静态生成）：构建时渲染，适合文档/博客
- **ISR**（增量静态再生）：SSG + 按需重建，适合电商列表页

### 状态管理分层

- **本地状态**：`useState` / `ref` — 组件内 UI 状态
- **全局状态**：Redux / Zustand / Pinia — 跨组件共享状态
- **服务端状态**：React Query / SWR / Apollo — 缓存 + 同步后端数据

### 性能优化清单

1. 代码分割（路由级 / 组件级 lazy import）
2. 懒加载（图片 / 非关键组件）
3. 虚拟列表（长列表场景）
4. 缓存策略（HTTP 缓存 + 客户端缓存 + Service Worker）

## Practical Guidance

### 选型决策流程

```
用户量和复杂度 → 框架选择
├─ 小型/中型 + 快速交付 → Vue
├─ 大型 + 复杂交互 → React
└─ 企业级 + 强规范 → Angular

渲染需求 → 策略选择
├─ 需要 SEO → SSR / SSG
├─ 纯 SPA → CSR
└─ 内容半静态 → ISR
```

### 常见陷阱

- 不要为了"技术先进"选框架，要为了团队能力和项目需求选
- 状态管理不要一上来就上 Redux/Zustand，先用 local state 够不够
- SSR 不是银弹，增加了服务端复杂度和部署成本

## Gotchas

- React 的 `useEffect` 依赖数组容易导致闭包陷阱，优先用 `useRef` 或拆分 effect
- Vue 3 的 `reactive` 丢失响应性时检查是否解构了顶层对象
- SSG + ISR 的 `revalidate` 时间设置过短会导致大量重建，浪费构建资源
- 虚拟列表不适合需要频繁动态高度的场景，考虑用 `@tanstack/virtual`

## Examples

```tsx
// React: 路由级代码分割
const Dashboard = lazy(() => import('./pages/Dashboard'));

// React Query: 服务端状态管理
const { data, isLoading } = useQuery({
  queryKey: ['todos'],
  queryFn: fetchTodos,
  staleTime: 5 * 60 * 1000, // 5 分钟缓存
});
```

## Integration

- 与 `testing` skill 配合：架构决策影响测试策略（SSR 需要集成测试，CSR 可以纯单元测试）
- 与 `performance` skill 配合：性能瓶颈分析 → 架构调整
- 与 `api-design` skill 配合：前后端接口设计影响状态管理方案选择
- 与 `refactoring` skill 配合：遗留项目架构升级时的渐进式迁移

## References

- React 文档: https://react.dev
- Vue 文档: https://vuejs.org
- Web Vitals: https://web.dev/vitals
