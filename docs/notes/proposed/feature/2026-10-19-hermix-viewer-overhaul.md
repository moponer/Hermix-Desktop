# Decision Note: hermix-viewer 双模式化与全能力重构（超长任务总纲）

Status: proposed — 方案已成形（2026-10-19），执行台账未开始，尚未产生代码改动

## Decision

把独立文件浏览器 `apps/viewer`（`hermix-viewer`）从「三栏只读资源管理器」升级为**双模式全能力查看器**：

- **纯净模式（light）** — 只保留预览面：被文件关联唤起（`HKCU` 登记的 `hermix-viewer.exe` 带路径启动）时进入。无侧栏、无资源管理器、无 AI 栏，只有一条细工具条（另存 / 打开方式 / 显示方式 / 切到重量模式 / 关闭）。目标是「双击一个文件就看到它」，进程与首屏都轻。
- **重量模式（heavy）** — 独立启动时进入：驱动器侧栏 + 目录资源管理器 + 多标签预览 + AI 协助 + **文件编辑与转换工具集**（图片处理、文档/PDF/文本编辑、格式转换）。产物一律另存，**原文件永不被覆盖**。

同时落地三件已定位根因的修复：

1. **启动卡顿** — `viewer_drives` 是同步 `#[tauri::command]`，内部对每个盘符做 `std::fs::metadata`（`apps/viewer/src-tauri/src/commands.rs:429-453`）。同步命令默认在主线程执行，而 `GetFileAttributesEx` 打在断开的网络映射盘上会阻塞到 SMB 超时（可达数十秒）。前端还把首屏路径挂在驱动器列表后面（`apps/viewer/src/App.tsx:207` 与 `:212-220` 两次调用 `drives()`，第二次拿到结果才 `setCurrentPath`）。→ 驱动器列表改为「异步 + 不做文件系统 IO + 不阻塞首屏 + 侧栏骨架/加载态 + 1.5s 超时降级」。
2. **全屏不平铺** — `apps/viewer/src/styles.css:304-306` 的 `.explorer { flex: 1; min-width: 280px; max-width: 460px; }` 把中栏宽度钉死在 460px，`.sidebar` 200px、`.ai-side` 320px 又是固定值；窗口没有任何全屏/最大化状态处理（`tauri.conf.json` 未启用，源码无 `fullscreen` 监听）。宽屏下剩余空间全是空底色。→ 三栏按比例铺满 + 可拖拽分隔条 + 全屏/最大化状态自适应。
3. **彩色类型图标** — 现状是 lucide 单色图标（`.entry-icon { color: var(--text-secondary) }`）。→ 建「扩展名 → 专属图标 + 专属色」的类型系统，文件夹有独立图标；桌面端 `FileAssocTab` 的关联后缀列表同步显示对应类型图标。

**归属**：`packages/preview`（`@hermix/preview`）继续是唯一渲染层，desktop 与 viewer 共用；类型判定、图标映射、渲染器样式归共享包，desktop 端 `code-view.css` 只留 `.code-*` 与桌面外壳（见 `scripts/check-preview-layer.mjs`）。编辑/转换的**计算内核**归 `apps/viewer/src-tauri`（Rust），交互面板归 viewer 前端。

**执行方式**：本篇既是立项记录，也是**执行台账**（见下「## 执行台账」）。执行 agent 以本篇为断点续跑的唯一真相源。

## Alternatives considered

