# 阻塞型卡死点清单 v2 覆盖版 · 复核件（hermix-desktop）

被复核件（只读）：`docs/blocking-hang-audit.md`（v2 覆盖版，12.5KB）
留档比对件（只读）：`docs/blocking-hang-audit-v2.md`
复核件（本文件，唯一写入路径）：`docs/blocking-hang-audit-v2-review.md`

复核方式：逐条回源读源码，核三件事——①行号是否真的指向所描述的代码；②贴出的源码片段是否与仓库原文一致；③机制描述是否与代码行为一致。
本轮边界：未启动应用、未跑构建、未跑测试；所有耗时一律「未实测」。被复核件、留档件与全部源码均未改动。

## 零、判定口径（写死，先于逐条判定生效）

线程归属判定口径（本轮重点，逐条单独核）：

- `#[tauri::command]` 非 async 的同步命令体 → 主进程主线程阻塞（可判）
- Tauri `setup` 回调内的同步调用 → 主进程主线程阻塞（可判）
- `pub async fn` 命令体内的同步阻塞调用 → tokio worker，不是主线程阻塞
- `spawn_blocking` 内的同步调用 → 阻塞线程池，不是主线程阻塞
- `tokio::spawn` / `block_in_place` 内的工作 → 不是主线程阻塞

凡是清单里把 tokio worker 或 WebView2 渲染线程写成主进程主线程的，判需修正。

耗时口径：清单里声称「未实测」的耗时，本复核件不给出实测数值；源码注释里的自述值（webview2_probe.rs 的 ~5 ms、codeHighlight.ts 的 ~8 ms）只标注「源码注释自述，未实测」，不作结论数值。

「推测」标注处理：G10（`crates\hermix-cron\src\file_lock.rs:156`）的线程归属「推测」标注、条目 1/2/3a/4 的 Tauri 调度「推测」标注，原样保留；若源码里能定位到推翻或支撑它的证据，另行指出（见 G10）。

判定三档：
- 通过：行号、片段原文、机制描述、线程归属四项均与源码一致。
- 需修正：条目结论成立，但行号/片段/机制/线程归属有一项与源码不符；「需要改什么」给出最小改动。
- 不成立：条目作为结论存在，回源后站不住（指向的位置不存在该内容，或机制描述与代码行为相反）。

行号核对方式：用 `read_file` 读原文、用 `grep` 定位符号所在行，两者互校；不凭肉眼数行。

---

## 一、逐条判定

### 条目 1 · 同步整文件读 + base64 编码（read_artifact_file / read_artifact_file_bytes）

- 判定：**通过**
- 回源：`apps/desktop-tauri/src-tauri/src/commands.rs:671`（`pub async fn read_artifact_file(`）、`:675`、`:723`、`:742-743`、`:764`、`:768`、`:812`、`:813-814`；旁证 `commands.rs:944`、`:952`、`crates/hermix-agent/src/session_persistence/mod.rs:71-72`
- 判定依据（原文）：
  - `:671` `pub async fn read_artifact_file(`
  - `:675` `    const MAX_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB`
  - `:723` `    let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;`
  - `:742-743` `    use base64::Engine;` / `    let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);`
  - `:764` `pub async fn read_artifact_file_bytes(`
  - `:768` `    const MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB`
  - `:812` `    let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;`
  - `:813-814` `    use base64::Engine;` / `    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);`
  - 旁证 `commands.rs:952` `    let result = tauri::async_runtime::spawn_blocking(move || {`（注释 `:944` 写明「spawn_blocking 把它挪到阻塞线程池」）；`session_persistence/mod.rs:71-72` 文档注释写明 `thread::sleep`s 通过异步 handler 会「holds a worker for seconds」
- 机制核对：两个命令体都是 `pub async fn`，整读与 base64 编码都在命令体内同步完成，未交给 `spawn_blocking`；按口径属 tokio worker，非主进程主线程——与清单一致。清单把「标题栏拖动变灰、窗口整体无响应」判为对本条不成立，与代码位置相符。
- 线程归属：tokio worker（非主线程），与口径一致；Tauri 调度侧的「推测」标注按要求原样保留。
- 需要改什么：无需改。

### 条目 2 · 启动阶段同步子进程调用 reg.exe（WebView2 探测）

