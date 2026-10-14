# Decision Note: 浏览器自动化 P0——浏览器永不现形 + 应用内小窗

Status: implemented — 2026-10-14（P0 范围）。P1（Token 纪律）/ P2（交互补全）/
P3（清理）仍在 [proposed 同名笔记](../../proposed/feature/2026-10-13-browser-virtual-window-token-discipline.md)，
其拆分原因与评审发现见文末"评审修正"。

承接已实现的 [native-os-automation](./2026-10-13-native-os-automation.md)，
与 [Space 与登录态](../../proposed/feature/2026-10-07-browser-space-and-login-state.md)（N2）维度不同、互补：
那篇管"进哪个浏览器"，本篇管"模型怎么看、人怎么看、每个动作花多少 token"。

## Decision（P0 范围内已全部落地）

> **浏览器永不现形、画面只进应用内小窗。**

1. **独立 Tauri 子窗口**：`headless` 参数从 schema 与 launch 路径**整体删除**
   （删字段而非留默认——留着就是诱惑），Edge 恒定 `--headless=new`
   虚拟渲染——本机不存在任何浏览器窗口，不进任务栏、不进 Alt+Tab、不抢焦点。
   画面经 screencast 帧流送进独立子窗（OS 级小窗：可拖动、可拖出主窗口、
   可上第二显示器、主窗口最小化时仍在）。所有输入走 CDP 合成事件，不进系统输入队列。
2. **实现锚点**（commit `0078da63`）：
   - `launch.rs` / `mod.rs`：无 headed 变体（module doc 写明这是刻意决定）；
   - `screencast.rs`：重写为可重置单例，session 建立即常开帧流（JPEG q60、
     broadcast 内存流转、逐帧 ack，不落盘）；
   - `browser_view_controller.rs`：Tauri 子窗 `browser-view`，常驻帧泵
     `emit_to` 推流；窗口与 session 生命周期解耦（关小窗 ≠ 关浏览器，
     Edge 只在 `browser_close` 或 Job Object 兜底时终结）；几何记忆（关闭时
     存 position + inner_size，重开还原）；
   - 前端 `BrowserView.tsx`：canvas 渲染（`createImageBitmap` + `drawImage`
     循环）+ 暂停 / 保存当前帧 / 关闭；主窗标题栏 🌐 入口；
   - `lib.rs`：接线 `browser_view_open` / `browser_view_set_paused` commands。

## 评审修正（2026-10-14，对照原 proposed 的三处偏离）

1. **"不看不推"未按原文实现，且缺实测记录**——原方案承诺小窗最小化/遮挡时
   `Page.stopScreencast` 彻底停编码（CPU 三道闸门之一），实现走的是"暂停只停
   Rust→前端推流，Chrome 侧 screencast 照常"（即原方案列的退化路径），但**没有
   "实测发现停流抖动"的依据**——拿"待实测"当跳过实测的挡箭牌，属流程违规。
   **已于同日修复（评审修复提交）**：`screencast::pause()/resume()` 落地——
   pause 发 `Page.stopScreencast` 彻底停 Chrome 编码，resume 对同一页面重发
   start 参数（通道/监听/帧泵原样保留，recv 挂起等待，CDP restart 毫秒级）；
   `browser_view_set_paused` 改 async 真调这对函数。ego 的 WGC/FFmpeg 恢复
   抖动教训不适用于 CDP 路线；真机若见恢复抖动，退化路径 = 只降
   every_nth_frame 不停流。**待真机实测闭环**：最小化状态开动画页，任务管理器
   看 Edge CPU 应归零、恢复小窗画面应在 1-2 帧内续上。
2. **逃生门自相矛盾已裁决**——原方案 Decision 说"废弃 headless=false"，
   P0 第 6 点又承诺设置页"一次性真窗口"，互相矛盾。实现取前者
   （`launch.rs` "deliberately no headed path"）。**裁决：维持废弃**——真窗口
   抢焦点违背产品承诺，极端调试场景（真实窗口指纹）接受无出口；如未来确有
   需求，走独立提案而不是给模型留 schema 后门。
3. **"现状"列高估**——原笔记写"24 000 字符硬顶已实现"，实际 `MAX_CHARS=48_000`
   且模型传参可达（后门已在本次修复中收紧，见 proposed 笔记 P1）。

## Alternatives considered（P0 相关部分）

- **引入 Playwright / browser-use 托管浏览器** —— 否决：Node/Python 运行时违背
  os_auto"零外部运行时"内核。
- **保留 headless 参数只改默认值** —— 否决：留着字段模型就会用；schema 是模型的
  行为边界，不是配置面。已按"删字段"落地。
- **WebContentsView 内嵌主窗** —— 否决：Tauri 的 WebView 无 CDP 能力
  （agent-view 证实 Windows 须 WebView2），"外部 Edge + screencast"恰好绕开。
- **Xvfb / 虚拟显示** —— 否决：Windows 不需要；headless=new 即虚拟窗口。
- **设置页"一次性真窗口"逃生门** —— 否决（见评审修正 2）：与总纲矛盾，
  留着即违背"永不现形"承诺。

## Consequences

- 获得：浏览器全程不现形（无窗口、不抢键鼠、无孤儿——Job Object
  `KILL_ON_JOB_CLOSE` 兜底），操作画面实时可见于应用内小窗，可拖出主窗、上第二屏。
- 代价：极端调试场景（需要真实窗口指纹的站点）无出口（评审修正 2 裁决）。
- 风险（待实测闭环）：暂停态 Chrome 侧仍在编码。真机测最小化时 Edge CPU；
  超 5% 则实现 stopScreencast 或降质降分辨率（1280×800→640×400，q60→q40）。
- 帧流全程不落盘；唯一写盘是用户点「保存当前帧」的单张 PNG。
- 与 N2（Space/登录态）无前置依赖，可并行。
- 参考：ego 系（CitroLabs/ego-lite、fisfzy/dsh-ego-browser）观察窗范式与技术选型
  同构（CDP JPEG 20FPS、SSE 推流、监控窗直操）；其 WGC/FFmpeg 停流抖动教训
  对我们是否适用**未实测**（我们走 CDP restart，理论毫秒级）。
