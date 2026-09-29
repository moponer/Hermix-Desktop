# Hermix 后端架构重构方案 (v6)

> **配套基线**: `ARCHITECTURE_BENCHMARK.md` v6(实测 179,624 行 / 427 文件 / 21 crates)
> **生成时间**: 2026-06-19 12:30
> **原则**: 只针对 ARCHITECTURE_BENCHMARK.md v6 中确证的现状提改进;不引申、不推演

---

## 0. 重构目标

3 个目标(按重要性排序):

1. **可扩展性** — 新增一个 IM 平台、新增一个记忆后端、新增一个 LLM Provider,**不应该修改核心 crate 的代码**
2. **可观测性** — 一次 agent loop 的每一步都有 trace id、可在 dashboard 回放
3. **可测试性** — 不依赖真实 LLM、不依赖真实 IM 平台、不依赖真实数据库就能跑端到端测试

---

## 1. 目标架构(5 层)

```
┌──────────────────────────────────────────────────────────┐
│ L1 入口层 (Entry)                                        │
│   - apps/desktop-tauri            (Tauri 命令 + Sidecar)│
│   - hermix-cli                    (CLI / TUI)            │
│   - hermix-mcp / hermix-acp       (MCP / ACP server)     │
└────────────┬─────────────────────────────────────────────┘
             │
             ▼
┌──────────────────────────────────────────────────────────┐
│ L2 服务层 (Service)                                      │
│   - hermix-server                 (HTTP API / Dashboard)│
│   - hermix-gateway                (IM 网关 / 群聊)       │
│   - hermix-runtime                (TickLoop / Watchdog)  │
└────────────┬─────────────────────────────────────────────┘
             │
             ▼
┌──────────────────────────────────────────────────────────┐
│ L3 业务层 (Business)                                     │
│   - hermix-agent                  (Agent loop / Plugin)  │
│   - hermix-tools                  (Tools / Backends)     │
│   - hermix-skills                 (技能注册表)           │
│   - hermix-cron                   (定时任务)             │
└────────────┬─────────────────────────────────────────────┘
             │
             ▼
┌──────────────────────────────────────────────────────────┐
│ L4 智能层 (Intelligence)                                 │
│   - hermix-intelligence           (LLM 路由 / 辅助)      │
│   - hermix-focus                  (焦点 / 上下文)        │
│   - hermix-memory_plugins         (8 个记忆后端)         │
└────────────┬─────────────────────────────────────────────┘
             │
             ▼
┌──────────────────────────────────────────────────────────┐
│ L5 基础设施层 (Infrastructure)                           │
│   - hermix-core                   (类型 / Trait / 解析)  │
│   - hermix-config                 (配置 / 迁移)         │
│   - hermix-bus                    (进程内 Bus)           │
│   - hermix-transport              (WS 协议定义)          │
│   - hermix-telemetry              (OTLP 遥测)            │
│   - hermix-environments           (本地 / SSH / 评测)    │
└──────────────────────────────────────────────────────────┘
```

---

## 2. 17 个 P0-P2 改进项(全部基于 v6 实测)

### P0(必须做,影响生产稳定性)— 6 项

#### P0-1 BusTransport 跨进程实现缺失
- **现状**(`hermix-bus` 491 行):
  - `transport.rs` 20 行,只有 trait 定义
  - `in_process.rs` 42 行,只一个实现
  - **没有任何跨进程实现**(WS / HTTP / Unix Socket)
- **影响**:所有跨进程通信只能走 `hermix-server` HTTP,bus 沦为进程内玩具
- **方案**:
  - 新增 `crates/hermix-bus/src/ws_transport.rs`(WS 实现,~400 行)
  - 新增 `crates/hermix-bus/src/http_transport.rs`(HTTP 实现,~300 行)
  - 在 `apps/desktop-tauri` 端先试用 WS Bus 把 Tauri sidecar ↔ Rust 后端打通
- **预计**: 1 周
- **风险**: WS 序列化协议要稳定,影响所有用 bus 的模块

#### P0-2 SSRF 防护有 DNS rebinding bypass
- **现状**(`hermix-gateway/src/ssrf.rs` 347 行):
  - 做 IP 段过滤,但**没有做 DNS 解析后再检查**
  - 攻击者可注册 `evil.com → 127.0.0.1`,绕过基于 hostname 的检查
