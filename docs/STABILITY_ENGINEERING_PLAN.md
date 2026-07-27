# Hermix Desktop 后端稳定工程优化计划

> 起草时间：2026-07-25
> 范围：`crates/` 全 18+ Rust crate + `apps/desktop-tauri/src-tauri/`
> 制定原则：第一性原则推导 + 对抗式审查（每个维度必须回答"在什么场景下会崩"）
> 不在范围内：跨平台移植、外部攻击面（DDoS / 鉴权绕过等网络安全）

---

## 0. 计划目标与 SLO

### 0.1 计划目标

把后端从"能跑"提升到"在异常输入、长会话、高并发 tool 调用、外部资源抖动下仍可预测地降级或恢复"。

### 0.2 SLO 定义（验收基线）

| 指标 | 当前基线 | 目标 |
|---|---|---|
| 单 session 连续运行不重启 | 未测 | ≥ 8 小时 soak test 无 panic / 无 fd 泄漏 / 内存增长 ≤ 200MB |
| WS 客户端断开后服务端 task 残留 | 未测 | 0（断开后 5s 内所有相关 task abort、所有锁释放） |
| 单 turn p99 延迟（无 LLM 抖动） | 未测 | ≤ 500ms（不含 LLM/工具自身耗时） |
| 同步 I/O 阻塞 async runtime 事件 | 已知存在（Bug #3） | 0（grep 可验证：async fn 内禁止裸 `rusqlite::Connection::open` / `std::fs::*`） |
| 生产路径 panic | 未测 | 0（CI gate 禁止 `unwrap`/`expect`/`panic!` 进入非测试代码新增） |
| 锁跨 `.await` 持有 | 已知存在（cdp.rs） | 0（CI gate 静态扫描） |
| 工具调用 timeout 覆盖率 | 60% | 100%（每个外部交互必须显式 timeout + 退避策略） |

---

## 1. 现状基线

### 1.1 测试基线（2026-07-22）

- `hermix-agent --lib`: 999 passed
- `hermix-tools --lib`: 618 passed
- `hermix-focus --lib`: 181 passed
- 前端 `tsc --noEmit`: 0 errors
- 总 ~1,834 unit tests，但 **0 soak test、0 chaos test、0 fault-injection test**

### 1.2 已知教训汇总（必须代码化）

| 教训 | 来源 | 根因 | 是否已代码化 |
|---|---|---|---|
| finalize 路径同步 I/O 阻塞 WS heartbeat → 前端 close | 2026-07-22 Bug #3 | `create_compression_child_session` 内 N×autocommit `INSERT` 无事务包装，3-6s 阻塞 > 15s heartbeat | 否（仅 memory） |
| ImageUrl base64 不清理 → ctx 永久膨胀 | 2026-07-22 Bug #4 | `compression.rs` Pass 4 只清 Tool role，跳过 User role 的 `content_parts: [Text, ImageUrl]` | 否（仅 memory） |
| `ApprovalRegistry::resolve()` 幂等，二次 WS 触发 `matched=false` 日志噪音 | commit 39e5b29 | 前端双保险重复 send | 是（App.tsx:1201 已删） |
| WS dedup key = `(session_id, content_hash)` | 设计文档 | — | 是 |
| `handle_ws_stream_send` 永久诊断 `!!!HANDLE_WS_STREAM_SEND_ENTRY!!!` | lib.rs:3611 | 排查 WS 静默丢消息 | 是（保留） |
| read path 不得套 streaming state machine | 设计文档 | — | 是 |

### 1.3 红黄绿灯地图（9 面盘点结果）

