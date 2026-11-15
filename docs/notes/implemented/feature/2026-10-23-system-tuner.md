# Decision Note: 系统资源管理增强（后台进程自动清理 + 磁盘清理扩项 + AI 辅助清理）

Status: implemented — 2026-10-23 落地；新增后台进程自动清理（启发式打分 + 安全网 + 两级退出）、磁盘清理规则扩到 31 条并引入评分分档、AI 辅助清理建议层（LLM 只裁决、规则锁死禁区）。

## Decision

### ① 后台进程自动清理：**识别自动化，执行受控**

用户要求「进程管理模块要有一个自动清理后台进程的能力」。本仓库的实现形态是
**扫描 → 打分 → 列出候选与理由 → 用户确认 → 执行**，而不是"定时无条件杀"。

- **启发式打分**（`apps/viewer/src-tauri/src/procclean.rs`）：内存档位（≥500MB +40 /
  ≥200MB +25 / ≥100MB +10）、本轮无磁盘读写活动（+20）、几乎不占 CPU（+15）、
  无可见窗口（+15）、系统空闲 24h（+20）/ 30min（+10）、已运行 1h 以上（+5）。
- **安全网（硬门槛）**：系统关键进程（`IsProcessCritical`）、提权进程
  （`TokenElevation`）、服务宿主进程（`EnumServicesStatusExW`）、有可见窗口的进程、
  自身与自身祖先链、名单兜底（`PROTECTED_NAMES`，分词全等匹配）。
- **两级退出**：先 `WM_CLOSE` 优雅退出（1.5s 宽限），仍存活才 `TerminateProcess`。
- **执行时重跑安全网**：前端传什么 PID 都不能绕过服务端校验。

### ② 安全网重心放在「问系统」，名字名单只做兜底

`IsProcessCritical`（kernel32，Win8.1+）能直接问系统"这是不是关键进程"，
`TokenElevation` 能问"是不是提权进程"，`EnumServicesStatusExW` 能问"是不是服务
宿主"。这些判定零维护成本、权威；硬编码名单（`CRITICAL_NAMES` / 新的
`PROTECTED_NAMES`）只在系统 API 拿不到句柄时兜底。

名单匹配用 **分词全等**（`xterminal` 不算 `terminal`），沿用 Idlekiller 的做法，
避免子串匹配误伤。

### ③ 磁盘清理：规则表从 11 条扩到 31 条，并引入评分分档

新增分类：显卡（NVIDIA/AMD/Intel 着色器缓存）、开发工具（VS Code / npm / pip /
NuGet / Cargo / Gradle 缓存）、应用（OneDrive / Teams / Discord / Spotify / Zoom
日志）、游戏（Steam 下载缓存）、系统（传递优化 / INet / 字体 / Minidump /
CBS / Panther / Prefetch / 浏览器离线站点缓存）。

每条规则新增两个字段：
- `score`（0-100）：**本地评分**，纯缓存高分、涉及用户数据或需重新下载的低分；
- `recoverable`：删掉之后**怎么恢复**（"自动重建"/"自动重下"/"不可恢复（失去崩溃现场）"），
  不写"可恢复"这种含糊说法。

分档：`safe`（score ≥ 80，默认勾选）/ `review`（需人工看一眼）/ `risky`
（高风险，默认不勾、需二次确认）。

### ④ AI 辅助清理：**形态 B**（规则打分 + LLM 只裁决边界项）

形态对比与取舍（基于开源调研 + token 成本核算）：

| 形态 | 做法 | 结论 |
|---|---|---|
| A | 全量候选喂模型逐条决定 | 否：token 随候选数线性膨胀（1000 项 ≈ 80K–150K token/次），误删风险不可控 |
| **B** | **本地规则打分分档，模型只裁决边界项** | **采用**：禁区由本地规则锁死；没有 AI 时规则评分仍独立可用 |
| C | 让模型生成清理脚本 | 否：等于把任意代码执行权交给模型，无法做白名单校验（与 AGENTS.md「信任边界校验」不可削减项冲突） |

实现（`apps/viewer/src-tauri/src/cleanup_ai.rs`）：
- 只送**结构化清单**（id/标题/分类/体积/文件数），绝不把扫描日志原文喂模型；
  候选按体积取前 60 条，超出显式告知模型已截断；
