# 阻塞型卡死点最终清单（hermix-desktop）

合并来源：`docs/blocking-hang-audit.md`（主清单）、`docs/blocking-hang-audit-review.md`（第 1 轮复核）、`docs/blocking-hang-audit-gaps.md`（补漏复核）。本文件按复核判定改写，不新增源码位置，不自行下新结论。

## 口径说明

- **阻塞线程归属**分三类：主进程主线程 / tokio worker（含 sidecar 进程内 worker 与 `spawn_blocking` 阻塞线程池）/ WebView2 渲染线程。
- **标题后的复核状态**四选一：已确认存在 / 已确认但归属修正 / 复核未发现 / 规模未实测。
- 所有毫秒耗时未实测（本轮未启动应用、未跑测试与构建）。文中出现的耗时只有源码注释自述值，标注为「源码注释自述，未实测」。
- 超时数值的口径不在本文件内，详见等待维度清单。

---

## 一、主进程主线程上的真阻塞（2 条）

### A1. 同步命令 `list_backend_log_files` 在主进程主线程枚举日志目录 —— 已确认但归属修正

- **位置**：`apps/desktop-tauri/src-tauri/src/commands.rs:194-200`（同步命令签名 + `read_dir`），枚举后逐条只取 `metadata()`（`:206-212`）。
- **阻塞机制**：同步 `fn` 命令体内直接调用 `std::fs::read_dir(&dir)`，同步枚举单个日志目录下的日志文件，非递归。`commands.rs:275` 的 `read_dir` 属于 `read_backend_log_tail_blocking`，与本命令无关。
- **阻塞线程归属**：主进程主线程。依据：命令是同步 `fn`；「Tauri v2 同步 command 在主线程执行」属框架行为，仓库内无 Tauri 源码可核对，标**推测**。
- **触发条件**：用户打开「日志」面板列出后端日志文件。
- **界面影响**：枚举期间主线程被占住，日志面板内容出现延迟；单目录、非递归，枚举量级取决于该目录下的文件数。
- **源码原文片段**：
  ```rust
  #[tauri::command]
  pub fn list_backend_log_files() -> Result<LogFileInfo, String> {
      let dir = log_dir();
      let mut files: Vec<LogFileEntry> = Vec::new();
      let mut total_bytes: u64 = 0;
      if dir.is_dir() {
          let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
  ```

### A2. 同步子进程调用 `reg.exe` —— 已确认但归属修正

