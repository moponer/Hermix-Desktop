# Transport Stdio Migration RFC

> **Status**: Phase 0 RFC — not yet implemented
> **Author**: 狗蛋
> **Owner**: 主人
> **Created**: 2026-09-03
> **Goal**: 消灭 Hermix Desktop 对 `127.0.0.1:3000` 的 TCP 依赖，从根消除"两个 hermix 装版跑同一台机器"时的端口冲突 + 数据互覆盖。

---

## 1. 背景与动机

### 1.1 主人原话（2026-09-03 14:37）

> 突然发现一个问题，如果我同时开发两个项目，都是 dev 模式启动，那么一定会有一个报错 1420 端口占用。如果这两个项目以后安装跑在一台设备上，端口岂不是冲突了？

### 1.2 实际冲突层级

| 资源 | 当前值 | dev 模式冲突？ | 装版跑同一台设备冲突？ |
|---|---|---|---|
| vite dev server | `localhost:1420` | ⚠️ 是（仅 dev） | ❌ 不适用（装版不走 vite） |
| hermix-server HTTP/WS | `127.0.0.1:3000` | ⚠️ 是（dev 也跑 hermix） | ✅ **真冲突** |
| 前端 hardcoded `ws://127.0.0.1:3000` | 见 §2.3 | ⚠️ 是 | ✅ 跟着冲突 |
| `~/.hermix/` 共享目录 | 默认未隔离 | ⚠️ 是（dev 都用 ~/.hermix） | ✅ **数据互覆盖**（最大隐患） |
| Tauri 主进程 | 每个 exe 独立进程 | ❌ | ❌ |
| Tauri WebView | `tauri://localhost`（Windows = edge://） | ❌ | ❌（**不走 TCP**） |

### 1.3 主人决策（2026-09-03 14:50）

> 统一模式

**dev / prod 共用同一条 stdio IPC 路径**——不保留 TCP 双 listen，不留 dev/prod 分流。完全消除 localhost TCP 依赖。

---

## 2. 现状审计

### 2.1 hermix-server 链路（crates/hermix-server/src/）

| 文件 | 关键行 | 现状 |
|---|---|---|
| `main.rs:15-22` | env 解析 | 只读 `HERMIX_SERVER_ADDR` env，**不解析 argv**（sidecar `["serve", "--port", "3000"]` 被忽略，注释 outdated） |
| `lib.rs:423` | `pub async fn run_server(addr, config)` | 入口 |
| `lib.rs:428-530` | `run_server_with_state` | TcpSocket + SO_REUSEADDR + axum::serve |
| `lib.rs:523` | `axum::serve(listener, app)` | TCP listen 主体 |
| `ws.rs:65` | `ws_upgrade` | WebSocket upgrade 入口 1（per-session） |
| `ws.rs:155` | `ws_stream_upgrade` | WebSocket upgrade 入口 2（streaming） |
| `ws.rs:2171` | 第三个 WS handler | groupchat / 群聊 |
| `dashboard/consciousness.rs:52` | consciousness WS | 仪表盘实时 |

**4 个 WS 路由 + 1 个 axum HTTP server，全部基于 TCP。**

### 2.2 hermix-transport 现状（crates/hermix-transport/src/）

- **不依赖 tokio / axum / tokio-tungstenite**——纯 wire-level 类型 + JSON Schema
- 仅 `lib.rs` 一个文件
- **不含 transport 实现**——WS 协议实现全在 hermix-server 用 axum 写的

> **结论**：hermix-transport 是"消息形状"层，与"传输方式"解耦。stdio 迁移**不需要改 hermix-transport 的内部**（消息类型不变），只需要在 hermix-server 增加 stdio 入口。

### 2.3 前端 HTTP / WS 客户端

| 文件 | 调用 |
|---|---|
| `apps/desktop-tauri/src/api/rest.ts:61-62` | `API_BASE = "http://127.0.0.1:3000"` / `WS_BASE = "ws://127.0.0.1:3000"` |
| `apps/desktop-tauri/src/main.tsx:44` | `HEALTH_URL = "http://127.0.0.1:3000/health"` |
| `apps/desktop-tauri/src/api/ws.ts` | WS 客户端封装 |
| `apps/desktop-tauri/src/hooks/useStreamChat.ts:568` | `new WebSocket(resolveWebSocketUrl(...))` |
| `apps/desktop-tauri/src/hooks/useWsConnection.ts:13,59` | 同上 |
| `apps/desktop-tauri/src/components/groupchat/useGroupChatWebSocket.ts:101` | 群聊 WS |
| `apps/desktop-tauri/src-tauri/tauri.conf.json:28` | CSP `connect-src ... ws://127.0.0.1:3000 http://127.0.0.1:3000 https:` |

