# Decision Note: 系统资源管理重构 —— 垃圾扫描 / 大文件清理 / 文件索引

Status: implemented — 查看器「系统资源管理」面板重构为「磁盘清理（默认）/ 文件索引 / 进程」三页签；磁盘清理默认进垃圾扫描、按类别分组并逐项明示清理依据，点磁盘进入「建议清理 + 大文件」视图；索引 helper 改为 GUI 子系统静默运行，体积不再显示 0，建索引进度改为真实数字

## Decision

把 `apps/viewer` 的「系统资源管理」面板从"进程 + 一个混杂的清理页"重构成三块彼此独立的页面，代码按页拆文件（`apps/viewer/src/system/{CleanupPane,IndexPane,ProcessPane}.tsx` + 共享的 `types.ts` / `format.ts`），`SystemPanel.tsx` 只剩弹窗外壳与页签。

### 1. 磁盘清理默认进「垃圾扫描」，不做全盘扫描

打开清理页即跑一次**规则驱动**的垃圾扫描（`cleanup.rs` 的 `RULES` 表），秒级出结果；全盘遍历只有用户**点进某块磁盘**时才发生。

规则表从 11 条扩到 22 条，分六类：系统与临时 / 浏览器与网络 / 日志与转储 / 残留安装包 / 开发缓存 / 回收站。每条规则带三个字段：

- `group` —— 分类，UI 按它分组；
- `hint` —— **清理依据**（为什么这东西可以删）；
- `source` —— **规则出处**（BleachBit / CCleaner / kondo 的同名规则）。

UI 上每条清理项都显示"依据 + 出处"两行，不显示用户无从判断的东西。

顺带修掉一个真 bug：旧实现只在路径**末段**处理 `*`，于是
`%LOCALAPPDATA%\Google\Chrome\User Data\*\Cache\*` 这种中间段带通配的规则
展开成了一个字面路径，`read_dir` 直接失败——**"浏览器缓存"那条规则一直是
0 字节**。现在通配逐段展开（`expand_template` → `Target::{Dir,Glob,File}`）。

### 2. 点磁盘 → 大文件清理视图，结果分两块

盘内扫描（`drive_scan.rs`）的产物按主流清理工具的交互逻辑重排：

- **建议清理**：规则命中的可删除项（构建产物 / 临时残留 / 日志与转储 /
  残留安装包），每项带 `reason`（凭什么说它是垃圾）+ 删除方式
  （整目录 vs 单文件）；
- **大文件**：>200MB 的文件，**仅作参考**，按体积排序，不预判可删。

两块默认都不勾选，删除仍需二次确认。

### 3. 盘内扫描并行化

目录遍历改成 2–8 个工作线程共用一个目录队列（`Mutex<VecDeque>` + `Condvar`），
体积聚合从"递归后序回调"改成"各线程记直属体积 → 自底向上按深度累加"。
单线程遍历一块数据盘要 40s+，并行后是几秒——用户点开磁盘视图最在意的就是这个等待。

### 4. 索引 helper 静默运行，不再弹黑窗

`hermix-filesearch` 改用 `#![cfg_attr(windows, windows_subsystem = "windows")]` 编译：
GUI 子系统**根本不申请控制台**，从根上杜绝"每次拉起弹一个黑色命令行窗口"。
代价是没有 stderr——Rust 的 `eprintln!` 对写失败是 **panic**，所以所有诊断改写
`%LOCALAPPDATA%\hermix\filesearch\filesearch.log`（新增 `logger.rs`）。
拉起侧同时把 `SW_SHOWNORMAL` 改成 `SW_HIDE`（viewer 与 hermix-server 各一处），
作为提权链路（consent.exe → CreateProcess）下的双保险。

### 5. 体积不再显示 0，进度是真实数字

- `IndexEntry.size` 里本来就有 MFT `$DATA` 的真实字节数，但 `to_file_entry`
  写死 `size: None` → 搜索结果永远显示 0 B。现在按索引里的值透出，
  结果集补全那一步也就不会再为这些文件做多余的 `stat`。
- 建索引进度改成**分阶段真实数字**：枚举 MFT（条目数）/ 读取体积（已读字节）/
  建表 / 落盘，随 `status` 一起报给 UI；`BuildIndexSummary` 增加四段耗时，
  落进日志，瓶颈一眼可见。

### 6. 缓存复用与增量更新

沿用 Everything 的 db+journal 模型：快照装载（秒级可搜）+ USN Journal 增量保鲜，
只有 Journal 失效才全量重建。新增 `rebuild` 协议方法与
`viewer_filesearch_rebuild` 命令：标记某卷需要重建，监听循环自己动手，
重建期间旧索引继续可搜。

体积读取这一段去掉了一个百万级的 `HashMap<FRN, size>`：调用方的记录顺序与
待读 FRN 列表一一对应，按下标对齐写回即可，省掉百万次哈希插入 + 查找。

