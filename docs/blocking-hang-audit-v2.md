# 阻塞型卡死点清单 v2（按复核件修正）

依据：`docs/blocking-hang-audit-review.md`（逐条判定）、`docs/blocking-hang-audit-gaps.md`（检索面 d/e 结论）。
行号取自复核件，并用 `read_file` / `grep` 逐条复核过。原 `blocking-hang-audit.md`、复核件、补漏件与所有源码均未改动；未启动应用、未跑测试、未构建。
凡未实测的耗时一律标「未实测」；tokio worker 与 WebView2 渲染线程不写成主进程主线程。

## 1. 同步整文件读 + base64 编码（read_artifact_file / read_artifact_file_bytes）

- 位置：apps/desktop-tauri/src-tauri/src/commands.rs:671（`pub async fn`）、:675（2 MiB 上限）、:723（整文件读）、:742-743（base64 编码）；同型第二处 commands.rs:764（`pub async fn read_artifact_file_bytes`）、:768（10 MiB 上限）、:812（整文件读）、:813-814（base64 编码）
- 阻塞机制：`std::fs::read` 把整个文件读进内存，`base64::engine::general_purpose::STANDARD.encode` 对全量字节编码；命令体是 async fn 且未包 `spawn_blocking`，同步读与编码都在命令体内跑完
- 线程归属：tokio worker（Tauri async runtime 的 worker），非主进程主线程；Tauri v2 把 async command 调度到 async runtime 属框架行为，标推测；仓库内旁证 commands.rs:944-952、crates/hermix-agent/src/session_persistence/mod.rs:71-72
- 界面表现：占住一个 worker，被影响的是同一 runtime 上其它命令排队与延迟；未实测毫秒数；原「标题栏拖动变灰、窗口整体无响应」对本条不成立
- 触发条件：读取接近上限的大文件；`read_artifact_file` 单文件 2 MiB 封顶，`read_artifact_file_bytes` 单文件 10 MiB 封顶

## 2. 启动阶段同步子进程调用 reg.exe（WebView2 探测）

- 位置：apps/desktop-tauri/src-tauri/src/webview2_probe.rs:125（`Command::new("reg")`）、:108（两个注册表视图的循环）、:90-98（`probe()` 与 `CACHED_STATUS` 缓存）、:72（缓存常量）；唯一调用点 apps/desktop-tauri/src-tauri/src/lib.rs:911（`setup` 闭包内）
- 阻塞机制：`Command::new("reg").args(["query", &key, "/v", "pv"]).output()` 同步等待子进程退出，每个注册表视图一次、最多两次；webview2_probe.rs:111-112 注释自述子进程启动成本约 5 ms
- 线程归属：主进程主线程（应用 `setup` 阶段，Tauri 事件循环启动前）；`probe()` 是同步 fn，调用点在 `setup` 闭包内，「setup 在主线程、事件循环启动前执行」属框架行为，标推测
- 界面表现：启动阶段主线程被同步子进程占用；约 5 ms 是源码注释自述值，未实测；原「窗口空白/无响应数秒」与源码自述冲突，已删除
- 触发条件：仅 `CACHED_STATUS` 缓存未命中时发生（缓存命中直接返回）；进程生命周期内只发生一次

## 3a. 同步命令 list_backend_log_files 在主进程主线程枚举日志目录

- 位置：apps/desktop-tauri/src-tauri/src/commands.rs:194-200（同步命令 + `std::fs::read_dir`）
- 阻塞机制：同步 `fn` 命令体里 `std::fs::read_dir(&dir)` 枚举单个日志目录，对每个条目再取 `metadata()`；逐条只取元数据，不递归子目录
- 线程归属：主进程主线程（Tauri v2 同步 command 在主线程执行，属框架行为，标推测）
- 界面表现：枚举期间主线程被占，同步命令执行期间该主线程不处理其它 IPC；未实测毫秒数；原「整文件读 + 逐行读尾部在主线程阻塞」的说法已删除
- 触发条件：日志目录条目数量多，或日志目录位于网络盘/同步目录

## 3b. read_backend_log_tail 的尾部读取（已在 spawn_blocking 内，不占主线程）

- 位置：apps/desktop-tauri/src-tauri/src/commands.rs:253-257（async 命令 + `tokio::task::spawn_blocking`）、:261-263（`LOG_TAIL_CHUNK` 64 KiB / `LOG_TAIL_MAX_BYTES` 4 MiB）、:265（阻塞实现）、:302-303（`File::open` + 流式计数）、:325-330（从文件尾按块回读）
- 阻塞机制：从文件尾按块回读，单次回读总量封顶 4 MiB；总行数用流式计数得到，常数内存，不逐行读全文件
- 线程归属：阻塞线程池（`spawn_blocking`），不占主进程主线程，也不占 tokio worker
- 界面表现：不构成主线程阻塞；未实测毫秒数
- 触发条件：请求较大 `tail_lines` 且日志文件体积大时；回读上限 4 MiB 封顶

## 4. 单层目录枚举（list_directory）

