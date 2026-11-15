# Decision Note: 集成 FilePulse 后端作为本地文件搜索

Status: implemented — 2026-10-14 交付，2026-10-15 转入 implemented
Archived: 2026-10-15

Archived 原因：P1/P2/P3 全部交付（fdabe182 / b284d54d / 6788d464 / 67e7ebd9），从 proposed 转入 implemented。
Class: feature

> 交付修订（2026-10-14 深夜实测）：提权 helper 与宿主之间**无法用 stdio 管道**
> （ShellExecuteExW 跨提权边界不支持句柄继承），改为 127.0.0.1 回环 TCP +
> token 握手（endpoint.json 发现）。另实测本机 `GetVolumeInformationW` 偶发
> 永不返回（~50%），list_volumes 已改每卷独立探测线程 + 5s 总超时。

## Decision

以「vendor 后端 + 丢弃前端」方式集成 [FilePulse](https://github.com/ning696/FilePulse)（已克隆到 `refs/FilePulse/`）。其 Rust 后端（src-tauri，2378 行）分层良好：core/ 六模块基本纯 Rust、有单测；commands.rs/monitor.rs 是薄 Tauri 壳。抽样结论来自逐文件评审（耦合点、行号见下文引用）。

### 架构总览

```
[desktop 前端] --apiInvoke(stdio 桥)--> [hermix-server] --stdio JSONL--> [hermix-filesearch.exe (独立、提权)]
                                              ^                                  |
                                              └── 快照读写 %LOCALAPPDATA%\hermix\filesearch\
```

### 1. Vendor 范围（crates/hermix-filesearch，library crate）

原样搬入（不改）：

| 文件 | 行数 | 说明 |
|---|---|---|
| core/index.rs | 506 | SearchIndex：内存索引 + USN 变更应用 + 路径重建 |
| core/model.rs | 145 | FileEntry（含 size/modified/is_dir）、快照结构 |
| core/search.rs | 170 | 线性子串搜索 + 排序，含单测 |
| core/metadata.rs | 107 | 文件 size/modified 补全（MFT 记录不含大小） |
| core/windows_ntfs.rs | 510 | Win32 FFI：MFT 枚举 + USN 读取 + 卷枚举 |
| core/scanner.rs | 153 | walkdir 扫描（原项目死代码，**我们复活它做降级路径**） |

不搬：serde_json / thiserror（死依赖）、tauri-plugin-*（前端用）。

### 2. 去 Tauri 化（最小机械改动，约 60 行，不动算法）

- `core/storage.rs`：全部函数的 `&AppHandle` 参数 → `&Path`（数据目录由调用方传入）。耦合点仅一处：`app.path().app_data_dir()`（storage.rs:57/64）。
- `monitor.rs`：`AppHandle` → `Arc<AppState>` 直传；`app.emit` → `tokio::sync::broadcast::Sender`；`tauri::async_runtime` → `tokio`。Tauri 在此只提供运行时/状态/事件三件事，均为机械替换。
- `AppState`（commands.rs:21-28，本身零 tauri 类型）移入 crate。
- 5 个 Tauri command 不搬——宿主侧重写为薄壳。

### 3. 多卷支持 = wrapper 组合，不改 core

FilePulse 的 `SearchIndex` 是单卷硬编码（`volume_state: Option<VolumeIndexState>`，file_ref 是卷内概念，跨卷撞 key）。不做 core 重构：wrapper 层持 `HashMap<volume, SearchIndex>`（每卷一实例 + 一快照，storage 本就按卷分文件），搜索 fan-out 各卷、合并排序后返回。自动全盘索引 = 对 `list_volumes()` 返回的全部 `is_supported` 卷循环建索引。

### 4. 过滤规则（唯一动 core 的新增代码，约 100 行 + 测试）

FilePulse **完全没有过滤机制**（$RECYCLE.BIN、System Volume Information、页面文件全入索引）。硬需求，做法：

- 新增 `FilterRules` 结构（model.rs）：排除目录名列表、排除扩展名列表、跳过隐藏+系统位文件（FILE_ATTRIBUTE 判定）、跳过 pagefile/hiberfil/swapfile 前缀。
- 索引期过滤：传给 `build_entries_from_records`（index.rs:278-307，构建源头）与 `apply_changes`（index.rs:143-229，增量入口）——**两处必须同一谓词**，否则被排除文件一有 USN 事件就回流。目录排除按名字匹配（MFT 记录无"系统"标志，`System Volume Information` 只能按名滤）。
- 默认值 + 用户可配置（见 §7）。

### 5. 进程形态：独立 exe（关键决策）

MFT 枚举与 USN 读取都要求管理员权限（`\\.\C:` 卷句柄 + backup privilege，windows_ntfs.rs:389-410）。FilePulse 的解法是让**整个应用**以管理员启动（manifest `requireAdministrator`）——它作为独立 app 可以这么要求，hermix 不能。

因此 FilePulse core 装进一个**独立 exe：`hermix-filesearch.exe`**：

- **搜索引擎 100% 是 FilePulse 的代码**（core 六模块 vendor + §2 的去 tauri 化 + §4 的 FilterRules）。
- **完全独立可运行**：自己双击就能跑全盘自动索引 + USN 保鲜 + 快照落盘，不依赖 hermix 主程序；带最小 CLI（`--volumes C:,D:` / `--status`）。
- 对外只通过**继承的 stdio 管道**提供搜索结果（JSONL），不开网络监听——提权进程暴露端口是安全面扩大。
- hermix 桌面端只是「使用方」：启动时 `ShellExecute "runas"` 拉起它（每会话至多一次 UAC），经 stdio 查询。拉起用的 UAC 样板（约 100 行 ShellExecute + 进程存活监测）是新写的 glue，与搜索引擎无关；已删模块 git 历史里有等价实现可参考，仅省事用，非功能依赖。

### 6. 无感知索引（客户视角）

1. app 启动 → 先加载各卷快照（bincode，FilePulse 已有原子落盘 + 30s 节流 + 退出强刷）→ **秒级可搜**（哪怕 UAC 还没点）。
2. 拉起 helper：校验快照新鲜度，过期卷后台重建；新鲜卷只做 USN 增量保鲜。
3. 用户拒绝 UAC / helper 起不来 → 旧快照继续可用，UI 明示「实时同步未开启」，不静默装死。
4. USN journal 失效（FilePulse 判定 journal_id 变化 → NeedsRebuild 后停下等手动重建）→ wrapper 改为**自动重建**，重建期间旧索引照常服务。

**卷级超时看门狗**：本机实测 F: 卷 `DeviceIoControl` 永不返回（管理员态也被拒，上一轮模块的现场教训）。每卷独立建索引线程 + 超时放弃 → 标记跳过（7 天清单）或转 walkdir 降级（scanner.rs 复活，同样产出 FileEntry）。这条继承自已删模块的三层根治经验，属于宿主 wrapper 逻辑，不改 FilePulse core。

### 7. 配置项（用户要求）

存 hermix 本地设置，照 `screenshot_apply_config` 范式（Tauri command + 设置页即时生效）：

- `enabled`（总开关）、`hotkey`（默认 Alt+F）
- `excluded_dirs`: 默认 `$RECYCLE.BIN, System Volume Information, node_modules, .git, target`
- `excluded_extensions`: 默认空（用户可加 `.tmp/.iso` 等）
- `skip_hidden_system`: 默认 true
- `volumes`: 默认全部 NTFS 固定卷（可勾选）

### 8. 前端（全新，FilePulse 前端不用）

- **全局快捷键**：复用 tauri-plugin-global-shortcut（Rust 侧注册范式照 screenshot_controller.rs:343-379，先注册新的再解旧的）。
- **独立窗口**：照 browser_view_controller.rs:79-123 范式（WebviewWindowBuilder + `#/file-search` 路由 + 几何记忆 + **additionalBrowserArgs 必须与主窗逐字一致**，否则 WebView2 0x8007139F）。
- **资源管理器列表**：文件名 / 路径 / 大小 / 类型(目录标记) 列 + 虚拟滚动；结果来自 server 端点（`/v1/filesearch/*` 注册进 axum router，apiInvoke 即达，零传输层改动）。
- **双击打开**：文件 → spawn `hermix-viewer.exe <path>`（viewer 已作为文件关联的 `%1` handler，commands.rs:1475 证实接受路径参数）；目录 → viewer 打开该目录。viewer 缺失时 fallback `revealItemInDir`。
- UI 遵循 impeccable-design 规范，改完跑 detect 门禁。

### 9. 实施分期

| 期 | 内容 | 验收 |
|---|---|---|
| P1 | vendor crate + 去 tauri + 多卷 wrapper + FilterRules + 超时看门狗 + 单测 | `cargo test -p hermix-filesearch`；模拟非管理员/坏卷路径 |
| P2 | 独立 exe（hermix-filesearch.bin 薄壳 + stdio 协议 + CLI）+ 提权拉起 glue + server 端点 | 手测：冷启动 UAC → 重建 → 重启秒加载；exe 单独双击可独立跑 |
| P3 | 快捷键 + 独立窗 + 列表 UI + 双击 viewer + 配置页 | tsc + impeccable detect + 手测全流程 |
| P4（可选） | agent `search_files` 工具接索引化 SearchBackend（hermix-tools/src/backends/file.rs:570） | cargo test |

## Alternatives considered

- **FilePulse 整包独立运行**（连它的 React 前端）：两个 Tauri runtime、两套窗口/托盘/更新链，无法共享配置与快捷键，且其前端正是被否的部分。否。
- **嵌入 hermix-server 进程内**：省一个 sidecar，但 MFT/USN 需要管理员，主程序整体提权不可接受；非提权下功能直接残废。否。
- **重写而非 vendor**：上一轮自研模块刚删（2026-10-13），FilePulse core 有单测、原子落盘、退避重试，质量不差；重写无增益。否。
- **改造 SearchIndex 支持多卷**：中等规模重构（分片、key 加卷前缀、快照加载重写），违背「少改人家代码」；per-volume 实例组合零改动。否。
- **搜索期过滤而非索引期**：零 core 改动，但百万条目快照 150-250MB 白吃磁盘内存，检索结果里永远混垃圾。过滤是硬需求，值得那 100 行。否。
- **Everything 式 Windows Service**：一次安装永久免 UAC，最无感；但要服务安装/生命周期/卸载清理（上一轮复杂度的主要来源）。暂缓，作为 P5 可选升级——FilterRules/helper 协议均不受影响。

## Consequences

- 代码量：+1 crate（约 2,400 行）+ helper 薄壳（约 400 行）+ 前端窗口（约 500 行），替换掉已删的约 3,600 行，净增可控。
- UAC：每会话一次（快照可让拒绝提权时依然可搜，只是不保鲜）。
- 失效模式（对抗性自查）：
  - helper 崩溃/被杀 → 搜索退化为快照静态数据，UI 显示状态，可重拉；不静默。
  - F: 类坏卷 → 看门狗超时放弃 + 跳过清单，C: 等正常卷不受拖累；绝不让一个卡死卷拖死整个索引线程。
  - USN journal 被清理 → 自动 NeedsRebuild，重建期间旧索引可用（FilePulse 原版会停摆，这是我们改的一处行为）。
  - 孤儿条目（parent_ref 失联 → 伪造根路径，index.rs:345-346）：上游缺陷，第一期接受（发生概率低、有后续 USN 事件自愈机会），不为此改 core。
  - `current_snapshot()` 整体 clone 的落盘内存尖峰：30s 节流下可接受，实测有压力再优化。
  - 搜索只匹配文件名、不支持路径/通配符/多关键词（上限 5000）：第一期接受，列筛选兜底；反馈强烈再做倒排/过滤参数。
  - 双 HashMap 死字段（by_file_ref/children_by_parent，白耗内存）：vendor 时顺手删除，属死代码清理非功能改动。
- 安全面：提权 helper 无网络监听，stdio 管道继承；过滤规则中用户配置的排除目录在索引期生效，无注入面。
- 官网文案残留（articles.js/changelog.html 提及已下线 usn-indexer）：随本功能上线一并更新。