## Alternatives considered

1. **保留"先全盘扫描、再谈清理"的流程** —— 拒绝：打开清理页要等 40–45 秒，
   而规则垃圾（临时文件、缓存、日志、安装包残留）本来就是秒级可得的。
   先给确定的答案，用户想深挖再点磁盘。

2. **盘内扫描改用 MFT 索引算**（索引里每个文件的体积都有，还带 USN 保鲜）——
   拒绝：索引的默认过滤规则**排除** `node_modules` / `target` / `.git`，
   而这些恰恰是"建议清理"里最大的一块。要用索引算清理候选，就得再维护一份
   不过滤的索引——那是另一个量级的工程，且与"搜索索引"职责混淆。

3. **自己写 `\$MFT` 批量解析器**（WizTree 式：顺序读 MFT，一次拿到全部记录
   与体积，替掉"枚举 MFT + 逐文件 IOCTL"）—— **推迟，未做**。
   理由：这次改动的执行环境**没有管理员权限**，无法读 MFT 做验证；
   而 NTFS 内部结构解析一旦有偏差，是**静默污染整个索引**（路径错、体积错），
   比慢更糟。已把耗时拆成四段落日志，等有 MFT 访问权的机器再上。
   代价：建索引速度的提升到此为止——主要瓶颈（逐文件 `FSCTL_GET_NTFS_FILE_RECORD`）
   仍在。

4. **只把控制台窗口藏起来（`SW_HIDE`），不改子系统** —— 拒绝：
   `ShellExecuteExW("runas")` 走 UAC 提权链路（consent.exe → CreateProcess），
   `wShowWindow` 是否透传到目标进程不可靠；GUI 子系统是根治。

5. **保留控制台子系统、继续用 `eprintln!`** —— 拒绝：GUI 子系统没有 stderr，
   `eprintln!` 写失败会 panic，一个诊断语句就能打死索引服务。

6. **把系统资源管理挪到桌面端** —— 拒绝：进程 / 磁盘 / 索引都是本机能力，
   而查看器是独立 sidecar；`hermix-server` 侧也有一份同样的 helper 客户端，
   挪过去等于让查看器依赖主应用的 HTTP 面。

7. **顺手把「进程」页删掉**（用户说"分为两大功能"） —— 拒绝：用户要的是
   新增两大功能，没让删掉第三块。进程页保留，只是挪进同一面板的第三个页签。

8. **清理依据写成一段自由文本** —— 拒绝：依据要**可核对**。所以是
   "依据（hint）+ 出处（source）"两段：依据说清为什么，出处说清这是谁定的规则。

## Consequences

- **建索引速度的提升有限**：本次消掉了百万级哈希表、加大了读体积的线程数
  （按逻辑核数，4–16），并让耗时可观测；但逐文件读 file record 这个主要瓶颈
  仍在（见「Alternatives considered」第 3 条）。
- **大文件仍然可以被勾选删除**："仅作参考"指的是**不自动判定可删、默认不勾**，
  不是禁删——WizTree / 磁盘分析类工具也都允许手动清理大文件。UI 上这一块
  有明确说明，且与大文件同级的"建议清理"块给出了规则依据。
- **规则表变长意味着命中面变大**：新增的 7 条（字体缓存 / 网络缓存 /
  Windows 诊断日志 / 残留安装包 / npm / Yarn / pnpm / pip / Cargo / Gradle 缓存）
  会命中更多文件。误判风险由三层兜底：每项明示依据与出处、危险项（更新缓存）
  默认不勾、删除需二次确认且服务端重新扫描复核。
- **开发缓存类规则会让人意外**（npm / pip / Cargo 缓存删了要重新下载）。
  依据行里写明了这一点，且整组默认勾选——这是"垃圾扫描"该做的事。
- **盘内体积仍跳过系统目录**（Windows / Program Files / 回收站）：
  它们的体积对用户的清理决策没有参考价值。
- **索引 helper 不再有控制台**：调试从
  `%LOCALAPPDATA%\hermix\filesearch\filesearch.log` 看；
  `--status` / `--stop` 在有控制台时挂到父进程控制台输出，没有就只落日志。
- **快照格式不变（v3）**，旧快照继续可用；新增的 `indexedBytes` 与
  `build` 是运行时字段，不进快照。
- **索引服务要管理员权限**（读 MFT），"启动索引服务"会弹一次 UAC——
  这是 NTFS 的硬性要求，不是本设计的取舍。

## 验证

- `cargo test -p hermix-filesearch --lib`：25 passed
- `cargo check -p hermix-viewer --all-targets`：exit 0
- `apps/viewer tsc --noEmit`：exit 0
- `cargo check --workspace --all-targets` + `cargo test --workspace --lib` + `ci-gate.sh`：见同批提交
