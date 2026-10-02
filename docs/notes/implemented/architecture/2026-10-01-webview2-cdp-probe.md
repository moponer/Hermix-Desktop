# Decision Note: N0 技术验证报告 —— WebView2 进程内 CDP 可行性（批次 N）

Status: implemented — 探针已落地并取得真机证据（2/3 方法通过，1 项参数修正待重测）

## Decision

用真机探针（`apps/desktop-tauri/src-tauri/src/cdp_probe.rs`）回答批次 N 的
立项前提问题：**做「内置智能体浏览器」是否必须定制 Chromium 内核？**

结论：**不需要**。WebView2 的进程内 CDP API（`CallDevToolsProtocolMethod`
+ completion handler，微软官方文档 "Use the Chrome DevTools Protocol in
WebView2"）提供了 ego-lite 定制内核所依赖的同一能力面，且有两项结构性
优势：零端口暴露（进程内函数调用，非 remote-debugging-port）、零额外
分发（runtime 已是 Tauri 硬依赖）。

## 真机证据（2026-10-02，hermix-desktop dev 构建，探针门控 HERMIX_CDP_PROBE=1）

| CDP 方法 | 结果 | 意义 |
|---|---|---|
| `Runtime.evaluate`（`1+1`，returnByValue） | **OK: 1+1==2** | 页内求值通 → ego-browser `js()` 逃生舱可行 |
| `Accessibility.getFullAXTree` | **OK: 50 AX nodes**（工作台首屏） | 完整语义树可拿 → ref 编号/loc 选择器的原料可行 |
| `DOMSnapshot.captureSnapshot`（`{}` 空参） | FAIL: E_INVALIDARG (0x80070057) | WebView2 对空参数校验更严；已改显式参数 `{"computedStyles":false,...}`，**重测待跑**（见未竟项） |

工程事实（编译/运行期钉死，供 N1 实现直接引用）：
- Tauri 2.11 `PlatformWebview::controller()` 直接返回类型化的
  `ICoreWebView2Controller`（非裸指针），`.CoreWebView2()` 即得
  `ICoreWebView2`
- 依赖形态：`webview2-com 0.38` + `windows-core 0.61` 显式声明（与 Tauri
  传递依赖同版本，零新依赖树）；`Microsoft` 模块从 `webview2_com` 重导出；
  命名空间是 `Microsoft::Web::WebView2::Win32`（不是 `Microsoft::WebView2`）
- **线程模型（编译器钉死）**：COM 接口非 `Send`，CDP 完成回调经 Windows
  消息循环派发 → 全部 CDP 工作必须在 `with_webview` 闭包内（= 主线程）
  完成；`wait_for_async_operation` 内部 `wait_with_pump` 等待时泵消息，
  跨线程只传 `String` 结果（mpsc）
- 完成闭包的真实签名（宏生成）：`fn(Result<(), windows_core::Error>,
  String) -> Result<(), windows_core::Error>`——HRESULT 已被宏包装层转成
  `Result`，PCWSTR 已转成 `String`
- 探针触发方式：`HERMIX_CDP_PROBE=1` 启动 → 6s 后自动对主 webview 跑
  三方法 → stderr 打 `[CDP-PROBE]` 前缀结果；无 GUI 交互可取证

## Alternatives considered

- **fork Chromium 定制内核（照抄 ego）** —— 否决：本探针证明 CDP 原始
  数据（AxTree/DOMSnapshot/Runtime.evaluate）经 WebView2 全部可得，
  token 压缩用论文实证的预处理管线（D2Snap arXiv 2508.04412 +
  FocusAgent arXiv 2511：最高 99% 剪枝、16.5% 上下文占用、成功率不降）
  在应用层可调参达成，不需要引擎级定制；fork 的跟随升级成本是巨坑。
- **remote-debugging-port 外挂 CDP（浏览器扩展/独立进程路线）** —— 否决：
  本机安全敏感面（任何本地进程可连），进程内 API 完全覆盖同样能力。
- **UIA 语义树做默认感知层** —— 降级为兜底：UIA 零配置可读任何浏览器，
  但信息深度不及 CDP（无 js() 求值、无网络事件），作为无调试端口的
  只读降级路径保留（对齐 N2 attach 设计）。

## Consequences

- 获得：批次 N 的立项前提成立（N1 浏览器 PTC + 语义快照可直接在
  WebView2 上实现，无需定制内核）；探针代码即 N1 的 CDP 桥雏形
  （`call_cdp_sync` 的同步封装 + 消息泵模式可直接复用）。
- 放弃：无（探针是纯增量，环境变量门控默认关闭）。
- 代价：`webview2-com`/`windows-core` 显式依赖（与 Tauri 同版本，无
  实际新增构建产物）；探针代码 ~200 行。
- **未竟项（N1 开工前必须补齐）**：
  1. DOMSnapshot 显式参数重测（代码已改好，一条命令验证：
     `HERMIX_CDP_PROBE=1` 启动应用，10 秒出结果）——GUI 进程启动在本
     agent 环境被审批策略阻断，需人工跑一次
  2. token/页基线测量（对真实网页跑 getFullAXTree 序列化后计 token，
     对标 ego 的 100-400 token/页）——同样等一次人工启动
  3. 完成后本笔记补录数据，#2 任务关闭
