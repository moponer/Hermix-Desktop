# Decision Note: 退出清理收口 + 后端离线可见化

Status: implemented — 关浏览器面板不再杀后端；退出清理收敛到一条路径；断线/启动失败都有可见提示

## Decision

### 1. 退出清理只有一条路径（`src-tauri/src/shutdown.rs`）

此前清理逻辑散在三处，且**没有一处覆盖全部退出路径**：

| 位置 | 做了什么 | 漏了 |
| --- | --- | --- |
| `on_window_event` 的 `Destroyed` | `sidecar.stop()` | 只停 sidecar，不管索引守护进程；且**对任意窗口生效** |
| 托盘「退出」 | sidecar + 写 filesearch 停止请求 | `app.exit()` 之外的退出路径全不经过它 |
| `RunEvent` | 完全没接 | —— |

现在 `shutdown::cleanup(app)` 是唯一实现，幂等（`AtomicBool`），由
`RunEvent::ExitRequested` / `RunEvent::Exit` / 托盘退出 / **主窗**销毁共同调用。
内容：sidecar 优雅 shutdown 帧 → 3s → 强杀并释放 Job Object（KILL_ON_JOB_CLOSE
带上孙进程）；索引守护进程写 `stop-request.json`（它是提权进程，`tasklist`
杀不动，只能请它自己走看门狗）。

`Builder::run(context)` 改成 `build(context).run(|app, event| …)`：`RunEvent`
是所有退出路径的必经点，窗口 `Destroyed` 不是（`app.exit()` 根本不触发它）。

### 2. 修一个真 bug：关内置浏览器面板会停掉整个后端

`Destroyed` 分支原先不判断窗口 label。`browser-panel` 是**真销毁**
（`browser_panel_close` → `window.close()`，与截图覆盖窗的 `hide()` 不同），
于是关一次浏览器面板就把 `hermix.exe` 停了：桥被 `mark_dead()`，此后每个
请求都吃 503 `backend offline`，界面没有任何提示 —— 用户只能"点项目报错"
才发现。现在只认 `main` 窗口。

### 3. 后端离线必须自己说出来（前端）

断线此前是绝对静默的：唯一的指示器在**侧栏底部**，侧栏一折叠/不在对话页
就看不见；`TitleBarConnectionIndicator` / `BackendStatus` 两个组件写了却
**从未被挂载**（死代码）。改动：

- `TitleBar` 右栏挂 `TitleBarConnectionIndicator`（常驻，全窗口唯一恒定可见位）。
- `TitleBar` 中栏在 `offline` 时改为「后端未连接 · 重启」chip（含 `WifiOff`
  图标、点击即 `restart()`、重启中显示「正在重启后端…」）。中栏平时放会话
  标题，后端断了本来也没法用这个会话，占位没有任何信息损失。
- `useBackendConnection`：在线探测 30s → 6s（`backend_ready` 走本机 stdio
  管道，毫秒级）；`online → offline` 与反向跃迁各弹一次 toast，
  20s 冷却防抖动刷屏；**冷启动宽限 30s**，宽限内保持 `connecting`（不闪
  "离线"），超时仍连不上就如实转 `offline`（此前会永远转圈）。
- 探测顺序改为「先 `backend_ready` 再 `getConfig`」：离线时省掉一次必然
  503 的往返。
- `rest.ts` 把桥层 `503 backend offline` 翻成「本地后端未连接（服务未启动
  或正在重启）」，不再把英文协议串丢进 toast。
- sidecar `start()` 失败此前**静默**（`is_ok()` 丢掉了错误串）。现在走
  `match`，把失败原因（含子进程 stderr）写进启动页文案并 emit
  `hermix:backend-error`，前端立即 toast + 关 splash。这条路径对应
  "二进制缺失 / 被杀软拦 / 库被占"，正是"下一次启动出问题"的现场。

### 4. 单实例锁按 exe 绝对路径打键（同日实测事故）