- **影响**:所有走 `url_safety` / `web` / `browser` 工具的请求都有 SSRF 风险
- **方案**:
  - 在 `url_safety` / `web` / `browser` 抓取前先 `tokio::net::lookup_host`
  - 解析后的 IP 再次进入 IP 段黑名单
  - 解析结果缓存到请求结束
- **预计**: 3-5 天
- **参考**: Python `urllib3` 的 `Retry` + DNS pinning

#### P0-3 hermix-agent/src/agent_loop.rs 9,178 行巨型单文件
- **现状**:项目最大单文件,主对话循环、tool dispatch、sub-agent、interrupt、steer、budget、reasoning 全在一起
- **影响**:任何修改都可能破坏主循环,新人难以理解,**几乎不可单测**
- **方案**(渐进式拆分):
  - 第一阶段:抽 `sub_agent_orchestrator.rs`(已存在,629 行)— 把 Sub-Agent 调用从 agent_loop 切出去
  - 第二阶段:抽 `interrupt.rs`(已存在,181 行)→ 并入新 `control.rs`
  - 第三阶段:抽 `steer.rs`(86 行)→ 并入 `control.rs`
  - 第四阶段:抽 `budget.rs`(102 行)→ 并入新 `cost.rs`
  - 第五阶段:agent_loop 只保留主循环骨架
  - **目标**:agent_loop ≤ 4,000 行
- **预计**: 3 周(渐进,不重写)
- **风险**: 大改动要保证每步可编译 + 集成测试通过

#### P0-4 Tauri sidecar 进程崩溃处理不完整
- **现状**(`apps/desktop-tauri/src-tauri/src/sidecar.rs` 611 行):
  - 有 `sidecar_crash_probe.rs`(76 行)和 `sidecar_orphan_pid_recycle.rs`(69 行)两个集成测试
  - 但生产路径上没有 watchdog
- **影响**:Sidecar 崩溃后 Tauri 主进程不感知,前端一直在转圈
- **方案**:
  - 在 `sidecar.rs` 加 5 秒心跳检测
  - 3 次失败触发 sidecar 重启
  - 重启失败后向 dashboard 推送事件
- **预计**: 3 天

#### P0-5 群聊子系统完整性
- **现状**(`hermix-gateway/src/groupchat/` 1,685 行 / 10 文件):
  - 已有完整的 dispatcher/leader/rank/rules/persistence
  - **但 dashboard 端的 groupchat_api.rs 434 行 + groupchat_sqlite.rs 379 行 + groupchat_dispatcher.rs 59 行是新写的**
  - 与 gateway 端的 groupchat 模块存在两套并行实现
- **影响**:两套实现容易漂移,数据模型不统一
- **方案**:
  - 把 `server/dashboard/groupchat_*` 全部标记 deprecated
  - 改为 dashboard 调用 `gateway` 的 groupchat API
  - 或抽出 `hermix-groupchat` crate 共用
- **预计**: 1 周

#### P0-6 telemetry 几乎为空
- **现状**(`hermix-telemetry` 298 行 / 2 文件):
  - 只有 lib.rs 253 + otlp.rs 45
  - **没有任何 OTLP exporter 实现**
  - agent_loop / tools / gateway 都无 trace
- **影响**:生产环境无法定位慢请求、失败请求
- **方案**:
  - 引入 `opentelemetry` + `opentelemetry-otlp` crate
  - 在 `hermix-telemetry` 加 OTLP gRPC exporter
  - 用 `tracing-opentelemetry` 在 agent_loop / runtime / tools 加 span
- **预计**: 1 周

### P1(应做,影响开发效率)— 6 项

#### P1-1 hermix-tools 90 文件缺乏统一注册
- **现状**:每个工具自己 `pub fn register(builder: &mut ToolRegistry)`,分散在 `register_builtins.rs`(1,378 行)
- **方案**:
  - 引入 `inventory` crate 做"全局静态注册"
  - 每个 tool/backend 用 `inventory::submit!` 自动注册
  - 移除 `register_builtins.rs` 1,378 行的手写注册代码
- **预计**: 2 周
- **风险**: 工具启动顺序问题