| 面 | 灯 | 关键证据 |
|---|---|---|
| 锁使用 | 🔴 | `cdp.rs:212-216` reader_loop 跨 `.await` 持 ws 锁；`session.rs:448-451` 嵌套锁（sessions.write + user_sessions.write） |
| 同步 I/O | 🔴 | `session_persistence.rs` 整文件 rusqlite 直连 + `std::fs::*` 在 async；`server/lib.rs:2696` HTTP handler 内 `Connection::open` |
| panic 边界 | 🟡 | 全仓 0 `catch_unwind`；`lib.rs:7435` `unwrap_or_else(panic!)` rebind 失败即 panic 整个 server |
| 超时重试 | 🟡 | LLM 主调用无显式超时（仅 stale refresh 3s）；多数外部交互无重试 |
| 收敛控制 | 🟢 | `max_turns` + `turn_transition watchdog` + sub_agent 双重上限 |
| 子进程生命周期 | 🟡 | detached 设计明确但依赖 taskkill 兜底，pid 文件失效即僵尸 |
| WS 链路 | 🟢 | biased select + `ws_disconnected` 6 处置位 + soft-drain + graceful shutdown |
| 资源泄漏 | 🔴 | SQLite 0 pool（15+ 处裸 `Connection::open`）；`weixin.rs:1262` poll_task 未 join；`inbound_tx` clone 模式可能让 rx 永不关闭 |
| 风险标记 | 🟡 | 0 处 `TODO: pre-existing` 显式标记，历史教训未代码化 |

---

## 2. 风险地图（8 维度）

### 2.1 死锁与锁竞争 🔴 P0

**现状**：
- `crates/hermix-gateway/src/session.rs:448-451` `remove_session` 持 `sessions.write().await` 中再取 `user_sessions.write().await`——锁顺序倒置即死锁。
- `crates/hermix-cua/src/browser/cdp.rs:212-216` `reader_loop` 在 `ws.lock().await` 内调 `ws.next().await`，整个 read 期间锁住 ws，导致 `call()` (cdp.rs:137) 的 send 串行阻塞——典型反模式。
- `gateway.rs` 30+ 处 `.read().await` / `.write().await`，部分持锁调 handler。

**会崩场景**：
1. 用户断开 + 新会话注册并发 → session.rs 锁顺序倒置 → 两个 task 互相等 → agent loop 卡死 → WS heartbeat 超时 → 前端 close，但服务端 task 永不退出。
2. CDP 浏览器自动化时，reader_loop 持 ws 锁等下一个 frame，主流程 call() 发送 Page.navigate 被阻塞 ≥ 30s → browser response timeout → 假死。
3. gateway.rs 持 read 锁调 handler，handler 内部再取 write 锁 → 同 task 递归锁（RwLock 非可重入）→ panic / 死锁。

**治理动作**：
- 重构 session.rs：所有写操作改为"先 clone 需要的数据 → 释放锁 → 再操作"，禁止嵌套取锁。
- 重构 cdp.rs：reader_loop 改为不持锁，通过 channel 把 frame 推给消费端；call() 直接持锁发送。
- gateway.rs 持锁调 handler 全部改为"持锁取数据 → 释放锁 → 调 handler"。
- CI gate 加静态扫描：禁止 `MutexGuard` / `RwLockReadGuard` / `RwLockWriteGuard` 跨 `.await`（用 `clippy::await_holding_lock` lint，已内置）。

### 2.2 阻塞 I/O 与 async 反模式 🔴 P0

**现状**：
- `session_persistence.rs` 整文件 rusqlite 直连（line 34 `open_with_wal`、1124 `std::thread::sleep` jitter、1437/1495/1510/1528/1540/1549 大量 `std::fs::*`），且被 async agent_loop 调用——Bug #3 教训根源。
- `context.rs:660/685/701/768/772`、`context_files.rs:104/126/171`、`researcher_loop.rs:234/238/286/299/332/393/424`、`skill_loader.rs:239`、`skill_orchestrator.rs:358/380/395/467/550/602`、`subdirectory_hints.rs:175/252`、`plugins.rs:590/611/647/667`、`native_plugin.rs:373/388`、`oauth_refresh.rs:66`、`memory_prefetch.rs:319`：async 链路中的 `std::fs::*`，未走 `spawn_blocking`。
- `server/lib.rs:2696/6635/6684`、`dashboard/status.rs:60`、`groupchat_sqlite.rs:88`、`analytics.rs:201`：HTTP handler 内直接 `rusqlite::Connection::open` + 同步查询，无 pool、无 `spawn_blocking`。

