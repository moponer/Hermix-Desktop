# Decision Note: OS 自动化全自研——浏览器 CDP + 桌面 UIA，零外部运行时

Status: implemented — 已拍板并开工（2026-10-13）；正文记录设计、全部已识别的坑与落地事实

## Decision

在 hermix 内**全自研**两条 OS 自动化能力线，不引入任何外部运行时（Python / Node / .NET），
不依赖 GitHub / PyPI / npm 在用户机器上的可达性，全部能力编译进现有 Rust workspace：

1. **浏览器自动化**：`chromiumoxide`（CDP 客户端，唯一新增依赖）驱动**系统自带 Edge**。
   有头/无头同一套代码路径，无头为默认（无头就是浏览器的"虚拟窗口"，不引入 Win32 虚拟桌面）。
2. **桌面自动化**：`windows` crate 直接调 **UI Automation（UIA）COM 接口**，
   以 Invoke/Value 等 control pattern 为主、SendInput 为兜底；专用 worker 线程串行执行。

范围 v1 = 约 12 个工具（浏览器 6 + 桌面 6，见下），注册进 `hermix-tools` 既有工具注册表，
走既有审批引擎（`risk gate` / `check_approval`），默认 profile **不含**这些工具——
与 2026-09-24 插件内核笔记（MCP 降级为可选插件、未配置零成本）同一纪律，设置面板显式开启。

预估总量 ~2,000–2,500 行（不含测试），符合 10 万行纪律；二进制增量 ~3MB。

### 为什么全自研是这次的正确答案

- 上个月刚删除 19,420 行自研 CUA（commit `c6442032`）。删的原因不是"自研错了"，
  而是范围失控：OCR、DXGI 捕获、UIA 树全量遍历、云浏览器、WinForms 深度支持全都在做。
  这次 v1 用**硬性行数上限**（12 个工具、~2,500 行）圈住范围，只做 LLM 高频路径。
- 外部项目（Windows-MCP / FlaUI-MCP / ahk-mcp / Playwright MCP）全部需要用户机器上
  多一个运行时或多一条外网下载链路；我们的核心卖点是**零安装便携**，这个 trade 不划算。
- 我们真正缺的知识（UIA COM 初始化模型、CDP 会话管理、僵尸进程清理）都是**坑**不是**量**，
  本笔记把它们逐条列死（见"坑清单"），自研的风险就收敛到可控。

### 线一：浏览器自动化（先做，最独立）

- 驱动方式：`msedge.exe --headless=new`（默认）或有头，`--remote-debugging-port=0`
  让系统分配端口，从用户数据目录的 `DevToolsActivePort` 文件读真实端口。
  **绝不写死 9222**——那是刚删掉的 `cdp_bootstrap` 的孤儿占用事故根源（见坑 F1）。
- 客户端：`chromiumoxide 0.9`（2026-02 仍在发版，tokio 原生，480 万下载，
  CDP 协议细节由它兜住：二进制帧、sessionId 路由、事件缓冲——手写这些是纯粹的坑）。
- 会话隔离：每次会话独立 `--user-data-dir`（临时目录，会话结束即清）；
  需要持久登录态时用**专用** automation profile 目录，文档写明与用户日常 profile 完全隔离。
- 进程生命周期：浏览器进程绑 **Job Object（KILL_ON_JOB_CLOSE）**——hermix 进程无论
  崩溃、被杀还是正常退出，浏览器必死，没有孤儿。这取代旧 `browser_cleanup.rs` 的
  PID 文件 + taskkill 方案（同一目的，但 Job Object 由内核保证，无竞态）。
- 有头模式：同代码路径，去掉 `--headless`；用独立 user-data-dir，绝不附着用户日常浏览器。

### 线二：桌面自动化（UIA）

- 接口层：`windows` crate（workspace 已依赖 0.58，只需追加 features
  `Win32_UI_Accessibility`、`Win32_System_Com`、`Win32_UI_Input_KeyboardAndMouse`、
  `Win32_Graphics_Gdi`）。**不用**第三方 `uiautomation` crate（单作者、停更风险）。
- 执行模型：**所有 UIA COM 调用集中在一个专用 worker 线程**（actor 模式），
  工具调用经 channel 进出、`recv_timeout` 超时保护（详见坑 A、J）。
- 交互优先级：**control pattern（Invoke/Value/Toggle）> SendInput 键盘 > 坐标点击**。
  pattern 不动鼠标不抢焦点，是 agent 场景的正确默认；坐标点击是最后手段且必须
  先校验目标窗口仍在前台（坑 B）。

