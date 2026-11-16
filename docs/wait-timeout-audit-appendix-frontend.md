# 等待点补充件：前端与桌面端未核命中回源核对（hermix-desktop）

状态：第 2 版（收口版）。7 组待核命中全部回源核对完毕；F-2 的 retry 定时器一项标「未能核对」并写明缺什么证据。
本文件是 `docs/wait-timeout-audit.md` 第四节的补充件，不改动主清单正文。

核对口径：每条结论来自 read_file 实读，行号与代码实际一致；查不到的写「未能核对」并说明缺什么证据。
范围：TS/TSX 前端 + `apps/desktop-tauri/src-tauri`；Rust crates 侧命中由另一单负责，本单不碰。
阻塞维度口径：记会让界面线程 / 主进程主线程长时间无响应的等待型代码点，并标注线程归属。

---

## 一、逐条结论

### F-1 `apps/desktop-tauri/src/api/rest.ts` 的 `fetchJSON`（定义行 181）

| 条目号 | 结论 | 路径:行号 | 等待机制 | 触发条件 | 界面表现 | 阻塞线程 |
|---|---|---|---|---|---|---|
| F-1 | **不成立**（主清单「无前端请求超时」的推测被推翻） | `apps/desktop-tauri/src/api/rest.ts:77`、`:99`、`:111`、`:113`、`:117-137`、`:142-149`、`:181`、`:203-209` | 默认超时值。客户端死线 `REQUEST_TIMEOUT_MS = 10_000`（10s），调用点可用 `opts.timeoutMs` 覆盖（`rest.ts:422` 有 `30_000` 的实际覆盖点）。机制：`AbortSignal.timeout(timeoutMs)`（`:111`）+ `AbortSignal.any([callerSignal, timeoutSignal])`（`:113`）+ `Promise.race([invoke(...), abortPromise])`（`:142-149`）。 | 后端不响应或桥不回帧 → 请求在 10s 处 reject，错误名 `TimeoutError`，消息 `request timed out after ${timeoutMs}ms`（`:133-136`）。 | 调用方在 10s 内拿到一次失败，不会无限期挂起；Error 抛出后由调用点 catch / toast 处理。**注意**：该死线只释放调用方，Rust 侧桥请求不被中断，可能在后台继续跑满 120s（`rest.ts:71-76` 注释明确说明）。 | **不阻塞 UI 线程**。`invoke` 是异步 IPC，Promise.race 不占主线程；渲染进程事件循环不冻结。 |

证据片段：

```ts
const REQUEST_TIMEOUT_MS = 10_000;
```

```ts
  const timeoutSignal = AbortSignal.timeout(timeoutMs);
  const combined = signal
    ? AbortSignal.any([signal, timeoutSignal])
    : timeoutSignal;
```

```ts
  const raw = await Promise.race([
    invoke<string>(IPC.backendHttpRequest, {
      method,
      path,
      body: payload,
    }),
    abortPromise,
  ]);
```

定论：主清单第四节的「grep 命中 `AbortController` 0 处，倾向无前端请求超时，未定论」**不成立**。代码没用 `AbortController` 构造函数，用的是 `AbortSignal.timeout` / `AbortSignal.any`，grep 关键字选错导致误判。`fetchJSON` 有硬上限（默认 10s）。调用方表现：抛 `TimeoutError` 的 `Error`，多数调用点用 `try { } catch { return null }` 或 `.catch()` 吞掉。

### F-2 `apps/desktop-tauri/src/hooks/useApprovalBridge.ts:34-44`（retry / timeout / rescue 三个定时器）

