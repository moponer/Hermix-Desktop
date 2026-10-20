# Decision Note: 查看器的网页预览与浏览器内核（内嵌视图 / 外部浏览器 双通道）

Status: proposed — 2026-10-21 调研完成并定路线；「钉住浮窗」已落地（见 [同批的 pinned-preview-windows 笔记](./2026-10-21-viewer-pinned-preview-windows.md)），网页预览与 agent 浏览器通道待实施。

## Decision

查看器的地址栏接受网址后，**在应用内展示网页**；菜单栏提供**浏览器内核
开关**；agent 可以对接浏览器做**有头 / 无头**操作，且多实例、多 agent
互不干扰。

定为**双通道**，而不是"挑一个内核"：

| 通道 | 是什么 | 谁用 | 体积/依赖 |
|---|---|---|---|
| **A 内嵌网页视图** | Tauri 子 webview（`WebviewUrl::External`），顶层加载 | 用户直接看；agent 可选驱动 | **零新增依赖、零新进程**（系统 WebView2 已是我们的运行时） |
| **B 系统浏览器** | 系统 Edge/Chrome + CDP（`--headless=new`），画面走 screencast | agent 操作用户看帧 | **零新增依赖、零下载**（用系统已装的 Edge） |

**「有头」的定义**：**应用内可见**——通道 A 是可交互的真窗口（用户直接点），
通道 B 是 screencast 帧流（用户看得到画面、agent 同时在操作）。
**不弹独立的 OS 浏览器窗口**：那会让浏览器抢焦点、进 Alt+Tab、可被误关，
与本仓库 2026-10-14 的既有裁决（`2026-10-14-browser-virtual-window-p0.md`
"浏览器永不现形"）冲突；要真窗口是另一条独立提案。

**「选内核」菜单做成两档**（内嵌网页视图 / 系统浏览器）+ 一个逃生口
（在系统默认浏览器打开）。WebView2 与 Edge 同为 Chromium——真正可切换的轴
不是"哪个渲染引擎"，而是**内嵌 vs 外部进程**，这个轴才值得做成开关。

**不用 iframe**（双重否决）：① 我们的 CSP 是 `default-src 'self'`，
`frame-src` 不放行外部站点；② 绝大多数站点发 `X-Frame-Options: DENY` /
`frame-ancestors`，直接拒嵌。用顶层子 webview 两者都绕开。

**隔离用四层里够用的三层**（CDP 官方语义）：
1. **进程级**：每 agent 一个 `--user-data-dir` → 独立进程组 + 独立
   cookie/storage/权限（本仓库 `launch.rs` 已如此实现）。
2. **上下文级**：`Target.createBrowserContext` → 同进程内的 incognito 式
   独立档案（轻量）。
3. **target 级**：`Target.attachToTarget{flatten:true}` → 每个 agent 持有
   自己的 `sessionId`，命令带 sessionId，不再抢"当前活动 target"。

## Alternatives considered

- **Playwright / Puppeteer** —— **本仓库已否决**（`2026-10-01-agent-browser.md`）：
  需要 Node/Python 运行时，违背 os_auto 的「零外部运行时」内核；默认下载的
  chromium 281M / firefox 187M / webkit 180M 直接撞「便携零安装」。
  （Playwright 确实支持 `channel:'msedge'` 不下载浏览器，但整套 API 是
  Node 的，与我们的 Rust 栈不搭。）
- **CEF / CEF-lite 随包分发** —— 否决：windows64 minimal 就是 163.3 MB，
  还要自己背 Chromium 的 CVE 更新。
- **WebView2 Fixed Version 随包分发** —— 否决：同上，且失去 Evergreen 的
  自动安全更新。
- **Servo / Ultralight / WKWebView / WebKitGTK** —— 否决：Servo 自称
  "prototype"，没有可用的 agent/automation 接口面；Ultralight 是商业授权
  且体积/定价未查证；WKWebView 仅 macOS 且不可再分发；WebKitGTK 是 Linux
  专用且要系统依赖。
- **attach 用户日常浏览器（拿用户的登录态）** —— 否决：隐私暴露最大
  （agent 能以用户已登录身份操作站点），而且 **Chrome ≥136 已封堵**：
  官方明文「`--remote-debugging-port` 不再对默认 Chrome 数据目录生效，
  必须配 `--user-data-dir`」。本仓库本来就用独立 profile，**不受影响**；
  但这条要挂到 `2026-10-07-browser-space-and-login-state.md`（N2，proposed）
  的待办上重新评估——Edge 是否同策略**未查证**。
- **自研 / 定制 Chromium 内核、Xvfb 虚拟显示** —— 已否决（既有决策）。

## Consequences

- **本轮不改 hermix-tools 的浏览器内核**：viewer 侧自建最小实现（面板
  约 45 行 + ws 发现约 120 行），保持 viewer 轻量（它现在只依赖
  hermix-config）。**代价**：与 `hermix-tools/src/tools/os_auto/browser/`
  有一份"CDP 客户端"的重复。**长期正解**是把浏览器内核抽成共享 crate
  （`crates/hermix-browser`），桌面端与查看器共用；这需要动 hermix-tools，
  等它旁边的并行改动落定再做。
- **安全项必须做**：Tauri/wry **默认关闭 SmartScreen**
  （`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`）。
  用 webview 承载**不受信任的远程网页**时必须显式恢复（具体 flag 待实测）。
  同时**不给远程 webview 配 `remote.urls`**（默认就没有 IPC）；万一要配，
  必须配合 Tauri 的 Isolation Pattern。
- **Tauri v2 的多窗口隔离原语**：不同 `data_directory` → 不同 UDF →
  **不同 browser 进程组**（WebView2 官方：每个 UDF 对应一个 session 与
  进程集合）。官方警告：不同 browser arguments 必须配不同 data directory。
- **菜单栏**：「视图」菜单本来就是数据驱动的（`MenuGroup[]`，已支持
  `checked`），加一档开关是纯增量。
- 未做：PDF/网页的**沙箱化**（远程页面与自家 IPC 同进程，靠不配 `remote.urls`
  与 UDF 隔离兜底）；**网页的下载拦截**（`CoreWebView2.DownloadStarting`
  未接）；**多 profile 的 WebView2 官方 API**（`ICoreWebView2Profile` 存在但
  签名未查证）。