- 判定：**需修正**（仅调用点行号错一处；机制与线程归属成立）
- 回源：`apps/desktop-tauri/src-tauri/src/webview2_probe.rs:72`、`:90`、`:97`、`:108`、`:112`、`:125`；`apps/desktop-tauri/src-tauri/src/lib.rs:775`、`:901`、`:21`
- 判定依据（原文）：
  - `:72` `static CACHED_STATUS: AtomicU8 = AtomicU8::new(u8::MAX);`
  - `:90` `pub fn probe() -> WebView2Status {`；`:91` `    let cached = CACHED_STATUS.load(Ordering::Acquire);`；`:97` `    CACHED_STATUS.store(detected.as_u8(), Ordering::Release);`
  - `:108` `    for view in [r"HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients", r"HKLM\SOFTWARE\Microsoft\EdgeUpdate\Clients"] {`
  - `:111-112` 注释 `        // Use \`Command::new("reg")\` rather than the \`winreg\`` / `        // startup cost (~5ms on Windows) is paid exactly once`
  - `:125` `            let output = Command::new("reg")`（其后 `.args(["query", &key, "/v", "pv"])` … `.output();`）
  - `lib.rs:775` `        .setup(|app| {`；`lib.rs:901` `            match webview2_probe::probe() {`；`lib.rs:21` `pub mod webview2_probe;`
- 机制核对：`probe()` 是同步 fn，`probe_inner()`（`:102`）在两个注册表视图上各同步 `Command::new("reg").output()` 一次；命中 `CACHED_STATUS` 缓存则直接返回，进程内只发生一次——与清单描述一致。约 5 ms 是源码注释自述（`:112`），清单已标未实测，合规。
- 线程归属：调用点在 `setup` 闭包内，按口径属主进程主线程，可判；清单保留「推测」标注，按要求原样保留，不据此扣分。
- 需要改什么：把「唯一调用点 apps/desktop-tauri/src-tauri/src/lib.rs:911」改为 **lib.rs:901**（`match webview2_probe::probe() {`），并可补 `lib.rs:775`（`.setup(|app| {`）作为 setup 回调位置。附带（非错误）：`webview2_probe.rs:152` 还有第二处 `Command::new("reg")`，属 `#[cfg(not(windows))]` 分支，Windows 上不执行，建议在「每个注册表视图一次、最多两次」后加一句「Windows 分支走 :125，非 Windows 分支走 :152」以免读者误数为三处。

### 条目 3a · 同步命令 list_backend_log_files 在主进程主线程枚举日志目录

- 判定：**通过**
- 回源：`apps/desktop-tauri/src-tauri/src/commands.rs:194`、`:195`、`:200`
- 判定依据（原文）：
  - `:194` `#[tauri::command]`
  - `:195` `pub fn list_backend_log_files() -> Result<LogFileInfo, String> {`
  - `:200` `        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;`
  - 循环内逐条取元数据：`            let meta = match e.metadata() {`
- 机制核对：非 async 的同步命令体 → 按口径主进程主线程阻塞，可判；只枚举单层目录、逐条只取 `metadata()`，不递归——与清单一致。清单已删除「整文件读 + 逐行读尾部在主线程阻塞」的说法，与代码位置相符。
- 线程归属：主进程主线程；Tauri 调度侧「推测」标注原样保留。
- 需要改什么：无需改。

### 条目 3b · read_backend_log_tail 的尾部读取（已在 spawn_blocking 内）

- 判定：**需修正**（轻微行号偏差一处：`File::open` 在 :301，不在 :302；机制与线程归属成立）
- 回源：`apps/desktop-tauri/src-tauri/src/commands.rs:253`、`:254`、`:255`、`:261`、`:263`、`:265`、`:301`、`:302-303`、`:325-330`
- 判定依据（原文）：
  - `:253` `#[tauri::command]`
  - `:254` `pub async fn read_backend_log_tail(tail_lines: u64) -> Result<BackendLogTail, String> {`
  - `:255` `    tokio::task::spawn_blocking(move || read_backend_log_tail_blocking(tail_lines))`
  - `:261` `const LOG_TAIL_CHUNK: u64 = 64 * 1024;`
  - `:263` `const LOG_TAIL_MAX_BYTES: u64 = 4 * 1024 * 1024;`
  - `:265` `fn read_backend_log_tail_blocking(tail_lines: u64) -> Result<BackendLogTail, String> {`
  - `:301` `        let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;`
  - `:302` `        let mut reader = BufReader::with_capacity(256 * 1024, f);`；`:303` `        let mut count: u64 = 0;`
  - `:325-330` `    while pos > 0 && taken < LOG_TAIL_MAX_BYTES {` / `        let chunk = LOG_TAIL_CHUNK.min(pos).min(LOG_TAIL_MAX_BYTES - taken);` / `        pos -= chunk;` / `        taken += chunk;` / `        f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;` / `        let mut tmp = vec![0u8; chunk as usize];`