1. **驱动器标签继续用 `std::fs::metadata`** —— 拒绝：这是启动卡顿的物理根因。改用 `GetDriveTypeW` / `GetVolumeInformationW` 判「本地磁盘 / 网络驱动器 / 可移动」不碰文件系统，标签文本随之降级（不再区分「本地磁盘」与「磁盘」的细微差别），换来的是断网映射盘不再冻结启动。
2. **首屏等驱动器就绪再渲染** —— 拒绝：盘符枚举是毫秒级的 IO，代价却是要等网络盘超时。改为首屏用兜底路径（上次访问目录 → 用户目录 → 首个可用盘）直接渲染，驱动器列表后到后替换。
3. **纯前端 Canvas 做全部图片处理** —— 备选但未全采纳：Canvas 交互与预览快、零依赖，但格式编解码覆盖面（尤其 ico 多尺寸打包、tiff、heic）与批处理弱。最终形态是「前端 Canvas 负责交互与即时预览，Rust 侧负责编码/封装与落盘」，Rust 侧只用 `image` 一类纯 Rust 库。
4. **引入 monaco / codemirror 做代码编辑** —— 拒绝：体量与编译/打包成本远超收益，且预览层已有 `CodeBlock` 渲染。纯文本编辑用原生 `textarea` + 现有 `textDecode` 编码探测即可。
5. **直接引入 vscode-icons / iconify 全量图标集** —— 未决，必须先量化：真彩色图标集体积大、许可各异；`lucide-react`（viewer 已装 `^0.511.0`）是单色，需要配色才能达到「彩色」诉求。执行时先调研三个方案的装包体积 / 首屏 chunk 影响 / 许可，再在台账里定案。
6. **纯净模式用 CSS 隐藏侧栏实现** —— 拒绝：隐藏的 DOM 仍会加载与订阅，纯净模式的意义就是轻。改为不渲染侧栏组件本身。
7. **编辑产物原地覆盖** —— 拒绝（用户硬要求 + 数据安全）：所有编辑/转换/AI 产物写到新路径（`<name>.edited.<ext>` / `<name>.ai.<ext>`），原文件字节不变，可用 sha256 前后比对验证。
8. **把「文件关联」从 desktop 搬到 viewer** —— 拒绝：关联登记是 `HKCU` 系统级写操作，已有 `file_assoc_list` / `file_assoc_set`（`apps/desktop-tauri/src-tauri/src/commands.rs`）在 desktop 侧。viewer 只做被关联唤起时的行为，登记仍归 desktop 设置页。
9. **一次性重写 viewer 全部代码** —— 拒绝：违反仓库「不堆屎山 + 六步决策阶梯」。按子任务逐个交付、逐个提交，任何一步都可 revert。

## Consequences

- **取代旧笔记？否。** 本篇是 `2026-10-13-hermix-viewer-resource-manager.md`（三栏形态 + 抽 `@hermix/preview` 包）与 `2026-10-09-standalone-file-viewer.md` 的**后续**，共享包与三栏决策仍以那两篇为准；本篇不推翻它们，只在其上加模式分层、编辑转换能力与三处修复。
- **依赖面变大**：预计新增 Rust 侧 `image`（图片编解码）、`lopdf`（PDF 结构级操作）两族纯 Rust 依赖；前端不新增大包（`lucide-react` 已在）。每一族都必须在立项笔记里写清「为什么不复用现有能力」与「对 `cargo check` 时长的实测影响」，超出预算则退回更轻方案（如 PDF 只做前端 pdf.js 侧的页操作 + 另存）。
- **代码量预算**：本任务新增前端 + Rust 源码合计目标 ≤ 6,000 行（不含测试与 fixture），单文件 ≤ 2,500 行（`ci-gate.sh` 硬告警）。超预算要写明理由并同步更新仓库基线。
- **编译速度**：`cargo check -p hermix-viewer` 当前约 9 分钟（首次），新增纯 Rust 依赖会带来编译期成本。执行时必须记录增量并对比，超出可接受范围（如 +60s 以上）时优先换方案。
- **viewer 尚不可打包分发**：`apps/viewer/src-tauri/tauri.conf.json` 的 `bundle.active = false`，它是独立开发态应用。本任务不负责打包发布，只保证 `cargo check` / `tsc` / `vite build` 三条链路绿。
- **风险点**：`packages/preview` 是 desktop 与 viewer 的共享面，改它会同时影响两个 app。共享层改动必须跑两端的 `tsc --noEmit` 与 desktop 的 vitest（当前 31 文件 / 297 用例）。
- **工作区不干净**：当前仓库有大量未提交改动（`git status` 可见数十个 `M`）。执行前必须记录基线，只提交本任务触及的文件，**禁止 `git add -A` / `git commit -a`**。

## Reintroduction conditions

- 若「彩色类型图标」最终选了轻量方案（lucide + 配色）而用户仍要求真彩色多色图标集，则在图标集体积与许可问题有解时重新引入。
- 若 PDF 结构级编辑（删页 / 插页 / 重排）被证明必须引入完整 PDF 渲染后端，则在 Rust 侧 PDF 栈落地时重新评估。
- 若纯净模式在实际使用中证明与重量模式差异不足以抵消复杂度，则合并回单模式 + 可折叠侧栏。

## 执行台账

**规则（执行 agent 必须遵守）**

1. 本表是**断点续跑的唯一真相源**。状态永远先于代码改动落盘：开工 → 把该项置 `doing` 并写当前心跳时间 → 再动代码 → 通过门禁并提交 → 置 `done` 并写 commit SHA。
2. 心跳时间格式 `YYYY-MM-DD HH:MM`（本地时间）。看门狗以「心跳距今 > 30 分钟且状态仍为 doing」判定主会话已中断。
3. 每次开工第一步：读本表 → `git log --oneline -5` → `git status --short`，确认从哪个点续。
4. 每完成一个子任务立刻更新本表，不要攒到最后。
5. 表格行只增不减（保留历史），状态只走 `pending` → `doing` → `done`。

