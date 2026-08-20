# Hermix Desktop 代码分析报告：上帝文件与废弃文件专项

> 分析日期：2026-08-15
> 范围：Rust workspace（`crates/` 21 crate）+ 前端（`apps/desktop-tauri/`）+ `apps/hx-shell/`
> 方法：全量行数普查 + 10 个隔离子代理并行深挖（职责 / 耦合 / 依赖 / 拆分 / 死文件）
> 度量基线：源码约 **31.4 万行**（Rust `crates/` ≈ 17.2 万 + 前端/其他 ≈ 14.1 万，已排除 `node_modules`/`target`/`release`）

---

## 0. 执行摘要

- **上帝文件集中度极高**：Top 23 个源文件均 ≥ 1800 行，单体最大 `hermix-server/src/lib.rs`（8921 行）。`hermix-agent` 一个 crate 贡献了 7 个 4000+ 行文件。
- **两类上帝问题并存**：
  1. **"大泥球"型**（单文件混合多关注点）：`lib.rs`、`agent_loop/mod.rs`、`session_persistence.rs`、`local_agent_service.rs`、`compression.rs`、`embedded.rs`、`backends/browser.rs`、`ChatView.tsx`、`App.tsx`、`MessageBubble.tsx`、`api.ts`。
  2. **"平行重复"型**（同一抽象被复制 N 份）：`hermix-gateway/src/platforms/*` 10 个平台文件（1443–2141 行）各重写一套收/发/媒体/交互逻辑，仅 SDK 不同；`provider.rs` 4 个 provider 各复制 ~700–900 行流式解析。
- **废弃文件可安全清理**：Rust 侧 5 个（含整目录 `memory_plugins/`）、前端侧 11 个文件 + 1 个死导出，均为高置信度孤儿/重复旧版，**删除无功能回退**。
- **预估重构总工作量**：上帝文件拆分约 **123–163 人日**（见各集群明细）；死文件清理约 **0.5–1 人日**（零风险，建议立即执行）。

---

## 1. 上帝文件清单（按严重度排序）

| # | 文件路径 | 行数 | 类型 | 核心问题 |
|---|----------|------|------|----------|
| G1 | `crates/hermix-server/src/lib.rs` | 8921 | 大泥球 | 所有 HTTP/WS handler + 巨型共享 State 堆一文件 |
| G2 | `crates/hermix-agent/src/agent_loop/mod.rs` | 6357 | 大泥球 | `AgentLoop` 47 字段 struct，调度/LLM/流/记忆/收敛/渲染全混一 impl |
| G3 | `crates/hermix-agent/src/session_persistence.rs` | 4715 | 大泥球 | 连接+迁移+DDL+CRUD+DTO+序列化全塞一个 struct |
| G4 | `crates/hermix-agent/src/local_agent_service.rs` | 4437 | 上帝门面 | `LocalAgentService` 抱全部依赖，单法 `send_message_stream` 约 1100 行 |
| G5 | `crates/hermix-agent/src/compression.rs` | 4567 | 大泥球 | 策略+脱敏+预算+摘要+主流程+熔断+trait 桥全一文件 |
| G6 | `crates/hermix-focus/src/embedded.rs` | 3606 | 大泥球 | 模型+DB+检索+打分+嵌入+适配器全一 struct，~1000 行内嵌测试 |
| G7 | `crates/hermix-agent/src/provider.rs` | 3164 | 平行重复 | 4 个 provider 各复制流式解析 ~700–900 行 |
| G8 | `apps/desktop-tauri/src/components/ChatView.tsx` | 3355 | 大泥球 | 列表+输入框+模型选择+错误卡+面板+~50 props 透传 |
| G9 | `apps/desktop-tauri/src/App.tsx` | 3141 | 大泥球 | 路由+全局状态(56 useState)+初始化+流式解析+模态 |
| G10 | `crates/hermix-gateway/src/gateway.rs` | 2899 | 大泥球 | 适配器注册+入站泵+路由+命令+看门狗+impl 混合 |
| G11 | `apps/desktop-tauri/src/components/MessageBubble.tsx` | 2702 | 大泥球 | 30+ 渲染器 if-else 分发，无类型化子组件 |
| G12 | `crates/hermix-agent/src/run_loop.rs` | 2684 | 大泥球(与 G2 同 struct) | `AgentLoop` 第二 impl，循环编排 |
| G13 | `apps/desktop-tauri/src/api.ts` | 2424 | 大泥球 | 200+ 函数 + 全部类型 + 缓存 + 熔断一文件 |
| G14 | `crates/hermix-agent/src/context.rs` | 2292 | 大泥球 | ContextManager + SystemPromptBuilder + 分层审计 |
| G15 | `crates/hermix-gateway/src/groupchat/runtime.rs` | 2254 | 大泥球 | 调度执行+审计调用+争议裁决一身兼任 |
| G16 | `crates/hermix-tools/src/backends/browser.rs` | 3296 | 大泥球 | CDP 会话/页面/截图/下载/网络/a11y/云端/注册表 11+ 关注点 |
| G17 | `crates/hermix-tools/src/tools/url_safety.rs` | 2051 | 大泥球 | URL 解析+网站策略+Tirith+SSRF 混合 |
| G18 | `crates/hermix-tools/src/approval.rs` | 1944 | 大泥球 | 请求模型+决策引擎+持久化混合 |
| G19 | `crates/hermix-tools/src/registry.rs` | 1663 | 大泥球 | 注册+元信息+校验+协议生成混合 |
| G20 | `apps/desktop-tauri/src/components/RightSidebar.tsx` | 1888 | 大泥球 | 三 tab + 预览 + 文件菜单堆一组件 |
| G21 | `apps/desktop-tauri/src/hooks/useStreamChat.ts` | 1872 | 大泥球 | WS 连接/重连/超时/批处理/事件解析/phase 机一 hook |
| G22 | `apps/desktop-tauri/src/components/SettingsView.tsx` | 1477 | 大泥球 | 多 Tab + 子面板 + 本地状态 + api 调用 |
| G23 | `apps/desktop-tauri/src-tauri/src/commands_cua.rs` | 1109 | 大泥球 | 20+ Tauri 命令 + OS 分支 + err 映射堆一文件 |

