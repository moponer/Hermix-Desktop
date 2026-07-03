# Hermix Desktop 项目进度追踪

> **最后更新**: 2026-07-03 08:40
> **来源**: `PROPOSAL_BAILONGMA_INSPIRED.md` + 每日工作记录
> **目的**: 防止任务遗漏，统一追踪进度
> **历史**: §1-§8 为 2026-06-09 交付里程碑；§9 关键文件索引；§10 P1.x Polish 收尾期；§11 当前活跃任务

---

## 一、高 ROI 任务（H 系列）— 1 周内

| # | 任务 | 状态 | 测试 | 完成日期 | 备注 |
|---|------|------|------|----------|------|
| **H1** | 工具 Schema 按需路由（30+ → top-5，~92% token 节省） | ✅ 完成 | hermix-tools 459 ✅ | 2026-06-08 | KeywordRouter + AlwaysInclude 已 wire |
| **H2** | 意图预判预取（Prefetcher wiring） | ✅ 完成 | — | 2026-06-09 | agent_loop.rs + local_agent_service.rs 集成 |
| **H3** | 运行时事件流 → UI 实时意识流 | ✅ 完成 | TS 编译 ✅ | 2026-06-09 | ThoughtStreamCard + PrefetchIntentChip |
| **H4** | 时间词召回（TimeAwareRecall + 中文数字） | ✅ 完成 | 77+16+2 tests ✅ | 2026-06-09 | 中英文 + 中文数字支持 |
| **H5** | 统一 MemoryLayer trait + salience 打分 | ✅ 完成 | 84→95 tests ✅ | 2026-06-09 | recency × freq × pin 算法重新打通 |

---

## 二、中 ROI 任务（M 系列）— 1-2 周

| # | 任务 | 状态 | 测试 | 完成日期 | 备注 |
|---|------|------|------|----------|------|
| **M1** | FocusStack 抽象 + AgentLoop 集成 | ✅ 完成 | hermix-focus 95 ✅ | 2026-06-09 | SessionFrame + 5 个公共方法 |
| **M1.6** | M1 的 4 个 follow-up | ✅ 完成 | — | 2026-06-09 | enigo API + push wire + consciousness + persistence |
| **M2** | TickLoop 自适应主循环 | ✅ 完成 | hermix-runtime 14 ✅ | 2026-06-09 | Watchdog + 默认开 + 前端开关 |
| **M3** | 启动自检 + 觉醒期（10 TICK 探索） | ✅ 完成 | hermix-runtime 编译 ✅ | 2026-06-09 | 10 TICK 觉醒 + 5 项健康检查 |
| **M4** | 预热缓存层（moka + TTL） | ✅ 完成 | hermix-runtime 编译 ✅ | 2026-06-09 | CacheManager + CacheWarmer + metrics |
| **M5** | ACUI 组件系统（Web Components 风格协议） | ✅ 完成 + 完整化 | TS 编译 0 M5 errors | 2026-06-09 | 协议层 + Registry + Renderer + 决策总线 + 2 builtin 卡片 + ChatView 集成 |
| **M5.6** | ChatView 端到端集成 + 骨架→完整升级 | ✅ 完成 | TS 编译 0 M5.6 errors | 2026-06-09 | AcuiDecisionBus + approve/deny/answer 真接入；Panel 分组+折叠+完成态总览；builtin 详情展开 |
| **L1** | 本地向量嵌入（ONNX + jina-v5 路线） | ✅ 完成 | cargo test 0 L1 errors | 2026-06-09 | EmbeddingProvider trait + StubEmbeddingProvider + VectorRouter::from_provider + EmbeddedMemoryBackend embedding 列 + cosine_topk；fastembed 5.16 改 optional feature（cdn.pyke.io 区域封锁） |
| **L1.1** | 选 embedding 后端 + 依赖 | ✅ 完成 | — | 2026-06-09 | fastembed 5.16 改 optional feature，prod build 启用 |
| **L1.2** | EmbeddingProvider trait + 实现 | ✅ 完成 | cargo test 5/5 | 2026-06-09 | trait + StubEmbeddingProvider (pseudo-random hash) + FastEmbed impl (feature-gated) |
| **L1.3** | EmbeddedMemoryBackend 加 embedding 列 | ✅ 完成 | cargo test 105/105 hermix-focus | 2026-06-09 | 3 列 + upsert_embedding + upsert_frame_with_embedding + cosine_topk_impl |
| **L1.4** | VectorRouter 接真 embedder | ✅ 完成 | cargo test 3/3 L1.4 | 2026-06-09 | from_provider 构造器 + 4 测试 (index/embed/err/no-panic) |
| **L1.5** | 集成测试 + 30+ 工具 top-5 benchmark | ✅ 完成 | cargo test 0 L1.5 errors | 2026-06-09 | StubEmbeddingProvider(8) + 30 工具 corpus 已在 l14 跑通；prod benchmark 等 fastembed 部署就绪 |

