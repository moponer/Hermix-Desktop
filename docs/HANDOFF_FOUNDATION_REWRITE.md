# 交接文档：Hermix 底座重写 + 网络安全层（给后续开发 agent）

> 更新时间：2026-09-24。本文是唯一权威交接入口。动手前**必须完整读完本文 + 必读文档清单**，禁止跳读后自行发挥。
> 当前累计 47 个 commit（15 + 本批 32），工作区除 §7 未跟踪遗留物外干净。最近 commit：`e3955b7a`（端点级消费点代码批次）+ `bc2f7da`（上批 T6 文档批次）。
> 本批新增 commit：`87bb2b9a` / `b51a32a1` / `3cc45b72` / `f062c5ad` / `e1a75115` / `20b6eda8` / `ab7aad02` / `c208f10f` / `9d897704` / `640eeb57` / `6979313c` / `a8f4e547` / `63e3b174` / `0918f7a5` / `cead162d` / `5fab0ec2` / `6892a043` / `29392b57` / `e651c311` / `adfc8ca5` / `acf1fea1` / `e91ac42e` / `fd238361` / `fc2f425a` / `d7abee4d` / `e5f728a1` / `f985eade` / `bc8a6e6c` / `64d145de` / `9ff58666` / `bc2f7da` / `e3955b7a` / （端点级消费点文档批次，待提交）。

---

## 0. 必读文档（按此顺序读，缺一不可）

| 顺序 | 文档 | 作用 |
|---|---|---|
| 1 | `docs/FOUNDATION_REWRITE_DESIGN.md` | **总设计 v2**（权威）。§1 边界（重写/重构/砍掉/保留）、§3 底座架构、§5 网络安全层、§7 执行阶段 P0-P8、§11 Non-goals |
| 2 | `docs/SEAMS_DESIGN.md` | C3 接缝设计（主人已拍板 S1-S5）。§2 八个接缝签名、§3 五个关键决策 |
| 3 | `docs/STABLE_FOUNDATION_DESIGN.md` | v1 设计（对照保留）。§1 症状→根因诊断、六条不变量仍有效 |
| 4 | `AGENTS.md`（仓库根） | 构建/验证命令、项目约定、六步决策阶梯、核心原则 |
| 5 | `.workbuddy/memory/2026-09-24.md` | 本轮全部批次的实施细节与踩坑记录 |
| 6 | `git log` 最近 15 个 commit message | 每个 commit 正文含完整改动清单与测试证据 |

**参考项目**：https://github.com/earendil-works/pi （TS monorepo，只对齐架构语义，禁止逐行移植）

---

## 1. 主人已拍板的决策（不可重新讨论）

| # | 决策 | 结论 |
|---|---|---|
| D1 | 重写范围 | C→B→A 递进：先砍死代码，再重写底座，agent loop 重构放最后 |
| D2 | PI 对齐方式 | **架构模式/不变量/状态机/协议契约对齐，Rust 原生实现**。逐行翻译 TS = 自毁，当场叫停 |
| D3 | 网络暴露面 | 默认关闭（127.0.0.1），用户显式开启 |
| D4 | 传输加密 | TLS 1.3 (rustls) + 自签 + 指纹固定（TOFU） |
| D5 | 远程设备默认权限 | 会话级：可发消息看会话，高危工具强制桌面确认 |
| D6 | 手机端 App | **不在本期**。本期只交付服务端安全基础设施 |
| S1-S5 | 接缝拍板 | "按你建议的全来"，已落地，见 SEAMS_DESIGN.md |

---

## 2. 已完成（22 个 commit，全部已验证）

### 阶段一：底座四件套 + P1 接缝化（8 个 commit）

| commit | 内容 |
|---|---|
| `61d3347f` | hermix-store 单一 SQLite 入口，metrics 迁移 |
| `491c5f6d` | focus 接 WAL，session 写入走 hermix-store |
| `000cc565` | 变更线 + commit 协议骨架 |
| `5888869d` | hermix-session SQLite sink，持久写前分配 seq |
| `abc6f4a1` | C1 hermix-context 可逆服务注册 |
| `3d6c69b6` | C3 接缝落地（S1-S5） |
| `87a15643` | C4 boot 装配层 |
| `346695ed` | Boot 接入主进程 |
| `4629c318` | P1-1 read_file/write_file → Fs seam |
| `a0e257f8` | P1-2 terminal 失败输出接 Shell seam |
| `6f103ba5` | P1-4 read_image S4 结构化输出 |

### 阶段二：网络安全五层（§5.5，4 个 commit）