### v1 工具清单（硬性上限，加工具须改本笔记）

浏览器（6）：`browser_navigate` / `browser_snapshot`（a11y 树文本化）/ `browser_click` /
`browser_type` / `browser_screenshot` / `browser_close`。
桌面（6）：`win_list_windows` / `win_snapshot`（UIA 树带 ref）/ `win_invoke`（pattern）/
`win_set_value`（pattern）/ `win_send_keys` / `win_read_text`。

明确**不做**（v1 边界）：OCR、屏幕级 DX 捕获、拖拽/手势、多虚拟桌面、云浏览器、
Firefox/WebKit（CDP 只保 Chromium 系）、`ahk_eval` 式任意代码逃生舱。

## 坑清单（每条都对应一个必须落地的防御，评审时逐条勾）

### COM / UIA 体系

- **A. MTA/STA 初始化错配**（历史事故：刚删的 CUA 曾在此整 run 拖死——
  `hermix-agent/src/agent_builder.rs` memory_bridge_tests 注释记录）。UIA COM 对象
  与线程单元模型强绑定：worker 线程启动时显式 `CoInitializeEx(COINIT_MULTITHREADED)`
  （MTA；UIA 官方推荐客户端用 MTA），线程退出 `CoUninitialize`。所有 UIA 调用
  只在 worker 内做，**绝不在 tokio worker / agent loop 线程上直接调**——
  禁止任何"顺手 block_on 进 COM"的写法。
- **B. UIPI / 前台抢占**：UIA Invoke 对提权窗口（管理员 CMD 等）静默失败；
  SendInput 只能打进前台窗口，点击坐标前必须校验目标窗口 rect 与前台一致，
  否则按键会打进别的窗口（用户正在打字时 agent 抢键盘 = 事故）。防御：
  pattern 优先于 SendInput；SendInput 前 `GetForegroundWindow` 二次校验；
  UIPI 失败返回明确错误"目标窗口以管理员运行"而非静默无操作。
- **C. UIA 树爆炸**：全量树 dump 在大型应用（VSCode/Chrome 自身）可达数万节点、
  数 MB 文本——直接回给 LLM 就是 ctx 爆炸（项目纪律：一切可能撑爆 ctx 的数据
  必须经子代理隔离或结构化裁剪）。防御：snapshot 强制 `max_depth` + `max_nodes`
  （默认 12 / 800，超出截断并附 truncation 标记），只输出 Role/Name/ref/State
  四列，用户可按需加深度。ref 映射表存 worker 会话内，随快照失效（stale ref
  返回"快照过期，请重新 snapshot"而不是乱点）。
- **D. COM 生命周期/RAII**：`IUIAutomationElement` 等裸指针忘 Release = 泄漏 +
  崩溃。防御：`windows::core` 的接口包装本身就是 RAII（Drop 自动 Release），
  禁止 `std::mem::transmute` / `ManuallyDrop` 绕过；worker 崩溃时整个线程重启
  而不是带病继续。
- **J. COM 调用卡死**：UIA 跨进程调用目标进程无响应时会阻塞到超时
  （默认可达 3s+ / 某些属性无限等）。防御：worker actor 收到请求后
  `recv_timeout` 包住整体工具调用（默认 8s），超时返回工具错误；可另开
  watchdog 线程监测 worker 心跳，连续卡死则杀线程重建（宁可丢一次操作，
  不可拖死 agent loop——与 2026-10-11 看门狗事故同一哲学）。

### 浏览器 / CDP 体系

- **F1. 端口冲突与孤儿浏览器**（旧 `cdp_bootstrap` 事故：9222 被残留实例占住，
  下次启动误判 AlreadyRunning）。防御：`--remote-debugging-port=0`（系统分配）
  + `DevToolsActivePort` 文件读端口 + **Job Object KILL_ON_JOB_CLOSE** 兜底；
  不再使用 PID 文件 + taskkill（有竞态）。
- **F2. Edge 用户数据目录锁**：附着用户日常 profile 必然撞
  `SingletonLock`（Edge 正在运行时第二个实例起不来）。防御：永远用独立
  `--user-data-dir`（临时或专用 automation 目录），v1 不提供"接管当前浏览器"。
- **F3. headless 差异**：`--headless=new` 与有头在视口、下载、部分网站
  反爬行为上有差异。防御：默认视口 1280x800 显式声明；`browser_screenshot`
  是主要观察通道；网站反爬导致的失败原样返回给 LLM，不伪装成功。