**会崩场景**：
1. 长会话归档时 `create_compression_child_session` 同步 N×INSERT 阻塞 3-6s > WS heartbeat 15s → 前端 `was_client_disconnect=true` close（Bug #3 复现）。
2. 高并发 dashboard 请求同时 `Connection::open` → SQLite fd 抖动 + 文件锁竞争 → 请求 p99 飙升 → HTTP handler 队列堆积。
3. skill_loader 在 async 加载 skill 时 `std::fs::read_to_string` 大文件 → 阻塞 runtime worker → 其他无关 task 饥饿。

**治理动作**：
- 引入 SQLite 连接池（`r2d2-sqlite` 或 `deadpool-sqlite`），所有 `Connection::open` 改为 `pool.get()`。
- 所有 async fn 内的 `std::fs::*` 与 `rusqlite::*` 同步调用必须 `tokio::task::spawn_blocking` 包裹。
- session_persistence.rs 的 finalize 后置副作用（child session / 异步归档 / 上报）必须 `spawn_blocking` fire-and-forget，禁止 await（Bug #3 修复规则代码化）。
- CI gate 加 grep 检查：`async fn` 体内禁止裸 `rusqlite::Connection::open` / `std::fs::read` / `std::fs::write` / `std::thread::sleep`。

### 2.3 panic 与错误传播 🟡 P1

**现状**：
- 全仓库 0 `catch_unwind`——任何 task panic 直接终止 task，WS 客户端看到突然 close，无错误响应。
- `lib.rs:7435` `unwrap_or_else(panic!)` rebind 失败直接 panic 整个 server。
- `dashboard/skills.rs:743/761/785/793/800/806` 6 处 `unreachable!`——若 NoopSkillProvider 被错误调用即 panic。
- `unsafe` 6 处 `std::env::set_var`（Rust 2024 edition 已 unsafe）+ `element.rs:172` EnumChildWindows 回调 + `storage_clean.rs:219` SHFileOperationW + `dashboard/status.rs:37` libc::kill。
- `unwrap`/`expect` 密度 Top 5：`server/lib.rs` 130、`session_persistence.rs` 176、`agent_loop/mod.rs` 87、`tools/backends/memory.rs` 56、`experience_archive.rs` 55。

**会崩场景**：
1. 单个 tool handler panic → 整个 agent loop task 终止 → 用户看到 WS 突然 close，无任何错误信息。
2. `lib.rs:7435` rebind 失败 panic → 整个 server 进程退出 → 所有 session 全部丢失。
3. `dashboard/skills.rs` NoopSkillProvider 被路由错误调用 → panic → dashboard 服务不可用。
4. `unsafe` env::set_var 在多线程下被调用 → UB（Rust 2024 已 unsafe 化的原因）。

**治理动作**：
- agent loop 主 task 与每个 tool dispatch task 包 `std::panic::catch_unwind`，panic 时返回结构化错误给 WS 客户端而非 close。
- `lib.rs:7435` 改为返回 `Result`，rebind 失败时降级为拒绝新连接而非 panic。
- `dashboard/skills.rs` 6 处 `unreachable!` 改为返回 `Result`，NoopSkillProvider 不应被调用就返回错误。
- `unsafe` env::set_var 全部移到进程启动早期单线程阶段；若必须在运行时设，用 `unsafe` 块 + SAFETY 注释 + 单测验证。
- CI gate 加 lint：`#![deny(clippy::unwrap_used, clippy::expect_used)]` 在 `src/lib.rs` 顶层（仅生产代码，测试代码豁免）。

### 2.4 超时与重试矩阵 🟡 P1

**现状（盘点出的 22 个超时点）**：