| 条目号 | 结论 | 路径:行号 | 等待机制 | 触发条件 | 界面表现 | 阻塞线程 |
|---|---|---|---|---|---|---|
| F-2 | **成立**（三个定时器数值全部在 `App.tsx` 写入点，不在该 hook 内；`retryTimers` 一项**未能核对**） | 表定义：`apps/desktop-tauri/src/hooks/useApprovalBridge.ts:34`、`:37`、`:44`、`:56`；写入点：`apps/desktop-tauri/src/App.tsx:2300-2306`（timeout）、`:2315-2330`（rescue）、`:1623-1625`、`:1678-1680`、`:1778-1780`、`:1816-1818`（rest） | **timeout**：默认超时值，延迟 = `Math.max(0, req.expiresAt - Date.now())`（`App.tsx:2301`），即服务端下发的墙钟 deadline 差值；到期 resolve 成 `expired`（`App.tsx:2303-2305`）。**rescue**：固定 15s 一轮（`App.tsx:2314` 的 `}, 15_000);`），到期若 resolver 仍在 → `api.getPendingApprovals(sessionId)` 直查服务端注册表，仍挂着就重新投影（重武装下一轮 15s），查不到就本地标 expired（`App.tsx:2315-2330`）。**rest**：固定 1s 延迟（`App.tsx:1623`、`:1679`、`:1779`、`:1817` 的 `}, 1000);`）后 POST `/v1/sessions/<sid>/approval` 兜底。**retry**：**未能核对**（见下）。 | timeout：服务端审批 deadline 到达而用户还没点（后端已自动拒绝）。rescue：审批事件因 WebView 渲染冻结 / 断链 / 投影落空而没变成卡片（`App.tsx:2310-2314` 注释）。rest：用户点了允许 / 拒绝后 WS 帧可能丢，用 REST 补发（`App.tsx:1620-1622` 注释）。 | 审批卡片在服务端 deadline 前一直是「等待你的决定」；deadline 到点自动变「已过期」。审批事件丢失时，15s 一轮的自救直查会持续到用户点击或服务端超时；界面无「正在重试」提示（catch 静默，`App.tsx:2328` `.catch(() => {})`）。 | **不阻塞 UI 线程**：全是 `setTimeout` 异步回调。rescue 每 15s 发一次 `getPendingApprovals`，该请求走 `fetchJSON`，受 F-1 的 10s 死线约束，单轮最长占 10s 后 reject（静默吞掉）。 |

证据片段（`useApprovalBridge.ts` 全文 1-79 行，只有表类型，无任何数值）：

```ts
  /** 审批响应的 WS 重发定时器——审批结算时清理。 */
  retryTimers: Map<string, ReturnType<typeof setInterval>>;
```

```ts
  /** 前端超时定时器：服务端 deadline（30s）到了用户还没点 → 卡片标"已过期" */
  timeoutTimers: Map<string, ReturnType<typeof setTimeout>>;
```

```ts
  rescueTimers: Map<string, ReturnType<typeof setTimeout>>;
```

证据片段（`App.tsx`）：

```ts
    if (req.expiresAt) {
      const delay = Math.max(0, req.expiresAt - Date.now());
      const timer = setTimeout(() => {
        approvalBridge.timeoutTimers.delete(req.requestId);
        resolveApproval(req.requestId, "expired");
      }, delay);
      approvalBridge.timeoutTimers.set(req.requestId, timer);
    }
```

```ts
    const rescueTimer = setTimeout(() => {
      ...
    }, 15_000);
    approvalBridge.rescueTimers.set(req.requestId, rescueTimer);
```

```ts
        const t = window.setTimeout(async () => {
          ...
        }, 1000);
        approvalBridge.restTimers.add(t);
```

**retry 定时器：未能核对。** `useApprovalBridge.ts:34` 声明 `retryTimers: Map<string, ReturnType<typeof setInterval>>`，类型是 `setInterval`（周期重发），但全仓 grep `retryTimers.set` **零命中**（`apps/` 范围两次搜索，`apps/desktop-tauri/src` 范围一次搜索，均无写入点）。只找到三处读取 / 清理：`App.tsx:1461-1465`（单卡结算时 `clearInterval`）、`App.tsx:1711-1715`（批量结算时 `clearInterval`）、`App.tsx:3028-3029` 与 `App.tsx:3390-3391`、`src/lib/chatStateReset.ts:138-139`（统一清理）。缺失的证据：找不到任何 `retryTimers.set(...)` 写入点，也就无从确认它的间隔数值与触发条件。可能的解释（**推测，未验证**）：该表的写入逻辑随「WS 重发」改造被删除，类型声明成了残留。确认这一点需要历史版本或跨仓搜索，本轮未做。**结论按「未能核对」记，不计入成立条数。**

### F-3 `apps/desktop-tauri/src/hooks/usePolling.ts:35`（轮询间隔 / 退避 / 上限）