- **F4. CDP 大响应撑爆 ctx**：整页 HTML / 全量 DOM dump 轻松 500KB+。
  防御：snapshot 只输出 a11y 树文本化结果（与桌面 UIA 同一套深度/节点上限），
  不提供 `get_full_html` 这类工具；`browser_evaluate` 返回值加长度上限（64KB，
  超出截断标记）。
- **F5. 下载/弹窗**：无头模式下载落盘位置与弹窗（beforeunload/permission）
  会挂起导航。防御：独立下载目录（`hermix_home/downloads/browser/`）+
  CDP `Page.setDownloadBehavior` 显式指路；dialog 类事件自动 accept 并记入
  工具返回（LLM 知道发生了什么）。
- **F6. `Binary::as_ref()` 不解码**（`browser_screenshot` 曾因此写出假 PNG）：
  CDP 把二进制回传为 base64 **文本**。`chromiumoxide_types::Binary` 同时实现了
  `AsRef<str>` 与 `AsRef<[u8]>`，而后者是 `self.0.as_bytes()` —— 看着像解码，
  其实是 base64 字符串的 ASCII。防御：**任何** CDP 二进制都要显式
  `base64::engine::general_purpose::STANDARD.decode()`；`browser::decode_cdp_binary`
  是唯一入口，回归测试用 1x1 PNG 钉住"`as_ref` 不等于解码"这个反直觉点。

### 进程 / 系统层

- **P1. Job Object 继承污染**：Job handle 若可继承，子进程（浏览器再拉起的
  渲染进程等）行为要明确——KILL_ON_JOB_CLOSE 只杀 job 内进程，浏览器及其
  子进程天然同 job，正确；但**不能**把 hermix 自己已经属于的 job 传给浏览器
  （嵌套 job 限制）。防御：`CREATE_BREAKAWAY_FROM_JOB` + 独立 job 分配，
  job 句柄不可继承。
- **P2. 会话边界**：工具超时/失败后浏览器 tab 要不要关？策略：会话对象
  显式关闭才关 tab；工具级失败只回错误。浏览器进程死亡由下次工具调用探测
  （连接失败→自动重启浏览器并报告，不做隐式重试超过 1 次）。

## Alternatives considered

- **Windows-MCP（Python/uvx，8.8k★）**：功能最全但需 Python 3.14 + uv + PyPI 链路，
  与零安装便携冲突；GitHub 不可达地区首装体验差（README 自认可能超时）。
  17 项依赖含 posthog 遥测。**被击败理由：运行时成本**。保留为将来可选 MCP 插件
  （hermix-mcp 客户端现成，接入只需一行配置），不是互斥关系。
- **FlaUI-MCP（.NET 8 自包含 exe，91★）**：单 exe 携带可行，但 3 个月项目
  边界踩坑深度存疑，且引入 .NET 侧 skill 维护（自己 build + pin + 审计），
  与"编译进 workspace"的单一技术栈相比多一条发布链。**被击败理由：多一条
  发布/审计链，收益不比自研 UIA 最小集大**。
- **ahk-mcp / win32-mcp-server**：Python+AHK 双运行时或 OCR 重依赖，更重。
- **Playwright MCP / chrome-devtools-mcp（Node）**：浏览器能力最强但引入
  node.exe（~80MB）+ npm 链路，浏览器场景 CDP 全部能力 chromiumoxide 直接给。
- **继续不带任何 OS 自动化**：手机端已能遥控 PC，OS 自动化是用户显式需求
  （主人拍板方向），放弃等于丢功能。
- **复活已删除的 hermix-cua**：删它是因范围失控（OCR/DXGI/云浏览器）。
  本笔记继承其 git 历史里的真金（窗口枚举、UIA 遍历逻辑可参考
  `git show c6442032~1:crates/hermix-cua/src/uia.rs`），但以 12 工具硬上限
  重建，不复活 crate。

## Consequences

- **得到**：零外部运行时、零外网链路的 OS 自动化；体积增量 ~3MB；
  审批/风控/ctx 纪律全部复用既有基建；两条线相互独立可分步交付
  （建议先浏览器线，~800 行，一周内可出）。
- **代价**：UIA 边界 case（老 Win32 控件、自绘应用、多语言窗口名）需要
  长期积累，短于 Windows-MCP 765 commits 的沉淀；chromiumoxide 是唯一
  新依赖（需评审其 tokio/reqwest 版本冲突）；~2,500 行新增源码。