| 交互 | 超时 | 重试 |
|---|---|---|
| WS heartbeat | 15s interval | 无 |
| tool_dispatch per_call | 60s | 无 |
| tool_dispatch total | 300s | 无 |
| turn_transition watchdog | 配置驱动 | 无 |
| stream_first_chunk | 30s | 无 |
| stream_overall | 180s | 无 |
| LLM HTTP stale refresh | 3s | 无 |
| LLM 主调用本身 | **无显式超时** | **无** |
| auxiliary task | 30-60s | 无 |
| email IMAP/SMTP | 30s read/write | 无 |
| weixin media download | 60s | reconnect 600s |
| groupchat_api | 90s | 无 |
| polish LLM | 12s | 无 |
| browser connect/navigate | 5s/15s | 无 |
| browser response | 30s | 无 |
| code_execution | timeout_secs 参数 | 无 |
| session_search | 60s | 指数退避 |
| mcp client | config.timeout_secs | 无 |
| hooks | self.timeout_secs | 无 |
| cdp wait_for_load | 15s | 无 |
| voice record | max_record_secs | 无 |
| sub_agent | DEFAULT_SUB_AGENT_TIMEOUT_SECS | 无 |

**会崩场景**：
1. LLM provider 主调用本身无超时——若 LLM 服务端 TCP 半开（accept 但永不响应），`stream_first_chunk` 30s 兜底，但 30s 内用户已经看到无响应；且 LLM provider 的 reqwest client 默认无 connect timeout，TCP 建连阶段可能挂 75s（Linux 默认）。
2. weixin reconnect 600s 退避期间，若用户重启 session，旧 reconnect task 仍持有 inbound_tx clone → rx 永不关闭 → 新 session 收不到消息。
3. 多数外部交互 0 重试——单次网络抖动即用户可见失败。

**治理动作**：
- LLM provider reqwest client 强制设 `connect_timeout(10s)` + `timeout(180s)`（与 stream_overall 对齐）。
- 所有外部交互（HTTP/IMAP/SMTP/MCP/CDP）必须 config-driven timeout + 指数退避（max 3 次，base 1s，jitter ±20%）。
- weixin reconnect task 必须在 session 销毁时 `task.abort()`，且 `inbound_tx` 写入前先 `Option::take()` 旧值 drop。
- 把超时矩阵文档化到 `docs/TIMEOUT_MATRIX.md`，每次新增外部交互必须更新。

### 2.5 资源生命周期与泄漏 🔴 P0

**现状**：
- SQLite 0 pool，每次请求 `Connection::open`（15+ 处）——fd 抖动 + 文件锁竞争。
- `weixin.rs:1262` `poll_task.write().take()` 后未 `await join`——task 泄漏。
- `weixin.rs:369`、`webhook.rs:96`、`dingtalk.rs:151`、`wecom_callback.rs:111` 的 `inbound_tx` clone 模式——旧 tx 未 None 化则 rx 永不关闭。
- `tempfile::TempDir` 持有到 `HostedAgentPool` 生命周期——pool 未 drop 则泄漏。
- `gateway.rs:1702/1721/1737` 三个 interval task 用 `Option<JoinHandle>` + shutdown_tx 控制——但 shutdown_tx drop 后 task 才退出，需验证 drop 顺序。

**会崩场景**：
1. 长会话（8h+）下 SQLite 反复 open/close → fd 短暂峰值 → 达 ulimit → 新 query 失败 → 用户看到随机错误。
2. 用户反复断开/重连 weixin session → 旧 poll_task 泄漏累积 → 内存增长 → 最终 OOM。
3. `inbound_tx` clone 未清 → rx 永不关闭 → message channel task 永挂 → 每 session 累积一个永挂 task。

**治理动作**：
- 引入 SQLite 连接池（与 2.2 共用），pool size = `num_cpus * 2`，max_lifetime = 30min，idle_timeout = 5min。
- 所有 `JoinHandle` 必须在销毁时 `abort()` + `await`（带 timeout），不允许 fire-and-forget。
- 所有 `Sender` clone 必须有显式 drop 路径（用 `Option<Sender>` + `take()` 模式）。
- CI gate 加 grep：禁止 `tokio::spawn` 后丢弃 `JoinHandle`（除非有 `// SAFETY: fire-and-forget justified by ...` 注释）。

### 2.6 连接稳定性与优雅降级 🟢 P1

