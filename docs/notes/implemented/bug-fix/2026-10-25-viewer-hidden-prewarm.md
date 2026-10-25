# Decision Note: 查看器 `--hidden` 预热模式 —— 启动主程序绝不弹查看器窗口

Status: implemented — 桌面端启动 10s 后仍预热查看器（MCP 就绪），但窗口保持隐藏，用户打开文件/点"打开文件管理器"时才弹出

## Decision

用户硬约束（2026-10-25）："绝对不可以启动桌面主程序时，弹出文件查看器"。
此前 6860e430 的自动拉起直接 spawn `hermix-viewer.exe`，而 viewer 的窗口
按 `tauri.conf.json` 的 `visible: true` 建成即显——每次启动主程序都会
弹出查看器窗口。

保住「viewer MCP（3911）随主程序就绪」的前提下消除弹窗：

- **viewer 窗口改成建成隐藏**（`visible: false`）+ `setup` 里非
  `--hidden` 启动立即 `show()`——普通启动（双击/开文件）视觉与原先等价。
- **`--hidden` 启动参数**：跳过 show，进程常驻后台，MCP HTTP 端点照常起。
- **桌面端自动拉起改带 `--hidden`**（`apps/desktop-tauri/src-tauri/src/lib.rs`）。
- **单实例投递把窗口叫回来**：
  - 已有实例在跑时，带 `--hidden` 的第二次 spawn **静默退出、不投递**——
    空投递的语义是"把窗口叫到前面"，照投会把藏着的预热实例弹出窗外。
  - 投递真实路径（打开文件）时 unminimize/show/set_focus——预热/最小化
    实例收到文件都能弹到前面。

## Alternatives considered

- **撤掉自动拉起**：阿黎的浏览器/文件自动化依赖 viewer MCP，viewer 缺席时
  第一个任务卡在"查看器工具不在工具列表里"（6860e430 的修复动机）。撤掉
  等于把那个 bug 请回来；server 的 60s×30 MCP 重试只是兜底不是解法。
- **托盘化隐藏**：引入托盘图标与菜单一套新表面积；单实例投递唤回已够用，
  YAGNI。
- **--mcp stdio 模式当预热**：stdio 通道要宿主持有管道，与 server 经 HTTP
  连 3911 的架构不匹配。

## Consequences

- 预热实例常驻约 100-200MB 内存（webview 已建）——预热的固有代价，换来
  MCP 工具开机即用。
- `visible: false` + setup show 之间有毫秒级窗口空档，与原先建成即显相比
  无感知差异；若未来 setup 变慢（新插件），首帧可能晚于进程创建。
- 用户双击 viewer exe 拉起已藏实例时走空投递路径（弹窗 + 资源管理器
  模式），行为与以前一致。