- 位置：apps/desktop-tauri/src-tauri/src/commands.rs:1098-1099（`pub async fn list_directory`）、:1133-1134（`std::fs::read_dir`）
- 阻塞机制：命令体内一次 `std::fs::read_dir(&canonical)` 同步枚举目录，未递归进子目录；async 命令体未交给 `spawn_blocking`
- 线程归属：tokio worker（async 命令体未交给 `spawn_blocking`），非主进程主线程；依据同条目 1（commands.rs:944-952、session_persistence/mod.rs:71-72），Tauri 侧调度行为标推测
- 界面表现：同一 runtime 上其它 IPC 排队，窗口不一定整体无响应；未实测毫秒数
- 触发条件：含大量文件的目录、网络盘、OneDrive 同步目录

## 5. 忙等重试：session 持久化写冲突重试（SQLITE_BUSY）

- 位置：crates/hermix-agent/src/session_persistence/sessions.rs:259（`std::thread::sleep(sleep);`）；crates/hermix-agent/src/session_persistence/mod.rs:119-125（`WRITE_MAX_RETRIES` 15、`WRITE_RETRY_MIN_MS` 20、`WRITE_RETRY_MAX_MS` 150、`WRITE_RETRY_BUDGET` 2 s 总预算）、:127-134（`is_busy_error` 判定 `SQLITE_BUSY` / `database is locked`）、:81-96（`run_blocking` / `block_in_place`）；调用点 crates/hermix-agent/src/local_agent_service/mod.rs:865-871
- 阻塞机制：SQLite 写冲突（`SQLITE_BUSY`）触发的应用层忙等重试，`thread::sleep` 20~150 ms 抖动，最多 15 次，受 `WRITE_RETRY_BUDGET` 2 s 总预算约束；源码里没有文件锁，只有 SQLite busy 判定
- 线程归属：sidecar 进程内的 tokio worker（`run_blocking` 在 MultiThread runtime 上走 `block_in_place`，把 worker 交回阻塞池；单线程 runtime 直接同步执行）；主进程无该调用链（apps/desktop-tauri/src-tauri/Cargo.toml 不依赖 hermix-agent），sidecar 可执行文件由 hermix-server 构建属推测（未核对构建产物）
- 界面表现：影响 sidecar 自己的请求延迟，最坏 2 s；桌面主进程不出现「对话保存时输入框短暂无响应」；未实测毫秒数
- 触发条件：session 持久化写入时命中 SQLite busy（并发写同一库）；最坏受 2 s 总预算约束

## 检索面 d/e 结论（原清单其余命中项）

| 编号 | 位置 | 判定 |
|---|---|---|
| G1 | apps/desktop-tauri/src/app/streamEvents.ts:55 `JSON.parse` | 输入体量受限，不构成阻塞 |
| G2 | apps/desktop-tauri/src/App.tsx:212 `JSON.parse` | 输入体量受限，不构成阻塞 |
| G3 | apps/desktop-tauri/src/components/MarkdownBody.tsx:507 `md.split` | 轻度同线程重活，线程为 WebView2 渲染线程 |
| G4 | apps/desktop-tauri/src/components/MarkdownBody.tsx:269-270 同步语法高亮 | 轻度同线程重活，线程为 WebView2 渲染线程 |
| G5 | apps/desktop-tauri/src/components/bubbles/ToolOutputPreviews.tsx:32 `JSON.parse` | 输入体量受限，不构成阻塞 |
| G6 | apps/desktop-tauri/src-tauri/src/commands.rs:200、:723、:812、:1133-1134 | 4 处命中已核，归属见条目 1、3a、4 |
| G7 | crates/hermix-agent/src/llm_retry.rs:295 | 引用错位：真实等待在 :320 且为异步 sleep，判未发现 |
| G8 | crates/hermix-agent/src/session_persistence/sessions.rs:259 | 成立，即条目 5 |
| G9 | crates/hermix-core/src/win_job.rs:336 | 位于 `#[cfg(test)]` 测试模块，判未发现 |
| G10 | crates/hermix-cron/src/file_lock.rs:156 | 成立：100 ms 退避且有 timeout 上界；线程归属标推测 |
| G11 | crates/hermix-mcp/src/transport.rs:1021、:1148 | 位于测试代码，判未发现 |
| G12 | apps/desktop-tauri/src/lib/codeHighlight.ts:383 | 与 G4 是同一处代码，不另计一处 |

## 未实测项

- 条目 2：webview2_probe.rs:111-112 注释自述子进程启动约 5 ms，未实测。
- 条目 1、3a、3b、4、5：没有任何实测毫秒数（本轮禁止启动应用与跑测试，源码里也无耗时注释）。
- G4 / G12：codeHighlight.ts:381-382 注释自述「~8 ms for 1000 lines of Python」，未实测。
- G3、G10：规模与耗时均未实测。

## 未核/缺证

- G5 的工具输出字符串长度上限未找到（工具输出写回与截断代码未定位）。
- G3 / G4 / G12 的 markdown 消息与代码块体量上限未找到（截断代码位置未定位）。
- G10 的 `hermix-cron` `file_lock::acquire` 调用点线程与所在进程未定位。
- G8 的 sidecar 与 hermix-server 构建对应关系为推测（未核对构建产物）。
- 触发 `SQLITE_BUSY` 的真实并发场景未复现。
- Tauri v2 同步/异步 command 的线程调度属框架行为，仓库内无 Tauri 源码可 pin，条目 1、2、3a、4 的调度结论标推测。
