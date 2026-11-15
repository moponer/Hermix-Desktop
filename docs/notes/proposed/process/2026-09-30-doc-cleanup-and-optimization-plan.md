# Decision Note: 文档清理与新决策记录制度全面落地 + 代码优化路线图

Status: implemented — 批次 A/B/C/D/E/G 已执行（2026-10-01）；仅批次 F 保留待立项

## Decision

三件事分三个批次落地：

1. **文档清理**：按本文迁移表处置 docs/ 现存 56 篇 + 根目录杂件——有效的迁入新体系或保留原位，过期/杂件/重复的移入 `docs/notes/archived/`（或直接删除纯快照类），杜绝"过期设计文档被 agent 当现行依据"的问题。
2. **制度落地补全**：`docs/notes/` 骨架、README、`scripts/check-notes.sh` 门禁与 AGENTS.md 规范节已于 2026-09-30 建立并挂进 ci-gate [10/10]；本提案批准后即成为全项目文档的唯一规则源。
3. **代码优化路线图**：把对 deepseek-harness 525 篇 Agent Notes 的分析结论沉淀为四批可执行工程批次（见文末），供后续会话逐批认领。

## Alternatives considered

- **维持现状 + 手工清理**：否决——文档会继续以每次 PR 新增一篇 PLAN 的速度劣化，根因是没有"新文档往哪放、旧文档何时退场"的制度。
- **全量翻译 dsh 的 Agent Notes 目录树到本仓库**：否决——201+ 篇架构笔记对应的是 microkernel+事件溯源+40 包 monorepo 的复杂度；hermix 18 crate 的规模只需要三态生命周期 + 6 类闭集 + 强制 Alternatives 节，其余照搬是仪式而非信息。
- **只立制度不清旧账**：否决——门禁只管新文件，56 篇旧文档（14 篇过期、16 篇杂件、1 篇重复）继续污染检索与 agent 上下文；清账与立制必须同批。

## Consequences

- 放弃：`docs/` 顶层"一篇 PLAN 一个方向"的自由命名惯例；今后规划一律走 notes 三态。
- 放弃：过期文档的可检索性（archived 件移出主视线；确实还要读的，git 历史与 archived 目录都在）。
- 代价：一次性迁移成本约半天（本表可机械执行）；此后每篇决策文档多写 3 个强制节（收益是决策不再被反复重新争论）。
- 风险与缓解：agent 可能仍往 docs/ 顶层丢新 PLAN → AGENTS.md 已加禁止条款 + ci-gate 提示；后续可在 check-notes.sh 加"docs/ 顶层新增 .md 即警告"（本批未做，见 TODO 节）。

## TODO 节（执行清单）

以下为可分派给执行 agent 的具体批次。每批完成的标准：迁移/修复落地 + 验证命令通过 + 本提案状态更新。

### 批次 A：文档清理（半天）

按 §迁移表 逐行执行。执行方式：
- 【归档】→ `git mv docs/<file> docs/notes/archived/<class>/`，文件头 Status 行下补 `Archived: 2026-09-30`（archived 件不受 check-notes 强制节约束，但 keep 原文冻结）
- 【删除】→ `git rm`（纯快照/报告类，git 历史即存档）
- 【保留】→ 不动；【迁 notes】→ 见批次 B
- 完成后 `bash scripts/check-notes.sh` 必须通过，`docs/` 顶层 .md 只允许 §迁移表"保留"行的文件

### 批次 B：有效规划文档迁入 notes（2 小时）

把 §迁移表中标"迁 notes"的文件改写为标准格式（补齐 Status/Alternatives/Consequences 三节，内容不改写只重组）：
- 现行规划 → `docs/notes/implemented/`（已部分落地的）或 `proposed/`（还没动的）
- design/ 下 3 篇有效 → `docs/notes/implemented/architecture/`
- 原文件 git rm，文档间引用链修复（grep 引用文件名）

### 批次 C：代码止血三件（2026-10-01 状态更新）

> **⚠️ 执行前必读**：另一 agent 会话（worktree `.worktrees/feat-auto-20261001-c09d7e4f`，基于 f469b2ae）已于 2026-10-01 完成本批 3 项中的 2 项，方案与本文不同且**以其实现为准**。已核实存在：`ApprovalFallback` 策略枚举（hermix-core::approval + agent_helpers.rs:387）、ApprovalDrawer Enter 守卫改用 `closest("button, a[href], summary, [role='button']")`（比本文原方案更宽）。**不得按本文 C.1/C.3 原文重做**，否则与已有设计冲突。

1. ~~审批 fail-open~~ → **已完成**（ApprovalFallback 显式策略，方案见 worktree 实现）。
2. **失败轮补录 tool result（唯一保留待做）**：失败收尾前为所有未回答 tool call 填合成结果（"tool not started / outcome unknown, do not blindly retry — side effects possible"），对齐 dsh `failed-step-tool-results`。位置：run_loop.rs 收尾路径。注意 worktree 若已合入，先 grep `TOOL_OUTCOME\|not started` 确认未做再动手。
3. ~~ApprovalDrawer Enter 守卫~~ → **已完成**（closest 方案）。

### 批次 D：可靠性批次（2026-10-01 状态更新）

> ⚠️ D.1/D.2/D.3 已由同 worktree 完成（`join.abort()` 已加 :838、task_id→call_id 映射已加、llm_retry.rs 早有重试循环——**D.1 原文基于错误前提，已推翻**）。保留待做：

4. 批超时不 drop JoinSet：改为 drain + 给运行中任务有界 grace，写工具避免半写。（先核实 worktree 是否已覆盖）
5. tool_dispatch 多审批并行发出（先全审批后执行），消除第 N 张卡串行等待。

### 批次 E：安全批次（2026-10-01 状态更新）

> ⚠️ E.2/E.4 已完成（`save_stored` 0o600 Unix 权限、key.pem rename 前收紧）；E.3 **已否决**（分桶会降低安全性，尊重执行 agent 判断）。保留待做：

1. `security.rs:331` 移除 `?token=` query 通道，仅 Authorization 头。（先核实 worktree）


### 批次 F：结构升级（季度级，单独立项再评审）

