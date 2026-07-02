# Hermix 短期待办（PLAN_UI_ENHANCEMENT 交付后）

> **生成时间**：2026-06-23 00:32 GMT+8
> **上下文**：[`docs/PLAN_UI_ENHANCEMENT_2026_DELIVERY.md`](./PLAN_UI_ENHANCEMENT_2026_DELIVERY.md) §"已知限制"记录了 3 条遗留，本清单负责第 1 + 第 3 条（剩下那条是 `Avatar role="assistant"` 视觉差异，留给后续接入专家 avatar 时处理，零额外工作）。
> **原则**：每条独立可做；做完一条划一条；新加 TODO 也放这里。
> **与 `MIGRATION_TODO.md` 的区别**：那份是 v6 架构迁移（8 阶段 / 5.5 周长跑），本份是交付后短期收尾（单条 ≤ 1 天）。

---

## §1 token / cost 持久化（PLAN_UI_ENHANCEMENT §6.1 限制 #1）

**位置**：
- 代码标记：`crates/hermix-agent/src/session_persistence.rs` `aggregate_usage` 上方 `TODO(token-persist)` 注释块
- 聚合入口：`crates/hermix-server/src/dashboard/analytics.rs::get_usage`
- 前端读：`apps/desktop-tauri/src/components/AnalyticsView.tsx`（已经在调 `get_usage`，零改动）

**现状**：sessions 表无 token / cost 列；`aggregate_usage` 真实返回 0（不虚拟填充）。前端 Statistics 显示 0 token + 真实 session/消息数。

**修复路径**（推荐 B，纯加法，零破坏性）：

- [ ] 在 `crates/hermix-core/src/message.rs`（或现有 `Message` 定义处）加 `usage: Option<Usage>` 字段
  - `Usage { prompt_tokens, completion_tokens, total_cost: Option<f64> }`
  - `#[serde(default)]` 保持向后兼容
- [ ] 在 `crates/hermix-agent/src/agent_loop.rs` 拿到 LLM response 后，把 `response.usage` 塞进要写盘的 Message
- [ ] `flush_messages` 把 usage 序列化进 `messages.extra` JSON（不动 schema）
- [ ] `aggregate_usage` 加聚合：
  ```sql
  SELECT
    COALESCE(SUM(json_extract(extra, '$.usage.prompt_tokens')), 0),
    COALESCE(SUM(json_extract(extra, '$.usage.completion_tokens')), 0),
    COALESCE(SUM(json_extract(extra, '$.usage.total_cost')), 0.0)
  FROM messages
  WHERE role = 'assistant' AND created_at >= ?1
  ```
- [ ] `UsageAggregate` 加 `total_input_tokens / total_output_tokens / total_cost` 字段
- [ ] `analytics::get_usage` 把这 3 个字段塞进 JSON 响应
- [ ] 前端 `AnalyticsView` 已有占位 UI，把 0 换成真值即可

**验收**：跑 5 个对话后 Statistics 页面 4 个数字全非零；旧 session（无 usage）显示 0 不报错。

**优先级**：P1（用户能直接看到收益）

---

## §2 后端 polish 端点（PLAN_UI_ENHANCEMENT §1.3 / §7.1 限制 #3）

**位置**：
- 代码标记：`apps/desktop-tauri/src/api/polish.ts` `callPolishEndpoint` 上方 `TODO(polish-backend)` 注释块
- 前端调：`PolishButton.tsx` / `AiGenerateButton.tsx`（已经在调 `polishChat` / `polishAutomation`，零改动）
- 后端未建：`crates/hermix-server/src/dashboard/polish.rs`（待建）

**现状**：fetch 返回 null 时降级为"原文压缩空白"；润色按钮目前只是空格整理，没有真正的语义改写。

**实现路径**：

- [ ] 新建 `crates/hermix-server/src/dashboard/polish.rs`（参考 `dashboard/analytics.rs` 模式）
  - `POST /api/polish/chat` handler：入参 `{ draft, ctx? }`，出参 `{ ok, text, error? }`
  - `POST /api/polish/automation` handler：同上，独立端点
- [ ] 在 `dashboard/mod.rs::router()` 加 2 行：
  ```rust
  .route("/api/polish/chat", post(polish::polish_chat))
  .route("/api/polish/automation", post(polish::polish_automation))
  ```
- [ ] handler 从 `HttpServerState` 拿 LLM 客户端：
  - 选项 A：通过 `agent_service` trait 暴露 chat 接口（侵入小）
  - 选项 B：在 `HttpServerState` 加 `llm_client: Arc<Mutex<...>>` 字段（更直接，但要改 `build*` 初始化）
  - 推荐 A
- [ ] handler 拼模板：`format!("{}\n\n{}", CHAT_POLISH_PROMPT, draft)`（prompt 字符串前端 `polish.ts` 已导出，后端要复制定义或把 prompt 模板移到一个 `hermix-prompts` crate 共享）
- [ ] 失败 / 超时（>8s）返回 `{ ok: false, error }`；前端已有降级路径
- [ ] 端点上线后：前端零改动，`callPolishEndpoint` 自动拿到真润色结果

**验收**：
- `curl -X POST localhost:端口/api/polish/chat -d '{"draft":"写个 hello world 脚本"}'` 返回润色后文本
- 端点宕机 / LLM 超时时前端按钮仍可用（降级原文）
- 改后端 prompt 不需要前端重新构建

**优先级**：P1（直接影响"润色"按钮的语义有效性）

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
