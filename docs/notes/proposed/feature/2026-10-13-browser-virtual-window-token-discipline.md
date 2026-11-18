# Decision Note: 浏览器自动化升级——Token 纪律 + 交互补全 + 清理

Status: proposed — 2026-10-13 提案；**P0 已于 2026-10-14 拆出实现**，见
[implemented: browser-virtual-window-p0](../implemented/feature/浏览器虚拟窗口 P0（已删除）.md)
（其"评审修正"一节记录了 P0 实现对本篇的三处偏离与裁决：逃生门已裁决废弃、
"不看不推"未兑现待实测、现状列高估）。本篇剩余范围：P1 Token 纪律 /
P2 交互补全 / P3 清理。承接 [native-os-automation](../implemented/feature/2026-10-13-native-os-automation.md)，
与 [Space 与登录态](./2026-10-07-browser-space-and-login-state.md)（N2）维度不同、互补。

> 原 P0（独立 Tauri 子窗口）内容已移入 implemented 笔记，此处不再重复。
> 原文对 ego 系 / agent-view 的调研（已核实真实）仍为 P1 设计依据，保留在
> "参考实现"一节。

## Decision

浏览器自动化升级分三档推进：**P1 Token 纪律**（快照定向提取、截图不喂模型、上下文预算硬顶）、**P2 交互补全**（screencast 接入前端小窗、操作过程可见）、**P3 清理**（ego 系 / agent-view 等历史实现退场）。P0（独立 Tauri 子窗口）已拆出并于 2026-10-14 实现，本篇不再重复其内容。

四条用户诉求各自对应一个明确机制，而不是「让模型少看屏幕」这一句口号：省 token → 定向提取 + 截图不进 prompt；不抢鼠标键盘 → 强制 headless；看到操作过程 → screencast 帧 + 应用内小窗；不用费 token 的方式 → Rust 原生运行时。

## 问题

用户四诉求逐条对现状：

| 诉求 | 现状 | 缺口 |
|---|---|---|
| 省 token | a11y 树 + 24000 字符硬顶 + 截图附件不进 prompt（均已实现） | 快照全量重发；无定向提取；screenshot 仍是模型可滥用入口 |
| 虚拟化窗口，不抢鼠标键盘 | headless=new 已有；Job Object 无孤儿保证已有 | `headless=false` 会弹真浏览器窗口抢焦点/被误关；该参数存在即诱惑 |
| 看到操作过程 | screencast.rs 原型已完成（JPEG 帧 + broadcast + 逐帧 ack） | **未接前端**，应用内没有承载画面的小窗 |
| 不用费 token 的方式 | Rust 原生零运行时 | 无vision 入口其实是优点，须防倒退（截图喂模型） |

## 2026 开源调研（2026-10-13 检索）

| 项目 | 维护方 / 协议 | 定位 | 交互模型 | Token 特征 | 结论 |
|---|---|---|---|---|---|
| Playwright MCP | Microsoft / Apache-2.0 | 跨浏览器流程 | ARIA snapshot + element ref | schema 每轮进 context；0.0.x 版本线 | **架构同构验证**（我们的 ref 方案与它一致）；不引进 |
| chrome-devtools-mcp | Google Chrome DevTools 团队 / Apache-2.0 | 诊断（perf/Lighthouse/network） | 29 tools，`--slim` 砍到 3 个 | 社区实测"extremely token heavy"；官方优化：`filePath` 写盘、`evaluate_script` 定向 JSON 替代 snapshot、`--no-page-id-routing` | 借鉴 evaluate 定向提取 + 写盘两项；不引进（Node 运行时 + 遥测默认开） |
| browser-use | 社区 93k★ / MIT | 自主网页任务 | DOM 蒸馏（剥广告/不可见/元数据）+ vision fallback | 2026 生产共识：DOM 主 vision 辅，纯 vision 贵一个量级 | 借鉴蒸馏过滤思想；不引进（Python 底座） |
| agent-browser | Vercel Labs | CLI 优先 | CLI + skill stub | 代表"反 MCP"趋势：MCP tool 描述每轮进 context，CLI 按需加载 | 方向印证：Hermix 原生 Rust 工具即"CLI 级" |
| Stagehand | Browserbase | 代码+自然语言 | act / extract / observe 分层 | observe 先看后动 | extract 分层思想进 browser_extract 设计 |

