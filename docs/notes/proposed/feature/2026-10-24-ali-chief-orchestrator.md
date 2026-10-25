# Decision Note: 阿黎总管化——子代理团队编排、外部客户端调度与资源治理

Status: proposed — 主体已落地（2026-10-24 实测）：P0/P0.5/P1/P2、P2 自动重派、
P3 修正（provider 通道）、P4.1-P4.4 桌面任务台、P6 评分、P4.5 手机面 REST 均已
交付并经真实 LLM 冒烟（dispatch→成员执行→review pass 全链路）。P5 账号能力
**用户已显式启用**（2026-10-24）：持久 profile、成员 clarify 验证码环（转发到
总管对话）、registrar 角色种子与注册/运营纪律均已接线。浏览器通道对齐
现行架构：持久 profile 落在 **viewer** 的 `browser_open(profile=…)`（内建
browser_* 线 2026-10-21 已退役，profile 参数加在 viewer 侧才是活代码），
registrar 白名单用 `hermix-viewer__browser_*`（**实机勘误**：MCP 工具前缀 =
mcp_servers.json 里的服务器名 `hermix-viewer`，仓库旧文档写的 `viewer__`
是错的，已随本方案修正指引与角色种子）。**交叉复查（2026-10-25）修掉 9 项**：
P0-1 runner 每 turn 重装致 stop 失效（改进程级首装 + emitter 槽分离）；
P0-2 browser_close 删持久 profile（Session.persistent 标志）；P1 stop 不杀
内层任务 / upsert 先校验后落盘 + YAML 引号转义 / 成员剥治理工具 /
clarify 带成员归属前缀 / stale Running 重启收敛 / 持久 profile 启动前清
stale DevToolsActivePort；P2 profile 名拒绝非法字符（防碰撞串号）等。
尚未落地：手机端 UI

## 附：文件搜索索引的重复重建修复（2026-10-25 实测）

用户报告"每次都重建索引/刚建完再搜索就不行"。根因三件，全部实锤：
1. **双客户端竞态**：server 与 viewer 各有一份 `ensure_helper`，同时首次
   搜索会各拉一个提权 daemon（实机截图 55880/55881 双实例）——旧实例变
   孤儿，若快照未落盘则两个实例同时全盘扫描。
2. **孤儿收不到协议 shutdown**（endpoint.json 已被新实例覆盖）。
3. **daemon 重启后装载快照**（1.85M 条目 ~15s）期间搜索不可用，前端
   "正在搜索全盘索引"文案让用户误以为在重建（实际快照命中，indexedAt
   不变——实机验证过）。

修复：① daemon `--serve` 启动先 `stop_running_helper`（shutdown 刷盘交接）；
② 新增 stop-request.json 停止请求文件 + 孤儿看门狗（10s 轮询，罩住
endpoint 已被覆盖的孤儿；shutdown 时校验 pid 只删自己的 endpoint.json）；
③ `installTask` 协议方法 + server 握手成功后 fire-and-forget 调用：
daemon（提权）注册**开机自启计划任务**（HermixFileSearch，Logon 触发 +
HighestAvailable，索引开机即就绪免手动）。注册实现三连踩坑留档：schtasks
/TR 内嵌引号被 argv 转义损坏 → /XML 的 schtasks 在提权进程玄学报"找不到
文件"（路径/编码/存在性全验证过）→ **PowerShell Register-ScheduledTask
成功**（RunLevel 用 HighestAvailable 不是 Highest；XML 需真 UTF-16；这些
只有提权上下文才测得出——非提权一律"拒绝访问"）。

## 背景与问题

用户要求把助手阿黎升级为「总管」：在桌面端发起子任务会话、按需创建 agent
角色、组建团队、跟进进度、执行验收、按反馈优化成员；调度本机任意 AI CLI
客户端（codex / opencode / CodeBuddy / AtomCode / Qwen Code / 元宝等）协作，
优先免费额度；严控磁盘（尤其 C 盘）/CPU/内存占用；支持桌面与手机双渠道接
令；必要时自行注册网站账号（人机协同过验证码）、凭据入库、定期互动。

现状底子（已核实）：