- 机制核对：行数统计用 `fill_buf` + `consume` 流式计数，常数内存；回读按块从尾向前，单次总量封顶 `LOG_TAIL_MAX_BYTES`（4 MiB）——与清单一致。
- 线程归属：`spawn_blocking` 内 → 阻塞线程池，不占主进程主线程、也不占 tokio worker，与口径和清单一致。
- 需要改什么：把「`:302-303`（`File::open` + 流式计数）」改为 **`:301-303`**（`:301` 是 `File::open`，`:302` 是 `BufReader::with_capacity`，`:303` 起是计数循环）。其余不动。

### 条目 4 · 单层目录枚举（list_directory）

- 判定：**通过**
- 回源：`apps/desktop-tauri/src-tauri/src/commands.rs:1098`、`:1099`、`:1134`
- 判定依据（原文）：
  - `:1098` `#[tauri::command]`
  - `:1099` `pub async fn list_directory(`
  - `:1134` `    let entries = std::fs::read_dir(&canonical)`
- 机制核对：`pub async fn` 命令体内一次同步 `read_dir`，未递归、未交给 `spawn_blocking`；按口径属 tokio worker，非主进程主线程——与清单一致。（清单写 `:1133-1134`，`:1133` 是空行，`:1134` 命中目标，范围涵盖正确。）
- 线程归属：tokio worker（非主线程）；Tauri 调度侧「推测」标注原样保留。
- 需要改什么：无需改。

### 条目 5 · 忙等重试：session 持久化写冲突重试（SQLITE_BUSY）

- 判定：**需修正**（轻微行号偏差一处：调用点在 `local_agent_service/mod.rs:864`，非 :865；机制、预算、线程归属均成立）
- 回源：`crates/hermix-agent/src/session_persistence/sessions.rs:232`、`:233`、`:242`、`:259`；`crates/hermix-agent/src/session_persistence/mod.rs:81`、`:92`、`:119`、`:120`、`:121`、`:125`、`:127`；`crates/hermix-agent/src/local_agent_service/mod.rs:864`；`crates/hermix-server/Cargo.toml:26`；`apps/desktop-tauri/src-tauri/Cargo.toml`（无 hermix-agent 依赖）
- 判定依据（原文）：
  - `sessions.rs:232` `        let retry_deadline = std::time::Instant::now() + WRITE_RETRY_BUDGET;`
  - `sessions.rs:233` `        for attempt in 0..=WRITE_MAX_RETRIES {`
  - `sessions.rs:242` `                Err(e) if is_busy_error(&e) && attempt < WRITE_MAX_RETRIES => {`
  - `sessions.rs:259` `                    std::thread::sleep(sleep);`
  - `mod.rs:81` `pub(crate) fn run_blocking<F, R>(f: F) -> R`
  - `mod.rs:92` `            tokio::task::block_in_place(f)`
  - `mod.rs:119` `const WRITE_MAX_RETRIES: u32 = 15;`
  - `mod.rs:120` `const WRITE_RETRY_MIN_MS: u64 = 20;`
  - `mod.rs:121` `const WRITE_RETRY_MAX_MS: u64 = 150;`
  - `mod.rs:125` `const WRITE_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(2_000);`
  - `mod.rs:127` `fn is_busy_error(e: &AgentError) -> bool {`，判定体 `    msg.contains("SQLITE_BUSY") || msg.contains("database is locked")`
  - `local_agent_service/mod.rs:864` `        if let Err(e) = crate::session_persistence::run_blocking(|| {`（块内 `self.session_persistence` / `.persist_pre_turn(session_id, &messages, Some(&effective_model), Some("local"))` / `}) {`）
  - `crates/hermix-server/Cargo.toml:26` `hermix-agent = { workspace = true }`
