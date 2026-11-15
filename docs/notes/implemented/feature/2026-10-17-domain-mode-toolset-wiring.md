# Decision Note: 域模式接入工具面与收敛参数

Status: implemented — 2026-10-17 落地（与提示词改造同 PR）

## 背景

`DomainPreset` 曾含 `priority_tools` / `convergence_signals` / `max_stall_turns`
等字段，2026-09-29 因消费方全为死代码而连字段一起删除（见 `domain_mode.rs:115-128`
注释）。此后域模式只改系统提示词，不改工具面与收敛行为——Code 模式与 Chat
模式的差异只有一段提示词，"模式"是半残的。提示词改造笔记批次 3 重写域指令时
需要一个决定：工具面接线做不做、怎么做。

## Decision

DomainPreset 恢复两个消费方（按需最小集，不做全量恢复）：

1. **工具路由偏置**：`DomainPreset` 新增 `priority_tools: &'static [&'static str]`
   （每个域 3-5 个工具），消费方是**既有**的 `PriorityBoost` 包装路由器
   （`tool_router.rs`，+5.0 统一加权）——`AgentLoopConfig::route_tools` 每
   turn 用 `resolve_domain_mode` 解析当前域并填 `TurnContext::priority_tools`，
   `default_tool_router` 包上 PriorityBoost。加权不是开关，未列出的工具仍可
   路由。Code/Research/Write/Operate 各配表，Chat 空表。
2. **收敛阈值偏置**：`ConvergenceConfig::for_domain(pin)`——Operate 域
   similar-content advisory/hard-stop 从 5/10 放宽到 8/16（批量文件操作天然
   高重复度），其余域保持默认。**只认用户 pin**（`domain_override`）：自动
   检测的域每轮变化而检测器是 per-run 状态，逐轮改阈值会破坏计数语义。

实现事实与原提案的差异：偏置用**统一 +5.0 的既有 PriorityBoost** 而非提案中
的带权 `toolset_bias` 表——权重无处消费，按 YAGNI 落最简形态。

不做：`safe_write_override`（审批引擎职责，不该被提示层开关）、
`output_style`（REPORT_SECTIONING 单一真相源，不按域分叉）。

## Alternatives considered

- **全量恢复原字段**：原字段当初因零消费方被删，全量恢复 = 重新引入
  死代码。只恢复有真实消费方的两个。否决全量。
- **不恢复，靠技能与提示词表达域差异**：提示词只能影响"倾向"，无法改变
  工具路由评分与收敛判定——这两处是纯代码路径。否决。
- **做成用户可配置表**：YAGNI，先内置静态表跑通，有真实调参需求再做配置。

## Consequences

- **检测误判放大**：域检测靠关键词正则，误判时工具面也跟着偏。缓解：
  bias 是加权不是开关（排除的工具仍可用），且用户 pin 优先。
- **DomainPreset 结构再次变更**：`preset_for` 的调用方（directions.rs）需
  同步适配，编译期保证。
- **放弃的东西**：放弃"域模式零风险纯提示词"的简单性；接线后域检测正确性
  成为工具路由正确性的一部分，测试面扩大（router 单测需覆盖各域 bias）。
