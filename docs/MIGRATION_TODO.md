# Hermix 后端架构迁移 TODO (v6)

> **配套文档**: `ARCHITECTURE_BENCHMARK.md` v6 + `ARCHITECTURE_REFACTOR_PLAN.md` v6
> **生成时间**: 2026-06-19 12:30
> **总工期**: 8 阶段 / 约 5.5 周(单人全职)
> **原则**: 每阶段都有可验收产物;每阶段都可独立合并

---

## 阶段 0:启动准备(0.5 周)

**目的**:把 12 个决策点拍板,锁基线

### 任务 0.1 决策点拍板
- [ ] D1:跨进程 Bus 第一站 → **B**(server ↔ gateway)
- [ ] D2:SSRF DNS 检查 → **B**(异步)
- [ ] D3:agent_loop 拆分 → **A**(留 1 版本 deprecation)
- [ ] D4:Sidecar 重启 → **C**(5 秒后)
- [ ] D5:groupchat 合并 → **A**(deprecated server 端)
- [ ] D6:telemetry backend → **C**(stdout JSON)
- [ ] D7:tool 注册 → **A**(inventory)
- [ ] D8:文件锁 → **A**(fd-lock)
- [ ] D9:parity 目标 → **B**(50 场景)
- [ ] D10:--read-only → **A**(加)
- [ ] D11:lint → **A**(allow)
- [ ] D12:命名 → **A**(维持 hermix-*)

### 任务 0.2 锁基线
- [ ] git tag `arch-v6-baseline`
- [ ] CI 全绿(workspace `cargo build --workspace --all-targets`)
- [ ] 所有现有集成测试通过

### 任务 0.3 创建追踪 issue
- [ ] GitHub Project:`arch-v6-migration`
- [ ] 17 项改进各开 issue,标签 `arch-v6/P0`、`arch-v6/P1`、`arch-v6/P2`

**验收**: 12 个决策点全部有主人口头/文字确认;git tag 已打;17 个 issue 已建

---

## 阶段 1:P0-2 SSRF DNS 验证(0.5 周)

**目的**:堵住 hermix-gateway/src/ssrf.rs 的 DNS rebinding 漏洞

### 任务 1.1 DNS 解析 hook
- [ ] 新增 `crates/hermix-core/src/dns_resolve.rs`(异步,基于 `tokio::net::lookup_host`)
- [ ] 提供 `ResolvedAddr` struct + `resolve_and_check(url) -> Result<IpAddr>` API

### 任务 1.2 接入 SSRF 防护
- [ ] 修改 `hermix-gateway/src/ssrf.rs`:在 IP 黑名单检查前先做 DNS 解析
- [ ] 修改 `hermix-tools/src/tools/url_safety.rs`(2,024 行):接入新 DNS hook
- [ ] 修改 `hermix-tools/src/backends/web.rs`(358 行):接入
- [ ] 修改 `hermix-tools/src/backends/browser.rs`(1,407 行):接入

### 任务 1.3 测试
- [ ] 新增 `crates/hermix-gateway/tests/ssrf_dns_rebinding.rs`
  - 测试 `evil.com → 127.0.0.1` 被拦
  - 测试 `evil.com → 169.254.169.254` 被拦
- [ ] 现有 SSRF 测试不破坏

**验收**: 所有 SSRF 集成测试通过;`cargo bench` 延迟增加 < 5ms

---

## 阶段 2:P0-6 telemetry OTLP 基础(0.5 周)

**目的**:给所有 P0 改进铺可观测性基础

### 任务 2.1 引入 opentelemetry
- [ ] workspace.dependencies 加 `opentelemetry = "0.24"` + `opentelemetry-otlp = "0.17"` + `tracing-opentelemetry = "0.25"`
- [ ] `hermix-telemetry` 实现 `init_tracing()` 入口,stdout JSON exporter

### 任务 2.2 关键路径打 span
- [ ] `hermix-runtime/src/lib.rs` TickLoop 主循环加 span `agent.tick`
- [ ] `hermix-agent/src/agent_loop.rs` 加 span `agent.iteration`、`agent.tool_dispatch`
- [ ] `hermix-gateway/src/gateway.rs` 加 span `gateway.message`、`gateway.platform.{name}`

### 任务 2.3 测试
- [ ] `cargo test -p hermix-telemetry` 全过
- [ ] 运行 desktop-tauri 一次,确认 stdout 有 trace JSON

**验收**: 任何 agent loop tick 在 stdout 都有 1 条 JSON trace;包含 `tick_id`、`duration_ms`、`tools_called`