- **失败模式与止损**：若 UIA worker 反复卡死/崩溃（坑 J 防御触发），
  降级方案 = 桌面线退回"仅 list_windows + read_text"只读集；
  若 CDP 线被证明不稳定（Edge 版本差异），浏览器线可整体替换为
  Windows-MCP MCP 插件（一行配置），笔记保留论证。
- **重启条件（对应 simplification 纪律的反向锚）**：若 v1 上线 8 周内
  工具调用成功率 < 70% 或月均新增行数超 500 行（范围失控信号），
  本笔记必须重审——回退到外部项目方案不丢人，丢的是纪律。

### 落地事实（2026-10-13 交付时）

代码在 `crates/hermix-tools/src/tools/os_auto/`，9 个文件约 2,930 行
（浏览器线 1,340 行 / 桌面线 1,460 行 / 共享骨架 130 行），比预估的
~2,500 行高约 17%，仍在同一量级，12 工具硬上限未破。

**依赖决策（与上文正文的一处偏差，以本节为准）**：chromiumoxide 锁 **0.8**
而非 0.9。0.9 把 reqwest 下限抬到 ^0.13，会在工作区已有的 0.12 旁边
再塞进一整套 hyper/rustls——而这两条线根本不发任何对外 HTTP 请求
（只讲 CDP over loopback WebSocket）。0.8 依赖 `reqwest ^0.12`
（default-features=false，已在树里），新增成本只有 async-tungstenite
与 cdp/types 三个小 crate。升 0.9 的前提是全工作区 reqwest 统一。

**API 现实与设计的三处被迫修正**（都是读源码后改的，不是猜的）：

1. `windows` 0.58 **没有** `IUIAutomation::ElementFromRuntimeId`，也没有
   `GetRuntimeId`。所以 ref 表直接存 COM 元素句柄
   `HashMap<String, IUIAutomationElement>`，每次 snapshot 整体替换；
   "快照过期" 判定退化为"表里没有这个 ref"，比代际计数器更简单也更准。
2. `chromiumoxide::Element::new` 是 crate 私有，ref 无法还原成 `Element`。
   于是 ref 路径统一走 `DOM.resolveNode` + `Runtime.callFunctionOn`，
   `user_gesture=true`（不开的话 Chrome 把脚本点击当不可信，一批按钮直接
   没反应）；`selector` 路径仍走高阶 `Element` API。
3. `Page::title()` 不存在（是 `get_title()`），导航是 `goto()` 而非
   `navigate()`。

**默认关闭的实现方式**（2026-10-16 起仅 `desktop`；`browser` 已移入
essential 硬地板，见
`docs/notes/implemented/architecture/2026-10-16-toolset-visibility-browser-cronjob-hard-floor.md`）：
`hermix_home/enabled_toolsets.json` 在全新安装时
是通配 `["*"]`，含义是"我们本来就知道的工具全开"，不是"同意让 agent 接管
我的鼠标键盘"。因此 `OPT_IN_TOOLSETS = ["desktop"]` 由
`os_auto::apply_opt_in_defaults` 处理，且启动路径上有**两处** reconcile
（`register_builtin_tools` 与 hermix-server 的
`apply_persisted_toolset_state`）必须调用同一个 helper——只改一处会在重启
后被另一处悄悄打开。hermix-server 侧有一条回归测试钉住这个契约。

**两条坑没有做到 100%，如实记录**：

- 坑 F5 的下载目录已落地（`Browser.setDownloadBehavior` →
  `hermix_home/downloads/browser/`），但 **JS 对话框自动 accept 没接**。
  `Handler` 事件流要单独订阅对话框事件，v1 没做；补偿是
  `browser_navigate` 全程带硬超时，弹窗不会把导航挂死。
- 坑 B 的提权窗口只能"给准提示"不能"确诊"：`Win32_Security` feature
  没开，拿不到 token API，所以 Invoke/Value 失败时返回的是
  `[PERMISSION_DENIED]` + "目标可能以管理员运行"的提示，而不是一次真实的
  提权比对。SendInput 短写计数是真实的（那来自内核）。

**验证到什么程度**：`cargo check -p hermix-tools --all-targets` 通过，
os_auto 单元测试（渲染/截断/文法/端口解析/ref 失效等）全绿，
hermix-server 的 opt-in 契约测试全绿。**没有**做过真机端到端：
需要用户装 hermix 包、在设置里打开工具集、手动调一次
`browser_navigate → browser_snapshot → browser_click` 与
`win_list_windows → win_snapshot → win_invoke` 才能确认 CDP 帧与
UIA provider 的实际行为。

