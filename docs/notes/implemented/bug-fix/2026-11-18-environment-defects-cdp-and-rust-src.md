# Decision Note: 两个环境缺陷的根因与修复（外部 CDP / rust-src）

Status: implemented — CDP 加 `--no-sandbox` 后整链打通；rust-analyzer 对齐到 rustup 1.95 工具链，标准库跳转可用

## Decision

两个问题此前都被当成「环境问题、修不了」搁置。实际都能修，根因如下。

### 一、外部 CDP「TCP 通、HTTP 超时」

**症状**：端口在监听（`Get-NetTCPConnection` 显示 msedge 监听 `127.0.0.1:<port>`），
TCP 连接成功，但 HTTP `/json/version` 和 WebSocket 握手都返回**零字节**后超时。

**排查过的错误方向**（都排除了，记下来省下次的时间）：

- 系统代理：`127.0.0.1:7897` 确实开着，但 `--proxy-server=direct://` 和 `--no-proxy-server` 都无效。
- Edge 版本过旧：Edge 是 156，比 Chrome 147 新。
- `--remote-allow-origins`：无效。
- 端口被别的进程占用：确认是 msedge 自己监听。
- headless 模式：`--headless=new` 和旧 `--headless` 都超时。

**根因**：**缺 `--no-sandbox`**。加上后同一套参数立刻返回 `HTTP/1.1 200 OK`，
`/json/version` 报 `Edg/156.0.4314.58`，WebSocket 握手 `101`，`Browser.getVersion` 正常应答。

推测是渲染进程沙箱与本机的安全软件（360 安全卫士、腾讯电脑管家都注册在 SecurityCenter2）
存在冲突，导致 CDP 监听线程接受连接但无法处理。自动化浏览器只加载一次性 profile，
失去渲染沙箱是可接受的代价——而且这是「能用」与「完全不能用」的差别。

**修复**：`browser.rs` 的启动参数加 `--no-sandbox`。

**验证**：`browser_open` → `browser_text`（读到完整正文）→ `browser_navigate` → `browser_close` 全通。

### 二、rust-src 缺失

**症状**：`rustup component list --installed` 列出 `rust-src`，
`rustup component add rust-src` 回「已是最新」，但 `$(rustc --print sysroot)/lib/rustlib/src` 不存在。

**根因**：**本机有两套 Rust**。

| | 路径 | rustc | rust-src |
|---|---|---|---|
| PATH 上的（`rustc --print sysroot` 指向它） | `C:\Program Files\Rust stable MSVC 1.92` | 1.92.0 | **缺失** |
| rustup 管理的 | `~\.rustup\toolchains\stable-x86_64-pc-windows-msvc` | 1.95.0 | **完整** |

`rustup component add rust-src --toolchain 1.92.0-...` 报 `Missing manifest in toolchain`——
因为 1.92 那套是独立安装，不归 rustup 管。所以 rustup 的「已安装」说的是 1.95，
而 rust-analyzer 去 1.92 的 sysroot 找，当然找不到。

**试过但无效的方案**：

- 往 `Program Files` 拷/链接 rust-src：需要管理员权限（`Access is denied`）。
- `RUST_SRC_PATH` 环境变量：新版 rust-analyzer 不再用它定位 sysroot。
- 让 1.92 的 rust-analyzer 用 1.95 的 src（`cargo.sysroot` 指过去）：
  `can't load standard library` 警告消失，但**所有跳转返回空**——版本不匹配的分析结果是错的，
  这比报错更糟，因为它看起来在工作。

**修复**：`rustup component add rust-analyzer --toolchain stable-...` 装 1.95 的 analyzer，
让 **analyzer / rustc / rust-src 三者同版本**。`lsp.rs` 在 spawn 时把该工具链的 `bin` 前置到 `PATH`。

**验证**：`Vec` 跳转到 `library/alloc/src/vec/mod.rs:437`，`.len()` 跳转到同文件 `:2995`。
项目内 `definition` / `references` / `symbols` 三个动作在真实小 crate 上通过（8.12 s）。

### 顺带修正的一处测试脚本缺陷

我最初用脚本直连 rust-analyzer 测试时，`definition` 一直返回空，差点误判成环境问题。
真实原因是脚本**漏发 `initialized` 通知**——LSP 规定 `initialize` 响应之后必须发它，
否则服务器不进入工作状态。这个坑与本次修复无关，但值得记：**验证脚本本身也会骗人**。

## Alternatives considered

- **CDP：让用户升级/重装 Edge**：本机 Edge 已是 156，且手动启动同样超时，升级不解决。
- **CDP：改用内嵌 WebView2 线**：那是产品主路径（已验证可用），但外部 CDP 线有独立价值
  （不占用用户视野、可多会话并行），修好比删掉好。
- **rust-src：卸载 1.92 独立安装，全用 rustup 1.95**：能根治「两套 Rust」的混乱，
  但会改动用户整个 Rust 环境，风险远超本次问题范围。留给用户决定。
- **rust-src：忽略标准库跳转**：标准库类型（`Vec`/`String`/`Option`）是 Rust 代码里出现
  频率最高的符号，跳不了等于这个功能价值打对折。

## Consequences

- 外部 CDP 线现在可用，代价是浏览器渲染进程没有沙箱。它只加载一次性 profile 做自动化，
  不接受用户登录态，风险可接受。若将来要跑用户自己的登录会话，应重新评估。
- `lsp` 工具现在依赖 rustup 工具链存在。没有 rustup 的机器会退回 `PATH` 上的 analyzer，
  行为与之前一致（标准库跳转可能不可用），不会更糟。
- 本机 PATH 上仍是 1.92 的 `rustc`，`cargo build` 用的还是 1.92；只有 LSP 走 1.95。
  两套并存的状态没有改变，只是 LSP 现在挑了对的那套。
- `RUST_SRC_PATH` 用户环境变量我设置过，实测对 rust-analyzer 无效，留着无害但也没有作用。