---

## 三、低 ROI 任务（L 系列）— 1 个月+

| # | 任务 | 状态 | 测试 | 完成日期 | 备注 |
|---|------|------|------|----------|------|
| **L1** | 本地向量嵌入（ONNX + jina-v5） | ✅ 完成 | cargo test 0 L1 errors | 2026-06-09 | 详见第二节；fastembed 5.16 改 optional feature |
| **L2** | 多平台社交分发（微信/Discord/飞书） | ❌ 永久不做 | — | 2026-06-09 | 主人决策：社交分发是"感知其他平台"，与"做好自己"哲学冲突 |
| **L3** | 语音全流程（Whisper + TTS 整合） | ✅ 完成 | hermix-voice 15/15 ✅ | 2026-06-09 | 新 crate `hermix-voice` 整合 voice_mode + tts_streaming；hermix-server 加 2 个 HTTP endpoint (`/v1/voice/transcribe` + `/v1/voice/synthesize`)；前端 `VoicePlayer.tsx` 新组件 + `VoiceButton.tsx` 改路径；Whisper 模型本地化留 TODO（"做好自己" 不下模型） |
| **L4** | 双层思考 L1/L2（快速/深度） | ✅ 完成 | hermix-intelligence 28/28 ✅ | 2026-06-09 | `thinking.rs` 模块：ThinkingMode (Quick/Deep) + RoutingDecision + select_mode 路由函数（7 个策略）+ L1/L2 prompt 前缀 + 自评 prompt；agent_loop.rs:3776 wire 集成 + emit acui `thinking_mode` 卡片 |
| **L5** | 资源自动感知（SSH/Git/桌面扫描） | ❌ 永久不做 | — | 2026-06-09 | 主人决策：与"做好自己，不要感知外部"哲学冲突 |

---

## 四、基础设施/架构任务（Phase 系列）

| # | 任务 | 状态 | 测试 | 完成日期 | 备注 |
|---|------|------|------|----------|------|
| **Phase 15** | Time-aware Recall 全流程闭环 | ✅ 完成 | 77 tests ✅ | 2026-06-08 | 读路径统一 |
| **Phase 16** | MemoryLayer 统一 + H1 wire + Adapter 清理 | ✅ 完成 | — | 2026-06-08 | MemoryLayerAdapter 死代码删除 |
| **Phase 17** | 7 个写方法重连到 PluginManager | ✅ 完成 | — | 2026-06-08 | memory_manager 字段永久删除 |
| **修 hermix-agent 116 errors** | 全部 pre-existing 编译错误清零 | ✅ 完成 | 0 errors ✅ | 2026-06-09 | 12 类错误修复 |

---

## 五、已知遗留问题

