# Decision Note: 桌面端单实例守护（Win32 命名互斥锁）

Status: implemented — 2026-10-25 落地，`apps/desktop-tauri/src-tauri/src/single_instance.rs`

## Decision

桌面端进程级单实例：第二个 `hermix-desktop.exe` 启动时拿不到命名互斥锁
`Local\HermixDesktop.SingleInstance`，改为打开命名事件
`Local\HermixDesktop.ShowWindow` 并 `SetEvent`，然后立即退出——退出点在
`Builder` / sidecar / 全局热键 / viewer 自动拉起**全部副作用之前**
（`lib.rs::run()` 第一行）。已有实例的监听线程收到事件后对主窗做
`unminimize + show + focus` 三连（主窗可能被关到托盘隐藏）。

实现是约 80 行 Win32 原生代码，不引新依赖。

`commands::restart_app` 在 `app.restart()` 前显式调用
`single_instance::release()`：Tauri 的重启是「先 spawn 新进程再退出本进程」，
不提前放锁，新进程会把自己误判成第二实例直接退出（表现为点重启后应用
整个消失）。updater 的替换脚本不受影响——它等旧进程退出后才
`Start-Process` 新 exe，无竞态。

## Alternatives considered

- **tauri-plugin-single-instance（官方插件）**：能力等价还带命令行转发
  （deep link 场景）。否决：本应用没有 deep link / 单实例命令行转发需求；
  `windows` crate 已在依赖里且所需 feature（Threading 等）全开，为 40 行
  逻辑加一个插件违反六步决策阶梯第 3 步（原生平台）。
- **FindWindowW 按窗口标题定位已有实例**：第二实例直接找主窗弹前。
  否决：标题可能被前端改动，字符串匹配脆弱；命名事件是 OS 级通道，
  与窗口标题解耦。
- **`Global\` 命名空间**：跨登录会话也单实例。否决：不同会话的桌面互不可见
  窗口，跨会话弹窗请求路由不到可见窗口；`Local\`（每会话一份）与
  "同一桌面上不该有两个实例"的物理约束精确对齐。

## Consequences

- 互斥锁创建失败时放行启动（宁可极端情况双开，不把单开挡死）——极端
  情况退回旧行为，不会比现在更糟。
- 老版本实例在跑（无 ShowWindow 事件）时，新实例通知不了弹窗，静默退出；
  行为退化为"纯单实例、不弹窗"，仅存在于升级后的第一次覆盖场景。
- `SetForegroundWindow` 受 Windows 前台锁约束：调用方不是前台进程时可能
  只闪任务栏不抢焦点（与 tauri-plugin-single-instance 同样的平台限制）。
- 双开相关的所有问题（sidecar 服务端口抢占、全局热键冲突、viewer 重复
  拉起、两个窗口写同一 sessions.db）从根上消失，无需逐项排查。
