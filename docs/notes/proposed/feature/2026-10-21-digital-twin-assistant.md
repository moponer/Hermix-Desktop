# Decision Note: 分身助理（Twin）—— 每个桌面端一个会成长的专属助理

Status: proposed — 需求已明确（6 条约束 + 独立入口 + 可配权限），未实施。最大前置项=代批的身份与审计（见「权限模型」）。

## Decision

为 hermix 桌面端配置一个**单例、持久、可成长**的助理实体，定位是用户的"分身"。她不是又一个聊天会话，也不是子代理，而是：

1. **代理人**（delegate）：能代表用户派活、建自动化、整理资料、更新工作台；
2. **镜像**（mirror）：持有用户的兴趣、项目、习惯、沟通偏好，且每条事实带证据；
3. **前台**（front desk）：用户不在时替她判断"这事要不要说、要不要做"。

她把此前设计的「守望者」吸收为自己的 L0–L2 子能力（监督执行），也就是说：**守望者是她的一个职责，不是另一个实体**。见 [守望者笔记](../architecture/2026-10-21-watcher-resident-agent.md)。

三条不可动摇的边界（全部来自前一阶段已拍板的约束）：

- **无状态唤醒**：LLM 是无状态的，她的"常驻"只能是「持久身份 + 短期唤醒」。真正的常驻状态是文件，不是内存里的 loop。
- **成本自律**：token 不限但要自己省。每轮记 `cost`，超软阈值降级为"只记不说"而不是停机。
- **成长可回滚**：凡是"她学到的"，必须是可查看、可 diff、可删除的结构化文件，不是一段越来越长的自我叙述。

## 需求 → 能力 → 缺口（逐条对账）

用户提出的 13 条需求，逐条落到现有代码上。**每一项都有落点，但其中 5 项有具体缺口**——这决定了这不是从零造，而是接线 + 补洞。

| # | 需求 | 现有能力 | 缺口 |
|---|---|---|---|
| 1 | 固定、可持续成长的专属助理 | `$HERMIX_HOME/agents/*.json` 人格、`SOUL.md` | 缺单例注册 + 备份/迁移（换机带走） |
| 2 | 长期记忆 | `MEMORY.md` / `USER.md`、`data/memory.db`（frames）、`daily_memory.db`、`session_search` | **`daily_memory` 只有写没有读**（工具 schema 只有 `section`+`content`，`list_days` 仅被 `memory_prefetch` 内部消费）→ 兴趣学习没有输入 |
| 3 | 主动学习 / 梳理资料 / 了解爱好 | `memory_prefetch`（被动注入）、`recall_scheduler` | 缺**主动提炼循环**与 `profile/` 结构化画像 |
| 4 | 发现新内容整理后主动告知 | 通知通道（见守望者 P1） | 缺**去重水位线**（`sources.json`） |
| 5 | 用户可与其对话 | `AgentLoop` + session | 需要她的专属 session 形态（可枚举、可回看） |
| 6 | 主动创建自动化任务 | `cronjob` 工具（action: create/list/update/pause/resume/remove/run） | **不能指定投递目标**；且自动化有三套机制（见下节） |
| 7 | 协助制定并落地工作流 | `workflow` 工具（save/list/remove，带 `{{参数}}` 占位） | **不能排期、不能触发** → "落地"缺最后一环 |
| 8 | 用户与她均可随时启动自动化/工作流 | `cronjob action=run` | 工作流没有触发入口 |
| 9 | 全面掌握 hermix 能力 | `tool_search` / `tool_describe` / `tool_call` | **只能搜 deferred 桶，没有全量枚举**（`execute` L286-287 只取 deferred；resident 与 bridge 工具被排除）→ 需加 `action=list` 分支 |
| 10 | 主动访问网络站点 | `web` 工具 | 无 |
| 11 | 从指定社区自动抓取并整理 | `web` + `terminal` + 文件工具 | 缺订阅源模型与增量机制 |
| 12 | 介入工作台完成数据更新 | `workbench_manager`（read/write/lint/index/undo，写入前 lint 闸门） | 有能力，缺**按领域的授权分级** |
| 13 | 整体定位"分身" | — | 概念层，见下 |

结论一句话：**她 80% 的能力已经躺在仓库里，缺的是把散件接起来 + 5 个具体的洞。**

