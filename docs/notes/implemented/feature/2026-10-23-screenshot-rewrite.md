# Decision Note: 截图模块重写（打不开 / 慢 / 黑底 / 选多大不显示多大）

Status: implemented — 2026-10-23 落地；抓屏换成 GDI 整块虚拟桌面、覆盖窗复用、幕布改事件推送、选区即窗口、窗口自动吸附绿框。

前置：[2026-10-07 系统截图功能](./2026-10-07-screenshot-tool.md)（原始设计，本次是它的结构修订）。

## 背景：用户报的四件事

用户报「项目的截图打不开」「打开慢」「截图后有黑色背景」「用户选择多大就显示截取的那部分即可」，
并要求「鼠标移动到某个位置或窗口，自动显示出绿色截图边框选择对应的窗口」。

四件事在旧实现里是**同一个根因簇**的不同表现：旧实现的抓屏、窗口、坐标系、交互
是四套各自为政的东西，任何一处对不齐，用户看到的就是"黑边 + 慢 + 偶尔打不开"。

## Decision

### ① 抓屏：从 xcap 单显示器 → GDI 一次抓整块虚拟桌面

`screenshot.rs::capture_desktop()` 用 `CreateDIBSection` + `BitBlt(SRCCOPY|CAPTUREBLT)`
一次抓下**虚拟桌面并集矩形**（`GetSystemMetrics(SM_*VIRTUALSCREEN)`），位图像素与
虚拟桌面物理像素 **1:1**。理由三条：

1. **xcap 只覆盖光标所在那一台显示器**，多显示器时其余区域天然是黑的；
2. **xcap 不带 `CAPTUREBLT`**，分层窗口（半透明 UI）抓回来是黑块 —— 这正是
   "截图后有黑色背景"最常见的来源；
3. 抓屏从 ~200ms（xcap 逐台 + RGBA↔BGRA 全帧转换）降到 ~10ms，一次拷贝。

`xcap` 依赖随之移除（`apps/desktop-tauri/src-tauri/Cargo.toml`）。

### ② 缩放换算：Rust 不再猜，改由网页自报

旧实现用 xcap 的 `scale_factor()` 做「物理 ↔ 逻辑」换算。它与 Windows 实际
per-monitor DPI **未必一致**，猜错就是「照片和屏幕对不齐」的黑边。现在前端用
`window.devicePixelRatio`（WebView2 实际使用的那个值）做换算，Rust 只负责
物理坐标系的裁剪。**单一真相源从 Rust 挪到了浏览器。**

### ③ 覆盖窗：只创建一次，反复复用

旧实现每次截图都新建一个 WebView2 窗口（含页面加载），第二次起每次都白扔一次
建窗 + 加载开销。现在覆盖窗**首次创建后一直复用**：每轮截图只是「定位 → 显示 →
推帧」。`end_session` 隐藏窗口而不是销毁。

### ④ 幕布帧：150ms 轮询 → 事件推送 + 页面回报后再亮窗

> 2026-10-24 起，幕布帧的 dataURL 推送机制被
> [原生分层窗直贴](./2026-10-24-screenshot-native-backdrop.md)取代（像素不再进
> webview）；本节的「页面回报后再亮窗」时序仍然现行。

旧实现前端每 150ms 轮询一次 `screenshot_frame_data`，轮询粒度本身就是感知的
等待下限。现在 Rust 编码完 `emit("hermix:screenshot-frame")`，页面解码完**再
回报** `screenshot_overlay_ready`，Rust 此刻才 `show()` —— 窗口要么显示已就绪的
画面，要么根本不出现，没有黑屏、没有半就绪画面。页面挂载时仍会拉一次
`screenshot_frame_data`，用来兜住「帧先到、页面后到」的时序。

### ⑤ 选区即窗口：选多大就显示多大

旧设计是「全屏覆盖窗 → 选区 → 进编辑器」，编辑器在**全屏黑底**（`rgba(0,0,0,.85)`）
中间显示一小块选区。现在选区确认后 Rust 把覆盖窗**缩到选区大小**
（`PhysicalPosition`/`PhysicalSize`，物理像素），编辑器只显示选中的那部分，
底下不再垫黑底。缩放换算交给 `devicePixelRatio`，物理 1:1。