#### P1-2 hermix-intelligence 3 个新子系统文档缺失
- **现状**:
  - `auxiliary/` 1,746 行(辅助 LLM 调用)— **没有任何 doc comment 说明何时用**
  - `session_insights/` 1,823 行(会话洞察)— **没有任何 API 文档**
  - `models_dev/` 1,500 行(模型元数据)— **没有 README**
- **方案**:每个子系统加 `//! # Module Overview` 模块文档

#### P1-3 hermix-config 18 文件迁移逻辑脆弱
- **现状**(`crates/hermix-config/src/migration.rs` 708 行):
  - 包含从 Python Hermes YAML 配置迁移的所有规则
  - 没有自动化测试覆盖所有迁移路径
- **方案**:每条迁移规则加 fixture 测试,目标覆盖率 80%

#### P1-4 hermix-cron 文件锁实现自研
- **现状**(`hermix-cron/src/file_lock.rs` 322 行):
  - 完全自研的 fcntl 文件锁
  - **没有 Windows 实现**
- **方案**:改用 `fd-lock` crate,跨平台

#### P1-5 hermix-eval 3 个 benchmark 集成度低
- **现状**(`hermix-environments/src/training/benchmarks/`):
  - swe_bench 308 / terminal_bench 245 / yc_bench 222
  - 都是独立文件,没共享 runner
- **方案**:抽出 `Benchmark` trait,3 个 benchmark 实现 trait

#### P1-6 hermix-skills 注册表与 markdown 文件无强绑定
- **现状**(`hermix-skills/src/store.rs` 486 行 + `hub.rs` 447 行):
  - `skill.md` 文件描述技能,但运行时只读 `Skill` 结构体
  - 修改 markdown 不会自动更新结构体
- **方案**:启动时检查 markdown 变更,触发 reload

### P2(可做,影响非核心场景)— 5 项

#### P2-1 hermix-voice 500 行单文件
- 单文件全部逻辑,应拆 `stt.rs` + `tts.rs` + `bridge.rs`

#### P2-2 hermix-bus 9 个 BusMessage 变体命名不直观
- 当前:`AgentCommand / AgentEvent / ...`,需要阅读消息体才能理解
- 方案:加 `///` doc 到每个变体

#### P2-3 hermix-parity-tests 473 行覆盖率低
- 只对齐 Python `hermes-agent` 几个核心场景
- 目标:对齐 30+ 个核心行为

#### P2-4 hermix-environments 沙箱本地模式安全
- `local.rs` 425 行,直接执行命令,**没有能力降级**
- 方案:加 `--read-only` flag + syscall 白名单

#### P2-5 hermix-server security.rs 343 行过粗粒度
- 只一个 auth middleware
- 方案:拆 `auth.rs` / `rbac.rs` / `rate_limit.rs` / `cors.rs`

---

## 3. 12 个决策点(拍板)

| # | 决策点 | 选项 | 我的推荐 |
|--:|--------|------|---------|
| D1 | P0-1 跨进程 Bus **第一站**用在哪里? | A) Tauri Sidecar ↔ Rust 后端<br>B) hermix-server ↔ hermix-gateway<br>C) hermix-cli ↔ hermix-server | **B**(gateway 已有 WS 客户端代码,可直接复用) |
| D2 | P0-2 SSRF DNS 检查**同步还是异步**? | A) 同步(dns.lookup before fetch)<br>B) 异步(tokio::net::lookup_host + tokio::spawn) | **B**(不影响主路径延迟) |
| D3 | P0-3 agent_loop 拆分是否保留 `#[deprecated]` 兼容层? | A) 是,留 1 个版本<br>B) 否,直接切 | **A**(避免一次大爆改) |
| D4 | P0-4 Sidecar 重启策略 | A) 立即重启<br>B) 指数退避<br>C) 5 秒后重启 | **C**(给 OS 资源回收时间) |
| D5 | P0-5 groupchat 合并方案 | A) deprecated server 端,只用 gateway<br>B) 抽出 hermix-groupchat crate<br>C) 保持两套 | **A**(改动最小) |
| D6 | P0-6 telemetry OTLP backend | A) 自建 OTLP gRPC<br>B) OpenObserve<br>C) 只 stdout JSON | **C**(第一阶段够用) |
| D7 | P1-1 `inventory` vs 手动注册 | A) inventory(运行时)<br>B) build.rs codegen | **A** |
| D8 | P1-4 文件锁 crate | A) fd-lock<br>B) 维持自研 | **A** |
| D9 | P2-3 parity-tests 目标场景数 | A) 30<br>B) 50<br>C) 100 | **B**(渐进) |
| D10 | P2-4 environments `--read-only` flag | A) 加<br>B) 不加 | **A** |
| D11 | 全局 Lint 策略 | A) 维持 allow<br>B) warn<br>C) deny | **A**(允许技术债) |
| D12 | Cargo workspace 命名 | A) 维持 hermix-*<br>B) 切回 hermes-* | **A**(已完成 Phase 1,不要回滚) |

