# Decision Note: Agent 提前收尾与工具黑盒（一次长任务卡死的逐层归因）

Status: proposed — P0/P1 已落地（含 2026-10-30 生产日志证据）；P2 只做了观测，瘦身与两个未决问题待做

## Decision

把「长任务干到一半就停，既不继续也不停止」拆成六个可独立验证的缺陷，按
P0→P2 分批修。每批只改一处判据或一条事件链，修完即可用现有日志与 UI 状态验证，
不引入新的抽象层。

### 归因（全部来自 `~/.hermix/logs/agent.log` 实测，2026-10-30 会话 `efd064e9`）

| # | 缺陷 | 证据 | 代码位置 |
|---|---|---|---|
| A | 长任务里「模型只说了一句话」被判收尾，nudge 被吞 | 单日 `finalizing` 13 次；`goal_continuation: nudging` 与 `finalize: trusting` **各 0 次** | `run_loop.rs:1410-1418`（`nudge_eligible` 要求 `!history_includes_tool`） |
| B | 同一问题两套判据互相抵消 | 上一处判「该继续」并 `goal_continuations += 1`，下一处判「该收尾」 | `run_loop.rs:1296` vs `1410` |
| C | 自动续跑预算全是死的 | `max_autonomous_nudges`（默认 8）传入后从未被读；`MAX_GOAL_CONTINUATIONS=40` 实际只能到 1 | `empty_response_handler.rs:190/265`、`run_loop.rs:200/249` |
| D | 收尾分支零日志，"为什么停"无法归因 | 14:35:20 之后日志一行不动，而 UI 仍在「正在回复…」 | `run_loop.rs:1302`（早退无 log） |
| E | 撞轮次预算后的摘要必然 400，退化文案被当成模型计划 | `unknown model 'minimax:minimax-m3.1-flash'`；`active_model == config.model` 连回退都跳过 | `pre_turn.rs:265-281`、`session_lifecycle.rs:997/1043` |
| F | terminal 是 600 秒黑盒，且 abort 对它无效 | turn 29 的 `terminal` 跑 401s（`tool_ms` +401s）期间零事件；残留 `pwsh` 进程（12:16 / 13:38 起） | `hermix-environments/src/local.rs:619`、`terminal.rs:29`、`tool_executor.rs:579-586`、`agent_helpers.rs:54` |
| G | 前端「卡片已结算 + 回合未结算」双状态打架 | 截图：`执行已中断 — 工具未汇报完成事件` 与「正在回复…」同时出现 | `App.tsx:3714-3716` |

A 与 B 是同一件事的两份真相源，后者把前者的决定直接覆盖——这是"明明下一步计划都有
却停"的直接机制。C 说明为这个场景写的预算从未生效过；D 说明为什么这事一直只能靠
读代码反推。

### 已交付（2026-10-30）

- **P0-1 可归因**：两条 finalize 早退各补一条 `info!`（`exit` / `blocker` /
  `goal_continuations` / `autonomous_nudge_count`）；「没有 tool_call」与
  「tool_call 名被丢弃」拆成 `finalize_no_tool_calls` / `finalize_unnamed_tool_calls`，
  且「工具未解析，可重发继续」的状态提示只在真丢弃空名时才发（此前普通收尾也会报）。
- **P0-2 判据合一**：新增 `AgentLoop::goal_continue_blocker` 返回唯一的
  `Option<&'static str>`（`autonomous_mode_off` / `no_tool_schemas` /
  `nudge_budget_exhausted` / `goal_continuation_cap` / `turn_budget`），
  `should_goal_continue` 与两处 `nudge_eligible` 全部改为读它；
  `decide_empty_response` 第 2 步由 `== 0` 改为 `< max_autonomous_nudges`，
  死字段接上；回放测试改为「推满预算才收尾」（1+8 次调用）。
- **P0-3 长工具可见**：`tool_executor` 在**串行（改状态）**执行循环与并行批收集
  循环各加一条 5s 心跳（`tool` 状态：「仍在执行：<tool> — 已 Ns」）。串行那条是
  关键——`terminal` 的 600s 阻塞就发生在那里，此前全程零事件。
- 验证：`cargo test -p hermix-agent --lib` 1226 passed / 0 failed；
  `cargo check --workspace --all-targets` 零 warning；新增回放测试
  `replay_long_serial_tool_emits_heartbeat`（6s 慢工具断言心跳存在）。

- **P1-2 前端状态单点收敛**：`useStreamChat` 的 `settle()` 现在**无条件**清
  `streamingSessionId / streamingStatus`，`keepSessionAlive` 只决定要不要关通道。
  此前失败终态（`done` 带 `outcome=failed/interrupted`，`useStreamParser` 传
  `keepSessionAlive=true`）会把这个槽位留着，于是 `isStreaming` 永真，
  MessageList 底部的 `AgentActivityIndicator` 一直显示「正在回复…」——而定时器
  已 disarm、`doneRef` 已置位，再没有任何东西能关掉它。stranded 卡片文案改为
  「本轮已结束，该工具未回报结果」，不再暗示工具失败。