- 模型输出经服务端**逐条校验**：id 必须在候选集内、decision 只能是
  `clean`/`keep`/`unsure`、confidence 夹到 [0,1]、reason 截断到 60 字；
- **本地标记 risky 的项，模型即使说 clean 也不采纳**（越权建议丢弃并计数，
  界面上如实告诉用户"模型有 N 条越权建议，已拦截"）；
- `unsure` 一律不自动勾选；
- 解析失败 → **无声降级**：界面不显示任何 AI 勾选（不能让用户误以为 AI 认可了什么）。

### ⑤ 上下文纪律：AI 清单遵守「零上下文膨胀」红线

AGENTS.md 的零膨胀原则对 AI 同样适用：送进模型的是**结构化清单**（每条一行
JSON），不是原始扫描输出；条数有上限；模型回复有 token 上限与长度截断。

## Alternatives considered

- **定时无条件清理后台进程**（类似 Idlekiller 的批量终止）：在 Windows 上
  「后台进程」与「服务 / 杀软 / 更新器 / 安装器」的边界极模糊，误杀代价是蓝屏或
  杀软自杀，**风险与收益比不成立**。故只做"自动识别 + 用户确认"。待积累真实
  误判数据后再考虑放开低风险档的一键清理。
- **一键全清（不做安全网）**：否。安全网是"自动清理"唯一的真风险点。
- **把 `CRITICAL_NAMES` 扩成大而全的黑名单**：名单会随软件生态腐化而漏杀，
  且维护成本高；改用系统 API 判定。
- **按进程树整体结束**（System Informer 的 `PhUiTerminateTreeProcess`）：会连带
  结束子进程，而子进程里可能有用户正在用的东西。本期不做整树结束，只做单进程。
- **自建 MFT 扫描器做重复文件检测**：本仓库已有 `hermix-filesearch` 的 MFT/USN
  索引，重复文件、大文件、旧文件应挂在它上面，不要再造一套遍历器。
- **引入 ClamAV 做病毒扫描**：签名库体积大、更新成本高，Windows 上 Defender
  已覆盖；只做"调用 Defender 扫描"（`MpCmdRun -Scan -ScanType 3 -File`），
  且只扫不自动删。
- **用 WinDivert 做网络拦截**：要加载内核驱动，会被安全软件标记，与本项目
  "零安装、免驱动"定位冲突。只做网络诊断与修复建议，不做拦截。
- **内核级弹窗拦截（类火绒）**：需要全局 hook / 驱动，同上。只做基于
  `EnumWindows` 的窗口级自动关闭。

## Consequences

- 新增模块：`apps/viewer/src-tauri/src/procclean.rs`（后台进程扫描/执行）、
  `apps/viewer/src-tauri/src/cleanup_ai.rs`（AI 建议层）。新增 Windows 依赖 feature：
  `Win32_System_Services`、`Win32_System_SystemInformation`、`Win32_UI_Input_KeyboardAndMouse`。
- 磁盘清理规则表变大后，单次扫描的**墙钟预算仍是 8 秒**（`SCAN_BUDGET`）——
  规则多不代表能扫得更久，超预算的后半部分如实报 0，界面必须说明"结果可能不完整"。
- AI 建议层依赖主程序的 AI 配置；未配置时按钮置灰并显示原因（`viewer_ai_status`
  已提供中文原因），**清理功能本身不依赖 AI**。
- 进程"可释放内存"用 `sysinfo` 的 `memory()` 口径（Windows 上是工作集口径），
  与 `GetProcessMemoryInfo.PrivateUsage` 可能有出入，界面上标注为"约"。

## Reintroduction conditions

- **重复文件 / 大文件 / 旧文件清理**：等 `hermix-filesearch` 的 MFT 索引稳定、
  且能提供稳定的"按体积分组 + 哈希"查询后，接入 `drive_scan` 的清理流水线。
- **低风险档一键清理**：积累 ≥ 一次真实机器上的误判样本（哪些进程被误判进建议档）
  后再评估；若误判率 < 1% 且无一例服务/杀软误伤，可放开"一键清理低风险项"。
- **隔离区 + 撤销**：清理项增加"移到隔离区、7 天内可移回"（同卷 `MoveFileExW`），
  用于让高风险项也敢一键执行。
