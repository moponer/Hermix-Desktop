# MCPHub Desktop 对照分析：可借鉴点

Archived: 2026-11-15
对照分析类文档；借鉴项的去留已由后续决策笔记记录，本文存档备查。

> 分析对象：<https://github.com/skrstop/MCPHub-Desktop>（Tauri 2 + Rust + SQLite，
> clone 于 2026-10-12，`/tmp/mcphub`，584 文件）。它是 samanhappy/mcphub（Web 版 MCP
> 聚合服务）的桌面端 Rust 重写，自带 Skills 分发、RAG 知识库、嵌入模型管理。
>
> 原则照旧：**只借鉴模式，不搬架构**。它是单 crate 桌面应用，我们是 21 crates 的
> agent 运行时；它的 RAG/i18n/多用户与我们的战略不同，明确不碰（见 §7）。

---

## 0. 结论速览

| # | 借鉴点 | 我们的现状（代码证据） | 优先级 | 预估量 |
|---|---|---|---|---|
| 1 | SSRF 防护 | `hermix-mcp` 全 crate `ssrf/is_private/blocked` **零命中**，transport 直连用户配置的 HTTP/SSE URL | **P0** | ~150 行 + 测试 |
| 2 | MCP token 明文存储 | `dashboard/mcp.rs` 注释自认 "plaintext at rest… DPAPI wrapping is a future step"，而 `hermix-vault` 已有 `crypto::encrypt` | **P0** | ~80 行 + 迁移 |
| 3 | 连接失败不可观测 | `state.rs` "deliberately ignore per-server connect failures"——坏了零反馈；他们 `stderr_tail` 32KB 滚动缓存拼进 error | P1 | ~120 行 |
| 4 | 连接进度事件 | 我们后台 spawn 连 MCP，UI 无任何进度；他们 `progress.rs` 300ms 节流事件 | P1 | ~150 行 |
| 5 | 自动重连 + 生命周期 | 我们 `client.rs` `pool/idle/on_demand/reconnect` **零命中**，连上就躺；他们 session_rebuild 循环 + post-connect 重检 + delete 先于 disconnect | P1 | ~300 行 |
| 6 | 应用内日志查看 | 我们日志只落文件（panic hook 昨天刚补）；他们 `app_log` 表 + FTS5 + LogsPage | P2 | ~200 行 |
| 7 | 前后端契约对账脚本 | 我们 `api/rest.ts` 与 dashboard 类型全手工镜像；他们 `check_frontend_rust_contract.py` 进 CI | P2 | 脚本 ~120 行 |
| 8 | 六条工程纪律 | unwrap_or_default 边界 / bool vs 计数器 / as 收缩 / 上限先于分配 / 迁移 panic = 崩溃循环 / 日志参数脱敏 | P2 | review checklist |

先说公平话：**MCP 基本面我们是通的**——`state.rs:649` 用 `ArcMcpManager::new(tool_registry)`
把 MCP 工具注册进 agent 同一个 registry，`client.rs:930` connect 时注册真实 schema。
差距不在"有没有"，在**托管深度**（池/生命周期/可观测/安全）。

---

## 1. P0-1：SSRF 防护（安全缺口，实锤）

### 他们的做法

`services/ssrf_guard.rs`：用户配置的 URL（桌面端唯一入口是 OpenAPI spec fetch）在
connect 前过闸—— blocked IPv4 九段（0/8、10/8、100.64/10、127/8、169.254/16、
172.16/12、192.0.0/24、192.168/16、198.18/15）+ IPv6（::/::1/fe80::/fec0::/ff00::/
6to4/Teredo/NAT64/ULA/v4-mapped）。两个关键设计：

1. **DNS 结果先验证再 connect，且把验证过的 IP `resolve()` pin 进 reqwest client**——
   防的是验证与 connect 之间 DNS 重绑（rebinding）。
2. fail-closed：配置解析不了就硬失败，不猜。

### 我们的洞

`grep -rn "ssrf\|is_private\|blocked" crates/hermix-mcp/src` → **零命中**。
我们的 `transport.rs`（884 行）对用户配置的 `url` 直接建连。桌面用户自己配的
MCP server 打内网地址是合法需求（本地服务），所以**不是无差别禁止**，而是：