---

## 4. 7 个参考架构(仅借鉴,不全抄)

| 参考项目 | 借鉴点 | 不借鉴点 |
|---------|--------|---------|
| **OpenClaw**(中文 IM 网关) | IM 平台 trait 设计、SSRF 防护 | 它是 Python,异步模型不同 |
| **Hermes Agent**(NousResearch,Python) | Agent loop、sub-agent、memory plugins | Python 类型系统 |
| **BaiLongma**(中文 LLM agent) | Tool 路由、工具集设计 | 模型路由策略 |
| **Letta / MemGPT** | 记忆分层、上下文压缩 | 服务化部署 |
| **Cline** | VSCode 工具调用 UX | 编辑器侧 |
| **Goose**(Block) | MCP 集成、provider 抽象 | Recipe 系统 |
| **Aider** | Repo Map、文件引用 | 跨文件编辑策略 |

---

## 5. 风险登记

| ID | 风险 | 影响 | 缓解 |
|----|------|------|------|
| R1 | P0-3 agent_loop 拆分引入回归 | 高 | 每步 PR + 集成测试 |
| R2 | P0-1 跨进程 Bus 协议不兼容 | 高 | 锁版本号 + 灰度 |
| R3 | P0-6 telemetry 增加延迟 | 中 | 用 batched export |
| R4 | P1-1 inventory 在 Windows 行为差异 | 中 | CI 上多平台验证 |

---

## 6. Agent Loop 核心引擎修复进度 (2026-07-08 ~ 2026-07-09)

> **目标**: 完善 Agent Loop 核心架构，解决工具结果未传递给 LLM、fallback 频繁触发、循环重复执行等问题。
> **原则**: 渐进增强，不重写。

### 已完成项

| # | 修复项 | 文件 | 状态 | 验证 |
|---|--------|------|------|------|
| AL-1 | **empty_response_handler 引入总结提示** — 工具执行后 LLM 只产生 thinking 时，发送总结提示而非"继续工作"提示 | `empty_response_handler.rs` | ✅ 完成 | 16 个单元测试通过 |
| AL-2 | **run_loop 安全网对齐** — `run_stream` 中空工具调用安全网仅检查 `has_visible`，与 `decide_empty_response` 逻辑一致 | `run_loop.rs` | ✅ 完成 | 编译通过 |
| AL-3 | **local_agent_service fallback 精简** — fallback 从冗长描述改为简洁"已完成。"，作为最后手段 | `local_agent_service.rs` | ✅ 完成 | 编译通过 |
| AL-4 | **stream_collect 过滤空名工具调用** — 流式组装时过滤掉 `function.name` 为空的占位符 | `stream_collect.rs` | ✅ 完成 | 编译通过 |
| **AL-5** | **Finalize 提前返回导致总结提示无法触发** — `process_post_response` 返回 `Finalize` 时，如果响应无可见文本（只有 thinking），不立即返回，而是落入 `decide_empty_response` 让总结提示触发。**这是"工具已执行但模型未能生成总结回复"问题的根因。** | `run_loop.rs` (run + run_stream) | ✅ 完成 | 编译通过 + 16 测试通过 |
| **AL-6** | **显式状态机 (LoopPhase enum)** — 引入 `LoopPhase` 枚举表示 9 个循环阶段（Init → PreTurnSetup → Prefetch → LlmCall → PostLlmCall → ToolValidation → ToolExecution → PostToolExec → Finalize），在 `run()` 和 `run_stream()` 的每个阶段边界通过 `tracing::debug!` 发射 phase 标签。 | `agent_loop/types.rs`, `run_loop.rs` | ✅ 完成 | 编译通过 |
| **AL-7** | **统一 ToolCallValidator** — 将 `run()` 和 `run_stream()` 中 ~110 行重复的工具调用验证逻辑（无效名称 + 无效 JSON）提取为 `tool_call_validator` 模块。返回 `ValidationOutcome` 枚举 + `ApplyResult` 枚举，让调用者明确知道循环控制流动作。 | `tool_call_validator.rs`, `run_loop.rs` | ✅ 完成 | 587 测试通过 |
| **AL-8** | **收敛检测增强** — 新增 `convergence_detector` 模块，超越简单工具去重。检测三种模式：(1) 无进展循环（连续 N 轮工具调用但无可见文本）、(2) 错误循环（连续 N 轮全部工具错误）、(3) 相似内容循环（连续 N 轮可见文本 >80% 相似）。使用 Jaccard 相似度算法。 | `convergence_detector.rs`, `run_loop.rs` | ✅ 完成 | 6 个单元测试通过 |
| **AL-9** | **上下文分层模型文档化** — 在 `context.rs` 模块文档中明确建模 5 层上下文：System Prompt (Stable) / Tool Definitions / Conversation History / Memory / Dynamic Suffix。每层标注失效语义和缓存策略。 | `context.rs` | ✅ 完成 | 编译通过 |
| **AL-10** | **结构化 Turn Report** — 新增 `turn_report` 模块，每轮生成可序列化的 `TurnReport` 结构体（轮次、阶段、API 时长、工具调用列表、token 使用、结果枚举）。通过 `tracing::info!` 发射。 | `turn_report.rs` | ✅ 完成 | 编译通过 |