### 2.4 Tauri sidecar spawn（apps/desktop-tauri/src-tauri/src/sidecar.rs）

```rust
// 行 192-202（关键片段）
cmd.args(["serve", "--port", &self.port.to_string()])
    .stdout(Stdio::piped())       // 已 piped（用于日志）
    .stderr(Stdio::piped())       // 已 piped（用于日志）
    .creation_flags(CREATE_NO_WINDOW)
    .env("HERMIX_SERVER_ADDR", format!("127.0.0.1:{}", self.port));
```

**当前问题**：
- `args([...])` 被 hermix-server 忽略（main.rs 不解析 argv）
- `HERMIX_SERVER_ADDR` env 生效，但需要 TCP 端口
- **stdin 未 piped**（`Stdio::piped()` 没加 → 子进程 stdin 默认继承父进程，Tauri 主进程没法往 hermix.exe 写）

### 2.5 已有 tauri::command 基础

`apps/desktop-tauri/src-tauri/src/commands.rs` 有 **15 个 `#[tauri::command]`**。前端已经在用 invoke（grep 命中 `BackendStatus.tsx` / `BrowserTab.tsx` / `ConnectionIndicator.tsx` / `BackendSettingsTab.tsx` / `Sidebar.tsx` / `TitleBarConnectionIndicator.tsx` / `desktopBridge.ts` / `lib/customAppApi.ts`）。

> **结论**：前端 invoke 模式**已落地**，D 只需把所有 REST/WS 调用**批量迁移**到 tauri command。

---

## 3. 目标架构

### 3.1 数据流（统一模式）

```
┌─────────────────────────────────────────────────────────────────┐
│  Tauri 主进程 (Rust)                                              │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ apps/desktop-tauri/src-tauri/                            │   │
│  │   ├─ commands.rs  (15+ #[tauri::command] — 扩到 ~40)    │   │
│  │   └─ sidecar.rs                                          │   │
│  │        ├─ spawn hermix.exe (Stdio::piped() ×3)          │   │
│  │        ├─ stdin  ← 子进程 AsyncRead                     │   │
│  │        ├─ stdout → 子进程 AsyncWrite                    │   │
│  │        └─ stderr → 日志 (已存在)                         │   │
│  └──────────────────────────────────────────────────────────┘   │
│         ▲                                                       │
│         │ tauri::ipc::invoke (HTTP-free)                         │
│         │                                                       │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ WebView (React 19, hermix-desktop frontend)             │   │
│  │   ├─ src/api/rest.ts → invoke(cmd_name, args)           │   │
│  │   ├─ src/api/ws.ts → invoke 流式 channel (NEW)          │   │
│  │   ├─ src/api/stream.ts → invoke 流式 API (NEW)          │   │
│  │   └─ 0 个 ws://  / 0 个 http://127.0.0.1               │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                ▲           │
                │stdin      │stdout/stderr
                │           ▼
┌─────────────────────────────────────────────────────────────────┐
│  hermix.exe 子进程 (Rust)                                         │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ crates/hermix-server/src/                                │   │
│  │   ├─ main.rs (NEW: 解析 argv + stdio flag)              │   │
│  │   ├─ stdio.rs (NEW: stdin→JSON Lines, stdout→JSON)      │   │
│  │   ├─ transport.rs (NEW: 提取 ws.rs 内部逻辑, 不绑 TCP)  │   │
│  │   └─ lib.rs                                              │   │
│  │        ├─ run_server_tcp(addr) → KEEP for unit tests    │   │
│  │        └─ run_server_stdio() → 新主入口                  │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

### 3.2 stdio 协议形状（JSON Lines 协议）

```
# stdin (前端 → 后端，每行一条 JSON):
{"id": "1", "op": "send_message", "session_id": "abc", "text": "...", ...}
{"id": "2", "op": "subscribe_stream", "session_id": "abc"}
{"id": "3", "op": "groupchat", "room_id": "xyz", ...}
{"id": "4", "op": "consciousness_subscribe"}