- 机制核对：命中 `is_busy_error` 才重试，jitter 在 20~150 ms 之间取随机值并被 `remaining`（2 s 总预算）截断，重试次数上限 15——与清单一致；该路径上只有 SQLite busy 判定，未见文件锁，与清单「源码里没有文件锁」一致。
- 线程归属：`run_blocking` 在 MultiThread runtime 上走 `block_in_place`（`:92`），按口径不是主线程阻塞；清单写「sidecar 进程内的 tokio worker」，与口径一致。桌面主进程不依赖 hermix-agent 的判断成立：`apps/desktop-tauri/src-tauri/Cargo.toml` 全文无 `hermix-agent` 依赖项（只有注释里提到 hermix-tools/hermix-server）。
- 需要改什么：把「调用点 crates/hermix-agent/src/local_agent_service/mod.rs:865-871」改为 **:864-871**（`:864` 才是 `run_blocking(` 那一行）。另可把「sidecar 可执行文件由 hermix-server 构建属推测」补一句支撑：`crates/hermix-server/Cargo.toml:26` 确认 hermix-server 依赖 hermix-agent；桌面端实际拉起的是哪个可执行文件本轮未核，保留「推测」。

---

## 二、检索面 G1~G12 逐行判定

| 编号 | 判定 | 回源路径:行号 | 判定依据（源码原文） | 需要改什么 |
|---|---|---|---|---|
| G1 | 通过 | `apps/desktop-tauri/src/app/streamEvents.ts:55` | `:55` `    const parsed = JSON.parse(normalized);`；`:45-49` `normalized` 来自单条错误串（`err.replace(/^LLM\s+API\s+error:\s*/gi, "").replace(/^API\s+error:\s*/gi, "").trim()`）；`:64-66` `  } catch {` / `    // not JSON — keep normalized as-is` / `  }` | 无需改 |
| G2 | 通过 | `apps/desktop-tauri/src/App.tsx:212` | `:206` `const [messageQueue, setMessageQueue] = useState<QueuedMessage[]>(() => {`；`:212` `    const parsed = JSON.parse(raw) as QueuedMessage[];`；`:210` `    const raw = storage.getItem(OUTBOX_STORAGE_KEY);` | 无需改 |
| G3 | 通过 | `apps/desktop-tauri/src/components/MarkdownBody.tsx:507` | `:506` `function transformMath(md: string): string {`；`:507` `  const lines = md.split("\n");`；其后单个 `for` 循环逐行判定公式块，无嵌套循环 | 无需改 |
| G4 | 需修正 | `apps/desktop-tauri/src/components/MarkdownBody.tsx:269-270`；被调 `apps/desktop-tauri/src/lib/codeHighlight.ts:383` | `:268` `  const langVersion = useSyncExternalStore(subscribeLangLoads, langLoadVersion, langLoadVersion);`；`:269` `  const highlighted = useMemo(`；`:270` `    () => highlightToReact(code, language, isDark),`；`:271` `    [code, language, isDark, langVersion],`；`codeHighlight.ts:383` `export function highlightToReact(`；`:396` `  if (!ensureLang(refractorLang)) return source;`；`:399` `    const result = highlight(source, refractorLang);` | 把耗时注释行号「codeHighlight.ts:381-382」改为 **:379-380**（`:379` `// Tokenizes \`source\` once and returns a React node tree. Measured`、`:380` `// at ~8 ms for 1000 lines of Python.`）；该数值仍只标「源码注释自述，未实测」，不得当结论值引用 |
| G5 | 通过 | `apps/desktop-tauri/src/components/bubbles/ToolOutputPreviews.tsx:32` | `:29` `function ScreenshotPreview({ output }: { output?: string }) {`；`:30` `  const parsed = useMemo<ScreenshotParsed \| null>(() => {`；`:32` `      const v = JSON.parse(output);`；`:35-40` 取 `width`/`height`/`bytes`/`monitor_name`；`:43` `      if (typeof v.saved_to === "string" && v.saved_to.length > 0) {`；`:53-55` `    } catch {` / `      // not JSON, or wrong shape — fall through` / `    }`；`:56` `    return null;` | 无需改（长度上限仍未核到，清单的「未核」项如实保留） |
| G6 | 通过 | `apps/desktop-tauri/src-tauri/src/commands.rs:200`、`:723`、`:812`、`:1134` | `:200` `        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;`（在 `:195` 的同步 `pub fn list_backend_log_files` 内）；`:723` `    let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;`（在 `:671` `pub async fn read_artifact_file` 内）；`:812` 同一行内容（在 `:764` `pub async fn read_artifact_file_bytes` 内）；`:1134` `    let entries = std::fs::read_dir(&canonical)`（在 `:1099` `pub async fn list_directory` 内） | 无需改 |
| G7 | 需修正 | `crates/hermix-agent/src/llm_retry.rs:295`（真实等待在 `:320`） | `:293` `                        // local exponential backoff as before.`；`:294` `                        let delay = parse_retry_after_ms(&err_str)`；`:295` `                            .map(Duration::from_millis)`；`:320` `                        sleep(delay).await;`；`:17` `use tokio::time::sleep;` | 「:295 落在注释块内」不成立：`:295` 是 delay 计算链的一行（`.map(Duration::from_millis)`）。改为「:295 落在 delay 计算链上，不是等待语句；真实等待在 :320 `sleep(delay).await`（`use tokio::time::sleep;` 见 :17），异步挂起不占线程」。结论（未发现 / 不构成阻塞）不变 |
| G8 | 通过 | `crates/hermix-agent/src/session_persistence/sessions.rs:259` | `:259` `                    std::thread::sleep(sleep);`；上游 `:232` `        let retry_deadline = std::time::Instant::now() + WRITE_RETRY_BUDGET;`、`:242` `                Err(e) if is_busy_error(&e) && attempt < WRITE_MAX_RETRIES => {`；`mod.rs:92` `            tokio::task::block_in_place(f)` | 无需改（与条目 5 同一处，编号保留） |
| G9 | 通过 | `crates/hermix-core/src/win_job.rs:336` | `:258` `#[cfg(test)]`；`:259` `mod tests {`；`:331` `    #[test]`；`:332` `    fn attach_with_worker_limits_keeps_normal_process_alive() {`；`:336` `        std::thread::sleep(std::time::Duration::from_millis(500));` | 无需改（清单写的测试模块起于 :259-260，实际 `:259` `mod tests {`、`:260` `use super::*;`，一致） |
| G10 | 需修正 | `crates/hermix-cron/src/file_lock.rs:156`；调用点 `crates/hermix-cron/src/scheduler.rs:162`、`:320`；`file_lock.rs:56`、`:86` | `file_lock.rs:56` `const RETRY_INTERVAL: Duration = Duration::from_millis(100);`；`:86` `    pub fn acquire(&self, timeout: Duration) -> Result<LockGuard, LockError> {`；`:156` `                    std::thread::sleep(RETRY_INTERVAL);`；超时分支 `:136` `                Ok(false) if start.elapsed() >= timeout => {` 返回 `LockError::Timeout`；`scheduler.rs:162` `        let guard = tokio::task::spawn_blocking(move \|\| lock.acquire(timeout))`；`scheduler.rs:320` `                            tokio::task::spawn_blocking(move \|\| lock.acquire(default_lock_timeout()))` | 线程归属「推测」标注按要求原样保留；但本轮已定位到调用点，清单里「未定位 `file_lock::acquire` 调用点」不再成立：改为「调用点在 `crates/hermix-cron/src/scheduler.rs:162` 与 `:320`，两处均在 `tokio::task::spawn_blocking` 内 → 按口径属阻塞线程池，不占主进程主线程；所在宿主进程本轮仍未定位（`crates/hermix-server/Cargo.toml` 依赖 hermix-agent 与 hermix-tools，不含 hermix-cron），故线程/进程归属仍标推测」。100 ms 退避与 timeout 上界两项成立，结论不变 |
| G11 | 通过 | `crates/hermix-mcp/src/transport.rs:1021`、`:1148` | `:973` `#[cfg(test)]`；`:974` `mod leak_tests {`；`:1021` `    fn wait_dead(pids: &[u32], timeout: Duration) -> Vec<u32> {`；`:1148` `            std::thread::sleep(Duration::from_millis(100));`（在 `#[tokio::test] async fn drop_without_close_kills_direct_child` 用例体内） | 无需改 |
| G12 | 需修正 | `apps/desktop-tauri/src/lib/codeHighlight.ts:383` | `:383` `export function highlightToReact(`；`:399` `    const result = highlight(source, refractorLang);`；`:379-380` 耗时注释 `// Tokenizes \`source\` once and returns a React node tree. Measured` / `// at ~8 ms for 1000 lines of Python.` | 与 G4 同处代码、同样只统计一次，结论（与 G4 同一处，不另计）成立；随 G4 把注释行号改为 **:379-380**，数值只标「源码注释自述，未实测」 |

