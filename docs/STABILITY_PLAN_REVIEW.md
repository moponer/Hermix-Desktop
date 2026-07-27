# 稳定性计划实现审查报告

> 审查时间：2026-07-25 19:31
> 审查对象：working tree 中按 `docs/STABILITY_ENGINEERING_PLAN.md` 落地的 P0/P1（部分 P2）改动（未提交）
> 审查方法：Explore sub-agent very thorough 审查 + 主 agent Grep 三角验证（`deadpool/r2d2` 依赖 / `HANDLE_WS_STREAM_SEND_ENTRY` 残留 / `catch_unwind` 用法）

---

## 结论速览

主人把计划里大部分 P0/P1 改对了，**但 P0 止血核心——SQLite 连接池完全没引入**，且 async 热路径仍有同步 SQLite 阻塞（Bug #3 同类风险）未根治。另有 `read_file` 路径校验过软、`eprintln` 诊断遗漏未清。

---

## ✅ 已正确落地（可放心）

| 计划项 | 证据 | 说明 |
|---|---|---|
| P0-4 cdp.rs 锁重构 | `cdp.rs:238-312` | reader_loop 改 `cmd_tx/cmd_rx` channel 推送，ws 由循环独占，锁不跨 `.await` |
| P0-5 session.rs 嵌套锁 | `session.rs:459-473` | remove_session 先释放 `sessions` 写锁再取 `user_sessions`，无嵌套锁 |
| P0-7 weixin 生命周期 | `weixin.rs:1267-1280` | stop() 执行 `abort()` + `timeout(2s).await` 真 join；`inbound_tx=None` 清 Option |
| P1-1/P1-2 panic 边界 | `lib.rs:5136-5141` | 用 `FutureExt::catch_unwind`（async 安全），**非** `std::panic::catch_unwind`，避开了 async 陷阱 |
| P1-3 unreachable 改 Result | `skills.rs:684` 起 | 6 处 `unreachable!` 已转 Result（仅剩 `#[tokio::test]` 内 1 处 `panic!`，非生产） |
| P1-5 LLM 超时 | `provider.rs:39-41` | 强制 `connect_timeout(10s)` + `timeout(180s)` |
| P2-4 crash dump | `crash_dump.rs` 新模块 | panic 后写结构化 JSON dump（含 backtrace + session id），超出计划预期 |
| P2-5 sandbox canary | `code_execution.rs:337-446` | canary plant/verify/remove 完整 |
| spawn_blocking 用法 | 多处 | 闭包内均为同步 fs/sqlite，无 `.await`（合法） |

**亮点**：主人实现 P1-1 时用了 `FutureExt::catch_unwind`，比计划文档 §2.3/P1-1 写的"包 `std::panic::catch_unwind`"更准确——这是计划文档自身的错误（见下）。

---

## ⚠️ 必须修复（3 项，P0 级）

### 1. P0-3 SQLite 连接池完全没引入 + async 热路径仍阻塞
- **证据**：`git diff` 各 `Cargo.toml` 无任何 `deadpool/r2d2/sqlx`（Grep 零匹配）；生产代码仍直接 `rusqlite::Connection::open`（`lib.rs:2790/2924/6829/6878`、`metrics.rs:182/206`、`status.rs:64`、`analytics.rs:201`）。
- **更严重**：`lib.rs:5553/6492/3656/7028` 的 **async 审批 handler 内联**调用 `lookup_pending_command` / `lookup_pending_resource`，其内部 `lib.rs:6829/6878` 直接 `Connection::open` 且**未包 `spawn_blocking`** → 阻塞 tokio async worker → 这正是 Bug #3 同类风险（同步 I/O 卡住 event loop，WS heartbeat 15s 超时 → 前端 close）。
- **计划 P0-3 自相矛盾**：计划要求"HTTP handler 内同步 SQLite 查询全部走 pool + spawn_blocking"，但审批 handler 这条漏了。
- **修复**：最短路径是先把 `lookup_pending_*` 及其内部 `Connection::open` 包进 `spawn_blocking`（不必等 pool 引入即可止血）；中长期补 pool。