# stdout (后端 → 前端，每行一条 JSON):
{"id": "1", "ok": true, "data": {...}}
{"id": "2", "event": "stream_chunk", "data": {...}}
{"id": "2", "event": "stream_done", "data": {...}}
{"id": "3", "event": "groupchat_message", "data": {...}}
{"id": "4", "event": "consciousness_tick", "data": {...}}
```

**设计要点**：
- **JSON Lines**（每行一个完整 JSON 对象）—— 简单、自描述、易调试
- **请求-响应匹配**：`{"id": "1", ...}` 用 id 字段配对（多次 invoke 复用同一连接）
- **服务端主动推送**：后端没有"匹配 id"的事件（如 stream_chunk、groupchat_message），是 fire-and-forget 流
- **stdio ≠ WebSocket 的核心区别**：TCP 双工 → JSON Lines 单工 stdin/stdout。**前端需要新组件**：`tauri-stdio-bridge.ts`，把 invoke + 长连接 stream 包装成 Promise<Event> 流

### 3.3 CSP 收紧

**改前**：
```json
"csp": "default-src 'self'; connect-src 'self' http://127.0.0.1:3000 ws://127.0.0.1:3000 https:; ..."
```

**改后**：
```json
"csp": "default-src 'self'; connect-src 'self' https:; ..."
```

**安全 +1**：删 ws/http 内网段，LLM provider 直连（`https://api.openai.com/...` 等）走 `https:` 不受影响。

---

## 4. 每个 crate / 文件的改动面

### 4.1 crates/hermix-server/

| 文件 | 改动 | 行数估计 |
|---|---|---|
| `Cargo.toml` | + `tokio-util`（Codec），+ `futures` | +5 |
| `src/main.rs` | 加 clap 解析 `--stdio` / `--tcp-addr` flag（默认 `--stdio`，向后兼容） | 重写 23 行 |
| `src/lib.rs` | 把 `run_server(addr, config)` 拆为 `run_server_stdio(config)` + `run_server_tcp(addr, config)`（TCP 仅供测试） | ~50 行 |
| `src/stdio.rs` | **新增**：JSON Lines 协议实现（stdin→enum Op, stdout→enum Event） | ~200 行 |
| `src/ws.rs` | 抽 `WsEnvelope` / `StreamEvent` → `transport.rs::Transport` trait（impl: Stdio / WebSocket / TcpStream） | 重构 2169 行 → 拆 ~300 行公共逻辑 |
| `src/dashboard/consciousness.rs` | 跟 ws.rs 同步抽 transport trait | ~50 行 |

### 4.2 crates/hermix-transport/

| 文件 | 改动 |
|---|---|
| `src/lib.rs` | **不变**——wire 类型层与传输方式解耦 |

### 4.3 crates/hermix-acp/

| 文件 | 改动 |
|---|---|
| `src/server.rs` | 加 `acp+stdio://` scheme 处理（已有 `acp+tcp://127.0.0.1:8765` 测试，参考） |
| `src/agent_loop/tests.rs:2275,2293` | 测试 fixture：保留 TCP test + 加 stdio test |

### 4.4 apps/desktop-tauri/src-tauri/

| 文件 | 改动 | 行数估计 |
|---|---|---|
| `src/sidecar.rs:192-202` | 删 `HERMIX_SERVER_ADDR` env；加 `.stdin(Stdio::piped())`；spawn 后**接管子进程 stdin/stdout** 作为 `tokio::process::ChildStdin/ChildStdout` | ~40 行 |
| `src/stdio_bridge.rs` | **新增**：在 Tauri 主进程持一个 `Mutex<ChildStdin>` + 后台 task 读 `ChildStdout` 拆 JSON Lines → `AppHandle::emit("hermix-event", payload)` | ~150 行 |
| `src/commands.rs` | 加 ~25 个新 `#[tauri::command]`（每个 REST endpoint + WS subscribe 一个），**全部走 stdio_bridge 写 stdin** | +400 行 |
| `src/lib.rs` / `tauri.conf.json` | 注册新 commands | ~20 行 |

### 4.5 apps/desktop-tauri/src/