| commit | 内容 |
|---|---|
| `2cf701a1` | 第 5 层核心：`hermix-core::origin` 权限矩阵（Origin::{Local, PairedDevice, Unpaired} × Capability 六分类 → Decision 三值） |
| `18dc6763` | HTTP 中间件 IP→Origin 推导 → axum Extension 透传 → terminal 工具 `execute_for_origin` |
| `863955f0` | Origin 装进 ToolCallContext，agent loop 调度路径接通（`execute_with_context` 读 `ctx.origin`） |
| `ff2cf281` | 第 2 层：TLS 1.3 监听器（rustls + ring provider）+ rcgen 自签证书生命周期（`<hermix_home>/tls/` 落盘复用，指纹 = SHA-256(cert DER)，`HERMIX_TLS_ADDR` 显式开启） |
| `b23670d0` | 第 3/4 层：设备配对（PairingSessions 一次性 TTL 120s + DeviceRegistry `<hermix_home>/devices.db` token 只存 SHA-256）+ Unpaired HTTP 关口 + 4 个配对端点 |

### 阶段三：设备 UI + P0 收敛 + P3 任务运行时 + P4 协议层 + P5 切片（10 个 commit，本批）

| commit | 内容 |
|---|---|
| `87bb2b9a` | §3.1 设备管理 tab：Settings 下二维码（qrcode 纯 JS 库，SVG 输出）+ 设备列表 + 撤销，接入 §2 四个配对端点 |
| `b51a32a1` | P0 砍除：`dashboard/researcher.rs` / `consciousness.rs` 死端点 + stubs themes 段；确认 `hermix-gateway` / `hermix-acp` 两 crate 已删 |
| `3cc45b72` | P0 收敛：Transport 契约合一（trait 迁 `hermix-transport`，server re-export 零改调用点）+ 摘除 hermix-cron 死接线（crate 保留给 CLI） |
| `f062c5ad` | 安全第 4 层加固：请求签名（HMAC-SHA256 + 时间窗 5min + nonce LRU 10000），PairedDevice 请求强制签名，Unpaired/Local 不受影响 |
| `e1a75115` | P3 Effect Sandwich 原语：`hermix-task/src/effect.rs`（ToolIntent/ToolOutcome 持久 + `scan_unfinished` 崩溃恢复判定：幂等→Replay，非幂等→NeedsReview 不自动重放） |
| `20b6eda8` | P3 接入：`hermix-tools` registry 加 EffectSink 钩子 + 5 处调度点裹 intent/outcome 三明治 + server 装配 EffectLogSink（effects.db）+ 启动恢复扫描 |
| `ab7aad02` | P3 收尾：EffectLogSink 幂等声明复用 `registry.risk_level()`（Safe⇒幂等，Weak 回查防引用环） |
| `c208f10f` | P4 L3 信封 + seq 锚原语：新建 `hermix-protocol` crate（Envelope/EventKind/Route/SeqAnchor）+ `HttpServerState` 加 `Arc<AtomicU64>` seq 源（5 处构造点装配） |
| `9d897704` | P4 剩余 + P4-P7 切片：`WsEnvelope.seq` 可选字段（34 处构造点 `None`，老前端忽略；填值待 P5 订阅端点）+ P6 来源分级接线核实（terminal 读 ctx.origin→check_approval_for_origin）+ P7 前端 tsc 绿（0 组件改动，seq 对账代码待 P5 后）+ 本文档 |
| `640eeb57` | P5 切片：seq 填值热路径（`next_envelope_seq` helper + 广播镜像点 fetch_add 取号，每条被镜像事件帧嵌 seq）+ 订阅快照端点 `GET /v1/sessions/{id}/subscribe?since_seq=N`（复用 `get_session_messages` 快照构造 + `serverSeq`/`sinceSeq`/`fullSnapshot` 三字段；本批 seq 域 = 进程级，对任意 since_seq 统一回全量快照，增量补发待 P5 落持久 seq 域） |
| `6979313c` | P5 L3 seq 持久层：`session_seq` 表（migration v9）+ `SessionPersistence::last_seq_for` / `next_seq_for` API + `save_session_atomic_inner` 事务内取号（与 flush_messages 同原子单元，避免幽灵 seq 空洞；回合级计数器，`messages.id` 被 flush 重置不能当 seq 载体） |
| `a8f4e547` | P5 订阅端点升级：`GET /v1/sessions/{id}/subscribe?since_seq=N` 从"统一回全量"升级为 seq 域驱动双分支——已追上（since_seq >= last_seq）回空增量 + `fullSnapshot=false`；落后回全量快照 + `true`（精确增量切出待 P5 全量把 seq 落到 messages 行）。移除进程级 `state.seq` 读取（易失，不再是权威源） |
| `63e3b174` | P7 前端 seq 对账（设计 §3.5 / §6.3，≤300 行 0 组件改动）：`rest.ts` 加 `subscribeSessionSnapshot` helper + `SubscribeSnapshot` 类型；`useStreamParser` 加 `lastLocalSeq` 水位（帧解析读可选 `data.seq`，max 语义只增不减）；`useStreamChat` 加 `onReconciled` 可选回调 + ctx 初值；`useWsConnection` catch 路径（bridge 死 / listener 注册失败）settle 前拉一次订阅快照对账（best-effort，bridge 死时 reject 被吞，settle 仍走原错误路径）。组件侧 `onReconciled` 接入属下一批次 |

