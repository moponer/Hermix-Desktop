# 等待与超时清单独立复核结论（hermix-desktop）

复核对象：`docs/wait-timeout-audit.md`（实际 365 行）。
复核方式：`read_file` / `grep` 回源逐条核对路径、行号与机制。本轮未启动应用、未跑测试、未跑构建，无任何实测。
所有耗时数值来自源码字面量或源码注释，**源码注释自述，未实测**。

## 一、实际条数与口径差异

主清单正文第一至第三部分共 **18 个编号条目**（条目 1–18），不是 21 条。
主清单开头写「条目 1、2、3、16、17、18 已独立复核；条目 4~15 待复核，暂不可对外引用条数」，第六部分却写「成立合计 18 条」，两处自相矛盾。
21 是旧口径（18 成立 + 3 不成立）。文件把 3 条初判「不成立」的项（stdio 桥 oneshot、MCP transport receive、本机 shell 子进程）改判并入其它条目后已无独立编号，但仍以「成立 18 条」表述覆盖全部 18 个编号条目。
**修正要求**：对外条数只能写「编号条目 18 条」，不得写「成立 18 条」；3 条改判并入项应保留独立条目号或单列一节说明改判过程。

## 二、判定汇总表

| # | 主清单标题 | 判定 | 修正要点 |
|---|---|---|---|
| 1 | LLM 流式响应：外层兜底 180s | 需修正 | 180s 执行点是 stream_collect.rs:353/371，不是 :351-353；首 chunk 期限在 :515-516 生效但**首 chunk 未到是提前收口**（finish_reason="no_first_chunk"），不是等满 30s |
| 2 | 前端流式看门狗：idle/stale 300s | 需修正 | IDLE_TIMEOUT_MS 是**固定 300s**（:648），不随工具在飞放大；600s 只有 stale 与 mid-stream 两路 |
| 3 | 审批等待：净等待 660s | 需修正 | 净等待计算成立，但服务器端按「在场可答复时间」计费 + 30min 硬顶（approval.rs:207-241），不是墙钟 660s |
| 4 | stdio 桥 REST：120s 预算 | 成立 | 行号与机制一致 |
| 5 | 后端就绪轮询：90s / 1s | 成立 | 行号与机制一致 |
| 6 | 后端重启探测：90s / 1s、在线 6s | 需修正 | 常量行号整体偏移（12/13/22/31/39/44，不是 13/14/25/33/43/48） |
| 7 | sidecar 就绪：30s / 500ms、探针 3s | 成立 | 30s 来自调用点 lib.rs:1054，非 sidecar.rs 内部 |
| 8 | 成员评审终态：600s / 500ms | 成立 | 行号与机制一致 |
| 9 | MCP 客户端：120s / 握手 15s | 需修正 | transport.rs 的 10s/120s 属 **StreamableHttpTransport**，stdio 传输无此预算 |
| 10 | LLM HTTP 客户端：connect 10s / 总 180s | 成立 | 行号与机制一致 |
| 11 | 本机 Shell 子进程：默认 120s | 成立 | 行号与机制一致 |
| 12 | 终端工具默认超时 120s | 成立 | 行号与机制一致 |
| 13 | cron tick 60s、文件锁 3s | 成立 | 行号与机制一致 |
| 14 | 更新器：15s / 600s / 30s 起每 2h | 成立 | 行号与机制一致 |
| 15 | 服务端状态探针：2s | 成立 | 行号与机制一致 |
| 16 | markitdown 子进程：无超时 | 成立 | 无 timeout 包裹；调用点 commands.rs:1026 |
| 17 | 运行时版本探测：无超时 | 成立 | 注释与实现不符成立；调用点 runtime_detect 命令 |
| 18 | reg.exe 探测 WebView2：无超时 | 需修正 | 无超时成立，但**调用点在 Tauri setup 主线程**（lib.rs:911），同步阻塞启动；主清单自述「未核到调用点与线程归属」 |