- **位置**：`apps/desktop-tauri/src-tauri/src/webview2_probe.rs:90-98`（`probe()` 与 `CACHED_STATUS` 缓存）、`:108`（两个注册表视图的循环）、`:125`（`Command::new("reg")`）；唯一调用点 `apps/desktop-tauri/src-tauri/src/lib.rs:911`。
- **阻塞机制**：`Command::new("reg").args(["query", &key, "/v", "pv"]).output()` 同步等待子进程退出；`:108` 的循环对 WOW6432Node 与原生两个视图各跑一次，最坏两次子进程启动。
- **阻塞线程归属**：主进程主线程（应用 `setup` 阶段，Tauri 事件循环启动前）。`probe()` 是同步 `fn`，调用点在 `setup` 闭包内；「setup 在主线程、事件循环启动前执行」属框架行为，标**推测**。
- **触发条件**：仅 `CACHED_STATUS` 缓存未命中时发生（首次探测）；缓存命中直接返回，不重复付这两次子进程成本。
- **界面影响**：事件循环启动前的一次性等待，窗口尚未进入可交互状态。源码注释自述单次约 5 ms（`webview2_probe.rs:111-112`，**源码注释自述，未实测**），进程生命周期内只发生一次。
- **源码原文片段**：
  ```rust
  pub fn probe() -> WebView2Status {
      let cached = CACHED_STATUS.load(Ordering::Acquire);
      if cached != UNPROBED {
          return WebView2Status::from_u8(cached);
      }

      let detected = probe_inner();
      CACHED_STATUS.store(detected.as_u8(), Ordering::Release);
      detected
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

---

## 二、tokio worker / 阻塞线程池上的阻塞（6 条）

### B1. `read_artifact_file` 整文件读 + base64 编码 —— 已确认但归属修正

- **位置**：`apps/desktop-tauri/src-tauri/src/commands.rs:671`（命令签名，`pub async fn`）、`:675`（2 MiB 上限）、`:723`（整文件读）、`:742-743`（base64 编码）。
- **阻塞机制**：`std::fs::read` 整文件读入内存，再 `STANDARD.encode(&bytes)` 生成约 1.33 倍体积字符串。命令是 `pub async fn`，函数体在 async runtime 的 worker 上执行，未交给 `spawn_blocking`；读文件与 base64 编码期间该 worker 被占住。
- **阻塞线程归属**：tokio worker。「async command 调度到 async runtime」属框架行为，标**推测**；仓库内旁证：`commands.rs:944` 注释「spawn_blocking 把它挪到阻塞线程池」、`:952` 实际调用 `spawn_blocking`；`crates/hermix-agent/src/session_persistence/mod.rs:71-72` 注释「Called inline from an async handler that holds a worker for seconds」。
- **触发条件**：打开产物文件预览，文件 ≤ 2 MiB 上限。
- **界面影响**：占住一个 worker，窗口标题栏拖动不会变灰；受影响的是同一 runtime 上其它命令排队与延迟。「窗口整体无响应」对本条不成立。
- **源码原文片段**：
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

### B2. `read_artifact_file_bytes` 整文件读 + base64 编码 —— 已确认但归属修正

- **位置**：`apps/desktop-tauri/src-tauri/src/commands.rs:764`（`pub async fn read_artifact_file_bytes`）、`:812`（整文件读）、`:813-814`（base64 编码）；上限 10 MiB。
- **阻塞机制**：同 B1，`std::fs::read` 整读后再做 base64 编码，命令为 `pub async fn`，未交给 `spawn_blocking`。
- **阻塞线程归属**：tokio worker（同 B1 的口径与旁证）。
- **触发条件**：走 bytes 通道读二进制文件，文件 ≤ 10 MiB 上限。
- **界面影响**：同 B1 —— 占住一个 worker，同 runtime 上其它命令排队与延迟。
- **源码原文片段**：
  ```rust
      let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;
      use base64::Engine;
      let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
  ```

### B3. `list_directory` 单层目录枚举 —— 已确认但归属修正

- **位置**：`apps/desktop-tauri/src-tauri/src/commands.rs:1098-1099`（`pub async fn list_directory`）、`:1133-1134`（`std::fs::read_dir`）。
- **阻塞机制**：`std::fs::read_dir(&canonical)` 同步枚举一层目录项，函数体内只有一次 `read_dir`，未递归进子目录；命令为 `pub async fn`，枚举是同步阻塞调用，未交给 `spawn_blocking`。
- **阻塞线程归属**：tokio worker（同 B1 的口径与旁证）。
- **触发条件**：文件选择器/产物浏览打开含大量文件的目录，尤其网络盘或 OneDrive 同步目录。
- **界面影响**：同 runtime 上其它 IPC 排队、延迟；窗口不一定整体无响应。
- **源码原文片段**：
  ```rust
      let entries = std::fs::read_dir(&canonical)
          .map_err(|e| format!("cannot read directory '{}': {e}", canonical.display()))?;
  ```

### B4. `read_backend_log_tail` 在阻塞线程池内回读日志尾部 —— 已确认存在（不占主线程）

- **位置**：`apps/desktop-tauri/src-tauri/src/commands.rs:253-257`（async 命令 + `spawn_blocking`）、`:262-263`（64 KiB 块 / 4 MiB 上限）、`:265`（阻塞实现）、`:302-303`（`File::open` + 流式计数）、`:325-330`（从文件尾按块回读）。
- **阻塞机制**：`spawn_blocking` 内的同步实现：从文件尾按 64 KiB 块回读，累计封顶 4 MiB；总行数用流式计数（O(size)、常数内存），既不是整读也不是逐行读全文件。
- **阻塞线程归属**：阻塞线程池（`tokio::task::spawn_blocking`），不占主进程主线程，也不长期占住 async runtime worker。
- **触发条件**：打开日志面板并请求尾部内容，文件大于 4 MiB 时触发多次回读。
- **界面影响**：日志尾部内容延迟返回，单次回读量封顶 4 MiB。
- **源码原文片段**：
  ```rust
  #[tauri::command]
  pub async fn read_backend_log_tail(tail_lines: u64) -> Result<BackendLogTail, String> {
      tokio::task::spawn_blocking(move || read_backend_log_tail_blocking(tail_lines))
          .await
          .map_err(|e| format!("log tail task join failed: {e}"))?
  }
  ```
  ```rust
      // 流式统计总行数：O(size) 但只占常数内存，在 blocking 线程不碍 UI。
      let total_lines = {
          let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
  ```
  ```rust
      while pos > 0 && taken < LOG_TAIL_MAX_BYTES {
          let chunk = LOG_TAIL_CHUNK.min(pos).min(LOG_TAIL_MAX_BYTES - taken);
  ```

### B5. SQLite `SQLITE_BUSY` 应用层忙等重试 —— 已确认但归属修正

- **位置**：`crates/hermix-agent/src/session_persistence/sessions.rs:259`（`std::thread::sleep(sleep);`）、`crates/hermix-agent/src/session_persistence/mod.rs:119-121`（重试次数与间隔常量）、`:125`（2 s 总预算常量）、`:127-134`（`is_busy_error`）、`:81-96`（`run_blocking` / `block_in_place`）、`crates/hermix-agent/src/local_agent_service/mod.rs:865-871`（调用点）。
- **阻塞机制**：SQLite 写冲突（`SQLITE_BUSY` / `database is locked`）触发应用层忙等重试，`std::thread::sleep(sleep)` 后重写，最坏受 `WRITE_RETRY_BUDGET` 2 s 总预算约束（`WRITE_MAX_RETRIES = 15`、`WRITE_RETRY_MIN_MS = 20`、`WRITE_RETRY_MAX_MS = 150`）。源码里没有文件锁，只有 SQLite busy 判定。
- **阻塞线程归属**：sidecar 进程内的 tokio worker。调用点包在 `run_blocking` 内，MultiThread runtime 上走 `tokio::task::block_in_place`（`mod.rs:92`）把 worker 交回阻塞池，单线程 runtime 直接同步执行（`:94`）。主进程侧无此调用链：`apps/desktop-tauri/src-tauri/Cargo.toml` 依赖列表里没有 `hermix-agent`，`hermix-agent` 只被 `crates/hermix-cron/Cargo.toml:13`、`crates/hermix-runtime/Cargo.toml:11`、`crates/hermix-server/Cargo.toml:26` 依赖。进程归属标**推测**（sidecar 可执行文件与 `hermix-server` 的构建对应关系未逐行核对）。
- **触发条件**：多线程并发写同一 session 数据库触发 `SQLITE_BUSY`。
- **界面影响**：桌面主进程不受影响；sidecar 内的等待表现为 sidecar 自身请求延迟，最坏 2 s。
- **源码原文片段**：
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

### B6. 文件锁 100 ms 退避重试 —— 已确认存在，线程归属标推测

- **位置**：`crates/hermix-cron/src/file_lock.rs:156`（`acquire` 内的退避重试，`acquire` 签名在 `:86`）、退避间隔常量 `crates/hermix-cron/src/file_lock.rs:56`、timeout 上界判断在 `:136`、`:146-152`。
- **阻塞机制**：文件锁被别的进程占住时每 100 ms 退避重试一次，直到调用方给的 `timeout` 到期返回 `LockError::Timeout`；等待有上界，不是无限忙等。
- **阻塞线程归属**：**推测** —— cron 调度线程。本轮未定位 `hermix-cron` 的 `acquire` 调用点，未确认它跑在桌面主进程还是 sidecar、是否在 UI 可感知线程上。缺什么证据：`file_lock::acquire` 的调用点位置与所在进程/线程。
- **触发条件**：多个进程争抢同一把 cron 文件锁。
- **界面影响**：未确定（归属未核实前无法判定是否影响 UI 线程）。
- **源码原文片段**：
  ```rust
                  Err(e) if start.elapsed() >= timeout => {
                      return Err(LockError::Timeout {
                          path: self.path.clone(),
                          waited: start.elapsed(),
                          source: e,
                      });
                  }
                  _ => {
                      // Another process holds the lock.
                      // Back off and retry.
                      std::thread::sleep(RETRY_INTERVAL);
                  }
  ```
  ```rust
  const RETRY_INTERVAL: Duration = Duration::from_millis(100);
  ```

---

## 三、WebView2 渲染线程上的阻塞（5 条）

### C1. 同步语法高亮（refractor 分词 + 转 React 节点） —— 已确认存在（G4 与 G12 为同一处代码，去重后只算一条）

- **位置（调用点）**：`apps/desktop-tauri/src/components/MarkdownBody.tsx:269-270`（`useMemo` 内同步调用）。
- **位置（被调函数）**：`apps/desktop-tauri/src/lib/codeHighlight.ts:383`（`export function highlightToReact`）。
- **调用链**：`MarkdownBody` 渲染 → `useMemo` 回调（`MarkdownBody.tsx:269-270`）→ `highlightToReact(code, language, isDark)`（`codeHighlight.ts:383`）→ refractor 同步分词 → `hast-util-to-jsx-runtime` 转 React 节点。
- **阻塞机制**：渲染线程同步跑分词与节点转换。`useMemo` 依赖 `[code, language, isDark, langVersion]`，同一段代码不重复计算；长尾语言未加载时先返回纯文本，包装到位后由 `langVersion` 触发一次重算。
- **阻塞线程归属**：WebView2 渲染线程（前端 JS 线程），不是主进程主线程。
- **触发条件**：消息里出现代码块；代码越长，分词耗时越大。
- **界面影响**：长代码块渲染时一帧变长。源码注释自述约 8 ms / 1000 行 Python（`codeHighlight.ts:379-381`，**源码注释自述，未实测**）。
- **源码原文片段**：
  ```tsx
  const highlighted = useMemo(
    () => highlightToReact(code, language, isDark),
    [code, language, isDark, langVersion],
  );
  ```
  ```ts
  // Tokenizes `source` once and returns a React node tree. Measured
  // at ~8 ms for 1000 lines of Python. Returns the raw string when
  // the language is missing or tokenize fails (so a highlight failure
  // degrades to plain text instead of an empty bubble).
  export function highlightToReact(
  ```

### C2. `transformMath` 整段 markdown 按行切分 —— 已确认存在（轻度，规模未实测）

- **位置**：`apps/desktop-tauri/src/components/MarkdownBody.tsx:507`，函数 `transformMath`。
- **阻塞机制**：把整段 markdown 按 `\n` 切成行数组，再逐行判断公式块并回写，同步执行，代价随消息长度线性增长；单次线性扫描，无嵌套循环。
- **阻塞线程归属**：WebView2 渲染线程。
- **触发条件**：渲染含多行内容的 markdown 消息。
- **界面影响**：长消息渲染时一帧变长。
- **源码原文片段**：
  ```tsx
  function transformMath(md: string): string {
    const lines = md.split("\n");
    const out: string[] = [];
    let inFence = false;
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i]!;
  ```
- **缺什么证据**：仓库内未找到 markdown 消息体的长度截断上限，无法给出触发阈值；毫秒耗时未实测。

### C3. 截图工具输出 `JSON.parse` —— 已确认存在（规模未知）

- **位置**：`apps/desktop-tauri/src/components/bubbles/ToolOutputPreviews.tsx:32`，组件 `ScreenshotPreview` 的 `useMemo` 内。
- **阻塞机制**：把工具输出字符串当 JSON 解析，提取 `width/height/bytes/monitor_name/saved_to`；解析失败返回 null（不抛错）。
- **阻塞线程归属**：WebView2 渲染线程。
- **触发条件**：渲染截图类工具输出气泡。
- **界面影响**：取决于工具输出长度，该长度上限未核到。
- **源码原文片段**：
  ```tsx
  function ScreenshotPreview({ output }: { output?: string }) {
    const parsed = useMemo<ScreenshotParsed | null>(() => {
      if (!output) return null;
      try {
        const v = JSON.parse(output);
        if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  ```
- **缺什么证据**：未找到工具输出字符串的长度上限/截断实现位置。

### C4. 流事件错误文本 `JSON.parse` —— 已确认存在（不构成阻塞）

- **位置**：`apps/desktop-tauri/src/app/streamEvents.ts:55`，函数 `localizeStreamError`。
- **阻塞机制**：把一条 provider 错误文本当 JSON 解析，取 `error.message` 等字段；解析失败走 `catch` 兜底保持原文。输入是单条 provider 错误消息，不是大文本或大数组。
- **阻塞线程归属**：WebView2 渲染线程。
- **触发条件**：收到带错误文本的流事件。
- **界面影响**：无卡顿。
- **源码原文片段**：
  ```ts
    let extracted = normalized;
    try {
      const parsed = JSON.parse(normalized);
      const msg =
        parsed?.error?.message ??
        parsed?.error?.error?.message ??
        parsed?.message ??
        null;
  ```

### C5. 待发队列 `localStorage` 解析 —— 已确认存在（不构成阻塞）

- **位置**：`apps/desktop-tauri/src/App.tsx:212`（`useState` 惰性初始化函数内）。
- **阻塞机制**：启动时从 `localStorage` 读 `hermix.outbox.v1` 待发队列并 `JSON.parse`，解析结果再 `filter` 一遍。发生在 React 状态初始化阶段，只跑一次，不在每次渲染里重跑。
- **阻塞线程归属**：WebView2 渲染线程。
- **触发条件**：应用启动时存在待发队列。
- **界面影响**：无卡顿；队列体量受待发消息条数限制。
- **源码原文片段**：
  ```ts
  const [messageQueue, setMessageQueue] = useState<QueuedMessage[]>(() => {
    // 待发队列持久化：重启后仍能看到上次没发出去的待发队列。
    // 带 File 句柄的条目（图片 / 文档）无法序列化，写入时会被过滤掉。
    try {
      const raw = storage.getItem(OUTBOX_STORAGE_KEY);
      if (!raw) return [];
      const parsed = JSON.parse(raw) as QueuedMessage[];
      return Array.isArray(parsed)
        ? parsed.filter((m) => m && typeof m.text === "string" && !!m.id)
        : [];
    } catch {
      return [];
    }
  });
  ```

---

## 四、复核未发现（3 条，不计入成立条数）

### G7. `crates/hermix-agent/src/llm_retry.rs:295` —— 复核未发现（引用行号错位）

- **判定**：未发现阻塞型忙等。`:295` 落在注释块内，真实等待在 `crates/hermix-agent/src/llm_retry.rs:320`，是 `sleep(delay).await`（`tokio::time::sleep`，`llm_retry.rs:17` 的 `use tokio::time::sleep;`），异步挂起，等待期间不占线程。
- **阻塞线程归属**：不占任何阻塞线程。
- **界面影响**：无。
- **源码原文片段**：
  ```rust
                          // Provider-suggested wait (Retry-After header echoed
                          // into the error, or prose) wins over local backoff —
                          // the provider knows its own load. Absent/over-cap →
                          // local exponential backoff as before.
                          let delay = parse_retry_after_ms(&err_str)
  ```
  ```rust
                          sleep(delay).await;
  ```

### G9. `crates/hermix-core/src/win_job.rs:336` —— 复核未发现（位于测试模块）

- **判定**：未发现生产阻塞点。该行是 `#[cfg(test)] mod tests`（模块起于 `win_job.rs:259-260`）里的 `std::thread::sleep(Duration::from_millis(500))`，属于测试断言前的等待，不在运行路径。
- **阻塞线程归属**：不适用。
- **界面影响**：无。
- **源码原文片段**：
  ```rust
      assert!(guard.is_some(), "带默认 worker 限额的附着应当成功");
      std::thread::sleep(std::time::Duration::from_millis(500));
      assert!(
          is_process_alive(child.id()).unwrap_or(false),
  ```

### G11. `crates/hermix-mcp/src/transport.rs:1021` / `:1148` —— 复核未发现（位于测试代码）

- **判定**：未发现生产阻塞点。两处 `std::thread::sleep(Duration::from_millis(100))` 都在测试代码里 —— `:1021` 是等子进程退出的测试辅助函数 `wait_dead`，`:1148` 是 `#[tokio::test]` 用例内等孙进程出现的前置条件等待。
- **阻塞线程归属**：不适用。
- **界面影响**：无。
- **源码原文片段**：
  ```rust
      fn wait_dead(pids: &[u32], timeout: Duration) -> Vec<u32> {
          let start = Instant::now();
          loop {
              let alive: Vec<u32> = pids.iter().copied().filter(|p| !is_dead(*p)).collect();
              if alive.is_empty() || start.elapsed() > timeout {
                  return alive;
              }
              std::thread::sleep(Duration::from_millis(100));
          }
      }
  ```
  ```rust
          for _ in 0..100 {
              let kids = proc::child_pids(root);
              if !kids.is_empty() {
                  tree = kids.into_iter().chain(std::iter::once(root)).collect();
                  break;
              }
              std::thread::sleep(Duration::from_millis(100));
          }
  ```

---

## 五、未经复核的补充

本轮未新增任何源码位置，也未对复核范围外的代码下新结论。本节仅承接主清单「未实测与缺证」第 6 条点名的位置，状态为未复核，不计入成立条数：

- `apps/desktop-tauri/src-tauri/src/webview2_probe.rs:152`：主清单记录此处另有第二处 `Command::new("reg")`，未核对上下文，未判定是否为阻塞点。

---

## 六、汇总

合并前输入：主清单「已确认发现」5 条（复核后拆为 6 条含 3a/3b）+ 补漏检索面 12 条（G1–G12）。

合并去重：
- G4 与 G12 是同一处代码（`MarkdownBody.tsx:269-270` → `codeHighlight.ts:383`），合并为 1 条（成立计数 -1）。
- G6 的 4 处位置（`commands.rs:200`、`:723`、`:812`、`:1133`）与复核发现 1、3a、4 指向同一批源码位置，按位置拆分计入 B1/B2/B3/A1，不重复计条。
- G8 与复核发现 5 是同一条（`session_persistence/sessions.rs:259`），合并为 B5。
- G7、G9、G11 复核判定为未发现，不计入成立条数。

**成立条数：13 条**，分三类：

| 类别 | 条数 | 条目 |
|---|---|---|
| 主进程主线程上的真阻塞 | 2 | A1（`commands.rs:200`）、A2（`webview2_probe.rs` 的 `reg.exe`） |
| tokio worker / 阻塞线程池 / WebView2 渲染线程上的阻塞 | 11 | B1、B2、B3（tokio worker）；B4（`spawn_blocking` 阻塞线程池）；B5（sidecar 进程内 tokio worker）；B6（线程归属标推测）；C1、C2、C3、C4、C5（WebView2 渲染线程） |
| 复核未发现 | 3 | G7、G9、G11（不计入成立条数） |

**去重情况**：合并去重 1 处（G4 与 G12 归并为 C1）；成立条目 13 条 = 检索面与主清单去重合并后的结果。

---

## 七、未实测与缺证

1. **所有毫秒耗时未实测**：本轮未启动应用、未跑测试与构建。耗时只有源码注释自述值 —— `webview2_probe.rs:111-112` 的「~5ms」与 `codeHighlight.ts:379-381` 的「~8 ms for 1000 lines of Python」。
2. **C2 / C1 / C3 的体量上限未找到**：仓库内未找到 markdown 消息、代码块、工具输出字符串的截断上限，无法给出触发阈值。缺：截断代码位置；工具输出写回路径的实现位置。
3. **B6 调用链未定位**：`hermix-cron` 的 `file_lock::acquire` 调用点与所在进程/线程未确认，线程归属标推测。
4. **B5 进程归属为推测**：sidecar 二进制与 `hermix-server` crate 的构建对应关系未逐行核对。
5. **框架行为均为推测**：Tauri v2 的 async command 调度到 async runtime、同步 command 在主线程执行、`setup` 在事件循环启动前于主线程执行，均无仓库内 Tauri 源码可 pin；仓库内旁证只有 `commands.rs:944`、`:952` 与 `session_persistence/mod.rs:71-72` 三处注释。
6. **未覆盖的相邻代码**：`webview2_probe.rs:152` 的第二处 `Command::new("reg")` 未核对（见第五节）；`crates/hermix-mcp`、`hermix-environments`、`hermix-tools` 等 crate 的同步 IO 与轮询细节未逐条核对。
7. **检索面 c（同步 IPC）**：主清单记 0 命中，复核未新增核对，不作改动。Tauri v2 无 `ipcRenderer.sendSync` 同构 API，已核查 `apps/desktop-tauri/src` 与 `src-tauri/src` 未见同步阻塞式 invoke 封装。
