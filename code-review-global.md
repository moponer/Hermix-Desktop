# 全局视角代码体检报告

> 立场：始终把"用户发一条消息 → 拿到可用结果"当作一条端到端链路，不在单点判断好坏。
> 链路：`system prompt 构造 → 工具 schema → 模型调用 → 流式解析归一 → 工具派发 → 工具结果 → 历史重放 → WS/会话生命周期`
> 红旗判据：**凡是"为了让链路上某一环立刻不报错，而改动另一环的语义/状态/历史"，就是局部最优候选**。
> 验证开关：这条改动在【不审批 / 审批中 / 中断重连 / 换 provider】等其它路径下是否仍成立？不成立 → 局部最优、全局有害。

---

## 分析方法（如何站在全局看）

1. **画端到端链路，不盯单文件**：每个改动都问"它改的是哪一环、影响的是哪一环"。跨环改语义（用剥离历史来避免 400）必查其它路径。
2. **找"症状修复" vs "根因修复"**：在适配器里把 `tool_call` 改名、在历史里把孤儿 tool_use 剥掉，都是在**绕过** provider 的坏输出，不是修 provider。绕过层越多，越说明根因没管。
3. **统计同一根因被修了几处**：一个 bug 在 N 个独立层各有一份 patch → 没有单一真相源，行为不可组合、不可预测。
4. **检查"失败信号"有没有被复用成"成功信号"**：生命周期里 finalize（失败）/ done（成功）若共用同一事件且前端同处理 → 失败对用户 == 会话死亡。
5. **把"审批中 / 待用户点击"当一等状态，不当临时标志位**：凡是靠 `let xxxPending` 布尔去堵计时器/重放的，全局时序都脆弱。

---

## 分类一：局部最优、全局有害（locally optimal, globally harmful）

### 1. strict-pairing pass —— 剥离"没收到结果的 tool_use"
- **位置**：`crates/hermix-agent/src/provider.rs:460-541`（`strict-pairing pass`），配套 `run_loop.rs:1849-1870`
- **局部收益**：重放历史时避免 MiniMax-M3 报 `400 (2013) tool call result does not follow tool call`，一次性让重连/续聊不崩。
- **全局代价**：它**无法区分"孤儿 tool_use"和"正在等用户审批的 tool_use"**。审批卡 pending 时，tool 结果还没回来，重放这条历史会被它当成孤儿**静默剥离** → 审批请求从历史里消失，前端卡在"等点击"而后端已无此调用，状态分叉（你看到的"审批同意后就结束/不审批也中断"正是这类）。
- **优化方式**：剥离前先查该 `tool_use_id` 是否在「未决审批表」里；在审批中的调用一律保留并标记 `pending_approval`，绝不剥离。把"审批中"提升为一等历史状态，而非靠剥离兜底。

### 2. run_loop 半助手把 tool_calls 整段置 None
- **位置**：`crates/hermix-agent/src/run_loop.rs:1858-1870`
- **局部收益**：中断后续的半条 assistant 不再带悬空 tool_call，重放不触发 2013。
- **全局代价**：**只丢 tool_call、保留 prose** → 模型"说"了要执行 X，但调用记录没了。下一轮模型读到这段 prose 会误以为"已经做过 X"或产生矛盾意图，静默污染上下文，且用户完全无感。
- **优化方式**：中断时整条 assistant（prose + tool_calls）作为一个原子单元处理——要么全留（带正确 pending 标记），要么全丢并在下一轮明确重发；不要"留话丢调用"。

### 3. anthropic_adapter 把 `tool_call` 占位名兜底改成真名
- **位置**：`crates/hermix-intelligence/src/anthropic_adapter.rs:644-688`
- **局部收益**：救活一次退化的工具名，避免立刻 finalize 中断。
- **全局代价**：
  - 这是**在 wire 层静默改写名字**，对其它 provider 是隐形耦合；真名若藏在 `input` 更深层级（如 `input.function.name` 之外），兜底拿不到却**不报错**，产出错误派发或空转。
  - 它和 #4 的 validator、#1 的 strict-pairing **三层各修同一根因**，互相覆盖、优先级混乱——adapter 改成功了 validator 永不触发，但 strict-pairing 仍在剥离，行为不可预测。
- **优化方式**：把"坏名修复"收敛到**唯一一层**（建议适配器，因为它是解析入口），且**改不到真名时必须显式报错/告警**而不是静默保留原值；同时打 trace 记录原始 input 结构，别让"拿不到"变成黑盒。

