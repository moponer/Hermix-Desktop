# 等待时间维度清单 第 2 轮复核（条目 4~15）

复核对象：`docs/wait-timeout-audit.md` 主清单条目 4~15（12 条）。
复核方式：逐条 read_file 读源码函数体，以实际代码为准，注释只作旁证。
三态：通过 / 需修正 / 不成立；无法核实的写「未核」并说明缺什么证据。

本次未跑构建、未装依赖、未启应用、未跑测试；所有行号来自 read_file / grep 的实际输出。

---

## 一、判定汇总

覆盖条数：12 条（条目 4~15 全部回源核对）。
判定分布：通过 9、需修正 2、不成立 1、未核 0。

| 条目 | 主题 | 判定 |
|---|---|---|
| 4 | stdio 桥 REST 120s 预算 | 通过 |
| 5 | 后端就绪轮询 90s / 1s | 需修正 |
| 6 | 后端重启探测 | 通过 |
| 7 | sidecar 就绪 30s / 500ms / 3s | 通过 |
| 8 | 成员评审终态 600s | 需修正 |
| 9 | MCP 120s / 15s / 10s | 通过 |
| 10 | LLM HTTP 10s / 180s | 通过 |
| 11 | 本机 Shell 120s | 通过 |
| 12 | 终端工具默认超时 120s | 不成立 |
| 13 | cron tick 60s / 文件锁 3s | 通过 |
| 14 | 更新器 15s / 600s / 30s / 2h | 通过 |
| 15 | 服务端状态探针 2s | 通过 |

---

## 二、逐条判定

### 条目 4 stdio 桥 REST 请求 120s 默认预算 —— 通过

- 位置：`apps/desktop-tauri/src-tauri/src/stdio_bridge.rs:272`（`http_request`）、`:281`（`request_with_budget`）、`:306`（`timeout(budget, rx)`）
- 源码：
```rust
        self.request_with_budget(method, path, body, std::time::Duration::from_secs(120))
            .await
```
```rust
        match tokio::time::timeout(budget, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err("http waiter dropped".into()),
            Err(_) => {
                // 超时清理挂起项，避免泄漏。
                if let Ok(mut p) = self.inner.pending_http.lock() {
                    p.remove(&id);
                }
                Err("backend request timed out".into())
            }
        }
```
- 等待发生位置：sidecar 进程之外的 **Tauri 主进程**，tokio worker 线程（`request_with_budget` 是 `async fn`，在 Tauri 命令的 async runtime 上跑）。等的是**本机管道**（sidecar stdin/stdout），不是网络。
- 界面表现：只是慢（120s 上限），但 120s 内界面无任何反馈，用户观感等同卡死。主清单判「卡死型」的理由（≥30s 无反馈）成立。
- 与前端的关系：前端 `fetchJSON` 有 10s 客户端死线（见条目 5 交叉核对），所以**用户实际看到的等待是 10s，不是 120s**——120s 是 Rust 侧后台 future 的存活时间，主清单只写 120s 会高估用户体感。

### 条目 5 后端就绪轮询 90s 预算 / 1s 间隔 —— 需修正

- 位置：`apps/desktop-tauri/src/api/rest.ts:1419`（`waitForBackendReady`）、`:1424-1426`（默认值）、`:1427-1430`（循环）
- 源码：
```ts
  const timeoutMs = opts.timeoutMs ?? 90_000;
  const intervalMs = opts.intervalMs ?? 1_000;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await checkBackendHealth()) return true;
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  return false;
```
- 与 F-1（10s 硬上限）的交叉核对结论：**两者不相干，90s 预算仍然成立**，但主清单的「最多约 90 次探测」这句推导是错的。
  - `waitForBackendReady` 调的 `checkBackendHealth`（`rest.ts:1376`）走的是 `invoke<boolean>(IPC.backendReady)`，**不是 `fetchJSON`**：
