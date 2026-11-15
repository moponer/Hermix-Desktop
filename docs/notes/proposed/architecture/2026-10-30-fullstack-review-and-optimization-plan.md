# Decision Note: 前后端复查与优化执行（2026-10-30）

Status: proposed — 安全、视觉一致、会话读取与重复 schema 收口**两轮已交付**（见「执行记录」）；数据生命周期策略、`App.tsx` 拆分、审批卡合并等结构项列为待认领，附精简可行性与行数收益估算。

## Decision

### 复查口径

- **范围**：`crates/`（24 member）+ `apps/desktop-tauri/src`（ts/tsx + css）+ 构建配置。SDK、`patches/`、`docs/` 不在代码质量面内。
- **与 2026-10-25 审计的关系**：那轮 P0 八项、双循环合并、SQLite 打开统一、vendor 拆分、轮询收敛、错误可见化、WS 帧上限、限流键均已交付且本轮复核成立，不重复列入。本笔记只收**仍未关闭**与**本轮新发现**。
- **实测基线**：Rust 261,489 行 / ts/tsx 63,293 行 / CSS 14,140 行（≈34 万行）；`release/frontend/assets` 首屏 `index` 469.8 KB、`App` 402.6 KB、`vendor-highlight` 602.0 KB、`globals.css` 297.1 KB；本机 `sessions.db` 55.2 MB + WAL 12.0 MB。
- **门禁基线**：`check-design-guard` clean、`check-theme-tokens` 全绿（120/120 契约、对比度 AA）、`tsc --noEmit` exit 0；`check-design-primitives` 报 G10 中性结构描边 **27 处 > 基线 15 处**（advisory，未阻断）。

### 对上一轮结论的三处事实修正

1. **`MessageBubble` 未 memo —— 不成立**。`MessageList.tsx:93-113` 有 `MemoMessageBubble`，自定义比较器覆盖 message/workspaceRoot/8 个回调/showHeader/isStreaming/keepOpen/hasRunningTool 等 15 项。这条不列入。
2. **refractor 25 个语言包「静态拖入」—— 部分是设计**。`lib/codeHighlight.ts:62-93` 已把长尾语法改成按需 `import()` + `subscribeLangLoads` 重渲染，静态保留的是「核心 12 类 + 常用 13 类」。剩余 602 KB 是取舍，不是遗漏。
3. **server 测试基线 506（非 507）**：本轮实测 506 passed，改动后 508（新增 2 条 CSRF 用例）。

### 任务清单

| # | 项 | 优先级 | 状态 |
|---|---|---|---|
| S1 | `/shutdown` 由 GET 改 POST 并校验 Origin/Referer/Sec-Fetch-Site | P0 | **已交付** |
| S2 | `/v1/filesearch/shutdown` 同一 CSRF 校验（跨站表单 POST 是简单请求） | P0 | **已交付** |
| S3 | 敏感 dashboard 端点补端点级能力位（env/reveal、vault×5、skills/toggle、toolsets/enabled、mcp 写×4、agents 写×2、custom-apps package/run） | P0 | **已交付**（中间件 `local_only_routes()`） |
| S4 | 出站 HTTP 补超时：`transcribe.rs`、`custom_providers.rs` 两处探测 client | P0 | **已交付** |
| S5 | 错误细节脱敏：`/api/analytics/usage` 的 `cause` 不再回传 SQLite 原文 | P1 | **已交付** |
| S6 | nonce 缓存加容量上限（4096，超限清空） | P1 | **已交付** |
| D1 | `sessions(parent_session_id)` / `sessions(workbench_id)` 索引 + 删重复索引 `idx_messages_session` | P1 | **已交付** |
| D2 | `get_session_messages` 的 DB 读取整体搬 `spawn_blocking` | P1 | **已交付** |
| D3 | 会话子表批量查询（替换 1+N 连接） | P1 | **已交付** |
| D4 | 读路径连接池 | P1 | **不做**（D3 后每请求已降到 1 条连接；长期持有读连接在 Windows 上会阻塞库文件清理） |
| D5 | 数据生命周期：保留期 + `incremental_vacuum`；`estimate_usage_from_messages` 去全表扫；memory 清理去前导通配 | P1 | 待认领（**删除策略需产品拍板**） |
| D6 | pre_turn 每轮同步文件 IO（AGENTS.md / experience）加缓存 | P1 | **已交付**（OAuth 双读保留：两个 provider 的刷新会互相覆盖同一份 JSON，合并读会写回旧 token） |
| M3 | 浏览器工具外层预算（60s < schema 允许的 120s） | P1 | **已交付** |
| V11 | 空状态：MissionBoard 嵌入态四组全空 → 空白 | 视觉 | **已交付** |
| V13 | 设置页健康检查失败只 `console.warn`（后端离线无可见提示） | 视觉/可用性 | **已交付** |
| V1 | 焦点环收口：删 3 条 2px outline + 去掉强制 `border-radius` | 视觉 | **已交付** |
| V2 | `::placeholder` 统一到 `--color-text-faint` | 视觉 | **已交付** |
| V3 | `.btn` / `.input` 字号 12→13px；`.btn`/`.icon-btn` 补 `:disabled` | 视觉 | **已交付** |
| V4 | `SettingsRow` 不再丢弃非交互行的 `className` | 视觉 | **已交付** |
| V5 | 审批卡补 `aria-live`；Composer「清除专家」补键盘可达 | 可访问性 | **已交付** |
| V6 | `AnalyticsView` 三处 `text-red-*` 换 `text-text-error` | 视觉 | **已交付** |
| V7 | 间距网格（`--space-*` 仅 3 处引用、半档 84 文件、7/11px 手调） | 视觉 | 待拍板（改 `Card`/`SettingsRow` 会整体改变密度） |
| V8 | 暗色 `text-tertiary` 与 `text-muted` 同值（#9b9b9b）→ 层级塌陷 | 视觉 | 待拍板（加灰阶会撞对比度门禁） |
| V9 | 独立窗口（file-search）跟随主题 | 视觉 | 待认领（需独立 CSS 入口改造） |
| V10 | `@container` 推广到 5 处视口断点 | 视觉 | 待认领 |
| V11 | 空状态：MissionBoard 嵌入态空白、`EmptyState` 无 `action` | 视觉 | 待认领 |
| V12 | G10 结构描边由 advisory 转阻断（基线冻结 15） | 视觉门禁 | 待拍板 |
| A1 | `App.tsx` 拆分 + 轻量 store | 架构 | 待认领（需冻结并行改动窗口） |
| A2 | 工具层重复后端合并（`backends/file.rs` → `tools/file.rs`；`session_search` 复用现有 FTS） | 架构 | 待认领 |
| A3 | 审批卡 4 份 / 澄清卡 2 份 → 共享变体 | 架构 | 待认领 |
| A4 | CI 增加 `cargo clippy -- -D warnings`（先按 crate 分批） | 工程 | 待认领 |
| A5 | HTTP request 级 `x-request-id` | 可观测性 | **已交付**（前半） |
| A6 | `/v1/metrics` 指标快照端点（走正常认证；同时删掉 `/metrics` 免鉴权死分支与 `metrics_require_auth` 死字段） | 可观测性 | **已交付** |
| D5a | `/api/storage/stats` + `/api/storage/vacuum`（本机专用）+ 诊断页存储卡片 | 数据 | **已交付**（保留期仍待产品决策） |
| C1a | 错误细节脱敏：vault / daily_memory / analytics 不再回传 SQLite 原文与路径 | 一致性 | **已交付** |
| C1b | `From<AgentError>` 一律 400 → 按变体映射；其余 `(StatusCode, String)` 手写点收敛 | 一致性 | 待认领（面广，需逐端点确认语义） |
| C2a | `GET /v1/sessions/search` 的 `limit` 钳位 + 搬阻塞池；`get_session_usage` 返回结构化错误体 | 接口 | **已交付** |
| A4 | CI 增加 clippy（correctness 级）门禁；修掉 `pnpm tsc --noEmit` 假信号 | 工程 | **已交付** |