## 自动化落点：建在引擎 A（现役），不是三表

**结论：建在引擎 A（`dashboard/automation.rs` + `automation_tasks.json`）。**

这条结论经过两轮修正，第一轮我说"三表空转所以要还债"，是**错的**——引擎 A 不是没人用的死代码，而是**今天刚被大修并端到端验证过的现役主力**。权威依据是 `docs/notes/implemented/bug-fix/2026-10-21-automation-create-run-wiring-broken.md`（2026-10-21 落地）。

### 三套机制的真实状态

| 机制 | 存储 | 谁写 | 状态 |
|---|---|---|---|
| **引擎 A**：`dashboard/automation.rs` 的 `AutomationScheduler::tick_once`（30s） | `$HERMIX_HOME/automation_tasks.json` | 面板 CRUD + `cronjob` 工具（`DashboardCronjobBackend`，`state.rs:1284-1289`） | **现役**。2026-10-21 四个提交大修：触发判定（旧实现 `after(&now)` 取严格未来时刻，`next_at <= now` 恒假 → **任务根本不触发**）、单飞（`running_since` + `RUNNING_STALE_MINUTES=120`）、认锚点（`anchor_session_id` 之前存了没人读）、启动清陈旧在途标记、回退会话按任务固定 |
| **引擎 B**：`automation_trigger.rs` + `hermix_automation::AutomationScheduler` | `sessions.db` 三表 | **没有人**（无路由、无工具 create） | 空转。最后实质改动 2026-10-05（`DeliveryTarget` 纯函数 + 投递计数） |
| `hermix-cron` | `cron/*.json` / `cron_jobs.db` | — | 更早的孤儿：唯一构造点在 `cli_support.rs:84`，仓内无 crate 依赖它 |

关键细节：**两套不是竞争关系，而是"A 干活 + B 出规则"**。引擎 A 的 `spawn_run` 已经 import 并调用引擎 B 的 `DeliveryGuardInput` / `decide_delivery_target`（`automation.rs:911-918`）——护栏只有一份，判定规则不重写。

### 为什么不现在合并到引擎 B

既有笔记已经给出理由，仍成立：引擎 B 的领域模型比 `AutomationTask` **窄**——没有 `skills` / `max_runs` / `description` / `category` / `icon` / `permission_level` / `workspace_path`。合并要么给 `automations` 表加列（动 `hermix-automation/src/repository.rs`），要么把这些字段搬进 SQLite 领域模型。它在笔记里被记为**后续项**，而不是做错了。

引擎 A 的真实弱项（相对于 B）也要如实记：没有 run 表那套幂等 / 退避 / `abandoned` 收割，去重是"槽 + 60 秒"加 `running_since` 单飞——够用但弱。**合并时要把这套护栏一起带过去，不能只搬调度。**

### 于是 Watch 的落点

- 复用引擎 A 的 JSON 任务表（用户可见、工具可写、今天刚验证过）
- `Watch` 需要的新字段（`watch_target` / `done_contract` / `event_key`）走 `AutomationTask` 的 `serde(default)` 扩展，不动 SQLite
- `event_key` 去重不能复用"槽 + 60 秒"（那是时间槽语义），要独立一个"上次处理的 event_key 集合"放在任务条目里

### 事件触发的返工点（若将来合并到引擎 B）

1. **`schedule_type` 是封闭两值**（`Recurring` / `Once`），`scheduler.rs:170-194 is_due` 把 `(schedule_type, next_run_at, last_run_at, now)` 当纯时间函数；事件没有 `next_run_at`。
2. **`compose_run_id(automation_id, trigger, scheduled_at)` 建在时间槽上**（`lib.rs:89-94`）——事件的标识是 `event_key` 而非时间槽，不复用会重放或漏触发。
3. **`RunTrigger::parse` 是白名单**（`lib.rs:62-68`），加 `Event` 变体必须同改 parse，否则新行读回来是 `None`。

## 权限模型：独立入口 + 可配置权限

用户的两个决定——**独立入口**、**权限可设置且完全权限 = 用户本人的最高操作能力**——在现有代码里几乎已经存在，只是缺"身份"与"可追溯"。

### "完全权限"就是现成的 `PermissionLevel::Full`