| 能力 | 现状 | 位置 |
|---|---|---|
| 子代理派单（数据通道） | 已有：in-process 子 `AgentLoop`、同步/后台、结果回传上限 8K、lineage 落盘 | `hermix-agent/src/sub_agent_orchestrator.rs`、`hermix-tools/src/tools/delegation.rs` |
| 子代理 toolset | 3 个写死（Explore/Plan/General）——这是数据通道的参数，不是角色系统 | `delegation.rs` schema 描述 |
| 成员（长任务执行单元） | **没有**：无独立长会话 worker、无进度上报、无暂停恢复 | — |
| 任务模型 | 已有：Task 树（`parent_task_id`）、状态机、session 归属、task_tracker 工具 | `hermix-task/src/{service,repository}.rs` |
| 输出验收 | 已有但口径不同：**轮次级**两分闸（行为/结构），不是**任务级**验收 | `hermix-agent/src/acceptance_gate.rs` |
| 外部 CLI 调度 | **没有**。stdio_session/stdio_bridge 是桌面↔sidecar 内部传输，不是外部客户端桥 | `hermix-server/src/stdio_session.rs`、`src-tauri/src/stdio_bridge.rs` |
| 后台命令管理 | 已有：jobs 目录 + meta.json + 崩溃恢复（可作 worker 进程监管的参考） | `hermix-environments/src/jobs.rs` |
| 进程容器 | Job Object 杀树已有；**无 CPU/内存上限**（limit_flags 只有 KILL_ON_JOB_CLOSE） | `hermix-core/src/win_job.rs` |
| 磁盘配额 | **没有任何机制** | 全仓库检索 |
| 手机通道 | 已有：自建 relay + 配对 + E2EE + 手机审批/任务列表/vault 面 | `hermix-server/src/{mobile,mobile_ws,pairing}.rs` |
| 凭据库 | 已有：AES-256-GCM + Credential Manager 主密钥，手机 E2EE reveal | `hermix-vault/` |
| 定时执行 | 已有：automation 引擎（定义/运行态/历史三表，WorkBuddy 式） | `hermix-automation/` |
| 浏览器 | 已有：headless Edge + CDP，**每会话一次性 profile**（登录态不持久） | `os_auto/browser/{launch,job}.rs` |

历史否决（必须遵守的边界）：群聊式多 agent（聊天室 + rank/勋章经济 + 总管
调度）已于 2026-09-21 下线删除，见
`docs/notes/rejected/architecture/2026-07-02-groupchat-task-engine.md` 与
`docs/notes/archived/architecture/2026-06-18-groupchat-collaborative-design.md`。
教训：**成员互聊 = 每条消息 ×N 成员的 token 爆炸**。本方案必须是星型
（hub-and-spoke）：阿黎是唯一编排者，成员之间不通信。

## Decision

### D0 概念界定：子代理 ≠ 成员（两条通道，不可混用）

| | 子代理（现状，保持不动） | 成员（本方案新增的执行单元） |
|---|---|---|
| 存在目的 | 防主上下文污染：读/分析数据，回传压缩结论 | 独立完成长任务，对交付物负责 |
| 生命周期 | 单次工具调用内（≤30 分钟、≤12 轮、一次性） | 小时级起步，可暂停/恢复/跨天 |
| 会话形态 | 无独立会话，仅 lineage 落盘 | 独立持久会话，可查看/中断/恢复 |
| 回传物 | ≤8K 字符结论（单条 tool_result） | 进度事件流 + 交付物 + 验收 verdict |
| 深度关系 | 父子链 depth+1（预算折半） | 编排者拉起的兄弟会话，depth 重置为 0 |

`delegate_task` 及其全部上限（结果 8K、轮次、超时）是**数据通道**的正确
设计，一行不改；子代理的 3 个 toolset 同样保持硬编码。成员**不**复用这
条通道。成员内部照样可以用子代理做自己的上下文保洁——成员会话 depth 重
置为 0，其子代理拿到完整预算，保洁能力不因嵌套而衰减。

### D1 星型派单，任务中心，不复活群聊

