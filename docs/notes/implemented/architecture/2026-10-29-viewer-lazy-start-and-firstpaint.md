# Decision Note: viewer 不再开机预拉起 + 首屏把预览栈拆出去

Status: implemented — 2026-10-29 交付

## Decision

用户报「程序跑起来一堆 WebView2 进程、CPU 25%+、内存 700MB+」，并提出要查查看器
renderer 那 300MB。先按宿主父进程链把全机 `msedgewebview2.exe` 归并实测（6 逻辑核，
5s/15s 两轮采样）：

| 归属 | 进程数 | 实时 CPU | 工作集 |
|---|---|---|---|
| spiker-tauri | 6 | **83%** | 177MB |
| GameViewer | 7 | **70%** | 80MB |
| wb-switch-rust | 7 | 1.6% | 227MB |
| prevhost（Explorer 预览窗格） | 10 | 0% | 286MB |
| **hermix-desktop（主窗）** | 6 | 0.3% | 110MB |
| **hermix-viewer（查看器）** | 6 | 0.3% | 97MB |

25% 那笔账是 spiker-tauri + GameViewer；700MB 是**提交（commit）**口径下
hermix 两棵树的合计（主窗 223MB + 查看器 491MB = 714MB），不是常驻。Rust 侧本身只有
`hermix-desktop` 17MB + `hermix.exe --stdio` 28.5MB。

在这个前提上做了两件与本仓代码有关的事：

**1）viewer 不再开机预拉起（省一整棵进程树）**

旧行为：主程序起来 10s 后静默 spawn `hermix-viewer.exe --hidden`，为的是让 agent 的
viewer MCP 工具一开始就在工具列表里。代价是第二棵完整的 WebView2 树（browser + gpu +
2 utility + renderer + crashpad = 6 进程 / 97MB 工作集 / ~490MB 提交），而多数会话根本
不看文件。

现在两边按需：
- 用户侧：`open_in_viewer` / 文件关联本来就 spawn（未改）；
- agent 侧：server 的 viewer MCP 首连失败时**自己拉起**（`state.rs::spawn_local_viewer`，
  `--hidden` 不变、`DETACHED_PROCESS`、`HERMIX_NO_SPAWN_VIEWER=1` 可关），随后 5s 一档
  快速复查 4 次（viewer 从进程起到 MCP 就绪实测 3–8s），仍失败才退回 60s 节奏。

**2）viewer 首屏把预览栈拆出去**

`main.tsx` 静态 import `PinWindow`，而 `PinWindow` 从 `@hermix/preview` 的 barrel 取
`PreviewSurface`；`App.tsx` / `fileSource.ts` / `AiSidebar.tsx` 也从 barrel 取三个纯函数。
barrel 把整条预览栈（PreviewSurface / pdf.js / docx-preview / xlsx）拖进首屏图，于是
`index.html` 的 `modulepreload` 一次性预载 2.66MB——而"浏览文件"一个解析器都不需要。

改：`packages/preview` 增加 `./resolveKind`、`./lib/textDecode`、`./lib/previewUtils`
子路径出口，四个首屏模块改走子路径；`PinWindow` 改 `React.lazy`（它只在 `pin-*` 浮窗
出现时才需要）。

## Diagnosis: 查看器 renderer 的 310MB 是什么

用 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222` 起 viewer，CDP
直接读页面（scripts 见下）：

```
JS heap used   9.4 MB   (total 13.4 MB, limit 4.4 GB)
DOM nodes      3,644    (documents 1)
resources      74 req / 5,748 KB   ← 含 PreviewSurface 898KB + PdfView 427KB
window         1200×769 @dpr1, deviceMemory 16
```

- **不是 JS 堆、不是 DOM、不是缓存**：9.4MB / 3.6k 节点，我们的代码没有留东西。
- 对照实验全部排除：新建进程即 310MB（不是预览历史残留）；可见窗口 309MB（不是
  `--hidden` 导致）；把主程序那整套 `--js-flags` / `--disable-features` 给 viewer，
  308MB（不是浏览器参数缺失）。主窗 renderer 只有 36MB 提交 / 28MB 常驻。
- 结论：这 300MB 是 Chromium 渲染进程的**本地（C++）提交**，常驻只在 26–105MB 之间
  浮动（Windows 会修剪）。应用层改不动它——能改的只有"存在几棵渲染进程树"和
  "每棵首屏拉多少代码"，这两条都已落地。

## Alternatives considered

- **保留预拉起、只做懒加载** —— 少 1.45MB 首屏 JS，但那 6 个进程 / 97MB 常驻照付；
  用户要的是看得见的占用。
- **主程序侧轮询 viewer 进程再拉起** —— 需要跨进程握手；server 的 MCP 首连失败本来
  就是"需要 viewer"的最准信号，直接在那里补位更短。
- **给 viewer 也上主程序那套 Chromium flags** —— 实测对 310MB 无效（308MB），只增加
  两处配置漂移面，不做。
- **只改 barrel，不动 PinWindow** —— `App.tsx` 的 `resolveKind` 也是 barrel 入口，
  单改一处 preload 清单不会变；四处一起改才有实测效果。

## Consequences

- 不打开文件、不派与文件相关的任务时，机器上少 6 个 WebView2 进程、少 97MB 工作集。
- agent 第一次需要 viewer 时多等约 5s（首个 5s 复查窗口内连上则不感知）。
- viewer 首屏 JS：2.66MB → **1.21MB**，`modulepreload` 里预览栈 chunk 从 4 个 → **0 个**
  （`index.html` 已无 PreviewSurface / PdfView / docx-preview / workbook）。dist 总量
  仍是 9.8MB（重型解析器仍在包里，只是不再首屏加载）。
- 取证命令（可复跑）：`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222`
  起 viewer → `GET http://127.0.0.1:9222/json/list` → CDP `Runtime.evaluate`
  `performance.memory` / `Memory.getDOMCounters` / `Performance.getMetrics`。
- 未做：给 WebView2 加渲染进程内存上限（Chromium 无此开关）；把 UI 换成原生栈（那是
  产品路线决策，不是修 bug）。