行业事实：

- 旧 headless 已删除（Chrome 132+），`--headless=new` 是唯一无头模式；screencast 在其下正常出帧，
  a11y 树与有头完全一致。
- Windows 无头**不需要** Xvfb 类虚拟显示（那是 Linux 方案）。headless=new + screencast 就是
  "虚拟窗口"的标准答案。
- CDP 端口绑 127.0.0.1 是硬边界（[N2 已裁决](./2026-10-07-browser-space-and-login-state.md)），本篇所有
  新增能力不触碰这条线。

## 现状评估（可复用的地基，别重造）

`crates/hermix-tools/src/tools/os_auto/browser/`（2715 行，chromiumoxide + 系统 Edge + CDP）：

- `launch.rs` — 随机调试端口（`--remote-debugging-port=0` + `DevToolsActivePort` 轮询）、一次性
  profile、`--window-size` 声明视口。**改点：删 headless 分支**。
- `job.rs` — Job Object `KILL_ON_JOB_CLOSE`，内核级无孤儿。
- `a11y.rs` — AX 树渲染（role/name/value/states + 自绘 ref），永不返回原始 HTML。
- `screencast.rs` — `Page.startScreencast` JPEG q60 → broadcast(4) → 逐帧 ack。**原型已通，
  只差消费者**。
- `mod.rs` — 6 工具 + `Session`（tokio Mutex + Drop 清理 profile）。

## 方案设计

### P0 · 独立 Tauri 子窗口（约 2 天，做完用户三诉求全部可用）

形态**已定：B·独立 Tauri 子窗口**（2026-10-13 用户选定）。可拖出主窗口、可上第二显示器、
主窗口最小化时仍在。范式复用 `screenshot_controller.rs` 的成熟实现（运行时建窗、先建后亮
防白屏、后台编码、incognito 对齐主窗）——不是新机制，是第二次使用。

1. `mod.rs` / `launch.rs`：删 `headless` 参数与 `--headless=new` 分支选择逻辑，恒定无头；
   `browser_navigate` schema 移除 `headless` 字段（**删字段而非留默认**——留着就是诱惑）。
2. `Session` 持有 screencast receiver；session 建立即 `Page.startScreencast`
   （JPEG q60，1280×800 上限）。
3. 新增 `browser_view_controller.rs`：
   - 运行时 `WebviewWindowBuilder` 建 label=`browser-view` 子窗，路由 `#/browser-view`；
     `incognito(true)` 对齐主窗（画面不落持久 profile）；`visible(false)` 先建后亮——
     前端 Ready 才 show，与截图覆盖窗同款防白屏；decorations 保留系统标题栏
     （拖动/缩放/最大化白拿，自定义标题栏留 P2 美化）；关闭时把 position + inner_size
     落盘，重开还原。
   - 帧出口：常驻 task 消费 screencast receiver → base64 JPEG →
     `emit_to("browser-view", "browser-frame", {data, seq, ts})`。子窗不存在时 task
     休眠（receiver 停消费，broadcast(4) 自动丢旧帧——重开即最新帧，零成本）。
   - **不看不推**：小窗最小化 / 被遮挡 / 前端 `document.hidden` 时，子窗发
     `browser-frame-pause` 指令，Rust 侧 `Page.stopScreencast`（彻底停编码）；
     恢复可见时 restart。screencast 全程不落盘——唯一写盘是用户点「保存当前帧」的单张 PNG。
     ⚠️ ego-browser 的实测教训：它走 WGC/FFmpeg 后端，窗口切后台后停流会导致回前台
     反复重建、画面抖动，因此它选择切后台**不**停流。我们是 CDP restart（成本毫秒级、
     无重建抖动），但仍需 P0 实测确认，若抖动则退化为"只降 every_nth_frame 不停流"。
   - **窗口与 session 生命周期解耦**：关小窗 ≠ 关浏览器。Edge 只在 `browser_close`
     工具或 hermix 退出（Job Object 兜底）时终结。
4. 主窗口入口：顶栏「浏览器小窗」按钮；`browser_navigate` 首次调用时小窗未开则自动拉起
   （设置提供"自动开窗"开关）。
5. 子窗前端 `BrowserView.tsx`：canvas 渲染（`createImageBitmap` + `drawImage` 循环，
   不建 DOM img 节点）；标题栏显示 URL / 连接状态 / 帧率；按钮「全屏 / 暂停 /
   保存当前帧 / 关闭」。