---

## 阶段 3:P0-4 Sidecar watchdog(0.3 周)

**目的**:Sidecar 崩溃后 Tauri 主进程 5 秒内重启

### 任务 3.1 心跳机制
- [ ] `apps/desktop-tauri/src-tauri/src/sidecar.rs` 加 5 秒心跳 token
- [ ] Sidecar 端 (hermix-server) 暴露 `/healthz` 返回 200

### 任务 3.2 重启策略
- [ ] 3 次心跳失败 → 等 5 秒 → 重启 Sidecar
- [ ] 重启失败 → emit `sidecar:restart_failed` 事件给前端
- [ ] 前端 `src/` 监听事件,弹非阻塞通知

### 任务 3.3 测试
- [ ] `apps/desktop-tauri/src-tauri/tests/sidecar_crash_probe.rs` 加心跳失败场景
- [ ] `apps/desktop-tauri/src-tauri/tests/sidecar_orphan_pid_recycle.rs` 加 PID 回收

**验收**: 测试脚本杀 Sidecar,5 秒内自动重启,前端通知正确

---

## 阶段 4:P0-1 跨进程 Bus(1.0 周)

**目的**:补 BusTransport 的 WS / HTTP 实现

### 任务 4.1 WS 实现
- [ ] 新增 `crates/hermix-bus/src/ws_transport.rs`
- [ ] 实现 `BusTransport` trait,使用 `tokio-tungstenite`
- [ ] 服务端模式 + 客户端模式
- [ ] 鉴权:静态 token(从 hermix-config 读)

### 任务 4.2 HTTP 实现
- [ ] 新增 `crates/hermix-bus/src/http_transport.rs`
- [ ] POST `/bus/send` + SSE `/bus/subscribe`
- [ ] 长连接 / 重连 / 退避

### 任务 4.3 集成到 hermix-server ↔ hermix-gateway
- [ ] `hermix-server` 启动时,如果 `gateway.transport = ws`,创建 WS server
- [ ] `hermix-gateway` 启动时,作为 WS client 连接 server
- [ ] 所有现有 `BusMessage` 都走 WS

### 任务 4.4 测试
- [ ] `crates/hermix-bus/tests/ws_transport_roundtrip.rs`
- [ ] `crates/hermix-bus/tests/http_transport_roundtrip.rs`
- [ ] 集成测试:server 关闭时 gateway 缓存消息,server 重启后投递

**验收**: hermix-server ↔ hermix-gateway 跨进程通信走 WS Bus;消息丢失率 0

---

## 阶段 5:P0-5 groupchat 合并(1.0 周)

**目的**:消除 hermix-server 和 hermix-gateway 的两套 groupchat 实现

### 任务 5.1 标记 deprecated
- [ ] `crates/hermix-server/src/dashboard/groupchat_api.rs`(434 行)加 `#[deprecated]`
- [ ] `crates/hermix-server/src/dashboard/groupchat_sqlite.rs`(379 行)加 `#[deprecated]`
- [ ] `crates/hermix-server/src/dashboard/groupchat_dispatcher.rs`(59 行)加 `#[deprecated]`
- [ ] 文档说明:用 `hermix-gateway::groupchat::*` 替代

### 任务 5.2 Dashboard 路由改造
- [ ] `crates/hermix-server/src/lib.rs` 把 groupchat HTTP handler 改为转发到 hermix-gateway
- [ ] 数据模型统一为 `hermix-gateway::groupchat::GroupState`

### 任务 5.3 删除
- [ ] 下个版本删除 deprecated 文件

### 任务 5.4 测试
- [ ] 现有 dashboard groupchat 集成测试全过
- [ ] 加测试:server → gateway 转发延迟 < 50ms

**验收**: dashboard 上 groupchat 功能行为不变;server 端 3 个 deprecated 文件已标

---

## 阶段 6:P0-3 agent_loop 拆分(2.0 周)

**目的**:把 9,178 行 agent_loop.rs 拆到 ≤ 4,000 行

### 任务 6.1 抽 Sub-Agent
- [ ] `hermix-agent/src/sub_agent_orchestrator.rs`(629 行,已存在)接管 agent_loop 中的 Sub-Agent 调用
- [ ] agent_loop 只保留 `sub_agent_orchestrator::spawn()` 调用点
- [ ] 集成测试:Sub-Agent 行为不变