- 默认放行 loopback/私网（单用户桌面，配置即信任）；
- **但要有显式开关 + 审计日志**：连了哪个 IP、什么时候连的——他们的活动日志
  模式（`source_ip` 落库）直接可抄。

### 落地

1. `crates/hermix-mcp/src/ssrf.rs`（新）：IP 段判定 + DNS pin 辅助函数，纯函数可单测。
2. `transport.rs` HTTP/SSE 建连路径接入：解析 host → 判定 → 私网/回环记
   `tracing::info!`（含 IP）；非私网但解析失败 → fail-closed。
3. 配置项 `mcp.allow_private_networks`（默认 true，桌面定位），关闭时私网直拒。
4. 测试：九段 IPv4 + IPv6  representative 各一条 + DNS pin 行为（mock resolver）。

---

## 2. P0-2：MCP token 不该明文躺 `mcp_servers.json`

### 现状（他们自己注释承认的）

`dashboard/mcp.rs:56`：

> Bearer token … Stored in mcp_servers.json alongside the rest of the entry
> (**plaintext at rest**, same trust level as relay.json; DPAPI wrapping is a
> future step).

### 我们已有的底座

`hermix-vault` 有完整实现：`crypto::encrypt(&self.key, secret)` + `store.rs` +
`credman.rs`（Windows 凭据管理器）。**房子盖好了，钥匙没用来锁这把门**。

### 落地

1. `McpServerEntry.token` 从 `Option<String>` 改为 vault 引用
   （`token_ref: Option<String>`），值进 vault（name = `mcp:<server_id>`）。
2. `load_mcp_servers` 读取时透明解密；解析不到 vault 条目但 JSON 里有旧明文 →
   自动迁移（读一次明文，写 vault，抹掉 JSON 字段）。
3. `dashboard/mcp.rs` 的 CRUD 全部改走 vault；前端拿不到明文（列表接口只回
   `has_token: bool`）——他们 bearer key 的管理界面就是这个形态。
4. 迁移测试：旧明文文件 → 加载 → JSON 字段消失 → vault 可解出原值。

> 顺序建议：先做这个（80 行、纯本地、无行为风险），SSRF 紧随其后。

---

## 3. P1-3：连接失败从"静默吞掉"到"看得见"

### 他们的做法（三件套）

1. **stderr_tail**：stdio 子进程 stderr 32KB 滚动缓存，连接失败时拼进 error message
   （单行截断 2000 字符）——npx 包下载失败、node 没装、权限拒绝，全在 error 里。
2. **非阻塞连接**：保存类命令后台 spawn，立即返回 `starting`，不被连接阻塞。
3. **进度事件**：`server://install-progress`（下载中/done/error，stderr 行匹配 +
   300ms 节流）。

### 我们的对应洞

`state.rs` 的连接循环注释原话："We deliberately ignore per-server connect
failures: one broken server must not prevent the rest from coming up."——
**不拖累别人是对的，但错了要说话**。现在用户配了 5 个 server 坏 1 个，
界面上什么都看不到，LLM 的工具列表里就是少几个工具，无从查起。

### 落地

1. `client.rs` connect 路径：子进程 stderr 起 reader task，环形缓冲 32KB；
   失败时 `McpError::ConnectionError` 带 tail 末 N 行。
2. `state.rs` 后台连接循环：失败记 `tracing::warn!`（server 名 + tail），
   dashboard 状态接口暴露 `last_error`——前端"连接器"tab 每个 server 卡片
   显示错误图标 + hover tail。
3. 进度事件（可选第二阶段）：`hermix:mcp-connect-progress`，300ms 节流。

---

## 4. P1-4：生命周期——重连、幂等、TOCTOU

### 他们的做法（`mcp_manager.rs` + `pool.rs`，注释全是生产教训）

- **session_rebuild**：断开服务器 30s 循环重连，只重连 enabled 的；
  **connect 成功后镜像 fail-closed 重检**（行缺/Err/disabled → disconnect）——
  防"tick 读配置→用户禁用→spawn 重连已禁用服务器→永久在线"。
- **delete 先提交再 disconnect**：delete 在 DB delete 前 disconnect 会有窗口期
  被 session-rebuild 重建→删除后活连接孤儿。