| # | 子任务 | 状态 | 心跳 | 提交 SHA |
|---|--------|------|------|----------|
| F0 | 基线记录（门禁现状 / 代码行数 / 编译耗时 / 脏树快照） | done | 23:38 | 106bd5f2 |
| F1 | 启动卡顿：驱动器列表异步化 + 侧栏加载态 | done | 23:52 | 106bd5f2（Rust）+ 93c6c499（前端） |
| F2 | 全屏/最大化平铺 + 可拖拽分隔条 | done | 23:45 | 84d126a9 |
| F3 | 彩色类型图标系统（列表 + 关联后缀 + 文件夹） | doing | 23:48 | - |
| F4 | 双模式：纯净模式 / 重量模式与切换 | pending | - | - |
| F5 | 重量模式：图片处理工具集 | pending | - | - |
| F6 | 重量模式：文本 / 文档 / PDF 编辑与转换 | pending | - | - |
| F7 | AI 协助：一键入对话 + 改图提示词 + 原图保留 | pending | - | - |
| F8 | 常见文件类型覆盖矩阵（预览 / 编辑下限） | pending | - | - |
| F9 | 收尾：一致性审查 + 全量门禁 + 笔记归档 + 总结 | pending | - | - |

### F0 基线（2026-10-19 23:38 实测）

| 项目 | 结果 |
|------|------|
| `cargo check -p hermix-viewer --all-targets` | exit 0，2m13s（sccache 热） |
| `apps/viewer` tsc --noEmit | exit 0 |
| `apps/desktop-tauri` tsc --noEmit | exit 0 |
| `apps/desktop-tauri` vitest run | 35 文件 / 330 用例全绿，exit 0 |
| design guard（desktop + viewer 两个 cwd） | clean |
| preview-layer guard | OK（64 个类名 / 42 个 token） |
| `npx -y impeccable@4.1.0 detect apps/viewer/src` | exit 0 |
| 行数基线 | viewer 前端 11 文件 / 1895 行；viewer rust 3 文件 / 740 行；packages/preview 35 文件 / 6116 行 |

**两个必须记住的环境事实**：

1. **本机没有可用的 bash**。`C:\Windows\System32\bash.exe` 只是 WSL 启动器，且未安装任何发行版；Git Bash 也不存在。因此 `scripts/ci-gate.sh`、`scripts/check-notes.sh`、`scripts/check-hygiene.sh`、`scripts/check-sidecar-freshness.sh` **无法直接执行**——每个子任务用等价命令复刻（cargo check / 两处 tsc / vitest / design guard ×2 / preview-layer / impeccable detect + notes 结构人工复核）。这不是代码缺陷，是本机缺件；门禁脚本本身保持 bash 不动。
2. **开工时工作区不干净**：100 个文件已被暂存（他人/上一轮的在途改动：预览共享层归属、查看器预览引擎懒加载、文本解码、CSP 修复、组合注入等）。这批改动已由 `106bd5f2` 单独落地；F1 的 Rust 侧改动被同一次提交捎带进去（见 F1 行的 SHA 说明）。

**既有门禁失败（与本任务无关）**：`docs/MCPHUB_BORROWINGS.md` 留在 `docs/` 顶层却不在 `scripts/check-notes.sh` 的 `ALLOWED` 白名单里 → notes 结构检查必红（该文件来自提交 `0b72f4f4`，当时归档了 MCPHUB 借鉴笔记但没同步白名单）。F9 一并处理。

### F1 提交归属说明

F1 的改动被拆在两个提交里，原因是**看门狗与主会话并发提交**（首次执行时台账还没落 doing 状态，看门狗判成「有 pending 无 doing」而接管，与主会话同时改同一个工作树）：

- `106bd5f2` — 102 文件：开工前已暂存的在途改动（100）+ F1 的 Rust 侧（`apps/viewer/src-tauri/Cargo.toml` / `src/commands.rs` / `src/lib.rs`）。归属混合。
- `93c6c499` — 2 文件：F1 的前端侧（`apps/viewer/src/App.tsx` / `src/styles.css`）。

**已修的根因**：看门狗的接管判据改为「先读存活标记 `<TMPDIR>\hermix-viewer-task\live.txt`，20 分钟内更新过 = 主会话活着，只报告不动手」。主会话每步都会刷新该文件。


