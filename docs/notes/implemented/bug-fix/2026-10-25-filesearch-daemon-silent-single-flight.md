# Decision Note: 文件索引守护进程静默化 + 宿主侧单飞拉起 + 启动预热

Status: implemented — 黑框不弹、并发拉起合并为一次、索引随 server 启动就开始建，不再等第一次搜索

## Decision

用户实测三个灾难现象（2026-10-25）：启动后常弹黑色控制台框、程序开了几小时
索引却要等搜索时才开始建、搜索框打字弹一个黑框 + 打开索引设置又弹一个。
根因与修法：

### 1. 黑框 = 控制台子系统的 exe 被可见拉起（三处入口全堵）

`hermix-filesearch.exe` 是控制台子系统，宿主 `ShellExecuteExW("runas")` 用
`SW_SHOWNORMAL` 拉起它、开机计划任务直接跑它、用户双击也跑它——三条路都会
亮一个黑框。用户对不明黑框的本能是关掉它，关掉 = daemon 被杀。

- daemon 侧（`daemon.rs`）：serve 模式在**快照装载之前**就
  `GetConsoleWindow() + ShowWindow(SW_HIDE)` 自藏。必须在装载前——装载
  220MB 快照要一两分钟，藏晚了等于黑框挂一分钟（实测验证：藏晚时
  MainWindowHandle 非 0，挪到装载前后为 0）。
- 宿主侧（`filesearch.rs`）：`spawn_elevated` 的 `nShow` 改 `SW_HIDE`。
- `install_autostart_task` 里拉 PowerShell 注册计划任务的子进程加
  `CREATE_NO_WINDOW`。

stdio 模式（开发联调）不藏，开发控制台不受影响。

### 2. 连环弹框/连环互杀 = `ensure_helper` 无并发防护

搜索窗每 2s 轮询一次 `/v1/filesearch/status`，每个请求都走
`ensure_helper`。helper 未就绪时（快照装载期 endpoint.json 还没写出来），
每个并发请求都会删 endpoint.json 再拉一个提权实例，而新实例的交接逻辑
（stop-request）会请走正在装载快照的旧实例——互相残杀、黑框连环弹、索引
永远建不完。

`ensure_helper` 现在全程持 `tokio::sync::Mutex` 单飞锁（锁内复查探活），
另加 60s 拉起失败冷却：UAC 被拒后 status 轮询直接复用失败结论，不再
"拒一次弹一次"。

### 3. 开几小时不建索引 = 只有首次搜索才拉起

开机自启计划任务注册后要等**下一次登录**才首次生效（实测
`schtasks` 查询"上次结果 267011 = 尚未运行"），而此前唯一的应用内触发点是
首次搜索请求。现在 `run_server_with_state` 启动 5s 后后台预热
`ensure_helper`：任务已在跑则只探活，没在跑就补拉一次。索引随应用启动
就开始建/续增量。

## Alternatives considered

- **exe 改 `windows_subsystem = "windows"`**：`--status` 模式要给脚本打
  JSON 到 stdout，无控制台子系统后输出丢失。自藏窗口保留控制台子系统，
  三条拉起路径一处代码全覆盖。
- **计划任务包 `conhost.exe --headless` / wscript 包装**：只为消灭登录时
  不到一秒的窗口残影引入包装层；daemon 自藏后残影已可忽略，不值。
- **`ensure_helper` 失败后靠前端退避重试**：把并发语义交给每个调用方各自
  实现，必有漏网（status 轮询、filter 回放、search 三个入口）。收口在
  服务端单飞锁一处。
- **预热无条件每次 app 启动都 UAC**：已跑（计划任务/上次会话）时探活
  即返回，不弹 UAC；只有确实没在跑才补拉。

## Consequences

- daemon 崩溃或被任务管理器杀掉后，最长 5s（下次 server 启动）或一次搜索
  内自愈，自愈那一刻可能弹一次 UAC（提权拉起的固有代价，静默无法绕过）。
- 藏掉控制台后 daemon 的 eprintln 日志不可见（写进隐藏控制台）；协议面
  （status/endpoint.json）是唯一可观测面，排障靠 `--status` 与回环协议。
- 60s 冷却期间搜索会收到明确错误文案（"上次拉起失败，60 秒后自动重试"），
  而不是卡 90s 等超时——行为变化，但用户感知更诚实。
