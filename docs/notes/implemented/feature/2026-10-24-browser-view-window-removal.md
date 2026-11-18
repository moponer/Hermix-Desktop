# Decision Note: 浏览器小窗移除——观看面收敛到内置浏览器面板

Status: implemented — 2026-10-24

## Decision

**移除浏览器小窗（`browser-view` 子窗）及其整条 screencast 推流链路；浏览器自动化的人眼观看面收敛到内置浏览器面板（`browser-panel`，WebView2 窗口 + CDP 附着）。**

小窗是 [2026-10-14 P0](../../archived/feature/浏览器虚拟窗口 P0（已删除）.md) 的方案：无头 Edge 恒开 screencast，JPEG 帧流经 sidecar stdout（`browser-frame` 通道）推到桌面端独立子窗，人看监控流、agent 走 CDP 合成输入。内置浏览器面板（`browser_panel_open`）落地后，agent 工具直接附着到用户眼前的可见 WebView2 窗口上驱动——「人在应用里看真实浏览器」取代了「人在小窗里看有损监控流」，小窗失去存在理由。

移除清单（桌面 ↔ sidecar 全链路）：

- 桌面 Rust：`browser_view_controller.rs` 整文件（建窗/几何记忆/`browser_view_open`/`browser_view_set_paused`）、`stdio_bridge.rs` 的 `note_browser_frame` 帧活动检测（≥60s 空窗自动弹小窗）；
- 前端：`BrowserView.tsx`、`browser-view.css`、`main.tsx` 的 `#/browser-view` 分支、标题栏 🌐 按钮、capabilities 的 `browser-view` 窗口；
- hermix-server：stdio 帧转发器 `spawn_browser_frame_forwarder`（订阅 screencast 广播 → base64 → 写 stdout）、`/api/browser-view/paused` 端点；
- hermix-tools：`screencast.rs` 整模块（CDP StartScreencast JPEG q60 + broadcast）、`browser/mod.rs` 会话建立处的两处 `screencast::start`（面板路径与无头回退路径）与 Drop 的 `screencast::shutdown`。

保留（不属于小窗）：`cdp_probe.rs`（N0 批次 WebView2 进程内 CDP 探针，内置浏览器的技术验证工具）、`browser_panel_open`/`browser_panel_close`（内置面板本身）、无头 Edge 回退路径——面板未注册时 agent 工具自起无头 Edge，headless-only 纪律不变，只是这条回退无人眼画面（模型走 AX 树，自足）。

连带作废：

- [2026-10-17 键控多路并行](./2026-10-17-browser-per-session-parallel.md) Decision 的第 3/4/5 条（帧流多路、小窗多标签、自动弹窗去重）；第 1/2 条（会话键控、资源闸）仍在。
- [2026-10-13 提案](../../proposed/feature/2026-10-13-browser-virtual-window-token-discipline.md) 的 P2 档（screencast 接入前端小窗）；P1 Token 纪律 / P3 清理不受影响。

## Alternatives considered

- **只删前端，保留 sidecar 推流** — 否决：小窗没了之后 screencast 编码（JPEG q60、20fps、逐帧 base64 过 stdout）是纯空转 CPU/IO；半删正是「界面上没东西了、机器还在偷偷干活」的同类根因。
- **小窗改为内置面板的画面镜像** — 否决：内置面板本身就是可见窗口，镜像多此一举；screencast 是有损监控流（1280×800、q60），比不上真窗口。
- **保留暂停端点等基础设施备将来** — 否决：YAGNI；git 历史可回溯，重加成本低于维护成本。

## Consequences

- 观看浏览器自动化的唯一入口是内置面板；面板关闭（`browser_panel_close` 或窗口 X）后工具回退无头 Edge，该路径无人眼画面。
- stdio 协议不再有 `browser-frame` 通道；桌面 `stdio-frame` 事件只剩流式/主通道载荷。
- 净删 4 个文件（screencast.rs / browser_view_controller.rs / BrowserView.tsx / browser-view.css）+ 3 处内联逻辑（转发器、暂停端点、弹窗检测），无新增依赖、无行为迁移。
- `<hermix_home>/browser-view-geometry.json`（小窗几何记忆）成为孤儿文件，不再读写；不做迁移代码。