```ts
export const checkBackendHealth = async (): Promise<boolean> => {
  try {
    return await invoke<boolean>(IPC.backendReady);
  } catch {
    return false;
  }
};
```
  - `fetchJSON` 的 10s 死线在 `apiInvoke`（`rest.ts:99` `REQUEST_TIMEOUT_MS = 10_000`、`rest.ts:111` `AbortSignal.timeout`、`rest.ts:142` `Promise.race`）里，**只作用于 `apiInvoke` / `fetchJSON` 这一条路径**。`backend_ready` 是独立的 Tauri 命令，不经过 `apiInvoke`，所以拿不到那个 10s 死线。
  - `backend_ready` 的 Rust 侧（`apps/desktop-tauri/src-tauri/src/commands.rs:550`、`:566`）自带 2s 预算：`request_with_budget("GET", "/health", None, Duration::from_secs(2))`。
  - 因此真实节奏是：**每轮探测最多 2s + 1s 间隔 = 最多 3s 一轮**，90s 预算内最多约 30 轮探测（不是 90 次）。90s 总预算成立。
- 等待发生位置：**WebView2 渲染线程**（`async/await` + `setTimeout`，前端 JS 事件循环）。不阻塞 UI 渲染（await 让出），但首屏数据延后。
- 界面表现：只是慢——期间连接指示灯显示 connecting（`useBackendConnection.ts:31` `BOOT_GRACE_MS = 90_000`）。
- 修正建议：主清单条目 5 的「最多约 90 次探测」改为「最多约 30 次（每轮探针自身 2s 预算 + 1s 间隔）」；补一句「该路径不经过 `fetchJSON`，不受 F-1 的 10s 客户端死线约束」。

### 条目 6 后端重启探测 90s / 1s；在线探测 6s —— 通过

- 位置：`apps/desktop-tauri/src/hooks/useBackendConnection.ts:12`、`:13`、`:22`、`:31`、`:36`、`:39`、`:44`
- 源码：
```ts
const RESTART_PROBE_BUDGET_MS = 90_000;
const RESTART_PROBE_INTERVAL_MS = 1_000;
```
```ts
const PROBE_INTERVAL_MS = 6_000;
```
```ts
const BOOT_GRACE_MS = 90_000;
```
- 其余常量同样核到：`OFFLINE_FAILURE_THRESHOLD = 2`（`:36`）、`AUTO_RESTART_DELAY_MS = 8_000`（`:39`）、`CONNECT_STALLED_MS = 22_000`（`:44`）。
- 使用点核实：`RESTART_PROBE_BUDGET_MS` + `RESTART_PROBE_INTERVAL_MS` 用在 `hermix:server-restarting` 事件处理（`:210-222`）与 `restart()`（`:239-252`），两处都是 `deadline = Date.now() + RESTART_PROBE_BUDGET_MS` + 1s 间隔 + `api.checkBackendHealth()`。`PROBE_INTERVAL_MS` 用于 `usePolling(() => check(), PROBE_INTERVAL_MS)`（`:166`）。`BOOT_GRACE_MS` 用于首次失败后的 connecting 宽限（`:131-134`）。
- 等待发生位置：**WebView2 渲染线程**。每轮探测的 2s 预算在 Rust 侧（`commands.rs:566`）。
- 界面表现：只是慢。指示灯有 connecting/offline/重启中状态切换，22s 后亮出重启入口（`CONNECT_STALLED_MS`）。
- 判定：通过。

### 条目 7 sidecar 就绪等待 30s / 500ms / 探针 3s —— 通过