### AL-5 根因分析

**问题现象**: 工具执行成功后，LLM 产生了 thinking-only 响应（无可见正文、无工具调用），导致 fallback 消息"工具已执行，但模型未能生成总结回复"频繁出现。

**根因链路**:
```
LLM 产生 thinking-only 响应 (无工具调用)
  → process_post_response 检测无工具调用 → 返回 Finalize
  → run()/run_stream() 的 Finalize 分支 → 立即 return finalize_agent_result()
  → ❌ 跳过 prepare_assistant_turn (助手消息未加入 ctx)
  → ❌ 跳过 decide_empty_response (总结提示永远不触发)
  → local_agent_service 检测空可见文本 → 注入 fallback 消息
```

**修复**: 当 `Finalize` 返回但响应无可见文本时，不立即返回：
1. 检查 `assistant_visible_text_after_think_blocks(&response.message)`
2. 有可见文本 → 立即 finalize（正确行为）
3. 无可见文本 → 落入 `prepare_assistant_turn` + `decide_empty_response`
4. 总结提示触发，LLM 在下一轮看到 ctx 中的工具结果，被要求生成可见正文

### 待实施项 (按优先级)

| 优先级 | 修复项 | 描述 | 状态 |
|--------|--------|------|------|
| P0 | **合并 run() 和 run_stream()** | 两者 ~60% 代码重复，已提取共享 helper 但仍需统一为 `run_engine` | 待实施 |
| ~~P0~~ | ~~**显式状态机 (LoopPhase enum)**~~ | ~~当前状态转换散布在多个函数中，需集中为单一 match~~ | ✅ AL-6 已完成 |
| ~~P1~~ | ~~**统一 ToolCallValidator**~~ | ~~工具调用验证逻辑在流式组装、后响应处理、安全网、fallback 注入等多处重复~~ | ✅ AL-7 已完成 |
| ~~P1~~ | ~~**上下文分层模型**~~ | ~~明确建模 5 层：System Prompt / Tool Definitions / Conversation History / Memory / Dynamic Suffix~~ | ✅ AL-9 已完成 |
| ~~P1~~ | ~~**收敛检测增强**~~ | ~~超越简单的工具去重，检测 LLM 是否在产生实质性进展~~ | ✅ AL-8 已完成 |
| ~~P2~~ | ~~**结构化 Turn Report**~~ | ~~每轮生成可观测性报告（工具调用、token 使用、状态转换）~~ | ✅ AL-10 已完成 |
| ~~P2~~ | ~~**移除 local_agent_service fallback 注入**~~ | ~~当 AL-5 修复充分验证后，fallback 应完全移除~~ | ✅ AL-3 已精简为 "已完成。" |
| P2 | **工具审批管线独立** | 将工具审批从 tool_dispatch 中分离为独立阶段 | 待实施 |

