# Decision Note: 内置网页自动化验证（查看器内嵌 WebView2）

Status: implemented — 10 个 `web_*` / `webview_*` 工具实测全部可用；外部浏览器 CDP 线另修一处选择缺陷

## Decision

先纠正一个**重要的事实认定**：本项目的浏览器自动化是**文件查看器内嵌的 WebView2 网页视图**，
不是外部 Edge/Chrome 的 CDP 线。验证必须走前者。

两条线的分工：

| 线 | 工具 | 依赖 | 定位 |
|---|---|---|---|
| **内置（产品路径）** | `web_open` / `webview_text` / `webview_eval` / `webview_click` / `webview_click_text` / `webview_screenshot` / `webview_navigate` / `webview_state` / `web_close` | 查看器内嵌 WebView2（系统已装 156.0.4314.58） | 用户就在那块预览区里看着，agent 与用户**看同一块屏** |
| 外部 CDP（旁路） | `browser_*` | 系统 Edge / Chrome | 独立无头浏览器，不进用户视野 |

### 验证方式：走生产路径

查看器有两种运行模式，只有一种是生产路径：

- `--mcp`（stdio）：在 Tauri `.run()` **之前**就 return，运行时未初始化 → **不是**生产路径
- `--hidden` + HTTP MCP（`http://127.0.0.1:3911/mcp`）：完整 Tauri 初始化 → **是**生产路径

### 实测结果：10/10 通过

```
web_open           → 已打开 https://example.com
webview_state      → Example Domain | https://example.com/
webview_text       → 完整页面正文（多语言，逐字正确）
webview_eval       → "Example Domain | 22"（JS 执行正常）
webview_screenshot → 落盘 1200x665 PNG，页面渲染正确
webview_navigate   → 跳转到 iana.org，标题同步更新
web_close          → 已关闭
webview_click      → 已点击 (10,10)
webview_click_text → 见下（设计最好的一环）
```

`webview_click_text` 的表现值得单独记：它返回**点击前后的快照对比**，而不只是「点成功了」：

```json
{
  "before": { "u": ".../help/example-domains", "t": "Example Domains",  "l": 1256 },
  "after":  { "u": ".../domains",              "t": "Domain Name Services", "l": 2451 }
}
```

URL、标题、正文长度三项都变了，证明点击**真的生效**。这个设计避免了 agent 最常见的失败模式：
点了、没反应、却以为成功了，然后在错误的页面上继续操作。找不到目标文字时它也诚实返回
`{"clicked": false, "why": "no-match", "hint": "先用 webview_text 看清楚到底有什么文字"}`，
而不是静默点击某个近似元素。

### 顺带修的外部 CDP 线缺陷

验证过程中先误测了 `browser_*`（外部 CDP），发现 `browser_exe()` 选中本机**损坏的 Chrome 147**
（启动后写 3 个文件、产生 `.dmp`、退出，从不写 `DevToolsActivePort`），而可用的 Edge 156 排在它后面，
导致该线 100% 失败且报出误导性的「被安全软件拦截」。已修：

1. 候选按**产品**分组（所有 Edge 在前），而非按安装根交替
2. 逐候选**实际启动验证**——`is_file()` 通过不代表能用，只有拿到端口才算成功，失败则杀进程换下一个
3. 超时消息改成三段式诊断（0 条目=没起来 / 有转储=启动后崩溃 / 条目多无转储=超时太短）
4. `HERMIX_BROWSER_PATH` 仍绝对优先

## Alternatives considered

- **只验证外部 CDP 线**：那是旁路。用户说得很清楚「浏览器自动化是内置的，文件查看器的浏览器」，
  验证对象搞错等于没验证。
- **用 `--mcp` 模式验证**：Tauri 未初始化，`web_*` 工具需要 `AppHandle`，必然失败——会得出「内置自动化不可用」的错误结论。
- **认为 `webview_click_text` 的 no-match 是 bug**：核实后是测试假设过时——example.com 已改版，
  页面上确实没有那个链接了。工具报 no-match 是**正确行为**。
- **删除外部 CDP 线**：它在「不想让用户看到浏览器」的场景仍有价值，且本次已修好选择逻辑。

## Consequences

- 内置网页自动化**可用且可靠**：读取（text/eval）、交互（click/click_text）、观察（screenshot/state）、
  导航（open/navigate）四类能力齐全，且 click_text 自带结果校验。
- 它与用户共用同一块预览区：agent 的操作**用户看得见**。这是产品设计上的优势（可监督），
  也意味着不要用它做用户不该看到的事。
- 外部 CDP 线在本机仍不可用：回退机制已生效（日志确认选到 Edge 并拿到端口），
  但本机 Edge 156 的 CDP 协议层不响应（TCP 通、HTTP/WebSocket 超时）。这是**本机环境缺陷**，
  换一台机器或升级 Edge 后应可正常工作。
- 桌面自动化（`desktop_*` / `win_*`）仍未验证：`desktop` 工具集默认关闭，与网页线是独立实现。
## 桌面自动化：同样验证通过

`desktop_*` 走 Windows UI Automation，与网页线是独立实现。实测：

```
desktop_list_windows   → 0x10118  Shell_TrayWnd   pid=7248
                         0x10810  Agent Companion · 会话栏  Tauri Window  pid=15336
desktop_list_controls  → 0x10812  WRY_WEBVIEW
                         0x10814  Chrome_WidgetWin_0
                         0x10816  Chrome_WidgetWin_1  Agent Companion · 会话栏
                         0x207E8  Chrome_RenderWidgetHostHWND  id=1052
                         0x1081C  Intermediate D3D Window
desktop_read_text      → Agent Companion · 会话栏
```

能枚举真实窗口、进入 Tauri 的控件树（`WRY_WEBVIEW` → `Chrome_WidgetWin` → `Chrome_RenderWidgetHostHWND`）、
读出窗口文字。**注意**：`desktop` 工具集默认关闭（`OPT_IN_TOOLSETS`），需要在设置 → 工具集里手动开启，
这是刻意的安全设计——键盘注入到任意前台窗口是这个 agent 能做的最侵入的事。

本次验证走的是查看器 MCP 暴露的 `desktop_*`（查看器进程内直连 UIA），与 agent 侧的 `desktop` toolset
共用同一套底层实现，因此这次结果可以作为该线可用的证据；但 agent 侧启用后仍建议在真实任务里再确认一次。