**现状**：WS 链路相对健康——biased select + `ws_disconnected` 6 点置位 + soft-drain + graceful shutdown。但有改进空间。

**会崩场景**：
1. WS 静默丢消息（WebView2 known issue）——目前依赖 REST 兜底 + `matched=true` 监控，但无主动探测；若用户网络半开（TCP 不断但 packet 丢），heartbeat ping 不超时也收不到 pong。
2. graceful shutdown 2s deadline——若归档任务 > 2s，强制中断 → 数据丢失。
3. 半开连接：依赖 heartbeat ping 失败检测，但 ping 间隔 15s × N 次失败才标记 disconnected——用户感知延迟。

**治理动作**：
- 加 WS application-level ping（区别于 protocol ping）：客户端每 10s 发 `{"type":"ping"}`，服务端 `{"type":"pong"}`，连续 2 次未收到 pong 主动 close。
- graceful shutdown deadline 改为配置驱动（默认 5s），且归档任务 spawn_blocking fire-and-forget（不阻塞 shutdown）。
- 加 `ws_disconnected` 状态监控 metric（断开次数、断开原因分类）。

### 2.7 逻辑安全（非网络攻击面）🟡 P1

**现状**：未系统审查，但已知风险面：
- 工具调用路径校验：`read_file` / `write_file` / `execute_command` / `code_execution` 的路径穿越与命令注入。
- Approval 竞争：`ApprovalRegistry::resolve()` 已幂等，但 `tc.id` 作 registry key 的稳定性依赖 LLM 不重复发同 id。
- 配置注入：`AgentConfig` 新字段必须 serde default（已规则化），但未审查恶意构造的 config。
- 子进程命令注入：`commands_custom_app.rs:225/233/241` explorer/open/xdg-open spawn 后立即 drop，参数未审查。

**会崩场景**：
1. `read_file` 工具被 LLM 调用读 `../../../etc/passwd`（Windows 下 `..\..\Windows\System32\config\SAM`）——若未做 workspace 边界校验，敏感文件泄漏。
2. `execute_command` 被注入 shell 元字符（`; rm -rf` / `& del`）——若未做参数化，命令注入。
3. `code_execution` 的 sandbox 边界被绕过（local_isolation.rs 的 TempDir 路径）——写到 sandbox 外。
4. Approval `tc.id` 重复 → registry 冲突 → 工具被错误 resolve。

**治理动作**：
- 全量审查 tool backends 的路径校验：所有文件操作必须 canonicalize 后校验在 workspace 内（已规则化但需验证）。
- `execute_command` 改为参数化（`Command::arg` 而非 `Command::new(shell, -c, cmd_string)`），除非用户显式批准 shell。
- `code_execution` sandbox 边界加 canary 文件检测泄漏。
- Approval `tc.id` 重复时拒绝（不覆盖），返回错误让 LLM 重发。

### 2.8 可观测性与故障恢复 🟡 P2

**现状**：
- `tracing` 已用，但覆盖率未盘点；关键路径（agent loop 边界、tool dispatch、WS 状态机）有 debug 日志，但无 metric。
- `!!!HANDLE_WS_STREAM_SEND_ENTRY!!!` 是 eprintln 永久诊断——应该改 tracing + 可关闭。
- 0 Prometheus / 0 自建 metric / 0 crash dump。
- 0 soak test / 0 chaos test / 0 fault-injection test。

**会崩场景**：
1. 生产环境 WS 静默丢消息——目前只能靠 `matched=true` 日志事后 grep，无实时告警。
2. 内存泄漏 / fd 泄漏——无监控，发现时已 OOM。
3. panic 导致 task 终止——无 crash dump，无法事后定位。

