# Decision Note: 关闭程序留下僵尸进程（Hermix Desktop + hermix.exe 双双残留，之后启动静默无反应）

Status: implemented — 2026-10-30 交付：主窗销毁即收尾并退出进程、退出流程启动后 watchdog 不再重生 sidecar、单实例在唤起无果时轮询抢锁接管启动

## Decision

用户实报：关掉程序后任务管理器里同时留着 `Hermix Desktop.exe` 与 `hermix.exe`，再点图标「启动不了」（没有任何窗口出现，也没有报错）。

`<install>/data/logs/desktop.log`（`D:\data\Hermix Desktop\data\logs\desktop.log`）里有完整链条：

```
cleanup: stopping sidecar (trigger=main-window-destroyed)   ← 主窗销毁，sidecar 已停
stdio: shutdown frame received, exiting gracefully
Sidecar exited gracefully (code=0, 1571ms)
Sidecar health check failed (1/6) … (6/6)   bridge_ready=false child_alive=false
Watchdog: restarting sidecar (attempt 1/5, backoff 2s)
Started sidecar in stdio mode               ← 15s 后又被拉回来
===== desktop start pid=1329236 =====
[single-instance] 同一程序已有实例在运行，已唤起它的主窗口，本进程退出。
```

三个缺陷串成一条链：

1. **主窗销毁不等于进程退出**。`on_window_event(Destroyed)` 里用
   `app.webview_windows().len() <= 1` 判断「没有别的窗口了才收尾」，但启动 6s
   后预热的**截图覆盖窗**（`screenshot-overlay`：隐藏、复用、永不销毁）本身就是
   一个 webview 窗口 —— 条件永远不成立。于是 cleanup 是否执行取决于覆盖窗预热
   有没有跑完（启动 6s 内关窗会收尾，之后就不会），行为不稳定。
2. **进程不退**。tao 只在「所有窗口都销毁」时才结束事件循环，那个隐藏覆盖窗
   让它一直活着；`RunEvent::ExitRequested` / `Exit` 因此永远不触发。用户得到的是
   一个没有界面、没有托盘可唤（主窗已销毁，`show_main_window` 是 `if let Some`
   空转）的进程。
3. **watchdog 与退出流程对打**。`cleanup()` 用 shutdown 帧停掉 sidecar 后，
   看护循环把「子进程没了、桥没接上」读成故障，6 次（~15s）后把 sidecar 重新
   拉起 —— 后台服务是主动停的，却按故障恢复。

修法：

- **`shutdown::cleanup_and_exit(app, trigger)`**（`shutdown.rs`）：`cleanup()` +
  `app.exit(0)` 收成一个动作。主窗 `Destroyed` 直接调用它，取代原先的
  `len() <= 1` 前置条件。语义定为「主窗 = 应用本身，它一销毁就退出」；浏览器
  面板 / 文件搜索窗都是主窗的附属，跟着一起走。托盘模式不受影响（那条路径在
  `CloseRequested` 已 `prevent_close` + 隐藏，根本不会走到 `Destroyed`）。
- **`shutdown::is_cleaned()`**：`sidecar::start_watchdog` 每轮开头检查，退出流程
  已启动就 `break`（打一行「退出流程已启动，停止看护」）。
- **单实例可自愈**（`single_instance.rs`）：老实例若是僵尸（主窗已销毁），
  `SetEvent` 过去它弹不出任何东西。两处补齐：
  - 监听线程收到唤起事件时，`get_webview_window("main")` 为 `None` →
    认定自己是失去界面的残留进程，`cleanup_and_exit`，把锁让出来；
  - 新进程通知完不立刻退出，而是 100ms × 60 轮（6s，大于 `cleanup()` 里
    sidecar 的 5s 宽限）继续 `CreateMutexW` 抢锁，抢到就照常启动。
    于是「点一次图标」即可接管，不需要先手动结束残留进程。

## Alternatives considered

- **只把 `len() <= 1` 改成「排除 `screenshot-overlay` 后计数」**：仍然靠「还有
  别的窗口吗」来推断要不要退出，下一个隐藏辅助窗（文件搜索窗的隐藏态、以后
  新增的预热窗）又会把它弄坏。主窗是唯一的产品界面，直接以它为准更稳。
- **保留 `len() <= 1`，只让覆盖窗在空闲时销毁**：截图冷启动又要付 WebView2
  建窗 + 页面加载（这正是 2026-10-23 重写时要消掉的成本），且覆盖窗的销毁时
  机与「用户是否还会截图」无关，无法判断。
- **改 `RunEvent::ExitRequested` 里统一退出**：它压根不触发 —— 触发条件就是
  「窗口全关」，正是这里卡住的那一步。只能前置到主窗 `Destroyed`。
- **单实例用真实所有权（`CreateMutexW(initial_owner=true)` + `WaitForSingleObject`）**：
  能等到锁真正释放，但 Tauri 的 `restart()` 是「先 spawn 新进程再退本进程」，
  需要精确的交接时序；当前「对象存在即有人在跑」的语义 + 轮询抢锁同样能自愈，
  改动面更小。留作将来需要「多实例排队」时的方案。
- **在僵尸实例里重建主窗**：要复制 `tauri.conf.json` 里整套窗口配置
  （尺寸 / 装饰 / incognito / 初始 URL），等于维护第二份窗口定义；而「主窗都没了」
  本来就不是正常工作态，让它退出更诚实。

## Consequences

- 关主窗（托盘关闭 / 未开托盘 / `close_to_tray=false`）一律等价于退出程序：
  浏览器面板、文件搜索窗、进行中的截图会话一并结束。想在后台留驻请开托盘 +
  「点击关闭按钮时最小化到托盘」（默认全开）。
- 退出时主线程会同步执行 `cleanup()`（sidecar 优雅退出宽限 5s，超时强杀），
  窗口已消失，这段时间用户看不到任何东西；单实例的 6s 抢锁预算正是为这段
  等待留的余量。
- `app.exit(0)` 在 `Destroyed` 处理中调用一次、在 Tauri 自己的退出流程里可能再
  触发一次 `Destroyed`；`cleanup()` 幂等（`CLEANED` 短路），重复 `exit` 无害。
- 单实例的正常路径多了最多 6s 的隐藏进程存活时间（有实例活着、弹出了它的
  窗口时）。只在「再次点图标」这一动作上，且不可见。
- 仍有一处未闭合：桌面进程被**强杀**（任务管理器结束任务）时靠 Job Object 的
  `KILL_ON_JOB_CLOSE` 带走 sidecar；Job 创建失败只在 `desktop.log` 留一行警告，
  没有二次兜底。这是既有设计（见 `sidecar.rs` 头注），本次未改。
