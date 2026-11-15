# Decision Note: 查看器的网页预览与浏览器内核（内嵌视图 / 系统浏览器 双通道）

Status: implemented — 2026-10-21 调研与路线已定，本轮落地：内嵌网页视图（通道 A）、
菜单栏内核开关、系统浏览器 + CDP 的 agent 通道（通道 B）、MCP 工具与浏览器会话面板。

## Decision

查看器的地址栏接受网址后，**在应用内展示网页**；菜单栏提供**浏览器内核
开关**；agent 可以对接浏览器做**有头 / 无头**操作，且多实例、多 agent
互不干扰。

定为**双通道**，而不是"挑一个内核"：

| 通道 | 是什么 | 谁用 | 体积/依赖 |
|---|---|---|---|
| **A 内嵌网页视图** | 主窗口里的**子 webview**（`Window::add_child`），顶层加载 | 用户直接看；人可点、可输入 | **零新增依赖、零新进程**（系统 WebView2 已是我们的运行时） |
| **B 系统浏览器** | 系统 Edge/Chrome + CDP（`--headless=new` 或有头），agent 驱动 | agent 操作用户看 | **零新增依赖、零下载**（用系统已装的 Edge） |

**「选内核」菜单**（视图菜单 + 设置 → 通用，两处都能切）：
`内嵌视图` / `系统浏览器`。另附逃生口：网页工具条上的「系统浏览器」按钮
（用系统默认浏览器打开当前页）。

**「有头 / 无头」的定义**（通道 B）：
- **有头** = 启动真实 Edge 窗口，用户看得见、也能接管；
- **无头** = `--headless=new`，不可见，agent 独占。

与仓库 2026-10-14 裁决（`2026-10-14-browser-virtual-window-p0.md`"浏览器永不
现形"）**范围不同**：那条裁的是桌面端 agent 浏览器面板（agent 驱动的网页
操作一律不现形）；这里是查看器里**用户显式选择**的浏览模式，用户要看就选
有头。默认仍是**无头**（安全：不抢焦点、不进 Alt+Tab）。

**不用 iframe**（双重否决）：① 我们的 CSP 是 `default-src 'self'`，
`frame-src` 不放行外部站点；② 绝大多数站点发 `X-Frame-Options: DENY` /
`frame-ancestors`，直接拒嵌。用顶层子 webview 两者都绕开。

**隔离用四层里够用的三层**（CDP 官方语义）：
1. **进程级**：每会话一个 `--user-data-dir` → 独立进程组 + 独立
   cookie/storage/权限。**会话 id 就是隔离键**（`browser_open(session=…)`），
   不同 agent 用不同 id → 互不干扰。
2. **上下文级**：`Target.createBrowserContext` → 同进程内的 incognito 式
   独立档案（轻量）。
3. **target 级**：`Target.attachToTarget{flatten:true}` → 每个 agent 持有
   自己的 `sessionId`，命令带 sessionId，不再抢"当前活动 target"。

### 内嵌网页视图（通道 A）的实现约束

- `Window::add_child` 是 Tauri v2 的 **unstable** API（`features = ["unstable"]`），
  且 **Windows 上必须在非主线程调用**（wry#583：主线程调用会死锁）→ 命令写成
  `async fn`，跑在 tokio 工作线程上。
- 子 webview 是**独立 HWND**，永远盖在 DOM 之上 → 前端必须留一个**占位
  元素**，把它的**物理像素**矩形同步给 Rust（`web_bounds`），子 webview
  正好盖在这个矩形上。CSS 像素会随 DPI 偏。
- 离开网页标签必须 `web_close`：否则子窗口会浮在应用界面上。

### 安全边界（第一准则：安全隐私）

- 只放行 http / https（`javascript:` / `file:` / `ms-msdt:` 一律拒），
  页面内跳转同样受限（`on_navigation`）——`ms-msdt:` 之类是 WebView2 的
  已知攻击面入口。
- 子 webview 走**独立的 WebView2 数据目录**（`$hermix_home/webview-profile`）
  → 远程站点的 cookie / localStorage 不落进主窗口 profile。