**治理动作**：
- 引入轻量 metric（不依赖 Prometheus server，用本地 SQLite 落盘 + dashboard 可视化）：WS 断开次数、task 数、fd 数、内存、SQLite pool 占用、tool dispatch p99。
- `eprintln!("!!!HANDLE_WS_STREAM_SEND_ENTRY!!!")` 改为 `tracing::debug!(target: "ws_stream_send", "entry")` + 配置开关。
- 加 soak test：`cargo test --test soak -- --ignored`，8h 跑 synthetic session，监控内存/fd/task 数。
- 加 fault-injection test：mock LLM 503 / mock tool panic / mock WS 断开，验证 agent loop 恢复。
- panic 时写 crash dump 到 `hermix_home()/crash_dumps/YYYY-MM-DD-HHMMSS.json`（stack backtrace + session id + last 10 turns summary）。

---

## 3. 分阶段优化路线图

### Phase P0：立即治理（1-2 周，止血）

> 目标：消除已知红灯，杜绝 Bug #3 类复发。

| # | 动作 | 维度 | 验收标准 |
|---|---|---|---|
| P0-1 | 引入 SQLite 连接池（`deadpool-sqlite` 或 `r2d2-sqlite`），替换 15+ 处裸 `Connection::open` | 2.2 / 2.5 | grep `rusqlite::Connection::open` 在生产代码 0 处 |
| P0-2 | session_persistence.rs 所有 async 调用路径走 `spawn_blocking`；finalize 后置副作用 fire-and-forget | 2.2 | 单元测试：finalize 后 100ms 内 WS heartbeat 不阻塞 |
| P0-3 | server/lib.rs HTTP handler 内同步 SQLite 查询全部走 pool + `spawn_blocking` | 2.2 | dashboard 100 并发请求 p99 ≤ 200ms |
| P0-4 | 修复 cdp.rs reader_loop 跨 `.await` 持 ws 锁（改 channel 推送） | 2.1 | CDP 并发 call + read 无阻塞（benchmark） |
| P0-5 | 修复 session.rs remove_session 嵌套锁（先 clone 后释放） | 2.1 | 并发断开+注册 1000 次无死锁 |
| P0-6 | CI gate 加 `clippy::await_holding_lock` lint + grep 禁止 `async fn` 内裸 `std::fs::*` / `rusqlite::Connection::open` | 2.1 / 2.2 | CI 失败即阻断合并 |
| P0-7 | weixin poll_task / inbound_tx 生命周期修复（abort + take 旧值） | 2.5 | weixin session 反复断开/重连 100 次无 task 累积 |

### Phase P1：中期加固（2-4 周，韧性）

> 目标：建立错误恢复与超时矩阵，消除 panic 雪崩。

| # | 动作 | 维度 | 验收标准 |
|---|---|---|---|
| P1-1 | agent loop 主 task + tool dispatch task 包 `catch_unwind`，panic 时返回结构化错误 | 2.3 | 注入 panic 测试：WS 不 close，用户看到错误消息 |
| P1-2 | `lib.rs:7435` unwrap_or_else(panic!) 改为 Result 降级 | 2.3 | rebind 失败测试：server 不退出 |
| P1-3 | `dashboard/skills.rs` 6 处 unreachable! 改为 Result | 2.3 | NoopSkillProvider 被错误调用测试：返回错误不 panic |
| P1-4 | CI gate 加 `#![deny(clippy::unwrap_used, clippy::expect_used)]` 在生产 lib.rs 顶层 | 2.3 | 现有 unwrap 密度 Top 5 文件清零 |
| P1-5 | LLM provider reqwest client 强制 `connect_timeout(10s)` + `timeout(180s)` | 2.4 | mock LLM TCP 半开测试：10s 内 connect 失败 |
| P1-6 | 所有外部交互（HTTP/IMAP/SMTP/MCP/CDP）config-driven timeout + 指数退避 | 2.4 | 超时矩阵文档化，新增外部交互必须更新 |
| P1-7 | WS application-level ping/pong（10s 间隔，2 次未收到 pong 主动 close） | 2.6 | 半开连接测试：20s 内检测到 |
| P1-8 | tool backends 路径校验全量审查（workspace 边界） | 2.7 | `read_file("../../../etc/passwd")` 测试被拒绝 |
| P1-9 | `execute_command` 参数化（禁止 shell -c 字符串拼接） | 2.7 | 命令注入测试被拒绝 |
| P1-10 | `eprintln!("!!!HANDLE_WS_STREAM_SEND_ENTRY!!!")` 改 tracing + 配置开关 | 2.8 | 生产关闭后 0 输出 |

