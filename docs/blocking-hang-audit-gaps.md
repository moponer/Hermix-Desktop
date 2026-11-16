# 阻塞型卡死点清单 · 待核检索面逐条结果（补漏清单）

复核对象：`docs/blocking-hang-audit.md`（主清单，只读，本次未修改）中标注为「待核」的检索面 d/e/f，以及主清单「未完成项」里点名的 `llm_retry.rs:295`、`session_persistence/sessions.rs:259` 重试与轮询命中。
复核人：验收员（技术结论复核）
复核方式：逐条 `read_file` 读函数体与调用点，只认函数体/调用点的实际代码；注释只作旁证。
本文件不引用 `docs/wait-timeout-audit.md` / `docs/wait-timeout-audit-review.md` 的任何说法。

## 汇总

- 条目数：12（d 面 5 条、e 面 2 条、f 面 1 条、重试/轮询命中 4 条）
- 判定分布：**已确认存在 9 条 / 未发现 3 条 / 推测 0 条**（其中 G10 的线程归属单独标「推测」，缺证据写在条目内）
- 条目与主清单的对应关系：d 面对应主清单检索面 d 的 5 个命中点；G4 / G12 对应检索面 e；G6 对应检索面 f；G7 / G8 对应主清单点名的两个重试点；G9 / G10 / G11 对应主清单「未完成项 2、4」与检索面 b 的剩余命中。
- 重要口径：d/e 两面的命中点全部位于 **WebView2 渲染线程**（前端 JS/TS），不在主进程主线程；主进程侧的阻塞判定见 `docs/blocking-hang-audit-review.md`。
- 未核项（写明缺什么，不补猜测）：见文末「未核与缺证」一节。

| # | 检索面 | 位置 | 判定 |
|---|---|---|---|
| G1 | d 前端大文本处理 | `apps/desktop-tauri/src/app/streamEvents.ts:55` | 已确认存在（不构成阻塞） |
| G2 | d 前端大文本处理 | `apps/desktop-tauri/src/App.tsx:212` | 已确认存在（不构成阻塞） |
| G3 | d 前端大文本处理 | `apps/desktop-tauri/src/components/MarkdownBody.tsx:507` | 已确认存在（轻度，规模未实测） |
| G4 | e 渲染层重活 | `apps/desktop-tauri/src/components/MarkdownBody.tsx:269-270` | 已确认存在 |
| G5 | d 前端大文本处理 | `apps/desktop-tauri/src/components/bubbles/ToolOutputPreviews.tsx:32` | 已确认存在（规模未知） |
| G6 | f 主进程主线程重活 | `apps/desktop-tauri/src-tauri/src/commands.rs` 4 处 | 已确认存在（归属有修正，结论见 review 文件） |
| G7 | 重试命中 | `crates/hermix-agent/src/llm_retry.rs:295` | 未发现（引用错位；真实等待在 320 行且为异步 sleep） |
| G8 | 重试命中 | `crates/hermix-agent/src/session_persistence/sessions.rs:259` | 已确认存在（有 2 s 预算 + `block_in_place` 包裹） |
| G9 | 轮询命中 | `crates/hermix-core/src/win_job.rs:336` | 未发现（位于 `#[cfg(test)]` 测试模块） |
| G10 | 轮询命中 | `crates/hermix-cron/src/file_lock.rs:156` | 已确认存在（100 ms 退避，有 timeout 上界） |
| G11 | 轮询命中 | `crates/hermix-mcp/src/transport.rs:1021` / `:1148` | 未发现（均位于测试代码） |
| G12 | d 前端大文本处理 | `apps/desktop-tauri/src/lib/codeHighlight.ts:383` | 已确认存在（与 G4 同一处代码，主清单重复计数） |

---

## G1 · streamEvents.ts 的 JSON.parse —— 已确认存在（不构成阻塞）

- 路径:行号：`apps/desktop-tauri/src/app/streamEvents.ts`，函数 `localizeStreamError`（读到的区间 40-69），`JSON.parse` 位于 `try` 块首行之后（第 55-56 行；主清单标 55，行号未用 grep 二次 pin，区间已核实）。
- 机制说明：把一条错误文本当 JSON 解析，取 `error.message` 等字段；解析失败走 `catch` 兜底保持原文。输入是单条 provider 错误消息，不是大文本或大数组。
- 证据片段：
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
- 判定：已确认存在（调用点存在），但**不构成阻塞型卡死** —— 输入体量由单条错误消息决定。
- 缺什么证据：不缺。