6. 逃生门放设置页（**用户主动点击才抢焦点**，模型永远拿不到这个能力）：「在真窗口中打开」——
   一次性 `--headless=false` 新 session，用完即焚。

### P1 · Token 纪律（约 1-2 天，最大头是增量 diff）

1. **可交互过滤**（默认 on）：默认只发 button/link/textbox/combobox/checkbox/radio/
   menuitem/tab/option + 带名 heading；generic 与无名 text 折叠为一行计数
   （`[+37 generic nodes hidden]`）。`all=true` 时展开。browser-use 蒸馏思想的本家化。
   手段对齐 agent-view CLI：`--compact`（实测砍 40-60%）、`--count`、`--max-lines`，
   外加一个 300ms 级 AX 树缓存（连续两次 snapshot 未超时则复用，`dom→click→dom`
   实测 17ms 往返的同类优化）。
2. **增量快照**：首帧全量，之后返回 diff——按 `(role, name, backendNodeId)` 三元组比对，
   输出 `+新增 / -消失 / ~状态变化` 三组 ref 行。多步任务重复元素不再反复进 context。
   实测预期省 50-70%（待 P1 落地后统计校准）。
3. **`browser_extract`**（借鉴 chrome-devtools-mcp `evaluate_script`）：模型给 JS 函数体，
   页面内执行，`return` 的 JSON 序列化回传（上限 8000 字符 + 5s 超时）。拿字段/列表/表格
   不再需要整页快照。**边界**：仅 http(s) origin 可用；`file://`/`data:` 页面禁用；
   JS 里禁碰 `window.__hermix*` 命名空间（防未来注桥）。
4. **`browser_screenshot` 对模型关门**：从模型工具集移除（注册层 disable，代码保留供
   「保存当前帧」按钮复用）。工具描述里删掉"image attached"暗示，杜绝模型走 vision 路线。
5. **大结果写盘**：任何 browser_* 结果超过 8000 字符 → 写 `<hermix_home>/browser-dumps/`，
   回传前 2000 字符 + 文件路径 + 「需要更多用 browser_extract 定向取」。

### P2 · 交互补全（约 0.5 天）

`browser_scroll`（ref/direction+pixels）、`browser_wait`（ref 出现 / URL 含子串 / 毫秒）、
`browser_back`、`browser_tabs`（list/switch/new/close）。全是 CDP 单命令封装，
schema 各一行。补完后模型不再需要"点不到就截图看看"。

### P3 · 清理

`git rm crates/hermix-tools/src/backends/browser.rs`（`backends/mod.rs` 无 `pub mod browser;`，
子模块不存在，零编译接触面）。删前 `grep -r "backends::browser"` 终确认真无引用。

## Alternatives considered

- **引入 Playwright / browser-use 托管浏览器** —— 否决：Node/Python 运行时违背 os_auto
  "零外部运行时"内核；且 N2 批次评审已判"换库不会让 snapshot 更小"。
- **引入 MCP 形态（chrome-devtools-mcp 等）** —— 否决：MCP tool schema 每轮进 context，
  恰是用户点名"费 token 的方式"；其遥测默认开启亦不合本地优先产品定位。
- **vision 主路线（截图喂多模态模型）** —— 否决：2026 生产共识 DOM 主 vision 辅；
  截图 token 贵一个量级且点击定位更差。画中画给人看已覆盖"看画面"需求。
- **Xvfb / 虚拟显示** —— 否决：Windows 不需要；headless=new 即虚拟窗口。
- **云浏览器（Browserbase 类）** —— 否决：本地桌面场景无意义，且外传页面数据。
- **保留 headless 参数只改默认值** —— 否决：留着字段模型就会用；schema 是模型的行为边界，
  不是配置面。

## Consequences

- 获得：浏览器全程不现形（无窗口、不抢键鼠、无孤儿），操作画面实时可见于应用内小窗；
  浏览器链路的 prompt 成本预计降一个量级（P1 后实测校准）；模型彻底失去"偷懒截图"的入口。
- 代价：P0 起 `headless=false` 消失（工期按独立子窗口计，约 2 天），极端调试场景（需要真实窗口指纹的站点）只能走设置页
  一次性逃生门。这是可接受的收缩：那种场景本就罕见，且真窗口抢焦点违背产品承诺。
