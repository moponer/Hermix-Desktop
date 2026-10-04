# Decision Note: proposed 批次评审 — 哪些能真正立项

Status: proposed — 2026-10-04 读 ZCode 源码（`zai-org/ZCode` @ v3.14.3，MIT）后，对 `proposed/` 下 13 篇笔记做立项裁决。结论：**3 篇可立项、2 篇须重写、1 篇降级为后续、7 篇归并或转 rejected**。

## 背景事实（读源码所得）

评审前先核实两件事，否则裁决没有意义。

**一、ZCode 的"稳定性"资产不在 notes 覆盖的那几篇里。** 通读 `packages/zcode-server-cli/src/supervisor/`（`supervisor.ts` 23KB + `crashBudget.ts`）、`runtime/startupRecovery.ts`、`runtime/lock.ts`、`runtime/statusSnapshot.ts`、`packages/ui/src/v4/{staleAuthorityRecovery,pendingCommandReplay,taskModelRecovery}.ts` 后确认：ZCode 的可靠性是**进程监管 + 崩溃预算 + 单实例锁 + 状态快照 + 权威失效恢复**五件套，全部有"踩过坑"的注释支撑。

**二、hermix 已有的底座比 notes 假设的更多。** `sidecar.rs:413-520` 已有 watchdog：`max_consecutive_failures=30`、退避 2s/5s/15s、`max_restarts_before_give_up` 封顶、`hermix:server-gave-up` 事件外呼。`automation_runs` 表已存在（`hermix-automation/src/repository.rs:97`）。`compression_summary_history` 已是 append-only。

因此"照抄 ZCode 补短板"的判断要逐条落到**这两个事实**上，而不是按 notes 的措辞直接认领。

## Decision

### 一、可立项（3 篇，按此顺序开工）

#### 1. `as_bool` 字符串参数扫描 — 立即做，半天

唯一"零架构风险、纯 bug、已在生产造成用户可见死循环"的一篇。`voice_mode.rs:657` 的 `unwrap_or(!previous)` 是**取反**而非取默认：模型说"关掉"实际"开启"，方向相反且无提示。这是安全缺陷不是体验问题。

范围已在笔记内定死（15 处替换 + 一个 `bool_param` helper），无外部依赖，无设计待定。**这是 13 篇里唯一可以今天开干的。**

#### 2. 自动化调度模型升级（run 状态机 + 退避重试）— 立项，2-3 天

笔记说"无 run 表"，**这句已过时**：`automation_runs` 表与 `record_run_start` 都在。真正的差距在笔记的后半段，且源码证实了——`scheduler.rs:97-128` 的触发判定是裸时间戳比较：

```rust
Some(next) if next <= now => match state.last_run_at { ... }
```

没有 claim、没有幂等键、没有派发状态机。进程重启 / 时钟跳变 / 重复 tick 三个场景都会漏跑或重跑。`automation_trigger.rs` 注释自己写着 fire-and-forget。

**立项范围收窄为三件事**（不做笔记里的结构化 `scheduleRule` 表单，那是 UI 工作量不是稳定性）：
- runId 幂等键（`automation_id + scheduled_at + trigger`）——这是 ZCode 的契约，抄它
- `automation_runs` 加 `claimed_at / dispatched_at / attempts / retry_at / outcome` 五列（表已存在，只是加列）
- 派发失败按 transient/permanent 退避重试；agent 执行失败不自动重试（笔记已论证，同意）

**结构化 scheduleRule 拆出去**，理由见下面「须重写」第 1 条。

#### 3. 自动化保活联动 — 立项，半天

`cecebc36` 落地时 keep-awake 只绑"会话流式"（`App.tsx` 监听 `streamingSessionId`）。定时任务恰恰是无人盯着、最需要保活的场景——笔记本合盖就错过窗口。这是**已落地功能的缺口**而非新功能，验收标准可写死：后台任务执行期间合盖，到点仍能跑完。

笔记里的闲时队列部分不并入此批（见「降级为后续」）。

### 二、须重写后再评审（2 篇）

#### 4. 动态工作流引擎 — 退回重写，理由是规模估算错了两个数量级

主人的笔记已定 boa 选型（这个选型我同意：纯 Rust、无内建 IO、构建时长可控，比随包 V8 划算）。问题在 M1-M5 的范围。

