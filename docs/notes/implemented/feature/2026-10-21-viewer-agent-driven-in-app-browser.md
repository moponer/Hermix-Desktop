# Decision Note: 内嵌网页视图变成 **agent 可驱动**的浏览器 + 桌面端接线

Status: implemented — 2026-10-21 落地并真机验证（超星学习通登录 → 悬停用户名 →
进入个人空间 → 进课程列表，全程由桌面端 agent 驱动**用户正在看的那块预览区**）。

## Decision

查看器的**内嵌网页视图**（通道 A）从「只给人看」升级为「人和 agent 共用同一个
页面」：

1. **子 webview 开调试端口**：建窗时加 `--remote-debugging-port=0`（端口写 0
   = 让浏览器进程自己挑，真实端口写进 profile 的 `DevToolsActivePort`；写死端口
   会撞用户自己的浏览器——hermix-tools 的 F1 教训）。子 webview 用**独立的
   WebView2 数据目录** → 它就是独立的浏览器进程 → 端口与主窗口天然隔离。
2. **查看器暴露 `web_*` / `webview_*` 工具**（MCP）：
   `web_open` / `web_close` / `webview_state` / `webview_text` /
   `webview_eval` / `webview_click` / `webview_hover` / `webview_navigate` /
   `webview_screenshot`。**用户看到的页面 = agent 操作的页面**（同一个登录态、
   同一块屏幕）。
3. **桌面端 agent 接上查看器**：`$HERMIX_HOME/mcp_servers.json` 里登记
   `http://127.0.0.1:3911/mcp`（HTTP 传输）→ 桌面端 sidecar 启动时用
   `ArcMcpManager` 连上 → 工具以 `hermix-viewer__<tool>` 前缀进入 agent 的
   工具表（`GET /v1/tools` 实测 113 个工具，其中 26 个来自查看器）。

## 为什么桌面端 agent 之前「不调用」查看器的浏览器自动化

**不是能力缺失，是从来没被登记。** 桌面端 agent 的外部工具面只来自
`$HERMIX_HOME/mcp_servers.json`（`hermix-server/src/state.rs` 启动时逐个
`connect()`，把发现的工具注册进 **agent 同一个 `ToolRegistry`**，下一次
`get_definitions()` 就能被 LLM 看到）。查看器的 MCP 端点从来没进过这个列表 →
agent 根本看不到那些工具。

补上登记后立刻生效，**不需要改桌面端任何代码**（协议已经是标准 MCP
JSON-RPC over HTTP，`hermix-mcp` 的 `StreamableHttpTransport` 与查看器的
「POST 一帧 JSON-RPC」实现兼容：inline JSON / SSE 双接受、202 空体、无会话
id 的无状态模式都支持）。

## Alternatives considered

- **只让 agent 用系统浏览器（通道 B，已实现）** —— 用户明确不认：「不在查看器
  里面开浏览器吗？」。而且通道 B 的页面在应用**外面**，用户与 agent 盯的是
  两个窗口，用户登录完还要口头交接。
- **只让桌面端用它自己的浏览器面板**（`browser_panel_open`：应用内 WebView2
  窗口 + 调试端口，agent 用 `browser_*` 驱动）—— 能满足「在 app 里」，但
  **只有桌面端能用**，查看器独立运行时没有；而且面板是独立窗口，不是查看器
  的预览区。两者不冲突，作为补充保留。
- **把浏览器内核抽成共享 crate `hermix-browser`**（长期正解，见
  `2026-10-21-viewer-web-preview-and-browser-kernel.md`）—— 仍要做，但本轮
  先让能力跑通：查看器自己那套 + 桌面端通过 MCP 复用它，**用户看到的效果
  已经是一套**。

## Consequences

- **端口文件在 `EBWebView` 子目录**：`<profile>/EBWebView/DevToolsActivePort`
  （WebView2 把用户数据放在 UDF 的子目录里；系统浏览器是放在 UDF 根下）。
  读端口要**两处都试**。
- **必须非主线程建 webview**：Tauri 的同步命令跑在主线程，而 Windows 上
  在主线程创建 webview 会死锁（wry#583）→ 症状是「窗口开得出来但永远停在
  `about:blank`」。查看器的建窗命令是 `async fn`；MCP 的 HTTP 端点每条连接
  一个线程，也不在主线程上。**MCP stdio 模式没有窗口** → `web_open` 会报
  「需要查看器窗口模式」。
- **agent 开视图时不知道预览区在哪**：它用「上次的位置」摆；前端收到
  `web-opened` 事件后开网页标签、把视图摆到预览区（位置不对时等前端跟上，
  别急着判失败）。
- **真鼠标 ≠ 合成事件**：有些站点的菜单是**悬停**才出现的（超星的用户名 →
  「进入空间 / 退出登录」）。JS 派发的合成 `mouseover` 不一定能触发它们的
  处理器 → 补了 `webview_hover`（CDP `Input.dispatchMouseEvent` +
  `mouseMoved`，浏览器照常派发 hover）。
- **遮挡坑**：页面自己的层叠上下文可能盖住顶栏（超星首页的 `.vue-grid-item`
  z-index 1 压在导航上，鼠标落到「超星读书卡会员」而不是用户名）→ 必要时
  临时提升该元素的 z-index 才能让真指针事件到达目标。
- **安全边界不变**：只放行 http/https；子 webview 无 IPC 权限；独立 profile
  （远程站点 cookie 不落进主窗口 profile）；调试端口只监听回环。
- **多 agent 隔离不变**：通道 B 仍按会话 id 独立 profile；内嵌视图是
  **单例**（一个查看器只有一个内嵌视图）——同一时刻只服务一个 agent 的
  页面操作，多 agent 并行要用通道 B。

## 验证（真机，2026-10-21）

- `web_open` → 页面在查看器预览区渲染；`webview_state` / `webview_text` /
  `webview_eval` / `webview_screenshot`（image/jpeg 内容块 + 落盘）全通。
- 桌面端 agent（sidecar，工具白名单只给 `hermix-viewer__web*`）：
  超星首页 → **真鼠标悬停用户名「汪风」** → 菜单「进入空间 / 退出登录」→
  点「进入空间」→ `i.chaoxing.com/base?ws=1`（个人空间）→ 列出继教课程等
  功能入口。**全程页面就在用户眼前的预览区里。**
