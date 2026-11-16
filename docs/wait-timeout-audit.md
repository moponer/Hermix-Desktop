# 时间维度等待点清单（hermix-desktop）

编号条目 18 条（条目 1–18），编号不重排。

范围：会让界面看起来像卡死的「等很久」——默认超时值、轮询间隔、重试退避、无上限 await、连接/子进程无超时等待。
技术栈：Tauri v2 桌面端（apps/desktop-tauri）+ Rust crates（crates/*）+ React 前端（apps/desktop-tauri/src）。
本轮未启动应用、未跑测试、未跑构建。所有秒/毫秒数值来自源码字面量或源码注释；源码注释自述的数值标注「注释自述，未实测」。行号均按本轮回源读取的源码位置标注。

影响分级（每条末尾标注）：
- **卡死型**：无上限等待，或超时值大到用户以为界面死了（≥30s 无任何反馈）。
- **只是慢**：有明确上限且期间有可见反馈（进度、提示、状态灯），到点必然收敛。

条目格式：位置（路径:行号）→ 等待时长口径 → 触发条件 → 界面表现（用户视角）→ 证据片段。

---

## 一、卡死型

### 1. LLM 流式响应：首 chunk 30s 提前 abort，整体流式兜底 180s
- 位置：`crates/hermix-agent/src/agent_config.rs:903`（`default_stream_overall_timeout_secs` = 180）、`:912`（`default_stream_first_chunk_timeout_secs` = 30）、`:918`（`default_stream_idle_timeout_secs` = 120）；180s 兜底执行点 `crates/hermix-agent/src/stream_collect.rs:353`（读 `config.stream_overall_timeout_secs`）与 `:371`（`tokio::time::timeout(dur, inner_fut)`）；首 chunk 提前收口 `crates/hermix-agent/src/stream_collect.rs:713-731`（`:729` 打 `finish_reason = "no_first_chunk"`）
- 等待时长口径：默认超时值，三档。30s 未到首 chunk → **提前 abort**（`finish_reason="no_first_chunk"`），**不落到 180s 兜底**；首 chunk 到达后静默 120s → idle abort；**已进入流式但整体超时** → 180s 外层兜底（`:353`/`:371`）。`overall_secs == 0` 时看门狗整体关闭（`stream_collect.rs:366-370`）。
- 触发条件：provider 打开 SSE 后不推任何 chunk；或已开流但长时间不再推进。
- 界面表现：用户点发送后 30 秒内看不到任何 token，界面一直转圈，随后以「无首块」结束；已开始出字后，整段流超过 180 秒才会被外层判超时。卡死型。
- 证据片段：
```rust
fn default_stream_overall_timeout_secs() -> u64 {
    180
}
```
```rust
                    let elapsed = collect_started.elapsed();
                    if elapsed >= first_chunk_deadline_dur {
                        tracing::warn!(
                            elapsed_secs = elapsed.as_secs(),
                            first_chunk_deadline_secs = first_chunk_deadline,
                            active_model = %active_model,
                            "no first chunk arrived within first_chunk_deadline — \
                             aborting stream early to recover from upstream hang"
                        );
```
```rust
    if overall_secs == 0 {
        return inner_fut.await;
    }
    let dur = std::time::Duration::from_secs(overall_secs);
    match tokio::time::timeout(dur, inner_fut).await {
```

### 2. 前端流式看门狗：idle 固定 300s，stale 与 mid-stream 在工具在飞时放大到 600s
- 位置：`apps/desktop-tauri/src/hooks/useStreamChat.ts:608`（`IDLE_TIMEOUT_MS` = 300_000）、`:609`（`STALE_STREAM_TIMEOUT_MS` = 300_000）、`:610`（`TOOL_STALE_CAP_MS` = 300_000 × 2）、`:618`（`MID_STREAM_SILENCE_MS` = 60_000）；使用点 `:648`（`resetIdleTimeout` 用固定 `IDLE_TIMEOUT_MS`）、`:651-660`（`resetStaleTimer` 按 `toolRunningRef.current` 选 600s/300s）、`:621-640`（`resetMidStreamSilenceTimer` 同样按工具在飞选 600s/60s）
- 等待时长口径：默认超时值。**idle 固定 300s，不随工具在飞放大**；**stale 与 mid-stream 两路在工具执行时放大到 600s**；mid-stream 在无工具在飞时为 60s。300s 与 600s 均为前端定时器。
- 触发条件：后端事件流停发（后端进程还在，但卡在死锁、阻塞 syscall 或 provider 停滞）。
- 界面表现：没在跑工具时，5 分钟内没有任何新消息气泡或状态变化，对话才被判超时；工具正在执行时，这个窗口拉到 10 分钟，用户全程看不到任何提示。卡死型。
- 证据片段：
```ts
        const IDLE_TIMEOUT_MS = 300_000;
        const STALE_STREAM_TIMEOUT_MS = 300_000;
        const TOOL_STALE_CAP_MS = STALE_STREAM_TIMEOUT_MS * 2;
```
```ts
        const resetIdleTimeout = () => {
          if (ctx.idleTimeout !== null) clearTimeout(ctx.idleTimeout);
          ctx.idleTimeout = setTimeout(() => {
            if (!ctx.doneRef.current) {
              ctx.updatePhase("timeout");
              ctx.settle("", undefined, "timeout", "timeout");
            }
          }, IDLE_TIMEOUT_MS);
        };
```
- 补充：审批卡片弹出时 idle/stale 定时器被清掉（`apps/desktop-tauri/src/hooks/useStreamParser.ts:708-709`），审批等待期间不会触发 300s 超时。

### 3. 审批等待：默认 600s 按可答复时间计费，无人在场墙钟硬顶 1800s
- 位置：`crates/hermix-config/src/config.rs:819`（`APPROVAL_TIMEOUT_SECS = AtomicU64::new(600)`）；`crates/hermix-server/src/approval.rs:207-241`（超时任务按「可答复时间」计费 + `HARD_CAP = 30 * 60`）、`:59`（`ApprovalRegistry::new` 读 `hermix_config::approval_timeout_secs()`）；`crates/hermix-agent/src/tool_dispatch.rs:473-480`（`approval_wait_bounds` 算兜底 net）
- 等待时长口径：默认超时值 + presence 计费。**默认 600s 是「可答复时间」计费**（presence 探针探到无人在线时暂停计费，`approval.rs:207-241` 只有 `seen()` 为真才累加 `visible`）；**无人在场时墙钟硬顶 1800s（30 分钟）**；**24h（86400s）只是 `tool_dispatch` 侧兜底 net**（primary 被显式配成 0 时 `approval_wait_bounds` 返回 86400），不是服务器端的等待口径。
- 触发条件：工具执行前需要用户点确认，且用户迟迟不点。
- 界面表现：工具卡片一直显示「等待审批」，用户不点就一直不往下走；人离开电脑时最长挂 30 分钟才算这次审批失败。卡死型。
- 证据片段：
```rust
            tokio::spawn(async move {
                // 无人在场时的绝对上限：不能因为有「暂时没人」就真挂住 agent。
                const HARD_CAP: std::time::Duration = std::time::Duration::from_secs(30 * 60);
                // 小超时（单测）要够快，分钟级超时不必一秒多探。
                let tick = (to / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
                let mut waited = Duration::ZERO; // 墙钟
                let mut visible = Duration::ZERO; // 有人可答复的时间
```
```rust
pub(crate) fn approval_wait_bounds(primary_secs: u64) -> (u64, u64) {
    let net_secs = if primary_secs == 0 {
        86_400
    } else {
        primary_secs.saturating_add(60)
    };
    (primary_secs, net_secs)
}
```

### 4. stdio 桥 REST 请求：120s 默认预算
- 位置：`apps/desktop-tauri/src-tauri/src/stdio_bridge.rs:272`（`http_request` 固定 120s）、`:306`（`tokio::time::timeout(budget, rx)` 包裹 oneshot 接收）
- 等待时长口径：默认超时值。`http_request` 固定 120s；超时分支清理 `pending_http` 挂起项后返回错误。
- 触发条件：前端任何走 stdio 桥的 REST 调用（后端卡住但进程还在）。
- 界面表现：点了某个要后端数据的功能后，按钮转圈最长 2 分钟才返回「backend request timed out」。卡死型（有上限，但 120s 内界面无反馈）。
- 证据片段：
```rust
        self.request_with_budget(method, path, body, std::time::Duration::from_secs(120))
            .await
```
```rust
        match tokio::time::timeout(budget, rx).await {
```

---

## 二、只是慢（上限明确、期间有反馈）

### 5. 后端就绪轮询：90s 预算 / 1s 间隔
- 位置：`apps/desktop-tauri/src/api/rest.ts:1424`（`waitForBackendReady`）、`:1422`（`timeoutMs` 默认 90_000）、`:1423`（`intervalMs` 默认 1_000）
- 等待时长口径：轮询间隔 + 总预算。90s 总预算、1s 一次探测；按 1s 间隔推算约 90 次探测（推算，未实测）。
- 触发条件：首屏 hydration 请求前等 sidecar 起来（冷启动）。
- 界面表现：冷启动时首屏数据最多推迟 90 秒才发出，这期间连接指示灯一直显示 connecting（`apps/desktop-tauri/src/hooks/useBackendConnection.ts:31` `BOOT_GRACE_MS = 90_000`）。只是慢。
- 证据片段：
```ts
  const timeoutMs = opts.timeoutMs ?? 90_000;
  const intervalMs = opts.intervalMs ?? 1_000;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await checkBackendHealth()) return true;
    await new Promise((r) => setTimeout(r, intervalMs));
  }
```

### 6. 后端重启探测：90s 预算 / 1s 间隔；在线探测 6s；连续失败 2 次判离线；断线静置 8s 自动重启；connecting 满 22s 亮重启入口
- 位置：`apps/desktop-tauri/src/hooks/useBackendConnection.ts:12`（`RESTART_PROBE_BUDGET_MS` = 90_000）、`:13`（`RESTART_PROBE_INTERVAL_MS` = 1_000）、`:22`（`PROBE_INTERVAL_MS` = 6_000）、`:31`（`BOOT_GRACE_MS` = 90_000）、`:36`（`OFFLINE_FAILURE_THRESHOLD` = 2）、`:39`（`AUTO_RESTART_DELAY_MS` = 8_000）、`:44`（`CONNECT_STALLED_MS` = 22_000）
- 等待时长口径：轮询间隔 + 总预算 + 阈值。重启后 90s 内每 1s 探一次；在线态 6s 一探；连续失败 2 次判离线；断线静置 8s 再自动重启；connecting 满 22s 亮出重启入口（只提前亮入口，不改判 offline，90s 冷启动宽限继续生效）。
- 触发条件：后端重启 / 崩溃 / 冷启动。
- 界面表现：后端挂掉时左下角指示灯先转离线、再转「重启中」，最长 90 秒后转「重启失败」并给出重启按钮。只是慢。
- 证据片段：
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
```ts
const OFFLINE_FAILURE_THRESHOLD = 2;
```
```ts
const AUTO_RESTART_DELAY_MS = 8_000;
```
```ts
const CONNECT_STALLED_MS = 22_000;
```

### 7. sidecar 就绪等待：30s deadline / 500ms 轮询，每次探针 3s
- 位置：`apps/desktop-tauri/src-tauri/src/sidecar.rs:403`（`wait_until_ready(deadline)`）、`:413`（探针预算 3s）、`:434`（sleep 500ms）；30s 来自调用点 `apps/desktop-tauri/src-tauri/src/lib.rs:1054`（`wait_until_ready(Duration::from_secs(30))`）
- 等待时长口径：轮询间隔 + 总预算。deadline 30s，每 500ms 探一次 `/health`，单次探针预算 3s，最多约 60 次（推算，未实测）。
- 触发条件：sidecar 启动后等服务可应答。
- 界面表现：启动页最多等 30 秒才继续渲染，随后按未就绪处理。只是慢。
- 证据片段：
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

### 8. 成员评审终态等待：600s 真实时钟 deadline / 500ms 轮询
- 位置：`crates/hermix-agent/src/member/runner.rs:695`（deadline 600s）、`:704`（返回 `review_timeout`）、`:710`（sleep 500ms）
- 等待时长口径：轮询间隔 + 总预算。真实时钟 deadline 600s，每 500ms 探一次，最多约 1200 次（推算，未实测）。
- 触发条件：派单后等 reviewer 出终态（评审通常 1-5 分钟）。
- 界面表现：任务停在「评审中」最多 10 分钟，到点提示评审超时并可稍后取结果。只是慢。
- 证据片段：
```rust
        // 等评审终态（真实时钟 deadline；评审通常 1-5 分钟）。
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
```
```rust
            tokio::time::sleep(Duration::from_millis(500)).await;
```

### 9. MCP 客户端（stdio 传输）：请求 120s、initialize 握手 15s
- 位置：`crates/hermix-mcp/src/client.rs:738`（`send_request` 固定 120s）、`:772`（`tokio::time::timeout(timeout, transport.receive())`）、`:852-858`（initialize 握手 15s）；握手失败换帧重试 `client.rs:353-357`（遍历 Jsonl / ContentLength）
- 等待时长口径：默认超时值。stdio 传输下常规请求上限 120s，initialize 握手上限 15s。
- 触发条件：调用 stdio 方式的 MCP server；握手不响应时换帧格式重试。
- 界面表现：MCP 工具调用最长挂 2 分钟才返回超时；握手阶段最长 15 秒。只是慢。
- 证据片段：
```rust
        self.send_request_with_timeout(method, params, std::time::Duration::from_secs(120))
            .await
```
```rust
                std::time::Duration::from_secs(15),
```

### 9b. MCP 客户端（HTTP 传输 / StreamableHttpTransport）：connect 10s、总请求 120s
- 位置：`crates/hermix-mcp/src/transport.rs:707`（`.connect_timeout(Duration::from_secs(10))`）、`:708`（`.timeout(Duration::from_secs(120))`），位于 `:704-710` 的 `StreamableHttpTransport::new` reqwest client 构造
- 等待时长口径：默认超时值。**connect 10s + 单请求总时长 120s，这组数值只对 StreamableHttpTransport（HTTP 传输）成立**，stdio 传输没有这层预算，其上限来自 `client.rs:772` 的 120s receive 超时。
- 触发条件：通过 HTTP 方式访问的 MCP server 建连或响应。
- 界面表现：走 HTTP 的 MCP 工具调用连不上时 10 秒就报错，能连上但不回数据时挂 2 分钟。只是慢。
- 证据片段：
```rust
    pub fn new(url: &str, auth_token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
```

### 10. LLM HTTP 客户端：connect 10s / 总请求 180s
- 位置：`crates/hermix-agent/src/provider.rs:40`（connect 10s）、`:41`（总 180s）；同型 `crates/hermix-agent/src/api_bridge.rs:41-42`
- 等待时长口径：默认超时值。connect 10s、单次请求总时长 180s。`provider.rs:28-37` 的注释自述此前 `Client::new` 无 connect timeout 时，半开连接可挂到 75s（Linux）或无限（Windows）——**注释自述，未实测**。
- 触发条件：调用 LLM provider 发 HTTP 请求。
- 界面表现：provider 不通时，连接阶段 10 秒就失败；连上了不回数据时最长 180 秒后报超时。只是慢（已封顶）。
- 证据片段：
```rust
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
```

### 11. 本机 Shell 子进程：默认 120s
- 位置：`crates/hermix-core/src/seams/local.rs:110`（`DEFAULT_TIMEOUT = Duration::from_secs(120)`）、`:153`（`tokio::time::timeout(self.timeout, child.wait_with_output())`）
- 等待时长口径：默认超时值。120s；超时后 tokio 只 drop future，Windows 上靠 Job Object 杀整棵树（`local.rs:151-152` 注释自述，**注释自述，未实测**）。
- 触发条件：执行本机命令且命令不退出。
- 界面表现：命令卡住时，用户看着这条命令的输出框空着，最长 2 分钟后才收到超时错误。只是慢。
- 证据片段：
```rust
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
```
```rust
        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
```

### 12. 终端工具默认超时 120s
- 位置：`crates/hermix-config/src/config.rs:758`（`timeout: default_terminal_timeout()`）、`:767`（`default_terminal_timeout` 返回 120）
- 等待时长口径：默认超时值。120s。
- 触发条件：走 terminal backend 执行命令。
- 界面表现：终端调用挂住时，用户看着终端面板不返回输出，最长 2 分钟后报错。只是慢。
- 证据片段：
```rust
fn default_terminal_timeout() -> u64 {
    120
}
```

### 13. cron 调度 tick 60s、文件锁 3s
- 位置：`crates/hermix-cron/src/scheduler.rs:26`（`cron_poll_interval`，默认 60s，clamp 1..300）、`:38`（`default_lock_timeout`，默认 3s，clamp 1..60）
- 等待时长口径：轮询间隔 + 超时值。tick 间隔 60s（环境变量 `HERMIX_CRON_TICK_SECS` / `HERMES_CRON_TICK_SECS` 可调，范围 1..300）；抢锁等待 3s（`HERMIX_CRON_LOCK_TIMEOUT_SECS` / `HERMES_CRON_LOCK_TIMEOUT_SECS`，范围 1..60）。
- 触发条件：cron 调度器运行；写 cron 文件时与其它写入方抢锁。
- 界面表现：定时任务最多比设定时间晚 1 分钟才触发；写 cron 配置时最多等 3 秒拿锁。只是慢。
- 证据片段：
```rust
    let secs = env_compat("HERMIX_CRON_TICK_SECS", "HERMES_CRON_TICK_SECS")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 300);
```
```rust
    let secs = env_compat("HERMIX_CRON_LOCK_TIMEOUT_SECS", "HERMES_CRON_LOCK_TIMEOUT_SECS")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(3)
        .clamp(1, 60);
```

### 14. 更新器：清单请求 15s、下载 600s、检查周期首轮 30s / 之后每 2h
- 位置：`apps/desktop-tauri/src-tauri/src/updater.rs:19`（`REQUEST_TIMEOUT_SECS = 15`）、`:213`（下载 `.timeout(Duration::from_secs(600))`）、`:272`（首轮 sleep 30s）、`:274`（之后 sleep 2×3600s）；进度事件 `:231-237`（每 512KB 推一次 `update://progress`）
- 等待时长口径：默认超时值 + 轮询间隔。清单请求 15s；更新包下载 600s；更新检查首轮等 30s，之后每 2 小时一轮。
- 触发条件：检查更新 / 下载更新包。
- 界面表现：下载更新时进度条按每 512KB 推进，用户看得到进度；卡住时最长 10 分钟超时。只是慢（期间有反馈）。
- 证据片段：
```rust
const REQUEST_TIMEOUT_SECS: u64 = 15;
```
```rust
        .timeout(std::time::Duration::from_secs(600))
```
```rust
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(2 * 3600)).await;
            }
```

### 15. 服务端状态探针：2s 预算
- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:398`（`tokio::time::timeout(Duration::from_secs(2), fut)`）、`:566`（`request_with_budget(..., Duration::from_secs(2))`）
- 等待时长口径：默认超时值。2s。
- 触发条件：前端拉后端状态（健康检查）。
- 界面表现：状态灯最多 2 秒就出结果，超时就显示后端未响应。只是慢。
- 证据片段：
```rust
                    tokio::time::timeout(std::time::Duration::from_secs(2), fut).await,
```

---

## 三、无上限等待（代码里没有 timeout，也没有等价兜底）

### 16. markitdown 子进程：`output().await` 无超时
- 位置：`apps/desktop-tauri/src-tauri/src/markitdown.rs:51`（`tokio::process::Command::new(&bin)`）、`:54`（`.output().await`）；调用点 `apps/desktop-tauri/src-tauri/src/commands.rs:1026`（`crate::markitdown::convert_to_markdown(...)`）
- 等待时长口径：**无上限**。裸等子进程退出，外层没有 `tokio::time::timeout`，也没有等价兜底。
- 触发条件：用户对文件执行「转 Markdown」，且 markitdown 进程不退出（依赖的 python 环境初始化慢、文件巨大、进程挂起）。
- 界面表现：点了「转 Markdown」后按钮一直转圈，这个调用永远不返回，用户只能重启应用。卡死型。
- 证据片段：
```rust
    let output = tokio::process::Command::new(&bin)
        .args(&args)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW: 防黑窗闪烁
        .output()
        .await
```

### 17. 运行时版本探测子进程：`output().await` 无超时
- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:1337`（`TokioCommand::new(exe)`）、`:1343`（`cmd.output().await.ok()?`）；调用点 `runtime_detect` 命令 `commands.rs:1418` → `:1465` `spawn_version(...)`
- 等待时长口径：**无上限**。函数上方注释写「Spawn `<python> --version` with a 2s timeout」，但函数体里没有 timeout 实现——注释与代码不符，实际无上限。修复方向（建议，非本次改动）：给 `spawn_version` 套 `tokio::time::timeout(Duration::from_secs(2), cmd.output())`。
- 触发条件：设置页探测 node / git / python 版本，被探测的可执行文件启动挂起（网络盘上的 exe、杀软扫描卡住）。
- 界面表现：打开设置页的运行时检测时，那几行版本号一直空白，不报错也不结束。卡死型。
- 证据片段：
```rust
/// Spawn `<python> --version` with a 2s timeout and capture the version
/// string. Returns `None` on any failure (timeout / non-zero exit / IO
/// error) — the caller treats that as "exists but didn't return a
/// version", which still beats nothing.
```
```rust
    let output = cmd.output().await.ok()?;
```

### 18. reg.exe 同步子进程探测 WebView2：无超时
- 位置：`apps/desktop-tauri/src-tauri/src/webview2_probe.rs:125`（第一次 `Command::new("reg")` + `.output()`）、`:152`（第二次注册表视图，同样 `.output()`）；调用点 `apps/desktop-tauri/src-tauri/src/lib.rs:911`（`match webview2_probe::probe() {`），位于 Tauri `setup` 回调内，与 sidecar 启动同一函数体
- 等待时长口径：**无上限**。同步 `std::process::Command::output()`，无 timeout；缓存由 `AtomicU8` 保证每进程只跑一次（`webview2_probe.rs:90-93`）。
- 触发条件：应用启动探测 WebView2 运行时（缓存失效时重新探测）。
- 界面表现：应用启动时窗口是空的、没有反应，`reg.exe` 挂住就一直停在这个画面（同步阻塞启动流程；「Tauri setup 在主线程执行」属框架行为推断，仓库内无代码可证，未实测）。
- 证据片段：
```rust
            let output = Command::new("reg")
```
```rust
                .output();
```
```rust
            match webview2_probe::probe() {
                webview2_probe::WebView2Status::Missing => {
                    eprintln!("{}", webview2_probe::explain_missing_runtime());
```

---

## 四、未核对 / 待核检索面命中

以下 grep 命中**未**逐条 read_file 核对行号与机制，**不编号、不计入 18 条编号条目**，仅作后续检索线索：

- `crates/hermix-agent/src/llm_retry.rs:295-318`（重试退避：`parse_retry_after_ms` + `jittered_backoff(attempt, base, max_delay)`，sleep 实际时长待核；`max_retries` 上限与总耗时未核）。
- `crates/hermix-agent/src/session_persistence/sessions.rs:259`（SQLITE_BUSY 重试：`WRITE_MAX_RETRIES` 次 + jitter sleep，次数与总预算未核）。
- `crates/hermix-agent/src/tool_executor.rs:910` / `:1013` / `:1093`（工具调用超时与 drain grace 5s，数值未核）。
- `crates/hermix-agent/src/compression/mod.rs:506-516`（`SUMMARY_FAILURE_COOLDOWN` 600s、`SUMMARY_LLM_TIMEOUT` 60s、链式 failover 未核）。
- `crates/hermix-agent/src/member/runner.rs:811`（`MEMBER_DEFAULT_TIMEOUT_SECS` 未核）。
- `crates/hermix-agent/src/sub_agent_orchestrator.rs:423`（`DEFAULT_SUB_AGENT_TIMEOUT_SECS` 未核）。
- `crates/hermix-agent/src/agent_config.rs` 内 `turn_transition_deadline_secs` 默认值（`:924` 附近）未核。
- `crates/hermix-agent/src/copilot_acp.rs:56-58`（`request_timeout` 60s、`reconnect_delay` 2s 未核）。
- `crates/hermix-agent/src/memory_prefetch.rs:58-60`（3s / 5s，已见 grep 数值，未 read_file 核对）。
- `crates/hermix-agent/src/rate_limit.rs`（Retry-After 解析，多处）。
- `crates/hermix-core/src/win_job.rs:336`、`crates/hermix-cron/src/file_lock.rs:156`（忙等 sleep，未核对）。
- `crates/hermix-server/src/approval.rs:156-160`、`crates/hermix-server/src/ws/mod.rs:573/618/685`（`rx.recv().await` 无 timeout 的通道等待，未核对是否属于后台任务）。
- `crates/hermix-filesearch/src/service.rs:518`（`VOLUME_ENUM_TIMEOUT` 未核）。
- `crates/hermix-plugin/src/hx_packager.rs:492`（30s recv_timeout 未核）。
- `crates/hermix-environments/src/local.rs:850`、`:956`、`:1025`（子进程 timeout 数值未核）。
- `crates/hermix-environments/src/docker.rs:153`（docker 命令 timeout 未核）。
- `crates/hermix-intelligence/src/auxiliary/client.rs:207`、`crates/hermix-intelligence/src/models_dev/client.rs:115`、`crates/hermix-intelligence/src/model_directory.rs:623`（HTTP timeout 未核）。
- `apps/desktop-tauri/src/hooks/useApprovalBridge.ts:34-44`（retry/timeout/rescue 定时器数值未核）。
- `apps/desktop-tauri/src/hooks/usePolling.ts:35`、`apps/desktop-tauri/src/hooks/useCustomApps.ts:47`（轮询间隔与退避未核）。
- `apps/desktop-tauri/src/api/rest.ts` 的 `fetchJSON` 是否带 AbortController / 超时未核（grep 命中 `AbortController` 0 处，倾向无前端请求超时，未定论）。
- `apps/desktop-tauri/src-tauri/src/commands.rs:723` / `:812`（同步整文件读 + base64）属于阻塞维度，与本文件的时间维度不重叠，未重复收录。
- `apps/desktop-tauri/src-tauri/src/cdp_probe.rs:36/63`（30s recv_timeout）、`apps/desktop-tauri/src-tauri/src/shutdown.rs:56/100-110`（2s 文件搜索退出等待）、`apps/desktop-tauri/src-tauri/src/screenshot_controller.rs:301/327`（session fallback / 2s deadline 轮询 30ms）未核。

---

## 五、未实测与缺证

- 本轮**未启动应用、未跑测试、未跑构建**，没有任何实测数据。所有秒/毫秒数值来自源码字面量或源码注释。
- 源码注释自述的数值，文中标「注释自述，未实测」：
  - `crates/hermix-agent/src/provider.rs:28-37`：此前 `Client::new` 无 connect timeout 时，半开连接 Linux 75s / Windows 无限。
  - `crates/hermix-core/src/seams/local.rs:151-152`：Windows 上靠 Job Object 杀整棵进程树。
  - `crates/hermix-agent/src/commands.rs:1322-1325`：`spawn_version` 注释写「2s timeout」，实现里没有对应 timeout（注释与代码不符，已按实现记为无上限）。
- 属框架行为或运行时语义、仓库内无代码可证的判断，一律标「推测 / 未实测」：
  - 条目 18 的「Tauri setup 在主线程执行」——源码只能证明 `lib.rs:911` 的调用点在 `setup` 回调内且是同步调用；主线程归属属框架行为推断。
  - 条目 2 的「前端定时器窗口」——浏览器在后台标签页会节流定时器，实际收口可能更晚，未实测。
  - 条目 3 的「presence 计时与 24h 兜底 net 的竞争关系」——运行时竞态，未实测。
  - 条目 16 / 17 的「界面一直转圈」——Tauri 异步命令调度与前端 loading 态的对应关系属框架行为，未实测。
  - 条目 18 的「`reg.exe` 实际挂起概率与启动期观感」——未实测。
- 推算而非实测的数字：条目 5「约 90 次探测」、条目 7「约 60 次」、条目 8「约 1200 次」——均由「预算 ÷ 间隔」推算得出。
- 第四部分列出的未核对命中本轮**未逐条回源**，不计入 18 条编号条目；其中 `fetchJSON` 无 AbortController 一项维持「未定论」。

---

## 六、检索面命中统计

- a. 默认超时值：条目 1、2、4、9、9b、10、11、12、14、15（其中 14 兼含轮询）。
- b. 轮询与间隔：条目 5、6、7、8、13、14。
- c. 重试退避：未核对命中，暂不计入（见第四部分）。
- d. 无上限等待：条目 16、17、18。
- e. 外部依赖等待（子进程与网络）：条目 9、9b、10、11、16、17、18。

编号条目 18 条，本轮按修正后的表述全部成立，不成立 0 条。另有 3 条初判「无上限」的项（stdio 桥 oneshot、MCP transport receive、本机 shell 子进程）经回源确认各有 120s / 120s / 120s 上限，已改判并入条目 4、9、10、11，不再单列编号条目。