实测 ZCode 侧：**216 个 workflow 相关文件 / 13,817 行**，横跨 run 派发、生命周期、日志、重放、产物投影、血缘、roster（子 agent 编排）、sequence capture、reconcile、retune。笔记规划的 M1-M5 覆盖不了这个面。

更关键的一条事实：**ZCode 自己把动态工作流做成了默认关闭的实验特性**——

```ts
// apps/zcode-cli/packages/bootstrap/src/zcode-protocol/server.ts:255
dynamicWorkflowEnabled: false,
```

服务端 `/client/configs` 下发 `dynamicWorkflow.mode` 才开；关闭时 `dynamic-workflow-gate.ts` 从技能发现、工具注册、`/` 目录三处同时做减法。一个 13,817 行的子系统，官方自己不敢默认开。

**结论**：这是个季度级项目，不该伪装成"5 个里程碑"。重写要求：
- 补一条 Alternatives：**先不做**（对齐 ZCode 的默认关闭，等有真实用户诉求再开）
- 若坚持做，M1 只做**单脚本 + 无工具桥 + 无状态**的 hello world，用来验证 boa 在 MSVC 上的构建时长与稳定性，**不碰 `workflow_runs` 表**（表结构跟着 M3 走，现在建必返工）
- 明确写入：M2 的工具桥是整个项目的成败点，也是最大攻击面，笔记里"逃生口为零"的说法在 `hermes.tool` 白名单真正接上 ToolRegistry 之前不成立

#### 5. Windows ACL 沙箱 — 保留，但改立项条件

`2026-10-01-windows-acl-sandbox.md` 的技术分析质量很高（12 个决策点、对抗式审查 16 行、测试钉死清单 9 条），**判定与机制都同意**。但它依赖的两篇 dsh 笔记，以及 hermix 侧的前置条件没被满足：

- 笔记自述"实现与真机测试尚未落地（P3）"，测试钉死清单是准入门槛——**未跑通前不得声称 enforcement 成立**，这条自律要保留
- 与 IM 工作区范围（`2026-09-30` 那篇）的 L1 有重叠：两者都要在**工具派发层**做路径检查。**先做派发层的统一策略根，ACL 是它的一个后端**，而不是两套并行判断（笔记 Alternatives 里"fs 与 bash 共享同一策略根"说的正是这件事，但没和 IM 那篇合上）

**立项条件**：改为「与 IM 工作区范围合并为「统一工作区策略根」一批，ACL 沙箱作为第二个后端」。单独做 ACL 会造出第三套判断。

### 三、降级为后续（1 篇）

#### 6. 闲时任务队列 — 只保活部分立项（已计入第 3 条），队列部分推迟

笔记自己已经给了判据："保活联动就能解决大部分，而闲时排队是'算力便宜时再跑'的成本优化"。同意。队列需要优先级/预算/队列深度展示，是完整功能；等 run 状态机（第 2 条）落地后自然承接——**有幂等 run 记录才谈得上排队**，现在排不了队。

### 四、须转 rejected（4 篇）

#### 7. 事件溯源会话改造 — 转 rejected（至少本轮）

这是本轮最需要拦下的一篇。理由不是"太大"，是**收益已被别处摘走**：

hermix 已有 `compression_summary_history`（append-only，压缩行为可审计 ✓）、`archive-index.jsonl`（原记录可查 ✓）、`backfill_unanswered_tool_results`（崩溃回合补合成 ✓）、消息窗口化加载（2026-09-28）。F-1 笔记列的四条收益里，**三条已经实现了**。

剩下的唯一真收益是「fork 任意 seq」与「字节级重建」。而笔记自己写了代价："四步约 2-3 周" + "放弃可变消息数组的简单性" + "投影与日志分叉风险"。为 `fork 任意 seq` 付这个代价，在 hermix 当前没有 fork 需求的前提下不成立。

**若将来真的需要 fork，这篇要重写而非复活**——因为届时压缩与 archive 的现状会被新设计重新解释一遍。

#### 8. PTC（程序化工具调用）— 转 rejected，与动态工作流同因

F-3 笔记的 Alternatives 自己写了："**先做 Dynamic Workflows（dsh 的 JS 编排脚本）——否决：PTC 是其子集且更小**"。这句话是**错的**：PTC 的执行器（写代码调工具）与动态工作流的执行器（写代码调工具）**是同一个东西**，ZCode 侧 13,817 行是它的完整形态。浏览器 PTC（`2026-10-01-agent-browser.md` N1）也写着"复用 F-3 执行器，不为第二个功能再造第二套"。