> 注：`crates/hermix-agent/src/stream_collect.rs`(1647)、`prompt_assembly.rs`(1478)、`compression_pipeline.rs`(1199)、`compression_ops.rs`(1136)、`memory_prefetch.rs`(1161)、`agent_config.rs`(1122)、`skill_orchestrator.rs`(1105)、`skill_loader.rs`(1046) 等作为上述集群的从属文件一并分析，拆分方案见对应集群。

---

## 2. 上帝文件深度分析与拆分方案

### G1 · `hermix-server/src/lib.rs`（8921 行）— 服务端入口上帝文件

**承载职责（带行号）**
- 共享状态 `HttpServerState` + `ApprovalRegistry`：1054–1945（30+ 字段，含多处 `Mutex`/`Arc`/`broadcast`）
- 路由装配 `router()` + 静态资源 + 中间件：1993–2168
- 健康/metrics/protocol/experts/tools：2278–2485
- sessions REST 全套（CRUD/消息/发送/undo/restore/lineage/search/archive）：2454–3763
- tool 审批 REST 兜底：3595–3695
- WS 单聊 `ws_stream_send`（约 1767 行）：4095–5862
- WS groupchat：5862–6536；WS consciousness 流：6536–7136
- 审批副作用/redact：7136–7657；voice STT/TTS：7632；tickloop：7715–7830
- tasks CRUD：8322–8655；automations CRUD：8712–8921
- 测试（`#[cfg(test)]`）：约 2000 行内嵌

**成因**：所有 handler 共享一个巨型 `HttpServerState`，任一字段变更波及全部路由；`ws_stream_send` 把去重、活动会话锁、agent loop、stream 转发全内联。依赖 17 个 hermix-* crate，但自身是叶子 crate（仅 `hermix` 二进制依赖），**拆分无上游风险**。

**影响**：① 单文件 8921 行，任何路由改动都需加载全文件，评审成本高；② 共享可变状态导致并发 bug 难定位；③ 测试与 prod 混排，CI 编译慢。

**拆分方案（依赖方向：`router → routes → state → hermix-*`）**

| 模块 | 职责 | 依赖 | 搬入内容 |
|------|------|------|----------|
| `state.rs` | `HttpServerState`/`ApprovalRegistry`/build | hermix-* + dashboard types | 1054–1945, 143–289 |
| `routes/sessions.rs` | sessions+experts+tools+metrics+health | state | 2278–3763, 2301–2485 |
| `routes/approval.rs` | REST 审批兜底 + 副作用 | state, tools | 3595–3695, 7136–7462 |
| `routes/ws_chat.rs` | 单聊 WS + 流式发送 | state, core, agent | 3894–5862 |
| `routes/ws_groupchat.rs` | groupchat WS | state, gateway | 5862–6536 |
| `routes/ws_stream.rs` | consciousness 流 WS | state, agent | 6536–7136 |
| `routes/tasks.rs` / `routes/automations.rs` | CRUD | state, task/automation | 8322–8655, 8712–8921 |
| `routes/voice.rs` / `tickloop.rs` | STT/TTS、tickloop | state, voice/runtime | 7632, 7715–7830 |
| `router.rs` | `router()` 装配 + 静态 + 中间件 | 各 routes | 1993–2168 |
| `error.rs` | `HttpError`/`redact`/user_facing | tools | 7462–7657 |