## G2 · App.tsx 的 JSON.parse —— 已确认存在（不构成阻塞）

- 路径:行号：`apps/desktop-tauri/src/App.tsx:212`（`useState` 惰性初始化函数内）。
- 机制说明：启动时从 `localStorage` 读 `hermix.outbox.v1` 待发队列并 `JSON.parse`。解析结果再 `filter` 一遍。发生在 React 状态初始化阶段，只跑一次，不在每次渲染里重跑。
- 证据片段：
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
- 判定：已确认存在（调用点存在），**不构成阻塞** —— 队列体量受待发消息条数限制，解析一次。
- 缺什么证据：不缺。

## G3 · MarkdownBody.tsx 的 md.split —— 已确认存在（轻度，规模未实测）

- 路径:行号：`apps/desktop-tauri/src/components/MarkdownBody.tsx:507`，函数 `transformMath`。
- 机制说明：把整段 markdown 按 `\n` 切成行数组，再逐行判断公式块并回写。字符串 split 是同步的，在渲染线程执行，代价随消息长度线性增长。
- 证据片段：
  ```tsx
  function transformMath(md: string): string {
    const lines = md.split("\n");
    const out: string[] = [];
    let inFence = false;
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i]!;
  ```
- 判定：已确认存在（调用点存在）。严重度为轻度：单次线性扫描，无嵌套循环。
- 缺什么证据（规模）：本轮禁止启动应用与跑测试，没有「单条消息最大长度 / 行数」的上限证据，也没测出毫秒耗时；仓库内未找到对 markdown 消息体长度的截断代码。要定量需要补：消息体长度上限的代码位置，或一次可复现的渲染耗时测量。

## G4 · 渲染层同步语法高亮 —— 已确认存在

- 路径:行号：`apps/desktop-tauri/src/components/MarkdownBody.tsx:269-270`（`useMemo` 内同步调用），被调函数 `apps/desktop-tauri/src/lib/codeHighlight.ts:383`（`export function highlightToReact`，grep 确认）。
- 机制说明：`highlightToReact` 在渲染线程同步跑 refractor 分词并转 React 节点；`useMemo` 依赖 `[code, language, isDark, langVersion]`，同一段代码不会重复计算；长尾语言未加载时先返回纯文本，加载完成后通过 `langVersion` 触发一次重算。
- 证据片段：
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
- 判定：已确认存在。阻塞线程归属：**WebView2 渲染线程（前端 JS 线程）**，不是主进程主线程。
- 缺什么证据：耗时只有源码注释自述的「~8 ms for 1000 lines of Python」，本轮未实测；没有「一段代码多长才卡」的阈值。要定量需要一次渲染耗时测量。

## G5 · ToolOutputPreviews.tsx 的 JSON.parse —— 已确认存在（规模未知）

- 路径:行号：`apps/desktop-tauri/src/components/bubbles/ToolOutputPreviews.tsx:32`，组件 `ScreenshotPreview` 的 `useMemo` 内。
- 机制说明：把工具输出字符串当 JSON 解析，提取 `width/height/bytes/monitor_name/saved_to`；解析失败返回 null（不抛错）。
- 证据片段：
  ```tsx
  function ScreenshotPreview({ output }: { output?: string }) {
    const parsed = useMemo<ScreenshotParsed | null>(() => {
      if (!output) return null;
      try {
        const v = JSON.parse(output);
        if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  ```
- 判定：已确认存在（调用点存在）。是否构成阻塞取决于工具输出长度，**该长度上限未核到**。
- 缺什么证据：未找到「工具输出字符串长度上限 / 截断」的实现位置，因此无法判定是否可能形成大文本解析；补齐需要主进程侧工具输出写回路径与截断代码。

## G6 · 主进程主线程上的同步重活（检索面 f）—— 已确认存在，归属有修正