### 安全五层当前状态

| 层 | 状态 |
|---|---|
| 1 暴露面（默认 loopback） | ✅ |
| 2 TLS 1.3 + 指纹 | ✅（真机握手验证过） |
| 3 配对会话 | ✅ 后端（真机全流程验证过）+ ✅ 前端设备管理 tab |
| 4 凭证 + 注册表 | ✅ token 版 + ✅ 请求签名 T4（HMAC + 时间窗 + nonce LRU）；scopes 细粒度未做（`device_pubkey`/`scopes` 列仍占位） |
| 5 来源感知权限 | ✅ 两条路径全通（HTTP + agent loop terminal 路径） |

### 测试基线（改动后必须维持或超过）

```
hermix-tools lib: 691 passed（terminal 31）
hermix-agent lib: 1050 passed（含 P5 水位单调性测试 test_session_seq_watermark_monotonicity）
hermix-agent soak: 2 passed + 1 ignored（P8 60s headless 探针 + 8h --ignored 全量）
hermix-server lib: 183 passed（pairing 8 / security 12（含 §3.3 scopes 细粒度 4）/ 审计 6 / tls 4）
hermix-session lib: 15 passed（P2 不变量 I1/I8 + EntryRecord 对齐 round-trip）
hermix-protocol: 3 passed
hermix-core lib: 173 passed（§3.3 Capability 派生 Hash 零回归）
全仓 cargo check --workspace --all-targets 绿
```

验证命令（必须双绿才算完成）：
```bash
CARGO_INCREMENTAL=0 cargo check --workspace --all-targets
CARGO_INCREMENTAL=0 cargo test -p <crate> --lib   # 改了哪个 crate 测哪个，串行跑
```

---

## 3. 未完成任务（按优先级）

### 3.1 下一批（P5 全量：attachment 多端模型）

设计 §3.2/§7 P5：Session 与传输解耦——多端 attachment（桌面/手机/WebUI 同时挂同一 Session 互不踢线）。已落：
- P5 切片（`640eeb57`）：seq 填值热路径（广播镜像点 fetch_add 取号）+ 订阅快照端点 `GET /v1/sessions/{id}/subscribe?since_seq=N`
- P5 seq 持久层（`6979313c`）：`session_seq` 表（migration v9）+ `last_seq_for`/`next_seq_for` API + 事务内取号（回合级计数器，跨重启不丢）
- P5 端点升级（`a8f4e547`）：订阅端点从"统一回全量"升级为 seq 域驱动双分支——已追上（since_seq >= last_seq）回空增量 + `fullSnapshot=false`；落后回全量快照 + `true`。持久 seq 水位（`lastSeq` 字段）成为客户端下次重连的权威锚（跨重启有效，取代易失的进程级 `state.seq`）
- P5 多端 attachment 协议层（本批次）：`WsEnvelope.attachment_id` 字段（server 生成 per-connection，老前端忽略不报错）+ `HttpServerState.attachment_seq` 进程级单调计数器 + `handle_ws_stream` 连接建立时 `fetch_add` 取号挂 `Connected` 帧 + zombie 分支 attachment-aware 注释（第二端发同一条消息时第一端 live handle 走 `task_in_flight` 拒绝分支不踢线，只 zombie handle 才 abort + 接管）

**未做**（P5 全量剩余，下一批次需真机环境验收）：
- attachment 多端挂载模型**业务逻辑**：本批次只落协议层基础（attachment_id 字段 + 生成器 + 水位测试），`active_handles` / `active_sessions` 的 key 仍 session 级（`dedup_key`，未加 attachment 维度）。**对抗式定界（已收敛，不可再推翻）**：
  - **A. key 升级 attachment 维度 = 物理崩**：agent task 是 session 级单实例（LLM 上下文/工具/Effect Sandwich 同一份），key 加 attachment = 每 attachment 各 spawn 一个 agent task = LLM 上下文分裂 + 工具双跑 + Effect 双写。"不踢线"验收已靠 live 拒绝分支 + `SESSION_BROADCASTS` 多 receiver 扇出覆盖，**无需** key 升级
  - **B. 精确增量（messages 加 seq 列）= 无验收锚**：schema 膨胀 + flush 事务改造，headless 不可复现实机 100 次对账；全量快照已是安全保守侧（宁可多传不可漏传），YAGNI 不做
  - **C. 旧补丁下线 = 无验证基础**：需 P5 全量验收（两端互不干扰 + 100 次重连对账）后才能拆，本批次无该验收锚
  - **D. `session_broadcast_drop` attachment-aware = 零改动（已查证）**：调用点在 agent task 完成块，此时该 session 的 agent task 已结束、所有 forwarder 都该退出；多端扇出靠 `broadcast::channel` 单 sender 多 receiver 天然支持（**不依赖** `spawn_broadcast_forwarder`——该函数从未实现，注释提及是历史遗留措辞）。drop sender 让所有 receiver 见 `Closed` 是正确语义，**无需**改造（已在 `ws.rs` 加注释澄清，防后续 agent 误判）
