# Decision Note: 「后端自己断了」的三层真相——外壳停机、服务端退不干净、界面撒谎

Status: implemented — 2026-10-29 交付：sidecar 有界收尾后显式退出（不再被强杀、不再留 Tokio panic）、桥侧停机词元统一成 `backend offline`、外壳每个停机调用点自报家门、watchdog 故障窗口 60s→15s、模型页区分「不可达 / 不支持 / 真的没有」并在恢复后自愈

## Decision

用户实报：点「启用社区免费模型」报 `免费模型开关未生效：backend exited`，同时「内置模型一个都看不到」。日志（`~/.hermix/logs/server.log`、
`<install>/data/logs/desktop.log`）显示**后端不是自己崩的**，是三个独立缺陷叠在一起：

1. **外壳主动停机**。`server.log` 里两次
   `stdio: shutdown frame received, exiting gracefully`（15:11:31 / 15:13:15）。
   `shutdown` 帧全仓只有一个发送者 —— `sidecar.rs::try_graceful_shutdown()`，
   只被 `stop()` 调用，而 `stop()` 的调用点只有三个：`restart_sidecar` 命令（前端
   「重启/重连」按钮）、watchdog（会先打 30 次 `health check failed`，这两次都没有）、
   `shutdown::cleanup()`（退出流程 / 主窗销毁）。第一次 5s 后就重启（`restart_sidecar`
   语义），第二次 64s 后才被 watchdog 拉回（`cleanup` 语义）。
2. **服务端退不干净**。每次停机都在 52ms 后 panic：
   `A Tokio 1.x context was found, but it is being shutdown`
   （tokio-1.53.1 `runtime/time/entry.rs:539`），随后外壳 3s 超时 TerminateProcess。
   根因是 `#[tokio::main]` 靠 drop runtime 收尾：MCP 60s 重试 / relay / 资源巡检 /
   目录刷新这些分离任务永不结束，drop 卡住；drop 期间还有任务被 poll 并注册定时器
   → panic。于是「优雅退出」一次都没真正跑完（SQLite/WAL 收尾被跳过，每次重启都留
   26 条 `unfinished tool effects NEEDS_REVIEW` 与一份 panic 日志）。
3. **界面把「读不到」说成「没有」/「太旧」**。`getModelList` / `getFreeChannel` 都是
   `catch → null`，于是同一屏同时出现三句假话：开关失败原因原样拼进标题、
   列表空 → 「还没有配置任何模型」、开关态 null → 「当前后端不支持这个开关（需更新后端）」。

修法（按层）：

- **服务端**（`crates/hermix-server/src/stdio_session.rs`、`main.rs`）：stdio 主循环
  退出时带出真实原因（stdin EOF / 读错误 / shutdown 帧 / watch 通道），IM 关连接
  用 2s `timeout` 兜底；`run_stdio_server` 返回后**显式 `std::process::exit`**，
  不进 runtime 析构。日志刷盘由新增的 `hermix_telemetry::flush_logs()` 负责
  （日志按 64 条 / 200ms 攒批，直接 exit 会丢掉最后 8KB —— 恰好是排查最需要的那几行）。
- **外壳**（`sidecar.rs`）：`stop()` 先发帧、**随后立刻** `mark_dead()` 再等退出
  （原顺序是等完才标死，等待期内进来的请求会被写进黑洞挂到 10s 死线）；
  宽限 3s → 5s 并打印实际耗时与退出码；watchdog 连续失败阈值 30 → 6（≈15s），
  并把 `bridge_ready` / `child_alive` 打进失败行（原来只有 `(n/30)`，无法区分
  「子进程没了」和「桥没接上」）。
  `shutdown::cleanup(app, trigger)` 增加必填 trigger（`tray-quit` /
  `run-exit-requested` / `run-exit` / `main-window-destroyed` / `dev-no-vite`），
  全部落 `desktop.log`；`restart_sidecar` 记录请求与结果。
  主窗 `Destroyed` 触发的 cleanup 增加 `webview_windows().len() <= 1` 前置条件 ——
  主窗销毁 ≠ 程序退出，预览窗/截图窗还在时进程会继续跑，此时停 sidecar 就是
  「界面还在、后端没了」。
  （该前置条件 2026-10-30 已废止：隐藏的截图覆盖窗让它恒为假，导致「关了程序
  进程还在、再点图标不起来」，改为主窗销毁即 `cleanup_and_exit`。见
  `2026-10-30-close-leaves-zombie-process.md`。）
