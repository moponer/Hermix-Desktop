# Hermix 后端架构迁移状态 (v6)

Archived: 2026-09-30

> **生成时间**: 2026-06-19（原始 8 阶段计划 / 5.5 周）
> **最后更新**: 2026-07-03 08:35（同步到实际状态）
> **配套文档**: `ARCHITECTURE_BENCHMARK.md` v6 + `ARCHITECTURE_REFACTOR_PLAN.md` v6
> **本份性质**: 阶段表 + 真实状态对照，不是新计划

---

## TL;DR — 2026-07-03 现状

| 阶段 | 原计划 | 真实状态 | 决策 |
|---|---|---|---|
| 0 | 启动准备 / 12 决策点 | D7 inventory 已落地 / D3 D11 已天然满足；其余未拍板 | 不需要拍板 |
| 1 | SSRF DNS 验证 | ✅ `hermix-gateway/src/ssrf.rs::lookup_host` 已用 | 完成 |
| 2 | OTLP telemetry | ⚠️ `hermix-telemetry/src/otlp.rs` 已实现 + `tracing-subscriber` 已用，但 OTLP 是 feature-gated（`--features otlp`） | 完成（默认走 stdout JSON，OTLP 按需启用） |
| 3 | Sidecar watchdog | ✅ `HermixSidecar::start_watchdog` + `apps/desktop-tauri/src-tauri/src/sidecar.rs` | 完成 |
| 4 | 跨进程 Bus（WS / HTTP） | ❌ `hermix-bus` crate 不存在 | **延期：scope 过大不在本次范围** |
| 5 | groupchat 合并 | ⚠️ 数据层已转 `hermix_gateway::groupchat::*`；HTTP handler 仍在 server 端 3 文件 | 完成（数据统一，handler 不强删） |
| 6 | agent_loop 拆分（9178 → 4000 行） | ❌ 9500 行未动 | **延期：scope 过大不在本次范围** |
| 7.1 | inventory 完整化（90% 注册瘦身） | ⚠️ `inventory` crate 已用（`Cargo.toml` workspace dep），`register_builtins.rs` 已从 1378 行 → 1100 行（P1.4 三步 pipeline） | 部分完成（不补完剩 10%） |
| 7.3 | fd_lock 替换 file_lock.rs | ❌ **不需要**：实际项目用 `fs4 = "0.x"`（workspace dep），跨平台编译通过，无 bug 报告 | **取消**（原建议本身 stale） |
| 7.4 | Benchmark trait 抽象 | ✅ 3 个 bench (swe/terminal/yc) 已 `impl HermesBaseEnv`（`hermix-environments/src/training/mod.rs`） | 完成（trait 名是 `HermesBaseEnv` 而非 `Benchmark`，文档命名不准） |
| 8 | 验证 + 发布 | ⚠️ 全 workspace 编译/测试全过；v6 文档未更新 | 部分完成（docs v7 未更新） |

---

## 已落地详情

### §1 SSRF DNS rebinding — ✅ 完成

- `crates/hermix-gateway/src/ssrf.rs` 用 `tokio::net::lookup_host`
- 异步解析 + IP 黑名单检查
- 测试：`crates/hermix-gateway/tests/ssrf_dns_rebinding.rs`（如需可补 `evil.com → 127.0.0.1` 单测）

### §2 OTLP telemetry — ✅ feature-gated

- `crates/hermix-telemetry/src/otlp.rs` 实现 OTLP HTTP exporter
- `tracing-subscriber = "0.3"` + features `["env-filter", "json"]` 已用（`Cargo.toml` workspace dep）
- 默认走 stdout JSON（满足产品哲学"做好自己"，不上 OTel collector）
- OTLP 通过 `cargo build --features otlp` 启用（按需）

### §3 Sidecar watchdog — ✅ 完成

- `apps/desktop-tauri/src-tauri/src/sidecar.rs` 已实现
- `HermixSidecar::start_watchdog(WatchdogConfig::default())` 在 `lib.rs` 启动时调用
- 5s 心跳 + 3 次失败重启 + 前端事件通知（架构就位，集成测试补一下即可）

### §5 groupchat 合并 — ✅ 数据层统一

