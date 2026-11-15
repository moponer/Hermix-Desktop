# Decision Note: 查看器的「钉住预览」浮窗 —— 可拖动、可置顶、位置记忆

Status: implemented — 2026-10-21 落地。上一轮因「浮窗白屏」回退，本轮定位到
真正的原因并改掉建窗路线，真机验证通过（见「两次翻车的真实原因」）。

## Decision

查看器支持**多个并存的独立浮窗**，把某个文件或目录的预览"钉"在屏幕某个
位置，方便来回对比、不用在一个窗口里反复切换。

落地的形态：

- **入口**：预览区工具条的「钉住」按钮（钉当前预览的文件）+ 文件列表右键
  菜单的「钉住（独立浮窗）」（可钉目录）。
- **浮窗是只读预览**：文件走共享预览层（`loadPreview` + `PreviewSurface`），
  目录走轻量条目列表。**浮窗不给编辑能力**——主窗口已经在编辑同一个文件，
  浮窗再来一份可编辑的等于两个实例写同一个文件。
- **可置顶**、**可挪到屏幕任意位置**、**位置跨次启动记忆**
  （`$hermix_home/pinned-windows.json`）。
- **浮窗有独立轻量入口**：`main.tsx` 看 URL 上的 `?pin=` 参数决定渲染
  `PinWindow` 还是完整 `App` —— 浮窗不加载资源管理器 / AI 栏 / 工具条。
- **同一路径重复钉 = 复用既有浮窗**（聚焦它），不刷出一堆重复窗口。
- **同时最多 6 个浮窗**，超限时关掉最旧的（每个浮窗 = 一个独立
  WebView2 实例，内存线性增长，必须有上限）。

## 两次翻车的真实原因（这一节是本笔记最有价值的部分）

第一轮实现（`be232c28` + `83e948f1`，已回退）失败在两个地方：

1. **`WebviewUrl::App("index.html?pin=…")`** → Tauri 把 `?pin=…` 当成
   **文件名的一部分**去找前端资源 → **建窗直接失败**。改成了
   `WebviewUrl::App("index.html")` + 路径走 Rust 侧注册表。
2. **窗口开得出来，但内容空白** → 当时据此判定"运行期创建的窗口加载不了
   应用资源"，把整个功能回退了。

**第 2 条是误判。真根因有两条，都跟"建窗"有关**（本轮用 **CDP 取证**定位，
不是靠看截图猜）：

取证手段：给查看器加 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=
--remote-debugging-port=9223` 启动，再用 CDP 直接问每个 webview target 的
`url` 与 `document.body.innerHTML.length`——比截图可靠得多。

- **根因 A：在主线程上创建 webview**。wry 的 Windows 注意事项（wry#583）写
  得清楚：在**同步命令 / 事件回调**里创建 webview 会死锁，必须放到单独线程。
  Tauri 的**同步命令跑在主线程上** → 浮窗的 HWND 建出来了，但导航事件排在
  主线程队列里没人处理 → **永远停在 `about:blank`**（实测：该 target 的
  `url = about:blank`、DOM 长度 0）。
  **修法**：建窗命令写成 `async fn`（Tauri 的异步命令跑在 tokio 工作线程上）。
  **旁证**：同一份代码里的**子 webview** 从第一版就是 `async` 命令 → 它一直
  正常。同一个坑，同一处差异。
- **根因 B：顶层窗口用 `WebviewUrl::External(应用地址)` 拿不到协议的
  处理器**。浮窗的 webview 确实导航到了 `http://tauri.localhost/`，但页面是
  **"ERR_CONNECTION_REFUSED"**（真空白，不是截图假象）；而**同样的地址在子
  webview 里能正常渲染**（子 webview 走的是另一条注入路径）。
  **修法**：顶层浮窗改用 `WebviewUrl::App("index.html")` —— 主窗口就是它
  这么起来的。

**顺带一条取证纪律**：当时的"白屏"证据来自 `PrintWindow` 截图。
`PrintWindow` 只抓窗口自己的 DC，**WebView2 的合成层不在里面**——网页 /
预览内容在 PrintWindow 截图里**本来就是一片空白**，跟它有没有渲染无关。
验证"界面有没有渲染出来"必须用**屏幕实拍**（`CopyFromScreen`）或 devtools /
CDP。取证方法错了，结论就会一路错到"回退一个本来能用的功能"。

**顺带一条**：Tauri 的资源服务**不认带 query 的地址**（`?pin=…` 找不到前端
资源）。凡是"用 URL 传参数给窗口"的设计都要先确认这条；窗口身份用
**label** 传（运行期就有，不进 URL）。

## 建窗路线：`WebviewUrl::App("index.html")`（与主窗口同一条路）

浮窗的 webview 用 `WebviewUrl::App("index.html")`：

- **不**用 `WebviewUrl::External("http://tauri.localhost/")`：实测（CDP 取证）
  顶层窗口里的那个 webview **拿不到应用的协议处理器** → 页面是
  "ERR_CONNECTION_REFUSED"（真空白，不是截图假象）。同样���地址在**子
  webview**里却能正常渲染（子 webview 走的是另一条注入路径）。