- **update 无连接相关变更不重连**：序列化比对（剔除 id/name/description，
  60000 视为未设置，strip nulls，深比对）。
- **on_demand**（852 行）：stdio server 首调冷启动 + idle 5min 关进程保工具缓存；
  `in_flight` 是 **u32 计数器不是 bool**（并发 A/B，A 先完成清 flag，B 还在跑
  时定时器拆了活调用——600s 调用 vs 300s idle 正是要防的场景）；取消时靠
  `InFlightGuard` 的 Drop 清理（future 被取消永不清计数器 → 进程孤儿）。
- **pool 的锁外调用**：`client: Option<Arc<Mutex<McpClient>>>` 放在
  `RwLock<HashMap>` 里，调用在读锁外执行——一个 600s 慢调用曾钉死读锁，
  冻结所有并发读者（UI 状态轮询、list_all_tools）。

### 我们的现状

`client.rs`（1203 行）`pool/idle/on_demand/reconnect` 零命中：启动全连、
断了就没了、没有重连循环。对桌面常驻应用，MCP server 崩溃（npx 包崩、
远程 502）后工具永久消失，要重启整个后端才回来。

### 落地（按性价比排序，不必全上）

1. **重连循环**（最高价值）：hermix-server 侧 30s tick，扫已断开连接，
   只重连 enabled；连上后重检 enabled（抄他们 R39 教训：三条路径
   toggle/disable/rebuild 都要有 post-connect 重检，别只修俩）。
2. **delete/update 次序**：dashboard `update_server`/`delete_server` 对
   MCP 的操作对齐——先持久化再动连接；`enabled` 翻转才 disconnect。
3. on_demand/idle：**暂不做**（我们的 server 数量是个位数，启动全连可接受；
   等用户真配了十几个 npx server 拖慢启动再上，他们的 852 行是那场景的产物）。
4. pool 锁外调用：我们并发度低，记录为 known debt，不提前优化。

---

## 5. P2-5：应用内日志查看（接上昨天的 panic hook）

昨天给 panic hook 落了盘（`<hermix_home>/logs/panic-*.log`），但**用户仍然
要去文件系统里翻**。他们的完整链路：

- `app_logger::log_to_db` 是唯一收口（截断 4000 字符）；
- 前端 `log_event` command（level 白名单 info/warn/error/debug + 4KB 截断，
  无鉴权是故意的——前端遥测）；
- FTS5 虚拟表 `fts_app_log`，LogsPage 后端搜索（300ms 防抖 + 请求 id 乱序守卫）；
- 保留 15 天 + 6h 定时清理 + VACUUM；
- **活动日志记工具调用**（arguments 只记键名+字节数，不记全文——参数常含
  API key）。

### 落地

1. 我们的诊断面板（`dashboard/` 已有 memory_inspect 等 inspect 面）加一个
   "日志" tab：读 `<hermix_home>/logs/hermes-server.<date>.log` + `panic-*.log`
   （文件读即可，不必上 DB——我们日志量级小，DB 化是他们的规模决策）。
2. 前端 `log_event` 命令同款实现（level 白名单 + 4KB 截断），让前端
   update-check 等生命周期事件进同一文件。
3. 工具调用活动日志：args 记键名 + 字节数（这条纪律直接抄，我们
   approval/dashboard 迟早记调用，别一开始就记全文）。

---

## 6. P2-6：工程纪律（零代码，进 review checklist）

他们的 AGENTS.md 是 40 轮复核的事故史，下面六条是**跨轮复现**的教训，
直接进我们的 review 清单：

| 纪律 | 他们的原话/案例 | 我们的对应风险面 |
|---|---|---|
| `unwrap_or_default` 跨构造器边界 = panic 制造机 | R27：read_chunks downcast 失败 unwrap_or_default → 空 vec 喂 `FixedSizeListArray::new`（内部 unwrap）→ 打崩索引线程 | 我们 serde 反序列化 + arrow/parquet 类边界 |
| bool vs 计数器由并发度决定 | R36：on_demand `in_flight` bool，并发调用互踩 | 我们的审批队列、scheduler running 行 |
| `as` 数值收缩必问越界 | R27：httpPort `as u16`，70000→4464 静默错绑 | 我们 config 里的端口/超时/limit 解析 |
| 资源上限检查必须在分配前 | R34：32MB 帽在 `bytes()` 整包缓冲后才生效 = 没有上限 | 我们 spill/document 的 size cap 检查点 |
| 迁移 panic = 崩溃循环 | R36：serde IndexMut panic 发生在 set_version 前 → 每次启动重跑 | 我们 store 的 migration |
| 前后端契约要脚本对账 | R23：Rust `Option<HashMap>` vs 前端恒发 `string[]`，serde 错误只进 toast，20+ 轮 E2E 盲区 | 我们 `api/rest.ts` × dashboard 类型全手写镜像 |