- `crates/hermix-server/src/dashboard/groupchat_api.rs` 已 `use hermix_gateway::groupchat::persistence::*`
- `crates/hermix-server/src/dashboard/groupchat_sqlite.rs` 已用 hermix-gateway 的 `GroupState` 数据模型
- `crates/hermix-server/src/dashboard/groupchat_dispatcher.rs` 仍作为 server 端 HTTP → gateway 转发层
- **不删除** 3 个 dashboard 文件原因：HTTP handler 物理上必须在 server（gateway 是 in-process crate，没起 HTTP server）

### §7.1 inventory 注册 — ⚠️ 部分完成

- `inventory = { workspace = true }` 已在 `hermix-tools/Cargo.toml`
- P1.8（2026-07-02，commit `55ccb5e`）：`register_builtins.rs` 90 行手写 YAML scanner → `serde_yaml` 38 行 schema
- P1.4（2026-07-02）：`register_builtins.rs` body 改三步 pipeline（browser backend / clarify backend / register_all + whitelist 后置 pass）
- **剩余**：从 1100 行再瘦身到 < 200 行 = 删 whitelist 后置 pass（前提是 Settings UI 接受"启动时一次注册"）

### §7.4 Benchmark trait — ✅ 完成（trait 名不同）

- `HermesBaseEnv` trait（`crates/hermix-environments/src/training/mod.rs`）已定义
- 3 个 bench 实现：`swe_bench::SweBenchEnv` / `terminal_bench::TerminalBenchEnv` / `yc_bench::YcBenchEnv`
- 命名"benchmark"在原计划里，但实际项目用"base env"语义更广（含未来 web research 等），保留现状不强行改名

---

## 延期 / 取消项（不重做）

### ❌ §4 跨进程 Bus（WS / HTTP）— 延期

**原因**：
- 当前架构是 `hermix-server` (axum) 在 Tauri sidecar 内 in-process 调 `hermix-gateway::groupchat::*`
- 跨进程只在 "Tauri GUI ↔ sidecar" 之间存在，通过 WS / HTTP（已实现）
- **真要补的是** `hermix-server ↔ hermix-gateway` 跨进程 Bus，但当前两者在同一进程，**没有真实需求**
- 1 周工作量 × 无 caller = 留半截子 = 删

**何时重做**：产品方明确要把 hermix-server 拆成独立进程（OS-level multi-instance）时再评估

### ❌ §6 agent_loop 拆分（9178 → 4000 行）— 延期

**原因**：
- 当前 9500 行（含 P1.x 系列新增注释）
- 已抽出的部分：`sub_agent_orchestrator.rs`（629 行已存在）、`archive.rs`（311 行，commit 短期上下文卸载 v2）
- **剩什么没拆**：`control.rs` (interrupt/steer/fallback 合并) + `cost.rs` (budget/rate_limit 合并)
- 2 周工作量 × 当前架构够用 = 留半截子 = 删

**何时重做**：agent_loop 编译时间 > 30s 或测试覆盖盲区暴露时再评估

### ❌ §7.3 fd_lock 替换 — 取消

**原计划说**：`hermix-cron/Cargo.toml` 加 `fd-lock = "0.1"` + 删 `file_lock.rs` (322 行)

**真实状态**：
- `hermix-cron/Cargo.toml` 已用 `fs4 = { workspace = true, features = ["sync"] }`
- `file_lock.rs` 跨平台编译过（Windows `LockFileEx` + Unix `flock`）
- 没有 bug 报告
- **换 crate 不会增加任何价值**

**结论**：MIGRATION_TODO.md 原建议 stale，删除该条目

---

## §8 验证 + 发布 — ⚠️ 部分完成

| 项 | 状态 |
|---|---|
| `cargo check --workspace --all-targets` | ✅ 0 error |
| `cargo test --workspace --lib` | ✅ 2224 passed / 0 failed / 2 ignored（2026-07-02） |
| `pnpm tsc --noEmit` | ✅ 0 errors |
| ARCHITECTURE_BENCHMARK.md → v7 | ❌ 没更新（等产品方确认 v7 命名 / 是否合并到本份） |
| CHANGELOG.md | ❌ 不存在（无 release 流程需要） |

**结论**：除文档 v7 升级外，技术验证全过

---

## 原则

- 本份不是新计划，是状态同步
- 延期 ≠ "TODO"，是"scope 评估后不做"
- 真有需求时产品方会触发；不主动开超出 scope 的活
- 任何"今天顺手能做的 P1"做完直接划掉

---

**文档维护者**: Senior Developer
**更新频率**: 每次任务完成后立即更新