# Decision Note: 截图幕布下沉原生分层窗（机制追平微信）

Status: implemented — 2026-10-24 落地。冻结帧从「JPEG→base64→事件→webview 解码」整体下沉为 Win32 `UpdateLayeredWindow` 原生直贴（`backdrop.rs`）；webview 变透明，只画选区几何。

前置：[2026-10-23 截图重写](./2026-10-23-screenshot-rewrite.md)（其「④ 幕布帧事件推送」的 dataURL 机制被本篇取代，其余条目仍然现行）。

## 背景

2026-10-24 上午的口径：幕布帧走 webview 显示，4K 下全链路 ~660ms、降采样优化后 ~200ms，其中 JPEG 编码占 90%+。同场景微信（原生 C++ 分层窗）~15ms。差距不在某一跳慢，而在**机制**：像素进 WebView 只能是字符串（编码→base64→JSON 跨进程→浏览器解码），四跳额外成本原生路径一个都没有。用户拍板：不做「感知追平」（~50ms 二进制通道方案），要**机制追平**。

## Decision

### ① 幕布 = 原生分层窗（backdrop.rs）

`BitBlt` 抓出的全分辨率 BGRA 由 `backdrop::paint_and_show` 在**主线程**一次
`UpdateLayeredWindow`（`ULW_ALPHA`，`AC_SRC_OVER`/`AC_SRC_ALPHA`）直贴到
`WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` 的弹出窗上，
`pptDst` 用虚拟桌面原点顺带完成定位。不编码、不序列化、不过进程边界；
幕布分辨率 = 屏幕分辨率（比降采样时代的幕布更清晰）。

预算（release，2026-10-24）：抓屏 ~10ms + alpha 补写+ULW ~10-20ms ≈ **4K 全链路
~20-35ms**、1080p ~15-25ms，与微信同一机制、同一量级。

alpha 是承重细节：GDI BitBlt 不保证 alpha 字节写 0xFF，而 ULW 按**预乘 alpha**
解释（alpha=0 = 整窗隐形）→ 拷入 ULW 位图时统一补 255（`copy_bgra_opaque`）。

### ② webview 变透明，只画几何

覆盖窗建窗加 `.transparent(true)`（WebView2 `DefaultBackgroundColor` alpha=0），
`index.html` 的 `html[data-overlay]` 底色改 `transparent`。webview 只画吸附绿框、
选区、压暗四片、工具条——Rust 只发一条几十字节的会话信息事件
`hermix:screenshot-session`（原点/尺寸/seq），**零像素载荷**。页面画完选区 UI
（双 rAF 确认上屏）回报 `screenshot_overlay_ready` 才亮 webview；幕布的上屏在
幕布闭包里独立完成，不等页面。

### ③ 交互时序

热键 → 抓屏(10ms) → 幕布 ULW 上屏（用户看到冻结画面）→ 会话信息事件 →
页面渲染选区 UI → 亮 webview（十字光标出现）。选区确认 → `screenshot_select`
裁剪 + 窗口缩到选区 → 编辑器渲染选区像素 → 页面调 `screenshot_backdrop_hide`
收幕布、回到活屏幕（编辑器画布与幕布是同一块像素，切换无缝无黑帧）。

### ④ 删除清单

`encode_jpeg`、`bgra_to_rgba`、`downscale_bgra_box`、`BACKDROP_MAX_SIDE`、
后台编码线程与「take/归还 session」时序整体删除；`image` 依赖退到 png-only；
`window_rect_at` 的排除参数从单 HWND 改为集合（覆盖窗 + 幕布窗都要排除）。

## Alternatives considered

- **二进制通道传原始字节 + putImageData（~50-60ms）**：感知已瞬时，但仍跨进程
  两次拷贝，且幕布要降采样（瞄准精度受损）。用户明确要机制追平，否。
- **Channel 传 JPEG 字节（~150ms）**：编码/解码两跳仍在，只是打包变便宜，
  过渡态无独立价值。否。
- **透明不冻结（零像素传输）**：选区期间底层画面会动，最终裁剪与用户所见
  可能不一致，破坏「所见即所得」契约。否。
- **原生窗整体替代 webview（编辑器原生重写）**：2026-10-23 笔记已否决；本次
  只下沉幕布层，交互/编辑器留在 webview。维持否决。
- **跨线程直接调 ULW**：幕布窗消息靠主线程 tao 事件泵派发，所有触碰必须经
  `run_on_main_thread`，否则跨线程 `SendMessage` 语义不可控。否。

## Consequences

- 新增 unsafe Win32 面（`backdrop.rs` ~250 行）：建窗/类注册/ULW/置顶。
  paint 在主线程停 5-15ms（4K）——一次性成本，与微信在 UI 线程画冻结帧同款。
- **webview 透明是承重假设**：`.transparent(true)` + `incognito(true)` 的组合
  未经真机长验。若真机出现白/黑整窗（DefaultBackgroundColor 未生效），
  排查方向：WebView2 Runtime 版本对 alpha=0 的支持、与 incognito 的交互。
- 幕布窗与覆盖窗都在 TOPMOST 带，paint 闭包末尾必须把 webview
  `set_always_on_top(true)` 抬回幕布之上，否则选区 UI 被冻结画面盖住。
- 多屏混合 DPI：ULW 位图由 DWM 按屏缩放，与 webview `<img>` 时代的误差同源，
  无回归（已知限制维持）。
- 裁剪路径 `crop_rgba` 仍透传抓屏原始 alpha；PNG 底图若在真机出现全透明，
  用 `copy_bgra_opaque` 同款补法（未观测到，暂不动）。
- image crate 不再编 JPEG：jpeg feature 移除，二进制略缩。
- 验证矩阵（真机手动）：1080p / 4K@100%+150% / 双屏混 DPI 的出画时延
  （`tracing` 的 `capture=Xms backdrop=Yms`）、TOPMOST 相对其他置顶窗、
  独占全屏视频下的黑块（GDI 固有，与改前一致）、ESC 兜底退出。