**第七条（P2-7）契约脚本**照他们的 `check_frontend_rust_contract.py` 做一个
`hermix` 版：扫 `api/rest.ts` 的 invoke 调用点 × `src-tauri` command 签名，
camel↔snake 对账，mismatch 即 FAIL，挂 ci-gate。我们的 invoke 面比他们小
（~60 vs 160），脚本 ~120 行，一次性成本，永久受益。

---

## 7. 明确不借鉴（及理由）

| 他们的功能 | 不做的理由 |
|---|---|
| RAG 知识库（lancedb + candle GGUF 嵌入 + tree-sitter 分块，~3000 行） | 战略不同：我们有压缩沉淀 + archive-index + memory_inspect 的记忆路线；上嵌入模型是另一个产品。等哪天要做"跨会话语义记忆"再评估，**其 chunker 防挂死三件套（快速否决/硬切/AST 预分区）可先抄进 markitdown/document 路径** |
| i18n（en/fr/tr） | 产品中文单语，主人明确偏好 |
| 多用户 + JWT + bcrypt + OAuth Provider | 单用户桌面；我们三认证体系（desktop_token/api_server_key/provider key）已覆盖 |
| 内置 Axum HTTP server 对外暴露 MCP | 我们有 relay 链路，形态不同 |
| MCP 2026-07-28 spec（subscriptions/tasks/discover，含 rmcp SDK vendored） | 规范追踪是重活，spec 领先一步没有消费者（我们自己的 serve 模式见 §10——那是死代码，不是资产） |
| Smart Routing $smart 元工具（lancedb + 嵌入索引工具描述） | 依赖嵌入模型；但其 `merge_hits` 纯函数（vw+kw 加权、阈值、limit）模式可在我们工具过多时零依赖复用（BM25/LIKE 通道即可） |
| 65 agent Skills 一键分发（symlink/copy + install.json catalog） | 形态不同：我们的 skills 是 agent 内置能力不是分发资产；但其 **agent catalog include_str! 编译进二进制 + 迁移回填** 的模式对我们 builtin skills 播种有参考价值（低成本，可顺手） |

---

## 8. 建议执行顺序（commit 粒度）

1. `feat(mcp)`: token 入 vault + 明文自动迁移（P0-2，~80 行，无行为风险）
2. `feat(mcp)`: SSRF guard + 私网审计日志（P0-1，~150 行 + 单测）
3. `feat(mcp)`: stderr tail 拼进连接错误 + dashboard last_error（P1-3，~120 行）
4. `feat(mcp)`: 30s 重连循环 + post-connect enabled 重检（P1-4，~200 行）
5. `feat(dashboard)`: 诊断面板日志 tab + log_event 命令（P2-5，~200 行）
6. `chore(ci)`: check_frontend_rust_contract 脚本（P2-7，~120 行）
7. `docs`: 六条工程纪律进 review checklist（P2-6，零代码）

每步独立可验证、可回滚；2/4 有单测，1/3 有迁移测试，5/6 有脚本自检。
全程遵守一语义一 commit；cargo 串行（禁并发两条）。

---

# 深挖篇：Skills 与 MCP（2026-10-12 晚追加）

## 9. Skills：两种完全不同的物种

### 9.1 他们的模式：可分发资产

`skill_service.rs`（1822 行）是一套**技能分发系统**：library（受管技能库）+
65 个外部 agent 目录（`install.json` include_str! 编译进二进制）+
扫描导入（`scan_for_import`）+ 导出（symlink/copy 双模式）+ 每 agent 导出状态 +
启动 reconcile 修复。锁的注释写明了各自防的竞态（`agents_lock` 防整数组覆盖吃写、
`skill_fs_lock` 防 export copy × uninstall 同路径并发出截断树），
`copy_dir_recursive` 带 symlink 环保护（visited 集 + symlink_metadata 判定）。