阿黎（主会话 agent）是唯一编排点。一切协作以「任务单」为载体：分解 →
派单 → 回收 → 验收 → 汇总。成员间无直接消息通道；需要协作时由阿黎中转
**结论摘要**。控制拓扑是星型（与 delegate_task 的拓扑同形），但成员的执
行机制是新筑的（见 D4），不在子代理通道上加盖。

### D2 成员角色注册表（Agent Role Registry）

角色文件描述的是**成员模板**（执行单元的人格/能力边界），不是子代理的
toolset。借鉴 Claude Code subagents（`.claude/agents/*.md`：frontmatter
`name/description/tools/model` + 正文即 system prompt），落地为
`~/.hermix/agents/*.md`：

```markdown
---
name: reviewer
description: 对照验收标准评审交付物。派单后由总管在验收阶段调用。
toolset: read_only
model: <免费档优先>
worker: internal        # internal(成员会话) | external:<driver> | browser
resources: {mem_gb: 2, cpu_weight: 30}
version: 3
---
（该成员的 system prompt 正文）
```

- 内置种子角色随仓库分发（reviewer / explorer / planner / coder /
  browser-operator / registrar）。**派单走新工具 `dispatch_member`**
  （派单 / 跟进度 / 回收 / 验收挂钩），与 `delegate_task` 并存且互不沾染
  ——后者的 schema 与语义保持纯数据通道。
- 用户与阿黎都能增改角色（`agent_role_list` / `agent_role_upsert`，走既有
  审批闸）。
- 「根据反馈优化成员」= 阿黎修订角色文件（prompt/模型/toolset），**版本化
  落盘**，评分证据不足不得改（见 P6），用户可随时回滚到任意版本。

### D3 任务树即团队，验收是独立一环

- **组建团队 = 建一棵 Task 子树**：mission（根，归主会话 session）→
  assignment（子任务，绑 `role + worker + acceptance_criteria + budget`）。
  完全复用 `hermix-task` 的 `parent_task_id` 层级与状态机，不新造表。
- **派单时生成结构化验收标准**（deliverable / 判据 / 边界），存入
  assignment。角色间接口是「交付物 + 判据」（MetaGPT 的 SOP 思路），不是
  聊天记录。
- **验收人 ≠ 执行人**：验收由 reviewer 角色（read_only toolset、尽量异构
  模型）对照判据出三态 verdict（pass/warn/reject），复用
  `acceptance_gate.rs` 的三态语义但作用在任务级。reject 自动重派 ≤2 次，
  仍败则升级问用户——防「自评自过」。
- 进度上报：internal 成员走成员会话的进度事件；external 成员由 Driver
  上报心跳/阶段事件；统一写 task 备注 + 推 UI。子代理没有进度概念——它是
  一次性数据调用，本就不需要。

### D4 成员 Worker 三档，Driver trait 收口外部客户端

```
internal  ：成员会话——编排层拉起的独立持久 AgentLoop 会话（新机制）。
           复用 session_persistence（落盘/恢复）与 InterruptController
           （中断），监管借鉴 jobs.rs（崩溃恢复、状态落盘、pid 追踪）。
           不是 delegate_task 子代理：无 12 轮/30 分钟/8K 上限，预算与
           超时由派工单的 budget 字段决定。
external  ：本机 AI CLI 客户端，每客户端一个 Driver 实现
browser   ：现有 headless Edge + CDP（网页类任务、账号运营）
```

`ExternalCliDriver` trait：`spawn(cfg) → id`、`send(prompt/session)`、
`events() → 流`（解析为统一的 WorkerEvent）、`stop()`、`resume(session)`。

**P3 修正（2026-10-24 本机实测重定向）**：原计划的 vendor 顺序按公开生态
（gemini/codex/opencode）排列，但用户实际只装 ZCode、MiniMax Code、
CodeBuddy CN 三家，且逐个探测后结论是**当前都没有可用的无头入口**：

