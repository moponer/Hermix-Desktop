# Decision Note: viewer 开放能力 —— 自实现规范兼容的 MCP 服务（文件能力 + GUI 自动操作）

Status: implemented — `hermix-viewer --mcp` 以 MCP 规范要求的「换行分隔 JSON-RPC」在 stdio 上提供服务，任意外部 AI（Claude Desktop / Cursor / VS Code 等）配一行配置即可把查看器当工具用：文件管理器的预览 / 搜索 / 编辑，以及一组桌面 GUI 操作（列窗口 / 找控件 / 点击 / 填文本 / 按键）。

关联：[hermix-viewer 双模式化与全能力重构](../feature/2026-10-19-hermix-viewer-overhaul.md)、[viewer 顶部路径栏万能输入 + 全盘索引搜索](../feature/2026-10-20-viewer-pathbar-universal-input-and-global-search.md)

## Decision

**1. viewer 自己实现 MCP 服务端，不引 `hermix-mcp`。** 新增 `apps/viewer/src-tauri/src/mcp_server.rs`，协议形态：

- **换行分隔的 JSON-RPC 2.0**（MCP 规范对 stdio 传输的要求），一行一帧，`stdout` 走协议、`stderr` 走日志。
- 方法：`initialize` / `ping` / `tools/list` / `tools/call` / `resources/list` / `prompts/list`；通知（无 `id`）不回帧。
- 工具结果一律走 `content: [{type:"text", text}]` + `isError`，输出截断到 8,000 字符（工具结果会被读进模型下一轮的上下文，无上限就是上下文炸弹）。

**2. 暴露两组能力。**

文件能力（用户点名的「预览 / 搜索 / 编辑」）：

| 工具 | 作用 | 风险标注 |
|------|------|---------|
| `viewer_list_dir` | 列目录 | 只读 |
| `viewer_read_file` | 预览：文本类返回内容，图片/PDF/二进制返回类型与体积摘要 | 只读 |
| `viewer_search` | 给了 `path` 在该目录内搜；不给则走**全盘索引**（类 Everything） | 只读 |
| `viewer_write_text` | 写文本文件。目标已存在时**必须**显式传 `overwrite=true` | 破坏性 |

GUI 能力（“agent 自动点 GUI”）：

| 工具 | 作用 |
|------|------|
| `desktop_list_windows` | 列顶层窗口（hwnd / 标题 / 类名 / pid） |
| `desktop_list_controls` | 列某窗口下的子控件（hwnd / 类名 / 文本 / 控件 id） |
| `desktop_click` | 点击控件（按钮走 `BM_CLICK`，其余走鼠标消息序列） |
| `desktop_set_text` | 设置控件文本（`WM_SETTEXT`，直接写内容，不是逐键敲字） |
| `desktop_read_text` | 读控件文本 |
| `desktop_press_key` | 发一次按键（默认回车） |

每个工具都带 MCP `annotations`（`readOnlyHint` / `destructiveHint`），客户端（Claude Desktop 等）据此决定是否弹权限确认。

**3. GUI 走 Win32 消息，不走 UI Automation。** 新增 `apps/viewer/src-tauri/src/gui.rs`，只用 `windows-sys`（已在依赖树，只加 `Win32_UI_WindowsAndMessaging` feature）。

**4. `viewer_run_executable` 不暴露给 MCP。** GUI 模式里运行 exe 前会弹**原生确认框**（用户亲手点「运行」）；MCP 模式没有窗口、弹不了框，让外部 AI 直接跑任意 exe 是最短的误触路径。该能力留在 GUI 模式。

**5. 外部 AI 的接法**（Claude Desktop / Cursor 的 MCP 配置，命令指向 viewer 可执行文件）：

```json
{
  "mcpServers": {
    "hermix-viewer": {
      "command": "C:\\path\\to\\hermix-viewer.exe",
      "args": ["--mcp"]
    }
  }
}
```

## Alternatives considered

1. **用 `crates/hermix-mcp` 的 `McpServer`** ——拒绝，两个硬理由：
   - **分帧不合规范**：`crates/hermix-mcp/src/transport.rs` 的 stdio 走 LSP 式 `Content-Length:` 头，而 MCP 规范要求换行分隔的 JSON-RPC。hermix↔hermix 自洽，但**第三方客户端连不上**——而「任意外部 AI」正是本篇的目标。
   - **依赖面爆炸**：`hermix-mcp` → `hermix-tools` 会拖进 chromiumoxide / rusqlite / docx-rs / ppt-rs / rust_xlsxwriter。viewer 是 8 个依赖的轻量 sidecar，不该背这些。
