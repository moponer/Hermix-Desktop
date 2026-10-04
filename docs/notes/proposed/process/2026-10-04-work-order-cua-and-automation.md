# Decision Note: 电脑控制 / 自动化 开工清单

Status: implemented — 2026-10-04 五项全部按序执行完毕。第 1/2 项已落代码并提交，第 3/4/5 项已落方案笔记。**核实过程中推翻了本篇三处判断**（`auto_id` 注入链、"两套词汇"、观察类降档的定性），逐条记录在对应笔记里。本篇转为执行记录，改动后的排序与裁决见各专项笔记。

## 执行结果

| # | 项 | 状态 | 落地位置 |
|---|---|---|---|
| 1 | 自动化保活联动 | ✅ 已实施 | `75f5f058` — `count_active_runs` + `/api/automation/active-count` + 前端 10s 轮询 |
| 2 | 浏览器/CU 授权面审计 | ✅ 已实施 | `9fbf236` — `proposed/feature/2026-10-04-cua-authorization-audit.md` |
| 3 | CU 注入防护方案 | ✅ 方案已写 | `7c8e75f` — `proposed/architecture/2026-10-04-cua-injection-hardening.md` |
| 4 | 自动化调度幂等 | ✅ 方案已写并裁决 | `c3c8fc1` + `c643ac7` 写方案 → `486d1da` 裁决采纳 [调度模型升级](2026-10-04-automation-schedule-model-upgrade.md)（runId 契约 + 派发状态机，更完备且覆盖时钟跳变）；幂等篇已归档、7 项实施要点已移植 |
| 5 | 会话锚定投递 | ✅ 方案已写（重写） | `347e1da` — `proposed/feature/2026-10-04-automation-session-anchored-delivery.md` |

### 执行中推翻的三处判断

**其一：`auto_id` 注入链不存在。** 本篇第 54 行与第 67 行都写了「`cua_ui_click` 收模型给的 `auto_id`，无签发校验」。核实：`cua_ui_click` 的 schema 只有 `name`；`cua_ui_find` 确实收 `automation_id`，但只做 `find_all` 查询，**不发任何输入**。CU 侧不存在可被利用的注入写入面。真实缺口在浏览器侧（`javascript:` URL 无 scheme 校验、selector 反斜杠未转义、PTC 审批卡不显示脚本），详见注入面方案。

**其二：不存在"两套风险词汇"。** 本篇第 49-51 行说浏览器与 CU「两套词汇、两套审批路径」。核实：`cua.rs:40` 的 `CoreRisk` 是 `hermix_core::RiskLevel` 的别名，两侧用同一套 `Safe/Caution/Dangerous`。真问题是**同一套词汇被一刀切标成 `Dangerous`**，与"两套"无关。

**其三：观察类不是"懒惰一刀切"，而是"全刀"。** 本篇第 61 行猜「观察类标 Dangerous 极可能是懒惰一刀切」。核实后更正：唯一标 `Safe` 的 `cua_move_cursor` 恰好是**唯一真正零副作用**的工具（只是移动光标），其余 15 个全标 `Dangerous`——包括截图和读 UIA 树这类纯读操作。已降到 `Caution`（不是 `Safe`：截图会把屏幕上的密码带进对话记录）。

## 背景事实（核实现状所得）

开工前逐项查了代码，**上一轮的排期有两处错**：

| 上一轮说的 | 实际 | 证据 |
|---|---|---|
| `as_bool` 待修，今天开干 | **10-01 已修完** | `1776c454` 新增 `param_coercion.rs`，15 处替换完成。残留的 `as_bool()` 调用只是注释与测试断言 |
| 浏览器"逐调用、token 吃亏" | **`browser_ptc` 已落地** | `5438a20e` N1 单次投递多步程序 + `1a07df29` 120s 预算 |
| 调度模型升级待做 | **确实没做** | `repository.rs` 无 `claimed_at/dispatched_at/retry_at/attempts/outcome`，只有 `run_id` 传给 `finish_run` |
| 保活只绑流式 | **确认缺口** | `App.tsx:563` `setKeepAwake(enabled && streamingSessionId !== null)` |

### 一处必须先讲的架构事实

`browser_ptc` 走的是**第三条路**，既不是 ZCode/ego 的 JS heredoc，也不是逐调用工具：