## 执行记录（2026-10-30）

### 安全

| 项 | 落地 | 验证 |
|---|---|---|
| S1/S2 | `security.rs` 新增 `browser_origin_allowed`（Origin/Referer host 白名单 + `Sec-Fetch-Site`，三头全缺放行以兼容原生客户端）；`lib.rs` 路由改 `post`；`filesearch::shutdown` 同校验 | 新增 2 条用例（`browser_origin_accepts_local_pages_and_native_clients`、`browser_origin_rejects_cross_site_pages`） |
| S3 | `dashboard/mod.rs` 新增 `require_capability` 中间件 + `guarded()` + `local_only_routes()`，把 15 条敏感路由从主链移出并按 `ConfigMutation` / `WorkbenchAccess` 分组。选这两个能力位而非新枚举：`Origin::may` 里 PairedDevice 对两者均 `Denied`，语义即「本机专属」，桌面端 `Local` 全放行、行为零变化 | server 508 passed |
| S4 | `transcribe.rs` 120s、`custom_providers.rs` 两处 20s | 编译零警告 |
| S5 | `aggregate_usage` 失败只写 `tracing::error!`，响应体给固定文案 | 编译零警告 |
| S6 | `NONCE_MAX = 4096`，超限清空（与限流表同策略） | 编译零警告 |

### 数据层

| 项 | 落地 | 验证 |
|---|---|---|
| D1 | `schema.rs` 建表批次补 `idx_sessions_parent` / `idx_sessions_workbench`，`DROP INDEX IF EXISTS idx_messages_session`；`CREATE TABLE sessions` 补 `workbench_id`（否则新库在同一批次里建索引会报 no such column —— 首轮测试即暴露） | 59 存储测试全绿 |
| D2 | `routes.rs::get_session_messages`：主会话尾部 + `find_children` + 归档子会话读取收进一个 `spawn_blocking`，失败只回「读取会话失败（详见 server.log）」 | server 508 passed |

### 视觉/可访问性

见 V1–V6。CSS 层校验：`check-theme-tokens` 全绿（含 120/120 契约、对比度）；`check-design-guard` clean；`tsc --noEmit` 0。

### 第二轮（会话读取 / 重复 schema / 每轮 IO）

| 项 | 落地 | 验证 |
|---|---|---|
| D3 | `SessionPersistence::load_children_with_approvals_and_timestamps`：一个连接读出全部归档子会话（原 `find_children` + 每子会话 3 次 `Connection::open`）。同时把「两个查询按位置 zip 时间戳」合并成**一条 SQL** —— 过滤条件不可能再漂移，从结构上消除「每条消息都显示刚刚」那类错行 | 新增 `children_batch_read_matches_per_child_reads`（与逐个读逐条比对），存储 61 passed |
| A2（前半） | `backends/session_search.rs` 删掉自建的 sessions/messages/messages_fts schema（第三份）、`ensure_parent_session_column`、零调用的 `index_message`；改为「只打开已存在的库，不存在就不注册」。副作用修复：它此前每次进程启动都会重建 `idx_messages_session`，正好抵消 D1 的索引去重 | tools 810 passed |
| D6 | `directions::load_project_conventions` 加 (mtime, len) 缓存；`experience_prompt_for_tools` 由「每个工具各 `load_all()` 一次」（≈109 次全文读/轮）改为一次读取 + HashSet 过滤 | 新增缓存失效用例，directions 16 passed、experience 18 passed |
| M3 | `per_tool_budget` 增加 `browser_*` → 180s（schema 允许 120s） | 新增 `browser_budget_exceeds_its_own_schema_ceiling` |
| V11 | MissionBoard 嵌入态四组全空时补 `EmptyState` + 下一步说明 | tsc 0、DesignGuard clean |
| V13 | 设置页健康检查失败改为 `toastError` + `reportError`，并把 `health` 置 false | 同上 |