### 9.2 我们的模式：两套班子，一套真的

| 面 | 内容 | 状态 |
|---|---|---|
| 真实产品面 | `dashboard/skills.rs` 18 个 markdown builtin skills（seed 到磁盘 + UI list/toggle + 工具集） | 活着，产品 surface |
| 遗留面 A | `hermix-skills/skill.rs` 26 项 `BUILTIN_SKILLS`（planning/debugging/…，"aligned with Python bundle footprint"）+ `install_builtin_skills` | **Python 遗留**，经 `hermix-tools` 的 skills 工具动作 `install_builtins` 暴露给 LLM |
| 遗留面 B | `hub.rs`（agentskills.io 客户端）+ `sync_with_hub` | **境外域零接线**（`with_hub` 全仓无调用），同我们 2026-10-04 删掉的 MCP 远程 registry 一个病 |
| 遗留面 C | `auto_create_from_task` / `self_improve` | 仅经 LLM 工具动作可达，无产品消费 |

### 9.3 可拿过来作为内置的（按价值排序）

1. **Skill 导出/导入到外部 agent（他们的核心 feature，~600 行）**
   主人每天用 CodeBuddy / WorkBuddy / Claude Code；把我们 18 个 builtin skills
   （debugging-methodology / code-review / tdd-practice / custom-app-development /
   impeccable-design…）导出到 `.claude/skills`、`.codebuddy/skills`、
   `.cursor/skills` 等 = **一份资产多处用**；反向再支持从已装 agent 扫描导入。
   抄的机制：catalog include_str! + 迁移回填、symlink/copy 双模式、per-agent
   导出状态、copy 的 symlink 环保护、双锁（注释写清竞态）、启动 reconcile。
   落地形态：dashboard 技能页加「导出到…」对话框（agent 多选 + 模式单选）+
   设置页 agent 目录管理；后端 ~400 行 + 前端 ~200 行。
2. **启动 reconcile + 删除传播（治我们的病，~80 行）**
   我们 `seed_builtin_skills` 只加不删（内置清单删了，用户 home 里的副本永远
   留着——有案底的旧病）。抄 `reconcile_pending`：启动时修中断安装；对清单里
   **已不存在**的 builtin，比对内容指纹后清理 home 副本（用户手改过的保留，
   删前记日志）。这条是 9.3-1 和 §10 删除的前置。
3. **skills FTS 搜索**：18 个不需要，>50 个再上（他们的 FTS5 模式可抄）。

### 9.4 MCP 深挖：拿什么、不拿什么

**拿（两条，都便宜）：**

1. **上下文占用可视化**（他们 `cost.rs`，~100 行）：4 字符≈1 token 启发式，
   per-server `exposed`（启用项）vs `gross`（全量）+ per-item 明细。我们的
   「连接器」tab 直接显示每个 MCP server 的工具 schema 占多少 token——
   跟主人 token 成本执念对口，也是关掉用不起的 server 时的决策依据。
2. **运行时探测**（轻量版他们的 runtime bundling）：MCP 连接失败时先看
   `node`/`npx`/`uvx` 在不在 PATH，不在就给一句人话指引（"未检测到 Node.js，
   stdio 类 MCP server 需要它"），配合已计划的 stderr tail。**不随包分发
   Node/UV/Bun**（他们 +100MB+ 的方案，我们用探测+指引替代）。

**不拿（有证据的否定）：**

| 他们的 | 不拿的理由 |
|---|---|
| market servers.json + registry 代理（`market_service.rs` / `registry.rs`） | 我们 2026-10-04 已主动删除同款：官方 registry 境外线路国内超时，内置目录才是确定性数据源。他们 `registry.rs` 还在代理官方源，是我们在第一阶段就踩过并拆掉的坑 |
| rmcp SDK 迁移（他们 6 个 transport 全 rmcp 化） | 我们手写 transport（884 行）有测试在跑；他们迁 rmcp 后自己踩出一串（stderr drain 死代码致进度/tail 静默失效等），迁移风险 > 收益。协议细节差异可个案修，不整体迁 |
| session_pool per-session 隔离 | 单用户桌面无场景（他们的 origin 多用户遗产） |
| Smart Routing $smart | 依赖嵌入模型；§7 已记 |