---

## 7. 批量处理架构：L1 / L2 / L3 三档 (2026-07-23 外部挑战 衍生)

### 7.0 触发场景

在子 agent 修复（Bug #5 → 2026-07-23 数据传递+并行+阻止递归）落地后追问：

> "如果有 100 张图片要处理呢？1000 张？10000 万张呢？"

L1（`delegate_task` 单点 fan-out）实际上限 ≤ 10 并发，超出会遇到 LLM 物理边界。本节用第一性原则推导三档可扩展架构。

### 7.1 第一性原则 (First Principles)

| 物理约束 | 影响 |
|---|---|
| LLM 单次响应能发多少 tool_call | 倾向 5–10，发 20+ 已经是极限；100+ 不可能 |
| LLM 上下文长度 | 每个 tool_call ≈ 200–500 token；100 个 call = 5–50K token 描述塞回 parent ctx |
| LLM 没有持久任务队列 | 下一轮 LLM "看不见"上一轮发了多少；它倾向 3–5 轮内收敛 |
| Sub-agent LLM 调用延迟 | 每个 sub-agent 5–30s（视觉 + LLM 推理） |
| 100 张图 × 30s/张 / 10 并发 | ≈ 5 分钟（理论上 OK） |
| 10000 张图 × 30s/张 / 10 并发 | ≈ 8.3 小时（**LLM 编排这条路径无论怎么扩都撑不住**） |

**关键结论**：LLM 不能编排"数据"，只能编排"模式"。10000 张图是数据流问题，不是 agent 问题。架构必须分层。

### 7.2 反模式 (Anti-Patterns)

| 反模式 | 为什么崩 |
|---|---|
| LLM 在 prompt 里被要求"分 50 轮发 100 个 tool_call" | 第 4 轮 LLM 就开始偷懒，第 8 轮只剩 30% call 真发出去；剩下 70 个静默丢失 |
| Sub-agent 自己再 delegate_task | 子 agent 已物理 block，但 prompt fragment "use delegate_task for images" 被复制进子 ctx，LLM 反复撞 deny 墙浪费 turn（已修：sub_agent_guidance HARD OVERRIDE） |
| Attachments 传 base64 内联而非路径引用 | 1 张图 ≈ 100–500KB base64，10 张就 1–5MB → parent ctx 爆 |
| 主 agent "等结果" 异步轮询 sub-agent 状态 | delegate_task 是**同步**的（tool_result 即结果），LLM 没有"等异步"的认知模型，回归 pending 死循环 |
| "LLM 自己循环发" 替代 Rust executor | 每轮 LLM 决策成本 × N + LLM 行为不稳定的概率 × N = 不可控 |

### 7.3 三档架构总览

| 档位 | 工具 | 容量上限 | 谁编排数据 | 状态 |
|---|---|---|---|---|
| **L1** | `delegate_task` (单点 fan-out) | ≤ 10 并发 / ≤ 10 attachments per call | LLM | **已上线** (2026-07-23 修) |
| **L2** | `delegate_batch` (Rust executor 串 fan-out) | 10–500 items | Rust executor + LLM 给模板 | **已设计，未实现** |
| **L3** | `pipeline_run` (纯数据流) | ∞ | Rust pipeline（可完全脱离 LLM） | **概念定义** |

### 7.4 L1 现状（`delegate_task`）

**已修问题** (2026-07-23)：

| 问题 | 修复 | 状态 |
|---|---|---|
| `SubAgentOrchestrator` 从未接线 → 永远返回 pending | `local_agent_service.rs` 加 `.with_sub_agent_orchestrator()` | ✅ Bug #5 |
| Sub-agent 拿不到图（父 prompt 有 base64 但被剥离） | `SubAgentRequest.attachments: Vec<ImageAttachment>` + `attach_images` 读盘 + base64 进 multimodal content | ✅ |
| 4 张图串行执行 = 12 分钟 | `tool_executor.rs` 改 `futures::join_all` 并发 fan-out | ✅ |
| Sub-agent 想再 delegate | `sub_agent_safety::BLOCKED_TOOLS_FOR_SUBAGENT` 物理 strip + `sub_agent_guidance` HARD OVERRIDE | ✅ |
| **Schema 不暴露 attachments/output_schema** | `delegation.rs` schema 增 5 字段 + 描述 L1 容量边界 | ✅ 本节 |

