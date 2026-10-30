# Decision Note: 查看器内嵌视图——单 UDF 不变量 + CDP 端口自愈

Status: implemented — 2026-10-30 交付：UDF 收口到唯一出处、端口文件探活、建视图串行/幂等、建视图错误自解释

## Decision

现场症状有三个面孔，其实是同一条不变量的三种破法：

| 面孔 | 真实原因 |
|---|---|
| `web_open` 报 `0x8007139F`（ERROR_INVALID_STATE） | 同一进程出现第二套 WebView2 用户数据目录（UDF） |
| `webview_eval` 报 `10061 连接被拒绝` | 端口文件是**上一轮进程**留下的死端口，视图还没建 |
| 查看器「界面正忙」、菜单建不出来 | 同 label 的 `add_child` 被并发/抢先调用，UI 线程卡在环境创建上 |

不变量只有一条：**一个进程生命周期内只能有一套 WebView2 用户数据目录，且"能不能驱动"取决于端口文件是否指向活进程。** 此前它写在三处注释里，没有一处被强制。

落地的四条约束：

1. **UDF 单一出处**（`apps/viewer/src-tauri/src/webview_host.rs`）

   `prepare_user_data_folder()` 在 `main()` 里把 `WEBVIEW2_USER_DATA_FOLDER` 钉到
   `hermix_home/viewer-webview`（必须在第一个 webview 创建之前——WebView2 只在那时读这个
   变量），`user_data_folder()` 是唯一读取口，`webview_cdp` 不再自己解析一份。
   查看器**必须覆盖**父进程传进来的值：桌面主程序把它的 `webview-cache` 设进了环境变量，
   子进程原样继承 → 两者抢同一份用户数据（跨进程锁冲突）。

2. **桌面端把 UDF 提前到 Builder 之前**（`apps/desktop-tauri/src-tauri/src/lib.rs`）

   `portable_env::install_portable_env()`（内含 `WEBVIEW2_USER_DATA_FOLDER`）此前只在
   `setup` 里跑，而配置里的主窗口在 `Builder::build()` 阶段就建好了 WebView2 环境 →
   主窗口落在默认目录（`com.hermix.desktop\EBWebView`）、之后拉起的子进程用
   `webview-cache`：同一台机器两套用户数据，本进程再建第二个 webview 就是
   `0x8007139F`。与 2026-10-23 那批 Chromium 参数（三份来源 → `0x8007139F`）是同款坑。
   函数幂等，`setup` 里那次调用变成空操作，用户可见的报错弹窗仍留在 `setup`。

3. **端口文件先探活再采信**（`apps/viewer/src-tauri/src/webview_cdp.rs`）

   - `discard_stale_endpoint()`：启动时删掉上一轮的 `DevToolsActivePort`（UDF 根与
     `EBWebView/` 两处）。**只由持有单实例互斥的进程清**——第二个实例若也清，会把活实例
     正在用的端口文件删掉；`--mcp` 模式在上游已提前返回。
   - `port_is_live()`：读到端口后先 `TcpStream::connect_timeout`，连不上就删文件继续等
     重写，而不是把 `10061` 抛给上层。`WebView2` 只在环境就绪后才写这个文件，写出来却
     连不上就只能是旧值。

4. **建视图串行 + 幂等 + 等旧视图销毁**（`webview_host::open_sync`）

   - `OPEN_LOCK`：MCP 是 HTTP 端点、每条连接一个线程，两个客户端同时 `web_open` 会对同一
     label 并发 `add_child` → 环境创建与 UI 线程互相等死（"界面正忙"）。
   - 同一个 URL 已在看 → 直接返回，不销毁重建（保住登录态、滚动位置；自动化流程本来就会
     反复调 `web_open`）。
   - `close()` 是异步的，`window.webviews()` 要等 Destroyed 才更新 → 建之前轮询等它消失
     （上限 1s），否则同 label 抢先重建同样 `ERROR_INVALID_STATE`。

5. **错误自解释**：`create_error()` / `endpoint_error()` 把判断依据写进错误串
   （`code=` / `udf=` / `port_file=` / `port_reachable=` / `view_exists=` / `hint=`）。
   此前错误只有 `内嵌网页视图创建失败：{HRESULT}`，于是同一类故障被反复误诊成
   "UDF 名字不对 / 内核不对 / 拉起方式不对"。

## Alternatives considered

- **切内核（`viewer_action menu{kernel-embedded|kernel-system}`）**：否。`0x8007139F` 是
  环境分叉，与内嵌/系统内核无关；而且它要先建标签页，实测把查看器 UI 线程搞死
  （"界面正忙"），把故障面从"视图开不了"扩大到"整个查看器不可用"。
- **清 `webview-profile` / `viewer-webview` 排障**：否。那是**用户登录态**，不是缓存。
  清一次用户就要重新登录一次，等于排障过程自己制造新的不可用。
- **约束"查看器必须由桌面端拉起"**：否。机制与拉起方无关——落地的是 UDF 收口，任何拉起
  路径（桌面 / 手动 / `--hidden` / server 按需）都成立。按需拉起本来就是要在手动路径上
  也能工作的。
- **只改注释（把机制写清楚，靠 agent 读 skill 遵守）**：否。同一段知识已经写进
  `browser-automation` skill §3.9 了，实际表现仍是每轮重新试错——约束必须落在代码里。
- **给 `add_child` 加超时中断**：不做。Tauri 不暴露取消手段；串行化 + 幂等已经消掉了
  "同时建两个视图"这个唯一会互相等死的场景，加超时只能得到"超时了但线程还卡着"。

## Consequences

- 端口自愈把 skill §3.9 教的"先 `web_open` 一下"从**人的操作**变成**代码行为**：
  `webview_eval` 在视图未建时不再报 `10061`，而是等待/给出可执行 hint。
- `web_open` 变成幂等操作：同一 URL 重复调用不再刷新页面（此前会销毁重建，丢滚动位置与
  表单内容）。副作用是"用 `web_open` 强制刷新"这个隐性用法失效——刷新要走
  `viewer_action` 或 `webview_navigate`。
- 桌面进程只剩一套 UDF；历史上跑出来的 `com.hermix.desktop\EBWebView` 只是**磁盘残留**，
  不再被使用，可安全删除（不影响登录态，登录态在 `viewer-webview`）。
- 串行闸只在查看器进程内生效：两个**不同**查看器进程仍需靠单实例互斥（已有）保证唯一。
- 验证边界：`cargo check -p hermix-viewer --all-targets` 与 `-p hermix-desktop
  --all-targets` 均零错误零警告；新增三个 `webview_cdp` 单测（缺文件 / 死端口被丢弃 /
  活端口被采信）。**`cargo test -p hermix-viewer --lib` 在本机跑不起来**
  （测试二进制缺 comctl32 v6 清单 → `0xC0000139`，见
  `docs/notes/implemented/process/2026-10-21-tauri-rust-test-binary-comctl32-v6.md`），
  故这四条约束的**运行期**验证仍需按下面矩阵人工跑一次：
  桌面拉起 / 手动 `Start-Process` / `--hidden` / 崩溃后重启（profile 保留、端口文件陈旧）
  × `web_open` / `webview_eval`，全绿才算真修好。