**子任务优先级与依赖**：F0 → F1 → F2 → F3 → F4 → F5 → F6 → F7 → F8 → F9。F3 的「关联后缀图标」依赖 desktop 设置页同步改动，与 F4 可并行但文件不可重叠（桌面端 `SettingsShell.tsx` 归 F3 独占，viewer 端归 F4 独占）。

## 执行指令（交给执行 agent 的原文，可整段粘贴）

```text
# 任务：hermix-viewer 双模式化与全能力重构（无人值守超长任务）

## 0. 运行模式：无人值守

- 禁止调用 ask_user / request_feature_enable，禁止停下来等主人回复，禁止写「请确认/请选择/是否继续」。
- 所有能靠事实拍板的分叉自己拍板，并在立项笔记的「Alternatives considered」里写清理由与被否决的方案（一句话即可，不要长篇论证）。
- 禁止因为「上下文快满了 / 看起来今天做不完 / 差不多了」而收尾。你没有工时限制。
- 唯一允许结束本轮的条件：下面 F0–F9 全部 `done`，或遇到**不可绕过的硬阻断**（如仓库无法编译且与本任务无关的错误无法规避）——此时把阻断点写进台账并继续做下一个不依赖它的子任务。
- 命令被权限门禁拒绝时，换一种写法绕过（改用允许的路径/参数/只读命令），不要停下来提问。绝对不要用不可逆的删除或强推。
- 不要为了让本任务跑通去改用户的安全设置或全局配置。

## 1. 真相源与断点续跑

- 真相源文件（本任务唯一的进度与决策载体）：
  <repo>\docs\notes\proposed\feature\2026-10-19-hermix-viewer-overhaul.md
  读它的「## 执行台账」表与「## Decision / Alternatives considered / Consequences」节。
- 每次开工第一步（顺序固定）：
  1) 读台账表；2) `cd <repo> && git log --oneline -5 && git status --short`；
  3) 确认上次停在哪个子任务的哪一步，接着做。
- 台账写入时机：开工置 `doing` + 心跳 → 再动代码 → 过门禁 → 提交 → 置 `done` + commit SHA。状态永远先于代码落盘。
- 提交规范：`feat(viewer): …` / `fix(viewer): …` / `refactor(viewer): …` / `docs(notes): …`（与仓库现有 `type(scope): 中文描述` 风格一致）。
- 提交范围：只 `git add` 本子任务触及的**具体文件路径**，禁止 `git add -A` / `git commit -a`（工作区开工前就有大量他人未提交改动）。
- 提交分支：直接提交到本地 `main`（主人明确要求）。每个子任务一个 commit，便于回滚。

## 2. 看门狗定时任务（开机第一件事）

用 tool_search 找到 `cron_create`，创建：

- cron_name: `hermix-viewer 长任务看门狗`
- schedule: `*/10 * * * *`
- timezone: `Asia/Shanghai`
- agent_name: `mavis`
- session: `{ "mode": "sessionId", "session_id": "fixed" }`（任务专用对话，跨轮复用）
- prompt: 见下方「看门狗 prompt」原文（照抄）

创建后用 cron_read 确认它已存在且 enabled。然后**继续干活，不要等它**。

- 另可加一个 self_reminder（≤1 个）做短周期自检：延迟 25 分钟后检查「台账里 doing 的心跳是否已更新，未更新则立刻续跑该子任务」。
- 全部子任务 done 后：先 `cron_update` 把看门狗 `enabled: false`，再 `cron_delete` 删除它（避免任务继续空转），并在最终总结里说明已清理。

## 3. 每个子任务的执行循环（七步，不可跳步）

1. **调研** — 先读现有代码与相关笔记（`docs/notes/implemented/feature/2026-10-13-hermix-viewer-resource-manager.md`、`2026-10-09-standalone-file-viewer.md`、`2026-10-19-shared-preview-layer-ownership.md`），再检索优秀开源实现（web_search / GitHub），产出 3–6 条可借鉴点 + 明确的「不直接引入它」的理由。可用 `task` 子 agent 并行调研多个方向。
2. **立项** — 在真相源笔记里为该子任务补一段决策（决定什么 / 否决了什么 / 代价），class 用 `feature`（新能力）或 `bug-fix`（F1、F2）。
3. **实现** — 遵守仓库六步决策阶梯：YAGNI → 标准库 → 原生平台 → 现有依赖 → 单行 → 最小实现。不可削减项：信任边界校验、防数据丢失的错误处理、安全措施、无障碍基础、主人明确要求的功能。
4. **自审** — 逐行回读自己的 diff，删掉临时变量、注释掉代码、重复实现、未使用的导出。
5. **独立审查** — 用 `verifier` 子 agent 对本子任务的 diff 做对抗式审查（找缺陷、找上下文膨胀、找违反门禁的点），按发现修复后复查。禁止自己给自己盖章。
6. **门禁** — 全部跑过（见第 5 节），零失败才算过。
7. **提交 + 更新台账** — 提交后立刻把台账该行置 `done` 并写 SHA，然后进入下一个子任务。

## 4. 子任务清单与验收标准

### F0 基线记录（必做，15 分钟）
- 记录：`./scripts/ci-gate.sh` 当前是否全绿（若本来就红，逐条记下**既有失败**并标注 `TODO: pre-existing` 同级标记）；`cargo check -p hermix-viewer` 耗时；`apps/viewer` 源码行数基线；`git status --short` 全量快照（既存未提交改动清单）。
- 交付：基线写进台账备注。后续所有门禁失败都要能对照这份基线判定是「本次引入」还是「既有」。
- 特别注意：AGENTS.md 禁止引入 `TODO: pre-existing` / `FIXME: pre-existing` 标记；既有缺陷要**真的修掉**或写进笔记，不要留标记。
- **已核实的既有门禁失败（与本任务无关，别误判成自己搞坏的）**：`docs/MCPHUB_BORROWINGS.md` 留在 `docs/` 顶层，但 `scripts/check-notes.sh` 的 `ALLOWED` 白名单里没有它 → `check-notes.sh` 现在在 main 上必红（该文件来自提交 `0b72f4f4`，那次提交已归档 MCPHUB 借鉴笔记却没同步白名单）。处理：把它移入 `docs/notes/archived/`（补 `Archived: YYYY-MM-DD` 行）**或**补进白名单——二选一自己拍板并在笔记里写明理由，然后 `check-notes.sh` 应转绿。

### F1 启动卡顿：驱动器列表异步化（根因已定位，照此修）
- 现状根因（已核实，勿再猜）：
  - `apps/viewer/src/App.tsx:207` 与 `:212-220` **两次**调用 `drives()`；第二个 effect 必须等 drives 返回才 `setCurrentPath(ds[0].path)`，首屏目录列表被驱动器枚举挡住。
  - `apps/viewer/src-tauri/src/commands.rs:429-453` 的 `viewer_drives` 是**同步** `#[tauri::command]`（Tauri v2 中同步命令默认在主线程执行——以本仓库 tauri 2.x 版本文档核实，必要时用 `#[tauri::command(async)]`），且对每个盘符调 `std::fs::metadata`；打在断开的网络映射盘上会阻塞到 SMB 超时（可达数十秒），启动直接冻住。