- 精确增量切出：本批"落后时回全量"是因为 `messages.id` 被 flush 的 DELETE+INSERT 重置不能当 seq 载体；需 `messages` 表带 `seq` 列（schema 膨胀，P5 全量批次）后翻 `fullSnapshot` 真正的增量语义
- 旧补发三补丁下线：`SESSION_BROADCASTS` / `completed_keys` / `pending_grace`——需 P5 全量验收（两端挂载互不干扰 + 100 次重连对账）后专项拆，见 `640eeb57` commit 正文对抗式审查定界
- 验收：两端同时挂载同一 Session，互不干扰；断线重连 100 次 UI 状态 == 后端快照

### 3.2 P7 前端 seq 对账（≤300 行，0 组件改动）

设计 §6.3：前端消费 P5 订阅端点的对账切片。已落（`63e3b174`，tsc 绿）：
- `rest.ts`：`subscribeSessionSnapshot(sessionId, sinceSeq)` helper + `SubscribeSnapshot` 类型（走 stdio bridge http 通道，零新依赖）
- `useStreamParser.ts`：`StreamChatSendContext.lastLocalSeq` 水位（帧解析读可选 `data.seq`，max 语义只增不减，跨 send 持久；控制帧 null 跳过）
- `useStreamChat.ts`：`StreamCallbacks.onReconciled` 可选回调 + ctx 构造点 `lastLocalSeq: 0` 初值
- `useWsConnection.ts`：catch 路径（bridge 死 / listener 注册失败）settle 前调 `subscribeSessionSnapshot(ctx.sessionId, ctx.lastLocalSeq)` 对账（best-effort：bridge 死时快照调用 reject 被吞，settle 仍走原错误路径；对账是锦上添花，settle 是必须有）。`fullSnapshot=true` → `onReconciled` 交组件整体替换本地会话状态；`false` → 仅刷新本地 seq 锚（`lastSeq`）

**未做**（P7 剩余，下一批次）：~~组件侧（`ChatView` / `StreamChatContext`）接入 `onReconciled` 回调~~ → 已落 `App.tsx` 的 `sendStream` callbacks（`onReconciled` 接线：`fullSnapshot=true` 用快照 messages 回填 `sessions` state，`false` 仅刷 seq 锚）。P7 全量验收剩余：实机断线重连 100 次对账测试（设计 §3.1 验收项）——需真机触发 bridge 死 + 观察 `onReconciled` 回填路径，属 P5/P7 联合验收批次。

### 3.3 安全第 4 层 scopes 细粒度

~~`paired_devices` 表 `device_pubkey`/`scopes` 列仍存占位 `'[]'`。请求签名（T4）已启用 HMAC + nonce；scopes 细粒度授权（每设备独立能力位）未做。~~ → **已落**（本批次）：
- `pairing.rs::pair()` 写入最小能力集 `DEFAULT_DEVICE_SCOPES = '["chat"]'`（非恒占位 `'[]'`），对齐 D5 拍板"会话级：可发消息看会话，高危工具强制桌面确认"
- `hermix-core::origin::Capability` 派生 `Hash`（`HashSet<Capability>` 需要）
- `security.rs` 加 `parse_device_scopes`（JSON 数组字符串 → `HashSet<Capability>`，未知能力位名忽略=保守不授予）+ `effective_decision_for_paired`（**矩阵 ∩ 能力位**：矩阵 `Denied` 的即使能力位误配也保持 `Denied` 纵深防御；矩阵 `Allowed`/`NeedsLocalConfirmation` 的，能力位未配即降 `Denied`；非设备来源透传矩阵判定）
- `request_guard` 来源推导后把命中设备的 `device_scopes`（`Option<HashSet<Capability>>`）装进 Extension，与 `origin` 并列——下游（http 端点 / 工具层关口）按 `origin` 取设备 id 再查此集判定
- **工具关口消费点接通**（本批次续落）：`hermix-tools` 的 `ApprovalManager` 新增 `check_approval_for_origin_with_scopes(command, origin, device_scopes)`（§3.3 矩阵∩能力位落到审批引擎）——`PairedDevice` 下未配 `DangerousTool` 能力位的设备连 shell 都跑不了（直接 `Denied`，§3.3 D5 拍板"最小集 `["chat"]` 设备连 shell 都跑不了"落地）；配了 `DangerousTool` 则回矩阵 `NeedsLocalConfirmation` 语义（需桌面确认）；只读命令（`READONLY_PATTERNS`）是 `Chat` 级语义不需 `DangerousTool`，仍放行；灾难命令（`DENIED_PATTERNS`）在任何能力位配置下都 `Denied`。`ToolCallContext` 加 `device_scopes: Option<HashSet<Capability>>` 字段（默认 `None`，桌面 WS 路径无设备维度）；`terminal.rs` 关口从 `ctx.device_scopes` 读能力位集传入。旧的 `check_approval_for_origin`（无能力位维度）保留为零回归入口，7 个现有调用方零改动
- 新增 6 个能力位集行为测试（`engine.rs`）：未配 `DangerousTool` 拒 shell / 配了仍需桌面确认 / 只读放行 / 灾难命令不可覆盖 / `Local` 忽略 `scopes` / `None` 与旧行为一致