| 客户端 | 探测证据 | 接入方式 |
|---|---|---|
| ZCode | `E:\zcode` 为 Electron 桌面应用（`ZCode.exe`），无独立无头 CLI；`~/.zcode/cli` 只是会话数据 | 不作为外部 worker——它与宿主同类，其角色由 internal 成员承担 |
| MiniMax Code | 旧无头入口 `~/.minimax/bin/mavis.cmd` 指向 `E:\minimax\MiniMax.exe`（路径已不存在，升级后布局变了）；新装 `E:\MiniMax Code` 的 daemon 闭在 `app.asar` 内无外部入口；`mcode-tools` 只是连接器/auth 工具集 | **走 provider 通道**：hermix 已配 `minimax-cn`（Anthropic 兼容端点），成员角色用 `model` 覆盖即可用其额度；`worker: external:mavis` 保留为「官方无头入口回归后再接」 |
| CodeBuddy CN | `E:\CodeBuddy CN\bin\buddycn` 的 CLI 是 VSCode 式**编辑器启动器**（diff/merge/open 参数面，无 agent 无头模式） | 降级为启动器集成：`buddycn <dir>` 打开工作区给人看（人在环验收时有用），不作为 worker |

因此 P3 的执行内容改为：① **provider 优先**——外部客户端的免费额度经
API provider 进成员系统（机制已就位：角色文件 `model` 字段）；②
Driver trait 与 `worker: external:<driver>` 的角色通路保留（P1 已留好，
派单时报错给出现状指引），待任一客户端恢复无头接口即插即用；③
CodeBuddy 启动器集成按需后置。原 gemini/codex/opencode 顺序仅在未来
用户装机后重启。

**免费优先路由**：worker 池带 per-vendor 配额计数器（requests/day），调度
策略「internal（自家免费代理）→ 免费档外部 → 付费外部（需用户批准）」，
额度耗尽自动降级并留痕。

### D5 资源治理是硬门槛，先于任何扩容（P0）

已核实：Windows **没有** per-process 磁盘配额；FSRM 仅 Server SKU；Job
Object 管 CPU/内存/进程数，不管磁盘与 IO 带宽。因此：

- **CPU/内存**：扩展 `hermix-core/src/win_job.rs`（手写 FFI 已在，加常量
  而已，不引 win32job crate）：`JOB_OBJECT_LIMIT_PROCESS_MEMORY`（per
  worker 默认 4GB，可按角色配）、CPU rate control（weight 式，前台交互
  优先）、active process limit。所有 external worker 必须进 job。
- **磁盘**：三道闸——
  1. **写入白名单**：每 worker 一个 scratch 目录，放用户指定盘（默认非
     C 盘），需要写入工作区时由 host 以 junction 映射；目录体积 host 侧
     统计，超配额（默认 5GB/worker，可配）即停派 + 要求清理。
  2. **缓存清单治理**：C 盘膨胀的真实来源是 npm/cargo/pip/日志等缓存
     目录——维护缓存目录清单 + 定期 GC 工具（复用 jobs.rs 的目录化思路）。
  3. **C 盘水位**：剩余空间低于阈值（默认 10GB）全局停派 + 桌面/手机告警。
- **IO/前台体验**：worker 进程 `SetPriorityClass(BELOW_NORMAL)`，避免风
  扇起飞、前台卡顿；并发 worker 数默认 2，按核数上限扩。
- 内存配额超限的 worker 由 Job Object 直接终结，host 记 lineage 后可重派。

### D6 双渠道指令面

- **手机**：现有 relay + 手机 WS + E2EE + 审批通道即入口，新增 mission
  面——下发/查看任务树、进度看板、验收审批、以及账号注册的「验证码请求」
  推送（用户回填，走 E2EE）。不新建通道。
- **桌面**：新增「任务台」看板视图（vibe-kanban 式：每张卡 = 一个
  assignment，列 = 待派/进行中/待验收/完成），成员、进度、验收 verdict
  一屏可见。

### D7 账号注册与运营：已启用（用户 2026-10-24 显式授权）、人机协同、保守合规

- **前提设施**：① 浏览器加 per-platform **持久 profile**（登录态保活，
  storage_state 复用；这是对现行 throwaway-profile 设计的产品级修订，
  需要显式决策）；② 凭据入 `hermix-vault`（已具备，含手机 E2EE reveal）；
  ③ 「定期互动」用 `hermix-automation` 引擎定义内容计划（已具备调度/历史/
  退避）。