### 第三轮（可观测性 / 结论修正）

| 项 | 落地 | 验证 |
|---|---|---|
| A5（前半） | 新增 `server/request_id.rs` 中间件：透传或生成 `x-request-id`（只接受 `[A-Za-z0-9_-]{1,64}`，拒绝换行/引号/空格以免日志与响应头注入）、回写响应头、5xx 追加一条带 `request_id` 的 error 日志。挂在**最外层**，CORS 短路响应也带 id | 2 条单测（拒注入、生成唯一） |
| 结论修正 | 核实 `tools/file.rs`（6 个 handler）与 `backends/file.rs`（2 个 backend trait 实现）是**分层委派**，不是两份实现；「命令执行 4 套」也分别是 shell / 代码解释器 / GUI 自动化。行数收益上限从 2.4K–3.3K 下调到 **600–900** | 见「代码精简可行性分析」 |

### 第四轮（接口与错误一致性）

| 项 | 落地 | 验证 |
|---|---|---|
| 凭据库错误脱敏 | `vault_api::vault_err` 按变体给稳定文案：`Duplicate` 回显用户输入的名字（409）、`NotFound` 给 404、其余只回「凭据库操作失败」并把 `Display`（SQLite/路径）写日志。`vault()` 打开失败同样不再回传底层错误 | server 510 passed |
| daily_memory 错误脱敏 | `memory_inspect` 三处 `format!("…{e}")` 改为固定文案 + `tracing::warn!/debug!` | 同上 |
| 搜索端点 | `GET /v1/sessions/search` 的 `limit` 加 `clamp(1, 100)`（此前原样进 SQL LIMIT），并把同步 SQLite 搬进 `spawn_blocking` | 同上 |
| 用量端点错误体 | `get_session_usage` 由裸 `StatusCode`（空响应体，前端只能报 "HTTP 500"）改为 `HttpError`（结构化 `{error}`） | 同上 |
| 截断实现 | `tools::os_auto::truncate_chars` 委托 `hermix_core::text_utils::truncate_chars`（删掉第二份 `chars().take()`） | tools 810 passed |

### 第五轮（指标 / 存储维护 / 授权第二批 / CI 门禁）

| 项 | 落地 | 验证 |
|---|---|---|
| 指标端点 | `GET /v1/metrics` 返回 `MetricsSnapshot`（走正常认证）。同时删掉 `security.rs` 里从未注册的 `/metrics` 免鉴权分支与 `metrics_require_auth` 字段 —— 死代码 + 未来的脚枪 | server 512 passed |
| 存储维护 | 新增 `dashboard/storage_admin.rs`：`GET /api/storage/stats`（库/WAL 体积、页数、空闲页、可回收字节）+ `POST /api/storage/vacuum`（单飞、阻塞池、`VACUUM` + `wal_checkpoint(TRUNCATE)`）。两者挂 `local_only_routes()`。诊断页新增「会话库存储」卡片：体积事实 + 回收按钮 | tsc 0、DesignGuard clean |
| 技能列表缓存 | `GET /api/skills` 加 15s TTL（`store.list()` 是同步目录扫描），`toggle_skill` 显式失效 | 编译零警告 |
| 授权第二批 | `/api/analytics/debug`（回传内部表样本行）、`/api/custom-providers/{id}` 写面、`/api/providers/test` 移入 `ConfigMutation`；`/api/automation/tasks/{id}/trigger` 移入 `DangerousTool`（配对设备配了 `dangeroustool` 才放行） | 路由无重复注册，512 passed |
| 错误状态码 | `From<AgentError>` 不再一律 400：参数类 400、上下文 413、限流 429、中断/超轮次 409、超时 504、上游故障 502、配置/IO 500；5xx 只回固定文案（细节进日志） | 2 条新单测 |
| 前端一致性 | 四张审批卡共用 `approvalRisk.ts` 文案（消灭「高危/高风险」两套说法）；`Card` `sm` 10px→12px、`SettingsRow` 14px→16px 回到 4/8 网格；`AnalyticsView`/`SkillsTab` 改容器查询（`@container`） | tsc 0、DesignGuard clean |
| CI | 新增 `cargo clippy --workspace --all-targets`（correctness 级默认 deny → 只拦真 bug，不拦存量风格告警，本地实测 exit 0）；把 `pnpm tsc --noEmit`（假信号，0.8s 就返回 0）改成直接调 `./node_modules/.bin/tsc` | 本地 clippy exit 0 |

**又一处结论修正**：`estimate_usage_from_messages` 的「全表扫」只在**窗口内没有任何 `token_usage` 行**时才跑（`usage.rs:313-325` 的 legacy 兜底），正常安装不会触发。原先按「每次仪表盘请求都扫全部 content」记的收益不成立。

### 第六轮（只读端点缓存 / IPC 常量表）