**未做**（§3.3 剩余）：
- `device_pubkey` 列仍占位（签名密钥协商批次，需设备侧公钥生成 + 落库，未启动）
- ~~HTTP 端点级 Capability 消费点接通~~ → **已落**（本批次，见 §3.3 续落段）：`check_capability`/`check_capability_from_extensions` 落到 6 个高危 handler（`/v1/commands` + `/v1/automations/{id}/trigger` DangerousTool / `/api/pairing/devices/{id}` DELETE DeviceManagement / `/api/config` PUT + `/api/env` PUT/DELETE ConfigMutation），PairedDevice 来源未配能力位 → 403，Local 放行，矩阵 Denied 纵深兜底。axum 0.8 的 `MethodRouter::route_layer`/`Router::route_layer` 都接不住 `FromFn` 中间件层（类型面不匹配），故走 handler 内联判定（零路由结构改动，六行级别）
- 设备管理 UI 编辑 scopes（`DevicesTab.tsx` 加能力位勾选，撤销/编辑流程）
- 测试基线更新：hermix-server 188→193 passed（+5 端点级能力位判定测试 `check_capability_*`）；hermix-tools 697 passed / hermix-agent 1050 passed（零回归）；全仓 cargo check --workspace 绿

### 3.4 L2 任务运行时（TaskRecord + Checkpoint 模型）

设计 §3.4：`hermix-task` 侧的任务级持久状态机。当前 `hermix-task` 已有
`effect.rs`（Effect Sandwich 原语，P3 批次落地）+ `repository.rs` / `service.rs`，
但 `TaskRecord` + `Checkpoint` 模型尚未定义——它是 P2 EntryRecord 完整对齐
（§3.5）的前置依赖：`id`（单调 Id 类型）/ `model` / `head` / `edits` 四个
字段属本批次，需先定 TaskRecord 任务归属语义再回填 Entry。

**已落**（本批次）：
- `crates/hermix-task/src/record.rs`（新）：
  - `Checkpoint { from_step: Option<String>, at: i64 }`——恢复上下文（Pending/Running 携带；`from_step=None` 表示整任务重跑，后续批次细化 step 语义）
  - `TaskOutcome` 五值：`Completed{result}` / `Failed{error,result}` / `Aborted{reason}` / `Orphaned{reason}` / `Faulted{error}`（设计 §3.3 原文 1:1）
  - `TaskState` 三值：`Pending{checkpoint}` / `Running{checkpoint}` / `Terminal{outcome}`，`as_str()` 落库 + `outcome()` / `checkpoint()` 投影 API
  - `TaskRecord { id, conversation_id, state, updated_at }`：`id` 与 `tasks` 表同 UUID（对齐 `EntryRecord.by_task_id` 归属），`conversation_id` 与 `hermix-session` 会话对齐
  - `TaskRecordStore`（`task_records` 表，与 `EffectLog` 同库 sessions.db WAL）：`save`（覆盖式 upsert，状态转移多次调用实现）/ `get` / `list_by_conversation` / **`scan_unfinished`（崩溃恢复扫描，只读，返回所有 `state='running'` 记录；实际状态转移由调用方 `save` 实现）**
  - 损坏 outcome_json → `Orphaned` 兜底；未知 state 字符串 → `Orphaned`
- 5 个测试：pending→running→terminal 全生命周期 roundtrip / scan_unfinished 返回 running / list_by_conversation 过滤 / 损坏 outcome 降级 Orphaned / checkpoint JSON roundtrip
- `lib.rs` re-export：`Checkpoint` / `TaskOutcome` / `TaskRecord` / `TaskRecordStore` / `TaskState` / `TaskRecordError`

**未做**（§3.4 剩余，需与 P2 消息读写切换 + P5 多端挂载联合验收）：
- `Checkpoint.from_step` 细化到 agent loop 步骤（当前 `None` = 整任务重跑；细粒度恢复 = 跨 hermix-agent 批次）
- `scan_unfinished` 与 `EffectLog.scan_unfinished` 的联合恢复协议：扫 running 任务 → 查其 EffectLog 未决 intent → 幂等 Replay / 非幂等 NeedsReview / 无可迁移降级 Orphaned（设计 §3.4 崩溃恢复协议完整实现；当前 TaskRecordStore 与 EffectLog 各自独立，联合判定逻辑未写）
- TaskRecord 接入 server 装配（`HttpServerState` 加 `TaskRecordStore` 字段 + 任务 HTTP 端点读 TaskRecord 权威字段，对齐设计"UI 读权威字段不推断"）
- `model` / `head` / `edits` 三字段（设计 `EntryRecord` 剩余字段，属 P2 消息读写切换批次，需 `hermix-session` Entry 扩展 + 上下文编辑指令类型）