| 条目号 | 结论 | 路径:行号 | 等待机制 | 触发条件 | 界面表现 | 阻塞线程 |
|---|---|---|---|---|---|---|
| F-3 | **成立**（等待型轮询点：固定间隔、无退避、无总预算上限、无单次请求超时） | `apps/desktop-tauri/src/hooks/usePolling.ts:19`、`:25`、`:33-35`、`:37`、`:45` | 轮询间隔由调用方传入（`intervalMs` 是必填参数，本 hook 内**无默认值**，`:19`）；本 hook **不做退避**、**不设总预算上限**、**不设单次请求超时**。`immediate` 默认 true（`:25`），挂载即打一次（`:34`），随后 `window.setInterval(tick, intervalMs)`（`:35`）。 | 组件挂载且 `enabled !== false`；页面可见时每 `intervalMs` 触发一次。 | 数据按固定间隔刷新；后端挂死时列表停在旧值并每轮重复失败，无「正在加载」收敛提示。 | **不阻塞 UI 线程**：`tick` 里是 `void saved.current()`（异步 fire-and-forget），不 await 上一次请求（`:33-35` 注释明示「本 hook 不等待上一次请求完成」）→ 慢请求会重叠堆积。 |

证据片段：

```ts
    const tick = () => {
      if (stopped || document.hidden) return;
      void saved.current();
    };
    if (immediate) tick();
    const timer = window.setInterval(tick, intervalMs);
```

各调用点实读到的间隔（全部 read_file 核对）：

| 调用点 | 间隔 | 备注 |
|---|---|---|
| `apps/desktop-tauri/src/App.tsx:692-694` | `KEEP_AWAKE_POLL_MS = 10_000`（常量定义 `App.tsx:95`） | 保活巡检，拉 `api.getActiveAutomationRuns()` |
| `apps/desktop-tauri/src/components/CustomAppsView.tsx:65-73` | `AUTO_REFRESH_INTERVAL_MS = 5_000`（定义 `CustomAppsView.tsx:21`），`immediate: false` | 连续失败达 `MAX_CONSECUTIVE_FAILURES = 3`（定义 `CustomAppsView.tsx:25`）即停打（`:68`） |
| `apps/desktop-tauri/src/components/FileSearchWindow.tsx:134-139` | `2000`（字面量） | 文件搜索状态轮询 |
| `apps/desktop-tauri/src/components/MissionBoardView.tsx:284` | `10_000` | members 快轮询 |
| `apps/desktop-tauri/src/components/MissionBoardView.tsx:285` | `60_000` | 静态面（巡检/角色/评分）慢轮询 |

### F-4 `apps/desktop-tauri/src/hooks/useCustomApps.ts:47`（轮询间隔 / 退避 / 上限）

| 条目号 | 结论 | 路径:行号 | 等待机制 | 触发条件 | 界面表现 | 阻塞线程 |
|---|---|---|---|---|---|---|
| F-4 | **成立**（指数退避 + 封顶，且**永不放弃**） | `apps/desktop-tauri/src/hooks/useCustomApps.ts:22`、`:23`、`:36`、`:38-48`、`:57`、`:62`、`:67` | 指数退避：基准 `BASE_INTERVAL_MS = 5_000`（`:22`），封顶 `MAX_INTERVAL_MS = 30_000`（`:23`），实际间隔 `Math.min(5000 * 2**consecutiveFailures, 30000)`（`:43-46`），用 `window.setTimeout(runPoll, backoff)` 排下一轮（`:47`）。失败计数 `consecutiveFailures++`（`:62`），成功复位为 0（`:57`）。**无总次数上限、无熔断、永不停止**（`:13-19` 注释：旧实现连续失败 3 次停表，现改为退避永不放弃）。 | 后端未起（hermix.exe 未 bind 3000）或请求持续失败 → 间隔 5s→10s→20s→30s→30s… 一直续。 | `@app` 列表在最长 30s 间隔上更新；后端一直不来的话列表长期为空，界面只有 error 文案（`:63`），无终止状态。 | **不阻塞 UI 线程**（异步请求 + setTimeout 链）；每轮失败都 setState（`:58-64`），持续失败会持续产生渲染压力。 |

证据片段：

```ts
const BASE_INTERVAL_MS = 5_000;
const MAX_INTERVAL_MS = 30_000;
```

```ts
      const backoff = Math.min(
        BASE_INTERVAL_MS * 2 ** consecutiveFailures,
        MAX_INTERVAL_MS,
      );
      timerId = window.setTimeout(runPoll, backoff);
```

### F-5 `apps/desktop-tauri/src-tauri/src/cdp_probe.rs:36` / `:63`（30s recv_timeout）