| 项 | 落地 | 验证 |
|---|---|---|
| TTL 缓存原语 | 新增 `server/ttl_cache.rs`：单槽 `TtlCache<K, V>`（`get`/`put`/`invalidate`），刻意不带淘汰策略——这些端点只有一个 key，带 LRU 是另一个量级的复杂度 | 2 条单测（命中/换 key/显式失效/TTL=0 立即过期） |
| 三个只读端点接入 | `GET /api/skills`（15s，toggle 失效，替换上一轮的临时实现）、`GET /api/dashboard/plugins`（10s，写入失效）、`GET /api/dashboard/plugin-manifests`（30s，rescan 写入缓存并失效插件状态） | server 514 passed |
| 会话消息上下文用量 | `get_context_usage` 也搬进 `spawn_blocking`（上一轮漏在阻塞块外，单条同步查询仍会钉住 worker） | 同上 |
| IPC 命令常量表 | 新增 `src/ipc/commands.ts`（25 个命令名）；`desktopBridge.ts`（21 处）与 `api/rest.ts`（2 处）改为 `IPC.xxx`。命令名从裸字符串变成一次可搜索的重命名，改 Rust 侧函数名不会再静默变成运行期 "Command not found" | `tsc --noEmit` 0；脚本交叉校验 25 个名字在 `src-tauri/src/*.rs` 里都存在 `fn <name>` |

### 第七轮（内存图缓存 / 渲染跳过 / 按钮层级）

| 项 | 落地 | 验证 |
|---|---|---|
| 内存图缓存 | `GET /api/memory/graph` 加 5s TTL（key = `max_nodes`，不同上限是不同结果）；`GraphResponse` 补 `Clone`；`pin_memory` / `delete_memory_item` 在入口失效 —— 这是最后一个「每请求遍历所有 session 目录 + 重建图」的读路径 | server 514 passed |
| ToolEventCard memo | ACUI 时间线在流式期间每批次重渲父级，而卡片内容只由 `events` / `streamComplete` 决定；`memo` 后可跳过含多次 `JSON.parse` 与 `groupEvents` 的整棵子树 | `tsc` 0 |
| 按钮主次 | 分析页 DebugPanel 的「关闭」从与「刷新」同款实心按钮改为次要文本按钮 | DesignGuard clean |

### 第八轮（工具列表 / 用量缓存 · 读写分离授权）

| 项 | 落地 | 验证 |
|---|---|---|
| `/v1/tools` 缓存 | 10s TTL（`list_tools()` 每次重新组装 ~109 个描述结构体）；`set_enabled_toolsets` 显式失效（缓存里含 `isEnabled`） | server 514 passed |
| `/api/analytics/usage` 缓存 | 15s TTL，key = `days`；缓存**序列化后的值**（为缓存给 6 个结构体加 `Clone` 派生链不划算），wire 格式不变 | 同上 |
| 读写分离授权 | `/api/dashboard/plugins` 的 PUT、`/api/persona/*` 的写面（soul/memory/user/overrides/personalities 增删改）移入 `local_only_routes()`，**GET 留在原处**（手机端对话要读系统提示与人格文件） | 同上 —— 顺带证实 axum 允许同一 path 按方法拆分注册到两个 router 再 merge（此前担心会 panic） |

### 第九轮（授权覆盖到写面全量 · 空态）

| 项 | 落地 | 验证 |
|---|---|---|
| 写面全量收口 | 读写拆分推广到剩余所有「`get(x).put(y)` 绑在同一行」的端点：`/api/config/auto-correct`、`/api/model/list`、`/api/voice/stt`、`/api/mobile/model-push`、`/api/models/context-windows`、`/api/models/refresh-context`、`/api/free-channel`（put/dead-model/probe-model）、`/api/custom-providers`（create）、`/api/automation/tasks`（create/update/delete/reset）、`/api/workflows`（upsert/delete）、`/api/dashboard/plugins/rescan` —— 全部 `ConfigMutation`；`/api/workflows/{id}` 的**运行**单独归 `DangerousTool`。至此 dashboard 的**每一个写端点**都在能力位之下 | server 514 passed（含路由构建）；workspace check 零警告 |
| VaultPanel 空态 | 「还没有凭据」补 `EmptyState` + 下一步说明；搜索无结果单独一条 | `tsc` 0、DesignGuard clean |

### 第十轮（截断收敛 · 又一个真 bug）

| 项 | 落地 | 验证 |
|---|---|---|
| **`display.rs` 截断 panic（真 bug）** | `hermix-intelligence/src/display.rs::truncate` 按**字节**判断并切片（`&text[..max-3]`）—— text 含中文/emoji 时切点落在多字节字符中间 → panic。与 2026-10-25 修掉的工具结果截断是同一类 bug，漏网在此。改走 `hermix_core::text_utils::truncate_chars`，新增 CJK 回归用例 | intelligence 9 passed（含新用例） |
| 截断收敛（8 处） | `server/util.rs`、`dashboard/memory_inspect.rs`、`channels/qq.rs`、`crash_dump.rs`、`agent/cognition_arch.rs`、`experience_archive.rs`、`harness_probe.rs` 的本地实现全部委托 core，本地只保留各自的省略号语义。**语义差异逐处保留并注释**：cognition 版没有"≤ max 原样返回"短路（恰好 max 长也会截），crash_dump 的判长从字节改字符（CJK 串此前会漏截断，不 panic 但语义错） | server 514 / agent cognition 13 / experience 18 / harness 23 全绿；workspace check 零警告 |

### 第十一轮（截断收敛收尾 · 空态/容器查询）

| 项 | 落地 | 验证 |
|---|---|---|
| 截断收敛（再 3 处，共 **11 处**） | `agent/self_improvement.rs`、`harness_self_edit.rs`（两份相同的字符循环 + 省略号）与 `tools/tool_chain.rs`（`(String, bool)` 变体，`was_truncated` 改整串比较）全部委托 core。**至此 `truncate` 类本地实现只剩 14 处**，其中 8 处是语义确实不同的（`truncate_middle_chars` 头尾保留、`truncate_one_line` 单行、字节行截断等），不再追并 | agent self_improvement 11 / harness 23 / tools tool_chain 13 全绿 |
| BuiltInExpertsTab | 手写空态 → `EmptyState`；`sm:/lg:/xl:grid-cols-4` → `@container` + `@2xl/@4xl/@6xl`（设置面板固定宽，`xl:` 曾硬塞四列） | `tsc` 0、DesignGuard clean |