- 路径:行号（4 处，全部 `read_file` 核对过）：
  - `apps/desktop-tauri/src-tauri/src/commands.rs:200`（`list_backend_log_files` 的 `read_dir`，同步命令）
  - `apps/desktop-tauri/src-tauri/src/commands.rs:723`（`read_artifact_file` 的整读 + base64，async 命令）
  - `apps/desktop-tauri/src-tauri/src/commands.rs:812`（`read_artifact_file_bytes` 的整读 + base64，async 命令）
  - `apps/desktop-tauri/src-tauri/src/commands.rs:1133-1134`（`list_directory` 的 `read_dir`，async 命令）
- 机制说明与证据片段：见 `docs/blocking-hang-audit-review.md` 的发现 1、3、4（含逐条代码片段）。
- 判定：已确认存在（4 处命中全部核对完毕）。**归属修正**：只有 `commands.rs:200` 所在的同步命令落在主进程主线程；723 / 812 / 1133 三处在 async 命令体内，落在 tokio worker。
- 缺什么证据：三条 async 命中的实际阻塞时长未实测（禁止跑应用/测试）。

## G7 · llm_retry.rs:295 的等待 —— 未发现（引用错位）

- 路径:行号：`crates/hermix-agent/src/llm_retry.rs:295`；真实等待在 `crates/hermix-agent/src/llm_retry.rs:320`（grep 确认）。
- 机制说明：第 295 行落在注释块里（「local exponential backoff as before.」）。真正的等待是 `sleep(delay).await`，用的是 `tokio::time::sleep`（`llm_retry.rs:17` `use tokio::time::sleep;`），是异步挂起 —— 等待期间不占线程。
- 证据片段：
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
- 判定：**未发现**阻塞型忙等 —— 主清单引用的行号不对，真实等待是异步 sleep，不阻塞线程。
- 缺什么证据：不缺（行号由 grep 确认）。

## G8 · session_persistence/sessions.rs:259 的重试 —— 已确认存在（有预算与缓解）

- 路径:行号：`crates/hermix-agent/src/session_persistence/sessions.rs:259`（`std::thread::sleep(sleep);`）。
- 机制说明：SQLite 写冲突（`SQLITE_BUSY` / `database is locked`）时的应用层重试，睡眠 20-150 ms 抖动，最多 15 次，但受 `WRITE_RETRY_BUDGET`（2 s）总预算封顶；调用点用 `run_blocking` 包住，MultiThread runtime 上走 `block_in_place`，worker 交回阻塞池。
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
  const WRITE_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(2_000);
  ```
  ```rust
            tokio::task::block_in_place(f)
  ```
- 判定：已确认存在。阻塞线程归属：sidecar 进程内的 tokio worker（`run_blocking` → `block_in_place`）；桌面主进程无此调用链（`apps/desktop-tauri/src-tauri/Cargo.toml` 依赖列表中没有 `hermix-agent`）。
- 缺什么证据：sidecar 二进制与 `hermix-server` 的构建对应关系未逐行核对（标推测）；触发 `SQLITE_BUSY` 的真实并发场景未复现。

## G9 · win_job.rs:336 的等待 —— 未发现（测试代码）

- 路径:行号：`crates/hermix-core/src/win_job.rs:336`。
- 机制说明：该行是 `#[cfg(test)] mod tests`（模块起于 `win_job.rs:259-260`）里的 `std::thread::sleep(Duration::from_millis(500))`，属于测试断言前的等待。
- 证据片段：
  ```rust
      assert!(guard.is_some(), "带默认 worker 限额的附着应当成功");
      std::thread::sleep(std::time::Duration::from_millis(500));
      assert!(
          is_process_alive(child.id()).unwrap_or(false),
  ```
- 判定：**未发现**生产阻塞点 —— 命中位于测试模块，不在运行路径。
- 缺什么证据：不缺。

## G10 · file_lock.rs:156 的等待 —— 已确认存在

- 路径:行号：`crates/hermix-cron/src/file_lock.rs:156`（`acquire` 内的退避重试）；退避间隔常量在 `crates/hermix-cron/src/file_lock.rs:56`，timeout 上界判断在 `:146-152`。
- 机制说明：文件锁被别的进程占住时，每 100 ms 退避重试一次，直到调用方给的 `timeout` 到期返回 `LockError::Timeout`；等待有上界，不是无限忙等。
- 证据片段：
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
- 判定：已确认存在（同步等待，有 timeout 上界）。阻塞线程归属标**推测**：cron 调度线程；本轮未定位 `hermix-cron` 在桌面主进程或 sidecar 中的调用点，也未确认调用时是否在 UI 可感知线程上。缺什么证据：`hermix-cron` 的 `acquire` 调用点位置与所在进程/线程。
- 缺什么证据：如上（调用点未核）。