判定分布：成立 13 条｜需修正 5 条｜不成立 0 条。

## 三、逐条明细

### 1. LLM 流式响应：外层兜底超时 180s —— 需修正
默认值成立：`agent_config.rs:903` 返回 180、`:912` 返回 30、`:918` 返回 120。
偏差一：180s 看门狗执行点在 `stream_collect.rs:353`（读 `config.stream_overall_timeout_secs`）与 `:371`（`tokio::time::timeout(dur, inner_fut)`），`:368-370` 有 `overall_secs == 0` 直接 `return inner_fut.await` 的关闭分支——主清单称 180s 可配 0 关闭，成立。
偏差二：**首 chunk 30s 不是「等满 30s 再走下一层」**。`stream_collect.rs:713-731` 在首 chunk 未到且 elapsed ≥ first_chunk_deadline 时直接 `break`，并打上 `finish_reason = "no_first_chunk"`（`:729`）。所以 provider 开 SSE 不推 chunk 时，收口点是 30s 的**提前 abort**，不是外层 180s 兜底；主清单「两层都没收敛 → 由外层兜底 180s 收口」与实现不符。
证据（`stream_collect.rs:713-731`）：
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
证据（`stream_collect.rs:366-371`）：
```rust
    if overall_secs == 0 {
        return inner_fut.await;
    }
    let dur = std::time::Duration::from_secs(overall_secs);
    match tokio::time::timeout(dur, inner_fut).await {
```
修正要求：首 chunk 档改写为「30s 未到首 chunk 即提前 abort（no_first_chunk），不落到 180s 兜底」；180s 兜底仅覆盖已进入流式但整体超时的情形。