**工作量与风险**：约 **8–12 人日**。风险：(1) `HttpServerState` 30+ 字段跨 handler 共享，迁移须保持同一 `Arc`；(2) `ws_stream_send` 须先抽纯函数（dedup_key/check_recent_send/转发）再迁移；(3) 内嵌 ~2000 行测试需同步迁移。

---

### G2+G12 · `agent_loop/mod.rs`(6357) + `run_loop.rs`(2684) + `stream_collect.rs`(1647) + `context.rs`(2292) + `prompt_assembly.rs`(1478)

**成因**：`AgentLoop` 是 47 字段巨型 struct，其 `impl` **分散在 3 个文件**（`mod.rs`/`run_loop.rs`/`tool_executor.rs`）。`mod.rs` 混合：调度总线、LLM 重试、流收集、记忆/压缩、hook 审批、收敛判定、ACUI 渲染、finalize。无循环依赖（单向 `mod.rs ← run_loop/tool_executor ← stream_collect`），但"巨型 struct + 多 impl 分散直读字段"造成事实强耦合。

**拆分方案（依赖方向：`scheduler → {phase,tool_exec,render,approval,convergence,memory,stream} → context/prompt_assembly → types`）**

| 模块 | 职责 | 搬入内容 |
|------|------|----------|
| `loop/phase.rs` | 状态机 `LoopPhase`/`LoopState` + `notify_phase` | mod.rs L502 |
| `loop/tool_exec.rs` | 工具执行（= `tool_executor.rs`） | `execute_tool_calls` |
| `loop/render.rs` | ACUI + stream 渲染下沉 | mod.rs L948-968 |
| `loop/approval.rs` | hook 审批 | mod.rs L1149 |
| `loop/convergence.rs` | 收敛/错误决策 | L2249/L600/L681/L2044 |
| `loop/memory.rs` | 记忆/压缩/recall | mod.rs L1159-1702 |
| `loop/scheduler.rs` | `run`/`run_stream` 编排 | run_loop.rs |
| `loop/stream.rs` | 流收集 | stream_collect.rs |
| `context.rs` / `prompt_assembly.rs` | 保持不变 | 被 scheduler 依赖 |

**工作量与风险**：约 **12–18 人日**。风险：(1) 47 字段 struct 被多 impl 直读，需先抽 `loop state` 子 struct 或统一 `&self` 传参；(2) 共享 `Arc<Mutex>` 字段跨模块移动后生命周期/借用需重审；(3) stream 生命周期（`collect_stream_inner` 与中断/超时耦合）搬移易破坏活动事件时序；(4) ~3700 行测试与 prod 同文件，路径变更需同步。

---

### G3+G4+G7 · `session_persistence.rs`(4715) + `local_agent_service.rs`(4437) + `provider.rs`(3164) + 从属文件

**成因**：历史增量开发把全部 public API 堆进单 struct。`SessionPersistence` 仅存 3 路径却 ~80 个 pub 方法（连接+迁移+DDL+CRUD+DTO）；`LocalAgentService` 抱 `config`/`tool_registry`/`session_persistence`/`provider_factory`/`tirith`/`consciousness`/`memory_router`，`send_message_stream` 单法 ~1100 行；`provider.rs` 4 个 provider 各复制流式解析。`LocalAgentService` 以 `Arc<dyn AgentService>` 被 `hermix-server` 与 `hermix-transport` 持有——**接口面不可破**。

**拆分方案（依赖方向：core → config → (tools/focus) → persistence/provider/skill → service 顶层）**