### Phase P2：长期保障（4-8 周，可观测)

> 目标：建立 SLO 度量与混沌测试，从"能跑"到"可预测"。

| # | 动作 | 维度 | 验收标准 |
|---|---|---|---|
| P2-1 | 本地 metric 系统（SQLite 落盘 + dashboard 可视化） | 2.8 | WS 断开次数 / task 数 / fd 数 / 内存 / pool 占用 / tool p99 实时可见 |
| P2-2 | soak test：8h synthetic session，监控内存/fd/task | 2.8 | 8h 后内存增长 ≤ 200MB，fd 不增长，task 数稳定 |
| P2-3 | fault-injection test：mock LLM 503 / tool panic / WS 断开 | 2.8 | 全部场景 agent loop 自动恢复或优雅降级 |
| P2-4 | crash dump 写 `hermix_home()/crash_dumps/` | 2.8 | panic 后 dump 文件存在，含 backtrace + session id + last 10 turns |
| P2-5 | `code_execution` sandbox canary 文件检测 | 2.7 | sandbox 泄漏测试被检测 |
| P2-6 | Approval `tc.id` 重复拒绝（不覆盖） | 2.7 | 重复 id 测试返回错误 |
| P2-7 | 历史教训代码化：每个教训加 `// LESSON: 2026-07-22 Bug #3 ...` 注释 + 单测 | 1.2 | grep `// LESSON:` 覆盖已知教训清单 |
| P2-8 | 子进程 detached 设计加 pid 文件 + 启动时清理孤儿 pid | 2.5 | 重启后无僵尸进程 |

---

## 4. 对抗式失败模式枚举（计划自检）

> 每条枚举一个本计划可能崩的场景，并给出应对。

| 失败模式 | 触发场景 | 应对 |
|---|---|---|
| F1: SQLite 连接池死锁 | pool 耗尽 + 持有 connection 的 task 又去取另一个 connection | pool 配 `wait_timeout(5s)`，超时返回错误而非阻塞；CI 加并发测试 |
| F2: `spawn_blocking` 线程池满 | 大量同步 I/O 任务堆积，超过默认 512 线程 | 监控 blocking pool 占用，超阈值拒绝新任务并降级（返回 "system busy"） |
| F3: `catch_unwind` 后状态不一致 | panic 在持锁期间发生，catch 后锁未释放 | 所有锁用 RAII guard，panic 自动 unwind 释放；catch_unwind 包在锁外层 |
| F4: WS application-level ping 误判 | 网络抖动导致 pong 延迟 > 20s，被误判断开 | ping 容忍 1 次丢失（仅 2 次连续丢失才 close），且 close 后客户端自动重连 |
| F5: fault-injection 测试漏覆盖 | mock 不够真实，生产暴露未注入的故障 | soak test 期间随机注入（chaos monkey），覆盖 mock 未覆盖的路径 |
| F6: metric 系统自身成为瓶颈 | metric 写 SQLite 频繁 → 自己阻塞 | metric 异步 batch 写，10s flush 一次；metric 自身失败不阻塞主流程 |
| F7: 历史教训注释过期 | 代码重构后 `// LESSON:` 注释指代的代码已变 | 每个 LESSON 注释附测试名，CI 验证测试仍存在且通过 |
| F8: CI gate lint 误报 | `clippy::unwrap_used` 在测试代码误报 | `#![cfg_attr(not(test), deny(clippy::unwrap_used))]` 测试豁免 |
| F9: 连接池引入新 bug | pool 库自身 bug 或配置不当 | 灰度：先在 dev profile 启用，soak test 验证 8h 无问题再 prod |
| F10: panic dump 写盘失败 | `hermix_home()` 磁盘满或权限不足 | dump 失败 fallback 到 tracing::error，不阻塞主流程 |

---

## 5. 验收路线图