- **P1-1 取消时清进程树（替代 Job Object 的最小实现）**：`local.rs` 在非
  background 路径上加 `KillTreeOnDrop` 守卫。用户按停止时 `tool_executor` 直接
  `abort()` 任务，future 被 drop → `tokio::time::timeout` 的 Err 分支不执行 →
  旧代码不会 `kill_process_tree`，PowerShell/cmd 进程残留（实测 12:16 / 13:38
  各一个 pwsh）。正常完成时 `disarm()`，不对已退出的 pid 做无谓 taskkill；
  background 分支在上面提前 return，构造不到守卫，不会被误杀。
- **P1-3 未知模型判定扩展到 400**：死模型自愈此前只认 404 + `does not exist`，
  而 MiniMax 报 `400 ... unknown model 'x' (2013)`，于是小写 slug 永远留在默认位、
  **每一轮**撞轮次预算的收尾摘要都 400。现在两种措辞都识别，并剥掉
  `provider:` 前缀再上报 `reportDeadModel`。
  **边界要说清**：`remove_dead_model` 只动免费渠道（`custom-free-*`）拥有的条目，
  所以本会话的 `minimax:minimax-m3.1-flash`（付费 provider `minimax`）**摘不掉**
  —— 这条修复覆盖的是免费渠道那一类死模型；付费 provider 的未知模型需要另走
  「会话模型校验 / 回退」，列入未决。
- 验证（P1）：`cargo check --workspace --all-targets` 零 warning；
  `tsc --noEmit` exit 0；`check-design-guard` clean；`check-design-primitives`
  仍为 21 条存量 advisory（本次改动未新增）。