- **注册流程人机协同**：阿黎填表 → 遇手机验证码 → 手机推送「请回填验证
  码」→ 用户输入 → 继续；30 分钟无人响应则挂起并告知，不自动重试。邮箱
  注册可由阿黎自建邮箱账号（同样走 vault）。
- **合规红线（写进角色 prompt + 工具层硬限制，不可配置绕过）**：一平台
  一账号；不做批量注册、养号、刷量、验证码自动识别或接码平台；发布内容
  须用户预审或明确授权模板；互动频率保守（automation 默认日级）。封禁/
  法律风险由用户决策承担，产品默认关闭该能力，开启需用户显式确认。

### D8 前端：总管面板（任务台）——总管负责的一切必须在阿黎界面可见

原则：**编排的每个状态转移都对应一个可见的 UI 位置**。看不见的编排等于
没发生——用户无法信任一个黑盒总管。落地为一级视图「任务台」，主侧栏入口
（导航图标保持单色描边，遵守导航例外规则）。

**四个页签：**

1. **任务**（看板，默认页签）：列 = 待派 / 进行中 / 待验收 / 已完成 /
   失败待处置；卡片 = 一张派工单。卡片元素：成员图标（`IconTile` 实心
   色块）、任务标题、进度（阶段事件推导，非百分比造假）、预算消耗、
   worker 徽标（internal / external:vendor / browser）、验收状态用**单侧
   色条**（G6b 形态）。点开卡片 → 详情抽屉：派单原文、验收判据逐项
   pass/fail、成员执行时间线（internal 成员可跳转完整会话查看；external
   显示 Driver 事件流）、重派 / 中止操作。
2. **团队**（成员库）：复用 `AgentsView` 的 `AgentCard`/`AgentForm`/
   AI 生成向导壳，但**persona 与 member 分为两类**（聊天人格 ≠ 打工成
   员，数据模型与入口都分开）；成员卡显示角色版本、评分走势（P6 数据）、
   运行中实例（当前任务、CPU/内存占用）。
3. **资源**：worker 池状态、各盘水位条（C 盘高亮）、scratch 目录配额
   用量、免费额度余额（per vendor 当日剩余次数）、整机 CPU/内存曲线。
   数据全部来自 server 事件流，不做轮询假进度。
4. **账号**（P5 落地后才出现，功能未开启则该页签不渲染）：平台列表
   （IconTile + vault 关联）、登录态健康（过期提醒）、互动计划（与
   AutomationView 联动）。

**对话内嵌**：阿黎主对话流中，派单发生时插入任务卡片（复用
`TodoListCard`/`ToolEventCard` 的卡片语言与折叠交互），进度 live 回流；
验收完成时卡片翻转为 verdict 态。总管 = 阿黎本人，任务台是她能力的展示
面，不是另一个「应用」。

**通知中心**（人机协同统一入口）：验证码请求（带倒计时与回填输入框）、
验收升级待决、资源告警（C 盘水位/配额触顶）、免费额度耗尽降级提示。桌面
toast（`ToastContainer`）+ 手机推送（relay），统一沉淀到通知列表。

**数据通道**：server 侧 mission 事件（task 状态、成员进度、verdict、资源
水位、配额计数）经既有 WS / stdio-frame 通道推送，前端订阅入 store；手机
mission 面消费同一事件流，两端不各造一套状态。

**UI 纪律（仓库硬约束，本视图无豁免）**：颜色/字号/圆角/阴影只走令牌；
语义四色文字用 `--color-text-{success,warning,error,info}` 安全档；表单
控件高度与图标尺寸走档位；页面容器用 `SettingsSection` 族、列表行用
`ToolListRow`、开关用 `ToggleSwitch`、按钮用 `btn-flat`/`chip-flat`；过
check-design-guard / check-design-primitives / check-theme-tokens /
impeccable detect 四道门禁 + `tsc --noEmit`。

### 分期落地（每期独立可验收）