| 新模块 | 职责 | 搬入内容 |
|--------|------|----------|
| `persistence/schema.rs` | 表/索引 DDL + DTO | CREATE TABLE/INDEX, `ApprovalRecord` 等 |
| `persistence/migrations.rs` | 迁移 + `schema_migrations` 版本戳 | `ensure_db` 迁移块 |
| `persistence/repo.rs` | CRUD 门面 | `SessionPersistence` 主体 |
| `persistence/{messages,sessions,usage,trajectories}.rs` | 分域读写 | 对应方法 |
| `provider/sse.rs` | 流式解析 | `parse_sse_chunk`/`parse_openai_response`/char_stream |
| `provider/{generic,openai,anthropic,openrouter}.rs` | 各 provider | 对应 struct + impl |
| `provider/registry.rs` | 工厂 | `ProviderFactory`/`resolve_provider` |
| `service/{builder,chat,stream,model_routing,tool_dispatch,lifecycle}.rs` | 门面拆分 | `new`/`send_message`/`send_message_stream`/路由/闸门/`reset_session` |
| `skill/orchestrator.rs` `skill/loader.rs` | 保持 | 仅移出测试 |

`LocalAgentService` 改为 `service/mod.rs` 薄壳委托子模块；保持 `AgentService` trait 与 `::new` 签名不变。

**工作量与风险**：约 **20–27 人日**（最大块）。风险：(1) DB 迁移顺序与版本戳不可拆散，改 `ensure_db` 调用序会破坏旧库升级；(2) `LlmProvider` trait 在 core 不可改签名；(3) service 门面被 `hermix-server`/`hermix-transport` 引用的接口面不可破；(4) `send_message_stream` 千行状态流拆分易错。

---

### G5 · `compression.rs`(4567) + `compression_pipeline.rs`(1199) + `compression_ops.rs`(1136) + `memory_prefetch.rs`(1161)

**成因**：`compression.rs` 确为上帝文件（策略+脱敏+预算+摘要+主流程+熔断+trait 桥）。但 `compression_pipeline.rs` 与 `compression.rs` **重叠很小**（前者是逐 Pass 原语，后者 `compress()` 编排组合），唯一重复是 `role_label()` 在两文件各定义一次。`memory_prefetch.rs` 属 memory 子系统，与压缩仅弱关联。真实痛点是"单文件过大 + 内联大量测试撑体量"。

**拆分方案（新建 `src/compression/`，依赖方向单向：`engine/ops → pipeline → budget/safety/strategy`）**

| 模块 | 内容（来源行） |
|------|----------------|
| `strategy.rs` | `CompressorConfig`/`CompressionError`/`validate`/`defaults` (642–775) |
| `safety.rs` | `redact_*`/`strip_image_placeholders`/`summarize_tool_result`/safety_net (88–403) |
| `budget.rs` | `count_tokens`/`estimate_*_tokens`/`CHARS_PER_TOKEN` (447–487, 2847) |
| `summarize.rs` | `summarize_messages_with_llm`/`generate_summary`/`COMPACTION_TEMPLATE` (611–1900) |
| `breaker.rs` | `BreakerState`/`BreakerSnapshot` 抽为独立 `Breaker` 结构 |
| `engine.rs` | `ContextCompressor` + `compress`/`compress_fast`/`prune`/`snip` + trait impls |
| `pipeline.rs` / `ops.rs` | 保留现状 |

去重：`role_label()` 只在 `pipeline.rs` 留一份，`compression.rs` 改为 `use`。`memory_prefetch.rs` 保持独立（建议归入 `memory/` 模块）。

**工作量与风险**：约 **5 人日**（低风险，已有 1600+ 行测试作护栏）。风险：中。压缩策略/预算常量/评分对对话质量高度敏感，改动须回归；操作顺序：测试外移 → 叶子模块 → summarize/breaker，每步跑 `cargo test`。

---

### G6 · `hermix-focus/src/embedded.rs`(3606)

**成因**：`EmbeddedMemoryBackend` 直接持有 `Mutex<Connection>`，随 L1.3 向量列、H5 显著性重排、session 钩子、reflections/facts 迭代逐层累加。注意：本文件**不生成** embedding（实际在 `hermix-intelligence`），也**无 HTTP handler**（所谓 API 层是 `MemoryLayer` 适配边界）。

**拆分方案（依赖方向：`adapter → store/retrieve/score/embed → model`）**

