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

## 3. 12 个决策点(主人拍板)

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

## 6. 文档版本

- v1-v5: 已废弃,基于估算的事实
- **v6: 2026-06-19 12:30(本版)**
  - 全部 17 项改进基于 `ARCHITECTURE_BENCHMARK.md` v6 实测数据
  - 12 个决策点待主人拍板
  - 5 个风险已登记
