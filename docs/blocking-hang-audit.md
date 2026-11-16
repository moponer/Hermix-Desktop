# 阻塞型卡死点清单（hermix-desktop）

范围：Tauri v2 桌面端（`apps/desktop-tauri`）+ Rust crates（`crates/*`）+ React 前端（`apps/desktop-tauri/src`）。本轮只读源码，未启动应用、未跑测试与构建，所有毫秒耗时均未实测。

线程归属口径：Tauri v2 把 async command 调度到 async runtime、把同步 command 放在主线程执行，属框架行为；本仓库无 Tauri 源码可核对，相关判定一律标「推测」，并在条目内给出仓库内旁证出处。

## 已确认发现

### 1. 同步整文件读 + base64 编码（tokio worker）
- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:671`（`pub async fn read_artifact_file`）、`:675`（2 MiB 上限）、`:723`（整读）、`:743`（base64）；同型第二处 `:764`（`pub async fn read_artifact_file_bytes`，10 MiB 上限）、`:812`（读）、`:814`（base64）。
- 阻塞机制：`std::fs::read` 整文件读入内存，再 `STANDARD.encode(&bytes)` 生成 1.33 倍体积字符串。命令是 `pub async fn`，函数体在 async runtime 的 worker 上执行，未交给 `spawn_blocking`；读文件与 base64 编码期间该 worker 被占住。
- 阻塞线程归属：tokio worker（async 命令体未交给 `spawn_blocking`）。「async command 调度到 async runtime」属框架行为，标**推测**；仓库内旁证：`commands.rs:944` 注释「spawn_blocking 把它挪到阻塞线程池」、`:952` 实际调用 `spawn_blocking`；`crates/hermix-agent/src/session_persistence/mod.rs:72` 注释「inline from an async handler that holds a worker for seconds」。
- 触发条件：打开产物文件预览（≤ 2 MiB 上限）；或走 bytes 通道读二进制文件（≤ 10 MiB 上限）。
- 界面表现：占住一个 worker；窗口标题栏拖动不会变灰，「窗口整体无响应」对本条不成立。受影响的是同一 runtime 上其它命令排队与延迟。
- 证据：
  ```rust
      let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;
      use base64::Engine;
      let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
  ```

### 2. 同步子进程调用 reg.exe（主进程主线程，启动阶段一次）
- 位置：`apps/desktop-tauri/src-tauri/src/webview2_probe.rs:90-98`（`probe()` 与 `CACHED_STATUS` 缓存）、`:108`（两个注册表视图的循环）、`:125`（`Command::new("reg")`）；唯一调用点 `apps/desktop-tauri/src-tauri/src/lib.rs:911`。
- 阻塞机制：`Command::new("reg").args(["query", &key, "/v", "pv"]).output()` 同步等待子进程退出；`:108` 的循环对 WOW6432Node 与原生两个视图各跑一次，最坏两次子进程启动。
- 阻塞线程归属：主进程主线程（`probe()` 是同步 `fn`，调用点在 Tauri `setup` 闭包内）。「setup 在事件循环启动前于主线程执行」属框架行为，标**推测**。
- 严重度：启动阶段主线程最多两次同步 `reg.exe` 查询，源码注释自述单次约 5 ms（`webview2_probe.rs:111-112`，**源码注释自述，未实测**），进程生命周期内只发生一次（`CACHED_STATUS` 缓存，`:72`、`:90-98`）。
- 触发条件：仅缓存未命中时发生（首次探测）；缓存命中直接返回，不重复付这两次子进程成本。
- 界面表现：事件循环启动前的一次性等待，窗口尚未进入可交互状态；不写「窗口空白/无响应数秒」。
- 证据：
  ```rust
      for view in [r"HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients", r"HKLM\SOFTWARE\Microsoft\EdgeUpdate\Clients"] {
  ```
  ```rust
              let output = Command::new("reg")
                  .args(["query", &key, "/v", "pv"])
                  .stdout(std::process::Stdio::piped())
                  .stderr(std::process::Stdio::null())
                  .creation_flags(CREATE_NO_WINDOW)
                  .output();
  ```

### 3a. 同步命令 `list_backend_log_files` 在主进程主线程枚举日志目录
- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:194-200`（同步命令 + `read_dir`），枚举后逐条只取 `metadata()`（`:206-212`）。
- 阻塞机制：同步 `fn` 命令体内 `std::fs::read_dir(&dir)` 同步枚举日志目录，单层、非递归；`commands.rs:275` 的 `read_dir` 属于 `read_backend_log_tail_blocking`，与本命令无关。
- 阻塞线程归属：主进程主线程（Tauri v2 同步 command 在主线程执行 —— 框架行为，标**推测**）。
- 触发条件：用户打开「日志」面板列出后端日志文件。
- 界面表现：枚举期间主线程被占住，日志面板内容出现延迟。
- 证据：
  ```rust
  #[tauri::command]
  pub fn list_backend_log_files() -> Result<LogFileInfo, String> {
      ...
          let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
  ```

### 3b. `read_backend_log_tail` 在阻塞线程池内回读日志尾部（不占主线程）
- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:253-257`（async 命令 + `spawn_blocking`）、`:261`（64 KiB 块）、`:263`（4 MiB 上限）、`:265`（阻塞实现）、`:302-303`（`File::open` + 流式计数）、`:325-326`（从文件尾按块回读）。
- 阻塞机制：`spawn_blocking` 内的同步实现：从文件尾按 64 KiB 块回读，累计封顶 4 MiB；总行数用流式计数（O(size)、常数内存），既不是整读也不是逐行读全文件。
- 阻塞线程归属：阻塞线程池（`spawn_blocking`），不占主进程主线程，也不长期占住 async runtime worker。
- 触发条件：打开日志面板并请求尾部内容，文件大于 4 MiB 时触发多次回读。
- 界面表现：日志尾部内容延迟返回，单次回读量封顶 4 MiB。
- 证据：
  ```rust
  pub async fn read_backend_log_tail(tail_lines: u64) -> Result<BackendLogTail, String> {
      tokio::task::spawn_blocking(move || read_backend_log_tail_blocking(tail_lines))
  ```
  ```rust
      while pos > 0 && taken < LOG_TAIL_MAX_BYTES {
          let chunk = LOG_TAIL_CHUNK.min(pos).min(LOG_TAIL_MAX_BYTES - taken);
  ```

### 4. 单层目录枚举（tokio worker）
- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:1098-1099`（`pub async fn list_directory`）、`:1133-1134`（`std::fs::read_dir`）。
- 阻塞机制：`std::fs::read_dir(&canonical)` 同步枚举一层目录项，函数体内只有一次 `read_dir`，未递归进子目录；命令为 `pub async fn`，枚举本身是同步阻塞调用，未交给 `spawn_blocking`。
- 阻塞线程归属：tokio worker（async 命令体未交给 `spawn_blocking`）；依据同发现 1 的两条仓库内旁证，Tauri 侧调度行为标**推测**。
- 触发条件：文件选择器/产物浏览打开含大量文件的目录，尤其网络盘或 OneDrive 同步目录。
- 界面表现：同一 runtime 上其它 IPC 排队、延迟；窗口不一定整体无响应。
- 证据：
  ```rust
      let entries = std::fs::read_dir(&canonical)
          .map_err(|e| format!("cannot read directory '{}': {e}", canonical.display()))?;
  ```

### 5. SQLite `SQLITE_BUSY` 应用层忙等重试（sidecar 进程内 tokio worker）
- 位置：`crates/hermix-agent/src/session_persistence/sessions.rs:259`（`std::thread::sleep(sleep);`）、`crates/hermix-agent/src/session_persistence/mod.rs:119-121`（重试次数与间隔常量）、`:125`（2 s 总预算常量）、`:127-134`（`is_busy_error`）、`:81-96`（`run_blocking` / `block_in_place`）、`crates/hermix-agent/src/local_agent_service/mod.rs:864`（调用点）。
- 阻塞机制：SQLite 写冲突（`SQLITE_BUSY` / `database is locked`）触发应用层忙等重试，`std::thread::sleep(sleep)` 后重写，最坏受 `WRITE_RETRY_BUDGET` 2 s 总预算约束（`WRITE_MAX_RETRIES = 15`、`WRITE_RETRY_MIN_MS = 20`、`WRITE_RETRY_MAX_MS = 150`）。源码里没有文件锁，只有 SQLite busy 判定。
- 阻塞线程归属：sidecar 进程内的 tokio worker。调用点包在 `run_blocking` 内（`local_agent_service/mod.rs:864`），MultiThread runtime 上走 `tokio::task::block_in_place`（`mod.rs:92`）把 worker 交回阻塞池，单线程 runtime 直接同步执行（`:94`）。主进程侧无此调用链：`apps/desktop-tauri/src-tauri/Cargo.toml` 依赖里没有 `hermix-agent`，`hermix-agent` 由 `crates/hermix-cron/Cargo.toml:13`、`crates/hermix-runtime/Cargo.toml:11`、`crates/hermix-server/Cargo.toml:26` 依赖。
- 触发条件：多线程并发写同一 session 数据库触发 `SQLITE_BUSY`。
- 界面表现：桌面主进程不受影响；sidecar 内的等待表现为 sidecar 自身请求延迟，最坏 2 s。
- 证据：
  ```rust
                      std::thread::sleep(sleep);
  ```
  ```rust
      let msg = e.to_string();
      msg.contains("SQLITE_BUSY") || msg.contains("database is locked")
  ```
  ```rust
              tokio::task::block_in_place(f)
  ```

## 检索面逐条结果（12 条）

- **G1 · 未发现阻塞点（不构成阻塞）** —— `apps/desktop-tauri/src/app/streamEvents.ts:55`，`localizeStreamError` 内 `JSON.parse(normalized)`。机制：把单条 provider 错误文本当 JSON 解析取 `error.message`，失败走 catch 兜底；输入体量由单条消息决定，不是大文本或大数组。归属：WebView2 渲染线程。界面表现：无卡顿。证据：`const parsed = JSON.parse(normalized);`
- **G2 · 未发现阻塞点（不构成阻塞）** —— `apps/desktop-tauri/src/App.tsx:212`，`useState` 惰性初始化内 `JSON.parse(raw)`。机制：启动时从 `localStorage` 读待发队列 `hermix.outbox.v1` 并解析后 filter，只在状态初始化阶段跑一次，不随每次渲染重跑；队列体量受待发消息条数限制。归属：WebView2 渲染线程。界面表现：无卡顿。证据：`const parsed = JSON.parse(raw) as QueuedMessage[];`
- **G3 · 已确认存在（轻度，规模未实测）** —— `apps/desktop-tauri/src/components/MarkdownBody.tsx:507`，`transformMath` 内 `md.split("\n")`。机制：整段 markdown 按换行切成行数组再逐行判断公式块并回写，同步执行，代价随消息长度线性增长；单次线性扫描，无嵌套循环。归属：WebView2 渲染线程。界面表现：长消息渲染时一帧变长。证据：`const lines = md.split("\n");`
- **G4 · 已确认存在** —— `apps/desktop-tauri/src/components/MarkdownBody.tsx:269-270`，`useMemo` 内同步调用 `highlightToReact(code, language, isDark)`，被调函数在 `apps/desktop-tauri/src/lib/codeHighlight.ts:383`。机制：渲染线程同步跑 refractor 分词并转 React 节点；`useMemo` 依赖 `[code, language, isDark, langVersion]`，同一段代码不重复计算；长尾语言未加载时先返回纯文本，包装到位后由 `langVersion` 触发一次重算。归属：WebView2 渲染线程，不是主进程主线程。界面表现：长代码块渲染时一帧变长；源码注释自述约 8 ms / 1000 行 Python（**源码注释自述，未实测**）。证据：`const highlighted = useMemo(() => highlightToReact(code, language, isDark), [code, language, isDark, langVersion]);`
- **G5 · 已确认存在（规模未知）** —— `apps/desktop-tauri/src/components/bubbles/ToolOutputPreviews.tsx:32`，`ScreenshotPreview` 的 `useMemo` 内 `JSON.parse(output)`。机制：把工具输出字符串当 JSON 解析提取 `width/height/bytes/monitor_name/saved_to`，失败返回 null。归属：WebView2 渲染线程。界面表现：取决于工具输出长度，该长度上限未核到。证据：`const v = JSON.parse(output);`
- **G6 · 已确认存在** —— `apps/desktop-tauri/src-tauri/src/commands.rs` 4 处：`:200`（同步命令 `list_backend_log_files` 的 `read_dir`，落主进程主线程）、`:723`（`read_artifact_file` 整读 + base64）、`:812`（`read_artifact_file_bytes` 整读 + base64）、`:1133-1134`（`list_directory` 的 `read_dir`），后三处在 async 命令体内，落 tokio worker。归属修正：只有 `:200` 所在同步命令真正落在主进程主线程。界面表现：见发现 1/3a/4。证据：见上述三条对应的代码片段。
- **G7 · 未发现阻塞点** —— 主清单引用的 `crates/hermix-agent/src/llm_retry.rs:295` 落在注释块内（相邻注释实际在 `:293`），真实等待在 `:320` 的 `sleep(delay).await`，用的是 `tokio::time::sleep`（`llm_retry.rs:17` `use tokio::time::sleep;`）。机制：异步挂起，等待期间不占线程。归属：不占任何阻塞线程。界面表现：无卡顿。证据：`sleep(delay).await;`
- **G8 · 已确认存在（线程归属标推测）** —— `crates/hermix-agent/src/session_persistence/sessions.rs:259`。机制同发现 5：SQLite `SQLITE_BUSY` 应用层重试，睡眠 20–150 ms 抖动、最多 15 次，受 `WRITE_RETRY_BUDGET` 2 s 封顶；调用点 `run_blocking` 在 MultiThread runtime 上走 `block_in_place`，worker 交回阻塞池。归属：sidecar 进程内的 tokio worker，**进程归属为推测** —— sidecar 二进制与 `hermix-server` 的构建对应关系未逐行核对；桌面主进程无此调用链（主进程 `Cargo.toml` 依赖里没有 `hermix-agent`）。缺什么证据：sidecar 构建对应关系、触发 `SQLITE_BUSY` 的真实并发场景。界面表现：sidecar 自身请求延迟最坏 2 s。证据：`std::thread::sleep(sleep);`
- **G9 · 未发现阻塞点** —— `crates/hermix-core/src/win_job.rs:336` 的 `std::thread::sleep(Duration::from_millis(500))` 位于 `#[cfg(test)] mod tests`（模块起于 `win_job.rs:259-260`），是测试断言前的等待，不在生产路径。归属：不适用。界面表现：无。证据：`assert!(guard.is_some(), "带默认 worker 限额的附着应当成功");`
- **G10 · 已确认存在（线程归属标推测）** —— `crates/hermix-cron/src/file_lock.rs:156`，`acquire`（`:86`）内的退避重试，间隔常量 `RETRY_INTERVAL = 100 ms`（`:56`），timeout 上界判断在 `:136`、`:146-152`。机制：文件锁被别的进程占住时每 100 ms 退避重试一次，直到调用方给的 `timeout` 到期返回 `LockError::Timeout`；有上界，不是无限忙等。归属：**推测** —— cron 调度线程；本轮未定位 `hermix-cron` 的 `acquire` 调用点，未确认它跑在桌面主进程还是 sidecar、是否在 UI 可感知线程上。缺什么证据：`file_lock::acquire` 的调用点位置与所在进程/线程。界面表现：未确定。证据：`std::thread::sleep(RETRY_INTERVAL);`
- **G11 · 未发现阻塞点** —— `crates/hermix-mcp/src/transport.rs:1021`（`wait_dead` 测试辅助函数）与 `:1148`（`#[tokio::test]` 用例内）的两处 `std::thread::sleep(Duration::from_millis(100))` 都在测试代码里，不在生产路径。归属：不适用。界面表现：无。证据：`std::thread::sleep(Duration::from_millis(100));`
- **G12 · 已确认存在（与 G4 同一处代码，不重复计数）** —— `apps/desktop-tauri/src/lib/codeHighlight.ts:383`，`export function highlightToReact`。机制：refractor 同步分词 + `hast-util-to-jsx-runtime` 转 React 节点，失败降级返回原文；长尾语言按需加载，未就绪时返回纯文本。归属：WebView2 渲染线程。界面表现：长代码块渲染时一帧变长；耗时只有源码注释自述值（`codeHighlight.ts:379-381`，**未实测**）。证据：`export function highlightToReact(`

## 未实测与缺证

1. **所有毫秒耗时未实测**：本轮禁止启动应用、跑测试与构建。文中耗时只有源码注释自述值 —— `webview2_probe.rs:111-112` 的「~5ms」与 `codeHighlight.ts:379-381` 的「~8 ms for 1000 lines of Python」，两者均标「源码注释自述，未实测」。
2. **G3 / G4 / G5 / G12 的体量上限未找到**：仓库内未找到 markdown 消息、代码块、工具输出字符串的截断上限，故无法给出触发阈值。缺：截断代码位置；工具输出写回路径的实现位置。
3. **G10 调用链未定位**：`hermix-cron` 的 `file_lock::acquire` 调用点与所在进程/线程未确认，线程归属标推测。
4. **G8 进程归属为推测**：sidecar 二进制与 `hermix-server` crate 的构建对应关系未逐行核对。
5. **框架行为均为推测**：Tauri v2 的 async command 调度到 async runtime、同步 command 在主线程执行、`setup` 在事件循环启动前于主线程执行，均无仓库内 Tauri 源码可 pin；仓库内旁证只有 `commands.rs:944`、`:952` 与 `session_persistence/mod.rs:72` 三处注释。
6. **未覆盖的相邻代码**：`apps/desktop-tauri/src-tauri/src/webview2_probe.rs:152` 另有第二处 `Command::new("reg")`，本轮未 read_file 核对上下文，不计入发现条目；`crates/hermix-mcp`、`hermix-environments`、`hermix-tools` 等 crate 的同步 IO 与轮询细节未逐条核对。
7. **检索面 c（同步 IPC）**：仍记 0 命中，本轮未新增核对。Tauri v2 无 `ipcRenderer.sendSync` 同构 API，已核查 `apps/desktop-tauri/src` 与 `src-tauri/src` 未见同步阻塞式 invoke 封装。
