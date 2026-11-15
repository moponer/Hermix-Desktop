---
name: "architecture-design"
display_name: "架构与设计决策"
category: "software-development"
description: "系统/接口/数据库/前端四个层面的架构决策：给多方案对比与取舍理由，输出可落地设计而非空洞原则。用于：新系统设计、技术选型拿不准、接口/表结构设计、前端框架与渲染策略选型。不适用于：具体代码实现细节（走 code-review/tdd-practice）、纯视觉样式（走 impeccable-design）。"
version: "0.2.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin", "architecture"]
---

# 架构与设计决策

覆盖四个层面：系统架构、API 设计、数据库设计、前端架构。共同纪律：先复述
目标与约束再给方案；至少两条候选路线并说清"为什么选它、放弃了什么"；
粒度到模块边界与数据流，不代替详细设计。

## 工具接线

1. 上下文不足时先 `delegate_task`（Explore 角色）摸清现状：现有模块边界、
   数据流、依赖版本，返回结论后再设计。禁止凭想象设计改造方案。
2. `read_file` 带 offset/limit 抽查关键文件（入口、路由表、schema 定义），
   单次 ≤ 500 行；确认结构与假设一致。
3. 决策含持久取舍时按项目文档制度落到 `docs/notes/proposed/`，不在聊天里
   一发了事。

## 1. 系统架构

- **先画边界上下文再定服务**：识别核心复杂度与偶然复杂度，能单体就别微服务
  （分布式是成本，不是勋章）。
- **反例**：三个人的项目上微服务 + 消息队列——运维成本先于业务价值到达。
- **正例**：单体 + 模块边界（`ui → features → services` 单向依赖），留出演进缝。
- 数据一致性优先最终一致，慎用分布式事务；热点路径用异步/事件解耦。

## 2. API 设计

- **契约优先**：先定 Schema（OpenAPI / protobuf）再实现；错误用统一结构 +
  稳定错误码，别把堆栈抛给调用方。
- **反例**：URL 里带 token、错误响应 `{"msg": "error"}` 无错误码——调用方
  无法程序化处理。
- **正例**：`GET /api/v1/users/{id}` + `404 {"code":"user_not_found"}`；
  幂等写操作考虑重试安全；分页/限流是稳定性底线。
- 别为小项目上重协议：GraphQL/gRPC 的成本（工具链、缓存、学习）先于收益。

## 3. 数据库设计

- **先 ER 后建表**：实体关系画清楚再写 DDL；热字段建索引，低选择性列不建
  （写放大）。
- **反例**：`SELECT *` + 深分页 + 每列都建索引——读放大与写放大同时发生。
- **正例**：外键约束 + CHECK 进库；`SELECT` 只取需要的列；大表考虑分区；
  缓存处理失效/穿透/雪崩三件事。
- 选型纠结时先看数据形态：结构化 + 强事务 → 关系型；半结构化高频写 → 文档型；
  简单键值 → Redis。别按流行度选。

## 4. 前端架构

- **状态分层**：本地 `useState` → 全局 store → 服务端状态（React Query 类）。
  **反例**：一上来上 Redux，本地状态明明够用。
- **渲染策略**：需要 SEO → SSR/SSG；纯后台 → CSR；半静态内容 → ISR。
  SSR 增加服务端复杂度，不是银弹。
- **性能清单**（按序做，别全上）：路由级代码分割 → 图片懒加载 → 虚拟列表
  （仅长列表）→ 缓存策略。
- **Gotcha**：React `useEffect` 依赖数组闭包陷阱优先拆 effect 而非堆依赖；
  Vue 3 解构丢响应性先查顶层对象。

## 输出契约

每个层面给 2~3 个候选方案：一句话架构描述 + 优劣 + 适用前提 → 明确推荐
一个并说明放弃了什么 → 关键风险与验证手段（原型/压测/灰度）。遵循系统提示
REPORT_SECTIONING 分节规则。中文输出，术语首次出现给英文原词。

## 参考

- 《企业应用架构模式》（Fowler）、《领域驱动设计》（Evans）、
  《设计数据密集型应用》（Kleppmann）
- OpenAPI Specification、Google API Design Guide
- web.dev Vitals（前端性能指标）