```rust
// browser.rs:119-121 注释原文
/// 以结构化步骤替代 JS heredoc：无 Node 依赖、每步骤风险可判定、可审计
```

`PtcStep` 是 `#[serde(tag="op")]` 枚举（`navigate`/`snapshot`/`click`/`js`…），一次调用 = 一个步骤数组顺序执行。这与上一轮 rejected 的 PTC 笔记**不冲突**——那篇否决的是"Rust 内嵌 JS 引擎"和"Node 侧车"，声明式步骤是第四条路，已落地且验证过（4 个测试：`test_browser_ptc_program` / `budget_exhausted` / `fail_fast` / `step_limit_and_missing_steps`）。

**结论：PTC 的核心收益 hermix 已经拿到了，且没有执行面。** 上一轮把 `agent-browser` N1 判为"随 PTC 冻结"是错的——N1 事实上已完成，只是笔记没更新。

## Decision

### 一、立刻可开工（两项，都不碰架构）

#### 1. 自动化保活联动 —— 缺口真实，改动最小

**问题**：`App.tsx:563` 的触发条件是 `enabled && streamingSessionId !== null`。后台定时任务执行时 `streamingSessionId` 是 null → **不保活**。笔记本合盖/休眠会让到点任务错过窗口。定时任务恰恰是最需要保活的场景（无人盯着）。

**现状接缝全在**：
- `keep_awake.rs:45 set_keep_awake(bool)` 已注册（`lib.rs:657`）
- `AutomationView.tsx:116-119` 已有开关 + localStorage + 广播事件，注释明写「本组件只在自动化页挂载，生效逻辑不能住在这里」
- `orchestration_bus.rs:56` 有 `AutomationTriggered` 事件

**要做**：后端在 run 进出时广播"有任务在跑"计数，前端 `App.tsx` 把保活条件从 `streamingSessionId !== null` 扩为 `streamingSessionId !== null || activeRunCount > 0`。

**验收**：后台任务执行期间合盖 → 到点仍能跑完；任务结束保活自动释放。
**风险**：两个信号要合并计时，不能互相顶掉（流式结束但任务还在跑时不能释放）。ZCode 用 `offpeak-active-count` 消息做同一件事。

#### 2. 浏览器/CU 统一授权面 —— hermix 独有的债

**问题**：两条路两套词汇、两套审批路径——
- 浏览器 `browser.rs` 用 `RiskLevel`（`browser_press: Safe` / `browser_ptc: Dangerous`）
- 桌面 CU `cua.rs` 用 `CoreRisk`（14 个 Dangerous / 1 个 Safe）

且**注入链在两侧同样存在**：
- `cua_ui_click` 收模型给的 `auto_id`/`class_name`/坐标，**无签发校验**（`cua.rs:1069+`）
- `browser_ptc` 的 `js` 步骤是任意页内执行；`browser_click` 收模型给的 selector

**ZCode 恰好在这点是对的**：CU 与 browser 收敛到同一个 `node_repl`，共用 broker 凭据注入 + `isReadOnlyBrokerMethod` 白名单。hermix 反过来是两套。

**要做**：先做**审计而非重构**——把两侧风险分级的实际依据查清，产出映射表，再决定是否收敛。理由见 Consequences 的风险条。

**先查清再动手**：`cua.rs:81-101` 里 `cua_screenshot`/`list_windows`/`ui_tree` 标 `Dangerous` 而 `cua_move_cursor` 标 `Safe`——**观察类工具标 Dangerous 极可能是懒惰一刀切**。若属实，审批面上它们在无谓地弹卡（用户 09-26 已定策「写文件和命令执行不用审批」，观察类读更不该弹）。

### 二、需要先写方案再开工（一项）

#### 3. CU 注入防护（签发层）

**问题**：`ui_find`（模型编造定位符）→ `ui_click` → `type_text` 是一条完整注入链。模型输出的定位符必须来自可信签发，否则等于让模型指挥点击任意位置。

**ZCode 的机制**（`frame-contract.d.ts` + `mcp/image-normalization.ts`）：producer 签 `_meta` integrity 标记 → 校验标记 + image 块后紧邻合法引用 → **非 authority 结果里的引用文本直接剥除**。动机不是防假图，是防坐标契约污染。