### 第十二轮（数据保留期 · Spinner 隐形 bug）

| 项 | 落地 | 验证 |
|---|---|---|
| **Spinner 隐形 bug（真 bug）** | `ui/Spinner` 只设了 `borderTopColor: transparent` 而**没有任何 border 宽度/颜色**（Tailwind preflight 归零、全仓 CSS 也无 `.spinner` 规则）→ 转圈渲染出来是**完全透明的圆**：`ui/Button` 的 loading 态、「记忆管理」加载大图，用户看到的都是空白。补 `border-2 border-current`（颜色随文字色走令牌） | `tsc` 0、DesignGuard clean、theme-tokens 全绿 |
| 数据保留期（安全版） | 新增 `storage.metrics_retention_days`（**默认 0 = 永不清理**）+ `server/storage_janitor.rs`（启动 60s 后首跑、每 6h 一次、阻塞池、删后 PASSIVE checkpoint）。范围刻意收窄：**只清 `token_usage` 指标数据**，聊天内容永不自动删 —— 内容保留期必须显式做成独立决策。诊断页的人工 `/api/storage/vacuum` 与之互补 | janitor 3 条单测（空库 noop / 只删老行 / 0=永不删）；config 145 + proptest 12；server 517 |

**设计取舍**：保留期做成「默认关 + 只动指标数据」而不是「默认开 + 全量」，因为删聊天内容需要"删了什么、还能不能找回"的用户沟通；`/api/storage/vacuum`（人工）与 janitor（自动、可配）补上了"文件不缩水"的另一半。

### 第十三轮（总验收 · 解除 App.tsx 的阻塞）

| 项 | 结果 |
|---|---|
| **前端测试套件可运行** | `npm test`（vitest run）实测 **47 文件 / 426 测试全绿（64s）**。此前记录的"执行环境把 vitest 判为 watch 命令"不成立——用 `npm test` 即可。这解除了 `App.tsx` 拆分"没有回归网"的前提 |
| 全仓 Rust 总验收 | `cargo test --workspace --lib --exclude hermix-environments --exclude hermix-viewer` 全绿：agent **1238**、server **517**、intelligence **317**、config **145**、focus **184**、store **64**、core **195** 等，0 failed |

#### `useApprovalBridge` 抽取执行方案（下一步，直接可做）

现状：7 个 ref 共 **72 个使用点**（`approvalResolversRef`×11、`approvalBatchResolversRef`×9、`approvalRetryTimersRef`×13、`approvalTimeoutTimersRef`×14、`approvalRescueTimersRef`×12、`pendingApprovalSessionsRef`×6、`approvalRestTimersRef`×7），散布在 App.tsx 1080–4028 行。

1. 新建 `hooks/useApprovalBridge.ts`，暴露**纯簿记 API**（不依赖任何 App state）：
   `setResolver(id, fn)` / `takeResolver(id)`（get+delete 一步，与现有 `get` 后 `delete` 等价）/ `setBatchResolver` / `takeBatchResolver`；`setTimer(kind, id, t)` / `clearTimer(kind, id)` / `clearAllTimers()`；`setPendingSession(id, sid)` / `takePendingSession(id)`；`addRestTimer(n)` / `removeRestTimer(n)` / `clearRestTimers()`。
2. App.tsx 侧逐 ref 替换，**每替换一个 ref 跑一次 `npm test` + `tsc`**（426 条回归网够密）。
3. 最后把 unmount 清理块（~3990–4030）改为调用 `bridge.dispose()`，删掉 7 个 ref。
4. 收益：App.tsx −~120 行、审批簿记获得独立单测位（hook 可用 `renderHook` 直接测 resolver 取还/定时器清理的竞态）。

### 第十四轮（App.tsx 拆分第一刀：useApprovalBridge）

| 项 | 落地 | 验证 |
|---|---|---|
| 新建 `hooks/useApprovalBridge.ts` | 7 个 ref（2 resolver 表 + 4 定时器表 + pendingSessions + restTimers）收进一个组件级单例对象。**只搬簿记不搬策略**：自动过期、自救探测、REST 兜底全部留在调用方——策略进 hook，测试就得伪造 WebSocket。原注释随迁移 | `tsc` 0 |
| 72 个使用点迁移 | `xxxRef.current` → `approvalBridge.<field>`（语义逐点等价：Map/Set 操作面不变）；unmount 清理块改从 `approvalBridge.restTimers` 取并补依赖 | 同上 |
| 顺带删掉一个类型谎言 | `chatStateReset.ts` 的 4 个 `RefLike` 字段改为直接收 `Pick<ApprovalBridge, …>`——App.tsx 3991 处的 `as unknown as RefLike<…>` 强转（resolver 签名对不上）随之消失；测试 `chatStateReset.test.ts` 同步改形 | 同上 |
| **验收** | `App.tsx` **4478 → 4279 行**；7 个 ref 声明与 72 个使用点零残留（grep 证实）；`npm test` **426/426 全绿** | 同上 |

下一刀（未做）：`session store`（42 处 `setSessions`）——需要先把 `updateSession` 的调用面收敛，规模大于本刀。

### 第十五轮（IPC 命令常量：全量收口）