2. **viewer 起 HTTP 端口给外部 AI 调** ——拒绝：新增一个网络监听面 + 一套新鉴权面，与仓库既有 `Origin::may` 权限体系割裂；且已有 `SINGLETON_PORT`（3907）是**明文单向投递协议**，不是请求/响应，不能直接复用。
3. **把 `hermix-tools` 的 UIA（`os_auto/desktop/uia.rs`）搬进 viewer** ——拒绝：UIA 需要 `windows` crate 的 COM 接口层 + 独立 MTA 工作线程（COM apartment 线程绑定），且 `hermix-tools` 本身绑着整套重依赖。Win32 消息方案覆盖了「点按钮 / 填输入框 / 触发按键」这一档需求，且**零新依赖**。
4. **抽一个 `hermix-uia` 独立 crate，让 hermix-tools 与 viewer 共用** ——备选但本轮不做：架构上更干净，但要动 `hermix-tools` 的模块边界（并行线上还有别的改动在 crates 里）。当前 Win32 版能力弱于 UIA（不能按无障碍名称定位、不做真实鼠标移动），但对「agent 点 GUI」够用，且没有复制代码。
5. **在 MCP 模式下也暴露「运行 exe」** ——拒绝（安全）：见 Decision 第 4 条。
6. **MCP 模式下让 `viewer_write_text` 直接覆盖** ——拒绝：AI「顺手改一下」就抹掉用户文件是最容易发生也最不可逆的事。默认拒绝覆盖、要求显式 `overwrite=true`，是这条工具唯一需要用户知情的开关。
7. **把 hermix 的 agent loop 包成 MCP（`hermix mcp serve`），让外部 AI 驱动整个 hermix** ——拒绝（本轮）：`crates/hermix-mcp/src/server.rs` 的 `McpServer` 全仓库**只在自身单测里被实例化**（死代码），没有 `hermix mcp serve` 入口；从死代码起步要先补分帧、补 bin、补审批闸门，属于另一件事。
8. **复用 hermix-server 的 `POST /v1/commands`（外部一句自然语言 → agent → 工具）** ——部分采纳作背景：这条路已跑通且能触发 GUI 工具，但它要求 hermix-server 以 HTTP 模式跑（桌面端是 `--stdio`，不绑端口），且只给文本回执、不给结构化工具结果。本篇的 MCP 面补的是「结构化工具 + 规范兼容」这一半。

## Consequences

- **新增 Rust 依赖：0**；新增前端 npm 依赖：0；新增网络监听面：0（MCP 走 stdio 子进程管道）。
- **安全边界与 hermix 侧不同，必须讲清楚**：
  - hermix 内部那条路的工具调用**经过**审批闸门（`crates/hermix-agent/src/local_agent_service.rs::decide_pre_tool_dispatch`：`Safe` 放行 / `Caution` 放行 / `Dangerous` 要审批卡）。
  - 本篇的 MCP 面**没有本地审批卡**（stdio 模式没有窗口可弹审批 UI）。边界是 **MCP 客户端自己的权限提示** + 工具上的 `destructiveHint` 标注 + `viewer_write_text` 的显式 overwrite 开关。GUI 的三个写操作（click / set_text / press_key）全部标了 destructive。
  - `crates/hermix-mcp/src/server.rs::handle_tools_call` 直接 `registry.dispatch_async` → `handler.execute`，**绕过**上面那道闸门；同时 GUI 工具在 hermix 侧是 `Caution` 级（`local_agent_service.rs` 对 `Caution` 一律放行）。也就是说「从网络可达的 WS 触发真实 GUI 操作、无人审批」这个缺口在 hermix 侧**仍然存在**，本篇没有顺手改它——改 `Caution → Dangerous` 会改变现有 hermix agent 的行为，需要单独决策。
- **GUI 能力边界**（写进代码注释，避免调用方误判）：能按窗口标题 / 控件类名 / 控件文本定位，**不能**按无障碍名称定位；点击不做真实鼠标移动；填文本是直接写控件内容而非逐键敲字。需要完整 UIA 树时用 hermix agent 的 `win_snapshot` 工具。
- **`--mcp` 模式不开窗口、不起单实例互斥体、不监听 3907 端口**：外部 AI 要的是工具，不是另一个查看器窗口。
- **`stdout` 是协议通道**：MCP 模式里任何 `println!` 都会污染帧，所以该模式的日志一律走 `stderr`。
- **Rust 侧有 6 条单测锁住协议契约**（initialize 响应形态、tools/list 覆盖度、写文件必须显式 overwrite、未知方法返回 -32601、通知不回帧、helper 条目 → 视图条目映射）。

## Reintroduction conditions

- 若外部 AI 反馈「找不到按钮」成为高频问题（即 Win32 消息定位不够用），则抽 `hermix-uia` 独立 crate，把 `hermix-tools` 的 UIA 实现迁过去，viewer 与 hermix-tools 共用同一份，不再各自维护。
- 若 MCP 客户端普遍忽略 `annotations`（不做权限提示），则必须在 viewer 侧自建审批：stdio 模式可改为「写一个待批准请求文件 + 轮询」，由 GUI 模式弹出原生确认框。
- 若需要把 hermix 的完整工具表（含 GUI 与浏览器自动化）暴露给外部 AI，则走 `hermix mcp serve`：先给 `crates/hermix-mcp` 的 stdio 增加换行分隔传输（不动现有 Content-Length 通道，避免破坏 hermix↔hermix 内部通信），再补 bin 入口与审批闸门接线。