- **桥**（`stdio_bridge.rs`）：停机时唤醒挂起请求的 503 body 从 `backend exited`
  改成 `backend offline`，与 `backend_http_request` 同一词元，前端已有映射
  （`api/rest.ts::offlineMessage`）把它翻成「本地后端未连接（服务未启动或正在重启）」。
- **前端**（`api/rest.ts`、`ModelListTab.tsx`）：新增 `probeModelList()` /
  `probeFreeChannel()`，返回 `ok | offline | unsupported | error` 四态；
  模型页据此分别措辞，**读不到时保留上一版列表**（读不到 ≠ 没有），并在 offline
  时每 3s 探活、恢复后静默刷新；开关写入失败若回读发现是 offline，就说
  「后端正在重连，开关没有生效」而不是转述桥的协议文本。

## Alternatives considered

- **只把外壳宽限从 3s 提到 10s**：治不了 panic，也治不了 drop 卡死；只是把
  「每次重启丢一次优雅收尾」推迟到 10s 后。且 TerminateProcess 仍在，SQLite 收尾
  仍然靠运气。否决。
- **给每个后台循环发 CancellationToken 让它们有序退出**：正解，但要改 MCP / relay /
  巡检 / 目录刷新 / IM 五条链路，且未来新增的循环仍会漏（这份清单每加一个后台任务
  就过期）。显式 exit 用一行覆盖全部，代价是这些循环不再执行清理动作 —— 它们本来就
  没有必须执行的清理（不持有未落盘状态）。等哪天真出现必须 flush 的后台状态，再
  逐个补约定；届时这个笔记就是「为什么当时没有」的答案。
- **保留 runtime drop 但 `shutdown_timeout(ZERO)`**：仍会在 shutdown 期间 poll 到
  分离任务并触发同一个 timer panic；且要改写 `#[tokio::main]`。收益不如显式 exit 直接。
- **前端在 offline 时显示「没有模型」并自动重试**：改文案不改语义 —— 用户仍然会以为
  配置丢了。必须复用上一版列表，让「读不到」与「真的是空」在界面上可区分。
- **把主窗 `Destroyed` 的 cleanup 整段删掉（只留 `RunEvent`）**：更干净，但失去
  「主窗销毁但进程还活着」时的兜底可能（当前实现是把它挡住而不是删掉收尾能力）。
  用「没有别的窗口」这个条件既保留兜底又不再误杀。

## Consequences

- 服务端退出不再执行任何 Rust 析构：SQLite 靠 WAL 在下次打开时恢复；metrics 的
  10s 内存缓冲会丢最后一批（≤10s）；`hermix-viewer` 等外挂进程本就由 Job Object
  与各自看门狗管理，不受影响。
- `flush_logs()` 之后若还有 `tracing::*` 写入会落进缓冲区并在进程结束时丢失 ——
  所以它必须是退出前最后一个动作。
- watchdog 故障窗口缩短到 ~15s：`is_healthy()` 不探 `/health`（只看 `bridge_ready`
  + 子进程存活），慢启动不会消耗失败预算，因此不会把慢启动误判成故障；反过来，
  真故障时用户对着死界面等待的时间从 60s 降到 ~15s。
- 仍有一处未闭合：**第二次停机（`cleanup`）究竟由哪个窗口事件触发**，10-29 的日志
  已无法区分。现在每个调用点都会在 `desktop.log` 里写 trigger，且主窗销毁路径
  加了前置条件，下次事件可直接定位。这条不确定性是这段代码唯一的残留风险。
  （2026-10-30 已定位：是主窗销毁触发的，而该路径同时留下了僵尸进程 —— 前置
  条件失效 + 进程不退出 + watchdog 重生 sidecar，见
  `2026-10-30-close-leaves-zombie-process.md`。）