## G11 · hermix-mcp/transport.rs 的两处 sleep —— 未发现（测试代码）

- 路径:行号：`crates/hermix-mcp/src/transport.rs:1021`（`wait_dead` 辅助函数内）与 `crates/hermix-mcp/src/transport.rs:1148`（`#[tokio::test]` 用例内）。
- 机制说明：两处 `std::thread::sleep(Duration::from_millis(100))` 都在测试代码里 —— 前者是等子进程退出的测试辅助函数，后者是等孙进程出现的前置条件等待。
- 证据片段：
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
- 判定：**未发现**生产阻塞点 —— 两处命中都在测试代码，主清单「未完成项 4」把它列为待核后可销案。
- 缺什么证据：不缺。

## G12 · codeHighlight.ts 的语法高亮本体 —— 已确认存在（与 G4 同一处代码）

- 路径:行号：`apps/desktop-tauri/src/lib/codeHighlight.ts:383`（`export function highlightToReact`，grep 确认）。
- 机制说明：refractor 同步分词 + `hast-util-to-jsx-runtime` 转 React 节点，失败时降级返回原文；长尾语言按需加载，未就绪时返回纯文本。
- 证据片段：
  ```ts
  export function highlightToReact(
      source: string,
      language: string | null,
      isDark: boolean = true,
      inline: boolean = false,
  ): React.ReactNode {
      if (!source) return null;
      if (!language) return source;
  ```
- 判定：已确认存在。**去重提示**：主清单把这一处同时计入检索面 d（第 4 项）与检索面 e（唯一项），实际是同一段代码，统计时不应算两处。
- 缺什么证据：同 G4 —— 耗时只有源码注释自述值，未实测。

---

## 未核与缺证（不补猜测）

1. **所有毫秒级耗时未实测**。本轮禁止启动应用、跑测试与构建，条目中的耗时只有源码注释自述值（`webview2_probe.rs:111-112` 的「~5ms」、`codeHighlight.ts:381-382` 的「~8 ms for 1000 lines of Python」）。要定量需要一次可复现的测量。
2. **G5 的工具输出长度上限未找到**：未在主进程侧找到工具输出写回与截断代码，无法判定 JSON.parse 是否可能处理大文本。缺：工具输出写回路径的实现位置。
3. **G3 / G4 / G12 的消息与代码体量上限未找到**：未找到 markdown 消息或代码块的截断上限，无法给出触发阈值。缺：截断代码位置。
4. **G10 的调用链未定位**：`hermix-cron` 的 `file_lock::acquire` 调用点与所在进程/线程未确认，线程归属标推测。
5. **G8 的进程归属为推测**：sidecar 二进制与 `hermix-server` crate 的构建对应关系未逐行核对。
6. **streamEvents.ts 的行号未二次 pin**：G1 给出的是区间 55-56（读到的区间 40-69 内），主清单标 55；两者不冲突，但精确行号未用 grep 复核。

## 检索面统计修订建议（供主清单下一版使用）

- 检索面 d（前端大文本处理）：5 个命中点全部核对完毕，其中 4 个（G1、G2、G5、G12）输入体量受限或不构成阻塞，G3、G4 属轻度同线程重活；面内 4 个命中点与 e 面命中点存在重复计数（codeHighlight.ts）。
- 检索面 e（渲染层重活）：唯一命中点 G4 确认存在，线程为 WebView2 渲染线程，量级按源码注释约 8 ms / 1000 行 Python。
- 检索面 f（主进程主线程重活）：4 处命中核对完毕，只有 `commands.rs:200` 所在同步命令真正落在主进程主线程，其余 3 处在 tokio worker —— 结论与修正要求见 `docs/blocking-hang-audit-review.md` 的发现 1、3、4。
- 检索面 b（忙等与轮询）：3 处命中核对完毕，`sessions.rs:259` 成立（见 G8），`win_job.rs:336` 与 `file_lock.rs:156` 分别落在测试代码与有界退避（见 G9、G10）。
- 检索面 c（同步 IPC）：主清单记 0 命中，本轮未新增核对，不作改动。