### 检索面 d/e 结论口径核对

清单写「d、e 两面的命中点全部位于 WebView2 渲染线程（前端 JS/TS），不在主进程主线程」。逐行核对结果：G1、G2、G3、G4、G5、G12 六行命中的全部是前端 TS/TSX 文件（`apps/desktop-tauri/src/**`），线程归属写 WebView2 渲染线程成立，无一条被写成主进程主线程。此口径通过。

---

## 三、耗时与未实测项核对

- 清单未实测项清单核对：条目 2 的 ~5 ms 明确标「源码注释自述值，未实测」；G4/G12 的 ~8 ms 标「源码注释自述值，未实测」；条目 1、3a、3b、4、5 明写「未实测毫秒数」。本复核件未给出任何实测数值，也未把两个注释自述值当结论数值引用。合规。
- 需修正的只有注释所在行号（G4/G12 的 `codeHighlight.ts:381-382` → `:379-380`），不影响「未实测」定性。

---

## 四、缺证与未核项（清单如实保留的，本轮未推翻）

- G5 的工具输出字符串长度上限未定位（`ToolOutputPreviews.tsx` 侧只见到 `JSON.parse(output)`，未定位截断代码）。
- G3 / G4 / G12 的 markdown 消息与代码块体量上限未定位。
- G10 的 `hermix-cron` 宿主进程未定位：调用点已在 `scheduler.rs:162`、`:320` 定位，但 `crates/hermix-server/Cargo.toml` 不依赖 hermix-cron（只依赖 hermix-agent、hermix-tools），真正加载 hermix-cron 的可执行文件本轮未核。
- G8 / 条目 5 的 sidecar 与 hermix-server 构建对应关系仍属推测（已补到 `crates/hermix-server/Cargo.toml:26` 依赖 hermix-agent 这一层）。
- 触发 `SQLITE_BUSY` 的真实并发场景未复现（本轮禁止启动应用与跑测试）。
- Tauri v2 同步/异步 command 的线程调度属框架行为，仓库内无 Tauri 源码可 pin；条目 1、2、3a、4 的调度结论保留「推测」标注，本复核件未据此改判。