### 2. P1-8 read_file 路径校验过软
- **证据**：`file.rs:181-189` 仅用 `CredentialGuard::check_read_access` + `check_path_traversal_advisory`（注释明说 *"logs but does not block"*）；`file.rs:130-146` **明确不 canonicalize**。
- **风险**：`..` 段 + 非规范路径可能绕过 workspace 边界检查，读到沙箱外文件。
- **计划要求**：hard canonicalize + 校验在 workspace 内。实际是 advisory 软检查，与计划偏离。
- **修复**：`read_file`/`write_file` 入口统一 `fs::canonicalize` 后校验 `path.starts_with(workspace_root)`，不合规直接拒绝。

### 3. P1-10 eprintln 诊断遗漏
- **证据**：`lib.rs:3997` 残留裸 `eprintln!("!!!DEDUP CALLED!!!")`。
- **说明**：P1-10 只删了 `HANDLE_WS_STREAM_SEND_ENTRY`，这条生产诊断忘记清理。应改 `tracing::debug!` + 配置开关。

---

## ❌ 遗漏（2 项）

| # | 项 | 证据 | 影响 |
|---|---|---|---|
| 1 | `metrics.rs` 新文件仍同步 `Connection::open` | `metrics.rs:182/206` | 若被 async 路径调用同样阻塞 event loop |
| 2 | weixin start() 重入泄漏 | `weixin.rs:1250` | 重复调用 `start()` 覆盖 `poll_task` JoinHandle 而不 `abort` 旧 task → 旧 task 泄漏（P0-7 未覆盖 restart 场景） |

---

## 📄 计划文档本身的 2 处错误

1. **§2.3 / P1-1 / F3 "包 `std::panic::catch_unwind`" 有 async 陷阱**
   - 错误：`std::panic::catch_unwind` 不能可靠跨 `.await` 捕获 panic（future 被 drop 时 panic 直接 propagate 到 task 边界）。
   - 正确写法（主人已实现）：用 `FutureExt::catch_unwind` 包 async block，或 `tokio::spawn` 后 `JoinHandle::await` 捕获 `JoinError`。
   - 计划文档应修正，避免后续执行者踩坑。

2. **§6 "不做性能优化" 与主人原始需求矛盾**
   - 原始需求明确要求"稳定性、**性能**、以及其他安全"全面审查。
   - 计划把性能整体排除（"性能优化另立计划"），8 维度风险地图里没有独立性能维度（§2.2 只顺带提了同步 I/O 阻塞，那是稳定性不是性能）。
   - 建议补一个"性能维度"（延迟/吞吐/内存增长/上下文压缩管线/序列化开销/高并发 tool 调用背压）。

---

## 建议优先级

| 优先级 | 动作 | 工作量 |
|---|---|---|
| **P0** | 修 `lib.rs` async 审批 handler 内 `lookup_pending_*` 包 `spawn_blocking`（直接止血 Bug #3 同类） | 小（~20 行） |
| **P0** | `read_file`/`write_file` 加 hard canonicalize + workspace 边界 | 小（~30 行） |
| **P1** | 引入 SQLite 连接池（根治 P0-3 止血核心） | 中（pool 依赖 + 替换 15+ 处 open） |
| **P1** | 清 `eprintln` 残留 + 修 weixin restart 重入 | 小 |
| **P1** | 修计划文档 2 处错误 + 补性能维度 | 文档 |
| **P2** | `metrics.rs` 同步查询包 spawn_blocking | 小 |

---

**下一步**：要我直接修 ⚠️ 三项（async 阻塞 + read_file 硬边界 + eprintln）吗？其中 #1 是 P0 止血，不必等 pool 即可先落地。