### 4. system prompt 注入"结论前置"
- **位置**：`crates/hermix-agent/src/context.rs:517`（提交 `2aa1a7a5`，2026-08-06）
- **局部收益**：满足一个 UX 偏好（先给结论）。
- **全局代价**：**一条指令对所有 provider、所有会话全局生效**。它直接改变模型输出结构，正是你反馈"输出和以前明显不一样"的代码侧唯一变量；且它不区分模型，对非 MiniMax 模型也是无意义的行为漂移。UX 微调变成了跨 provider 行为开关。
- **优化方式**：这类输出风格指令应**进 SOUL.md / persona 层**（用户可覆盖、可按会话调），不当硬编码进默认 identity；且任何影响输出结构的 prompt 改动都要带回归测试（快照 diff），不能"觉得好就加"。

### 5. 前端 settle 关 WS + 后端 finalize 复用 done 信号
- **位置**：`apps/desktop-tauri/src/hooks/useStreamChat.ts:731`（`settle` 内 `wsRef.current?.close()`）；后端 finalize 与 natural done 共用同一 `done` 事件（你的日志 `reason=tool_calls`）
- **局部收益**：回合结束统一关 socket，逻辑简单。
- **全局代价**：**失败 finalize 和成功 done 在前端被同等对待**——只要后端发 done，前端就 settle+关连接。于是一次"工具名退化→重试耗尽→finalize"在用户眼里 == "会话死了"。失败信号丢失了语义。
- **优化方式**：done 事件区分 `outcome: completed|failed|interrupted`，前端只对 `completed` 关连接；`failed/interrupted` 保留会话、提示错误、允许重试/换模型，不让一次工具错误升级成会话终结。

---

## 分类二：局部、全局都差（neither locally nor globally good）

### A. invalid_tool 重试 3 次后直接 finalize 放弃
- **位置**：`crates/hermix-agent/src/tool_call_validator.rs:88`（`count >= max_invalid_tool_retries` → `FatalInvalidTool`），配置 `agent_config.rs` `invalid_tool_call_max_retries=3`
- **为什么局部也差**：它**根本没修好任何东西**——同一份坏输出原样重试 3 次，纯属拖延，然后放弃。没有一次尝试去纠正（而适配器那层其实能纠正，却因分层没被复用）。
- **为什么全局也差**：把一次**可恢复**的工具名退化升级成"会话死亡"，且无任何降级（不提示、不切模型、不回退到上一次好响应）。既没有解决局部问题，也没有保住全局可用性。
- **优化方式**：把"纠正"和"放弃"分开——先走适配器修正通道（修正成功就继续），修正失败才进重试；重试耗尽后**降级**（提示用户/自动换 fallback 模型/保留已生成内容并标记 partial），而非 fatal finalize。

### B. 同一根因被 3 个独立层各修一遍（无单一真相源）
- **位置**：适配器改名（`anthropic_adapter.rs:655`）+ validator 拒绝（`tool_call_validator.rs`）+ strict-pairing 剥离（`provider.rs`）
- **为什么局部也差**：每层都觉得自己"修好了"，但三层规则互相覆盖，新增一个 provider 要同时改 3 处，且**适配器改成功时 validator 永不触发、strict-pairing 却仍在剥离**——行为取决于运行时哪层先跑、改没改到。
- **为什么全局也差**：没有"坏工具名归谁管"的 ownership，排障时三层日志各说各话（你之前看到 stripped=1 却找不到真因，就是这层叠加的结果）。
- **优化方式**：确立**单一纠偏层**（解析归一层），其它层只做"结构合法性校验"和"历史一致性"，不做语义改写；三层职责写进架构注释，新增 provider 只需在一处注册其行为特征。

### C. "审批中"靠 `let approvalPending` 布尔绕过计时器
- **位置**：`apps/desktop-tauri/src/hooks/useStreamChat.ts:632-636, 1136-1140`（审批时 suspend idle/stale 计时器）
- **为什么局部也差**：它只是**临时堵住计时器**，没有把"审批中"建模成会话的一等状态；approve 之后的 settle 时序完全依赖这个布尔在正确时刻被复位，脆弱。
- **为什么全局也差**：后端有未决审批事实、前端只有个布尔标志，两端状态不同源；一旦审批 resolve 与 WS 事件竞态，就会出现"卡住/提前关"两种相反故障，且难复现。
- **优化方式**：审批 pending 应由后端在会话状态里持久化为 `awaiting_approval`，前端依据 WS 下发的状态而非本地布尔决定计时器；resolve 走明确的状态机转换，不靠标志位副作用。

---

## 一句话总结
你遇到的"中断"不是某行代码写错，而是**架构层面把"绕过 provider 坏输出"散落到了解析/校验/历史/生命周期四个地方，且让失败信号复用成功信号**。修法不是再打一个 patch，而是：① 确立单一纠偏层；② 审批/未决调用提升为一等状态、不被历史剥离；③ finalize 与 done 分离语义、失败不升级为会话死亡。
