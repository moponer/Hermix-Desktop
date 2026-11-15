# Decision Note: 使用统计口径做真：本地锚点 + 逐调用落库 + 公开价目表

Status: implemented — 2026-10-27 落地

## Decision

使用统计页（`/api/analytics/usage`）的三个数字此前各有一个"不真"的根源，本次在同一链路上收口：

1. **Token 上报虚高 → 本地锚点校验 + 替换**。kilo 类免费代理上报的 `prompt_tokens` 是会话级累计计数器（实测：第 7 分钟报 2.8M，真实单次 ~17K），原样落盘导致 15.8M 输入、4.2M 日柱全部虚高。现在 `process_post_response` 在任何下游（cost guard / DB）看到用量之前做两个检测：
   - **累计计数器**：上报值 ≥ 1.5 × 之前所有调用上报之和（物理上不可能的单次请求量，是 running total 的精确签名）→ 替换为边际值（上报 − 前和，即本次真实 prompt）；
   - **锚点离群**：上报值 > 6 × 本地估算 且 > 25K（下限编码系统提示+工具 schema 的固定开销，防止小上下文首误报）→ 替换为本地估算。
   - 本地估算器 `estimate_prompt_tokens`：ASCII ≈ 4 字符/token、CJK ≈ 1 字/token（修正 chars÷4 对中文 ~1.8× 的低估）。completion 同理用响应文本做上界锚点。
   - 命中行的值已替换为可信估计，`untrusted` 列标记，仪表盘模型行显示「N 次上报异常，已按本地估算」。
2. **一行 = 一个 turn → 一行 = 一次 API 调用**。`persist_token_usage` 从 finalize（每 run 一行累计）移到 `process_post_response`（每次成功响应一行），行内数字即该次请求的计费口径，费用/缓存/token 语义全部对齐；finalize 的旧落库块删除（避免双计）。逐行打开 SQLite WAL 的开销（<1ms）相对 LLM 调用延迟可忽略。
3. **费用 $0.0000 → 按状态区分**。此前 `estimated_cost` 恒 NULL（provider 从不填），仪表盘全显示 $0.0000（是"没数据"不是"免费"）。现在：
   - 每次调用用 `hermix-intelligence` 价目库算价并盖章落盘（config 自定义费率 > 价目库 > 旧 fallback 表）；
   - DeepSeek 价目更新到 2026-08 峰谷调价后官方价（峰时上界，缓存命中分列），新增 MiniMax M3 / M3.1-Flash / M2.5（2026-10 快照）；
   - 路由解析学会 `provider:model` 前缀剥离（`minimax:MiniMax-M3`、`custom-free:kilo:...`）与免费代理识别（`custom-free` → SubscriptionIncluded，$0 是真的免费）；
   - 未知价模型 UI 显示「—」而不是 $0.0000（`cost_known` 字段）。
4. **缓存命中率**：实测路径已对（前端 `cache_read/(cache_read+input)`），但分母此前被 kilo 虚高污染——锚点替换后自动修正。静态估算回退保留但仅用于 provider 完全不报缓存的场景（UI 标「估算」）。

## Alternatives considered

- **把锚点存进 `UsageStats`/`token_usage` 新列、保留上报原值**：需要改 ~24 处构造点，且 UI 要同时展示两个口径，复杂度高；替换 + `untrusted` 标志 + warn 日志（含原值）达到同样的可审计性。
- **只在前端过滤离群值**：DB 里仍是脏数据，`/insights`、cost guard 全被污染；校验必须在记账入口。
- **OpenRouter `/api/v1/models` 活价目缓存**：数据更新但引入运行时网络依赖（国内可达性），静态快照已覆盖当前用量模型；未覆盖的显示「—」比拉到过期价诚实。
- **MiniMax M2.7 价目**：公开源标价 $0.000（促销或数据异常），宁缺毋滥，显示「—」。

## Consequences

- `token_usage` 口径变更：旧行是"每 run 累计"、新行是"每调用"，混窗口聚合时两者都直接 SUM，语义兼容（都是计费口径）；但日环比跨越切换点时 token 消耗会因口径更细而略降。
- 累计计数器代理的消耗从"天文数字"变为"本地估算"，仍非计费级精确（估算器无 tokenizer）；要看精确值用 `server.log` 的 `prompt_cache: assembled prompt`。
- 免费代理行现在显示 $0.0000（真免费），已知价模型显示真估算，未知模型显示「—」。
- Anthropic 原生协议的费用估算会略偏低（其 `prompt_tokens` 不含缓存命中，减法多扣一次）；当前主要流量走 OpenAI 兼容协议（prompt 含缓存），误差可接受，已注明。

## Reintroduction conditions

若接入 tokenizer 精确计数（tiktoken-rs / 各家 tokenizer），`estimate_prompt_tokens` 可替换为真计数，25K 下限与 6× 阈值随之收紧；若接入 OpenRouter 活价目，静态 `official_pricing_db` 降级为离线兜底。