```
Week 1-2 (P0):
  ├── P0-1 SQLite pool 引入
  ├── P0-2 session_persistence spawn_blocking
  ├── P0-3 server lib.rs handler 改造
  ├── P0-4 cdp.rs reader_lock 重构
  ├── P0-5 session.rs 嵌套锁修复
  ├── P0-6 CI gate 静态扫描
  └── P0-7 weixin task 生命周期修复
  验收：grep 验证 0 处违规 + 1000 次并发断开无死锁

Week 3-4 (P1):
  ├── P1-1/2/3 panic 边界
  ├── P1-4 CI lint
  ├── P1-5/6 超时矩阵
  ├── P1-7 WS app-level ping
  ├── P1-8/9 工具安全审查
  └── P1-10 eprintln 改 tracing
  验收：注入 panic/timeout/injection 测试全过

Week 5-8 (P2):
  ├── P2-1 metric 系统
  ├── P2-2 soak test
  ├── P2-3 fault-injection test
  ├── P2-4 crash dump
  ├── P2-5/6 安全加固
  ├── P2-7 教训代码化
  └── P2-8 子进程 pid 清理
  验收：8h soak test 达 SLO + metric 全可见
```

---

## 6. 不做什么（防止 scope creep）

- **不做**跨平台支持（Windows-only 是产品边界）
- **不做**外部网络安全（DDoS / 鉴权 / TLS）——这是另一个计划的范围
- **不做**前端稳定性（本计划只覆盖后端；前端稳定性另立计划）
- **不做**性能优化（除非稳定性需要）——性能优化另立计划
- **不做**大规模重构（如重写 agent loop）——只做稳定性必要的最小改动

---

## 7. 风险与依赖

| 风险 | 影响 | 缓解 |
|---|---|---|
| SQLite pool 引入可能改变查询语义（事务边界） | 高 | 先 dev profile 灰度 + 全量回归测试 |
| `clippy::unwrap_used` 强制开启可能阻塞 PR | 中 | 分批治理，先 deny 新增再治理存量 |
| `catch_unwind` 性能开销 | 低 | 仅在 task 边界包裹，不在热路径 |
| soak test 8h 占 CI 资源 | 中 | 每日夜间跑 1 次，不阻塞 PR |
| fault-injection mock 维护成本 | 中 | 优先用 trait 抽象 + 真实 mock，避免过度 mock |

---

## 8. 附录

### 8.1 盘点证据索引（sub-agent 报告关键行号）

- 锁使用：`crates/hermix-gateway/src/session.rs:448-451`、`crates/hermix-cua/src/browser/cdp.rs:212-216`、`gateway.rs` 30+ 处
- 同步 I/O：`session_persistence.rs:34/1124/1437/1495/1510/1528/1540/1549`、`server/lib.rs:2696/6635/6684`、`context.rs:660/685/701/768/772` 等
- panic：`server/lib.rs:7435`、`dashboard/skills.rs:743/761/785/793/800/806`、unsafe 6 处 env::set_var
- 子进程：`browser_cleanup.rs:23`、`cdp_bootstrap.rs:456`、`lib.rs:322`、`commands_custom_app.rs:225/233/241`
- WS：`lib.rs:6041` handle_ws_stream、`lib.rs:3953` handle_ws_stream_send、`lib.rs:5189` biased select、`lib.rs:4976` ws_disconnected
- 资源泄漏：`weixin.rs:1262` poll_task 未 join、`weixin.rs:369` inbound_tx clone

### 8.2 已知防线层级（保留参考）

```
tool_dispatch 60s/300s timeout
  → select!(rx, shutdown)
    → turn_transition 30s watchdog
      → stream_first_chunk 30s
        → stream_overall 180s
          → WS heartbeat 15s
```

### 8.3 现有测试基线（保留参考）

- `hermix-agent --lib`: 999 passed
- `hermix-tools --lib`: 618 passed
- `hermix-focus --lib`: 181 passed
- 总 ~1,834 unit tests
- 0 soak / 0 chaos / 0 fault-injection（本计划 P2 补齐）

---

**计划状态**：草案 v1，待主人评审后进入执行。