- **不**在 URL 里带 query：Tauri 的资源服务不认带 query 的地址。
- 浮窗的身份（被钉住的是谁）**不进 URL**，前端按自己的窗口 label 去 Rust
  侧查（`pin_path`）。

## 三条被调研与实测钉死的约束

1. **每个浮窗是独立的 JS realm**。Tauri 的每个 `WebviewWindow` 都是独立
   WebView2 环境，模块级变量不共享（Obsidian 官方开发者文档明确：pop-out
   有自己的 `Window`/`Document`/全套全局构造器）。所以浮窗**不共享前端
   状态**——要共享的一律走 Rust 侧 + `emit_to(label)` 定向投递。
   （推论：跨窗口 `BroadcastChannel` 不可靠，别指望它。）
2. **位置必须记"显示器是谁"**。便携应用拔着 U 盘换机器是常态：显示器拔了
   以后按旧坐标恢复 = 窗口跑到屏幕外。落位记忆里存 `Monitor::name()` +
   `scaleFactor`，恢复时原屏不在（或 DPI 变了）就落到主屏的
   **`work_area`**（工作区，不含任务栏 —— 用 `size` 会被任务栏压住）。
3. **坐标语义不能混**。Rust 侧 `PhysicalPosition`/`PhysicalSize` 是物理
   像素，`Monitor::position()` 也是物理像素；但**预设位置**用
   `LogicalPosition` 交给 Tauri，它按目标显示器的缩放换算（直接给物理像素
   在 150% 缩放的屏上会偏）。

## Alternatives considered

- **纯应用内「并排对比视图」（不建原生窗口）** —— 借鉴 XnView MP 的比较
  模式（一个比较窗 + N 面板 + 同步缩放/滚动）。**半采纳**：它做"对比"更
  便宜，但**满足不了用户原话**（"钉在屏幕某个位置，方便多个窗口不用来回
  切"）——应用内并排仍然被主窗口框住，不能浮在别的应用之上、不能跨屏。
  留作后续（对比模式）。
- **`tauri-plugin-window-state`** —— 拒绝。它的键是 **window label**，而
  这里的语义是**文件路径 → 窗口**（每次启动可能是不同文件）；而且它的兜底
  是"显示器没了就让 OS 决定"，不如"主动 clamp 回主屏工作区"符合预期。
  自写约 50 行 JSON 持久化更省，且**零新依赖**。
- **`tauri-plugin-positioner`** —— 只提供"移到 well-known 位置"，没有
  pin/置顶逻辑，仍需自写；且它的语义是固定九宫格，我们还要"记住上次位置"。
- **Electron 多窗口** —— 出局：需打包约 150 MB Chromium，与「便携零安装」
  正面冲突。Tauri 多窗口**不引入任何新运行时**（WebView2 Evergreen 是
  系统级共享，viewer 已在用）——新增成本只有内存，没有磁盘。这是本路线
  敢被认真评估的前提。
- **引入 tiling / 分区系统（i3 / sway / Hyprland / FancyZones）** —— 不做。
  分区是**操作系统 / 窗口管理器**的活（PowerToys FancyZones 已经做了），
  应用层自己造 tiling 是重造轮子且用户不会用。

### 交互形态的来源（开源项目）

- **Microsoft PowerToys**（MIT，139K★）的 **Peek**：按住空格出预览窗，
  **pinning 按钮保留当前窗口的尺寸与位置**——这几乎就是用户原话
  "钉在某个位置"的语义；**Always On Top**：把窗口 pin 到所有窗口之上。
- **VS Code**（MIT，194K★）：把标签**拖出窗口**落进浮动窗 + 标题栏的
  **pin 图标 = Set Always on Top**。这是肌肉记忆最顺的交互，本项目照抄。
- **Obsidian**（闭源 App，插件生态 MIT）：pop-out 窗口的**归属语义**
  （关主窗 → pop-out 全关）与**多窗口 JS realm 隔离**的官方说明。

## Consequences

- 查看器**第一次能开出第二个窗口**：单实例模型（Mutex + 固定端口 TCP 投递
  路径）仍保留，但"已有实例收到路径"这条路径只作用于主窗口——浮窗是进程
  内在主窗口里创建的。
- **浮窗只读**：想在浮窗里编辑必须回主窗口。这是刻意的（见 Decision 第 2 条
  的双写冲突）。
- **位置记忆只保证"不丢"**：显示器拓扑变了会重排，而不是精确还原到原来的
  那块屏（拔屏在便携场景下是常态，精确还原反而更糟）。
- **内存随浮窗数线性增长**：每个浮窗一个独立 WebView2 实例，故设 6 个上限。
- 浮窗的**轻量入口**只渲染预览，不加载资源管理器 / AI 栏 / 系统面板 ——
  这既是性能考虑，也避免了"两个窗口都能改同一个文件"的歧义。
- 未做：**并排对比视图**（同步缩放 + 同步滚动的比较模式）；**浮窗内的
  编辑**；**浮窗分组/工作区**（PowerToys Workspaces 那套）；浮窗的
  **拖动条**（用系统原生标题栏，拖动由系统负责）。