三篇笔记（动态工作流 / PTC / 浏览器 PTC）互相引用对方作为前置，构成**循环依赖**。三篇必须一起裁决，不能各自推进。

**裁决**：一起转 rejected，理由记为「与 ZCode 同款的能力，官方默认关闭，且 hermix 当前无真实用户诉求」。若将来 agent-browser 立项（N1 依赖 PTC 执行器），先单独重开 PTC 那一篇。

#### 9. 智能体浏览器 — 降级为条件立项

`ego-lite` 那个论证（单次往返、语义 snapshot 省 token）是对的，hermix 已有 CDP 底座也确实够用。但 N1 依赖 PTC 执行器，而 PTC 刚被拒。N2（Space + 登录态）不需要 PTC，N3（独立 crate 分发）是纯包装。

**只保留 N2 为可立项**，N1 随 PTC 一起冻结，N3 待 N2 跑通后再看。理由：ego-lite 是 macOS-only 且未披露安全设计，hermix 的差异化在 Windows 原生 + 审批链；为一个省 token 的浏览器能力引入 heredoc 执行面，收益/攻击面比不划算。

#### 10. IM 渠道工作区范围 — Phase A 立项，并入统一策略根

Phase A（`scope_expansion` 审批 kind + IM 1/2/3 + `session_bypass` 加工作区维度）**全复用现有机制**（`ApprovalRegistry` / `approval_requester` / `resolve_permission`），零新 UI、零新凭据字段——笔记自己标了"全复用"。这是三篇架构提案里最便宜的一个。

但必须与第 5 条（ACL）**合批**：两者都要在派发层做路径前缀检查。Phase C（语音解码）明确推迟——语音授予持久权限不可接受，笔记这条自律是对的。

#### 11. 会话锚定投递 — 立项，但排在调度模型升级之后

`target_task_id` 语义对齐 ZCode 没问题，但它的护栏（"注入超过 N 次后转独立会话"）依赖 run 记录与注入计数——**又是 run 状态机的下游**。且它要改 `ChatView` 的行类型（新增任务消息一等表示），前端工作量不小。

**排序**：调度模型升级 → 会话锚定投递。中间不要插别的。

## Alternatives considered

- **按 notes 原计划全部立项，13 篇排进季度** —— 否决：三篇互相引用作为前置（F-3 的 Alternatives 明写"PTC 是 Dynamic Workflows 的子集"，动态工作流 M2 依赖 PTC 式的工具桥，浏览器 N1 依赖 PTC 执行器），排出来的序必然含环。且 ZCode 对其中最大的一篇默认关闭。
- **全部否决，只留 `as_bool`** —— 否决：调度幂等（漏跑/重跑）与后台任务保活是真实失败模式，主人已经吃过"到点任务错过窗口"的亏。事件溯源与 PTC 该拒，但这两篇该立。
- **先做动态工作流再谈其他** —— 否决：ZCode 用 13,817 行 + 默认关闭来回答了这个问题。hermix 在稳定性债（漏跑、休眠漏跑、参数取反）未清时做编排引擎，是把最难的题放在最不利的时机。

## Consequences

- **放弃**：动态工作流引擎、PTC、浏览器 PTC N1/N3、事件溯源会话改造、闲时队列。放弃的理由都已写进 rejected/revised 笔记，触发重来的条件是「ZCode 把 dynamicWorkflow 默认打开」或「hermix 出现真实的 fork/编排需求」。
- **代价**：本轮不碰会话存储层结构。压缩与 archive 的现有机制继续用，不做迁移。
- **风险**：`bool_param` helper 若写成宽松 truthy-cast，会削弱删除类操作的保险栓——笔记已用测试锁死，接手时别放松。
- **收益**：四篇"看得见的失败"（参数取反、漏跑重跑、休眠漏跑、IM 越界）全部有明确验收标准，且**四篇都不需要新的架构概念**。

## 排序（认领顺序）

1. `as_bool` 字符串参数扫描（半天，bug）
2. 自动化保活联动（半天，缺口）
3. 统一工作区策略根 = IM Phase A + ACL 沙箱（1-2 周，安全）
4. 自动化调度模型升级（2-3 天，幂等）
5. 会话锚定投递（前端为主，依赖 4）

第 3 与第 4 顺序可互换，互不依赖。