**L1 边界（schema 里写明）**：

- ≤ 10 attachments per call（更多会塞爆 parent + child ctx）
- ≤ 10 parallel calls per round（LLM 实际并发上限）
- LLM 不持久任务队列（fan-out 必须同 round 完成）
- 超出 → 主 agent 应该告诉用户"L2 待实现"或分多轮自己做

### 7.5 L2 设计：`delegate_batch` (待实施)

**动机**：100 张图 = 10 轮 × 10 并发，LLM 自己做得到但脆弱（要每轮重新决策、不稳定）。Rust executor 接管后变成"一次调用 + 自动分批 + 进度可见"。

**核心思路**：L2 内部就是 L1 fan-out 的"聚合 + 模板化"。Rust executor 拿到模板和 items，按 concurrency N 启动一组 L1 sub-agent，每个 sub-agent 接收渲染后的 task（含单个 item）+ output_schema。**L2 完全复用现有 SubAgentOrchestrator，不另起炉灶**。

**API 草案**：

```json
{
  "tool": "delegate_batch",
  "params": {
    "task_template": "识别这张车辆照片的车牌号。照片路径：{{item.path}}",
    "items": [
      {"path": "D:\\imgs\\a.jpg"},
      {"path": "D:\\imgs\\b.jpg"},
      // ... up to 500 items
    ],
    "concurrency": 10,           // optional, default = 10, max = 20
    "model": "gpt-4o-mini",      // optional, sub-agent 模型
    "role": "general",           // optional, default = general
    "output_schema": "{\"plate\":\"string\"}",  // 同 L1
    "fail_fast": false,          // optional, true = 任一失败中止；默认 false 继续跑完
    "per_item_timeout_secs": 300, // optional, default = 300
    "total_timeout_secs": 3600   // optional, default = 3600
  }
}
```

**行为契约**：

1. **模板渲染**：Rust 端用 `minijinja` / `handlebars` 之类做轻量 mustache 替换（`{{item.path}}` / `{{item.id}}`），禁止 LLM 二次 prompt 注入。
2. **并发控制**：semaphore 限制在 flight 的 sub-agent 数 ≤ concurrency；每个完成的 sub-agent 立即拉起下一个 item，直到 items 跑完。
3. **进度汇报**：WS 广播 `batch_progress` 事件，含 `completed/total/failed/pending_ids` ——前端可实时显示进度条和"还剩 N 张"。
4. **失败处理**：
   - 单个 item 失败 → 记入 `failed_items: [{index, error}]`，不影响其他 item
   - 整体超时 → 已完成的保留结果，未完成的标 `failed` 并给 `batch_timeout` 原因
   - `fail_fast=true` → 任一失败立刻 abort，所有飞行中的 sub-agent cancel
5. **取消传播**：主 agent interrupt → executor 停止派发新 item + 取消所有飞行中的 sub-agent（复用 `InterruptController`）
6. **结果聚合**：返回 `BatchResult { results: Vec<ItemResult>, summary: { total, completed, failed, elapsed_secs } }`，主 agent 拿到结构化结果直接做汇总分析

**与 L1 的关系**：

- L2 = 聚合器 + 模板化 + 进度 + 取消，**每个 item 仍走 SubAgentOrchestrator.execute()**（一次 L1 调用）
- L2 不重新实现 sub-agent 沙箱、context 隔离、attachment 加载——全部继承 L1
- L1 schema 改动对 L2 完全无感（L2 用 `SubAgentRequest` 直接构造，绕过 schema）

**实施成本估算**：

| 子任务 | LOC 估 | 风险 |
|---|---|---|
| `crates/hermix-tools/src/tools/batch.rs` (新工具 + schema) | ~200 | 低 |
| `crates/hermix-agent/src/batch_executor.rs` (并发调度 + 进度 + 取消) | ~400 | 中（要小心 Send recursion，与 L1 同样的 boxing 套路） |
| `crates/hermix-server/src/lib.rs` (WS `batch_progress` 事件) | ~50 | 低 |
| `apps/desktop-tauri/src/components/BatchProgress.tsx` (UI) | ~150 | 低 |
| 测试 (Rust 单测 + 集成测试 + 一个端到端 e2e 跑 50 张图) | ~300 | 中 |
| **合计** | **~1100 LOC** | — |