### 2. 前端流式看门狗：idle / stale 300s，工具在飞 600s —— 需修正
四个常量行号成立（`useStreamChat.ts:608/609/610/618`）。
偏差：**IDLE_TIMEOUT_MS 是固定 300s，不随工具在飞放大**。`resetIdleTimeout`（:641-650）直接用 `IDLE_TIMEOUT_MS` 常量，没有 `toolRunningRef` 分支；只有 `resetStaleTimer`（:651-660）与 `resetMidStreamSilenceTimer`（:621-640）按 `ctx.toolRunningRef.current` 选 600s/60s。主清单称「工具运行中对话最长 600s 无任何事件才判超时」——600s 成立，但「idle 与 stale 各 300s；工具执行中放大到 600s」的写法会被读成三条都放大。
证据（`useStreamChat.ts:641-656`）：
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
        const resetStaleTimer = () => {
          if (ctx.staleStreamTimer !== null) clearTimeout(ctx.staleStreamTimer);
          const timeout = ctx.toolRunningRef.current
            ? TOOL_STALE_CAP_MS
            : STALE_STREAM_TIMEOUT_MS;
```
补充旁证（主清单未提，属影响面修正）：审批卡片弹出时 idle/stale 定时器被清掉（`useStreamParser.ts:708-709`），审批等待期间不会触发 300s 超时——这使条目 3 的 660s 审批等待在正常路径上不被前端 300s 看门狗打断。
修正要求：改写为「idle 固定 300s；stale 与 mid-stream 在工具在飞时放大到 600s」。

### 3. 审批等待：默认档净等待 660s —— 需修正
净等待计算成立：`tool_dispatch.rs:473-479`，`primary==0 → 86_400`，否则 `primary+60`；`config.rs:819` 默认 600；`approval.rs:59` 读 `hermix_config::approval_timeout_secs()`。
偏差：**主层的 600s 不是墙钟 600s**。`ApprovalRegistry::new()`（`approval.rs:58-61`）带 presence 探针，`request_approval` 的超时任务（`approval.rs:207-241`）按「可答复时间」计费：只有 `seen()` 为真才累加 `visible`，同时有 `HARD_CAP = 30 * 60`（1800s）绝对上限。即无人在线时，实际等待是 1800s 墙钟而非 660s。
证据（`approval.rs:207-241`）：
```rust
            tokio::spawn(async move {
                // 无人在场时的绝对上限：不能因为有「暂时没人」就真挂住 agent。
                const HARD_CAP: std::time::Duration = std::time::Duration::from_secs(30 * 60);
                // 小超时（单测）要够快，分钟级超时不必一秒多探。
                let tick = (to / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
                let mut waited = Duration::ZERO; // 墙钟
                let mut visible = Duration::ZERO; // 有人可答复的时间
```
偏差二：主清单称「配 0 时落 24h 档」——`approval_wait_bounds(0)` 返回 net 86_400 成立，但该 net 只是 `tool_dispatch` 侧兜底 select 的 sleep（`tool_dispatch.rs:244/250`），与服务器端 presence 计时是两套。
修正要求：改写为「默认 600s 为可答复时间计费（presence 暂停），无人在线时墙钟上限 1800s；24h 仅为 tool_dispatch 兜底 net」。

### 4. stdio 桥 REST 请求：120s 默认预算 —— 成立
`stdio_bridge.rs:272` 固定 120s、`:306` 用 `tokio::time::timeout(budget, rx)` 包裹，超时分支清理 `pending_http`。行号与机制均一致。
证据（`stdio_bridge.rs:270-273`）：
```rust
    ) -> Result<String, String> {
        self.request_with_budget(method, path, body, std::time::Duration::from_secs(120))
            .await
    }
```
证据（`stdio_bridge.rs:306-309`）：
```rust
        match tokio::time::timeout(budget, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err("http waiter dropped".into()),
            Err(_) => {
```

### 5. 后端就绪轮询：90s / 1s —— 成立
`rest.ts:1419` 定义 `waitForBackendReady`，`:1422` 默认 90_000ms、`:1423` 默认 1_000ms，`:1425` 用 `Date.now() + timeoutMs` 算 deadline。主清单称「最多约 90 次探测」——按 1s 间隔与 90s 预算推算，未实测，标注为推算而非实测。
证据（`rest.ts:1422-1429`）：
```ts
  const timeoutMs = opts.timeoutMs ?? 90_000;
  const intervalMs = opts.intervalMs ?? 1_000;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await checkBackendHealth()) return true;
    await new Promise((r) => setTimeout(r, intervalMs));
  }
```

### 6. 后端重启探测：90s / 1s；在线探测 6s —— 需修正
机制成立：重启探针预算 90s、间隔 1s（`:207`/`:209` 与 `:243`/`:245`），在线探针 6s（`:167` `usePolling(() => check(), PROBE_INTERVAL_MS)`），连续失败 2 次判离线（`:36`、`:136`），断线静置 8s 自动重启（`:39`、`:285`），connecting 满 22s 亮重启入口（`:44`、`:271`）。
偏差：**全部常量行号偏移**。实际行号：`RESTART_PROBE_BUDGET_MS`=`:12`、`RESTART_PROBE_INTERVAL_MS`=`:13`、`PROBE_INTERVAL_MS`=`:22`、`BOOT_GRACE_MS`=`:31`、`OFFLINE_FAILURE_THRESHOLD`=`:36`、`AUTO_RESTART_DELAY_MS`=`:39`、`CONNECT_STALLED_MS`=`:44`。主清单写的是 13/14/25/33/43/48（且未列 OFFLINE_FAILURE_THRESHOLD），逐条错位 1~4 行。
证据（`useBackendConnection.ts:12-13`）：
```ts
const RESTART_PROBE_BUDGET_MS = 90_000;
const RESTART_PROBE_INTERVAL_MS = 1_000;
```
证据（`useBackendConnection.ts:39-44`）：
```ts
const AUTO_RESTART_DELAY_MS = 8_000;
/** 冷启动失速阈值：splash 在 ~22s 撤掉，此后 connecting 状态对用户没有任何
 *  出口（标题栏的重启入口只在 offline/gaveUp 显示）。与 splash 兜底对齐，
 *  到点把 connectStalled 置位 —— 只提前亮出重启入口，不改判 offline
 *  （90s 冷启动宽限继续生效，慢启动不会被误杀）。 */
const CONNECT_STALLED_MS = 22_000;
```
修正要求：按上列实际行号改写位置栏。

### 7. sidecar 就绪等待：30s / 500ms，每次探针 3s —— 成立
`sidecar.rs:403` `wait_until_ready(deadline)`、`:413` 探针预算 3s、`:434` sleep 500ms 均成立。30s 来自调用点 `lib.rs:1054`（`wait_until_ready(Duration::from_secs(30))`），主清单标 `:403` 为位置并把调用点写为 `lib.rs:1054`，此处成立。
证据（`lib.rs:1053-1054`）：
```rust
                    let ready =
                        HermixSidecar::wait_until_ready(std::time::Duration::from_secs(30)).await;
```
证据（`sidecar.rs:412-414`）：
```rust
                let probe = bridge
                    .request_with_budget("GET", "/health", None, Duration::from_secs(3))
                    .await;
```

### 8. 成员评审终态等待：600s / 500ms —— 成立
`member/runner.rs:695` deadline 600s、`:704` 返回 `review_timeout`、`:710` sleep 500ms。成立。
证据（`member/runner.rs:694-696`）：
```rust
        // 等评审终态（真实时钟 deadline；评审通常 1-5 分钟）。
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        let mut reviewer_snapshot = self.get(&reviewer_id).await;
```
证据（`member/runner.rs:702-706`）：
```rust
                    "verdict": "review_timeout",
                    "reviewer_member_id": reviewer_id,
                    "note": "评审 10 分钟未到终态。稍后用 get_member_result 查 reviewer 结果并重试验收。",
```

### 9. MCP 客户端：常规 120s、握手 15s；HTTP 传输 connect 10s / 总 120s —— 需修正
`client.rs:738` 常规请求 120s、`:772` `tokio::time::timeout(timeout, transport.receive())`、`:852-858` initialize 握手 15s，均成立。握手失败换帧重试成立（`client.rs:353-357` 遍历 Jsonl / ContentLength）。
偏差：**`transport.rs:707-708` 的 connect 10s / 总 120s 是 `StreamableHttpTransport` 的 reqwest client 构造**（`:704-710`），属于 HTTP 传输路径；stdio 传输没有这层预算，其上限来自 `client.rs:772` 的 120s receive 超时。主清单把两者并列写成「MCP 客户端」统一数值，未区分传输类型。
证据（`transport.rs:704-710`）：
```rust
    pub fn new(url: &str, auth_token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
```
证据（`client.rs:737-739`）：
```rust
    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        self.send_request_with_timeout(method, params, std::time::Duration::from_secs(120))
            .await
    }
```
修正要求：拆成「stdio 传输：请求 120s、握手 15s」与「HTTP 传输：connect 10s、总 120s」两行，标注 transport.rs 的数值只对 StreamableHttpTransport 成立。

### 10. LLM HTTP 客户端：connect 10s / 总 180s —— 成立
`provider.rs:40-41` 成立，`api_bridge.rs:41-42` 同型成立。主清单引的注释（半开连接 Linux 75s / Windows 无限）确在 `provider.rs:28-37`，属**源码注释自述，未实测**。
证据（`provider.rs:39-42`）：
```rust
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .build()
```

### 11. 本机 Shell 子进程：默认 120s —— 成立
`local.rs:110` `DEFAULT_TIMEOUT = Duration::from_secs(120)`、`:153` `tokio::time::timeout(self.timeout, child.wait_with_output())`。主清单称「Windows 上靠 Job Object 杀整棵树」——代码注释在 `:151-152` 说明 attach JobGuard，属**源码注释自述，未实测**。
证据（`local.rs:110` 与 `:151-153`）：
```rust
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
```
```rust
        #[cfg(windows)]
        let job = child.id().and_then(crate::win_job::JobGuard::attach);
        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
```

### 12. 终端工具默认超时 120s —— 成立
`config.rs:758` `timeout: default_terminal_timeout()`、`:767-769` `default_terminal_timeout()` 返回 120。成立。
证据（`config.rs:767-769`）：
```rust
fn default_terminal_timeout() -> u64 {
    120
}
```

### 13. cron 调度 tick 60s、文件锁 3s —— 成立
`scheduler.rs:26` `cron_poll_interval`（默认 60、clamp 1..300）、`:38` `default_lock_timeout`（默认 3、clamp 1..60）。成立。
证据（`scheduler.rs:26-33`）：
```rust
fn cron_poll_interval() -> Duration {
    let secs = env_compat("HERMIX_CRON_TICK_SECS", "HERMES_CRON_TICK_SECS")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 300);
    Duration::from_secs(secs)
}
```

### 14. 更新器：清单 15s、下载 600s、检查 30s 起 / 每 2h —— 成立
`updater.rs:19` REQUEST_TIMEOUT_SECS=15（用于 `:98` 的 reqwest timeout）、`:213` 下载 600s、`:272` 首查 30s、`:274` 每轮 2×3600s。进度事件每 512KB 一次成立（`:231-237`，`update://progress`）。成立。
证据（`updater.rs:270-275`）：
```rust
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(2 * 3600)).await;
            }
```

### 15. 服务端状态探针：2s 预算 —— 成立
`commands.rs:398` `tokio::time::timeout(Duration::from_secs(2), fut)`、`:566` `request_with_budget(..., Duration::from_secs(2))`，均成立。
证据（`commands.rs:396-400`）：
```rust
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(2), fut).await,
                    Ok(Ok(reply)) if reply.contains("\"status\":\"ok\"")
                )
```

### 16. markitdown 子进程：`output().await` 无超时 —— 成立
`markitdown.rs:51-54` 裸 `tokio::process::Command::new(&bin).output().await`，外层无 `tokio::time::timeout`。主清单称「已核到调用为 async fn（markitdown.rs:41-44）」成立；调用点为 `commands.rs:1026`（`crate::markitdown::convert_to_markdown(...)`），主清单未列。成立。
证据（`markitdown.rs:51-56`）：
```rust
    let output = tokio::process::Command::new(&bin)
        .args(&args)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW: 防黑窗闪烁
        .output()
        .await
        .map_err(|e| format!("failed to run markitdown: {e}"))?;
```

### 17. 运行时版本探测子进程：无超时 —— 成立
`commands.rs:1337` `TokioCommand::new(exe)`、`:1343` `cmd.output().await.ok()?`，无 timeout 包裹成立。注释「with a 2s timeout」在 `:1322-1325`，函数体无对应实现，注释与代码不符成立。调用点：`runtime_detect` 命令（`:1418`）经 `:1465` `spawn_version(...)` 调用。成立。
证据（`commands.rs:1340-1343`）：
```rust
    cmd.arg(flag)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = cmd.output().await.ok()?;
```

### 18. reg.exe 同步探测 WebView2：无超时 —— 需修正
无超时成立：`webview2_probe.rs:125-130` 与 `:152-156` 都是同步 `std::process::Command::output()`，无超时。缓存由 `AtomicU8` 保证每进程只跑一次（`:90-93`）。
偏差：**主清单自述「未核到调用点与线程归属」，本次已核到**——`lib.rs:911` `match webview2_probe::probe() {` 位于 Tauri `setup` 回调内，与 sidecar 启动同一函数体，是**同步阻塞在启动主线程**上。因此「启动阶段窗口空白无响应」有源码依据（不是推测）。
证据（`webview2_probe.rs:124-130`）：
```rust
            let output = Command::new("reg")
                .args(["query", &key, "/v", "pv"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .output();
```
证据（`lib.rs:911-913`）：
```rust
            match webview2_probe::probe() {
                webview2_probe::WebView2Status::Missing => {
                    eprintln!("{}", webview2_probe::explain_missing_runtime());
```
修正要求：把「未核到调用点与线程归属」改为「已核：调用点 `lib.rs:911`，在 Tauri setup 回调内同步执行，阻塞启动主线程」；「窗口空白无响应」由推测升级为有源码依据。

## 四、不成立项清单

**无。** 18 个编号条目中没有一条的核心主张被源码推翻：13 条成立、5 条需修正（偏差均为行号错、机制错、线程归属未核，均已在上文写明应改成什么）。
主清单第六部分「成立合计 18 条」这条汇总结论本身不成立（应为「编号条目 18 条，成立 13 条、需修正 5 条」），已列在第一节。

## 五、推测项清单

以下判断属框架行为或运行时语义，仓库内只能给出旁证，**不作为已证事实**：

1. 条目 1「外层 180s 看门狗可被配置为 0 关闭」——旁证：`agent_config.rs:630` 的 `#[serde(default = "default_stream_overall_timeout_secs")]` 与 `stream_collect.rs:368-370` 的 `overall_secs == 0` 分支；**缺什么证据**：没有配置写入路径的运行时验证，未实测。
2. 条目 2「前端看门狗 600s 是最长无事件窗口」——旁证：`useStreamChat.ts:621-640` 的 cap 选择；**缺什么证据**：前端定时器在标签页后台时被浏览器节流（clamping）的行为，仓库内无代码可证，未实测。
3. 条目 3「24h 兜底」——旁证：`tool_dispatch.rs:474-478`；**缺什么证据**：`ApprovalRegistry` 端 presence 计时与 1800s 硬顶如何与 24h net 竞争，属运行时竞态，未实测。
4. 条目 16/17「界面一直转圈」——旁证：调用为 async fn / `#[tauri::command]`；**缺什么证据**：Tauri 异步命令调度与前端 loading 态的对应关系属框架行为，未实测。
5. 条目 18「reg.exe 挂住时永久停在这里」——旁证：`webview2_probe.rs:125-130` 同步 `.output()`；**缺什么证据**：Windows 下 `reg.exe` 实际挂起概率与 Tauri setup 阻塞窗口的观感，未实测。

## 六、未实测与缺证

- 本轮未启动应用、未跑测试、未跑构建，**没有任何实测数据**。所有秒/毫秒数值均取自源码字面量或源码注释，标注「源码注释自述，未实测」。
- 主清单第四部分列出的未核对 grep 命中（llm_retry 退避、sessions SQLITE_BUSY 重试、tool_executor 超时、compression cooldown、member/sub_agent 默认超时、copilot_acp、memory_prefetch、rate_limit、win_job/file_lock 忙等、approval.rs/ws 无 timeout 的通道等待、filesearch、plugin、environments、intelligence HTTP、useApprovalBridge、usePolling/useCustomApps、fetchJSON 无 AbortController、cdp_probe/shutdown/screenshot_controller）本次**未逐条回源**，不在 18 个编号条目范围内，不计入判定分布。
- 其中 `apps/desktop-tauri/src/api/rest.ts` 的 `fetchJSON` 无 AbortController 一项，若要并入清单需单独回源核对；主清单标「未定论」，本次维持未定论。

## 七、整体结论

**这份清单不能直接给用户。** 18 个编号条目里 5 条需修正（1、2、3、6、9、18 中的 1/2/3/6/9/18 共 6 项位置或机制有偏差，其中 6 条编号实际为 1、2、3、6、9、18），核心等待点与上限数值本身站得住，但行号、机制归属与线程归属存在可核实的偏差，直接交付会把错误行号和「等满 30s 再兜底」一类不准确机制描述带给读者。
另需修正的是汇总口径：对外只能写「编号条目 18 条」，不能写「成立 18 条」。
修正上述 5 条后，条目 1–18 的数值与上限可作为「等待点存在性 + 上限值」的清单交付；界面表现与线程阻塞类结论须保留「未实测 / 推测」标注。