- 位置：`apps/desktop-tauri/src-tauri/src/sidecar.rs:403`（`wait_until_ready`）、`:413`（探针 3s）、`:434`（sleep 500ms）；调用点 `apps/desktop-tauri/src-tauri/src/lib.rs:1049`
- 源码：
```rust
    pub async fn wait_until_ready(deadline: Duration) -> bool {
        let start = std::time::Instant::now();
        while std::time::Instant::now() < start + deadline {
```
```rust
                let probe = bridge
                    .request_with_budget("GET", "/health", None, Duration::from_secs(3))
                    .await;
```
```rust
            tokio::time::sleep(Duration::from_millis(500)).await;
```
- 调用点核实（`lib.rs:1049`）：`HermixSidecar::wait_until_ready(std::time::Duration::from_secs(30)).await`，超时后仍 `emit("hermix:server-ready", ())`，与主清单「随后按未就绪处理」一致。
- 等待发生位置：**Tauri 主进程**（`tauri::async_runtime::spawn` 的 tokio worker），探针走本机 stdio 管道。等的是 sidecar 进程。
- 界面表现：只是慢（启动页最多等 30s）。启动期间窗口有 splash，不是白屏。
- 判定：通过。

### 条目 8 成员评审终态等待 600s / 500ms —— 需修正

- 位置：`crates/hermix-agent/src/member/runner.rs:695`（deadline）、`:710`（sleep 500ms）
- 源码：
```rust
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
```
```rust
            tokio::time::sleep(Duration::from_millis(500)).await;
            reviewer_snapshot = self.get(&reviewer_id).await;
```
- 修正点一：主清单写「最多约 1200 次」轮询。这只在 `self.get()` 瞬时返回时成立；`get()`（`runner.rs:457`）要读成员记录文件（`tokio::fs::read_to_string`），且循环体里 `self.get()` 的 await 在 sleep 之后串行执行。单轮实际耗时 = 文件读 + 500ms，故实际轮次数**少于** 1200 次。600s deadline 是真上限，1200 是上限上界。
- 修正点二：主清单写「轮询间隔 + 总次数」，描述不准确——代码里是**真实时钟 deadline**（`Instant::now() + 600s`），间隔只是节流；到点立即返回 `review_timeout`。这是条目 8 标题已写对的点，但「等待机制」栏的表述需要对齐。
- 等待发生位置：**hermix-agent 侧 tokio worker**（`runner.rs` 跑在服务端/agent 进程的异步运行时内，不在 WebView2，也不在 Tauri 主进程）。500ms 的 sleep 不阻塞 worker。
- 界面表现：只是慢（任务停在评审态，最多 10 分钟后返回 `review_timeout`）。
- 判定：需修正（数值成立，「1200 次」「总次数」的表述要改成真实时钟 deadline）。

### 条目 9 MCP 客户端 120s / initialize 15s / HTTP connect 10s、总 120s —— 通过

- 位置：`crates/hermix-mcp/src/client.rs:738`、`:772`、`:858`；`crates/hermix-mcp/src/transport.rs:707`、`:708`
- 源码：
```rust
        self.send_request_with_timeout(method, params, std::time::Duration::from_secs(120))
            .await
```
```rust
        let response =
            tokio::time::timeout(timeout, transport.receive())
                .await
                .map_err(|_| McpError::Timeout(format!("{method} timed out after {secs}s")))??;
```
```rust
                std::time::Duration::from_secs(15),
```
```rust
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
```
- 核实要点：`transport.receive()` 确实被 `tokio::time::timeout(timeout, ...)` 包裹（`client.rs:772`），stdio 传输的 receive 不是无上限等待。主清单「MCP transport receive 无上限」已改判，此处一致。
- 等待发生位置：**hermix-agent / server 进程的 tokio worker**（MCP client 在 agent 侧调用外部 MCP server；HTTP 传输时是外部网络）。
- 界面表现：只是慢（明确上限）。
- 判定：通过。

### 条目 10 LLM HTTP 客户端 connect 10s / 总 180s —— 通过

