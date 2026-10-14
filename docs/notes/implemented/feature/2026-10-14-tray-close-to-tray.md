# Decision Note: 托盘重做——关窗最小化到托盘 + 持久化 + 完整托盘菜单

Status: implemented — 已拍板（2026-10-14）；关窗去向、托盘状态持久化、托盘菜单扩容

## Decision

托盘从「设置页手动开一次、重启即失、菜单只有显示/退出」重做为**常驻后台能力的完整入口**，三块：

### 1. 关窗去向（核心诉求：关闭程序 ≠ 退出）

- 拦截点：`on_window_event` 的 `CloseRequested`（TitleBar 关闭按钮、splash 的
  sp-close、Alt+F4 都汇到这里）。条件 = 托盘存在 **且** `close_to_tray` 开：
  `prevent_close()` + `hide()`；任一不满足走原生销毁（原行为）。
- 首次隐藏发一条系统通知（`notify_hidden_once`，进程级一次），否则「点了关闭
  程序却没退」会被当成僵尸进程。
- 更新重启不受影响：`restart_app` 走 `app.restart()` 直接退进程，不经窗口关闭。

### 2. 状态持久化（`~/.hermix/desktop.json`）

- `DesktopSettings { tray, close_to_tray }` 一份文件两个开关，默认全开。Rust
  （CloseRequested 判断）与前端（设置页回显）读同一份，不各存一处。
- 托盘开关不再「只活在当前进程」：启动时 setup 按文件恢复托盘；`get_desktop_settings`
  的 `tray` 字段实读托盘存在性（文件说了不算），`close_to_tray` 以文件为唯一真相。
- 默认值变化是刻意的：本产品的 agent 回合/定时任务在后台跑，关窗即杀进程会
  中断任务——「最小化到托盘」必须是出厂行为。

### 3. 托盘菜单（右键）

显示主窗口 / ─ / 正在执行的任务（动态文本+可用态） / 检查更新 / 关于 Hermix
Desktop / ─ / ☑ 点击关闭按钮时最小化到托盘 / 退出。

- 「正在执行的任务」：前端 `App.tsx` 上报流式会话数（`tray_set_task_count`），
  Rust 只改菜单项文本（0 个 = 置灰「没有正在执行的任务」）。点击 = 唤起主窗 +
  emit `hermix:tray-open-tasks`，前端跳到流式会话。
- 「检查更新」：emit `hermix:tray-check-update`，`Sidebar`（useUpdater 的宿主）
  收到后 `checkUpdate` + 打开更新面板——与侧栏菜单按钮同一出口，不建第二条
  更新链路。
- 「关于」：原生 message dialog（tauri-plugin-dialog），不进前端路由。
- 勾选项即设置：翻转直接写 desktop.json，托盘菜单与设置页共用同一持久层。

## Alternatives considered

- **Rust 侧轮询后端会话状态驱动托盘**：托盘要另起一条 HTTP 轮询链路，而前端
  本来就持有 streaming 状态——事件上报一行代码，轮询是重复建设。否。
- **关窗行为存 localStorage**：托盘拦截在 Rust，localStorage 的值 Rust 拿不到，
  势必出现「前端以为最小化、后端直接退出」的分裂。否。
- **托盘菜单塞子菜单显示任务列表**：Tauri v2 菜单无 menu-open 钩子，列表无法
  在展开时刷新，静态快照会撒谎；且菜单项堆叠在 Windows 托盘菜单里难读。入口
  项 + 主窗导航是对的形态。
- **关窗拦截做在前端（TitleBar 拦 close 事件）**：Alt+F4 / splash 关闭绕过前端
  拦截，只有 Rust 的 `CloseRequested` 是全部关闭路径的必经点。

## Consequences

- 流式计数是主窗视角（`streamingSessionId`）：后端定时任务在跑但主窗无流式时，
  托盘显示「没有正在执行的任务」。作为入口项可接受；要精确计数需后端出
  running-sessions 端点，目前不值得。
- splash 阶段点关闭也会被拦截为隐藏（同一 `close()` 路径）：卡启动时应用转入
  后台继续起服务，托盘可恢复可退出。若确要「启动期强杀」，需另设 boot 阶段
  旁路，当前无此需求。
- 老用户升级后托盘图标会**重新出现**（原 `sync_tray` 状态不持久、按新默认全
  开）——想要旧行为去设置页关掉即可，关掉从此是持久的。
- `get_tray_enabled` command 删除（被 `get_desktop_settings` 取代），前端
  `getTrayEnabled` 同步删除；GeneralTab 的「关闭按钮行为」行在托盘关闭时
  只给描述不给可用分支——托盘不存在时 close_to_tray 不参与判断，形同虚设。
