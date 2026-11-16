# 阻塞型卡死点清单 · 修正版（hermix-desktop）

本文件取代 `docs/blocking-hang-audit.md`（原主清单）。原主清单 5 条发现经 `docs/blocking-hang-audit-review.md` 逐条回源复核后全部判为「需修正」，本版按复核结论改写，并合并 `docs/blocking-hang-audit-gaps.md` 的补漏条目（G1~G12）与 `docs/wait-timeout-audit-appendix-frontend.md` 中与阻塞维度相关的条目（F-5/F-6/F-7）。

约束：本轮只读源码，未启动应用、未跑构建与测试，所有毫秒耗时均未实测；文中出现的耗时只有源码注释自述值，逐处标注「源码注释自述，未实测」。本文件不引用《等待时间维度清单》的任何说法。

线程归属口径（五类，逐条标注）：主进程主线程 / tokio worker（桌面主进程 async runtime）/ sidecar 进程内 worker / WebView2 渲染线程 / 独立线程与阻塞线程池（`spawn_blocking`）。

---

## 一、总体结论

确认成立的阻塞点 9 条，按是否冻结界面分两类。

**会让界面看起来卡死（4 条，全部落在主进程主线程）**：
1. `list_backend_log_files`（`apps/desktop-tauri/src-tauri/src/commands.rs:194-200`）—— 同步 `fn` 命令里 `std::fs::read_dir` 枚举日志目录，打开日志面板时窗口事件循环停摆。
2. `sidecar::stop()`（`apps/desktop-tauri/src-tauri/src/sidecar.rs:294`、`:314`、`:324`）—— 退出路径上同步轮询等 sidecar 子进程退出，预算 5s。
3. `wait_filesearch_gone`（`apps/desktop-tauri/src-tauri/src/shutdown.rs:56`、`:100-110`）—— 退出路径上同步轮询等文件索引守护进程退出，预算 2s。2 与 3 串在同一条退出链上，最坏合计约 7s。
4. `cdp_n0_probe`（`apps/desktop-tauri/src-tauri/src/cdp_probe.rs:33`、`:107`）—— 主线程同步命令，内层 `result_rx.recv()` 无超时，可永久阻塞；前端无调用点，只能手工 `invoke` 触发。

另有 1 条在主进程主线程但发生在窗口出现之前：`webview2_probe::probe` 同步拉起两次 `reg.exe`（`webview2_probe.rs:125`），进程生命周期内只付一次，用户只感觉启动慢。

**只占用 worker、不冻结界面（3 条）**：两条「整文件读 + base64」（`commands.rs:723`、`commands.rs:812`）与一条「单层目录枚举」（`commands.rs:1133`）都写在 `pub async fn` 命令体内，没交给 `spawn_blocking`，占住 tokio worker，后果是同一 runtime 上其它 IPC 排队与延迟。

**跨进程，不影响桌面界面（2 条）**：SQLite `SQLITE_BUSY` 忙等重试（`crates/hermix-agent/src/session_persistence/sessions.rs:259`）发生在 sidecar 进程内，最坏 2s；截图会话的 30s fallback 与 ESC 2s 轮询（`screenshot_controller.rs:301`、`:327`）在独立线程。

WebView2 渲染线程侧的重活（Markdown 按行切分、refractor 代码高亮）只让单帧变长，属于「占用渲染线程」而非「无响应」。所有阻塞点的实际耗时未实测，量级留空，见第四节。

---

## 二、确认成立的阻塞点清单

### A1 · 整文件读 + base64 编码（tokio worker）

- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:671`（`pub async fn read_artifact_file`）、`:675`（2 MiB 上限）、`:723`（整文件读）、`:742-743`（base64 编码）；同型第二处 `:764`（`pub async fn read_artifact_file_bytes`）、`:768`（10 MiB 上限）、`:812`（整读）、`:813-814`（base64）。
- 阻塞机制：`std::fs::read` 把整个文件读进内存，再由 `STANDARD.encode` 生成约 1.33 倍体积的字符串。两处命令都是 `pub async fn`，函数体内的同步 IO 与编码没有交给 `spawn_blocking`，执行期间占住一个 tokio worker。
- 阻塞线程归属：tokio worker。「Tauri v2 把 async command 调度到 async runtime」属框架行为，标**推测**；仓库内旁证两处 —— `commands.rs:944`（注释「spawn_blocking 把它挪到阻塞线程池」）与 `:952`（实际调用 `spawn_blocking`）；`crates/hermix-agent/src/session_persistence/mod.rs:72`（注释「inline from an async handler that holds a worker for seconds」）。
- 触发条件：打开产物文件预览（≤ 2 MiB 上限）；或走 bytes 通道读二进制文件（≤ 10 MiB 上限）。
- 界面表现：**只占用 worker，不冻结界面**。窗口标题栏拖动不会变灰；受影响的是同一 runtime 上其它命令排队与延迟。
- 证据片段：
  ```rust
  #[tauri::command]
  pub async fn read_artifact_file(
  ```
  ```rust
      let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;
  ```
  ```rust
      use base64::Engine;
      let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
  ```
- 严重度：中。体积有上限（2 MiB / 10 MiB），单次占用有界；风险点在并发多文件读取时 worker 被连续占满。

### A2 · 同步子进程调用 `reg.exe`（主进程主线程，启动阶段一次）

- 位置：`apps/desktop-tauri/src-tauri/src/webview2_probe.rs:90-98`（`probe()` 与 `CACHED_STATUS` 缓存）、`:108`（两个注册表视图的循环）、`:125`（`Command::new("reg")`）；唯一调用点 `apps/desktop-tauri/src-tauri/src/lib.rs:901`（`setup` 闭包内，`setup` 起点 `lib.rs:775`）。
- 阻塞机制：`Command::new("reg").args(["query", &key, "/v", "pv"]).output()` 同步等待子进程退出；`:108` 的循环对 WOW6432Node 与原生两个视图各跑一次，最坏两次同步子进程启动。
- 阻塞线程归属：主进程主线程。`probe()` 是同步 `fn`；「setup 在事件循环启动前于主线程执行」属框架行为，标**推测**。
- 触发条件：仅缓存未命中时发生（首次探测）。`CACHED_STATUS` 命中后直接返回，进程生命周期内不再重复付这两次子进程成本。
- 界面表现：**不是「界面卡死」**。事件循环启动前的一次性等待，窗口尚未进入可交互状态，用户看不到无响应过程。
- 证据片段：
  ```rust
  pub fn probe() -> WebView2Status {
      let cached = CACHED_STATUS.load(Ordering::Acquire);
      if cached != UNPROBED {
          return WebView2Status::from_u8(cached);
      }
  ```
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
- 严重度：低。源码注释自述单次子进程启动约 5 ms（`webview2_probe.rs:111-112`，**源码注释自述，未实测**），最多两次，进程生命周期内一次。

### A3 · 同步命令 `list_backend_log_files` 枚举日志目录（主进程主线程）

- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:194`（`#[tauri::command]`）、`:195`（`pub fn list_backend_log_files`，同步 fn）、`:200`（`std::fs::read_dir`），枚举后逐条只取 `metadata()`（`:206-212`）。
- 阻塞机制：同步 `fn` 命令体内直接 `std::fs::read_dir(&dir)` 同步枚举日志目录，单层、非递归，逐条目只取一次 `metadata()`。
- 阻塞线程归属：主进程主线程。依据是它被声明为同步 `fn`（Tauri v2 同步 command 在主线程执行 —— 框架行为，标**推测**）。
- 触发条件：用户打开「日志」面板列出后端日志文件。
- 界面表现：**会让界面看起来卡死**。枚举期间主线程被占住，窗口事件循环停摆，输入框与标题栏一起变灰；日志面板内容延迟出现。
- 证据片段：
  ```rust
  #[tauri::command]
  pub fn list_backend_log_files() -> Result<LogFileInfo, String> {
      let dir = log_dir();
      let mut files: Vec<LogFileEntry> = Vec::new();
      let mut total_bytes: u64 = 0;
      if dir.is_dir() {
          let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
  ```
- 严重度：中高。本轮唯一确认成立、由用户操作直接触发并看得见的界面冻结点。

### A4 · 单层目录枚举（tokio worker）

- 位置：`apps/desktop-tauri/src-tauri/src/commands.rs:1099`（`pub async fn list_directory`）、`:1133-1134`（`std::fs::read_dir`）。
- 阻塞机制：`std::fs::read_dir(&canonical)` 同步枚举一层目录项，函数体内只有一次 `read_dir`，未递归进子目录；命令是 `pub async fn`，枚举本身是同步阻塞调用，未交给 `spawn_blocking`。
- 阻塞线程归属：tokio worker（async 命令体未交给 `spawn_blocking`）。Tauri 侧调度行为标**推测**，旁证同 A1。
- 触发条件：文件选择器 / 产物浏览打开含大量文件的目录，尤其网络盘或 OneDrive 同步目录。
- 界面表现：**只占用 worker，不冻结界面**。同一 runtime 上其它 IPC 排队、延迟；窗口不一定整体无响应。
- 证据片段：
  ```rust
      let entries = std::fs::read_dir(&canonical)
          .map_err(|e| format!("cannot read directory '{}': {e}", canonical.display()))?;
  ```
- 严重度：中。单层枚举、无递归，规模取决于目标目录条目数与存储介质。

### A5 · SQLite `SQLITE_BUSY` 应用层忙等重试（sidecar 进程内 worker）

- 位置：`crates/hermix-agent/src/session_persistence/sessions.rs:232`（`WRITE_RETRY_BUDGET` 起点）、`:233`（重试循环）、`:242`（`is_busy_error` 判定）、`:259`（`std::thread::sleep(sleep);`）；`crates/hermix-agent/src/session_persistence/mod.rs:119-121`（重试次数与间隔常量）、`:125`（2 s 总预算常量）、`:133`（`is_busy_error`）、`:81-96`（`run_blocking` / `block_in_place`）；调用点 `crates/hermix-agent/src/local_agent_service/mod.rs:864`。
- 阻塞机制：SQLite 写冲突（`SQLITE_BUSY` / `database is locked`）触发应用层忙等重试，`std::thread::sleep(sleep)` 后重写，最坏受 `WRITE_RETRY_BUDGET` 2 s 总预算约束（`WRITE_MAX_RETRIES = 15`、`WRITE_RETRY_MIN_MS = 20`、`WRITE_RETRY_MAX_MS = 150`）。源码里没有文件锁，只有 SQLite busy 判定。
- 阻塞线程归属：sidecar 进程内的 tokio worker。调用点包在 `run_blocking` 内（`local_agent_service/mod.rs:864`），MultiThread runtime 上走 `tokio::task::block_in_place`（`mod.rs:92`）把 worker 交回阻塞池，单线程 runtime 直接同步执行（`:94`）。主进程侧无此调用链：`apps/desktop-tauri/src-tauri/Cargo.toml` 依赖里没有 `hermix-agent`；`hermix-agent` 只被 `crates/hermix-cron/Cargo.toml:13`、`crates/hermix-runtime/Cargo.toml:11`、`crates/hermix-server/Cargo.toml:26` 依赖，而 sidecar 就是 `hermix-server` 的 `[[bin]] name = "hermix"`（`crates/hermix-server/Cargo.toml:17-19`），主进程 spawn 的 `hermix.exe --stdio` 即该 bin（`apps/desktop-tauri/src-tauri/src/sidecar.rs:1`、`:79`、`:132`）。进程归属据此从「推测」升级为**有源码依据**，但二进制打包对应关系未逐行核对，仍标推测。
- 触发条件：多线程并发写同一 session 数据库触发 `SQLITE_BUSY`。
- 界面表现：**不影响桌面主进程界面**。sidecar 内的等待表现为 sidecar 自身请求延迟，最坏 2 s。
- 证据片段：
  ```rust
                      std::thread::sleep(sleep);
  ```
  ```rust
  const WRITE_MAX_RETRIES: u32 = 15;
  const WRITE_RETRY_MIN_MS: u64 = 20;
  const WRITE_RETRY_MAX_MS: u64 = 150;
  ```
  ```rust
      let msg = e.to_string();
      msg.contains("SQLITE_BUSY") || msg.contains("database is locked")
  ```
  ```rust
              tokio::task::block_in_place(f)
  ```
- 严重度：中（有 2 s 预算上界）。

### A6 · 退出路径上同步等 sidecar 子进程退出（主进程主线程）

- 位置：`apps/desktop-tauri/src-tauri/src/sidecar.rs:294`（`pub fn stop`）、`:297`（`try_graceful_shutdown`）、`:314`（`let deadline = started + Duration::from_secs(5)`）、`:315`（轮询条件）、`:324`（`Ok(None) => std::thread::sleep(Duration::from_millis(50))`）、`:329-336`（超时强杀）；调用链 `apps/desktop-tauri/src-tauri/src/shutdown.rs:49-51`（`state.sidecar.lock()` → `sb.stop()`）。
- 阻塞机制：`child.try_wait()` 轮询子进程是否退出；未退出就 `std::thread::sleep(50ms)` 再等，直到 5s deadline（`:314`）后强杀（`:329-336`）。整段是同步阻塞等待，占住调用线程。
- 阻塞线程归属：主进程主线程。调用点 `shutdown::cleanup` 由 `RunEvent::ExitRequested` / `RunEvent::Exit`（`lib.rs:1228-1229`）、窗口 `Destroyed`（`lib.rs:1106`）、托盘「退出」（`system_settings.rs:320`）、单实例无主窗（`single_instance.rs:196`）触发，都是事件循环回调；「Tauri 事件回调跑在主线程」属框架行为，标**推测**，旁证是这些回调内没有把工作派发到别的线程的写法。
- 触发条件：退出应用（托盘退出 / 主窗销毁 / `RunEvent::Exit` / 单实例唤起）。
- 界面表现：**会让界面看起来卡死**。点「退出」后窗口在最长 5s 内无响应（事件循环停摆）；叠加 A7 的 2s，最坏约 7s 才真正退出，体感是「点了退不出去」。
- 证据片段：
  ```rust
              let deadline = started + Duration::from_secs(5);
              while std::time::Instant::now() < deadline {
                  match child.try_wait() {
  ```
  ```rust
                      Ok(None) => std::thread::sleep(Duration::from_millis(50)),
  ```
  ```rust
      Ok(mut sb) => sb.stop(),
  ```
- 严重度：中。预算有界（5s），但发生在用户点「退出」的那一刻，体感直接。

### A7 · 退出路径上同步等文件索引守护进程退出（主进程主线程）

- 位置：`apps/desktop-tauri/src-tauri/src/shutdown.rs:56`（`wait_filesearch_gone(Duration::from_millis(2000))`）、`:100`（`fn wait_filesearch_gone`）、`:107`（`TcpStream::connect_timeout(&addr, Duration::from_millis(120))`）、`:110`（`std::thread::sleep(Duration::from_millis(120))`）、`:112`（超时日志）。
- 阻塞机制：每轮一次 120ms 回环端口连接探测 + 120ms sleep，直到 2s 预算耗尽后写一条日志返回。
- 阻塞线程归属：主进程主线程（推测，同 A6，调用点在 `cleanup` 内，`shutdown.rs:56`）。
- 触发条件：退出路径，且 `LOCALAPPDATA/hermix/filesearch/endpoint.json` 存在（`shutdown.rs:119-124` 的 `filesearch_port()` 读不到就直接返回）。
- 界面表现：**会让界面看起来卡死**。退出时最长再卡 2s，与 A6 叠加最坏约 7s。
- 证据片段：
  ```rust
      request_filesearch_stop();
      wait_filesearch_gone(Duration::from_millis(2000));
  ```
  ```rust
      let deadline = Instant::now() + budget;
      while Instant::now() < deadline {
          if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_err() {
              return;
          }
          std::thread::sleep(Duration::from_millis(120));
      }
  ```
- 严重度：中。预算有界（2s），只在退出路径。

### A8 · CDP 探针的主线程同步等待（主进程主线程，前端无调用点）

- 位置：`apps/desktop-tauri/src-tauri/src/cdp_probe.rs:32-33`（`#[tauri::command] pub fn cdp_n0_probe`，同步 fn）、`:36`（30s 超时）、`:41`（`probe_via_tauri`）、`:48`（`with_webview` 闭包）、`:63`（`rx.recv_timeout(timeout)`）、`:107-109`（内层 `result_rx.recv()`，**无超时**）；注册点 `apps/desktop-tauri/src-tauri/src/lib.rs:1163`。
- 阻塞机制：同步命令在主线程执行；`with_webview` 闭包内跑三个 CDP 调用，`call_cdp_sync` 的 `result_rx.recv()` 是无上界阻塞等待 —— 完成回调若不来，线程永久停住；外层 `recv_timeout(30s)` 只覆盖闭包排队，覆盖不到这一层。
- 阻塞线程归属：主进程主线程（同步 `fn` 命令 + `with_webview` 闭包必须落在主线程；`cdp_probe.rs:18-21` 的模块注释说明「全部 CDP 工作必须在 with_webview 闭包内（= 主线程）完成」）。
- 触发条件：仅手工在 devtools console `invoke("cdp_n0_probe")`；前端 `apps/desktop-tauri/src` grep `cdp_n0_probe` **零命中**，无产品路径。
- 界面表现：手工触发时窗口最长停 30s；内层无界等待命中时可能永久不返回。正常用户碰不到。
- 证据片段：
  ```rust
  #[tauri::command]
  pub fn cdp_n0_probe(
  ```
  ```rust
      rx.recv_timeout(timeout)
          .map_err(|_| "探针超时：with_webview 闭包未执行（事件循环未泵？）".to_string())?
  ```
  ```rust
      result_rx
          .recv()
          .map_err(|_| format!("CDP {method} 完成回调未带结果 JSON"))
  ```
- 严重度：低（无产品调用点），但内层无界等待是真隐患：真要接产品功能前必须先补超时。

### A9 · 截图会话的 30s fallback 与 ESC 2s 轮询（独立线程）

- 位置：`apps/desktop-tauri/src-tauri/src/screenshot_controller.rs:47`（`SESSION_FALLBACK_SECS = 30`）、`:300-301`（`std::thread::spawn` + `sleep(30s)`）、`:325-329`（ESC 监视线程，2s 上限、30ms 一轮）、`:333`（40ms 一轮边沿检测）。
- 阻塞机制：两处 `std::thread::sleep` 轮询，都在 `std::thread::spawn` 起的独立线程里。
- 阻塞线程归属：独立线程，不是主进程主线程，也不是 WebView2 渲染线程。
- 触发条件：截图会话启动后覆盖层 30s 未就绪；或用户按住 ESC。
- 界面表现：**不冻结界面**。覆盖层 30s 未就绪时弹「截图覆盖层 30s 未就绪，已关闭」；ESC 监视最长 2s 内按 ESC 无效。
- 证据片段：
  ```rust
  const SESSION_FALLBACK_SECS: u64 = 30;
  ```
  ```rust
          std::thread::sleep(std::time::Duration::from_secs(SESSION_FALLBACK_SECS));
  ```
  ```rust
          let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
          while screenshot::esc_pressed() && std::time::Instant::now() < deadline {
              std::thread::sleep(std::time::Duration::from_millis(30));
          }
  ```
- 严重度：低。两处都有上界，且不在 UI 线程。

---

## 三、判为不成立的条目

### C1 · 「日志尾部整文件读 + 逐行读全文件在主线程阻塞」—— 不成立

- 依据位置：`apps/desktop-tauri/src-tauri/src/commands.rs:253-257`（async 命令 + `spawn_blocking`）、`:261-263`（64 KiB 块 / 4 MiB 上限）、`:265`（阻塞实现）、`:275`（该实现内的第一次 `read_dir`）、`:302-303`（`File::open` + 流式计数）、`:325-326`（从文件尾按块回读）。
- 不成立理由：尾部读取已在 `spawn_blocking` 里执行，落阻塞线程池，不占主进程主线程；实现是从文件尾按 64 KiB 块回读、总量封顶 4 MiB，总行数用流式计数（O(size)、常数内存），既不是整读也不是逐行读全文件。原主清单把它列为「主线程整文件读」，机制与线程归属两处都错。原主清单另称「`:275` 对每个条目再 `read_dir`」，源码里 `:275` 是 `read_backend_log_tail_blocking` 的第一次 `read_dir`，与 `list_backend_log_files` 无关。
- 证据片段：
  ```rust
  #[tauri::command]
  pub async fn read_backend_log_tail(tail_lines: u64) -> Result<BackendLogTail, String> {
      tokio::task::spawn_blocking(move || read_backend_log_tail_blocking(tail_lines))
          .await
          .map_err(|e| format!("log tail task join failed: {e}"))?
  }
  ```
  ```rust
      while pos > 0 && taken < LOG_TAIL_MAX_BYTES {
          let chunk = LOG_TAIL_CHUNK.min(pos).min(LOG_TAIL_MAX_BYTES - taken);
  ```
- 归属：阻塞线程池（`spawn_blocking`），不是主进程主线程。

### C2 · 「同步目录递归枚举」中的「递归」—— 不成立

- 依据位置：`apps/desktop-tauri/src-tauri/src/commands.rs:1099`、`:1133-1134`。
- 不成立理由：`list_directory` 函数体内只有一次 `std::fs::read_dir`，没有递归进子目录。目录枚举本身成立（保留为 A4），「递归」这一机制描述不成立。

### C3 · 「session 持久化重试是文件锁竞争」—— 不成立

- 依据位置：`crates/hermix-agent/src/session_persistence/mod.rs:127-134`。
- 不成立理由：重试判定只看错误文本是否含 `SQLITE_BUSY` 或 `database is locked`，源码里没有文件锁机制。忙等重试本身成立（保留为 A5），机制描述需改正。

### C4 · G7 · `llm_retry.rs:295` 忙等 —— 不成立

- 依据位置：`crates/hermix-agent/src/llm_retry.rs:295`、`:320`、`:17`。
- 不成立理由：`:295` 落在注释块内（相邻注释实际在 `:293`），真实等待在 `:320` 的 `sleep(delay).await`，用的是 `tokio::time::sleep`（`:17` `use tokio::time::sleep;`），是异步挂起，等待期间不占任何线程。
- 证据片段：
  ```rust
                          sleep(delay).await;
  ```
- 归属：不占任何阻塞线程。

### C5 · G9 · `win_job.rs:336` 的 500 ms sleep —— 不成立

- 依据位置：`crates/hermix-core/src/win_job.rs:336`（`std::thread::sleep(Duration::from_millis(500))`）、测试模块起于 `:259-260`。
- 不成立理由：该 sleep 位于 `#[cfg(test)] mod tests` 内，是测试断言前的等待，不在生产路径。
- 证据片段：
  ```rust
      assert!(guard.is_some(), "带默认 worker 限额的附着应当成功");
  ```

### C6 · G11 · `transport.rs:1021` 与 `:1148` 的 sleep —— 不成立

- 依据位置：`crates/hermix-mcp/src/transport.rs:1021`（`wait_dead` 测试辅助函数）、`:1148`（`#[tokio::test]` 用例内）。
- 不成立理由：两处 `std::thread::sleep(Duration::from_millis(100))` 都在测试代码里，不在生产路径。

### C7 · G1 / G2 / G5 · 输入体量受限的 JSON 解析 —— 不构成阻塞

- G1 · `apps/desktop-tauri/src/app/streamEvents.ts:55`，`localizeStreamError` 内 `JSON.parse(normalized)`：把单条 provider 错误文本当 JSON 解析取 `error.message`，失败走 catch 兜底；输入体量由单条消息决定，不是大文本或大数组。归属 WebView2 渲染线程。证据：`const parsed = JSON.parse(normalized);`
- G2 · `apps/desktop-tauri/src/App.tsx:212`，`useState` 惰性初始化内 `JSON.parse(raw)`：启动时从 `localStorage` 读待发队列 `hermix.outbox.v1` 并解析后 filter，只在状态初始化阶段跑一次，不随每次渲染重跑；队列体量受待发消息条数限制。归属 WebView2 渲染线程。证据：`const parsed = JSON.parse(raw) as QueuedMessage[];`
- G5 · `apps/desktop-tauri/src/components/bubbles/ToolOutputPreviews.tsx:32`，`ScreenshotPreview` 的 `useMemo` 内 `JSON.parse(output)`：把工具输出字符串当 JSON 解析提取 `width/height/bytes/monitor_name/saved_to`，失败返回 null。归属 WebView2 渲染线程；该字符串的长度上限本轮未核到。证据：`const v = JSON.parse(output);`

### C8 · G3 · Markdown 按行切分 —— 成立但只是同线程轻度重活

- 位置：`apps/desktop-tauri/src/components/MarkdownBody.tsx:507`，`transformMath` 内 `md.split("\n")`；同文件 `:858` 的 `normalizeWindowsImagePaths` 是第二处 `md.split("\n")`，同型、同样在渲染线程。
- 机制：整段 markdown 按换行切成行数组再逐行判断公式块并回写，同步执行，代价随消息长度线性增长；单次线性扫描，无嵌套循环。
- 归属：WebView2 渲染线程。
- 界面表现：**只是占用渲染线程**（单帧变长），不是「无响应」。

### C9 · G4 / G12 · 代码高亮 —— 同一段代码，只算一处

- 位置：`apps/desktop-tauri/src/lib/codeHighlight.ts:383`（`export function highlightToReact`），调用点 `apps/desktop-tauri/src/components/MarkdownBody.tsx:269-270`（`useMemo` 内同步调用 `highlightToReact(code, language, isDark)`），另有行内 code 调用点 `MarkdownBody.tsx:1006`。
- 机制：渲染线程同步跑 refractor 分词并经 `hast-util-to-jsx-runtime` 转 React 节点，失败降级返回原文；`useMemo` 依赖 `[code, language, isDark, langVersion]`，同一段代码不重复计算；长尾语言未加载时先返回纯文本，包装到位后由 `langVersion` 触发一次重算。
- 归属：WebView2 渲染线程，不是主进程主线程。
- 界面表现：**只是占用渲染线程**（长代码块渲染时一帧变长）。耗时只有源码注释自述值（`codeHighlight.ts:379-381`，**源码注释自述，未实测**）。
- 说明：G4 与 G12 指同一段代码，补漏清单里重复计数，本版只算一处。
- 证据片段：
  ```tsx
    const highlighted = useMemo(
      () => highlightToReact(code, language, isDark),
      [code, language, isDark, langVersion],
    );
  ```

### C10 · G10 · 文件锁退避重试 —— 成立但线程归属未定位

- 位置：`crates/hermix-cron/src/file_lock.rs:156`（`acquire`（`:86`）内的退避重试）、间隔常量 `RETRY_INTERVAL = 100 ms`（`:56`）、timeout 上界判断在 `:136`、`:146-152`。
- 机制：文件锁被别的进程占住时每 100 ms 退避重试一次，直到调用方给的 `timeout` 到期返回 `LockError::Timeout`；有上界，不是无限忙等。
- 归属：**推测** —— cron 调度线程。本轮未定位 `hermix-cron` 的 `file_lock::acquire` 调用点，未确认它跑在桌面主进程还是 sidecar、是否在 UI 可感知线程上。
- 界面表现：未确定。
- 证据片段：
  ```rust
                      std::thread::sleep(RETRY_INTERVAL);
  ```

---

## 四、量级未实测项

1. **所有毫秒耗时未实测**。本轮禁止启动应用、跑构建与测试。文中出现的耗时只有源码注释自述值：`webview2_probe.rs:111-112` 的「~5ms」、`codeHighlight.ts:379-381` 的「~8 ms for 1000 lines of Python」，两处均已标「源码注释自述，未实测」。
2. **A1 / A3 / A4 的实际耗时未测**：缺「在目标文件规模下调用一次耗时多少毫秒」的实测数据。缺：可复现的基准脚本与运行结果。
3. **A3 的目录条目数分布未知**：缺后端日志目录条目数与所在存储介质的现场数据，因而无法给出「多大盘子会卡多久」。
4. **A5 的 2 s 是代码常量上限，不是实测分布**：缺并发写同一 session 数据库时 `SQLITE_BUSY` 的实际触发频率与重试次数分布。
5. **A6 / A7 的等待是预算上界，不是实测分布**：5s 与 2s 均为 `deadline` 常量；实际等待时长取决于 sidecar 与 filesearch 的退出速度，本轮未测。
6. **A8 的内层无界等待触发概率未验证**：`result_rx.recv()` 无超时是代码事实，但是否真会命中未验证。
7. **C10 的调用点未定位**：缺 `hermix-cron` 中 `file_lock::acquire` 的调用位置与所在进程/线程证据。
8. **框架行为均为推测**：Tauri v2 的 async command 调度到 async runtime、同步 command 在主线程执行、`setup` 在事件循环启动前于主线程执行、事件回调跑在主线程，均无仓库内 Tauri 源码可 pin；仓库内旁证只有 `commands.rs:944`、`:952` 与 `session_persistence/mod.rs:72` 三处注释。
9. **未覆盖的相邻代码**：`apps/desktop-tauri/src-tauri/src/webview2_probe.rs:152` 另有第二处 `Command::new("reg")`，本轮未核对上下文，不计入条目；`crates/hermix-mcp`、`hermix-environments`、`hermix-tools` 等 crate 的同步 IO 与轮询细节未逐条核对；前端附录的 F-1~F-4 属等待时间维度，不在本文件口径内，未合并。

---

## 五、按用户体感排序（谁最容易被误认为卡死）

排序依据：是否落在主进程主线程、是否在用户可见的交互过程中触发、是否让整窗事件循环停摆、用户能否看到「正在等待」的提示。

1. **A6 + A7 退出链（`sidecar.rs:314`、`shutdown.rs:56`）** —— 落在主进程主线程，点「退出」后窗口最长停约 7s，是最像「程序卡住」的一段。
2. **A3 `list_backend_log_files`（`commands.rs:194-200`）** —— 唯一在主进程主线程、且由用户操作直接触发的同步 IO。打开日志面板时窗口整体变灰。
3. **C8 / C9 渲染线程重活（`MarkdownBody.tsx:507`、`codeHighlight.ts:383`）** —— 长消息或长代码块渲染时整窗掉帧、输入框迟钝，用户常说「界面卡住了」，但没有阻塞点，只是单帧变长。
4. **A1 整文件读 + base64（`commands.rs:723`、`:812`）** —— 打开大产物文件时其它 IPC 排队，表现为「操作没反应一会儿」，但窗口仍可拖动。
5. **A4 单层目录枚举（`commands.rs:1133`）** —— 在网络盘或 OneDrive 目录里打开文件选择器时的等待，最容易被当成网络问题。
6. **A2 `reg.exe` 探测（`webview2_probe.rs:125`）** —— 启动阶段一次性等待，用户只看到启动慢，不会有「卡死」体感。
7. **A5 `SQLITE_BUSY` 重试（`sessions.rs:259`）** —— 在 sidecar 内，桌面界面无感；只有 sidecar 自身请求延迟最坏 2 s。
8. **A8 CDP 探针（`cdp_probe.rs:107`）** —— 无产品调用点，普通用户碰不到。
9. **C10 文件锁退避（`file_lock.rs:156`）** —— 归属未定位，体感未知。