| 条目号 | 结论 | 路径:行号 | 等待机制 | 触发条件 | 界面表现 | 阻塞线程 |
|---|---|---|---|---|---|---|
| F-5 | **成立**（有 30s 上限，等待发生在主线程） | `apps/desktop-tauri/src-tauri/src/cdp_probe.rs:33`、`:36`、`:41-42`、`:63-64`、`:107-109` | 默认超时值：`probe_via_tauri(&webview, Duration::from_secs(30))`（`:36`），`rx.recv_timeout(timeout)`（`:63`）等 `with_webview` 闭包把结果送回；超时文案「探针超时：with_webview 闭包未执行（事件循环未泵？）」（`:64`）。三层等待叠加：外层 30s `recv_timeout`（`:63`）→ 闭包内 `call_cdp_sync` 的 `result_rx.recv()`（**无超时**，`:107-109`）→ 宏生成的 `wait_for_async_operation` + `wait_with_pump` 泵消息循环（`:83` 附近注释：等待期间泵消息循环，必须在主线程调用）。三次 CDP 调用串行（`probe_results`，`:114-172`）。 | 人工/脚本触发 `invoke("cdp_n0_probe")`（`:33` 注释：前端 UI 没有调用点，是真机验证工具，不是产品功能）。 | 无产品界面影响（无 UI 调用点）；调用方最多挂 30s 拿到一条超时错误。 | **主进程主线程**：`:45-46` 注释写明「在主线程（with_webview 闭包内）跑全部探针」。等待期间主线程被占用（泵消息循环，不是冻结），最长 30s。**风险点**：`call_cdp_sync` 的 `result_rx.recv()`（`:107-109`）没有 `recv_timeout`，若完成回调不带结果 JSON，会在主线程上无限期阻塞，且外层 30s `recv_timeout` 只覆盖 `with_webview` 闭包排队阶段，**救不了这一层**。 |

证据片段：

```rust
    probe_via_tauri(&webview, std::time::Duration::from_secs(30))
```

```rust
    rx.recv_timeout(timeout)
        .map_err(|_| "探针超时：with_webview 闭包未执行（事件循环未泵？）".to_string())?
```

```rust
    result_rx
        .recv()
        .map_err(|_| format!("CDP {method} 完成回调未带结果 JSON"))
```

### F-6 `apps/desktop-tauri/src-tauri/src/shutdown.rs:56` / `:100-110`（2s 文件搜索退出等待）

| 条目号 | 结论 | 路径:行号 | 等待机制 | 触发条件 | 界面表现 | 阻塞线程 |
|---|---|---|---|---|---|---|
| F-6 | **成立**（有 2s 上限的同步阻塞轮询） | `apps/desktop-tauri/src-tauri/src/shutdown.rs:56`、`:100`、`:105`、`:107`、`:110` | 轮询间隔 + 总预算：`wait_filesearch_gone(Duration::from_millis(2000))`（`:56`）→ 预算 2s；循环内每次 `TcpStream::connect_timeout(&addr, Duration::from_millis(120))`（`:107`），连不上即返回；否则 `thread::sleep(Duration::from_millis(120))`（`:110`）。约 120ms 一轮，2s 内约 16 轮。 | 关闭主程序 / 主窗销毁 / 僵尸实例唤起（`cleanup` 路径，`shutdown.rs:40-57`；`cleanup` 由 `RunEvent::ExitRequested` / `Exit` / 托盘退出 / 主窗销毁共同调用，见 `shutdown.rs:1-14` 注释）。 | 退出时最多卡 2s；超时只写一条桌面日志「索引服务 {budget:?} 内未退出（多半在刷盘，稍后自行结束）」（`:112`），随后照常退出。 | **主进程主线程**：`cleanup` → `cleanup_and_exit` → `app.exit(0)`（`shutdown.rs:60-71`）在退出路径上同步调用，这段是 `std::thread::sleep` 阻塞式等待，最长 2s。 |

证据片段：

```rust
    request_filesearch_stop();
    wait_filesearch_gone(Duration::from_millis(2000));
```

```rust
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
```

### F-7 `apps/desktop-tauri/src-tauri/src/screenshot_controller.rs:301` / `:327`（session fallback / ESC 2s 轮询）