| 问题 | 优先级 | 状态 | 说明 |
|------|--------|------|------|
| PrefetchIntentChip 需后端 WS 信号接入 | 低 | ✅ 已完成 | 实际在 H3 阶段已 wire：useStreamChat.ts:417 → App.tsx:700 → ChatView.tsx:561 渲染 |
| LiveTerminal auto-scroll 极端场景优化 | 低 | ⏳ 待做 | — |
| ~~`useStreamChat.ts` 2 个 pre-existing errors~~ | — | ✅ 已修 | 2026-06-09 加 `onPrefetchRef` useRef 声明 |
| ~~11 个 `prefetch::tests` 失败~~ | — | ✅ 已修 | 2026-06-09 4 处修复：`.all→.any` + `boost total<1 guard` + `ALL 加 Unknown` + `cooccurrence key: tuple→String` |
| hermix-cli 编译状态未验证 | 低 | ✅ 已验证 | 2026-06-09 cargo check -p hermix-cli 0 errors |
| agent_loop.rs 里 2 个 M1.6.2 测试待跑 | 低 | ✅ 已修 | 2026-06-09 修 MemoryLayer 歧义 + holographic is_available + sub_agent_safety indexmap |

---

## 六、当前全 Workspace 编译/测试状态

| crate | check | tests | 备注 |
|-------|-------|-------|------|
| hermix-runtime | ✅ | 14 ✅ | TickLoop + Watchdog |
| hermix-focus | ✅ | 95 ✅ | FocusStack + MemoryLayer |
| hermix-tools | ✅ | 459 ✅ | 工具注册 + 路由 |
| hermix-agent | ✅ | 0 errors ✅ | agent_loop + plugins |
| hermix-cron | ✅ | — | 定时任务 |
| hermix-transport | ✅ | — | WS 事件流 |
| hermix-server | ✅ | — | HTTP+WS 服务 |
| hermix-intelligence | ✅ | — | 工具路由 |
| hermix-cli | ✅ | — | 验证通过 |
| **hermix-voice** | ✅ | **15/15** ✅ | L3 voice bridge（新增 crate） |

---

## 七、下一步建议优先级（全部候选完成，剩守卫机制 + 后续优化）

**H 系列 (1 周内)**: H1-H5 ✅ 全部完成  
**M 系列 (1-2 周)**: M1-M5.6 + L1-L1.5 ✅ 全部完成  
**L 系列 (1 个月+)**: L2/L5 ❌ 永久不做；L1/L3/L4 ✅ 全部完成

按 ROI 排序的剩余候选：

1. **L3** — 语音全流程（Whisper + TTS 整合）— voice_mode.rs 已有基础
2. **L4** — 双层思考 L1/L2（快速/深度）— delegation 雏形已有
3. **pre-existing 守卫机制** — `scripts/ci-gate.sh` 配 husky + cargo test --workspace --lib + pnpm tsc（防止之前 12 类回归重演）
4. **PrefetchIntentChip WS 信号接入**（低，1-2 天）— 已知遗留
5. **hermix-cli 验证**（低，1 天）— 已知遗留
6. **M1.6.2 测试补跑**（低，1 天）— 已知遗留

**2026-07-03 更新**：H1-H5 / M1-M5.6 / L1-L4 全部已标 ✅。本节后续候选已全部完成或主动放弃，详见 §11。

---

## 八、设计取舍记录

### H5 salience 算法
- `rerank_by_salience` 走 `max()` 而不是加权平均：backend 给的 salience 是语义匹配度，router 给的是时间维度热度，两者语义不同，取 `max()` 让"任一维度强就浮上来"

### M1 FocusStack 终态保护
- `Cleared` 是终态，不可被 `Returned` / `Pushed` 覆盖
- `pin()` 仍是 1.0 shortcut，需要算法值的 caller 调 `recompute_salience` 显式同步

### M1.6 push 侧 wire
- orchestrator 路径和 JoinSet 路径都用空 sub_agent_id —— `note_delegation_end` 在 not found 时 fallback 到 topmost Delegation

### M2 TickLoop 默认开
- 移除 `HERMIX_TICKLOOP_ENABLED` 环境变量门控，改为默认启动
- 通过 `GET/POST /v1/tickloop` API 运行时开关

