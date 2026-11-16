# 阻塞型卡死点清单 · 复核结论（逐条判定）

复核对象：`docs/blocking-hang-audit.md`（主清单，只读，本次未修改）
复核人：验收员（技术结论复核）
复核方式：逐条回源 `read_file` 读函数体与调用点，只认函数体/调用点的实际代码；注释只作旁证。
复核范围：主清单「已确认发现」5 条。主清单检索面 d/e/f 与其余 sleep 命中的逐条结果见 `docs/blocking-hang-audit-gaps.md`。
本文件未引用 `docs/wait-timeout-audit.md` / `docs/wait-timeout-audit-review.md` 的任何说法。

## 汇总

- 条目数：5（对应主清单「已确认发现」1/2/3/4/5，逐条一一对应）
- 判定分布：**通过 0 条 / 需修正 5 条 / 不成立 0 条**
- 判定三态定义：通过 = 机制、线程归属、引用行号均可原样保留；需修正 = 阻塞点真实存在，但线程归属 / 机制描述 / 引用位置有事实偏差，需按本文件改写；不成立 = 源码中不存在该阻塞点。
- 五条共有的偏差：主清单把 5 条全部归为「主进程主线程」。回源后实际归属是 3 条落在 tokio worker（async 命令体）、1 条落在 sidecar 进程内的 worker、1 条（WebView2 探测）才在主进程主线程且每次进程只跑一次。
- 机制层面的两处硬错误：发现 3 的「整文件读 + 逐行读尾部」已经走 `spawn_blocking`，不在主线程，也不是整读；发现 5 的重试对象是 SQLite `SQLITE_BUSY`，不是文件锁。

| # | 主清单标题 | 判定 | 回源后的阻塞线程归属 | 关键偏差 |
|---|---|---|---|---|
| 1 | 同步整文件读 + base64 编码 | 需修正 | tokio worker（async 命令体，无 `spawn_blocking`） | 归属错；命令是 `pub async fn` |
| 2 | 同步子进程调用 reg.exe | 需修正 | 主进程主线程（`setup` 阶段，一次性） | 归属对；严重度与耗时描述与源码注释矛盾 |
| 3 | 同步日志目录枚举 + 整文件读 | 需修正 | 拆成两处：`list_backend_log_files` 在主进程主线程；`read_backend_log_tail` 在阻塞线程池 | 尾部读取部分不成立 |
| 4 | 同步目录递归枚举 | 需修正 | tokio worker（async 命令体） | 归属错；无递归，标题的「递归」不成立 |
| 5 | 忙等轮询循环：session 持久化重试 | 需修正 | sidecar 进程内的 tokio worker（`run_blocking` → `block_in_place`） | 不是文件锁；有 2 s 总预算；主进程无调用链 |

---

## 发现 1 · 同步整文件读 + base64 编码 —— 需修正

**判定理由**：同步读文件与全量 base64 属实，但 `read_artifact_file` 是 `pub async fn`，函数体由 Tauri 的 async runtime 调度，占住的是 worker 线程而非主进程主线程；主清单的「在主进程主线程执行」不成立。

**回源后的阻塞线程归属**：tokio worker（Tauri async runtime 的 worker）。

**归属依据（含推测标注）**：Tauri v2 把 async command 调度到 async runtime 属于框架行为，本仓库内没有 Tauri 源码可核对，标**推测**；仓库内旁证两条 —— `apps/desktop-tauri/src-tauri/src/commands.rs:944-952` 的注释写明 async 命令里的同步重活要靠 `spawn_blocking` 才挪出 worker，`crates/hermix-agent/src/session_persistence/mod.rs:71-72` 写明「Called inline from an async handler that holds a worker for seconds」。两条旁证都把 async 命令体定位在 worker 上。