- 要求：
  1. Rust 侧改异步（`async` + `spawn_blocking` 或 `#[tauri::command(async)]`），**标签判定不再做文件系统 IO**——用 `GetDriveTypeW`（需给 `windows-sys` 补 `Win32_Storage_FileSystem` 已有 feature 即可，必要时加 `Win32_System_Ioctl`）区分本地盘 / 网络盘 / 可移动盘 / 光驱。
  2. 前端**去重**调用：驱动器列表只请求一次，结果进 state 缓存；首屏路径不依赖它——兜底顺序：上次访问目录 → 用户目录 → 首个可用盘；全部失败再退到第一个盘符。
  3. 侧栏在就绪前显示**明确的加载态**（骨架行或 spinner + 「正在读取驱动器…」文案），就绪后替换；**绝不允许空白侧栏**。
  4. 加超时（建议 1.5s）降级：超时后先显示已探到的盘符 + 「部分驱动器无响应」的提示，后台继续探测。
- 验收：断网的映射盘存在时，冷启动首屏渲染 < 1.2s（以 `vite build` 产物为准，dev 模式不作为指标）；侧栏有加载态；`cargo test -p hermix-viewer` 全绿。

### F2 全屏/最大化平铺
- 现状根因（已核实）：`apps/viewer/src/styles.css:304-306` 的 `.explorer { flex:1; min-width:280px; max-width:460px }` 把中栏钉死在 460px；`.sidebar` 200px、`.ai-side` 320px 固定；窗口无全屏状态处理（`tauri.conf.json` 未开启，源码无 `fullscreen` 监听）。
- 要求：
  1. 三栏改为**比例 + 可拖拽分隔条**：侧栏/中栏可拖拽调整宽度并持久化；去掉会留白地的 `max-width` 钉死。
  2. 监听窗口全屏/最大化状态（`getCurrentWindow().onResized()` / `isMaximized()` / `isFullscreen()`）并做响应式重排；全屏时 AI 栏可自动折叠。
  3. 预览区吃掉全部剩余空间；宽屏下不出现空白底色带。