| 模块 | 职责 | 搬入内容 |
|------|------|----------|
| `model.rs` | 数据模型 + SQL DDL + 版本 | TurnRecord/StoredFrame/SessionSnapshot/Reflection, migrate DDL |
| `store.rs` | 连接/迁移/CRUD | `open`/`migrate`/全部 upsert_*/record_*/count_*/delete_* |
| `embed.rs` | 向量存取 | `upsert_embedding`/`upsert_frame_with_embedding` (BLOB 编解码) |
| `retrieve.rs` | 检索路径 | `cosine_topk_impl`/fts/like/facts_search/hydrate_visible |
| `score.rs` | 显著性重排 | `rerank_by_salience`/`l2_normalize`/`dot` |
| `cjk.rs` | 纯文本/工具 | `cjk_bigram_segment`/`is_cjk`/`build_session_note` |
| `adapter.rs` | `MemoryLayer` 集成边界 | `impl MemoryLayer` + session 钩子 |
| `tests.rs` | 单测 | `mod tests` 外移 |

**工作量与风险**：约 **4–6 人日**。风险：中。(1) `hermix-agent` 大量文件直接调用其 pub 方法，拆分须保持签名不变（可用 re-export 过渡）；(2) `upsert_frame_with_embedding` 是"帧+向量"单事务，拆分须保持原子性；(3) 内联 ~1000 行测试按模块切分本身即降风险。

---

### G10+G15 · `hermix-gateway/src/gateway.rs`(2899) + `groupchat/runtime.rs`(2254) + `scheduler.rs`(1316) + `yanluo.rs`(1059)

**gateway.rs 职责**：适配器注册(367–403)、入站泵(201)、路由/命令(529–1602，占近半)、用量状态压缩(1371–1602)、看门狗(1706–1735)、`impl PlatformSender`(1886)。典型 router+ingress+manager+config+monitor 混合。

**platforms 平行重复问题**：10 个平台文件（1443–2141 行）都已实现 `PlatformAdapter`（trait 仅 7 方法），但每个重写：收消息循环、send 系列、媒体、交互组件、inbound 解析；`split_message` 在 discord/telegram/slack 三处近乎逐字复制。trait 过薄，缺 `upload_media`/`interactive`/收消息循环公共抽象——**典型平行重复上帝问题**。

**groupchat 边界**：`scheduler.rs` 纯状态机（无 I/O，边界最清晰）；`yanluo.rs` 纯规则审计（无 I/O）；`runtime.rs` 一身兼任"调度执行+审计调用+争议裁决"，`on_review_verdict` 在 scheduler 与 runtime 两套并存。

**拆分方案**

platform 层：扩 `PlatformAdapter` 增 `connect`/`recv_loop`/`upload_media`/`interactive`；扩展 `BasePlatformAdapter` 统一 client/proxy/心跳/重连/ACK/分片。各平台只留"配置+SDK 调用+inbound 映射"。可下沉：`ingress::connector`（通用收消息骨架）、`common::split`、`common::media`、`common::interactive`。

| 模块 | 职责 | 搬入 |
|------|------|------|
| `router.rs` | 路由/slash/stream | route_message/route_*/execute_slash_command |
| `ingress.rs` | 入站泵/注册/起停 | gw_pump_inbound/get_adapter/register_adapter |
| `platform_manager.rs` | 看门狗/重连/cleanup | *_watcher/run_maintenance |
| `config.rs` | 三结构体 + load_* | GatewayConfig/IncomingMessage/RuntimeContext |
| `gateway.rs`(瘦) | 组装 + `impl PlatformSender` | new/usage/status/compress/hooks |
| `groupchat/audit.rs` | 评分/争议 | 从 runtime 拆出 |
| `groupchat/dispatch_executor.rs` | 真调 LLM | 从 runtime 拆出 |
| `groupchat/runtime.rs`(瘦) | 仅编排 | — |

依赖方向：`platforms → hermix_core`；`gateway 子模块 → platforms + groupchat`；groupchat: `task_types ← yanluo ← scheduler ← runtime ← manager`。

**工作量与风险**：约 **26–35 人日**（最大集群）。风险：(1) 跨平台消息一致性（split/parse 语义差异，公共化易破特例）；(2) SDK 差异（discord 心跳/resume、telegram 长轮询、slack SocketMode ACK、matrix WS）使通用 connector 易过度/不足；(3) groupchat 双判定合并可能改评分时序，须保住现有 200+ 单测。

---

### G16+G17+G18+G19 · `hermix-tools` 集群