---

## 10. 我们该删的（不实用清单，全部有证据）

| # | 对象 | 量 | 证据 | 处理 |
|---|---|---|---|---|
| 1 | `hermix-mcp/src/serve.rs` | 724 行 | 全仓零调用；`lib.rs` 仅 `pub use`，无 CLI flag / 无 server 接线 / 无 UI 入口 | **删**（git 历史可追溯） |
| 2 | `transport.rs` 的 `HttpTransport`（~420-561 行） | ~140 行 | `client.rs:644` 只用 `StdioTransport` + `StreamableHttpTransport`；后者注释自认 "Replaces the previous HttpSseTransport" | **删**（含其测试） |
| 3 | `auth.rs` 的 `OAuthConfig`（~69-255 行） | ~190 行 | 全仓零使用，仅 `lib.rs` 导出；我们的远端 MCP 只需 bearer | **删**，保留 `McpAuthProvider` trait + `BearerTokenAuth` |
| 4 | `hermix-skills/src/hub.rs`（agentskills.io） | ~200 行 | `with_hub` / `sync_with_hub` 全仓无调用；境外域，与我们 2026-10-04 删掉的 MCP 远程 registry 同一病 | **删** + 删 LLM 工具动作 `sync` |
| 5 | `skill.rs` 26 项 `BUILTIN_SKILLS` + `install_builtin_skills` + `auto_create_from_task` | ~120 行 | "aligned with Python bundle footprint" 遗留；经 LLM 工具动作 `install_builtins` / `auto_create` 污染技能库 | **删**清单和这两个动作；`create/update/delete/export_to_file/import_from_file` 保留 |
| 6 | 6 个 builtin skills：travel-planning / health-wellness / learning-coach / finance-planning / investment-analysis / writing-assistant | ~430 行 md | 玩具/泛泛型，与目标用户（开发者桌面 agent）不符；`dashboard_smoke.rs` 有 spot-check 断言需同步 | **删**（见下方确认事项） |
| 7 | `self_improve`（SkillManager 方法 + LLM 动作） | ~40 行 | 无产品消费，LLM 自助append 反馈的形态未被任何 UI 读取 | **建议删**，主人若想留"技能自进化"叙事可保留 |

**删除的前置依赖**：6/7 会改产品面，动手前需要主人对清单点头（尤其
finance/investment 两个——若你哪天想拿 Hermix 管台账金流，它们就是种子）；
且删除必须带 §9.3-2 的 home 副本清理，否则老用户机器上留着孤儿技能。

**保留清单（防误伤）**：web-novel（主人有网络小说集成计划）、
impeccable-design（AGENTS.md 门禁引用）、custom-app-development（核心工作流）、
全部 software-development/devops 类。

---

## 11. 执行顺序（commit 粒度，删→建）

**第一批 · 删除（每步独立可回滚，先跑 `cargo check -p hermix-mcp` / `-p hermix-skills` / `-p hermix-server --all-targets`）**

1. `chore(mcp): 移除死代码 serve.rs`（724 行，零调用）
2. `chore(mcp): 移除遗留 HttpTransport（旧 SSE）`（~140 行）
3. `chore(mcp): 移除零使用 OAuthConfig`（~190 行）
4. `chore(skills): 移除 agentskills.io hub 客户端与 sync 动作`（~200 行）
5. `chore(skills): 移除 Python 遗留 builtin 清单与 install_builtins/auto_create 动作`（~120 行）
6. `chore(skills): 下架 6 个不实用 builtin skills + home 副本清理`（~430 行 + §9.3-2 机制 + 测试断言更新）

**第二批 · 建设（删完立省约 1800 行，再往上盖）**

7. `feat(dashboard): MCP 上下文占用可视化`（~100 行，§9.4-1）
8. `feat(skills): 导出/导入到外部 agent`（~600 行，§9.3-1，含 catalog + 双模式 + reconcile）

**验证基线**：每批末尾 `cargo check --workspace --all-targets` + `cargo test -p hermix-server --lib`（dashboard_smoke 的 seed 断言）+ tsc；cargo 串行，禁并发两条（老规矩）。