| 文件 | 改动 | 行数估计 |
|---|---|---|
| `src/api/rest.ts` | 删 `API_BASE` / `WS_BASE` / `HEALTH_URL`；每个 `fetch(API_BASE + "/x")` 改成 `invoke("x", args)`；保留类型导出 | 重构 ~600 行 |
| `src/api/ws.ts` | **删**——由 `src/api/stream.ts` 替代（tauri event 订阅） | -150 行 |
| `src/api/stream.ts` | **新增**：`subscribeStream(sessionId)` 返回 AsyncIterator<Event>（基于 `listen("hermix-event", ...)`） | ~120 行 |
| `src/api/types.ts` | 不变 | - |
| `src/hooks/useStreamChat.ts:568` | `new WebSocket(...)` → `for await (const ev of subscribeStream(...))` | 重构 |
| `src/hooks/useWsConnection.ts:13,59` | 同上 | 重构 |
| `src/components/groupchat/useGroupChatWebSocket.ts:101` | 同上 | 重构 |
| `src/main.tsx:44` | `HEALTH_URL` 删除，改 `await invoke("health")` | 1 行 |

### 4.6 tauri.conf.json

```diff
-"csp": "default-src 'self'; connect-src 'self' http://127.0.0.1:3000 ws://127.0.0.1:3000 https:; ..."
+"csp": "default-src 'self'; connect-src 'self' https:; ..."
```

---

## 5. 双轨测试策略

### 5.1 hermix-server 单元测试

- **保留** `run_server_tcp()` 仅供单元测试用（`#[cfg(test)]` gating）
- **新增** `run_server_stdio()` 单元测试（用 `tokio::io::duplex` 模拟 stdin/stdout）
- **传输层 trait 测试**：`Transport::send / recv` 用 `duplex` 跑 N 次，验证序列化

### 5.2 hermix-acp 测试

```rust
// crates/hermix-agent/src/agent_loop/tests.rs:2275
base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
```
**保留** + 新增 `base_url: Some("acp+stdio://".to_string())` fixture。

### 5.3 端到端

- `cargo test --workspace --lib` 跑全 lib tests（**预期 ≥1500 tests**）
- `pnpm vitest` 跑前端 tests（新增 `stream.test.ts`）
- **dev / prod 双场景**：dev 走 vite 1420 + stdio spawn hermix.exe（dev 也是），prod 走 stdio spawn hermix.exe——**代码路径完全相同**

---

## 6. 风险点

### 6.1 流式事件的 tauri event 桥接

**问题**：Tauri `AppHandle::emit` 是 fire-and-forget，**不保证顺序**。当前 WS 通过同一 socket 推 stream 事件，**天然有序**。stdio 桥接如果多个事件并发 emit → 前端收到顺序可能乱。

**对策**：
- hermix.exe → Tauri 主进程的 stdout 读 task 内部**串行 emit**（`AppHandle::emit` 是顺序的，从同一个 task 调）
- 或用 `app.emit_to(window_label, ...)` 替代全局 emit
- 加 stdio 协议 `seq` 字段，前端收到时检查 seq 跳过陈旧事件

### 6.2 hermix-server 多窗口支持

**问题**：当前 ws.rs 的 `session_broadcasts` 是 `static OnceLock`，单进程多窗口 OK。stdio 后同一进程也 OK（一个 stdin 多个 EventId 路由）。

**对策**：协议设计 `{ "op": "subscribe", "session_id": "X" }` → 后续 `{"event": "...", "session_id": "X"}` 自动路由到正确订阅者。**多窗口共用同一份订阅**（要单独再设计 `op: "subscribe_window"`）。

### 6.3 Cargo workspace 编译时间

`hermix-server` 加 tokio-util / futures 后冷依赖增加 30s~1min。**F 盘 target 仍 OK**（master memory 已记）。

### 6.4 hermix-server argv 解析

main.rs 当前不解析 argv，注释说"future when the binary grows a clap parser"。**D-Phase 2 是引入 clap 的时机**。

### 6.5 dev 模式 vs prod 模式分流

按主人"统一模式"，**不引入**分流。dev 模式 Tauri 主进程也 spawn hermix.exe 接管 stdio（dev 不再 `cargo run` 直接跑 server）——这跟当前 dev 模式行为有微小差别（当前 dev 直接 `cargo run -p hermix-server` 跑 TCP server，**没走 Tauri spawn**），需要确认 dev 模式是否要保持"直接 cargo run"。

**主人待确认**：dev 模式下
- (i) 仍用 `cargo run -p hermix-server`（需要 hermix-server 支持 `--stdio` flag + stdin/stdout）
- (ii) 改用 Tauri 主进程 spawn（dev / prod 完全统一）

按主人"统一模式"语义，倾向 (ii)，但需要主人确认。

---

## 7. 阶段拆分与工作量