- 位置：`crates/hermix-agent/src/provider.rs:40`、`:41`；`crates/hermix-agent/src/api_bridge.rs:41`、`:42`
- 源码：
```rust
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .build()
        .unwrap_or_else(|_| Client::new())
```
```rust
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(180))
                .build()
                .unwrap_or_else(|_| Client::new()),
```
- 注意（不改判定）：两处都是 `unwrap_or_else(|_| Client::new())`——client 构建失败时**回退到无超时的 `Client::new()`**。`Client::new()` 默认 connect timeout 为 None，所以构建失败路径上 10s/180s 都不生效。构建失败在正常网络环境下极罕见，但主清单若对外引用「provider 一定 180s 封顶」需要带上这个例外。这属于条目 10 的限定条件，不推翻主判定。
- 等待发生位置：**hermix-agent 进程 tokio worker**，等外部 provider 网络。
- 界面表现：只是慢（已封顶），与条目 1 的 180s 外层看门狗同量级。
- 判定：通过（附上述限定）。

### 条目 11 本机 Shell 子进程默认 120s —— 通过

- 位置：`crates/hermix-core/src/seams/local.rs:110`（`DEFAULT_TIMEOUT`）、`:153`（`timeout(self.timeout, child.wait_with_output())`）
- 源码：
```rust
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
```
```rust
        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
```
- 补充核实：`LocalShell::default()` 把 `timeout: DEFAULT_TIMEOUT` 带进去（`local.rs` `impl Default for LocalShell`），所以「默认 120s」成立。Windows 侧挂 `JobGuard::attach`（`local.rs` `#[cfg(windows)] let job = child.id().and_then(crate::win_job::JobGuard::attach);`），超时能杀整棵树。
- 等待发生位置：**hermix-core / agent 进程 tokio worker**，等**外部子进程**（cmd /C 或 sh -c）。
- 界面表现：只是慢（120s 上限）。
- 判定：通过。

### 条目 12 终端工具默认超时 120s —— 不成立

- 位置：`crates/hermix-config/src/config.rs:726`（`#[serde(default = "default_terminal_timeout")]`）、`:758`、`:767`
- 源码：
```rust
fn default_terminal_timeout() -> u64 {
    120
}
```
```rust
            timeout: default_terminal_timeout(),
```
- 判定理由：全仓 grep 只找到 `crates/hermix-config/src/config.rs`（定义与 `Default` 写入）和 `crates/hermix-config/src/loader.rs:1149`、`:1151`、`:1228`（校验 `terminal.timeout == 0` 报错，以及 `#[cfg(test)]` 里的用例）。**没有任何消费点把这个 `timeout` 值传给实际的执行超时**：`crates/hermix-environments/src/manager.rs` 只用 `config.backend` 选后端（`create_backend`，`TerminalBackendType::Local / Ssh / Docker`），`LocalBackend::default()`（`crates/hermix-agent/tests/agent_builder_unit.rs:48`）走的是 `hermix_core` 的 `LocalShell` 默认 120s，而不是来自 `TerminalConfig.timeout`。
- 所以 `TerminalConfig.timeout = 120` 目前是一个**只被校验、不被执行**的配置字段：真正生效的 120s 是 `LocalShell::DEFAULT_TIMEOUT`（条目 11），两者数值相同但来源无关。
- 「不成立」的具体所指：主清单条目 12 声称「终端工具默认超时 120s」由 `config.rs` 的 `default_terminal_timeout` 支撑——这个因果关系不成立。数值 120 在终端工具上确实存在（经 LocalShell），但依据的代码位置错了。
- 等待发生位置：同条目 11（外部子进程，agent 进程 tokio worker）。
- 修正建议：条目 12 改判为「配置项未接线」；把它并入条目 11，或改写为「终端工具的 120s 实际来自 `hermix-core` 的 `LocalShell::DEFAULT_TIMEOUT`；`TerminalConfig.timeout` 当前仅在 loader 做非零校验，未见消费点」。若要给 `TerminalConfig.timeout` 接线，是产品改动，不属本次复核范围。