**各文件职责**
- `backends/browser.rs`(3296)：`CdpBrowserBackend`/`CamoFox`/`AutoBrowser` + 3 云端提供方 + `BrowserBackend` trait 实现 + a11y→YAML + 配置 + `BrowserRegistry` + 会话包装（11+ 关注点）。注意 `backends::browser → tools::browser::BrowserBackend` 反向引入（轻微异常）。
- `tools/url_safety.rs`(2051)：`WebsitePolicy` + Tirith 引擎 + `SsrfGuard` + `UrlSafetyHandler`（**无假设中的缓存子模块**，用户假设不成立）。
- `approval.rs`(1944)：`TerminalApprovalContext` 模型 + `ApprovalManager` 决策引擎 + 持久化（UI 轮询/超时在 dispatcher 层，本文件仅模型+决策 API）。
- `registry.rs`(1663)：`ToolRegistry` 注册/索引/校验/协议生成。
- 其余 `tools/{cua,memory,skills,terminal,file,todo}.rs`、`backends/{memory,web}.rs` 多为内聚的多 Handler/多后端同文件，仅需文件级切分或代码生成，非紧急。

**拆分方案（依赖方向统一：纯函数/模型 ← 引擎/ops ← 注册包装）**

| 文件 | 新模块 | 职责 |
|------|--------|------|
| backends/browser.rs | `cdp/session.rs` `cdp/page.rs` `cdp/screenshot.rs` `cdp/download.rs` `cdp/network.rs` `cloud.rs` `a11y.rs` `registry.rs`/`config.rs` `session_wrapper.rs` | CDP 生命周期/页面操作/专项 ops/云端/纯函数/注册表 |
| url_safety.rs | `parse.rs` `policy.rs` `tirith.rs` `ssrf.rs` `handler.rs` | 解析/网站策略/Tirith/SSRF/Handler |
| approval.rs | `model.rs` `engine.rs` `persist.rs` `allowlist.rs` | 模型/决策/持久化/白名单 |
| registry.rs | `entry.rs` `validation.rs` `schema.rs` + core | 条目/校验/协议 |

**工作量与风险**：约 **18–25 人日**。风险：(1) CDP 会话生命周期+并发，`BrowserBackend` 被 20+ Handler 调用，trait 签名不可变；(2) `ApprovalManager` 在 `&self` 下用 `Mutex` 支撑 WS 并发，拆分须保持内存模型；(3) `ToolHandler::handle` 全局依赖，改动级联。建议先抽 trait/纯函数边界、保持编译通过再移文件。

---

### 前端集群 G8+G9+G11+G13+G20+G21（+ Settings/Plugins/TaskPanel/commands_cua）

**成因**
1. **无全局聊天 Context**：App 调 `useStreamChat` 后把 `streamingPhase`/`streamError`/`todoGroups`/`subAgents`/`contextUsage` 等几十项经 props 透传 ChatView（L1153–1203），再层层下钻，props 爆炸。
2. **渲染与逻辑同体**：消息渲染（文本/图片/工具/思考/错误/审批/澄清）全在 `MessageBubble` 的 if-else + 30 个子函数，无类型化子组件。
3. **api.ts 既是传输层又是类型层又是缓存/熔断层**：200+ 函数+全部类型一文件。
4. **重复实现**：流式 step 拼接在 App(L284) 与 ChatView(L106/122) 各一套；`formatBytes`/`formatDuration` 等工具多文件重复。
5. **Rust 命令桥**为求单一注册点而堆在 `commands_cua.rs`。

**拆分方案（依赖方向：组件 → hook → api）**

| 文件 | 新模块 | 职责 |
|------|--------|------|
| ChatView.tsx | `MessageList.tsx` `Composer.tsx` `ModelSelector.tsx` `StreamErrorBanner.tsx` `useChatUI.ts` `ChatContext.tsx` | 列表/输入/模型选择/错误卡/本地状态/集中透传 |
| App.tsx | `routes.tsx` `AppProviders.tsx` `useAppBootstrap.ts` `streamEvents.ts` | 路由/Provider聚合/初始化/step拼接(搬 L284) |
| MessageBubble.tsx | `TextBubble` `ToolCallBubble.tsx` `ThinkingBubble.tsx` `ApprovalBubble.tsx` `ClarifyBubble.tsx` `bubbleUtils.ts` | 类型化子组件 + 工具函数 |
| api.ts | `rest.ts` `ws.ts` `types.ts` `errors.ts` `mcp.ts`/`groupchat.ts`/`analytics.ts`/`persona.ts` | 按领域拆分 |
| useStreamChat.ts | `useWsConnection` `useStreamParser` `useStreamPhase`（主 hook 组合） | 连接/解析/phase 机 |
| RightSidebar.tsx | `ComputerTab` `ProductsTab` `BrowserTab` `PreviewPanel` `FileContextMenu` | 各 tab 独立文件 |
| Settings/Plugins/TaskPanel | `XxxTab.tsx` + `useXxxData.ts` | 子面板 + api 封装 |
| commands_cua.rs | `cua_mouse.rs` `cua_keyboard.rs` `cua_window.rs` `cua_ui.rs` `cua_browser.rs` | 共享 `CuaCommandResponse`/`trace_path` |