| Phase | 内容 | Commit | 文件数 | 行数 | 预计 commit 标题 |
|---|---|---|---|---|---|
| **D-1** | hermix-server 加 `Transport` trait + 抽 ws.rs 公共逻辑；**不动调用方** | 1 | ~3 | ~300 | `refactor(server): extract Transport trait from ws.rs` |
| **D-2** | hermix-server 加 `stdio.rs`（JSON Lines 协议）+ `--stdio` argv 解析；TCP 路径标 `#[cfg(test)]`；**新增**不破坏 | 1 | ~4 | ~250 | `feat(server): stdio JSON Lines transport` |
| **D-3** | Tauri sidecar.rs 加 `Stdio::piped()` 接管 stdin/stdout；新增 `stdio_bridge.rs`（Tauri 主进程内部 → AppHandle::emit 桥）；**新增**不破坏前端 | 1 | ~3 | ~200 | `feat(tauri): stdio bridge between hermix sidecar and WebView` |
| **D-4** | Tauri commands.rs 加 ~25 个新 `#[tauri::command]`（每个 REST endpoint 一对一）；**新增**不删旧 REST endpoint | 1 | ~2 | ~400 | `feat(tauri): expose all REST/WS endpoints as tauri commands` |
| **D-5** | 前端 `src/api/rest.ts` 重写（fetch → invoke）；`src/api/ws.ts` 删；`src/api/stream.ts` 新增；4 个 hook 重构 | 1 | ~7 | ~700 | `refactor(frontend): migrate REST/WS to tauri invoke + stream events` |
| **D-6** | CSP 收紧；删 `HERMIX_SERVER_ADDR` env 处理；删 old REST/WS endpoint + dead code；装版默认隔离 `HERMIX_HOME=$INSTALL_DIR/data` | 1 | ~5 | ~150 | `chore: remove TCP transport + tighten CSP` |
| **D-7** | 测试：stdio 集成测试 + 前端 stream.test.ts | 1 | ~4 | ~300 | `test: stdio transport integration coverage` |
| **D-8** | 文档更新：`docs/transport-stdio-migration.md` 标 "Status: Implemented" + 注释 update | 1 | ~5 | ~80 | `docs: mark stdio migration as implemented` |

**合计**：8 commit，~33 文件，~2380 行（含新增 + 删除 + 注释）。

---

## 8. 兼容性策略

### 8.1 hermix-server 向后兼容

- **保留** `--tcp-addr` argv flag（默认 127.0.0.1:0 = 随机端口），仅供**单元测试和 hermix-cli 调试**使用
- **新增** `--stdio` argv flag（默认 stdio，作为生产入口）

### 8.2 Tauri 向后兼容

- **新增** stdio spawn 模式（默认）
- **保留** `HERMIX_SERVER_ADDR` env 在代码里但**注释标 deprecated**（D-6 删除）

### 8.3 前端向后兼容

- **删除** `API_BASE` / `WS_BASE` / `HEALTH_URL` 常量（D-5 一刀切，全栈统一）
- 不做"渐进式迁移"——主人要"统一模式"，前端不留 ws:// fallback

---

## 9. 验收标准

1. `cargo check --workspace --all-targets` 0 errors 0 warnings
2. `cargo test --workspace --lib` ≥1500 passed
3. `pnpm vitest` 142/142 passed + 新增 stream.test.ts ≥5 tests
4. `pnpm tsc --noEmit` 0 errors
5. dev 模式：`pnpm tauri dev` 能起 hermix.exe + 前端能 invoke 全部命令 + stream 事件有序
6. 装版：`cargo tauri build` 产出 exe 装版后能正常 invoke 全部命令
7. **冲突验收**：同一台机器跑两个 hermix-desktop 装版，**两个都能正常启动**（不撞 3000 端口）；数据 home 默认隔离（撞 ~/.hermix 也不可能）
8. CSP 收紧后 LLM provider 直连（`https://api.openai.com/...`）不受影响

---

## 10. 待主人确认决策点

1. **dev 模式是否改用 Tauri spawn hermix.exe**（统一 vs 保留 `cargo run -p hermix-server`）——主人语义"统一模式"倾向改 Tauri spawn，但 dev 体验是否一致要验证
2. **Phase 拆分粒度**：8 commit 是细粒度，是否要合并到 4-5 commit（按"协议 / Tauri 桥 / 前端 / 收紧"四大块）
3. **是否需要保留 hermix-server TCP listen 给外部调试**（如 hermix-cli 单独命令行工具）

---

**End of RFC — 待主人 review 后开 Phase 1**