| 期 | 内容 | 验收标准 |
|---|---|---|
| P0 | 资源底座：win_job 加内存/CPU/进程上限；worker scratch 白名单 + 体积配额；C 盘水位监控；BELOW_NORMAL 优先级 | 拉一个内存爆炸的样本进程被 job 终结；scratch 超配停派；C 盘低水位告警可达手机 |
| P1 | 成员会话与角色注册表：internal 成员 = 独立持久会话运行器（监管/中断/恢复，无 12 轮/30 分钟/8K 上限）；`~/.hermix/agents/*.md` 成员模板解析 + 内置角色文件化 + 新工具 `dispatch_member` | 一个 internal 成员跑通 30 分钟以上长任务，进度实时可见、可中断可恢复；角色文件热加载；`delegate_task` schema 一字不动 |
| P2 | 验收闭环：派单生成 criteria；reviewer verdict 三态；reject 重派 ≤2 次后升级 | 端到端：一个含错任务被 reject 并重派成功 |
| P3 | external worker（**2026-10-24 重定向**：三家本机客户端均无无头入口 → provider 优先——角色 `model` 覆盖接 minimax-cn 等免费额度；Driver 通路保留待接口回归；CodeBuddy 启动器按需） | 内置/示例角色带 `model` 覆盖，成员工单确实经该 provider 跑通；external 派单报错给出现状指引 |
| P4 | 总管面板（D8 拆五步）：P4.1 mission 事件通道与前端 store；P4.2 任务看板 + 详情抽屉；P4.3 团队页签（复用 AgentsView 壳）+ 资源页签；P4.4 对话内嵌任务卡 + 通知中心（验证码环）；P4.5 手机 mission 面对齐同构 | 桌面看板列状态与 task 库一致；手机可下发任务并看到 verdict；四道 UI 门禁 + tsc 全绿 |
| P5 | 账号运营：持久 profile、注册向导（验证码环）、vault 记录、automation 互动模板 | 一个平台完成注册→凭据入库→定时发帖（用户预审内容） |
| P6 | 成员自优化：lineage 统计（成功率/成本/时长）→ 角色修订建议 → 版本化 A/B | 低分角色被建议改 prompt/模型，改后两个任务证据优于改前才落版本 |

## GitHub 借鉴对照

| 来源 | 借什么 |
|---|---|
| Claude Code subagents | 成员角色文件格式（frontmatter + description 触发语义 + 正文即 prompt）——只借格式，不借其瞬时调用语义 |
| vibe-kanban（BloopAI，Rust+React/Tauri 同栈） | 任务-会话-工作者的看板建模；多 agent 后端适配层的形状 |
| claude-squad / Conductor | 「每个工作者一个隔离会话/worktree」的管理抽象与会话生命周期 |
| LangGraph（supervisor 模式） | 验收/人审环用显式状态机 + checkpoint 建模（reject→重派→升级的转移） |
| MetaGPT | 角色间以「交付物文档 + 判据」为接口，而非聊天 |
| AutoGen（反面） | 多 agent 互聊的 token 爆炸教训——星型架构的直接论据 |
| codex app-server / opencode serve | external worker 的驱动协议事实标准 |

## Alternatives considered

1. **引入 AutoGen/CrewAI/LangGraph 作编排引擎**——否。Python 生态与 Rust
   宿主不符；对话式路由每轮烧编排 token；`run_loop` + `delegate_task` 已
   覆盖其核心价值，增量改造远低于引入外引擎。LangGraph 仅借鉴状态机建
   模思想。
2. **复活群聊总管（rank/勋章经济，多 agent 聊天室）**——否，已被
   `2026-07-02-groupchat-task-engine.md` 否决并整体删除。星型派单是对其
   教训的直接回应。
3. **internal worker 也用独立进程隔离**——否。internal 成员的隔离单位是
   **会话**（独立持久 AgentLoop 会话 + 预算 + 中断控制），进程化只增加
   启动成本与 IPC 复杂度；进程边界留给外部 CLI（天然进程）与需要硬资源
   上限的场景（Job Object 在 P0 统一供给）。
