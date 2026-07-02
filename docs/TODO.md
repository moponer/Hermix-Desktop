# Hermix 短期待办（PLAN_UI_ENHANCEMENT 交付后）

> **生成时间**：2026-06-23 00:32 GMT+8
> **更新**：2026-07-02 收尾。§1 + §2 已落地（见 commits `fa645cd` / `c4077e0`）。§3 留待 P3。
> **上下文**：[`docs/PLAN_UI_ENHANCEMENT_2026_DELIVERY.md`](./PLAN_UI_ENHANCEMENT_2026_DELIVERY.md) §"已知限制"记录了 3 条遗留，本清单负责第 1 + 第 3 条（剩下那条是 `Avatar role="assistant"` 视觉差异，留给后续接入专家 avatar 时处理，零额外工作）。
> **原则**：每条独立可做；做完一条划一条；新加 TODO 也放这里。
> **与 `MIGRATION_TODO.md` 的区别**：那份是 v6 架构迁移（8 阶段 / 5.5 周长跑），本份是交付后短期收尾（单条 ≤ 1 天）。

---

## §1 token / cost 持久化（PLAN_UI_ENHANCEMENT §6.1 限制 #1）

**状态**：✅ 完成（commit `fa645cd` 2026-07-02）

**落地**：
- `Message` 已含 `usage` 字段（`prompt_tokens` / `completion_tokens` / `cache_*` / `reasoning_tokens` / `estimated_cost`）。
- `hermix-agent::persist_token_usage()` 写入 `token_usage` 表（每次 LLM call 一次）。
- `aggregate_usage()` 通过 LEFT JOIN + SUM 拿真实数字，不再 0 占位。
- `AnalyticsResponse.total_input_tokens` / `total_output_tokens` / `total_estimated_cost` 已 expose。
- 前端 `AnalyticsView` 已经读这些字段，零前端改动。

---

## §2 后端 polish 端点（PLAN_UI_ENHANCEMENT §1.3 / §7.1 限制 #3）

**状态**：✅ 完成（commit `c4077e0` 2026-07-02）

**落地**：
- `crates/hermix-server/src/dashboard/polish.rs` 已建。
- `POST /api/polish/chat` + `POST /api/polish/automation` 已挂到 `dashboard::router()`。
- 走 `HttpServerState::agent_service()` 暴露的新 getter + ad-hoc `__polish__<uuid>` session id，调用完 `reset_session()` 清掉，不污染用户 chat history。
- prompt 模板（`CHAT_POLISH_PROMPT` / `AUTOMATION_POLISH_PROMPT`）Rust 端定义为 const + snapshot tests 锁定（drift against 前端 TypeScript 副本 → CI fail）。
- 12s `tokio::time::timeout` 映射到 504，前端拿 null 走原 whitespace fallback。

---

## §3 头像/时间视觉差异（PLAN_UI_ENHANCEMENT §1.1 限制 #2 — 留待后续）

**位置**：
- 数据线已通：`hooks/useExpertCutoff.ts` 返回 `expertCutoffAt`
- `MessageBubble` 已接 prop
- 渲染层未动：`Avatar role="assistant"` 仍写死走 Hermix 内置头像

**现状**：切专家时头像/时间视觉无差异（数据线备好，但还没接专家 avatar）。

**为什么单独列出**：未来接入"专家自带头像 URL"字段时，**仅需在 `MessageBubble` 的 `<Avatar>` 处按 `expertCutoffAt` 取对应头像即可，零额外工作**。等专家库里有头像数据再做。

**优先级**：P3（被动等待）

---

## 原则

- 每条 TODO 不依赖其他 TODO（独立可做）
- 任何"今天顺手能做的 P1"做完直接划掉；不确定优先级的放 P2/P3 留 review
- 写新 TODO 也加到本文件，**不要散落到代码里**（代码里只用 `// TODO(关键词):` 简短标记 + 引用本文件 §N）
- 完成时同步更新 `docs/PLAN_UI_ENHANCEMENT_2026_DELIVERY.md` "已知限制"段