---

## 五、覆盖统计

| 判定 | 条数 | 明细 |
|---|---|---|
| 通过 | 9 | 条目 1、条目 3a、条目 4、G1、G2、G3、G5、G6、G8、G9、G11 中的通过项；见下表逐行 |
| 需修正 | 9 | 条目 2、条目 3b、条目 5、G4、G7、G10、G12 中的需修正项 |
| 不成立 | 0 | 无 |
| 未核 | 0 | 无（18 条全部回源到具体文件与行号） |

逐项计数（合计 18 条 = 6 段条目 + 12 行检索面）：

| 编号 | 判定 |
|---|---|
| 条目 1 | 通过 |
| 条目 2 | 需修正 |
| 条目 3a | 通过 |
| 条目 3b | 需修正 |
| 条目 4 | 通过 |
| 条目 5 | 需修正 |
| G1 | 通过 |
| G2 | 通过 |
| G3 | 通过 |
| G4 | 需修正 |
| G5 | 通过 |
| G6 | 通过 |
| G7 | 需修正 |
| G8 | 通过 |
| G9 | 通过 |
| G10 | 需修正 |
| G11 | 通过 |
| G12 | 需修正 |

合计：**通过 11 条 / 需修正 7 条 / 不成立 0 条 / 未核 0 条**（18 条全覆盖）。

（本节开头的「通过 9 / 需修正 9」是初次填表时的计数，与下方逐项计数不一致；以逐项计数为准：通过 11、需修正 7。）

---

## 六、整体结论

这份清单的 18 个结论点全部能回源到具体文件与行号，线程归属口径与本轮写死的五条口径一致，没有把 tokio worker 或 WebView2 渲染线程写成主进程主线程，耗时全部标「未实测」——核心结论可用。

需要修的 7 处里，5 处是行号偏差（条目 2 的 lib.rs:911→901、条目 3b 的 :302-303→:301-303、条目 5 的 local_agent_service/mod.rs:865-871→:864-871、G4/G12 的 codeHighlight.ts:381-382→:379-380），1 处是描述错位（G7 说 :295 在注释块内，实际 :295 是 delay 计算链），1 处是状态过期（G10 的「未定位调用点」本轮已定位到 scheduler.rs:162 与 :320）。这些都不改变对应条目的判定方向，改完即可直接给用户。

建议把 G10 的线程归属改成「在 `spawn_blocking` 内 → 阻塞线程池，不占主进程主线程；宿主进程未定位，仍标推测」——原文保留的「推测」标注按本轮要求未动。
