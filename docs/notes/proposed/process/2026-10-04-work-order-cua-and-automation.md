# Decision Note: 电脑控制 / 自动化 开工清单

Status: proposed — 2026-10-04 整理。基于 [批次评审](2026-10-04-proposed-batch-review.md) 与 [CUA 差距分析](2026-10-04-cua-gap-analysis-vs-zcode.md)，逐项核实代码现状后重排。**本篇是唯一开工入口**，两篇分析只提供依据。

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

## 排序

| # | 项 | 规模 | 阻塞 |
|---|---|---|---|
| 1 | 自动化保活联动 | 半天 | 无 |
| 2 | 浏览器/CU 授权面审计 | 1 天 | 无（先审计不改） |
| 3 | CU 注入防护方案 | 1 天（只写方案） | 需评审 |
| 4 | 自动化调度幂等 | 2-3 天 | 与 [批次评审](2026-10-04-proposed-batch-review.md) 第 4 项同源 |
| 5 | 会话锚定投递 | 前端为主 | 依赖 4 |

第 1 项与第 2 项可并行——一个动 `App.tsx` + 事件，一个只读审计 `browser.rs`/`cua.rs`。

## Alternatives considered

- **先做 CU 注入防护（上一轮的建议顺序）** — 否决：签名方案有未定的降级取舍，先写代码会把这个决策塞给实现者。改为先做零风险的两项。
- **把统一授权面做成一次重构** — 否决：风险分级现状可能本身就是错的（观察类标 Dangerous）。**先审计拿到依据**，再决定是收敛还是修正分级。审计成本一天，重构成本不定——顺序反了就是拿不确定的方案去改审批面。
- **保活联动放到最后做** — 否决：这是唯一"真实用户已吃亏、接缝全在、半天可完"的一项。先啃骨头是浪费。
- **把 `browser_ptc` 当成 ZCode 的短板去补** — 否决：`browser_ptc` 的声明式路线在攻击面上优于 heredoc，hermix 无需对齐。

## Consequences

- **获得**：一个无阻塞的起步（保活）+ 一份授权面事实依据（审计）+ 一份待评审的安全方案（注入防护）。
- **放弃**：本轮不动 CU 进程边界；token 成本不追。
- **风险**：审计可能得出"分级基本正确"的结论，那就只剩映射表与文档，无代码变更——这是可能且可接受的结果，不应为了有产出而强行重构。
- **前置修正**：`proposed/architecture/2026-10-01-agent-browser.md` 的 Status 行称 N1 未实施，与 `5438a20e` 矛盾，须在下批修正（本篇已记录事实，未改该文件以免与批次评审的引用关系交叉）。