现场：安装版（`D:\data\Hermix Desktop\hermix-desktop.exe`）点 X 后隐藏到托盘、
进程仍在，于是 `cargo run` 的 dev 进程在 `acquire_or_notify()` 拿到
`ERROR_ALREADY_EXISTS` 后 `return` —— 控制台只有 `Running …hermix-desktop.exe`
紧接着结束，无任何解释，看起来就是"dev 起不来"。

修法两条：
- 命名对象加 **exe 路径的 FNV-1a 指纹**（`Local\HermixDesktop.SingleInstance.<hash>`），
  锁的含义从"本机只许一个 Hermix"收窄为"同一个可执行文件只许一个"。
  dev（`target\debug\`）与安装版因此天然并存。不用 `DefaultHasher`：它只保证
  同一进程内一致，std 换算法会让新旧实例认不出彼此。
- 冲突分支不再是哑的 `return`：打印「已有实例在运行，已唤起它 / 它不支持唤起」，
  dev 控制台直接可见。

## Alternatives considered

- **`on_window_event` 里按窗口 label 白名单过滤，保留 `Destroyed` 作为唯一清理点**：
  `app.exit()`、任务管理器结束、注销都不触发 `Destroyed`，仍需 `RunEvent` 兜底；
  两套入口反而要写两次幂等。收敛到一处更省。
- **放弃 Job Object，只在退出时 `taskkill /T /F`**：Job Object 是内核级保证，
  连主进程被强杀都生效；`taskkill` 反过来会漏掉"主进程已死"的场景。
  现在两者并存：Job Object 管兜底，`cleanup()` 管优雅。
- **退出时一并杀掉 `hermix-viewer.exe`**：查看器是独立文件工具，用户关掉主
  程序后继续看文件是正常用法；它的残留只来自"用户没关它的窗口"，
  不属于本进程该代为决定的事。（它自己 `hide()` 而非 `close()` 的窗口不受影响。）
- **离线提示做成全局浮层横幅（像 `denial-banner`）**：浮层会与审批卡/拒绝横幅
  抢同一块顶部区域，且需要新的关闭手势。标题栏中栏本来就是空着的状态位，
  零新增 UI 面。
- **探测间隔压到 1-2s**：离线时每次探测要等满 2s 超时，高频率会让探测请求
  首尾相接。6s 是"30s 内必然发现"与"不自我拥塞"的折中。

## Consequences

- 关内置浏览器面板、关截图覆盖窗都不再影响后端；`Destroyed` 只对主窗生效。
- 从托盘「退出」与点 X（关闭到托盘关闭时）走同一份清理，索引守护进程会在
  10s 内自己刷盘退出（提权进程的固有代价，不能同步等它）。
- Windows 关机/注销时 `ExitRequested` 未必触发：sidecar 由 Job Object 兜住，
  索引守护进程留给下次启动的火并（它本身是设计常驻的服务，有开机计划任务）。
- `browser-panel` 的 webview 销毁不再触发 `stdio_bridge::mark_dead()`，此前
  那条"面板一关全站 503"的误导性故障消失。
- 启动失败现在有明确原因（sidecar stderr），但 splash 会立即关闭 —— 用户看到
  的是主界面 + 错误 toast + 标题栏「后端未连接 · 重启」，比原来的空白等待好。
- 6s 探测意味着一次 IPC 往返每 6s，同一台机器上并存的自定义应用轮询（30s）
  仍是主要开销方，量级无变化。
- **dev 与安装版现在可以同时跑，但它们共享同一个 `~/.hermix`**
  （`portable_env::find_dev_data_dir` 恒返回 `None`，dev 不另开数据根）：
  两套 sidecar 会读写同一个 `sessions.db`（WAL 可并发，但定时任务/审批会各算
  一份）。要彻底隔离，显式设置 `HERMIX_HOME` 再跑其一。