`PermissionLevel` 只有两个变体：`Default` / `Full`（`hermix-core/src/traits.rs:295-301`）。`Full` 的语义在 `local_agent_service.rs:1463-1465` 短路：**跳过 Tirith 策略 + RiskLevel 闸 + 审批卡**，直接 `Allow`。

而且 `AutomationTask.permission_level: Option<String>` 已经是这条通道的开关（`dashboard/automation.rs:87-90`，运行时 `"full"` → `PermissionLevel::Full`，L964-969）——**引擎 A 建任务时可以逐条指定权限级别**。所以"完全权限"不需要新造模型。

一条重要的边界：**L0 hardline 黑名单仍在 `Full` 之前生效**（`local_agent_service.rs:1451` 早于 `1463`），按"工具名 + 参数模式"拦截。所以"完全权限"= 用户本人能做的事，**不是无限的**——这恰好符合"分身"的语义：她与你是同一个人，所以权限等值，而非超越。

### 缺的三件（按阻塞顺序）

1. **身份维度缺失**：`PermissionLevel::Full` 无法表达"以谁的身份"。审计里分不清"用户点了 Full"和"分身代批"。需要 `Delegated { by, scope }` 或给 `Full` 挂一个 actor。
2. **裁决无 actor 回传**：`ApprovalOutcome::Approved`（`hermix-core/src/approval.rs:32-49`）不带"谁批的"；`ApprovalRecord`（`session_persistence/mod.rs:152-190`）也没有 `actor` / `decided_by` 字段。**代批因此不可追溯**——这是"完全权限"最危险的地方。
3. **委托无持久化载体**：`UserAllowlistEntry` 缺 `owner` / `expires_at` / `delegation_id`；`session_bypass` 是纯内存 `HashMap`（`state.rs:192`），重启即失效。

### 设计：权限挂在入口，不挂在人格

**入口携带权限级别**，而不是"她这个人被永久授予 Full"。同一个助理在不同入口下权限不同：

| 入口 | 默认权限 | 说明 |
|---|---|---|
| 日常对话入口 | `Default` | 危险操作弹审批卡 |
| "放手模式"（用户显式切换） | `Full` | 跳过审批，但**每一笔工具调用仍落 `approval` 行**，且带 `actor: twin` |
| 她创建的自动化任务 | 逐任务 `permission_level` | 已有字段；默认 `Default`，用户可对单条任务放开 |
| 定时/A3 自主行动 | 永远不高过"最近一次用户授予的级别" | 用户下次打开入口若降级，自主行动同步降级 |

"放手模式"的三个必备护栏：

1. **一切可追溯**：`ApprovalRecord` 加 `actor`（`user` / `twin` / `automation`），用户可按 actor 过滤 review。
2. **一键回收**：切换权限是一个可撤销的状态，不是一次性的授权；入口上常驻显示当前级别。
3. **按域而非全局**：建议 `Full` 之上再分域（文件 / 终端 / 网络 / 工作台 / 外部 CLI），至少先做到"能单独关掉终端执行"——因为那是唯一能造成不可逆破坏的域。

### 独立入口的形态

独立入口（不是侧栏里的一个 session），理由是：她要能被随时叫出来、且权限级别要显眼。形态上是一个独立路由 + 独立窗口（与形象层同一个载体，见守望者笔记「需求约束 6」的透明窗口 spike）。她的历史对话仍然落在 `sessions.db` 里（可回看），只是入口不在会话列表里。

## 记忆分层：让"成长"可查看、可回滚

```text
$HERMIX_HOME/twin/
  identity.md          # 名字、称呼、口吻        冻结层：用户可改，她不可写
  charter.md           # 职责、授权边界、L3 清单   冻结层：用户可改，她不可写
  profile/
    interests.json     # 兴趣与关注点（每条带证据链接）
    projects.json      # 项目与进展
    people.json        # 你提到过的人与关系
    style.md           # 沟通偏好（要不要主动问、说多长）
  ledger.json          # 证据账本：她的判断 vs 实际结果、介入被采纳/被忽略
  sources.json         # 订阅源 + 水位线（上次看到哪）
  journal/YYYY-MM-DD.md  # 只追加，每轮带 cost
  notes/               # 她整理出的资料
```

三层不可混：**冻结层她不可写**（防人格漂移）、**学习层她可写但带证据与版本**（可 diff 可回滚）、**产物层只追加**。