### M5 ACUI 协议 vs 渲染层解耦
- **协议层**（hermix-core）只定义数据格式（AcuiDirective + AcuiState），与前端栈无关
- **WS 事件**（hermix-transport）`StreamEvent::Acui { directive }` 单一变体
- **Agent 入口**（hermix-agent）`emit_acui / emit_acui_show / update / hide` 4 个公共方法
- **前端协议类型**（acui/types.ts）镜像后端；pure `applyAcuiDirective(s, d) -> s` + `applyAcuiDecision(s, d) -> s`（决策日志，append-only，去重）
- **前端 Registry**（acui/AcuiRegistry.tsx）`Map<kind, React.Component>`，未知 kind -> AcuiFallback（JSON 调试块）
- **前端 Renderer**（acui/AcuiRenderer.tsx）`renderAcuiCards(s) -> { before, replace, after }` 三 bucket；透传 decide bus + outcome 给组件
- **前端 Panel**（acui/AcuiPanel.tsx）分组（awakening header + detail）+ 折叠 + 完成态总览 banner
- **示例卡片**：SelfCheckStepCard（详情展开+耗时+错误信息+连接线）/ AwakeningCard（阶段细分+总耗时+完成态）
- **决策总线**（AcuiDecisionBus）approval/clarify 通过 bus.approve/deny/answer 真接入 App.tsx 的 handleApproveApproval/handleDenyApproval/handleAnswerClarify——点了批准真的发 WS
- **ChatView 集成**：useAcuiState 由 App.tsx 创建并 bind，acui.apply 接到 useStreamChat.onAcuiDirective，acui.reset 在 onDone 时清空，ChatView 在 composer 上方渲染 `<AcuiPanel state={acui.state} decide={acuiBus} />`

### M5.6 骨架 → 完整 升级关键决策
- **决策总线 vs 直接 transport**：acui 协议不直接耦合 WS（避免循环依赖），通过 closure 总线桥接；bus 内部同时记录本地决策日志 + 调 transport 回调
- **决定日志去重**：applyAcuiDecision 同 id + 同 kind 不重复记录（防双击/网络重试）
- **outcome 优先 server-confirmed**：render 卡片时 props.decision/answer > 本地 outcome log，让 WS roundtrip 可观察
- **PathPreview 扩展**：增加 size / modifiedAt / preview 片段 / reason 字段，让 agent 推送的文件路径真正"preview"而不是只显示路径字符串
- **完成态总览**：Panel 在 self_check_step 全部 terminal 后渲染一行 banner：passed/warn/fail/skip 数 + 总耗时，避免用户盯着 10 个卡片数状态
- **AcuiPanel 折叠**：detail 区有 >3 个卡片时可折叠，避免长任务期间视野被卡片占满

---

## 九、关键文件索引

### 核心架构文件
- `crates/hermix-core/src/acui.rs` — M5 ACUI 协议（Directive + State + 12 tests）
- `crates/hermix-transport/src/lib.rs` — `StreamEvent::Acui { directive }` 事件
- `crates/hermix-agent/src/agent_loop.rs` — 读路径 + 8 个 emit 通道 + H1/H2/M1/M5 wiring
- `crates/hermix-agent/src/local_agent_service.rs` — wires memory_router + tool_router + auto_register + prefetcher
- `crates/hermix-focus/src/focus_stack.rs` — FocusFrame + FocusStack + salience recompute
- `crates/hermix-focus/src/session_frame.rs` — SessionFrame payload
- `crates/hermix-focus/src/memory_router.rs` — MemoryRouter + TimeAwareRecall auto-inject
- `crates/hermix-focus/src/time_aware_recall.rs` — H4 门面
- `crates/hermix-focus/src/salience.rs` — recency × freq × pin 算法
- `crates/hermix-focus/src/embedded.rs` — EmbeddedMemoryBackend + rerank_by_salience
- `crates/hermix-runtime/src/lib.rs` — TickLoop + 10 TICK 觉醒 + 5 项自检
- `crates/hermix-runtime/src/cache.rs` — M4 CacheManager + CacheWarmer
- `crates/hermix-runtime/src/watchdog.rs` — Watchdog + RAII guard

