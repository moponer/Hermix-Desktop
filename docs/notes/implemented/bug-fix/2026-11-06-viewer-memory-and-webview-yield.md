# Decision Note: 查看器两项优化 —— 内嵌网页多视图保活池 + 浮层相交让位 / 预热空闲降级 + 页位图 LRU

Status: implemented — 对应两个用户实报问题：①多页面/多窗口/多文件内存占用过大；②轻量模式下点标题栏菜单后内嵌浏览器界面空白（疑似被遮挡）。

## Decision

**问题二（菜单点开 → 内嵌浏览器空白）**，两层修复：

1. **期望态对账（修持续空白）**：`webview_host.rs` 新增 `WEB_VISIBLE: AtomicBool` 作为网页期望可见性的唯一事实。`web_set_visible` 先记期望态再套用现存视图；`open_sync`（含幂等路径）建完视图按期望态收尾——浮层 hide 与 webview 销毁重建无论怎么交错都收敛。前端 `App.tsx` 的让位 effect 失败重试一次（200ms），浮层收起 300ms 后经 `web_state`（新增 `visible` 字段）对账，实例在却不可见则幂等补发 show。此前 show 是 fire-and-forget 且错误被吞，撞上 `web_open` 销毁重建窗口就永久丢失（= 用户看到的持续空白）。
2. **相交才让位（修固有闪烁的面积）**：子 webview 是独立 HWND，z-index 无效，让位本身不可取消；但改为**浮层根与网页占位矩形相交才 hide**（`WEB_YIELD_SELECTORS` 中心清单：menubar-pop / ctxmenu / syspanel-overlay / set-overlay / bpanel-overlay / tab-list-pop），点不重叠的小浮层不再整块闪网页。让位期间占位区 `is-yield` 上 `--bg-secondary` 暗面底色，读作"暂时盖住"而不是空白。

**问题一（内存占用）**，四项：

1. **预热实例空闲降级**：`--hidden` 预热实例 15 分钟内未被真正使用（主窗口没 show 过 / 没开过子 webview / 没开过浮窗 / agent 没让开过文件——`mark_used` 各入口打标）→ 进程自行退出；server 首连失败会按需重拉（connect_mcp_servers_with_retry）。背景：桌面端 2026-10-29 已取消开机预拉起，此项兜的是 server 侧按需拉起后常驻的隐藏实例。
2. **内嵌网页多视图保活池**：子 webview 从单例 label `webview` 改为每标签一个 `web-<tabId>`（agent 路径独立命名空间 `agent-N` 防与前端数字 tab id 撞 label）。切标签 `web_release`（hide 保活，页面状态/滚动/登录态保留），标签真正关闭由 App 的标签对账 `web_close` 销毁；保活池上限 `WEB_KEEPALIVE=3`，创建时 LRU 淘汰最久未用的。活动视图指针 `ACTIVE_WEB` 是 agent（CDP / webview_* MCP 工具）的目标——用户与 agent 看同一块的语义不变。
3. **页位图 LRU**：`DocPager` 的 painted 集从"只增不减"改为上限 `PAINTED_CAP=12` 的 LRU（observer 双向跟踪，离开预渲染范围最久的页先淘汰，正被看到的页不淘汰）；`NearSlot` 缩略图从 sticky 改双向切换（滚走即卸载位图）。一张整页 canvas 在高清屏上是十几 MB 级，50 页文档全保留逼近 GB 级。
4. **pptx 上限收紧**：100MB→50MB（解析副本是字节数倍），超限文案指引「打开方式」用系统应用查看。

## Alternatives considered

- **调 z-index / 换 iframe**：2026-10-21 笔记已定案拒绝——HWND 与 DOM 不同层，z-index 无效；CSP `frame-src` + X-Frame-Options 双堵 iframe。
- **无限保留所有网页标签的子 webview**：每个是一整棵 WebView2 渲染进程（30-80MB），与"内存占用合理"的目标直接冲突——保活池 + LRU 是两个目标的折中。
- **完全保留"切标签销毁重建"**：切换丢页面状态且销毁等待期出现空白窗口期（正是问题二的放大器），保留。
- **预热实例直接取消**：server 侧首连自拉逻辑依赖快速就绪的实例，冷启动代价回到每次打开；空闲降级是两全。
- **桌面端开机预热回归**：2026-10-29 已定案按需拉起，不回头。

## Consequences

- `web_open` / `web_bounds` / `web_exec` / `web_close` 命令签名带可选 `id`；`web-navigated` / `web-opened` 事件载荷从纯 URL 变为 `{id, url}` JSON。前端（WebPane / App 标签模型）与 Rust 两侧同步改，`apps/viewer` 内闭环，无其它 crate 依赖。
- 被保活池淘汰的网页标签切回来会重新加载（丢页面状态）；上限 3 可在 `WEB_KEEPALIVE` 调整。
- DocPager 滚回被淘汰的页会重画（重画比爆内存便宜）。
- 验证：`cargo check -p hermix-viewer --all-targets` 零错误；viewer 单测 98/98（测试二进制按 2026-10-21 笔记流程注入 comctl32 v6 清单后运行）；viewer 前端 tsc 在本沙箱被 pnpm 软链遍历策略挡住（"不受信任的装入点"），改用 esbuild 逐文件语法校验通过——实机上仍应跑 `tsc --noEmit` + `vite build` 复核。