| 项 | 落地 | 验证 |
|---|---|---|
| 常量表扩容 | `ipc/commands.ts` 从 25 → **69 个命令名**（截图 9、更新/环境 9、自定义应用 2、工作台 20、窗口/其余 4） | 每个名字与 `src-tauri/src` 的 `fn <name>` 逐条交叉校验存在 |
| 全量迁移 | 剩余 **59 个**裸 `invoke("…")`（20 个文件：workbench/api 21、ScreenshotOverlay 6、useStreamParser 6、useUpdater 4、main 4、其余散点）全部改为 `IPC.xxx`；导入按文件深度自动补注。VoiceButton 只有一行注释提到命令名 → 撤掉多余导入 | `tsc` 0、`npm test` **426/426**；裸 invoke 残留 **0**（`api/rest.ts` 的 "GET" 是正则跨行误报，非命令名） |

自此：改任何 Tauri 命令名，TS 编译器立刻报错——这是此前 45+ 处裸字符串做不到的。

### 第十六轮（memory_inspect 收口 · session store 地图修正）

| 项 | 落地 | 验证 |
|---|---|---|
| memory_inspect 同步 IO 收口 | `get_memory_item` 的 SQLite 分支与 daily 分支、`search_memory`（DB 检索 + 全 md 目录扫描，提为 `search_memory_sync`）、`get_daily_memory`（提为 `get_daily_memory_sync`）全部搬 `spawn_blocking` —— 至此该文件只剩写操作（pin/delete，Mutex 单连接、微秒级）留在 worker 上 | server 518 passed |

**「42 处 `setSessions`」的高估修正**（第二刀的地图重画）：逐点分类后，**32/42 已经走 `updateSession` 单一写入路径**（2026-10-25 建的 `lib/sessionUpdates.ts` 已起效）；剩余 10 处是合法的全列表操作（evict×2、stampDecision×4、stampBatchDecision×2、删除快照×1、合并去重×2）与跨会话定位（clarify/plan）×2。

所以第二刀的**正确定义不是"引入 store 收敛写入"**（写入已收敛），而是：

1. 把 29 个内联 updater 抽成 `lib/sessionUpdaters.ts` 的**纯函数**（`(session, payload) => session`）——纯函数可用 vitest 直接单测（现有 `chatStateReset.test.ts` 同款模式），不依赖 React；
2. updater 抽完后再评估是否需要跨组件缓存（真正的痛点是 ArchiveTab/WorkbenchView 重复 fetch 同一份会话列表，不是 setSessions 本身）。

这把第二刀从"高风险状态重构"降级为"逐个可测的纯函数抽取"——每个 updater 独立迁移、独立配测试，不再是一锤子买卖。

### 第十七轮（sessionUpdaters 第一批：4 个纯函数）

| 项 | 落地 | 验证 |
|---|---|---|
| 新建 `lib/sessionUpdaters.ts` | `stampApprovalDecision` / `stampBatchApprovalDecision`（单条与批量审批的决策打戳；`now` 由调用方注入以便断言 `decidedAt`）、`resolveClarify`（澄清卡作答，跨会话）、`acknowledgePlan`（计划卡本地确认）。纯函数、不改写输入、无副作用 | 8 条新单测（stale click 契约 / 跨会话隔离 / 引用相等） |
| App.tsx 迁移 | 两个本地 `stampDecision`/`stampBatchDecision` 定义删除，6 个调用点 + clarify/plan 两段内联 map 改调纯函数 | `App.tsx` **4279 → 4197**（累计 −281）；`tsc` 0、`npm test` **434/434**（+8） |

**抽取顺带发现的死分支**：`resolveClarify` 的 `...(m.clarify ?? {...})` 兜底 spread 实际不可达——守卫 `m.clarify?.requestId !== requestId` 在它之前，`clarify` 为空的澄清卡永远不匹配。已用测试固化真实契约；将来若要支持"裸 clarify 消息"需连守卫一起改（未动）。

### 第十八轮（sessionUpdaters 第二批：列表级操作 ×4）

| 项 | 落地 | 验证 |
|---|---|---|
| `evictSessionMessages` | 切走会话的内存驱逐（清 messages、保留徽标数、幂等）。App.tsx 两个效果此前**各自内联一份相同的 evict 闭包**——收敛为一份，ref 镜像同步继续在调用方 | 3 条单测（徽标回退、幂等、其它会话引用相等） |
| `prependOlderMessages` | 「加载更早」拼页头。**去重挪进函数内**（以目标会话现有 id 为准）→ 幂等；App 侧的 knownIds 预过滤随之删除 | 3 条单测（去重、全重复幂等、其它会话引用相等） |
| `mergeSessionMeta` | `refreshSessionMeta`：服务端权威三元组（title/message_count/updated_at）覆盖本地、messages 不碰、新会话追加尾部 | 2 条单测 |
| `appendNewSessions` | WS 推送路径：只追加新 id，**已有的一律不覆盖**（本地可能持有更细的状态）；无新增时返回原数组引用（零拷贝） | 2 条单测 |
| App.tsx 迁移 | 5 处调用点替换（refreshSessionMeta、双 evict、loadOlderMessages、upsertSessions） | `App.tsx` **4197 → 4172**（累计 −306）；`tsc` 0、`npm test` **444/444**（sessionUpdaters 累计 +18 条） |

### 第十九轮（sessionUpdaters 第三批：发送/流式热路径 ×3）