4. **用 MCP 统一所有 worker 接口**——部分吸收。codex（`codex mcp-server`）
   与 claude（`claude mcp serve`）可用 MCP，但 opencode 的 HTTP API 更稳，
   gemini headless 是纯 CLI。统一收口在我们自己的 Driver trait，MCP 是其
   中一种实现，不作为唯一协议。
5. **Docker/WSL 做资源限制**——否。桌面产品不能依赖用户装 Docker；WSL
   的 vhdx 自身会膨胀，恰是要防的问题。
6. **FSRM/NTFS 配额做磁盘限制**——否。FSRM 仅 Server SKU；NTFS 配额按
   用户不按进程，且对管理员账户无效。
7. **引 win32job crate**——否。需求只有几个常量与一次
   SetInformationJobObject 扩展，`win_job.rs` 手写 FFI 延伸即可（六步阶
   梯：现有依赖优先/最小实现）。
8. **验证码自动识别 / 接码平台全自动注册**——否。合规与安全红线；人机
   协同（手机收码）是产品特性而非妥协。
9. **把成员做成 delegate_task 的放宽版（抬高轮次/超时/结果上限）**——否。
   子代理的上限是为上下文保洁定的，放宽后它仍是数据通道：进度不可见、
   不可暂停恢复、8K 结果对长任务交付物是有损压缩、depth 链还会把成员
   嵌进父预算。执行单元需要的是**会话与监管**（独立会话、事件流、可恢
   复），不是更长的瞬时调用。

## Failure modes（对抗性审查）

- **外部 CLI 协议漂移**（codex resume 已知 bug #24502 即是先例）：Driver
  收口 + 事件解析 golden 测试 + 启动时版本探测，不兼容即拒派该 vendor
  并降级；接受「适配器跟版本跑」的长期维护税。
- **多 worker 并发把机器拖死**：P0 硬门槛先行；并发默认 2；CPU weight +
  BELOW_NORMAL + 内存硬上限三重保险。
- **C 盘被缓存而非 scratch 撑爆**：缓存目录清单化 GC 是独立一闸，不只盯
  worker 目录。
- **免费额度被单一类任务烧光**：配额分池（coding/浏览器/运营），跨池不
  借用；耗尽降级链留痕可审计。
- **验收者与执行者同模型同 prompt → 自评自过**：reviewer 强制 read_only
  toolset + 尽量异构模型 + 结构化判据（有客观项可程序化核验的优先程序
  核验，如测试通过、文件存在）。
- **角色文件被 LLM 越改越差**：版本化 + 评分回归门槛（P6：两个任务以上
  证据）+ 用户可回滚。
- **junction 在 OneDrive/受控文件夹下失败**：探测降级为普通目录 + 告知
  用户，不静默失败。
- **手机验证码环节用户不在**：挂起 30 分钟后中止并汇报，不自动重试注册。
- **账号运营的封号/法律风险**：默认关闭、一平台一账号、内容预审、保守
  频率（D7 红线）；工具层硬限制并发注册数 = 1。

## Consequences

- **前端是一大块独立工程量**：任务台四页签 + 对话内嵌卡 + 通知中心 +
  手机面对齐（P4 五步），且受仓库四道 UI 门禁约束；复用 AgentsView 壳与
  既有卡片语言是为了把这块控制在「新视图」而非「新设计系统」的量级。
- **维护面扩大**：N 个外部 CLI Driver 是长期维护税，协议漂移必然发生；
  通过 golden 测试与拒派降级把税限制在单 vendor 内。
- **验收是新增成本**：每张派工单多一轮 reviewer 调用；可用免费档承担，
  并允许用户对低风险任务关闭验收。
- **P0 不产出用户可见能力**：资源底座先行是刻意的——没有它，放开多
  worker 就是磁盘/CPU 事故制造机。
- **账号运营功能自带 ToS/法律灰度**：产品默认关闭 + 红线硬编码，风险边
  界向用户显式披露。
- **角色文件成为新的用户可见表面**：需要文档、UI（角色管理）与导入导出，
  是一块新的产品面。
- **元宝等纯 C 端客户端明确不接入**：无可编程入口，UI 自动化硬凿的脆性
  不值得；用户预期需要提前对齐。