**应引用的源码位置**：
- `apps/desktop-tauri/src-tauri/src/commands.rs:671`（命令签名，`pub async fn`）
- `apps/desktop-tauri/src-tauri/src/commands.rs:675`（2 MiB 上限）
- `apps/desktop-tauri/src-tauri/src/commands.rs:723`（整文件读）
- `apps/desktop-tauri/src-tauri/src/commands.rs:742-743`（base64 编码）
- 同型第二处：`apps/desktop-tauri/src-tauri/src/commands.rs:764`（`pub async fn read_artifact_file_bytes`，10 MiB 上限）、`:812`（读）、`:813-814`（base64）

**应贴的代码片段（照抄源码）**：

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

```rust
    let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
```

**修正要求**：
1. 线程归属由「主进程主线程」改为「tokio worker（async 命令体未交给 `spawn_blocking`）」，并保留推测标注与上面的两条仓库内旁证。
2. 界面表现改写：占住一个 worker，不会让窗口标题栏拖动变灰；被影响的是同一 runtime 上其它命令排队与延迟。窗口整体无响应的结论对本条不成立。
3. 补上规模边界：2 MiB（`read_artifact_file`）与 10 MiB（`read_artifact_file_bytes`）两个上限写在引用行里。

---

## 发现 2 · 同步子进程调用 reg.exe —— 需修正

**判定理由**：同步子进程等待属实，线程归属也确实是主进程主线程；但主清单写「启动阶段窗口空白/无响应数秒」与源码自述冲突 —— `webview2_probe.rs:111-112` 明写子进程启动成本约 5 ms、每进程生命周期只付一次。

**回源后的阻塞线程归属**：主进程主线程（应用 `setup` 阶段，Tauri 事件循环启动前）。

**归属依据（含推测标注）**：`probe()` 是同步 `fn`，调用点 `apps/desktop-tauri/src-tauri/src/lib.rs:911` 位于 Tauri `setup` 闭包内；「setup 在主线程、事件循环启动前执行」属于 Tauri 框架行为，标**推测**（仓库内无 Tauri 源码可 pin）。

**应引用的源码位置**：
- `apps/desktop-tauri/src-tauri/src/webview2_probe.rs:90-98`（`probe()` 与 `CACHED_STATUS` 缓存）
- `apps/desktop-tauri/src-tauri/src/webview2_probe.rs:108`（两个注册表视图的循环）
- `apps/desktop-tauri/src-tauri/src/webview2_probe.rs:125`（`Command::new("reg")`）
- `apps/desktop-tauri/src-tauri/src/lib.rs:911`（唯一调用点）

**应贴的代码片段（照抄源码）**：

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

**修正要求**：
1. 循环行号从 106 改为 108（`webview2_probe.rs:108`）。
2. 严重度描述改为「启动阶段主线程最多两次同步 `reg.exe` 查询，源码自述单次约 5 ms，进程生命周期内只发生一次（`CACHED_STATUS` 缓存，`webview2_probe.rs:72`、`:90-98`）」；删除「窗口空白/无响应数秒」。
3. 触发条件补一句：仅缓存未命中时发生，缓存命中直接返回。

---

## 发现 3 · 同步日志目录枚举 + 整文件读 —— 需修正

**判定理由**：主清单把两件事并成一条。目录枚举部分属实且确实在主进程主线程；「整文件读 + 逐行读取尾部在主线程阻塞」不成立 —— 尾部读取已经在 `spawn_blocking` 里执行，且是按块回读、总量封顶 4 MiB，不是整读。

**回源后的阻塞线程归属**（拆成两处）：
- `list_backend_log_files`：主进程主线程（同步 `fn` 命令，Tauri v2 同步 command 在主线程执行 —— 框架行为，标**推测**）。
- `read_backend_log_tail`：阻塞线程池（`tokio::task::spawn_blocking`），不占主线程。

**应引用的源码位置**：
- `apps/desktop-tauri/src-tauri/src/commands.rs:194-200`（同步命令 + `read_dir`）
- `apps/desktop-tauri/src-tauri/src/commands.rs:253-257`（async 命令 + `spawn_blocking`）
- `apps/desktop-tauri/src-tauri/src/commands.rs:262-263`（64 KiB 块 / 4 MiB 上限）
- `apps/desktop-tauri/src-tauri/src/commands.rs:265`（阻塞实现）
- `apps/desktop-tauri/src-tauri/src/commands.rs:302-303`（`File::open` + 流式计数）
- `apps/desktop-tauri/src-tauri/src/commands.rs:325-330`（从文件尾按块回读）