- 验收：1920×1080 / 2560×1440 / 3840×2160 与「最大化」「F11 全屏」两种状态下，三栏铺满、无横向滚动条、无残留空白；分隔条拖拽后布局稳定不塌。

### F3 彩色类型图标系统
- 要求：
  1. 建「扩展名 / 类型 → 专属图标 + 专属色」映射，覆盖图片、文档、表格、演示、PDF、文本、代码、压缩、音视频、字体、数据库等常见类型（≥ 40 个扩展名各有独立图标与色），文件夹另有独立图标（可再分「普通 / 含图片 / 含视频 / 含代码」若干态）。
  2. **先调研再定案**三个方案并量化（装包体积 / 首屏 chunk / 许可 / 维护成本）：A. 复用已有 `lucide-react`（单色）+ 每类型配色；B. 真彩色 SVG 图标集（如 vscode-icons 系的精简子集，注意许可）；C. iconify 按需。结论写进笔记。**默认倾向 A**（零新增依赖、代价最低），除非 A 达不到「一眼分辨类型」的效果。
  3. 类型判定逻辑与桌面端 `apps/desktop-tauri/src/components/FileTypeIcon.tsx` **同源**（`packages/preview/src/resolveKind` 已是共享判定层）——不要造第二套扩展名表。
  4. 桌面端「设置 → 文件关联」页（`FileAssocTab`，当前内联在 `apps/desktop-tauri/src/components/settings/SettingsShell.tsx` 约 449–474 行，数据来自 `file_assoc_list`）的扩展名列表**每个后缀显示对应类型图标**。
  5. 共享层的图标组件与类名必须遵守 `scripts/check-preview-layer.mjs`：共享组件的类名要在 `packages/preview/src/styles/preview-doc.css` 里有规则，**禁止 Tailwind 原子类**（viewer 与 preview 包都没装 Tailwind），用到的每个 `var(--token)` 两端都要定义。
- 验收：常见 40+ 扩展名各有专属图标与颜色；文件夹图标独立；关联后缀列表带图标；`npm run check:design` 与 `scripts/check-preview-layer.mjs` 全绿。

### F4 双模式：纯净 / 重量
- 要求：
  1. **纯净模式**：被文件关联唤起（启动参数带文件路径）时默认进入——**不渲染**侧栏 / 资源管理器 / AI 栏（不是 CSS 隐藏），只留预览面 + 一条细工具条（另存 / 打开方式 / 显示方式 / 切到重量模式 / 关闭）。
  2. **重量模式**：独立启动进入，即现有三栏 + AI + 工具。
  3. 模式可手动切换并持久化；被关联唤起时若用户曾选过重量模式，尊重用户选择（在标题栏/工具条给出明显「切到纯净」入口）。
  4. 两种模式都不能出现「空壳栏」：纯净模式不产生侧栏 DOM。
- 验收：双击关联文件 → 纯净模式，进程内无侧栏/AI 栏；独立启动 → 重量模式；模式切换即时生效且跨启动保持。

### F5 重量模式：图片处理工具集
- 能力清单（按此顺序做，逐个可交付）：裁剪（自由 / 比例 / 批量）、旋转与翻转、缩放、格式转换（png/jpg/jpeg/webp/bmp/gif/tiff/ico，ico 需多尺寸打包）、拼接（横向 / 纵向 / 网格）、转黑白、亮度 / 对比度 / 饱和度、模糊、马赛克（指定区域）、加文字 / 贴图、压缩（质量或尺寸）、EXIF 方向修正、批量处理。
- 架构决策（必须先调研后定案，写进笔记）：交互与即时预览放前端 Canvas（快、零依赖），编码 / 封装 / 落盘放 Rust 侧（`image` 一类纯 Rust 库）；PDF 栅格化复用前端已有的 pdf.js，**不要**为此引入后端 PDF 渲染器。
- 硬约束：
  - **原文件永不覆盖**：所有产物写到新文件（`<name>.edited.<ext>`，允许用户改路径/文件名）；「另存为」走系统保存对话框。
  - 每个操作都有撤销/重做；操作面板用工具条 + 侧边参数区，不堆弹窗。
- 验收：一张 PNG 完成「裁剪 → 转黑白 → 与另一张拼接 → 导出 webp + ico」全链路；原文件 sha256 前后一致；批量处理对 20+ 文件可用。

