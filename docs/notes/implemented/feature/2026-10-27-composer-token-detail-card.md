# Decision Note: 输入框上方的 Token 消耗明细卡片

Status: implemented — 2026-10-27 落地

## Decision

对话输入框上方原先只有一行「上下文百分比」小圆点，缓存命中率被塞进 `title` 属性里（鼠标悬停才有一行字）。现在换成「图标 + 焦点/悬停弹出卡片」：

- **入口**：`components/TokenUsagePopover.tsx` 的图标按钮（`Gauge`，`aria-label="Token 消耗明细"`），悬停 / 键盘聚焦 / 点击都会打开卡片；悬停离开有 160ms 宽限（移到卡片上不关），Esc 与点击外部关闭。上下文百分比小圆点保留在同一行（不删既有信息）。
- **卡片**：输入（缓存命中 / 未命中 / 写入）→ 输出（思考过程 / 回复内容）→ 缓存命中率（数值 + 命中/写入/未命中三段堆叠条 + 图例）→ 上下文（已用 / 上限 + 进度条）。生成中卡片每 3s 刷新（每次 API 调用结束即落库，读数即当前账）。
- **数据**：新增 `GET /v1/sessions/{id}/usage` → `SessionPersistence::session_usage()`，对 `token_usage` 按 `session_id` 求和（一行 = 一次 API 调用，见 2026-10-27 使用统计口径笔记）。字段：prompt/completion/cache_read/cache_creation/reasoning/estimated_cost/calls/untrusted_calls（serde camelCase）。聚合在阻塞池执行（同步 SQLite 不占 tokio worker）。
- **口径**（前后端一致，卡片里直接体现）：`promptTokens` 含缓存命中 → 未命中 = `prompt − cache_read`；`completionTokens` 含思考 → 回复 = `completion − reasoning`。命中率 = `cache_read / (cache_read + 未命中)`。
- **上下文**不走 SQL：取流式 `contextUsage`（实时），无实时值时由后端 `sessions.last_context_usage` 恢复；与 Token 数字分开标注，避免两套口径打架。

## Alternatives considered

- **只展示「本轮」快照**：`usageSnapshot` 在 WS 事件里现成，但用户问的是「这个会话花了多少」，单轮数字每次都重置，无法回答。会话级 SQL 聚合才是问题的答案。
- **把卡片放进 Composer 内部（顶部工具行）**：Composer 的工具行有 `toolbarCompact` 自适应收缩逻辑（ResizeObserver 测量后隐藏 chip），塞进去要额外处理收缩档位；而 ChatView 在 Composer 正上方已有一行现成的上下文行，替换它零布局风险。
- **前端自行累计（把每轮 usage 相加）**：恢复会话后历史轮次的数字拿不到（WS 只推当前轮的），刷新页面即丢失；SQL 聚合天然覆盖历史。

## Consequences

- 多一个 `GET /v1/sessions/{id}/usage` 请求，仅在卡片打开时发起（生成中每 3s 一次），不常驻轮询。
- 卡片数字与「会话恢复」不冲突：历史轮次在 `token_usage` 里，全部计入。
- 与全局分析页（`/api/analytics/usage`）同源同口径，两处数字可以直接对账。
- `untrusted_calls > 0` 时卡片图例右侧显示「N 次上报异常」——沿用本地锚点替换的同一套标记。