**应贴的代码片段（照抄源码）**：

```rust
#[tauri::command]
pub fn list_backend_log_files() -> Result<LogFileInfo, String> {
    let dir = log_dir();
    let mut files: Vec<LogFileEntry> = Vec::new();
    let mut total_bytes: u64 = 0;
    if dir.is_dir() {
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
```

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

**修正要求**：
1. 拆成两条独立发现：3a「同步命令 `list_backend_log_files` 在主进程主线程枚举日志目录」；3b 原来的尾部读取**不成立**，删除该部分。
2. 3a 的规模说明补齐：枚举对象是单个日志目录下的日志文件，逐条只取 `metadata()`，不是递归；主清单的「275 行对每个条目再 `read_dir`」与源码不符 —— 275 行是 `read_backend_log_tail_blocking` 里的第一次 `read_dir`，与 `list_backend_log_files` 无关。
3. 3b 保留在检索面统计里时，注明它在 `spawn_blocking` 内执行、回读上限 4 MiB、行数统计是流式计数而非逐行读全文件。

---

## 发现 4 · 同步目录递归枚举 —— 需修正

**判定理由**：`read_dir` 同步枚举属实，但命令是 `pub async fn list_directory`，占住的是 tokio worker；函数体只有一层 `read_dir`，没有递归。

**回源后的阻塞线程归属**：tokio worker（async 命令体未交给 `spawn_blocking`），依据同发现 1（`commands.rs:944-952` 与 `session_persistence/mod.rs:71-72` 两条旁证），Tauri 侧调度行为标**推测**。

**应引用的源码位置**：
- `apps/desktop-tauri/src-tauri/src/commands.rs:1098-1099`（`pub async fn list_directory`）
- `apps/desktop-tauri/src-tauri/src/commands.rs:1133-1134`（`std::fs::read_dir`）

**应贴的代码片段（照抄源码）**：

```rust
    let entries = std::fs::read_dir(&canonical)
        .map_err(|e| format!("cannot read directory '{}': {e}", canonical.display()))?;
```

**修正要求**：
1. 标题去掉「递归」，改为「单层目录枚举」。函数体内只有一次 `read_dir`，未递归进子目录。
2. 线程归属由「主进程主线程（阻塞 tokio worker……）」改为 tokio worker 一句 —— 主清单这一条自相矛盾（同时写了主线程和 worker），按证据取 worker。
3. 行号写作 `commands.rs:1133-1134`（`read_dir` 起始行是 1133，主清单写的 1134 是续行）。
4. 触发条件保留「含大量文件的目录 / 网络盘 / OneDrive 同步目录」，界面表现改写为「同 runtime 上其它 IPC 排队，窗口不一定整体无响应」。

---

## 发现 5 · 忙等轮询循环：session 持久化重试 —— 需修正

**判定理由**：`thread::sleep` 重试属实，但重试对象是 SQLite `SQLITE_BUSY`（不是文件锁），且代码里已有 2 s 总预算与 `block_in_place` 包裹，调用点在 sidecar 进程内而非桌面主进程。

**回源后的阻塞线程归属**：sidecar 进程内的 tokio worker。
- `crates/hermix-agent/src/local_agent_service/mod.rs:865-871` 的调用点包在 `crate::session_persistence::run_blocking(|| { … })` 里；
- `crates/hermix-agent/src/session_persistence/mod.rs:81-96` 的 `run_blocking` 在 MultiThread runtime 上走 `tokio::task::block_in_place`（`:92`），把 worker 交回阻塞池，不长期占住 runtime worker；单线程 runtime 直接同步执行（`:94`）。
- 主进程侧没有这条调用链：`apps/desktop-tauri/src-tauri/Cargo.toml` 的依赖列表里没有 `hermix-agent`；`hermix-agent` 只被 `crates/hermix-cron/Cargo.toml:13`、`crates/hermix-runtime/Cargo.toml:11`、`crates/hermix-server/Cargo.toml:26` 依赖。**推测**（未逐行核对构建产物）：sidecar 可执行文件由 `hermix-server` 构建，故该重试跑在 sidecar 进程内，不在桌面主进程。