### F6 重量模式：文本 / 文档 / PDF 编辑与转换
- 文本 / 代码 / Markdown：行内编辑、另存、编码探测（复用 `packages/preview/src/lib/textDecode.ts`）、大文件只加载前 N 行并明确提示。
- PDF：页删除 / 插入 / 重排 / 旋转 / 提取页、合并与拆分、导出页为图片、图片 → PDF、文本层有限编辑（先调研可行性：纯 Rust `lopdf` 能做什么、做不到什么）。
- Office：xlsx 改单元格值（复用已有 exceljs）、docx 文本替换、pptx 标题 / 备注编辑。
- 同样遵守：原文件不覆盖，一律另存。
- 验收：5 页 PDF 完成「删 1 页 → 插 1 页 → 重排 → 另存」成功且原文件不变；xlsx 改值后另存可被 Excel 正常打开。

### F7 AI 协助：一键入对话 + 改图提示词 + 原图保留
- 要求：
  1. 「一键加入对话」：把当前文件（**副本**，原文件不动）连同路径、类型送进 AI 会话；图片类自动带图。
  2. 提示词模板库：图片方向预设模板（去背景 / 加文字 / 风格化 / 修复 / 去水印 / 放大 / 换背景等），用户可改后再发；模板要**写好提示词**（含主体描述、约束、输出要求、保留项），不是一句话占位。
  3. 产物落盘：AI 返回的图片 / 文本 → 另存为 `<name>.ai.<ext>`，并保留「原文件 → 产物」映射记录。
  4. 现状 `apps/viewer/src/AiSidebar.tsx` 是非流式对话（一期形态）。流式化是**可选**子项，不得因此拖慢主线；做则做，不做就在笔记里写明。
  5. 注意 `AiSidebar` 的会话 id 挂在面板生命周期上（历史上出过串台），任何改动不得重新引入「换文件不换会话」。
- 验收：选中图片 → 一键加入对话 → 模板预填 → 发送 → 产物落盘为新文件 → 原图 sha256 不变；换一个文件后是新会话。

### F8 常见文件类型覆盖矩阵
- 逐类定「预览下限」与「编辑/转换下限」并写进笔记（不允许留空）：
  图片（png/jpg/jpeg/webp/gif/bmp/svg/tiff/ico）、文档（pdf/docx/doc/rtf/odt）、表格（xlsx/xls/csv/tsv）、演示（pptx/ppt）、文本与代码（txt/md/json/yaml/toml/xml/html/css/js/ts/jsx/tsx/py/rs/go/java/c/cpp/sql/sh…）、压缩（zip/7z/rar/tar/gz）、音视频（mp3/wav/mp4/mov/mkv — 至少元数据与封面；转码若引入 ffmpeg 则必须写明体积与许可代价，默认不做）、字体、SQLite / 数据库、压缩包内容浏览。
- 每类要么有实现，要么在笔记里写明「只读预览 + 元信息」及理由（依赖、体积、许可）。不允许出现「待定」。
- 预览能力尽量复用 `packages/preview` 既有 viewers（Image/Pdf/Docx/DocCanvas/Spreadsheet/Pptx/Markdown/Html/Code/Text），**不要另起炉灶**。

### F9 收尾
- 逐条复核 F0–F8 的验收标准；跑一次全量 `ci-gate.sh`；更新受影响的既有笔记（`2026-10-13-*`、`2026-10-09-*`、`2026-10-19-shared-preview-layer-ownership.md`）里的事实（路径、名称、结构），**不重写它们的决策**；把本篇相关段落按实际交付现状改写（`Status: implemented`），过时则移入 `archived/`。
- 产出最终总结：做了什么 / 每个子任务的 commit SHA / 门禁证据（命令 + 结果）/ 遗留项与理由 / 停用看门狗的确认。

## 5. 硬约束（不可协商）

**依赖增长**
- 新增依赖每一族都要在立项笔记里论证：为什么现有能力不够、装包体积、对 `cargo check` 时长的实测增量、许可证。
- 预算：Rust 侧新增 ≤ 2 个直接依赖（预期 `image` + `lopdf`）；前端 npm 新增 = 0（`lucide-react` 已在 viewer）。
- 禁止引入：monaco / codemirror、完整 PDF 渲染后端、ffmpeg、任何多 5MB 的前端包。

**代码量与结构**
- 本任务新增源码目标 ≤ 6,000 行（不含测试 / fixture），单文件 ≤ 2,500 行。超预算要在笔记里写明理由。
- 六步决策阶梯不可跳：YAGNI → 标准库 → 原生平台 → 现有依赖 → 单行 → 最小实现。
- 注释只写功能 / 技术根因，禁止「为以后预留」的脚手架，禁止注释掉的死代码。
- 平台能力优先于库：`<input type="date">` 优于日期库；CSS 优先于 JS；DB 约束优于应用代码。