- capabilities 只覆盖 `main` 窗口 → 子 webview 拿不到我们的 IPC，远程页面
  无法调用本地命令；也**不配 `remote.urls`**。
- agent 通道：每个会话独立 profile，会话关闭时**删掉 profile**（cookie /
  缓存不留痕）；会话数上限 8（每个会话一个浏览器进程，内存线性增长）。
- **已知做不到的**：wry 默认关 SmartScreen
  （`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`），
  而 Chromium 的 disable 列表**优先于** enable 列表 → **无法**在子 webview
  里单独恢复。补偿手段是上面三条（独立 profile / 无 IPC / 协议白名单）。

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
  必须配 `--user-data-dir`」。本仓库本来就用独立 profile，**不受影响**。
- **自研 / 定制 Chromium 内核、Xvfb 虚拟显示** —— 已否决（既有决策）。
- **screencast 帧流（把无头浏览器的画面实时送进应用）** —— **本轮不做**：
  原方案里"有头 = 应用内可见的画面流"改成了"有头 = 真实浏览器窗口"，
  因为用户诉求是"有头 / 无头任意切换"，而真实窗口更符合"有头"的直觉，
  也少一套帧流传输。代价：无头模式下用户看不到 agent 的实时动作，
  只能靠 `browser_screenshot` 的截图（落盘 + 图片内容块）。
- **在 viewer 里内嵌 Craft 系列（WordCraft / GridCraft）** —— 已否决
  （体积 + 许可，见 `2026-10-21-viewer-office-editing-inplace.md`）。

## Consequences

- **agent 的入口是 MCP 工具**（不是 viewer 内部 UI）：`browser_open` /
  `browser_navigate` / `browser_text` / `browser_eval` / `browser_click` /
  `browser_screenshot` / `browser_sessions` / `browser_close`。任意 MCP
  客户端（Claude Code / Cursor …）接上 `http://127.0.0.1:3911/mcp` 就能用。
- **截图给模型看的是缩略图**（最长边 1024、JPEG q72），原图落盘到
  `$hermix_home/browser-shots/`。直接塞原图 PNG 就是上下文炸弹。
- **`browser_text` 比截图省 token**，读页面内容应优先用它。
- **viewer 与 hermix-tools 各有一份 CDP 客户端**：本轮 viewer 自建最小实现
  （同步 tungstenite 0.29 + JSON-RPC，已在 Cargo.lock → 零新增下载），
  保持 viewer 轻量（它只依赖 hermix-config）。**长期正解**是把浏览器内核
  抽成共享 crate（`crates/hermix-browser`），桌面端与查看器共用；这需要动
  hermix-tools，等它旁边的并行改动落定再做。
- **拉起浏览器必须走 `ShellExecuteExW`**（viewer 红线：自己 CreateProcess
  会被 360 判木马）。CDP 三个坑照抄 hermix-tools 的实测：端口写死会撞用户
  自己的浏览器（用 `--remote-debugging-port=0`，真实端口在 profile 的
  `DevToolsActivePort`）、绝不能用用户日常 Edge profile（Edge 单实例会把
  URL 交给已运行实例）、headless 默认 800×600 会让响应式布局变形（显式
  声明 `--window-size=1440,900`）。
- **踩过的坑：`ShellExecuteExW` 的返回值是 BOOL，不是 `SE_ERR` 码**。
  `ShellExecute`（没有 Ex）才返回 `SE_ERR_*`（0/1/2…）。按 `ok <= 32` 判
  失败会把 **TRUE(1) 当成失败**——表现为"浏览器怎么都起不来"，而其实它
  每次都正常启动了。正确写法是 `if ok == 0 { 失败 }`，真正的错误原因在
  `GetLastError()` 里。
- **每次开新会话 = 全新 profile**：复用旧 profile 会读到上次留下的
  `DevToolsActivePort`（端口已死 → 连接被拒），而且上次的 cookie 留着
  也不符合"隐私第一"。
- **内核切换只影响之后输入的网址**：已经开着的网页标签不变（用户看得见的
  页面不该被后台改掉）。
- 未做：网页的**下载拦截**（`CoreWebView2.DownloadStarting` 未接）；子
  webview 的**远程调试端口**（不给远程页面开 devtools）；**并排对比视图**。