窗口尺寸此后有了下限（560×280，CSS 逻辑像素）：小选区把窗口缩成几十像素时
工具条无处可站，见
[2026-10-28-screenshot-editor-min-window.md](../bug-fix/2026-10-28-screenshot-editor-min-window.md)。

### ⑥ 窗口自动吸附：默认常驻 + 绿色边框

旧实现的窗口识别藏在「窗口模式」里，要按 Tab 切过去。现在**鼠标移动即查询光标下的
窗口**（`EnumWindows` 取 Z 序第一个可见未最小化未 cloak 的 HWND），画出**绿色边框
+ 窗口标题**；单击该窗口即选中，拖拽仍然照常框选自由区域。两种交互同时可用，
不再需要模式切换。

### ⑦ WebView2 浏览器参数：单一来源（这条修的是"打不开"）

旧代码里 WebView2 的浏览器参数有**三份**：进程环境变量
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`（lib.rs）、`tauri.conf.json` 的
`additionalBrowserArgs`、截图覆盖窗自己传的 `additional_browser_args`。
**同一 user data folder 下所有 webview 必须共享同一套浏览器参数**，不一致时
WebView2 建窗直接失败 `0x8007139F`（ERROR_INVALID_STATE）—— 这就是"截图打不开"
的根因（2026-10-10 的 `224b0fd8` 只把覆盖窗对齐到 conf，没发现环境变量是第三份）。

现在**只留环境变量一份**：`tauri.conf.json` 的 `additionalBrowserArgs` 与覆盖窗的
`additional_browser_args` 全部删除，所有 webview（含浏览器小窗、文件搜索小窗）
统一继承环境变量。顺带修了一个潜在 bug：`--disable-features` 在旧环境变量里
写了三次，而 Chromium 只取**最后一次**的值 —— 前两项（`CalculateNativeWinOcclusion`、
`IntensiveWakeUpThrottling`）从来没生效过；现在合成一个。

### ⑧ 收尾：焦点还给原窗口

截图前记下前台窗口（`GetForegroundWindow`），会话结束时
`SetForegroundWindow` 还回去，用户不用自己切窗口。

## Alternatives considered

- **保留 xcap，只补 CAPTUREBLT**：xcap 不暴露 raster op，且仍然只抓单显示器，
  多屏黑边问题解决不了。否。
- **用 DXGI Desktop Duplication / WGC 抓屏**：能抓 HDR 与独占全屏，但 API 面大、
  Win10 1903+ 才有，且交互式选区场景用不上（瓶颈在交互不在抓屏）。保留为将来的
  升级路径，不在本次范围。
- **原生 Win32 覆盖窗（WS_EX_LAYERED + 位图）替代 WebView2**：能彻底绕开
  0x8007139F 与页面加载时间，但编辑器（画笔/文本/马赛克/裁剪）要重写成原生绘制，
  成本与风险远大于收益。否。
- **每次截图新建窗口 + 只做"参数对齐"**：保留重建语义，但每次仍要付建窗与页面
  加载的钱（用户明确抱怨"慢"）。否。
- **多显示器各建一个覆盖窗**（10-07 原设计的 M3 范围）：每屏一窗要处理跨屏选区
  合成与混合 DPI，复杂度陡增；单窗覆盖整块虚拟桌面 + 一次抓屏已经消除了黑边。
  否（架构上仍可扩展）。

## Consequences

- 新增依赖：0；移除依赖：`xcap`。
- 覆盖窗常驻（隐藏），占一个 WebView2 实例的内存；换来第二次起几乎零启动开销。
- 跨不同缩放的多显示器：抓屏是物理像素（正确），但网页只有**一个**
  `devicePixelRatio`（取窗口所在主显示器的），另一块屏的显示会略有缩放误差。
  已知限制，未做 per-monitor DPI 适配。
- 编辑态窗口 = 选区大小：极小选区（如 100×60）时底部工具条会挤，工具条因此
  改成可横向滚动。
- 用户按下 ESC / 右键仍可随时退出；Rust 侧 `GetAsyncKeyState` 兜底监视保留。