| 项 | 落地 | 验证 |
|---|---|---|
| `computeAutoTitle` | 首条消息自动起标题（含 `<think>` 外壳清洗的既有注释）。从 App.tsx 组件内闭包上移为导出纯函数——它同时被 `beginAssistantTurn` 和 renameSession 路径使用 | 3 条单测（占位判定/28 字截断/真实标题不动/无用户消息回退） |
| `beginAssistantTurn` | **flushSync 同步 commit 的那个 updater**：追加用户消息 + 空 assistant 占位 + 自动标题。竞态契约（占位必须在 approval step 写入前进 state）现在有单测位 | 2 条单测 |
| `appendAssistantChunk` | 全应用**最热的写入路径**（每 rAF 一次的流式文本落盘）：正文拼接 + `appendTextStep` 时间线步。保持纯函数以便将来对攒批语义做精确单测 | 2 条单测（拼接、目标缺失时原样） |
| App.tsx 迁移 | flushSync 块与 `flushPendingTextChunks` 各缩为一行调用；顺带清掉三个失效导入（`appendTextStep`/`patchMessageById`/`NEW_CHAT`） | `App.tsx` **4172 → 4151**（累计 −327）；`tsc` 0、`npm test` **451/451**（sessionUpdaters 累计 +25 条） |

### 第二十轮（sessionUpdaters 第四批：工具生命周期 ×8）

| 项 | 落地 | 验证 |
|---|---|---|
| 新增 8 个纯函数 | `appendApprovalRequestStep` / `appendApprovalBatchStep`（审批卡注入，**去重是契约**——live 重发/双发同 requestId 会出两张卡）、`appendChatMessage`（澄清卡/计划卡共用）、`applyToolStdoutChunks`（stdout 攒批合并，找不到调用则新建 `tool_stdout`）、`appendTimelineEvent`（tool_start/tool_complete/file_change **三份内联拷贝**收敛为一份）、`markAssistantError`（错误收尾，`errorText: string \| null` 按真实类型收）、`finalizeAssistantStopReason`（done 收尾：text 仅兜底、durationMs 条件写入、无变化返回**原引用**让调用方跳过）、`stampTrailingAssistantStopReason`（无目标 id 时的旧行为兜底） | 见下 |
| App.tsx 迁移 | 10 处调用点替换（onApprovalRequest/onApprovalBatchRequest/onClarifyRequest/onOrchestrationEvent/flushPendingToolChunks/onError/onToolStart/onToolComplete/onDone 主路径+兜底）；`file_change` 保留原样（其 path+change_type 去重是独立契约）；顺带清 `updateToolStepStdout` 失效导入；`markAssistantError` 调用处 `?? ""`（原语义：空串不匹配任何消息 id = 无操作） | `App.tsx` **4151 → 4040**（累计 **−438**）；`tsc` 0、`npm test` **461/461**（sessionUpdaters 累计 **+35 条**） |

## 代码精简可行性分析

### 方法论

按「删掉后行为不变」与「删掉后只剩一份实现」两类分开评估。**行数不是目标**：本仓 34 万行里测试与注释占比可观，删测试/删注释不是精简。真正的收益是**同一语义的实现份数**——它决定「修一处要改几处」。

### 已交付（两轮，净 −143 行）

| 位置 | 前 | 后 | 说明 |
|---|---|---|---|
| `session_persistence/schema.rs` | 714 行，12 份 8 行迁移块 | **654 行** | 抽 `add_column(conn, table, column, decl)`；容错口径（`duplicate column` / `no such table`）从 12 份收敛为 1 份。**净 −60 行**，且以后加一列只需一行 |
| `backends/session_search.rs` | 685 行，含第三份 sessions+FTS schema、启动期 DDL、零调用 `index_message` | **614 行** | 只打开既有库，不建表。**净 −71 行**，并消除「启动时重建已删索引」的隐性回退 |
| `styles/tokens.css` | `--font-size-*` 6 个别名（含越档的 14px） | 删除 | `AppMentionInput` 两处改用 `--text-body`。**−8 行** |
| `dashboard/types.rs` | `PaginatedSessions`（全仓零引用） | 删除 | 分页契约残留。**−4 行** |
| `dashboard/mod.rs` | 15 条敏感路由平铺 | 移入 `local_only_routes()` | 路由行数减少；`+39` 行中间件换掉了「每 handler 各写 8 行」的 ~120 行方案 |
| `session_persistence/messages.rs` | 双查询 + 位置 zip | 单条 SQL + 批量读 | 行数略增（+41，注释与批量方法），但去掉一次全量查询与 N 次连接 |

### 待做（按「行数收益 ÷ 风险」排序）

| 项 | 证据 | 可回收 | 风险 | 建议 |
|---|---|---|---|---|
| ~~工具后端合并~~ | **前提不成立（2026-10-30 核实）**：`tools/file.rs` 是 6 个工具 handler（read/write/patch/search/image/move），`backends/file.rs` 是 `LocalPatchBackend` / `LocalSearchBackend` 两个 trait 实现，handler 通过 `Arc<dyn PatchBackend>` 委派——这是**分层**不是重复。同理「命令执行 4 套」分别是 shell / 代码解释器 / GUI 自动化，语义不同 | 0 | — | 不做。这条原估 1500–2000 行是误判，已从收益表移除 |
| 审批/澄清卡片合并 | 4 份审批卡（292/394/232/341）+ 2 份澄清卡 | **400–600 行** | 中（视觉回归需截图比对） | 抽 `ApprovalCard` + `variant`，保留 4 个调用点薄封装 |
| `App.tsx` 拆分 | 4478 行 / 32 `useState` / 32 `useEffect` / 42 处 `setSessions`；`handleSendMessage` ≈1700 行 | **300–500 行**（样板与重复事件分发） | 高（需冻结并行改动） | 先把流事件处理搬进 `useStreamParser`，再抽 session store；总行数不是重点，冲突面才是 |
| `truncate` 收敛 | 43 处 `fn truncate*`，规范实现在 `hermix-core/src/text_utils.rs:30`，同义实现 ≥13 处 | **~90 行** | 低（逐处核对语义：char vs byte、省略号） | 每 crate 一批，改完跑该 crate 测试 |
| 共享 HTTP client | 93 处 `Client::new/builder`，各自配超时/UA | **~150 行** + 统一超时 | 中（代理/TLS/超时差异需保留 override） | 先收敛 server 侧（本轮已补 3 处超时），再 tools |
| `.route()` 单行化 | `dashboard/mod.rs` 72 处 `.route(\n p,\n h,\n)` | **~140 行** | 极低 | 纯格式；建议随下次改动顺手做，不单独提交 |
| 死配置/死分支 | `--space-*`（3 处引用）、`/metrics` 特判（无路由）、`DispatchTimeouts` 60/300（注释自认不用于生产） | **~25 行** | 低 | 一次性清理，需确认 `--space-*` 是否仍计划推广 |
| token 估算收敛 | `context.rs` / `compression` / `intelligence` / `session_persistence` 各一份，口径不同 | **~40 行** | 中（口径变化会影响压缩触发点） | 先统一到 `hermix-core`，用现有 usage 测试锁口径 |
| 仓库噪音（非代码） | `tmp-probe-preview/`（34 文件，全仓零引用）、`hermix-logo-concepts_assets/`（9 PNG）、`EXECUTION-LOG-2026-10-06.md`、根目录 `check-freshness.ps1`（与 `scripts/check-sidecar-freshness.sh` 重叠） | 0 行 | 低 | **本轮未删**：工作树有并行会话的未提交改动，删除文件会混进别人的 diff；建议单独一个 chore 提交 |