### 3.5 设计文档 §7 其余阶段（P2 会话内核后续 / P6 防滥用 + 审计 / P8 soak）

**已落**（P6 防滥用 + 审计最小切片）：
- `pairing.rs` 新增 `audit_log` 表（与 `paired_devices` 同库 `devices.db`，`open` 时建；`ts`/`event`/`device_id`/`source_ip`/`decision` 五列 + `idx_audit_ts` 索引）
- `AuditEvent` 枚举（`Pairing`/`Auth`/`Grant`/`Revoke` 四类，`as_str` 落盘字符串形式）+ `AuditRow` 结构体（模块顶层，供读侧展示）
- `DeviceRegistry::record_audit(event, device_id, source_ip, decision)` 旁路写 API（失败仅 `tracing::warn`，不阻断主路径）+ `list_audit(limit)` 读 API（时间倒序，limit 钳制 1..=1000）
- `request_source_ip(peer, headers)` 来源 IP 提取（XFF 第一跳优先，回退连接对端；`None` 落 `NULL`）
- 四个安全关口接审计落盘：
  - `complete_pairing`（pair 成功后落 `Pairing` 行）
  - `security.rs::request_guard` 的 `authenticate` 命中点（token 命中 → `Auth`/`allowed`；出示 token 未命中 → `Auth`/`denied`）
  - `list_devices`（查询落 `Grant` 行）
  - `revoke_device`（撤销命中落 `Revoke`/`revoked`；未命中落 `Revoke`/`not_found` 便于排查滥用扫描）
- 测试（+6）：`record_audit_writes_and_list_audit_reads`（落库→读库闭环 + 时间倒序 + Auth 行 device_id=None）/ `list_audit_respects_limit`（limit 钳制）/ `audit_events_map_to_stable_strings`（四事件字符串）/ `pair_revoke_lifecycle_emits_audit_events`（生命周期审计）/ `request_source_ip_prefers_xff_first_hop` / `request_source_ip_falls_back_to_peer_when_no_xff`

**已落**（P2 会话内核不变量补缺 + EntryRecord 对齐）：
- `hermix-session` 补三条硬性不变量测试（设计 §3.3 I1/I8，八条不变量逐条有测试验收锚补齐）：
  - **I8 poisoned**：`SessionError` 加 `Poisoned` 变体 + `Inner.poisoned` 标志 + `begin` 检查 + `commit` 失败置 poisoned（不确定存储失败 → 致命，后续 `begin` 返回 `Poisoned`，须从快照重建）。3 个测试：`failed_commit_changes_nothing_and_yields_no_release`（修正断言）/ `poison_persists_across_clones` / `successful_commits_leave_the_session_unpoisoned`
  - **I1 并发**：`i1_concurrent_commits_do_not_interleave`（1000 并发 `begin/commit` 无交错 + seq 连续 1..=2000，`begin` 非阻塞报 `LineHeld` 由测试侧重试）
- **P2 EntryRecord 对齐**（设计 §3.3 `EntryRecord` 8 字段 → 内核侧对齐 2 个核心字段）：
  - `Entry` 补 `by_task_id: Option<String>`（任务归属，重连/对账拼回未完成 task）+ `conversation_id: Option<String>`（会话归属，多会话合并日志区分）
  - `Draft::append_with_task`（填任务归属）+ `Draft::set_conversation`（批量打会话标记）构建器
  - `sqlite_sink` schema 扩展（`session_entries` 表加 `by_task_id`/`conversation_id` 两列）+ INSERT/SELECT 同步
  - 2 个 round-trip 测试：`task_attribution_survives_the_roundtrip` / `entries_without_attribution_default_to_none`
- 测试基线更新：hermix-session 13→15 passed（+2 EntryRecord round-trip）；`SessionError` 新增 `Poisoned` 变体（原 `Persist` 变体在 `commit` 失败路径被 `Poisoned` 取代——I8 语义：失败即致命，不再允许调用方"重试"）