**应引用的源码位置**：
- `crates/hermix-agent/src/session_persistence/sessions.rs:259`（`std::thread::sleep(sleep);`）
- `crates/hermix-agent/src/session_persistence/mod.rs:119-125`（重试次数与 2 s 预算常量）
- `crates/hermix-agent/src/session_persistence/mod.rs:127-134`（`is_busy_error`：判定 `SQLITE_BUSY` / `database is locked`）
- `crates/hermix-agent/src/session_persistence/mod.rs:81-96`（`run_blocking` / `block_in_place`）
- `crates/hermix-agent/src/local_agent_service/mod.rs:865-871`（调用点）

**应贴的代码片段（照抄源码）**：

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
    match tokio::runtime::Handle::try_current() {
        Ok(handle)
            if matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
```

**修正要求**：
1. 机制改写为「SQLite 写冲突（`SQLITE_BUSY`）的应用层忙等重试，最坏受 `WRITE_RETRY_BUDGET` 2 s 总预算约束」；删除「文件锁竞争」表述（源码里没有文件锁，只有 SQLite busy 判定）。
2. 线程归属由「调用方线程——若从主进程主线程同步调用则连带卡窗口；未定位调用点，标推测」改为「sidecar 进程内的 tokio worker」，并注明 `block_in_place` 已把 worker 交回阻塞池；主进程无调用链的证据（`Cargo.toml` 依赖关系）写进条目。
3. 界面表现改写：主清单写的「对话保存时输入框短暂无响应」在桌面主进程不成立；sidecar 内的等待影响的是 sidecar 自己的请求延迟，最坏 2 s。
4. 保留引用 `crates/hermix-agent/src/session_persistence/mod.rs:71`（模块注释里的「up to 15 retries (~2 s+)」自述）——该行号经复核正确。

---

## 线程归属修订后的总表

| 条目 | 主清单写的归属 | 回源后的归属 | 依据 |
|---|---|---|---|
| 发现 1 | 主进程主线程 | tokio worker（推测：Tauri v2 async command 调度到 async runtime） | `commands.rs:671` 为 `pub async fn`；旁证 `commands.rs:944-952`、`session_persistence/mod.rs:71-72` |
| 发现 2 | 主进程主线程 | 主进程主线程（推测：setup 在事件循环启动前于主线程执行） | `lib.rs:911` 调用点在 `setup` 闭包内；`webview2_probe.rs:90-98` 同步 fn |
| 发现 3 | 主进程主线程 | 3a 主进程主线程（同步 command，框架行为推测）；3b 阻塞线程池 | `commands.rs:195` 同步 fn；`commands.rs:254-256` `spawn_blocking` |
| 发现 4 | 主进程主线程（阻塞 tokio worker） | tokio worker | `commands.rs:1099` 为 `pub async fn` |
| 发现 5 | 调用方线程（推测，未定位调用点） | sidecar 进程内 tokio worker | `local_agent_service/mod.rs:865-871` + `session_persistence/mod.rs:81-96`；主进程 `Cargo.toml` 无 `hermix-agent` 依赖 |

## 交付前的状态说明

- 本文件只判定主清单 5 条发现；主清单检索面 d/e/f 及其它 sleep 命中的逐条结果在 `docs/blocking-hang-audit-gaps.md`。
- 本轮未修改 `docs/blocking-hang-audit.md`，未修改任何源码；未运行构建、测试或应用。
- 未实测项：各阻塞点的实际耗时（毫秒）没有测量，本轮禁止启动应用与跑测试；条目中的耗时只有源码注释自述值（`webview2_probe.rs:111-112` 的「~5ms」、`codeHighlight.ts:381-382` 的「~8 ms for 1000 lines of Python」）。