- 风险：增量 diff 的节点身份依赖 backendNodeId 稳定性——SPA 重渲染会全量翻转，
  退化路径 = 检测到 diff 超过 60% 时自动回退全量并告知模型。
- 风险：screencast 的 CPU 成本集中在 Edge 侧 JPEG 编码（静止页零开销，动画页 30fps 拉满）。
  三道闸门：broadcast 丢旧帧 / 子窗关闭或最小化时 stopScreencast 彻底停编码 /
  P0 落地实测 Edge CPU 占比，超 5% 则降质量（q60→q40）或缩分辨率（1280×800→640×400，
  监看场景足够，成本砍 70%+）。帧流全程不落盘，唯一写盘是用户主动保存的单帧 PNG。
- 前置：无。P0 不依赖 N2（Space/登录态），两者可并行合入。

## 参考实现（2026-10-14 调研）

**ego 系——同款方案的成熟实现，重点研究对象**：
- `CitroLabs/ego-lite`（给 agent 用的 Chromium，官网原话"让 agent 用你已登录的浏览器，
  而不打扰你"——与本篇"不抢键鼠"同旨）+ 官方 macOS App
- `fisfzy/ego-browser`（DSH 插件版，观察窗完整实现，v0.4.0 已适配 Windows）
- **观察窗范式**（直接对标我们的 P0）：右下角 🌐 常驻小球 → 点开是 agent 当前页面
  **实况直播** + 顶部标签条 + 🕘 历史抽屉（按时间回看轨迹）+ 滚轮缩放/拖动平移/双击复位；
  v0.5.0 起**监控窗鼠标直操**——用户可在观察窗里直接点击/滚动/输入（含中文 IME）接管
  真实浏览器。这是"看得见 + 随时接手"的产品完整体，我们的独立子窗是它的 OS 级增强版
  （可拖出主窗口、上第二屏）。
- **技术选型一字不差**：画面后端 = `cdp: Page.startScreencast` JPEG 默认 20FPS；
  SSE 推流 + `/api/ego/input` 输入路由；`captureBackend=auto|cdp|ffmpeg`（FFmpeg 仅用于
  Linux 无显示环境）；前端 frameCache 帧缓存上限 + worker 单实例守卫 + 崩溃自重启 +
  120s worker lease 兜底。
- **工具集设计照抄清单**：ego_snapshot（语义树带 `[ref=N]`，同我们的 ref 方案）/
  ego_navigate（复用 tab）/ wait 四件套（selector/url/response/固定毫秒——正是 P2）/
  ego_js（页面求值——正是 P1 的 browser_extract）/ ego_cdp（原始 CDP 逃生舱）/
  ego_space_open（任务空间=隔离+登录态继承=N2）/ ego_captcha、ego_dialog、
  ego_auth_flush（登录落盘）。**ego_screenshot 的语义值得学：返回 PNG 路径"可交给
  vision 工具"——可选而非强制，图默认不进上下文。**
- **实测教训**（已同步进 P0）：ego 的 WGC/FFmpeg 后端在窗口切后台后停流，回前台会
  反复重建导致抖动，故它选择切后台不停流、仅面板关闭才停。我们 CDP restart 成本
  毫秒级，但仍需实测。

**Steel-browser**（steel-dev/steel-browser，开源 server+UI）：live session viewer +
session replay + **human handoff**（人接管 2FA/支付后交还 agent，run 无缝继续）。
云架构不适用本地桌面，但"人接管"正是我们设置页逃生门的产品化表达，P2 可考虑
把「保存当前帧」升级为「接管控制权」按钮（暂停 agent 输入、开放子窗直操）。

**agent-view**（petukhovart/agent-view，CLI）：token 优化手段清单已抄入 P1
（--compact 40-60% / --diff / --count / --max-lines / screenshot --crop --scale /
300ms AX 缓存）。另注意它证实：**Tauri 的 WebView 没有 CDP 能力**（Windows 有
WebView2 才有），我们"外部 Edge + screencast"路线恰好绕开该限制，优于 WebContentsView
内嵌方案。

**Skyvern**（23k★, AGPL）：实时流媒体到本地 + 可视化调试器；vision+LLM 路线与本篇
DOM 路线相反，仅参考其"实时流"产品表达。