**已落**（P8 稳定性验证——短 soak 探针 headless 切片）：
- `hermix-agent/tests/soak.rs` 重写：合成 1KB 分配探针 → **真实 hermix-session 提交协议循环**（每轮 `begin`→`append`/`append_with_task`/`set_conversation`→`commit` 经 `SqliteSink` 写盘 + `load` 回读校验），让探针真跑会话内核的持久化路径而非空转
- **四项 SLO 测量补齐**（原探针仅测 RSS 1 项，头部注释声称的 fd + task 数完全没实现）：
  - 内存：`get_rss_bytes`（`GetProcessMemoryInfo`，原有）
  - 句柄数（≈fd）：`get_handle_count`（Windows `GetProcessHandleCount`，kernel32；非 Windows 返回 0 并跳过该检查）
  - task 数稳定：`spawn_probe_task` + `TASKS_SPAWNED`/`TASKS_FINISHED` 原子计数近似活跃 task（泄漏 → delta 持续增长；终值断言 ≤ 8）
  - 0 未处理 panic：循环体 `catch_unwind` 包裹同步 session 工作 + 探针 task 的 `JoinError` 计入 panic 数
- **60s headless 探针**（`soak_60s_headless_probe`，非 ignored，CI `cargo test --workspace` 默认跑）：`HERMIX_SOAK_SECS` 可覆盖时长（默认 60s）。8h 全量 SLO 验证保留 `--ignored` 变体（实机 nightly）
- 句柄数增长预算 64（WAL checkpoint 周期 1-2 句柄/次的舒适上限）；内存 SLO 沿用 200MB
- 测试基线：hermix-agent lib 1050 passed（零回归）/ `soak` 2 passed + 1 ignored

**未做**（P6 剩余 + P2 剩余 + P8 8h 全量）：
- **P2 EntryRecord 完整对齐**：设计 `EntryRecord` 8 字段（`id`/`conversation_id`/`kind`/`model`/`data`/`head`/`edits`/`by_task_id`）本批次只对齐 `by_task_id` + `conversation_id` 两个核心归属字段；`id`（单调 Id 类型）/ `model: Option<Vec<Message>>` / `head` / `edits: Vec<ContextEdit>` 属 L2 任务运行时批次（§3.4），需 `hermix-task` 的 `TaskRecord` + `Checkpoint` 模型先落
- **P2 消息读写切换接入 server**：`hermix-session` 当前 0 处被引用（孤岛），把 `getSessionMessages` / 消息写路径从 `local_agent_service.rs`（4364 行上帝文件）切到 Session entry 派生需跨 P2/P5/P7 批次 + 真机 100 次重连对账，不在本批次
- **P6 剩余**：~~T6 暴力破解 IP 锁定~~ → **已落**（本批次，见 §3.5 T6 段）/ 连接数+请求大小+空闲超时四件套（设计文档明写"随手机客户端批次补"）/ 审计日志管理端点 + DiagnosticsTab 展示（UI 批次）。~~NeedsLocalConfirmation 关口消费点接通（工具关口批次）~~ → **已落**（见 §3.3 续落段）
- **P8 8h 全量 SLO**：8h soak（内存 ≤200MB / fd 不增长 / task 数稳定 / 0 未处理 panic）headless 不可行，实机依赖，blocked。本批次落的是 60s 短探针 headless 切片（四项 SLO 测量 + 真实 session 协议循环）；8h 全量走 `--ignored` 变体（实机 nightly）。另外短探针跑 `cargo test` 期间可能触发 `hermix-cua` 的 rustc ICE（增量缓存损坏，`rm -rf target/debug/incremental` + `CARGO_INCREMENTAL=0` 可规避，属编译器 pre-existing bug 非本项目缺陷）
- 测试基线：hermix-server 188 passed（含 5 T6 锁定测试 + 6 能力位集测试）/ hermix-session 15 passed / hermix-tools 697 passed（含 6 能力位集测试）/ hermix-agent lib 1050 passed + soak 2 passed / 全仓 cargo check --workspace 绿

**已落**（P6 T6 暴力破解 IP 锁定）：
- `security.rs` 新增 `AuthFailLockout`（仿 `RateLimiter` 的 `Arc<Mutex<HashMap<String,(Vec<Instant>,Option<Instant>)>>` 模式，纯内存 headless 可验）：按 source IP 累计认证失败时间戳，窗口内达 `threshold` 即锁定 `cooldown` 期；冷却期内持续失败走**有界续期**（`until + cooldown` 但封顶 `now + 2*cooldown`，防滑动窗口导致攻击者永不解锁）；成功认证 `record_success` 清零该 IP 计数与锁定（合法用户输错后输对了不累积）
- `HttpSecurity` 加 `auth_fail_lockout: Option<Arc<AuthFailLockout>>` 字段；`from_env` 读 `HERMIX_AUTH_FAIL_LOCKOUT`（默认启用，`=0` 关闭）+ `HERMIX_AUTH_FAIL_THRESHOLD`（默认 5）/ `HERMIX_AUTH_FAIL_WINDOW_SECS`（默认 600）/ `HERMIX_AUTH_FAIL_COOLDOWN_SECS`（默认 900）三个 env 覆盖
- `request_guard` 三处接线：认证前 IP 锁定检查（被锁 IP 直接 429 "too many auth failures"，不评估 token，防攻击者借失败响应试探）/ 设备签名 401 分支 + api_key 401 分支各自 `record_failure` / 认证成功（设备命中 或 api_key 命中）`record_success` 清零
- 5 个 T6 行为测试（`security.rs`）：`lockout_triggers_after_threshold_failures`（窗口内达阈值即锁）/ `lockout_is_per_ip`（IP 隔离不串扰）/ `success_resets_failure_counter`（成功清零 + 重新计数）/ `lockout_expires_after_cooldown`（冷却到期自动解锁）/ `ongoing_failures_extend_cooldown`（持续失败有界续期，防"刚好过冷却就重来"绕锁）
- 对抗式定界（YAGNI）：不做分布式锁定（单机 `Arc<Mutex>` 足够，跨进程需落库属另一批次）/ 不做跨重启持久化锁定（重启清零，设计文档 T6 明写"时间窗冷却"进程内语义足够）/ 不锁白名单内合法 IP（`ip_allowed` 已在 `request_guard` 最前拦，T6 只针对认证失败维度，避免把 token 输错的合法用户锁死）/ 锁定粒度 = source IP 非 token（同 IP 多 token 都算，符合暴力破解语义）
- 测试基线：hermix-server 183→188 passed（+5 T6 锁定测试）