### 任务 6.2 抽控制平面
- [ ] 新增 `crates/hermix-agent/src/control.rs`
- [ ] 把 `interrupt.rs`(181 行)+ `steer.rs`(86 行)+ `fallback.rs`(102 行)合并
- [ ] agent_loop 通过 `control::process(&mut state, event)` 调用

### 任务 6.3 抽成本控制
- [ ] 新增 `crates/hermix-agent/src/cost.rs`
- [ ] 把 `budget.rs`(102 行)+ `rate_limit.rs`(285 行)合并
- [ ] agent_loop 通过 `cost::check_and_increment()` 调用

### 任务 6.4 验证
- [ ] agent_loop.rs ≤ 4,000 行
- [ ] 所有现有集成测试通过
- [ ] `cargo bench -p hermix-agent` 性能退化 < 5%

**验收**: agent_loop ≤ 4,000 行;集成测试全过;bench 退化 < 5%

---

## 阶段 7:P1 改进批量(0.5 周)

**目的**:6 个 P1 改进集中处理

### 任务 7.1 inventory 注册
- [ ] 引入 `inventory` crate
- [ ] `hermix-tools/src/tools/*` + `backends/*` 全部用 `inventory::submit!`
- [ ] 删除 `hermix-tools/src/register_builtins.rs`(1,378 行)的 90% 手动注册

### 任务 7.2 模块文档
- [ ] `hermix-intelligence/src/auxiliary/mod.rs` 加 `//! # Module Overview`
- [ ] `hermix-intelligence/src/session_insights/mod.rs` 加 `//! # Module Overview`
- [ ] `hermix-intelligence/src/models_dev/mod.rs` 加 `//! # Module Overview`

### 任务 7.3 fd-lock 替换
- [ ] `hermix-cron` Cargo.toml 加 `fd-lock = "0.1"`
- [ ] 删除 `hermix-cron/src/file_lock.rs`(322 行),改用 `fd_lock::RwLock`

### 任务 7.4 Benchmark trait
- [ ] `hermix-environments/src/training/benchmarks/mod.rs` 定义 `Benchmark` trait
- [ ] swe_bench / terminal_bench / yc_bench 改为 trait impl

**验收**: hermix-tools 编译后不依赖 register_builtins.rs 的 1,378 行;hermix-cron 跨平台编译过

---

## 阶段 8:验证 + 发布(0.2 周)

**目的**:全链路验证,准备发布

### 任务 8.1 全量回归
- [ ] `cargo build --workspace --all-targets` 通过
- [ ] `cargo test --workspace` 全过
- [ ] `cargo clippy --workspace` 无 error

### 任务 8.2 性能基准
- [ ] `cargo bench --workspace` 记录基线
- [ ] 与阶段 0 基线对比,关键路径退化 < 5%

### 任务 8.3 文档
- [ ] `ARCHITECTURE_BENCHMARK.md` 更新到 v7(反映 P0 改进后的现状)
- [ ] `ARCHITECTURE_REFACTOR_PLAN.md` 标记已完成项
- [ ] `MIGRATION_TODO.md` 本文档标记全部完成

### 任务 8.4 Release
- [ ] git tag `arch-v6-complete`
- [ ] CHANGELOG.md 更新
- [ ] 通知主

**验收**: 全 workspace 编译 + 测试 + clippy 全过;性能退化 < 5%;文档同步到 v7

---

## 附录 A:每日节奏建议

| 时段 | 工作 |
|------|------|
| 上午 9:00-12:00 | 编码 + 单测 |
| 下午 13:00-17:00 | 编码 + 集成测试 |
| 17:00-17:30 | 当日总结 + 更新 ARCHITECTURE_BENCHMARK.md |
| 17:30-18:00 | 邮件 + 下日计划 |

## 附录 B:风险触发条件(立即升级)

- 任何 P0 改动导致集成测试失败 > 1 小时无法定位 → 暂停该 P0,先打 patch tag
- 任何阶段超过预算 50% → 重新评估决策点
- 任何性能退化 > 10% → 立刻回滚该 PR

## 附录 C:与 v5 优化清单的差异

| v5 优化清单 | v6 TODO | 说明 |
|------------|--------|------|
| 14 条优化项 | 17 项 P0/P1/P2 | v6 多了 3 项 SSRF / Sidecar / groupchat |
| 3 阶段 | 8 阶段 | v6 细化到 0.3-2 周的小步 |
| 无决策点 | 12 决策点 | v6 把决策前置 |
| 无基线 tag | git tag `arch-v6-baseline` | v6 强调"可回滚" |
| 估算法 | 全部实测 wc -l | v6 不再估算 |