**UI 纪律**
- **一律不用描边**。层次靠底色差 + 阴影 + 间距（`bg-bg-card` / `bg-bg-tertiary` 这类 token）。唯一保留的是键盘 `focus-visible` outline（无障碍必需）。
- 禁止字面色值、禁止 `dark:` 变体、图标按钮必须有 `aria-label`、视图文件禁 `加载中…` 字面量与 `window.confirm`（`scripts/check-design-guard.mjs` 会 grep 拦截）。
- 改完 UI 必跑 `npx -y impeccable@4.1.0 detect <路径>`，退出码 0 才算干净（**注意：detect 对解析不了的文件会静默返回 0**，先跑 `tsc --noEmit` 再信它的绿灯）。
- 令牌层规范见 `docs/TYPOGRAPHY_SYSTEM.md` + `npm run check:tokens`。

**数据安全**
- 编辑 / 转换 / AI 产物一律新文件；原文件不覆盖；写前校验目标路径；失败要报可读错误，不吞异常。
- 用户显式点击「在资源管理器中显示 / 用系统程序打开」才允许调系统程序。

**范围边界**
- 不碰 hermix agent loop、审批引擎、hermix-server 协议；不做跨平台；不升级现有大依赖版本。
- 共享包 `packages/preview` 的改动必须同时验证 desktop（`tsc --noEmit` + vitest 31 文件/297 用例）与 viewer。
- 上下文膨胀防护：读大文件 / 长输出 / 多文件调研一律走子 agent 隔离，主会话只留结论。

## 6. 门禁（每个子任务都要全过，零失败）

```bash
cargo check -p hermix-viewer --all-targets
cargo test -p hermix-viewer
cd apps/viewer && ./node_modules/.bin/tsc --noEmit
cd apps/desktop-tauri && ./node_modules/.bin/tsc --noEmit     # 改了共享包/设置页才需要
cd apps/desktop-tauri && ./node_modules/.bin/vitest run        # 改了共享包才需要
npx -y impeccable@4.1.0 detect apps/viewer/src packages/preview/src
npm run check:design        # scripts/check-design-guard.mjs
node scripts/check-preview-layer.mjs
./scripts/ci-gate.sh        # 全量闸门，收尾时必跑
```

注意：
- `tsc` 必须直接调 `node_modules/.bin` 二进制；`pnpm tsc --noEmit` 只跑 install 流程就返回 0，是假绿灯。
- 门禁失败先对照 F0 基线判定归属：本次引入 → 必须修；既有 → 真的修掉或写进笔记，不留 `TODO: pre-existing` 标记。

## 7. 停止条件与汇报

- 全部 F0–F9 `done` + `ci-gate.sh` 绿 → 停用并删除看门狗 → 输出最终总结。
- 总结必须含：完成清单（逐条对验收标准）、commit SHA 列表、门禁证据（命令 + 实际输出）、新增依赖清单（版本 + 理由 + 体积/编译增量）、代码行数增量、未完成项与原因、已知限制。
- 汇报里不要写「已完成全部验证通过」这种空话——只写你实际跑过并看到结果的命令与结果。

## 看门狗 prompt（创建 cron 时照抄）

检查 hermix-viewer 超长任务进度。工作目录 <repo>。
1) 读 <repo>\docs\notes\proposed\feature\2026-10-19-hermix-viewer-overhaul.md 的「## 执行台账」表。
2) 判定：
   - 所有行都是 done → 用 cron_update 把本任务 enabled=false，再 cron_delete 删除；输出最终总结（做了什么 / commit 列表 / 门禁证据 / 遗留项）；结束，不做任何代码改动。
   - 存在 doing 且心跳距今 ≤ 30 分钟 → 只把该行心跳更新为当前时间，输出一行「进行中：<子任务>」；结束，不改任何代码。
   - 存在 doing 且心跳距今 > 30 分钟，或存在 pending 却无 doing → 判定主会话已中断，进入接管。
3) 接管步骤：
   a) cd <repo> && git status --short && git log --oneline -5
   b) 若有半成品未提交改动：先读 diff 判断它属于哪个子任务的哪一步，**补完而不是回滚**；若无法判断，回到该子任务开头按「七步循环」重做。
   c) 从台账里第一个非 done 的子任务继续，严格按「调研→立项→实现→自审→独立审查→门禁→提交」执行。
   d) 每完成一个子任务立刻更新台账（状态 + 心跳 + commit SHA）。
4) 严禁：调用 ask_user、停下来等回复、把半成品留在工作区不提交、动与本任务无关的文件、覆盖他人未提交改动。
5) 单次运行最多推进 3 个子任务；推进不完就更新台账心跳后正常结束，让下一次看门狗接着跑（这也是「中断自动接上」的机制）。
```