### 为什么"每条事实带证据"是硬要求

没有它，"主动了解用户爱好"就会退化成模型幻觉——她会写出"用户喜欢摄影"，然后你永远查不到依据。要求形态是：

```json
{ "fact": "偏好 Rust 而不是 TS 做后端", "evidence": ["session:abc#turn12", "2026-10-19 对话"], "confidence": "explicit" }
```

`confidence` 三档：`explicit`（用户明说）、`inferred`（从行为推断）、`stale`（超过 90 天未印证）。**`inferred` 的事实不得用于主动打扰**，只能用于检索排序。这条是防"自作聪明"的闸门。

### 三个学习循环，频率递减

| 循环 | 频率 | 输入 | 产物 |
|---|---|---|---|
| 对话后 | 每轮，几乎零成本 | 刚结束的对话 | journal 追加候选事实（不改 profile） |
| 每日 | 一次 LLM | journal + `daily_memory`（需先补读能力） | 更新 `profile/`，需证据 |
| 每周 | 更低频 | `ledger` + profile | 校准：标 `stale`、问她"还关注吗" |

## 主动性分级（"分身"最危险的地方）

她越能干，越可能替你做错决定。所以主动性必须是分级的、按领域授权的。

| 级 | 她可以 | 默认 |
|---|---|---|
| A0 只观察 | 写 journal、更新 ledger | — |
| A1 只研究不说 | 主动查资料、整理进 `notes/` | — |
| A2 主动说 | 发现重要的事通知你（去重后） | **默认级别** |
| A3 主动做 | 创建自动化、整理文件、更新工作台 | 需**按领域白名单**授权 |

与守望者笔记的三级授权合并：A0/A1 ≈ L1 自决，A2 ≈ L2 代发可撤销，A3 ≈ L3 必须问（按领域逐步放开）。

**证据门**：任何 A3 动作前，她必须能回答"依据是什么"并写进 journal。否则会出现"她凭空建了 8 个自动化，你一个都不想跑"——这是这类功能最典型的翻车方式。

## 主动告知的命门是去重，不是抓取

抓取是简单部分（`web` + `terminal` 够用）。**这类功能死于"每天推同样的东西"**。

`sources.json` 每个源记一个水位线，不同类型的源用不同水位：

- 订阅源（RSS/Atom）：`last_guid` 或 `last_published_at`
- 固定页面：`content_hash`
- 搜索关键词：`top_n_fingerprint`（上次前 10 条结果的标题指纹集合）

判定规则：**只有"新"且"与 profile.projects/interests 相关"才进入 A2 通知**；新的但无关的，进 `notes/` 等待被问。这条规则让她的通知量天然收敛。

## 全面掌握 hermix 能力

两个来源，都不靠"把 50 个工具塞进 system prompt"：

1. **工具自省**：给 `tool_search` 加 `action=list` 分支（几行），全量枚举 name + description。她现在只能按关键词盲搜 deferred 桶，resident 与 bridge 工具永远搜不到——这是"掌握不全"的直接原因。
2. **能力地图**：`builtin-skills/hermix/twin/SKILL.md`，12 类能力 + "什么时候该用哪个" + 常见误区。走现有 skill 机制，随版本更新，比硬编码省 token。

两者合起来才是"知道自己会什么"，而不是"背下来了但记错"。

## 落地顺序

七步，按"输入源 → 身份 → 主动性 → 执行 → 形象"排。前三步不碰任何现有行为，风险为零。

| 步 | 内容 | 依赖 |
|---|---|---|
| 1 | `daily_memory` 补读能力 + `tool_search` 加 `action=list` | 无——这是她的两个输入源 |
| 2 | 能力地图 `builtin-skills/hermix/twin/SKILL.md`（12 类能力 + 何时用哪个） | 无，走现有 skill 机制 |
| 3 | `twin/` 身份骨架 + 对话后写 journal + 可查看/可删 | 无 |
| 4 | 每日提炼 `profile/`（带证据 + confidence）+ A2 通知 | 1、3 |
| 5 | `sources.json` 水位线 + 增量告知（去重） | 4 |
| 6 | 执行层：`workflow` 补触发、`cronjob` 补投递目标、A3 领域授权、代批身份与审计 | 引擎 A（现役） |
| 7 | 形象层：透明置顶穿透窗口 | 透明窗口 spike 结论 |