### 明确不做（附理由）

- **删 `--space-*` 令牌**：它是间距系统的唯一载体，删掉等于承认「间距没有系统」；正确方向是让 `Card`/`SettingsRow` 用回整档，而不是删令牌。
- **删测试/压缩注释**：本仓测试密度本已偏低（`hermix-store` 4 个、`url_safety/{ssrf,policy,tirith}` 内联 0 个），删测试是把可维护性换行数。
- **合并 `Switch` 与 `ToggleSwitch`**：需先确认引用面（`ui/index.ts` 有再导出，直接删会破坏调用点），收益 79 行、风险中等，不值得单独一批。
- **`no_op_compression_engine` / `build_active_skills_block`**：前者 `pub` 跨 crate 可见、后者 `pub` 且 5 处测试在用，删除要改测试，净收益为负（2026-10-26 已记录过一次）。

**结论（核实后修正）**：本轮已回收 **143 行**；剩余可回收上限约 **600–900 行（全仓 0.2%–0.3%）**——最大单笔是审批卡合并（400–600 行），其余是 `truncate` 收敛（~90）与死配置（~25）。

**原先「工具后端合并可回收 1500–2000 行」是误判**：`tools/file.rs`（6 个 handler）与 `backends/file.rs`（2 个 backend trait 实现）是分层关系，handler 通过 `Arc<dyn PatchBackend>` 委派，不存在两份实现。**行数不是这轮的主要矛盾**：34 万行里真正的问题是「同一语义多处实现」的少数几处（truncate / token 估算 / base64 / HTTP client），以及 `App.tsx` 这类单体。想显著缩小体积只能砍能力，那是产品决策。

## Alternatives considered

1. **逐 handler 加能力位检查**（而非中间件）：被否。15 个 handler × ~8 行 ≈ 120 行，且新增端点仍会忘记加；中间件把「哪个路径属于哪个能力位」变成路由表上的声明，漏写会立刻在 code review 里看见。
2. **全局 Origin 校验（所有非 GET 请求）**：被否。自定义应用在 `file://` / `hx-shell` 下 Origin 为 `null`，全局拦截会打断它们的后端调用；本轮只保护「副作用且可能是简单请求」的两个 shutdown 端点。
3. **给 `WorkbenchAccess` 单独开一个 `LocalOnly` 能力位**：暂缓。现有矩阵里 PairedDevice 对它已 `Denied`，加枚举要动 `Capability::may` 的穷尽 match 与所有消费点，收益只是语义更准确。
4. **一次性重排间距网格**：被否（本轮）。`p-2.5`/`py-3.5` 遍布 84 个文件，机械替换会改变所有页面密度，必须配截图回归；留给专门的视觉批次。
5. **用 rustfmt 全仓重排换行数**：被否。格式不是精简，且会与并行会话产生大面积冲突。

## Consequences

- **正面**：两个 shutdown 端点不再能被任意网页触发；配对设备（默认最小能力位）不再能读 env/vault 明文、改工具集、拉起自定义应用；会话读取不再阻塞 tokio worker；迁移的容错口径只剩一份；焦点环/占位符/按钮字号三处一致性债务清零。
- **负面/代价**：
  - 能力位收紧后，若手机端将来需要 vault 只读，必须显式加能力位（当前矩阵对 PairedDevice 恒 `Denied`，需改矩阵）。
  - `/shutdown` 由 GET 改 POST：仓库内无调用点，但外部排障脚本若用 `curl http://127.0.0.1:3000/shutdown` 会失效，需改 `-X POST`。
  - 新增索引会在首次启动时对旧库做一次 `CREATE INDEX`（55 MB 库约百毫秒级），可接受；`DROP INDEX idx_messages_session` 不可逆但该索引本就是冗余。
- **验证基线**：`cargo check --workspace --all-targets` 零错误零警告；`hermix-server` 508 passed、`hermix-agent` 存储 61 passed、`hermix-tools` 810 passed；`tsc --noEmit` 0；`check-design-guard` clean、`check-theme-tokens` 全绿。前端 `vitest` 本轮未跑（执行环境把 `vitest` 判定为 watch 命令并提前结束），下次改动前端逻辑时补跑。
- **行为变化清单（需要告知用户/排障文档）**：`/shutdown` 改 POST；`session_search` 在「尚无任何会话库」时不再注册（不再是启动即建空库）；浏览器工具超时上限 60s → 180s。