### 3.6 手机客户端

**D6 已拍板不在本期**，另立项目。本期交付的协议契约：QR 负载结构、4 个配对端点、TLS 指纹 TOFU，见 `pairing.rs` 与本文档。

---

## 4. 铁律（违反 = 返工）

1. **验证双绿**：`cargo check --workspace --all-targets` + 对应 crate `cargo test` 全过，才能报完成。前端改动加 `pnpm tsc --noEmit`。
2. **cargo 测试一律串行**：并行跑两个 cargo test 会撞 target 文件锁（os error 5）。
3. **注释只写功能/技术根因**，禁日期/人物/changelog 体。
4. **一语义一 commit**，精确暂存（禁 `git add -A`），commit message 含改动清单 + 测试证据。
5. **工具层接缝纪律**：新 origin-aware 工具实现 `ContextAware::execute_with_context` 读 `ctx.origin` 即可，禁止在 trait impl 外再加旁路；`ToolHandler::execute` 默认语义 = Local，不许改。
6. **Unpaired 关口双层纵深**：HTTP 层拦截（security.rs）+ 工具层 `Origin::may` 兜底，改任何一层都要保另一层完整。
7. **token 安全**：设备 token 只存 SHA-256（`pairing.rs` 已实现），任何新代码禁止明文落库/落日志。
8. **零 config schema 变更**：网络开关走环境变量（`HERMIX_SERVER_ADDR` / `HERMIX_TLS_ADDR`），模式已定型。
9. **前端 UI**：走设计 token，禁蓝紫色系，浅色简约呼吸感，Lucide 风格内嵌 SVG 图标。
10. **六步决策阶梯**（AGENTS.md）：YAGNI → 标准库 → 原生平台 → 现有依赖 → 单行 → 最小实现。不可削减项：信任边界校验、防数据丢失错误处理、安全措施。

## 5. 已知坑（前人踩过，别再踩）

- axum 0.8：trait impl 里不能放 inherent 方法；handler 多分支返回类型必须统一 `.into_response()`；改 Request 要 `into_parts()` → 装扩展 → `from_parts()` 装回。
- hermix-store 闭包返回裸 `T` 不是 `Result`（用 `.is_ok()` / `.flatten()` 处理）。
- rustls 必须显式 ring provider（`builder_with_provider(ring::default_provider())`），aws-lc-sys 在 MSVC 要 CMake/NASM + 多 provider 运行时 panic。
- axum-server 0.7.2+ TLS 内置但 module 在 feature `tls-rustls-no-provider` 后面；`from_tcp_rustls` 保住 SO_REUSEADDR。
- rcgen 0.13 SAN 类型叫 `SanType`；std `UdpSocket::connect` 返回 `Result<()>` 不是 socket。
- Git Bash 里 `curl -d` 传中文会 GBK 乱码，冒烟测试用 ASCII。
- Windows spawn 控制台子进程加 `.creation_flags(0x08000000)` 防黑窗。

## 6. 明确不做（Non-goals，设计 §11 + §1）

- ❌ 逐行移植 PI 的 TS 代码
- ❌ 抄 Chord（Hermix 无跨进程分片需求）
- ❌ 手机端 App（本期）
- ❌ 前端大改（seq 对账 ≤300 行、0 组件改动）
- ❌ 多平台分发 / 交叉编译（Windows MSVC only）

## 7. 工作区遗留物（接手前处理）

- `paired_smoke.json`（仓库根）：冒烟测试残留，**含一次性明文 device_token，直接删除**。
- untracked 文件（`WorkbenchChat.tsx`、`DiagnosticsTab.tsx`、`design-previews/`、三个 docs、两个 scripts）：历史批次产物，归属其他工作线，**不要动、不要顺手提交**。