### 条目 13 cron tick 60s、文件锁 3s —— 通过

- 位置：`crates/hermix-cron/src/scheduler.rs:26`（`cron_poll_interval`）、`:29`（`.unwrap_or(60)`）、`:38`（`default_lock_timeout`）、`:41`（`.unwrap_or(3)`）
- 源码：
```rust
fn cron_poll_interval() -> Duration {
    let secs = env_compat("HERMIX_CRON_TICK_SECS", "HERMES_CRON_TICK_SECS")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 300);
    Duration::from_secs(secs)
}
```
```rust
fn default_lock_timeout() -> Duration {
    let secs = env_compat("HERMIX_CRON_LOCK_TIMEOUT_SECS", "HERMES_CRON_LOCK_TIMEOUT_SECS")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(3)
        .clamp(1, 60);
    Duration::from_secs(secs)
}
```
- 消费点核实：
  - tick：`scheduler.rs:250` `_ = time::sleep(cron_poll_interval()) => {`，包在 `tokio::select!` 里（另一分支是 stop 信号）。60s 是「最多延迟 60s」的轮询间隔，clamp 上限 300s。
  - 文件锁：`scheduler.rs:157` `let timeout = default_lock_timeout();` → `:164` `tokio::task::spawn_blocking(move || lock.acquire(timeout))`；`scheduler.rs:320` 同样传 `default_lock_timeout()`。锁的实际等待在 `crates/hermix-cron/src/file_lock.rs:126-158` 的循环里：`file.try_lock_exclusive()`，失败且未到 timeout 就 `std::thread::sleep(RETRY_INTERVAL)` 重试，超时返回 `LockError::Timeout`。timeout=0 走 `try_lock_exclusive` 非阻塞分支（`file_lock.rs:104-124`）。
- 等待发生位置：**hermix-cron 进程 tokio worker**；文件锁的 OS 级抢锁在 `spawn_blocking` 的阻塞线程池线程上（不会卡住 worker）。
- 界面表现：只是慢（cron 触发最多延迟 60s；锁竞争最多等 3s）。
- 判定：通过。

### 条目 14 更新器 15s / 600s / 30s / 2h —— 通过

- 位置：`apps/desktop-tauri/src-tauri/src/updater.rs:19`（`REQUEST_TIMEOUT_SECS = 15`）、`:98`（清单请求套 15s）、`:213`（下载 600s）、`:272`（首查 30s）、`:274`（每轮 2h）
- 源码：
```rust
const REQUEST_TIMEOUT_SECS: u64 = 15;
```
```rust
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
```
```rust
        .timeout(std::time::Duration::from_secs(600))
```
```rust
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(2 * 3600)).await;
```
- 进度反馈核实：`updater.rs` 下载循环里每 512KB 发一次 `update://progress` 事件（`received / total`），确认主清单「有反馈」成立。
- 等待发生位置：**Tauri 主进程 tokio worker**（`tauri::async_runtime::spawn`），等外部网络（更新服务器）。
- 界面表现：只是慢——下载有进度条，不是卡死。
- 判定：通过。

### 条目 15 服务端状态探针 2s 预算 —— 通过

- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:396-398`（`get_server_status` 的 2s）、`:566`（`backend_ready` 的 2s）
- 源码：
```rust
                let fut = bridge.http_request("GET", "/health", None);
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(2), fut).await,
                    Ok(Ok(reply)) if reply.contains("\"status\":\"ok\"")
                )
```
```rust
    match bridge
        .request_with_budget("GET", "/health", None, std::time::Duration::from_secs(2))
        .await