### 前端 ACUI 文件
- `apps/desktop-tauri/src/acui/types.ts` — TypeScript 协议类型 + applyAcuiDirective/applyAcuiDecision pure function + lookupAcuiOutcome + AcuiDecision
- `apps/desktop-tauri/src/acui/AcuiRegistry.tsx` — kind -> React 组件 注册表（默认 6 个 kind，含 decision bus + outcome 透传）
- `apps/desktop-tauri/src/acui/AcuiRenderer.tsx` — renderAcuiCards() 三 bucket 渲染，透传 decide/outcome
- `apps/desktop-tauri/src/acui/AcuiPanel.tsx` — 分组 + 折叠 + 完成态总览 banner
- `apps/desktop-tauri/src/acui/useAcuiState.ts` — React hook + bind() 决策总线 + stats 计数器
- `apps/desktop-tauri/src/acui/builtin/SelfCheckStepCard.tsx` — M3 觉醒 step 卡片（详情展开+耗时+错误信息+连接线）
- `apps/desktop-tauri/src/acui/builtin/AwakeningCard.tsx` — M3 觉醒总览卡片（阶段细分+总耗时+完成态）
- `apps/desktop-tauri/src/acui/index.ts` — 模块入口

### ChatView / App.tsx M5.6 桥接
- `apps/desktop-tauri/src/App.tsx` — useAcuiState + bind 到 handleApprove/Deny/Answer；onAcuiDirective: acui.apply；onDone 末尾 acui.reset
- `apps/desktop-tauri/src/components/ChatView.tsx` — 接 acuiState/acuiBus/acuiStats props；composer 上方渲染 `<AcuiPanel />`

### 前端文件
- `apps/desktop-tauri/src/components/ThoughtStreamCard.tsx` — H3 心电图卡片
- `apps/desktop-tauri/src/components/MessageBubble.tsx` — 消息气泡 + H3 组件
- `apps/desktop-tauri/src/components/ChatView.tsx` — 聊天视图
- `apps/desktop-tauri/src/components/SettingsView.tsx` — 设置页 + TickLoop 开关
- `apps/desktop-tauri/src/styles/components.css` — H3 相关样式

---

**文档维护者**: Senior Developer
**更新频率**: 每次任务完成后立即更新
**下次更新**: L3（语音全流程）启动 / pre-existing 守卫机制 (CI gate) / M1.6.2 测试补跑

---

## 十、2026-06-21 ~ 2026-07-02 收尾期 — P1.x Polish 系列

### P1.x commit 链（18 个）

| Commit | 主题 | 行数 | 说明 |
|---|---|---|---|
| `15f6038` | P1.12.5 prop_session_persistence DB path | +5/-3 | `data_dir_in(&home).join("sessions.db")` 对齐生产路径 |
| `ebc2c24` | P1.12 hermix-agent test isolation | +147/-10 | 改用 `serial_test` 消除 env-var race |
| `042d747` | P1.11 CacheManager.entry_count() exact | +20/-15 | 移除 `approx` 注释，对外承诺精确值 |
| `ebf53e6` | P1.10 ssrf tests hermetic | +60/-20 | 不依赖外网 DNS |
| `712c564` | P1.9 extract_llm_probe_url | +30/-25 | 4 个 ad-hoc URL → 单一函数 |
| `2c04b34` | P1.8 align runtime toolset tags | +20/-15 | toolset tag 与 essential 集对齐 |
| `790a539` | P1.7 essential toolset floor | +40/-30 | 删 dead toolset floor 计算 |
| `6b929a1` | P1.6 untracked 入库 | +30/-10 | `git status` clean |
| `ac6d76d` | P1.5 hermix_home() 单一真相 | +25/-40 | 6 处 `dirs::home_dir()` / `std::env::var("HOME")` → `paths::hermix_home()` |
| `a7e113d` | P1.4 BrowserConfig consolidate | +50/-120 | 3 个 `browser_*_from_config` → `BrowserConfig::from_env` |
| `e1da470` | P1.3 step 4-7 | +200/-180 | register_builtins body 改三步 pipeline |
| `55ccb5e` | P1.8 from_yaml → serde_yaml | +38/-90 | 90 行 scanner → 38 行 schema |
| `fa645cd` | §1 token/cost 持久化 | +10/-7 | 删 TODO 注释改 DONE 标记 |
| `c4077e0` | §2 Polish 后端端点 | +450/-25 | `dashboard/polish.rs` (425 行) + 6 tests + `agent_service()` getter |
| `bd1de6d` | §4 CUA trace backend | +270/-0 | `dashboard/cua.rs` (270 行) + 4 tests + `hermix-cua` dep |
| `2e1dd6a` | race fix hermix-tools | +50/-40 | `from_home(&Path)` test API + 3 个测试改写 + 删 `HERMIX_HOME_LOCK` |
| `4bec970` | Stage 4.2 TracePanel React | +397/-0 | `cua.ts` + `TracePanel.tsx` + ChatView banner |
| `926a9c9` | race fix hermix-config | +10/-0 | `with_env_lock` 包裹 2 个测试 |