1. 事件溯源会话改造（先写 notes/proposed 定事件域语义：session/* 事实 vs agent/* 信号，再动 session_persistence）——dsh 全部架构的根，越晚做迁移越贵。
2. Windows ACL 沙箱（SANDBOX_WINDOWS_ACL_DESIGN.md 迁入 notes/proposed 后认领）：WRITE_RESTRICTED token + 低完整性标签 + FILE_DELETE_CHILD deny ACE；fs/bash 统一策略根。
3. PTC（模型写代码组合工具调用）。
4. 子 agent 推式结算 + 模型自选路由（用户授权门控）。

### 批次 G：代码结构清理（2026-09-30 增补，半天 + 渐进）

盘点方法：crate 依赖图逐边核对、全仓库符号引用扫描、非代码资产引用计数。执行顺序按风险升序：

**G1 死 crate（立即可做）**
- `crates/hermix-protocol/`（127 行）：全仓库 `.rs` 源码零引用（grep hermix_protocol 无命中），hermix-server 的 Cargo.toml 依赖声明是死依赖。先删 server 依赖声明，编译通过后 `git rm -r` crate 并从 workspace members 移除。预期 -127 行 + 编译图瘦身。

**G2 死脚本（立即可做）**
- 以下 scripts/ 文件在 docs/ 与源码中零引用、非 CI gate 组成部分，属一次性调试残留：`probe-redrive.py`、`probe-stdio-approval.py`、`probe-stdio-frames.py`、`repro-stuck-send.py`、`clean-webview-cache.ps1`、`verify-fresh-model-config.py`、`verify-model-list-roundtrip.py`、`generate_hermix_icons.py`（图标已生成入库）。删除前跑一次 ci-gate 确认无隐藏依赖。保留：check-notes/check-theme-tokens/ci-gate/gen-workbench-skill/verify-*（gate 链）、vendor-mirror-probe（有 1 处文档引用）。

**G3 空/近空目录与原型（低风险）**
- `docs/prototype/`（1.3M，10 张截图 + index.html）：仅被 2 篇已判定归档的 UI 文档（V3/V4）引用，随批次 A 归档后一并删除。
- `docs/architecture/`（空目录仅 .gitkeep）：删除。
- `docs/compose/specs/` 两篇过期群聊设计：随批次 A 归档后目录即空，删除。

**G4 死代码候选（需逐个证明后删，不可只信 lint）**
- `hermix-cron/src/file_lock.rs` 被 agent_loop 引用需保留；但 crate 其余部分与 `hermix-automation` 的调度存在职责重叠，合并评估（2 crate → 1）列入批次 F 附带项。
- `#[allow(dead_code)]` 密集点逐一核实：`convergence_detector.rs`（5 处，含 apply_domain_max_stall——AL-8 回退后仅测试调用）、`workbench_lint.rs`（3 处）、`harness_self_edit.rs`（2 处）。规则：无 import 不构成死代码证据，须核对调用点/配置/生成物后按 simplification 类笔记流程删除并写 Reintroduction conditions。
- `hermix-environments` 的 ssh/docker feature 默认关闭，确认无产品路径启用后可裁（当前 server 全量依赖 environments，需先查启用面）。

**G5 上帝文件拆分（对齐已有 2500 行纪律，渐进）**
按 ci-gate 已告警的 6 个文件，拆分优先级（拆分原则：砍重复/搬测试，不单纯切文件）：
1. `local_agent_service.rs`（4625）— 审批/权限/工具路由/overrides 四块各自成模块；高危 fail-open 修复（批次 C）就在此文件，拆分前先修 bug 以免冲突。
2. `agent_loop/tests.rs`（3731）— 测试文件，按主题搬到独立测试文件，零运行时风险。
3. `App.tsx`（3479）— 页面级状态容器，抽 composables（键盘/缩放/审批 resolver 已有 hooks 雏形）。
4. `ws.rs`（3245）/ `provider.rs`（3202）/ `run_loop.rs`（2755）— 各按 handler/协议/阶段边界切。

**G6 资源体积（打包项，非代码）**
- `resources/loader/` 15M + `resources/icons/` 3.3M：随 NSIS 打包体积直接相关，暂无删除候选（loader 二进制现行），仅记录基线供后续对比。

### 批次 H：内置工具打磨（2026-09-30 增补，来自与 deepseek-harness 工具层逐项对比）

背景：用户实测反馈"命令执行、文件编辑、读、查找不顺手"。对比 dsh 的 tool-fs / tool-bash(-persistent) / tool-fs-search / tool-str-replace-editor 后，差距按影响排序：

**H2 执行结果 + 一次漏改（2026-10-01）**：单条路径已按契约改完（字面唯一性 + `allow_fuzzy` opt-in + `match: literal|fuzzy` 如实回报）。但**多编辑路径（`edits` 数组）原封不动绕过了全部三样**——它直接调 `fuzzy_find`，既无唯一性校验也无 opt-in 判断，而"模型用 `edits` 批量改文件"恰恰是推荐用法，覆盖面比单条更广。

- 漏改的暴露方式值得记：`patch_file_multi` 早就把 `allow_fuzzy` 一路传到了后端签名（`tools/file.rs` 层完好），但后端**从未使用该参数**，编译器 `unused variable` 警告是唯一的线索。这类"参数已加但未接线"的半成品，靠读代码发现不了，靠测试更发现不了——旧测试全部只打单条路径。
- 修法不是补一段 multi 分支的校验，而是把契约抽成 `apply_single_edit` 共享 helper 让两条路径共用；`fuzzy_find` 现在全仓只有这一个调用点，**结构上无法再走散**。教训：契约类修复要抽到唯一入口，补条件判断只会留下下一个人漏改的第二个分支。
- 回归测试钉 multi 侧而非单条侧：歧义拒绝 / 无 fuzzy 拒绝 / fuzzy opt-in 可达且**如实标记 `match`** / 顺序应用 / 失败时全批丢弃（all-or-nothing，不得落半截文件）。

**H1 read_file 自相矛盾（bug 级，先修）**
`file.rs:220-226`：Fs 接缝路径**拒绝 offset/limit 参数**，但字符截断的提示文案却叫模型"Use offset/limit to read further chunks"——接缝装配时模型照提示翻页必报错。且 Fs 路径一次读整文件，仅靠 300K 字符硬截断兜底，无行号、无分页脚注。修法：Fs 路径支持行级 offset/limit（读入后切片），截断提示与实际能力一致；输出加行号前缀（对齐 dsh `read`，模型引用行号更准）。

**H2 编辑工具用模糊匹配（正确性风险）**
`patch` 描述自认"fuzzy matching，空白/缩进差异不阻断"——模糊匹配意味着 old_string 可能命中**错误位置**并静默改错。dsh `edit` 是唯一匹配的**字面替换**（不唯一即报错，除非 replace_all）+ read-before-edit（编辑前必须先 read 且文件未变）。修法：patch 默认字面精确匹配 + 唯一性校验；模糊降级为显式 opt-in 参数；加 observed-state 检查（读后已变即拒绝，防覆盖用户手工编辑）。

**H3 无持久终端会话（不顺手的主因之一）**
hermix terminal 每次调用都是新进程：cd、激活 venv、REPL/gdb 状态在调用间全部丢失，模型只能用 `cd x && y` 串接或反复绝对路径。dsh 有 tool-bash-persistent（持久 PTY：cwd/env/REPL 状态跨调用存活）。hermix terminal.rs 已有 `pty` 参数与 background 机制，缺的是**跨调用会话句柄**（session id + 后续 write/read 到同一 PTY）。修法：加 `persistent: true` + `session_id` 参数，会话表挂 TerminalBackend，超时回收。REPL/gdb/venv 场景一步到位。

> H3 状态（2026-10-02）：执行中曾否决（判为平台限制），2026-10-01 因事实错误（"Windows 无 PTY"）被推翻并独立立项，2026-10-02 交付（implemented）：[2026-10-01-persistent-terminal-conpty](../architecture/2026-10-01-persistent-terminal-conpty.md)。

**H4 命令输出截断无恢复路径**
`format_command_output` 100K 硬截断只说"truncated at N chars"——被截断的中间内容**拿不回来**（dsh 超限输出落 spill 文件，模型用 read_file 取回全文）。hermix 其实已有 spill 机制（run_loop.rs SPILL_INLINE_CHARS），但 terminal 工具层没接。修法：截断时落 spill 文件并在提示中给出路径。

**H5 查找工具缺 mtime 排序与上下文行**
search_files 返回匹配行但无 `-A/-B/-C` 上下文参数（模型要再 read_file 一轮才能判断）；无按修改时间排序（"最近改过的配置"类问题低效）。ripgrep 本身支持，补 schema 参数即可。

**H6 工具描述的"何时不用我"指引**
dsh 每个工具 README 有 Model Experience 节（何时用、何时换别的工具）。hermix 的 search_files 描述已做了（"何时 reach for"），terminal/read_file/patch 缺同类指引（如 patch 描述里该写"整文件重写用 write_file、只看内容用 read_file"）。

执行顺序：H1（bug）→ H2（正确性）→ H3（体验，工作量最大可独立排期）→ H4/H5（小改）→ H6（文案）。

**H 方针升级（2026-09-30 二次分析定稿）：对齐 dsh 工具契约**
H1-H6 的修法统一为"抄行为不抄代码"：dsh 工具层是 MIT 协议的 TypeScript 实现，代码搬不过来（语言不同），但其**参数契约、行为语义、错误文案**经过大量生产验证且与 Claude Code 同款词汇（模型训练数据里见过大量用例，对齐后模型用对工具的概率天然更高）。执行标准：
- 参数名与语义照抄 dsh：`read`(file_path/offset/limit)、`edit`(file_path/old_string/new_string/replace_all)、`glob`、`grep`(pattern/path/-A/-B/-C/output_mode)
- 错误文案风格照抄：稳定错误码 + 恢复指引（"don't blindly retry — side effects possible"）
- 需在分发物（NSIS 打包的 LICENSE 页 / THIRD_PARTY_NOTICES）加 MIT 归属声明：行为契约参考 deepseek-ai/deepseek-harness
- 不抄：Cordis 插件装载体系、8 包拆分、PTC SDK（批次 F 另议）

### 批次 I：垃圾文件与测试脚本清理（2026-09-30 增补，1 小时）

**I1 仓库内垃圾文件（git rm / 直接删，全部已核实零引用）**
- 根目录散件（均未被 git 跟踪，直接删 + 补 .gitignore）：`c_scan.log`(0B)、`usn_build.log`(0B)、`cur_routes.txt`、`head_routes.txt`（一次性路由探针输出）、`satteri-win32.tgz`（1.4M，gitignore 已列但文件仍在盘上）
- `apps/desktop-tauri/_tmp_17660_*`、`_tmp_18632_*`（gitignore 已覆盖的运行残留，删盘上文件即可）
- 本地产物不入库（gitignored，可留在盘上或手动清）：`.scratch/`(118K)、`design-previews/`(84K)
- `docs/prototype/`（1.3M，仅被 2 篇待归档 UI 文档引用，随批次 A 一并删）；`docs/architecture/`（空目录）
- 提案自身第 24 行承认的 TODO 项已闭环：docs 顶层守卫已随 a6a1018 落地（现为 WARNING，批次 A 完成后转 fail）

**I2 scripts/ 清理（8 删 9 留）**
删（一次性调试探针，docs/源码零引用、非 CI 组件）：`probe-redrive.py`、`probe-stdio-approval.py`、`probe-stdio-frames.py`、`repro-stuck-send.py`、`clean-webview-cache.ps1`、`verify-fresh-model-config.py`、`verify-model-list-roundtrip.py`、`generate_hermix_icons.py`（图标已入库）
留（CI gate 链或现行工具）：`ci-gate.sh`、`check-notes.sh`、`check-theme-tokens.mjs`、`verify-skill-freshness.mjs`（gate 四件套）；`verify-stability-suite.py`（CI 引用）、`verify-wb-tools.mjs`、`verify-workbench-ui.cjs`、`gen-workbench-skill.mjs`、`vendor-mirror-probe.ps1`（有文档引用）
删前执行：`bash scripts/ci-gate.sh` 全绿后再 `git rm`，删后重跑一次。

**I3 crate 内测试文件（不清理，仅治理）**
35 个 tests 文件 / 约 1834 用例是资产不是垃圾；唯一治理项：`agent_loop/tests.rs`（3731 行）按主题拆分（批次 G5 已列）。17 个 `#[ignore]` 测试逐个标注理由或删除（单独小任务）。

### 批次 J：LLM 乱写文件防护规范（2026-09-30 增补——「设立项目规范」专项）

问题定义：模型在开发过程中把方案/计划/临时脚本写到错误位置（docs/ 顶层 PLAN、仓库根 py/txt/log 散件、把 .scratch 外的东西落进源码树），污染仓库且诱导后续会话把垃圾当资产。根因是三层：①系统提示没有"往哪写"的硬约束；②工具层没有工作区边界校验；③违规后没有门禁拦截（只靠事后人工发现）。

**J1 系统提示规范（directions.rs 增补一段「文件写入纪律」）**
- 源码树内只允许改**已存在的源码文件**与**测试文件**；新文件必须落在既有模块结构内
- 方案/规划/设计 → `docs/notes/proposed/{class}/yyyy-mm-dd-topic.md`（已由 AGENTS.md 文档制度节确立，提示词引用同一条）
- 临时脚本/探针 → **一律不落盘**，用 `code_execution` 工具在内存跑；确实要落盘的放 `.scratch/`（gitignored）并在会话结束说明
- 日志/中间产物 → 严禁写进仓库根；工具输出自然进 ctx，不需要文件
- 禁止新建顶层目录（`crates/`、`apps/`、`docs/` 之外）——新 crate 是架构决策，先写 proposed 笔记

**J2 工具层工作区边界 —— 已否决（2026-10-01）**
原方案（write/patch/terminal 重定向写命中散件模式即弹审批卡）被执行 agent 正确否决：与 2026-09-26 用户明确定策「写文件和命令执行不用审批，只有删除等高危才审批」直接冲突，且 `local_agent_service.rs:1120` 有注释 + 三个测试钉死该行为；批次 I 实测发现的 5 个根目录散件全部已被 .gitignore 覆盖，运行时拦截的靶子大半不存在。详见 `docs/notes/rejected/process/2026-10-01-j2-resource-guard-rejected.md`。J 批次保留 J1（系统提示）+ J3（CI 门禁兜底），残余缺口（子目录散件运行时不拦）由 J3 在 git add 时刻兜住，属可接受摩擦取舍。

**J3 CI 门禁兜底（check-notes.sh 已有雏形，扩展为 check-hygiene.sh）**
- 扫描 git 工作区：根目录新增 `.log/.txt/.tgz/py` 散件（白名单外）→ fail
- `docs/` 顶层新增 .md（白名单外）→ 已有，WARNING 转 fail 的时机随批次 A
- 新建顶层目录 → fail
- 与 pre-existing debt 标记检查同一层，进 ci-gate

**J4 验收**（J2 已否决，验收面收窄为 J1+J3）
构造两个违规样本（仓库根散件、顶层 PLAN / 新顶层目录）触发 J3 门禁 fail；再构造两个合规样本（notes 笔记、.scratch 脚本）确认放行；J1 生效性以提示词注入后的模型行为抽查确认。全部通过才算完成。

### 批次 K：工具契约对齐 + 差距修复（= 批次 H 的实现批次）

K 与 H 是同一件事的拆分：H 是差距分析（本文 §批次 H），K 是给执行 agent 的实现任务书——**按 H 的 H1-H6 顺序执行，行为契约以 dsh 为参照标准（见 H 方针升级节）**。K 完成定义：H1-H6 全部落地 + `cargo test -p hermix-tools` 全绿 + 新增行为契约测试（read 分页、edit 唯一性、persistent 会话存活、spill 恢复路径各至少 1 个测试）。

### 批次 M：MCP 目录国内可用化（2026-10-01 增补，起因：用户网络直连 registry.modelcontextprotocol.io 超时，目录功能 502 不可用）

背景核实（2026-10-01 实测）：官方 registry 托管境外线路，用户网络直连 HTTP 000 超时；后端逻辑正确（失败回退缓存），但该部署从未成功拉取过 → 无缓存可回退 → 502。官方 registry 开源且鼓励自建子注册中心（同 API 形状），国内无权威同形镜像；ModelScope MCP 广场国内可直连但 API 形状不同。按优先级：

**M1 内置精选目录兜底（必做，解决开箱即用）**
- 编译期打包一份 curated `catalog_fallback.json`（30-50 个常用 MCP server，中英文描述，全部预过 mcp_scan 安全扫描）
- `/api/mcp/catalog` 改三层：新鲜缓存 → 网络拉取（成功则落缓存）→ **内置目录**（`from_cache: true` + `stale_reason: "联网不可达，显示内置精选目录"`），502 场景彻底消失
- UI 标注数据来源徽标（在线 registry / 缓存 / 内置离线）

**M2 registry 源可配置 + 多源手动切换（必做，本批核心）**
- **内置源清单**（`mcp_catalog.rs` 静态数组，每项含 id/名称/URL/说明/默认标志）：
  1. `official` — 官方 registry（默认；境外线路，有代理时最佳数据源）
  2. `builtin-offline` — 内置精选目录（M1 的打包 JSON 作为"伪源"，零网络）
  3. `self-hosted` — 用户自建子注册中心（设置里填 URL；官方 ghcr 镜像一条命令，API 同形）
  4. `modelscope` — ModelScope MCP 广场（**实验性**，默认隐藏，设置里"显示实验源"开关打开才出现；API 形状不同需适配层，2026-10-01 实测主站可直连 0.6s 但公开 API 端点未稳定）
- **切换机制**：`HERMIX_MCP_REGISTRY_URL` env（优先）> 设置项存储的源 id（`registry_source`，持久化到 hermix_home 配置）；`self-hosted` 选中时额外读 `registry_custom_url`
- **UI**（并入 L3 设置中心，L3 落地前先挂在现有 ToolTabs/MCP 页）：源下拉（显示名称+连通性徽标：绿=上次探测通/灰=未知/红=不通）+ self-hosted URL 输入行 + 「测试连接」按钮（后端加 `/api/mcp/catalog/probe?source=` 探测端点，返回延迟与形状校验结果）
- **归一化契约**：所有源统一输出现有 `CatalogEntry` 结构；`modelscope` 适配器做字段映射，映射不了的字段置空并如实标注（install 所需 command/args/env 缺失时该条目"仅浏览不可安装"）
- **install 端点不变**：仍以 server 反域名 id 为键；`modelscope` 源条目 id 前缀 `modelscope:` 防跨源冲突

**M3 ModelScope MCP 广场适配器（可选，先验证再接）**
- 国内可直连的目录站（阿里系）；API 形状与官方不同，需写转换层归一化到现有 CatalogEntry 结构
- 前置：先手动验证其公开 API 的稳定性与字段完备度（install 所需的 command/args/env 是否可得），不稳就不接，M1+M2 已覆盖 90% 场景

**M4 自建子注册中心指引（文档级，不写代码）**
- 官方文档路径：`docker run -p 8080:8080 ghcr.io/modelcontextprotocol/registry:latest` + PostgreSQL；写进 cookbook 或 mcp_catalog.rs 文档头，供企业/重度用户路径

执行顺序：M1 → M2（合一个 PR）→ M4（文档）→ M3（验证后再说）。完成定义：断网环境打开 MCP 目录页显示内置精选目录且来源徽标正确；配置自建 registry URL 后目录/一键添加全流程可用。

**情报来源与边界**：截图为 ZCode（MiniMax Code 桌面端 3.1.0）的通用×2、外观深/浅×2、浏览器页 + 主工作区。桌面端**闭源**（github.com/MiniMax-AI/minimax-code 仅收 issue）；其 CLI 已开源（MIT，`@minimax-ai/code`），已克隆至本地翻找：TUI 主题系统（`packages/tui/src/tui/theme/`：contracts.ts 定义 text/muted/border/line/success/warning/error + userMessageBg/diffAddedBg/diffRemovedBg 角色；palettes 带 WCAG AA 回归测试——正文 4.5:1、非文本 3:1；自定义主题 = 数据目录 `tui/themes/*.json` 一文件一外观、name 相同即成对、热重载、未知值回退不阻断启动）。桌面端截图外的规格（精确字号/间距/开关尺寸）**拿不到源码**，以截图目测 + 上述 token 词汇推断，执行 agent 需按像素截图比对微调。

**L0 颜色债清偿（L1 的硬前置，2026-10-01 审计后增补）**
审计结论：颜色统一管理三层体系已建立（tokens.css @theme 契约 + themes/*.css 主题覆盖 + check-theme-tokens.mjs 门禁），87 处组件已走 `--color-accent` 语义类，E1-E8 品牌色例外合理。但 ZCode 式"强调色可选、全组件联动"要求 100% 组件吃语义 token，当前还有三块债：
1. **101 处 Tailwind 原生色类**（`text-red-400` 等；集中：AnalyticsView 13、ToolCallBubble 8、SelfCheckStepCard 8、AwakeningCard 6、BackendStatus 4）→ 迁语义 token（--color-error/--color-success-soft 等四色组软硬版本）；迁完把 check-theme-tokens.mjs 的 `palette-debt` 从 WARN_ONLY 改 `exit(1)`（EXCEPTIONS.md 已写明此收紧步骤）
2. **E9-E16 历史例外**（8 个文件：遮罩 rgba→--color-overlay-scrim、阴影→--shadow-pop、边框/提醒色→对应 token）→ 逐条消化后从 EXCEPTIONS.md 删除
3. **CSS 层 34 处 `[data-theme]` 字面覆盖块**（components-*.css，EXCEPTIONS.md 已登记）→ 能映射四色组的迁 token，不能的逐条编号留档
完成定义：`node scripts/check-theme-tokens.mjs` 全绿 + palette-debt 收紧为硬门禁 + EXCEPTIONS.md 只剩 E1-E8 合法例外。**此步不做，L1 的强调色切换就是半吊子**（切换后 109 处组件不变色）。

**L1 执行结果（2026-10-01 完成）**：`b05766e0` 分层 token + useTheme 注入、`884b4538` accent 家族移出 @theme 并由使用槽 color-mix 派生、`b573b0a7` 深色 fg/bg 校准。
- 验收逐条：`check-theme-tokens.mjs` 全绿且 CONTRACT 由 106 **收缩到 101**（accent-hover/soft/soft-strong/on-text 退出契约，dark.css 同步停止声明）；`pnpm tsc --noEmit` exit 0；三态中"不注入=主题默认"已由编译探针验证。
- 走过的弯路（两次"看起来能工作、实际零效果"，均被编译探针拦下）：① `--color-accent-choice: ;` 声明为空时 `var(x, fallback)` **不回退**而返回空串，强调色会静默消失；② 自引用 `var(--choice, var(--accent))` 与 `themes/*.css` 同特异性被后者胜出 + 循环依赖 → 整条声明 invalid。**"跟随主题"必须用 `removeProperty`（属性不存在），不能用"设为空"。**
- 深色校准只动两个锚点（fg `#EDEDED`、bg-app/chat `#171717`）；下方灰阶**保持不动**——它们按 WCAG 对比度推导，非截图采样。核算确认 primary 15.31 / secondary 6.67 / tertiary 4.77 均达 AA-normal，faint 2.97 是装饰级（符合原约定）。
- **未做**：L3.5 选色 UI（依赖 L3.1/L3.2）、策展调色板与 `--color-on-accent` 注入（自由取色无正解）、`--contrast-scale` 对比度滑杆（需 UI 承载）。
- **`--color-on-accent` 定案（2026-10-02）**：该 token 随强调色**明度自动选浅/深文字**（`color-mix` 派生出不来，只能程序性求值）。**自由取色没有正解**——任意色相上硬做必然在某些色相糊掉（亮黄配深字、深紫配浅字之间的过渡带无普适阈值）。因此不先做自由取色器：`--color-on-accent` **与策展调色板一起交付**，每个策展条目预计算 on-accent，注入时与 `--color-accent` 成对写入。其余依赖前置 UI 组件的项（L2-L5、L3.5 选色 UI、`--contrast-scale` 滑杆）同样顺延。

**L1 方针定稿（2026-10-01，审核 agent 拍板）：路线 A 分层 token，范围 = token 层 + useTheme 注入 + 深浅色校准，UI 归 L3.5**
- **路线 A（已采纳）**：`--color-accent-base` 归主题层（dark.css/light.css 各自提供），`--color-accent` 仅在 base.css 使用层以 `var(--color-accent-base)` 供给；用户选色时 useTheme 在 **React mount 前**注入 `--color-accent`（防首帧闪色），不选色时不注入。主工作区已有半实现（tokens.css/base.css/dark.css 未提交改动），在其上完成而非重来。
- **路线 B（否决）**：@theme 是 L0 刚建立的契约层（check-theme-tokens 从它派生 CONTRACT 85→106），把"运行时派生"混进契约层会模糊 check 1 的语义，且多一层间接无收益。
- **accent 家族派生规则**（执行必读）：`--color-accent-hover/soft/soft-strong` 从 @theme 静态值改为 **base.css 使用层 color-mix 派生**（`color-mix(in srgb, var(--color-accent) 88%, ...)`），否则用户注入 accent 后 hover/soft 仍是主题静态值（青 accent 配绿 hover = 破相）。`--color-on-accent` 无法 color-mix 派生（需按明度选择深/浅文字），**强调色必须是策展清单**（对齐 ZCode 下拉：绿/青/蓝/紫…，不做自由取色器），每个条目预计算 on-accent，注入时写 `--color-accent` + `--color-on-accent` 两个变量，其余全靠 CSS 派生级联。
- **范围确认**：本批 = 上述 token 重构 + useTheme 注入能力（localStorage 放值即生效，手动可验）+ 顺手做深浅色校准（#171717/#EDEDED，30 分钟，同在 token 层）。UI（设置页选色行）→ **L3.5**，依赖 L3.1/L3.2 的 Switch/SettingsRow 基础件，届时只写 localStorage 键。
- **验收**：`check-theme-tokens.mjs` 全绿（accent-hover/soft 移出 @theme 后 CONTRACT 相应收缩，各主题 css 同步停止声明它们，防 check 1 报缺失）；手动验证三态：不注入=主题默认色、注入绿=全站联动（含 hover/soft）、注入青=同上；`pnpm tsc --noEmit` 全绿。

**L1 设计 token 对齐（截图提取的确定值）**
- 深色：前景 `#EDEDED`、背景 `#171717`、选中/强调 = 绿色（开关 on 色）、页面标题 14px semibold、节标题（模式/应用/文件…）14px、设置项标题 14px regular、描述 12px 次级灰、卡片分组圆角 ~12px、开关 on 绿 `#34C759` 系
- 浅色：前景 `#171717`、背景 `#FFFFFF`、强调色青/绿可选（强调色下拉：绿色/青色…，即**强调色可选主题化**）
- 落地方式：hermix 已有 7 档字号 token + 纸墨双主题（tokens.css/themes/*.css），**不改架构**——新增 `--color-accent-choice`（用户可选强调色，CSS 变量切换）、对齐 `#171717/#EDEDED` 作为深色 fg/bg 可选项或直接校准现有 dark.css 值；页面对比度参数（截图 0-100 滑杆）→ `--contrast-scale` 乘算表面色 alpha
- 字号映射：截图的 14/12 px 正好落在 hermix `--text-body`(14)/`--text-meta`(12)/`--text-ui`(13)，无需新增档位

**L2 设置导航重构（布局抄 ZCode）**
现状：hermix 设置是平铺 Tab（Backend/Model/Security/Devices/Advanced/Diagnostics…），无左侧分组导航。ZCode 结构：左侧栏 = 搜索框「搜索设置…」+ 三组分节（偏好：通用/外观/语音/快捷键/个性化/浏览器；插件：插件/技能/MCP/Agents；管理：用量与模型/连接/账户；编码：代码审查/工作树；归档：已归档任务）+ 顶部「← 返回应用」。落地：新 `SettingsShell.tsx`（左导航 + 右内容区路由），现有各 Tab 内容组件**原地复用**塞进新分组；设置搜索（节标题+设置项标题索引，`cmd/ctrl+K` 已有先例）。

**L3 缺失组件重写清单（ZCode 有、hermix 无）**
1. **Switch 开关**（截图主角，全设置页大量使用）——hermix 现在用 checkbox/按钮拼；新 `ui/Switch.tsx`（on=强调绿、off=灰、36×20 圆角胶囊 + 滑块动画）
2. **设置行组件** `ui/SettingsRow.tsx`：左标题+描述 / 右控件（Switch|Select|Button|色板）三段式；分组卡片（圆角容器内多行分隔线）
3. **模式选择卡**（通用页顶部两张大卡：适用于编程开发/日常办公，图标+单选环+标题+说明）——hermix 已有 domain_mode 后端字段（`domain_override`），**只缺这块 UI**，对接 Composer 的 chip 状态
4. **三选卡**（明暗模式：浅色/深色/跟随系统，缩略图预览 + 选中描边）——hermix ThemeToggle 只有二态，补"跟随系统"（tauri 有 window theme API）
5. **主题下拉 + 强调色选择 + 前景/背景色板（重置按钮 + #HEX 显示）+ 对比度滑杆 + 导入/复制主题**——对应 mcode CLI 的 theme 契约（自定义主题 json、热重载思路映射到 hermix themes/*.css + registry.ts）
6. **壁纸**（添加壁纸 + 1024×576 提示 + 缩略图占位）→ 后端无对应能力，**需新 tauri command**（壁纸落 app 数据目录 + webview 背景注入）；低优先
7. **数据目录行**（路径 + 「更改」按钮）→ hermix 有 hermix_home 概念，对接 `hermix-config::paths`
8. **语言下拉、发送快捷键（enter/ctrl+enter）、跟进消息行为（排队/立即发送 SegmentedControl）、去除 AI 水印、数据用于优化体验、上传日志、检查更新**——逐项判断：快捷键/语言可做（前端存储即可）；水印/数据优化是 MiniMax 云服务专属**不抄**；上传日志对接现有 DiagnosticsTab；检查更新对接 tauri updater

**L4 功能对接映射（ZCode 设置项 → hermix 后端现状）**
- 通用页「桌面通知」→ hermix 无系统通知，需 tauri-plugin-notification（新对接）
- 「开机自启动」→ tauri autostart 插件（新对接）
- 「显示在菜单栏」→ 托盘（tauri tray，hermix 未启用，评估后可裁）
- 「加速索引」→ hermix 已有 usn-indexer/本地查找，**已有对应**，接一个开关状态显示
- 浏览器页「自动打开浏览器面板/链接打开位置/Chrome Cookie 导入/清除浏览器数据」→ hermix cua/browser 工具已有浏览器面，前三项是前端行为开关；Cookie 导入后端不支持**标不抄/后补**
- 「会话管理：上下文窗口使用情况/纯文本编辑器」→ 上下文用量 hermix 已有（CompactionStatusBanner），补一个开关；纯文本模式**不抄**（hermix 走 Markdown 渲染是定位）
- 「已归档任务」归档页 → hermix 会话已有 pinned/archived（usePinnedArchived），导航加分组入口即可

**L5 主工作区对照（第 6 张截图）**
左侧栏：顶部图标列（侧栏开关/搜索）+「新建任务」主按钮 + 插件技能/定时/网站/远程 + 本地/云端 Tab + 项目树（会话列在项目下，「更多」收纳）——hermix Sidebar 已有会话分组/钉选/归档，**差距**：①会话树挂在项目节点下（hermix 是平铺分组）②左下用户卡（头像+订阅档+通知铃）。对话区：顶部会话名下拉 + 右上三图标（VSCode 连接/终端/历史）+ 底部输入框（附件+/始终授权/模型选择器 M3.1-Flash-Preview default/语音/发送）——hermix Composer 已有等价件，差距：①「始终授权」常驻开关（hermix 在设置深处）②模型选择器带 provider 标注。代码块卡片：语言标签 + 复制/运行按钮 —— hermix MarkdownBody 已有复制，补运行按钮对接 code_execution。表格/行内 code 样式对齐即可。

**执行顺序**：L1（token，半天）→ L3.1/L3.2（Switch/SettingsRow 基础件）→ L2（SettingsShell 导航）→ L3.3/L3.4（模式卡/明暗三选）→ L4 逐项对接 → L3.6 壁纸/L5（最后，可裁）。完成定义：6 张截图逐张对照走查通过 + `pnpm tsc --noEmit` 全绿 + 新组件 storybook 式自测页（`/dev/settings-preview` 路由，仅 dev 构建）。

## 迁移表

判定来源：2026-09-30 对 docs/ 全部 56 篇 + 根目录 3 篇杂件的逐篇审读。class 按笔记分类法标注（归档时入同名目录）。

| 文件 | 判定 | 处置 |
|---|---|---|
| docs/ARCHITECTURE_BENCHMARK.md | 杂件（06-19 行数快照） | 删除 |
| docs/ARCHITECTURE_ISSUES_REPORT.md | 杂件（07-17 问题清单） | 删除 |
| docs/ARCHITECTURE_REFACTOR_PLAN.md | 过期（被 PLUGIN_KERNEL 取代） | 归档 architecture |
| docs/CHANNEL_WORKSPACE_SCOPE.md | 有效（IM 通道现行规划） | 迁 notes/implemented/feature |
| docs/CODEBASE_AUDIT_REPORT.md | 杂件（07-24 审计，已消化） | 删除 |
| docs/COMPRESSION_NO_MEMORY_LOSS.md | 有效（现行机制说明） | 迁 notes/implemented/architecture |
| docs/CSS_NAMING_AUDIT.md | 杂件（自标不批量执行） | 删除 |
| docs/DEEPSEEK_HARNESS_BORROW.md | 重复（主=BORROWINGS） | 删除 |
| docs/DEEPSEEK_HARNESS_BORROWINGS.md | 有效（P1-P3 待做，现行规划） | 保留原位（本文来源文档），P0 落地部分可拆笔记 |
| docs/DEEP_ANALYSIS_2026-07-30.md | 杂件（与审计重叠） | 删除 |
| docs/DESIGN.md | 过期（被纸墨 UI 取代） | 归档 architecture |
| docs/FEATURE_ANALYSIS_SETTINGS_MODEL_SECURITY.md | 已完成 | 归档 feature |
| docs/FILE_PREVIEW_REFACTOR.md | 有效（09-27 现行方案） | 迁 notes/proposed/feature |
| docs/FOUNDATION_REWRITE_DESIGN.md | 过期（自标被取代） | 归档 architecture |
| docs/FRONTEND_AUDIT_2026-07-13.md | 杂件 | 删除 |
| docs/FRONTEND_AUDIT_TODO_2026-07-13.md | 杂件 | 删除 |
| docs/FRONTEND_DESIGN_AUDIT.md | 杂件（被字体系统接手） | 删除 |
| docs/GROK_BUILD_MIGRATION_REPORT.md | 杂件（执行报告） | 删除 |
| docs/HANDOFF_FOUNDATION_REWRITE.md | 有效（唯一交接入口，09-26 仍更新） | 保留原位 |
| docs/HERMIX_AGENT_SYSTEM_PROMPT.md | 有效（现行系统提示词设计） | 保留原位 |
| docs/MIGRATION_TODO.md | 已完成（06 重构收口） | 归档 architecture |
| docs/MODEL_CONFIG_REDESIGN.md | 有效（P3/P4 待做） | 迁 notes/implemented/feature |
| docs/OPTIMIZATION_DIRECTIONS_2026-07-26.md | 已完成（4/5 落地） | 归档 process |
| docs/OPTIMIZATION_PLAN.md | 过期（被 DIRECTIONS 覆盖） | 归档 process |
| docs/ORCHESTRATION_ENGINE_DEV_PLAN.md | 过期（路由已整体砍除，方向推翻） | 归档 architecture |
| docs/PLUGIN_KERNEL_DESIGN.md | 有效（现行内核方向） | 迁 notes/implemented/architecture |
| docs/PRODUCT_ZERO_FRICTION_DESIGN.md | 杂件（自标不开发） | 删除 |
| docs/PROPOSAL_PORTABLE_DESKTOP.md | 有效（产品定位铁律） | 迁 notes/implemented/process |
| docs/SANDBOX_WINDOWS_ACL_DESIGN.md | 有效（P3 待做，批次 F 认领） | 迁 notes/proposed/architecture |
| docs/SEAMS_DESIGN.md | 已完成（自标已落地） | 归档 architecture |
| docs/STABILITY_AND_REMOTE_PLAN.md | 有效（阶段 1-3 落地，5-8 执行中） | 迁 notes/implemented/process |
| docs/STABILITY_ENGINEERING_PLAN.md | 过期（路线被抛弃） | 归档 process |
| docs/STABILITY_PLAN_REVIEW.md | 杂件（一次性审查） | 删除 |
| docs/STABLE_FOUNDATION_DESIGN.md | 过期（自标被取代） | 归档 architecture |
| docs/TYPOGRAPHY_SYSTEM.md | 有效（现行字体规范） | 保留原位 |
| docs/UI_DESIGN_V2_AXIS.md | 过期（v3 逐条否决） | 归档 architecture |
| docs/UI_DESIGN_V3_REVISED.md | 过期（被 V4 取代） | 归档 architecture |
| docs/UI_DESIGN_V4_SCOPE.md | 已完成（四批当晚全落地） | 归档 feature |
| docs/UI_PROPOSAL_CC_HAHA_2026-09-29.md | 有效（现行 UI 方向） | 保留原位 |
| docs/UI_REDESIGN_PAPER_INK_2026-09-28.md | 有效（现行 UI 主张） | 保留原位 |
| docs/WORKBENCH_CHAT_REDESIGN_PLAN.md | 已完成（工作台已上线） | 归档 feature |
| docs/WORKBENCH_MODULE_DESIGN.md | 已完成（同上） | 归档 feature |
| docs/code-analysis-report-2026-08-15.md | 杂件 | 删除 |
| docs/compose/specs/2026-06-18-groupchat-collaborative-design.md | 过期（架构不符） | 归档 architecture |
| docs/compose/specs/2026-06-18-groupchat-implementation-plan.md | 过期（同上） | 归档 architecture |
| docs/custom-app-isolation-analysis.md | 杂件（修复已落地） | 删除 |
| docs/design/DISCUSSION_MODE_DESIGN.md | 有效（群聊现行） | 迁 notes/implemented/feature |
| docs/design/GROUPCHAT_UI_REDESIGN.md | 过期（被纸墨 UI 取代） | 归档 architecture |
| docs/design/THEME_SYSTEM.md | 过期（被纸墨双主题取代） | 归档 architecture |
| docs/design/custom-app-architecture-spec.md | 有效（custom-app 线现行） | 迁 notes/implemented/architecture |
| docs/design/hermix-design-spec.md | 过期（最早视觉规范，多轮被取代） | 归档 architecture |
| docs/god-file-split-report-2026-08-17.md | 已完成（自标全部完成） | 归档 process |
| docs/groupchat-silent-failure-analysis.md | 杂件（修复后历史记录） | 删除 |
| docs/groupchat-task-engine-design.md | 有效（段位机制现行依据） | 迁 notes/implemented/architecture |
| docs/prompt-comparison-report.md | 杂件（已被吸收） | 删除 |
| docs/transport-stdio-migration.md | 已完成（全量迁 stdio） | 归档 architecture |
| 根 windows_cmd_analysis.md | 杂件（一次性生效性分析） | 删除 |
| 根 SOUL.md | 有效（agent 人格文件，运行时读取） | 保留原位 |
| 根 README.md / AGENTS.md | 有效 | 保留原位 |

汇总：删除 22、归档 19、迁 notes 12、保留 6（+根 3）。
---

## 执行结果（2026-10-01）

三个 commit，全部验证通过（`cargo check --workspace --all-targets` 零错误、
hermix-agent 1043 / hermix-core 181 / hermix-server 262 测试全绿、`tsc` exit 0）。

| commit | 内容 |
|---|---|
| `a796dca6` | 批次 A：删 16 篇杂件、归档 22 篇过期（净减 5368 行） |
| `a613c993` | 批次 B：11 篇迁入 notes 三态 + 门禁由 warn 升级为 fail |
| `1a5251fd` | 批次 C/D/E/G 的代码项 + 审批 fail-open 显式化 |

### 执行中发现并纠正的迁移表错误

1. **`CHANNEL_WORKSPACE_SCOPE.md` 在 git 历史中从不存在**。迁移表与门禁
   白名单都列了它，实际查无此文件。已从白名单剔除。
2. **两篇群聊文档改判 `rejected` 而非 `implemented`**。`80fab6be`（09-21）
   已删除整个群聊模块（前后端源码零残留），而迁移表写于 09-30，晚于删除
   9 天，"现行依据"判断已失效。已父级复核确认。
3. **`FILE_PREVIEW_REFACTOR` 改判 `implemented` 而非 `proposed`**。
   `components/preview/` 下 10 个文件按提案结构落地，PreviewPanel 736→315 行。
4. **`transport-stdio-migration` 归档而非删除**。文档自述"Phase 0 RFC 未实施"，
   但 stdio 已是活跃传输（`stdio_session.rs` + `stdio_bridge.rs`）——文档没跟上代码。
5. **实际归档 17 篇而非 19 篇**。逐项核验后，迁移表"归档"行只有 17 项存在。

### 批次 G 结论：无可执行项

- **G1（死 crate）**：23 个 crate 逐一核查依赖边，**无孤儿**。
- **G2（死脚本）**：`verify-stability-suite.py` 与 `verify-workbench-ui.cjs`
  零引用，但两者都是**一次性验收工具**（稳定性 P0-7 验收清单、UI 状态注入
  测试），按设计不通过 CI 调用。删除会丢失验收依据，故保留。

### 明确未做

- **D.1（LLM 429/5xx 重试）**：基于错误前提。`llm_retry.rs:159` 已有重试循环
  （默认 3 次 + `jittered_backoff`），`classify_error` 把 429 与 5xx 都路由进去。
- **E.3（配对错误预算按来源分桶）**：经评估**否决**。6 位码空间下 5 次上限是
  唯一的暴力破解防线，改成按 IP 反而降低安全性。
- **批次 F**（事件溯源 / Windows ACL 沙箱 / PTC）：季度级架构改造，
  按原提案保留"单独立项再评审"。