第 6 步里的"代批身份与审计"是唯一有架构风险的项（动 `hermix-core` 的 `PermissionLevel` / `ApprovalOutcome` / `ApprovalRecord` 三处），建议单独立项，不和功能混在一个 PR。

## Alternatives considered

1. **给她独立 crate（`hermix-twin`）** — 否决。她的执行引擎（跑一次 AgentLoop、投递、熔断）全部复用现有路径；独立 crate 只会让"哪个在跑"更难回答（本项目已有 automation/cron/task/kanban 四套在记债）。
2. **常驻 `AgentLoop` actor** — 否决，理由同守望者笔记：LLM 无状态，常驻只能体现为膨胀的上下文 + 7×24 待机成本。
3. **把成长做成"定期让模型写自我总结"** — 否决。产出漂亮废话，且污染后续 prompt；且无法回滚。
4. **兴趣画像自动全量采集（读所有会话全文）** — 否决。上下文成本与隐私感双重不可接受。改为：daily 循环只读 journal（已压缩的候选事实）+ `daily_memory`（三行摘要）。
5. **复用 automation 三表（引擎 B）** — 否决，但不是因为它"空转"（那个判断我一开始下错了）。真正的理由是：引擎 B 领域模型比 `AutomationTask` 窄（缺 skills/max_runs/description/category/icon/permission_level/workspace_path），合并要给 `automations` 表加列；而引擎 A 今天刚修好并端到端验证，此刻动它是最高风险。既有笔记已把合并记为后续项。
6. **现在就合并两台引擎** — 否决（同上）。终态是引擎 B，但顺序是"先让分身跑在 A 上、拿到真实需求，再一次性合并"，而不是先做一个月的纯重构。
7. **把 13 条需求各自的缺口一次性补齐** — 否决。先补"记忆输入"（`daily_memory` 读能力）与"能力自省"（`tool_search list`），这两项是所有主动行为的输入源；其余按分期来。

## Consequences

**放弃的东西**

- 不做全量监视。她只盯用户点名的对象，因此**永远发现不了没被点名的意外**。这是用"不打扰"换"不完全覆盖"。
- 不做跨机器同步她的状态。`twin/` 目录跟随 hermix_home 迁移，不做多端实时一致。
- 不做"她有独立于你的价值观"。`charter.md` 由用户写死，她只能优化执行方式，不能优化"要不要听你的"。
- 不做无证据画像。宁可画像稀薄，也不要一条查不到来源的"你喜欢 X"。
- 不做 A3 的默认开启。所有主动执行都需要先按领域授权，冷启动阶段只有 A2。

**欠下的债 / 落地前置**

1. **分身代批的身份与审计**（本笔记最大的前置项）：`PermissionLevel` 加身份维度、`ApprovalOutcome::Approved` 加 actor、`ApprovalRecord` 加 `actor` 列、allowlist 条目加 `owner`/`expires_at`。**在它落地前，"完全权限"只能做成"用户手动切换的 Full"，不能做成"她自动代批"**。
2. **两台 automation 引擎合并**（后续项，非阻塞）：终态是引擎 B；合并时要把引擎 A 的单飞/陈旧标记清理一起带过去。
3. `daily_memory` 补读能力（否则兴趣学习没有输入）。
3. `tool_search` 加 `action=list`。
4. `workflow` 补触发入口（否则"落地工作流"缺一环）。
5. `cronjob` 补投递目标参数。
6. 透明置顶穿透窗口的 spike（结论影响形象层形态，见守望者笔记「需求约束 6」）。
7. `Watch` 的 `event_key` 去重键设计（与选哪张表无关）。

**仍不确定**

1. **主动性默认级别**是否就定 A2？A1 更保守但反馈更慢（用户不知道她在学）。
2. **A3 领域白名单的首批范围**：建议只放"整理类"（归档、写 notes、更新工作台只读汇总），不放"改代码/删文件"。
3. **她的会话形态**：独立 session（侧栏可见）还是独立入口（像"设置"那样）？前者简单，后者更符合"分身"的定位感。
4. **`twin/` 的体积控制**：profile 与 journal 会持续增长，何时压缩、压缩到什么粒度，需要一个明确的策略，否则三年后她的启动读入就超预算。