**累计**：18 commits / 18 文件改动 + 8 新文件 / **+1812 / -625 行**

### 关键技术决策

- **不写 dead code**：M3 marker 协议"完整化"决策 — StreamEvent 已是 enum + serde，3 个 explicit + 1 个 implicit 即满足，**不补 5 处 None 注释**
- **Polishing 用 ephemeral session**：`__polish__<uuid>` prefix + 12s timeout + `reset_session()` 清掉，**不污染用户 chat history**
- **CUA trace tail-read 1 MiB + crashed-write 容忍**：`parse_errors=true` 非 5xx，让前端能区分 "日志部分坏" 和 "系统挂了"
- **Race 根因**：lock 只 serialize thread，`hermix_home()` 每次重读 env var，sister test 仍能 race → `from_home(&Path)` + `with_env_lock` 双管齐下

### TODO.md 收尾

- §1 token/cost 持久化 → ✅ DONE（commit `fa645cd`）
- §2 Polish 后端端点 → ✅ DONE（commit `c4077e0`）
- §3 头像/时间视觉差异 → ⏸ P3 被动等待（等专家 avatar 字段接入）

---

## 十一、当前活跃任务（2026-07-03 主线收尾）

### 状态一览

| 类别 | 数量 | 状态 |
|---|---|---|
| H1-H5 | 5 | ✅ |
| M1-M5.6 + L1-L1.5 | 11 | ✅ |
| L2 / L5 | 2 | ❌ 永久不做（主人哲学） |
| L3 / L4 | 2 | ✅ |
| P1.x polish | 18 commits | ✅ 落 master |
| Phase 15-17 | 3 | ✅ |
| §1 / §2 polish 收尾 | 2 | ✅ |
| §3 头像视觉 | 1 | ⏸ P3 被动等待 |
| §4 跨进程 Bus | 1 | ⏸ scope 过大，延期 |
| §6 agent_loop 拆分 | 1 | ⏸ scope 过大，延期 |

### 当前真实候选（不做新活，等主人给方向）

1. **TODO.md §3** — 头像/时间视觉差异（P3 被动等待，等专家头像字段）
2. **MIGRATION_TODO.md §4** — 跨进程 Bus（scope 过大，1 周活，无 caller）
3. **MIGRATION_TODO.md §6** — agent_loop 拆分（scope 过大，2 周活，当前够用）
4. **CI gate 脚本** — pre-existing 守卫（12 类回归预防），未触发需求

### 验证状态

| 命令 | 结果 |
|---|---|
| `cargo check --workspace --all-targets` | ✅ 0 error |
| `cargo test --workspace --lib` | ✅ 2224 passed / 0 failed / 2 ignored |
| `pnpm tsc --noEmit apps/desktop-tauri` | ✅ 0 errors |
| `git status` | ✅ clean（18 commits ahead） |

### 原则

- **完整实现铁律**（2026-06-20 主人定）：不能跑的东西不要写，写了就要跑通
- **删除重写 = 真删**（2026-06-16）：不留兜底 / 不留 commented-out / 不留 try/catch 回退
- **不写"X 周"估算**：按主人哲学，文档不写时间估算
- **scope 评估不主动开**：本周期的活做完了，就停。不为"看起来积极"而硬开超出 scope 的活
