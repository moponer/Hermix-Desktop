# Decision Note: 用 async-lsp 接代码智能（definition / references / hover / symbols / diagnostics）

Status: implemented — 新增 `lsp` 工具，复用 async-lsp 客户端，不自己实现协议

## Decision

新增 `lsp` 工具，把「这个符号在哪定义 / 谁在调用它 / 它是什么类型」这类问题交给真实的
语言服务器回答。支持 rust-analyzer、pyright、typescript-language-server、gopls。

### 为什么必须做

实测本仓库的工具调用分布：`read_file` 140 次、`terminal` 122 次、`edit_file`/`patch`
17 次——读写比 **5.7:1**。模型不是不想改代码，是它**没有向代码库提问的手段**：只能
正则搜、然后一个个文件读进上下文。语言服务器用一个往返给出精确答案（带文件、行、列、
以及该行源码），替代「搜索 + 若干次读取」。

### 为什么用 async-lsp，不自己写 JSON-RPC

这是本次的关键决策，也是**不要造轮子**原则的直接应用。

| 候选 | 判断 |
|---|---|
| **async-lsp 0.2.4** | ✅ 采用。212 万下载，2026-04 更新，官方 README 明写支持 client 侧，自带 `client_builder` 示例（initialize/didOpen/hover/shutdown 全流程） |
| tower-lsp 0.20 | ❌ 否决。2023-08 后无更新；实践中是**服务端**框架；其通知处理是异步的，维护者自己在 tower-lsp#284 里承认这**语义上是错的**（通知必须有序） |
| 自己写 JSON-RPC | ❌ 否决。要自己实现 Content-Length 分帧、请求/响应关联、取消、通知顺序——每一项都容易出微妙错误，而这些正是 async-lsp 已经解决的 |

依赖代价很小：`lsp-types`（协议类型，唯一权威来源）、`tower`（builder 模式需要）、
`tokio-util`（compat 桥接，已在依赖树里）。

### 实现要点

- **一个进程池**，按 `(workspace root, language)` 键控。rust-analyzer 启动要几秒到几分钟
  （它要索引 crate 图），每次调用都重启等于不可用。进程在 `Drop` 时杀掉。
- **工作区根靠向上找标记文件**（`Cargo.toml` / `package.json` / `pyproject.toml` /
  `go.mod` / `.git`）。在 `src/deep/nested/` 里启动 rust-analyzer 会索引不到任何东西。
- **诊断是服务器主动推送的通知**，LSP 没有「取诊断」的请求。所以用 `PublishDiagnostics`
  的 handler 缓存起来，`action=diagnostics` 读缓存。
- **位置转换只在一个地方做**：LSP 是 0-based，人和 `grep -n` 是 1-based。传 0 直接报错，
  因为「差一行」会静默返回上一行的符号——这是最难发现的错误类型。
- **服务器没装要报「去装」**，不能报超时。`which` 检查在 spawn 之前。

### 一个工具而不是六个

`definition` / `references` / `hover` / `symbols` / `diagnostics` 都吃 (文件, 位置)，
只是发的 LSP 请求不同。做成一个工具带 `action` 参数，工具列表更短，模型的选择也更简单。

## Alternatives considered

- **只做 references + definition**：hover（类型）和 diagnostics（错误）是同一个连接上的
  额外请求，成本几乎为零，砍掉反而让模型退回「读文件猜类型」。
- **接入 rust-analyzer 的 CLI 输出而非 LSP**：rust-analyzer 没有稳定的 CLI 查询接口，
  只有 LSP。
- **每个语言单独一个工具**（`lsp_rust`、`lsp_python`…）：工具列表膨胀，且模型需要先
  判断语言——而扩展名已经能判断了。
- **把语言服务器常驻在 server 进程里**：会与 `hermix-tools` 的进程边界冲突；当前用
  进程级池，够用且简单。

## Consequences

- `lsp` 注册在 `code` 工具集（essential）。模型默认看得见它。
- **只有 rust-analyzer 装了**（1.92.0）。pyright / typescript-language-server / gopls
  未安装，调用会得到明确的「请安装」错误而不是静默失败。这是有意的：错误信息就是
  安装指令。
- 语言服务器进程常驻会占内存（rust-analyzer 索引本仓库约数百 MB 到 1GB+）。
  进程在工具句柄 drop 时被杀；当前池是进程级单例，不会随会话回收。
- 首次调用某个语言会很慢（rust-analyzer 索引）。这是语言服务器的固有成本，不是本工具的
  缺陷；后续调用是毫秒级。
- **本改动不解决延迟问题**。API 中位数 90 秒才是编码体感差的根因，那是模型通道问题。
  LSP 解决的是「一个往返换掉多次读取」，能把轮次压下来，但压不掉单轮延迟。
## 实现过程中发现的三个真实陷阱

这三条都不是文档写明的，是跑通之前一个个撞出来的。记下来是因为**任何自己实现 LSP 客户端的人都会撞上**，而这正是「不要造轮子」之外还要选对轮子的理由。

### 1. 未处理的通知会直接杀死 mainloop

`async-lsp` 对**未注册 handler 的通知**报 `Unhandled notification: window/showMessage`，然后**终止整个 mainloop**。mainloop 一关，写端就断了，服务器继续发消息时收到 `SendError` 并 panic（`rust-analyzer reload.rs:299` / `global_state.rs:585`）。

表面症状是「client exited without proper shutdown sequence」，看起来像服务器崩了，实际上是客户端先死。`showMessage` / `logMessage` 必须显式注册成 no-op——没有「忽略未知通知」的开关。

### 2. 服务器会向客户端发请求，不答就崩

LSP 是**双向**的。rust-analyzer 在 `initialized` 之后立刻发 `workspace/configuration`；客户端回 MethodNotFound 时它打 `failed to fetch the server settings` 然后走异常路径。必须实现：

- `workspace/configuration` → 按 `params.items.len()` 回等长的 `[{}]`
- `window/workDoneProgress/create` → 空确认

### 3. 不要给 `cargo.sysroot` 传 `null`

为了容忍本机缺失的 `rust-src`，我曾发 `{"rust-analyzer":{"cargo":{"sysroot":null}}}`。结果 RA 把 `null` 理解成「**明确知道 sysroot 为空**」，在 setup 阶段直接退出。正确做法是**什么都不发**，让它用默认值——缺 `rust-src` 对 RA 只是 warning，它照样服务用户自己的 crate。

## 本机环境现状（诚实记录）

- `rust-analyzer 1.92.0` 已装，工作正常。
- **`rust-src` 缺失且无法修复**：`rustup component list --installed` 列出 `rust-src`，`rustup component add rust-src` 回「已是最新」，但 `lib/rustlib/src` 目录不存在。这是 rustup 的状态不一致，不是本仓库的问题。后果：标准库符号（`Vec`、`String` 等）的跳转不可用，用户自己的代码完全正常。
- pyright / typescript-language-server / gopls 均未安装。调用会得到「请安装 X」的明确错误，而不是静默失败。

## 验证方式

- 8 个单元测试（位置转换、语言表、工作区根、schema 完整性、错误路径）默认运行。
- 1 个端到端测试默认 `#[ignore]`（会启动 rust-analyzer 并索引）：`cargo test -p hermix-tools --lib lsp_real -- --ignored --nocapture`。它在真实的小 crate 上验证 definition / references / symbols 三个操作。**已实测通过，2.28 秒。**