```
- 判定说明：`get_server_status` 是**外层**再套 2s timeout 包 `http_request`（内层本身 120s）。按源码注释（`commands.rs:392-394`），外层 timeout 的意图是覆盖半开管道。这条路径确实 2s 收敛。
- 等待发生位置：**Tauri 主进程 tokio worker**，等本机 stdio 管道。
- 界面表现：只是慢（最多 2s）。
- 判定：通过。

---

## 三、未核与缺证

本轮 12 条全部回源到函数体，未留「未核」条目。以下三点是判定中记录到的**限定条件 / 未完全证实的旁支**，引用时需要带上：

1. **条目 12 的因果链不成立**（已判不成立）：`TerminalConfig.timeout` 只在 `hermix-config/src/loader.rs:1149` 做非零校验，仓库内未找到把它传给执行层的消费点。缺的证据：若存在仓外/生成代码/宏注入的消费点，本条需复议；本次 grep 范围覆盖 `crates/**`，未覆盖 `apps/desktop-tauri/src-tauri`（该侧不读 `TerminalConfig`）。
2. **条目 10 的 fallback 缺口**：`unwrap_or_else(|_| Client::new())` 在构建失败时回到无 connect timeout、无总超时的客户端。这不是超时缺失，而是「超时可能被绕过」的边缘路径，缺的是构建失败的发生条件证据。
3. **条目 4 与条目 5 的体感差**：用户实际等待是前端 10s（`rest.ts:99` `REQUEST_TIMEOUT_MS`）而非 Rust 侧 120s。120s 是 Rust 后台 future 的存活上限，两者不冲突，但对外引用「最长等 120s」会高估用户体感。

---

## 四、总体结论

### 可以直接对外引用（9 条）

条目 4、6、7、9、10、11、13、14、15。数值、行号、等待机制均经 read_file 核对通过，等级（卡死型 / 只是慢）符合代码实际。

引用时带三个限定：
- 条目 4 的 120s 是 Rust 侧上限，用户可见等待是前端 10s。
- 条目 10 的 10s/180s 在 client 构建失败时会被 `Client::new()` 绕过。
- 条目 11 的 120s 对应 `LocalShell`，与条目 12 的配置项无关。

### 需修正后再引用（2 条）

- **条目 5**：数值与总预算成立（90s 预算、1s 间隔），但「最多约 90 次探测」错误（每轮探针自身 2s 预算，实际约 30 次）；须补「该路径不经过 `fetchJSON`，不受 F-1 的 10s 死线约束」——这是简报第 4 条交叉核对要求的落点，结论是**90s 预算成立，叠加关系不成立**。
- **条目 8**：600s 真实时钟 deadline 成立；「最多约 1200 次」是上界而非实际值，「总次数」表述要改成 deadline。

### 不可对外引用（1 条）

- **条目 12**：所述因果关系（终端工具 120s 来自 `config.rs:767`）不成立。该字段未被消费。终端工具的真实 120s 上限在条目 11（`hermix-core` `LocalShell`），改引条目 11 即可。

### 清单整体能否直接给用户

不能整份直接给。12 条里 1 条需撤下（条目 12）、2 条需改表述（条目 5、8）。改完这 3 条后，条目 4~15 这段可以对外引用。

主清单第六节的汇总口径（"成立合计 18 条"、"成立 9 条/5 条/0 条/3 条/4 条"）本次未采信、未复核——按简报要求不作为判定依据。该口径与本轮结论的关系：**主清单的成立条数不因条目 12 的改判而直接减少**，因为条目 11 已经独立覆盖了本机 shell 120s；但主清单把 12、11 拆成两条分别计数，条目 12 的独立成立性不成立，这一点要在主清单修订时体现。第 1 轮复核指出的汇总口径问题仍未解决。

### 建议的主清单修订项

1. 条目 12 撤下或改写为「配置项未接线」，120s 归入条目 11。
2. 条目 5 的探测次数改为「约 30 次」并注明不经过 F-1 的 10s 死线。
3. 条目 8 的「总次数」改为「真实时钟 deadline 600s，500ms 节流」。
4. 条目 4 补一句「用户可见等待由前端 10s 死线决定」。