**工作量与风险**：约 **30–35 人日**。风险：(1) props→Context 重构易漏传导致渲染错位；(2) 流式状态（phase/chunk 批处理/超时）时序敏感，须保持 `useStreamChat` 返回契约(L1871)不变；(3) Tauri `invoke` 接口名不可改，Rust 拆分后需在 `lib.rs` 重新注册。建议先做 `api.ts`/`types` 与 `MessageBubble` 子组件化（低风险高收益），再动 ChatView/App 的 Context 化。

---

## 3. 废弃 / 孤儿文件清单与清理方案

### 3.1 Rust 侧（高置信度，可安全删除）

| 文件路径 | 信号 | 置信度 | 建议动作 | 备注 |
|----------|------|--------|----------|------|
| `crates/hermix-tools/src/tools/jira.rs` | 孤儿+可疑名 | H | **删除** | `tools/mod.rs:33` `// pub mod jira;` 与 `:209` `jira::register` 均被注释，标注 "Untracked, broken" |
| `crates/hermix-tools/src/tools/slack.rs` | 孤儿+可疑名 | H | **删除** | 同上（注意与 gateway `platforms/slack.rs` 无关，后者仍接线） |
| `crates/hermix-agent/src/memory_plugins/mod.rs` | 孤儿（整目录） | H | **删除整目录** | 全仓无 `mod memory_plugins;` 声明；文件自述"外部 memory 插件已移除，trait 已删" |
| `crates/hermix-plugin/src/loader_packager.rs` | 废弃标记 | H | **删除（先解耦）** | `lib.rs:20` 注释 `// legacy (unused, kept for compile stability)` |
| `crates/hermix-plugin/src/node_setup.rs` | 二级孤儿 | M | **随上条删除** | 全仓仅被 `loader_packager.rs` 引用，删上条后即成孤儿 |

> 清理顺序：`node_setup.rs` 与 `loader_packager.rs` 需先确认 `lib.rs` 引用解除后再删除；`jira.rs`/`slack.rs` 直接删；`memory_plugins/` 整目录删。删除前再全局 grep 确认无动态注册（字符串路由表）引用。

### 3.2 前端侧（高置信度孤儿/重复旧版）

| 文件路径 | 信号 | 置信度 | 建议动作 |
|----------|------|--------|----------|
| `apps/desktop-tauri/src/lib/customAppBridge.ts` | 孤儿+重复旧版 | H | **删除**（已被 `lib/customAppApi.ts` 取代，IPC 函数零调用） |
| `components/InlinePathPreview.tsx` | 孤儿 | H | 删除 |
| `components/ArchiveRefsGraph.tsx` | 孤儿 | H | 删除 |
| `components/ExecutionTimeline.tsx` | 孤儿 | H | 删除（注意同名类型 `ExecutionTimelineEvent` 在 types.ts，勿混淆） |
| `components/PlanConfirmationDialog.tsx` | 孤儿 | H | 删除 |
| `components/VoicePlayer.tsx` | 孤儿 | H | 删除 |
| `components/InlineThinkingDot.tsx` | 孤儿 | H | 删除（其 CSS 仍在 `main.tsx` 引入，一并清理） |
| `components/ModeSelector.tsx` | 孤儿+重复类型 | H | 删除（其 `TaskMode` 类型与 `api.ts:156` 重复冲突） |
| `components/groupchat/RankBadge.tsx` | 孤儿 | H | 删除/合并（`tierColor`/`RoomRankChip` 等全无引用） |
| `utils/fetchWithRetry.ts` | 孤儿 | H | 删除（614B 小工具，全仓无引用） |
| `apps/desktop-tauri/src/api.ts` → `listTodos()` | `@deprecated` 死导出 | H | 删除该导出（前端随 TodoPanel 移除，grep 确认无 import） |

> 以上 11 项均为 H 置信度，**删除无功能回退**。删除前建议再全局 grep 一次确认无字符串路由表/动态注册引用即可安全移除。

### 3.3 误报排除（看似孤儿实为有意设计，保留）

以下经检测为 **feature 门控的可选编译代码**（默认不编译，非死文件），建议保留；仅当要裁剪二进制体积时再评估：