- **P1-3 补：摘要失败的归因可见**：`deterministic_progress_summary` 正文加稳定
  标记 `[PROGRESS-RECORD]`（`DETERMINISTIC_SUMMARY_MARKER`），`pre_turn` 在注入
  机械记录时同时 `emit_status` 说明「撞了轮次预算 + 摘要调用失败，下面是记录不是
  模型的完成总结」。此前这段记录会被模型转述成「已完成 / 下一步」，读起来正是
  "计划都写好了却停了"。判据用标记而不用正文句子：正文靠 `\` 续行拼接，落盘空格
  与源码不一致，`contains` 会静默失配（这次真踩到了）。
  用例：`replay_max_turns_summary_failure_emits_attribution_status`。
- **P2 观测（不是瘦身）**：`TurnReport` 增加 `context_max_tokens` 与 `model`，
  日志同时输出 `model` / `context_max` / `context_pct`。没有这两个字段，"这一轮
  用了哪个模型、占窗口几成"都只能靠猜。
- **P2 的真相（已查清，结论是"参数决策"不是 bug）**：`agent.log` 里
  `ContextManager::for_model_with_providers resolved context window
  model=minimax:MiniMax-M3.1-Flash resolved_context_tokens=1048576` ——
  窗口解析成 **1M**，于是 229K 上下文只占 22%，压缩阈值（80%）**永远够不到**。
  每轮把 22 万 token 重发一遍 ⇒ `api_ms ≈ 140s/轮`。压缩管线本身没坏。
  **建议（未实施，需拍板默认值）**：给 `CompressorConfig` 加一个**绝对软上限**
  （例如 120K token）—— 即便模型窗口是 1M，超过该值也压缩。1M 窗口下"按比例"
  触发的代价是长会话每轮都在付 20 万 token 的往返时间。
- 验证（P1 补 + P2 观测）：`cargo test -p hermix-agent --lib` 1227 passed；
  `cargo check --workspace --all-targets` 零 warning。

### 批次划分

**P0-1 让停止可归因（10 行）** — `run_loop.rs` 的 finalize 早退补 `info!`：
`reason`（no_tool_calls / dropped_empty_names / nudge_budget_exhausted）、`turn`、
`goal_continuations`、`autonomous_nudge_count`、`history_includes_tool`。同时把
`1417` 那条名不副实的文案拆开：过滤前有 tool_calls 与否分别打
`dropped_empty_names` / `no_tool_calls`。
*验证*：随后任何一次停止都能在 `agent.log` 一眼读出出口名。

**P0-2 收掉 A/B/C 三个判据** — 删 `nudge_eligible` 里的 `!history_includes_tool`，
让 `should_goal_continue` 与 `decide_empty_response` 共用 `autonomous_nudge_count <
max_autonomous_nudges`（接上死字段），`MAX_GOAL_CONTINUATIONS` 保留为全局硬上限。
*验证*：`cargo test -p hermix-agent --lib` 现有的 nudge 回归用例；`goal_continuation:
nudging` 在真实长任务里首次出现（这是"修好了"的可观测信号，不是日志噪音）。

**P0-3 terminal 不再是黑盒** — `tokio::process` + 逐行 stdout 事件 + 每 5s 心跳
（"已运行 Ns"），`terminal` 的 600s 硬上限保留但执行期可见；忙等类命令
（`while` + `Start-Sleep`、`wait`）在参数预处理阶段提示改用 `background:true`。
*验证*：跑一条 `Start-Sleep 30` 的终端用例，UI 在 5s 内出现心跳、30s 后 normal complete。

**P1-1 真取消（Windows Job Object）** — 每条命令挂一个
`CREATE_SUSPENDED`→assign→resume 的 Job（`KILL_ON_JOB_CLOSE`），Stop 时
`TerminateJobObject`。同步 handler 的 `abort()` 单独留下，改为「kill 进程 + abort 任务」
两件事都做。
*验证*：Stop 后 `Get-Process pwsh` 不残留新进程（当前会残留）。

**P1-2 前端状态单点收敛** — `replying` 由「收到终态帧 / WS 断开 / 无事件超时」三条任一
清除；终态帧携带 `turn_id` 以便重连后对账；stranded 文案改成「本轮已结束，该工具未回报
（详见日志出口 X）」，不再暗示工具失败。
*验证*：断网 30s 再恢复 / 强杀 sidecar 两种场景，UI 都停在「已中断」而不是「正在回复…」。

**P1-3 收尾摘要的模型 id** — `handle_max_iterations` 拿 `active_model`（路由后的
小写 slug `minimax:minimax-m3.1-flash`）直接打 provider → 400 `unknown model`；
又因为 `active_model == config.model`，`session_lifecycle.rs:997` 的回退分支没走，
直接落到 `deterministic_progress_summary()`。**已交付**：前端死模型自愈扩展到
`unknown model` 措辞（见「已交付」），死模型会被摘掉、默认模型顺延。**仍未决**：
`config.model` 何时被写成小写 slug（`model_selector` 的免费列表
`minimax/minimax-m3.1-flash:free` 是嫌疑源，主循环靠每轮 runtime route 拿到合法
id 才没跟着 400）；这条查清前，"摘要用错模型"只能靠事后自愈，不能从源头避免。
另：摘要失败时把 `deterministic_progress_summary()` 注入对话、由模型转述，读起来
就是"计划都写好了却停"——是否改成直接可见的收尾状态（不再让模型转述），待定。

**P2 观感放大项** — 本轮 `api_ms ≈ 140–153s/轮`、`context_tokens ≈ 229K`。30 轮 ≈ 75
分钟，任何收尾缺陷都会被放大成"卡死"。修 A–F 之后再看是否值得做上下文瘦身（现有
compression 管线不动）。

## Alternatives considered

- **只调 `max_turns`（30 → 无限）**：否决。撞轮次只是放大器，不是原因；放宽后每次
  "干一段就写总结"依然 immediate finalize，只是更晚发生。
- **把 `autonomous_mode` 关掉，改成"用户说继续才继续"**：否决。这是现状的显式化，
  与产品既定方向（try everything before stopping）相反。
- **给 `decide_empty_response` 加"目标完成度判定"（Goal Mode 式实据校验）**：暂缓，
  不是替代。正向完成判定需要一套"实据"模型（文件写入 / 命令输出 / 测试结果），
  属于新能力，先把"该继续却被判收尾"的判据错误修掉；否则正向判定会被同一个
  `nudge_eligible` 提前短路。
- **terminal 直接不做同步执行，一律后台 + 轮询**：否决。短命令（`dir`、`git status`）
  的前台同步是最短路径，改成轮询会让每个工具调用多一轮 LLM 往返。
- **前端超时自动"重连并重放"整轮**：否决。重放会重复执行工具（副作用不可逆），
  代价高于收益；只做状态对账，不做重放。

## Consequences

- **P0-2 会让长任务里"写总结"的模型被推回去多次**（最多 `max_autonomous_nudges` 次）。
  代价是 token 消耗上升、单轮时间变长；收益是不再"干到一半交卷"。若实测发现模型
  空转，调小该默认值即可（现在它是死的，所以这个调节旋钮第一次真正可用）。
- **P0-3 让 terminal 输出变成流式事件**，前端 `tool_stdout` 的合并逻辑（已有
  `flushPendingToolChunks` 帧合并）会承受更高事件率，需要复测大输出（>1MB）时的
  主线程占用。
- **P1-1 引入 Job Object** 是 Windows 专有实现，在 `local_isolation` 之外多一条
  进程管控路径；非 Windows 构建走 `#[cfg]` 直通，不提供等价取消语义。
- **P1-2 改文案**会让"执行已中断"这条既有文案消失，历史会话截图对不上；以笔记为准。
- 不做正向完成判定（Goal Mode）意味着：模型确实"认为"任务完成时仍会收尾。这是
  有意的——宁可多推几次，也不要在没有实据模型的前提下判它没完成。