| 条目号 | 结论 | 路径:行号 | 等待机制 | 触发条件 | 界面表现 | 阻塞线程 |
|---|---|---|---|---|---|---|
| F-7 | **成立**（两处都有上限；均在独立线程，不是 UI 线程） | `apps/desktop-tauri/src-tauri/src/screenshot_controller.rs:47`、`:299-318`、`:324-346` | 默认超时值 + 轮询间隔：session fallback 线程 `sleep(Duration::from_secs(SESSION_FALLBACK_SECS))`，`SESSION_FALLBACK_SECS = 30`（`:47`）；30s 后会话仍 active 且覆盖窗不可见 → 发 `hermix:screenshot-error` 并结束会话（`:301-318`）。ESC 监视线程：等 ESC 松开上限 2s（`:327`），每 30ms 轮询一次 `esc_pressed()`（`:329`）；之后进入 40ms 一轮的边沿检测循环（`:333`），会话结束即 break。 | 截图会话启动后覆盖窗 / 幕布异常（页面未亮），或用户按住 ESC。 | 覆盖层 30s 未就绪 → 弹「截图覆盖层 30s 未就绪，已关闭。若反复出现请重启应用。」；ESC 监视最长 2s 后才进入按键检测（此间用户按 ESC 无效）。 | **不阻塞 UI 线程**：两处都是 `std::thread::spawn` 起的独立线程（`:300`、`:325`）；主线程只被 `end_session` 之类短调用触碰。 |

证据片段：

```rust
const SESSION_FALLBACK_SECS: u64 = 30;
```

```rust
        std::thread::sleep(std::time::Duration::from_secs(SESSION_FALLBACK_SECS));
```

```rust
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while screenshot::esc_pressed() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
```

---

## 二、成立 / 不成立统计

| 结论 | 条目 | 数量 |
|---|---|---|
| 成立 | F-2（timeout/rescue/rest 三个定时器）、F-3、F-4、F-5、F-6、F-7 | 6 |
| 不成立 | F-1（`fetchJSON` 有 10s 硬上限，主清单推测被推翻） | 1 |
| 未能核对（不计入成立） | F-2 的 `retryTimers` 子项（全仓无 `.set` 写入点） | 1 |
| 与等待无关 | 无 | 0 |

按阻塞位置分：主进程主线程阻塞 2 条（F-5 的 CDP 探针、F-6 的退出等待）；UI 线程阻塞 0 条（前端命中全是异步等待 / 定时器，不冻结渲染进程事件循环）。

---

## 三、本次读过的文件清单

| 文件 | 读取范围（行） |
|---|---|
| docs/wait-timeout-audit.md | 1-120, 300-365 |
| apps/desktop-tauri/src/api/rest.ts | 60-259, 550-569（grep 定位 fetchJSON 定义行 181） |
| apps/desktop-tauri/src/hooks/useApprovalBridge.ts | 1-79（全文） |
| apps/desktop-tauri/src/hooks/usePolling.ts | 1-47（全文） |
| apps/desktop-tauri/src/hooks/useCustomApps.ts | 1-119（全文） |
| apps/desktop-tauri/src/App.tsx | 88-101, 686-705, 706-790, 1420-1479, 1450-1479, 1580-1669, 1650-1663, 1700-1729, 1740-1829, 1810-1823, 2270-2359, 2288-2335, 2400-2434, 2990-3044 |
| apps/desktop-tauri/src/hooks/useStreamParser.ts | 720-739, 765-824 |
| apps/desktop-tauri/src/components/CustomAppsView.tsx | 15-32, 58-82 |
| apps/desktop-tauri/src/components/FileSearchWindow.tsx | 128-147 |
| apps/desktop-tauri/src/components/MissionBoardView.tsx | 276-293 |
| apps/desktop-tauri/src/lib/chatStateReset.ts | 130-148 |
| apps/desktop-tauri/src-tauri/src/cdp_probe.rs | 20-89, 90-178（全文） |
| apps/desktop-tauri/src-tauri/src/shutdown.rs | 1-40, 40-124 |
| apps/desktop-tauri/src-tauri/src/screenshot_controller.rs | 40-64, 280-349 |

## 四、未能核对与后续需要

- **F-2 的 `retryTimers` 写入点缺失**：`apps/` 与 `apps/desktop-tauri/src` 两轮 grep `retryTimers.set` 零命中，只读到三处 `clearInterval` 清理。缺的是该表的写入代码（间隔数值与触发条件）。若需定论，需跨仓/历史版本搜索，或向作者确认该机制是否已被删除。
- **F-2 的服务端 deadline 数值**：`App.tsx:2296` 注释写「后端在 30s 自动拒绝」，但前端 timeout 定时器用的是服务端下发的 `req.expiresAt` 差值（`App.tsx:2301`），不是前端写死的 30s。前端这一档的实际上限取决于服务端 `expires_at`，本轮未核对服务端计算逻辑（属 Rust crates 侧，未越界读）。
- **F-5 的 `call_cdp_sync` 内层 `result_rx.recv()`**（`cdp_probe.rs:107-109`）无超时，外层 30s 覆盖不到这一层。结论已按代码事实记录，未验证该路径在真实机器上的触发概率。