- `crates/hermix-intelligence/src/embedding.rs` — `#[cfg(feature="fastembed")]`
- `crates/hermix-environments/src/ssh.rs` / `docker.rs` — `ssh`/`docker` feature
- `crates/hermix-gateway/src/platforms/*.rs`（~19 个平台）— 各平台独立 feature，`platforms/mod.rs` 中 cfg 接线
- `crates/hermix-telemetry/src/otlp.rs` — feature 门控

> 注：前端检测中标记的 8 个 `.test.ts(x)` 与 3 个环境声明（`vite-env.d.ts` 等）为 vitest/编译器加载，非死代码，保留。

---

## 4. 影响评估与实施路线

### 4.1 问题成因总览
- **增量开发无架构护栏**：所有大文件均为"每次加一个 handler / 一个平台 / 一个工具"逐步膨胀，无人定期抽取模块。
- **上帝 struct 传递**：`HttpServerState`/`AgentLoop`/`LocalAgentService`/`EmbeddedMemoryBackend` 以巨型 struct 抱全部依赖，方法可直触任意子系统，无边界抽象。
- **抽象缺失导致平行重复**：`PlatformAdapter` 过薄、provider 流式解析未抽公共 crate，导致 10 平台 / 4 provider 复制粘贴。
- **测试与 prod 混排**：多个文件内联 ~1000–2000 行 `#[cfg(test)]`，进一步撑大体量、拖慢编译。

### 4.2 影响评估
- **评审/合并成本**：8921 行单文件使每次 PR 冲突概率与评审时长指数上升。
- **新人上手**：上帝文件隐藏真实模块边界，理解成本极高。
- **回归风险**：共享可变状态（State 的 30+ Mutex 字段、ApprovalManager 并发模型）改动易引入隐蔽并发 bug。
- **编译耦合**：单文件过大 → 单翻译单元编译久 → CI 慢。

### 4.3 实施路线（按风险/收益排序）

**Phase 0 — 立即执行（零功能风险，≈0.5–1 人日）**
- 删除 §3.1 的 5 个 Rust 废弃文件 + §3.2 的 11 个前端孤儿文件 / 死导出。
- 验证：`cargo check --workspace` + 前端 `pnpm tsc --noEmit` 通过。

**Phase 1 — 低风险高收益（≈15–20 人日）**
- `compression.rs` 叶子模块提取 + `role_label` 去重（5d，有测试护栏）。
- `embedded.rs` model/store/cjk 拆分（4–6d）。
- 前端 `api.ts`→`types.ts`/`rest.ts`/`ws.ts`/`errors.ts` + `MessageBubble` 子组件化（7–9d）。
- `url_safety.rs` / `approval.rs` / `registry.rs` 叶子模块提取（9–12d 部分）。

**Phase 2 — 中风险（≈40–55 人日）**
- `server/lib.rs` 路由提取为 `routes/*` + `state.rs`（8–12d）。
- `agent_loop` 3-impl 收敛 + `loop/*` 子模块（12–18d）。
- `tools/backends/browser.rs` trait/纯函数边界 + 文件切分（8–10d）。
- `gateway.rs` 瘦壳 + groupchat audit/dispatch 解耦（11–15d）。

**Phase 3 — 高风险大块（≈55–75 人日）**
- `local_agent_service` 门面分解 + `session_persistence` 分域 + `provider` 公共 sse（20–27d，DB 迁移须极谨慎）。
- `gateway` 10 平台公共抽象（15–20d）。
- 前端 `ChatView`/`App` Context 化 + `useStreamChat` 拆 hook（15–20d）。

> 总估算：上帝文件拆分 **123–163 人日**。建议按 Phase 顺序增量推进，每步保持 `cargo check` + `cargo test` + `pnpm tsc` 绿灯，按 AGENTS.md 要求每 PR 附测试证据。

### 4.4 护栏建议
1. **先抽 trait/纯函数边界，再移文件**——保持编译通过，避免大爆炸式重构。
2. **DB 迁移类（session_persistence）单独评审**——版本戳与 `ensure_db` 调用序不可拆散。
3. **保留接口签名**——`AgentService`/`LlmProvider`/`PlatformAdapter`/`ToolHandler`/`BrowserBackend` trait 与 Tauri `invoke` 命令名不可改，用 re-export 过渡。
4. **测试先行外移**——把内联 `#[cfg(test)]` 迁到 `tests/` 或独立文件，降低单文件体量同时保住回归网。
5. **CI 门禁加体量预警**——在 `ci-gate.sh` 增加"单文件 > 2500 行即告警"，防止复发。