**预计工作量**：1–2 天（agent_executor.rs 是最复杂的一块）。

### 7.6 L3 概念：`pipeline_run` (远期)

**动机**：10000 张图 = 数据流问题，应完全脱离 LLM 编排。LLM 只设计"processor"（"调 OCR 库抽车牌"），数据流走文件队列。

**形态（仅概念）**：

```json
{
  "tool": "pipeline_run",
  "params": {
    "stages": [
      {"name": "discover", "kind": "glob", "pattern": "D:\\imgs\\*.jpg"},
      {"name": "ocr",      "kind": "processor", "exec": "python ocr.py --input {input} --output {output}"},
      {"name": "summarize","kind": "llm",      "model": "gpt-4o-mini", "prompt_template": "总结这些 OCR 结果:\n{input}"}
    ],
    "concurrency": 16,
    "checkpoint_dir": "D:\\pipelines\\run-2026-07-23\\"
  }
}
```

**与 L2 区别**：

- L2 = LLM-driven（每个 item 仍调 sub-agent LLM）
- L3 = Dataflow-driven（中间 stage 可以不调 LLM，纯本地 OCR / 视觉模型 / 数据库查询）
- L3 适合"100% 任务可机械化"的场景（OCR、分类、嵌入生成）；不适合"每张图都需要 LLM 推理"的场景

**实施门槛**：高。需要独立的 `hermix-pipeline` crate（stage DSL、checkpoint、resumability、worker pool），是 1–2 周工作量。**当前不进规划**，等 L2 落地后再决定。

### 7.7 实施路线

```
L1 (2026-07-23)  ─── schema 暴露 + 并发 fan-out ──→ 已上线 ✅
       │
       ▼
L2 (V2.1)  ── Rust executor + 模板 + 进度 + 取消 ──→ 1–2 天，待拍板启动
       │
       ▼
L3 (V3.x)  ── pipeline DSL + checkpoint ──→ 概念阶段，不进当前规划
```

**给拍板的 3 个问题**：

1. L1 schema 的"≤ 10 attachments per call"软上限，**是否要在 tool_executor.rs 加硬警告日志**（不拒绝，只 `tracing::warn!` 提示可能溢出）？
2. L2 `delegate_batch` 是否进下一阶段（1–2 天实现）？
3. L3 是否进规划（明确"未来再做"，避免半途开新坑）？

### 7.8 教训（固化进 SOUL/AGENTS）

- **LLM 编排"模式"，Rust 编排"数据"** —— 永远不要让 LLM 在 prompt 里"循环发 N 次 tool_call"做大批量。
- **Attachments 走路径引用，不走 base64 内联** —— 10000 张图只需要 `glob` pattern，token 成本固定。
- **任何"批量处理"先想 L1/L2/L3** —— 不要直接动手写代码，先判断档位。
- **Sub-agent 必带 HARD OVERRIDE prompt** —— 任何"agent-of-agent"架构都要主动阻断递归 delegation。
- **schema 里讲清容量边界** —— 让 LLM 在调用前就知道上限，不要靠它自己试错发现。

---

## 8. 文档版本

- v1-v5: 已废弃,基于估算的事实
- **v6: 2026-06-19 12:30**
  - 全部 17 项改进基于 `ARCHITECTURE_BENCHMARK.md` v6 实测数据
  - 12 个决策点待拍板
  - 5 个风险已登记
- **v6.1: 2026-07-09**
  - 新增 §6: Agent Loop 核心引擎修复进度
  - AL-1 ~ AL-5 已完成（含根因分析和修复验证）
  - 7 个待实施项按优先级排列
- **v6.2: 2026-07-23**
  - 新增 §7: 批量处理架构 L1/L2/L3 三档（外部挑战 "100/1000/10000 张图" 衍生）
  - 第一性原则论证 + 反模式 + L2 delegate_batch API 草案 + L3 pipeline 概念
  - L1 (`delegate_task`) schema 暴露 attachments/output_schema 并明确容量边界
  - 待拍板：L2 是否进 V2.1 实施 / L3 是否进规划