**为什么不能直接开工**：签名方案有真实的取舍——强校验会在 UIA 控件无稳定 `auto_id` 时让模型频繁失败。**"拒绝"与"降级为坐标点击"之间的边界必须先定，不能由实现者临场决定。** 本项先写方案笔记（含降级策略与失败面预算），评审后再动代码。

### 三、明确不做（本轮）

- **CU 进程独立化**（差距 A）——`hermix-cua` panic 会打穿 sidecar 是真问题，但要先做第 2 项的授权面，且拆 crate 是架构决策，另篇立项。
- **token 成本**——`browser_ptc` 已解决浏览器侧；桌面 CU 侧细粒度工具是 UI Automation 的固有代价（每个交互都要独立可审计），不追。

## 排序（执行后回填）

| # | 项 | 原估规模 | 实际 | 阻塞 |
|---|---|---|---|---|
| 1 | 自动化保活联动 | 半天 | ✅ 半天（`75f5f058`） | 无 |
| 2 | 浏览器/CU 授权面审计 | 1 天 | ✅ 审计 + 实施（`9fbf236`） | 无 |
| 3 | CU 注入防护方案 | 1 天（只写方案） | ✅ 方案（`7c8e75f`），**范围比预估大**：真缺口在浏览器侧共 3 项，跨前后端 | 无 |
| 4 | 自动化调度幂等 | 2-3 天 | ✅ 方案（`c3c8fc1`+`c643ac7`），**实施需重跑 4 次退避的真实网络失败** | 无 |
| 5 | 会话锚定投递 | 前端为主 | ✅ 方案重写（`347e1da`），**前置比预估多一层**：`cronjob` 工具读不到 session，需先建注入机制 | 依赖 4 |

第 1 项与第 2 项可并行——一个动 `App.tsx` + 事件，一个只读审计 `browser.rs`/`cua.rs`。

## Alternatives considered

- **先做 CU 注入防护（上一轮的建议顺序）** — 否决：签名方案有未定的降级取舍，先写代码会把这个决策塞给实现者。改为先做零风险的两项。
- **把统一授权面做成一次重构** — 否决：风险分级现状可能本身就是错的（观察类标 Dangerous）。**先审计拿到依据**，再决定是收敛还是修正分级。审计成本一天，重构成本不定——顺序反了就是拿不确定的方案去改审批面。
- **保活联动放到最后做** — 否决：这是唯一"真实用户已吃亏、接缝全在、半天可完"的一项。先啃骨头是浪费。
- **把 `browser_ptc` 当成 ZCode 的短板去补** — 否决：`browser_ptc` 的声明式路线在攻击面上优于 heredoc，hermix 无需对齐。

## Consequences

- **获得**：保活联动已落代码并提交；授权面已修正（观察类不再无谓弹卡、UIA 动作路径按副作用分档接入限速）；三份方案笔记（注入面 / 调度幂等 / 会话锚定）可直接进入实施。
- **放弃**：本轮不动 CU 进程边界；token 成本不追。
- **风险（已发生）**：本篇的执行前提错了三处（`auto_id` 注入链、两套词汇、观察类一刀切），全部靠执行时的逐行核实才发现。**若当初直接按本篇开工，第 2 项会做成一次"统一两套词汇"的重构——而实际上根本不存在两套词汇。** 这正是本篇第 2 项"先审计不改"的判断救了场。
- **遗留**：注入面方案里 `browser_navigate` 的 `javascript:` URL 在 `Safe` 档下可静默执行，**这是本批发现的最严重问题，尚未修**。修 1（URL scheme 白名单）应优先于其余三项。
- **遗留（已裁决）**：调度幂等方案与 [调度模型升级](2026-10-04-automation-schedule-model-upgrade.md) 第 2 条曾是同一件事的两种设计，**已裁决采纳后者**（runId 契约 + 派发状态机更完备，且覆盖时钟跳变）。幂等那篇的独有实施要点已移植进调度模型升级的「实施要点」节，幂等笔记已归档。
- **落地时的新增约束**：采纳方案 2 后，**保活联动的 `count_active_runs` 查询需与 run 状态机同 PR 更新**（`75f5f058` 查的是 `status='running'`，新状态机为 `claimed/dispatched/abandoned`），否则中间态改名期间会漏判。这一条两篇笔记都已记录。
