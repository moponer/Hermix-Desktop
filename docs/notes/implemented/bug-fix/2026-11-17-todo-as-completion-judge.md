# Decision Note: 用未完成 todo 作为「任务真的做完了吗」的判据

Status: implemented — 新增 `todo_probe`，未清空的 todo 越过 nudge 预算让 run 继续

## Decision

用户报告的症状是：**从来没有见过项目启动 todo 列表来执行任务，总是一段结束就汇报，
然后用户发「继续」，又完成又汇报，反复浪费时间。**

实测确认了两件事：

1. `todo` 工具调用次数 **0**（全量 `agent.log`）。工具存在、有 schema、有指引，但从未被用过。
2. **agent loop 完全无法读取 todo 状态**——`FileTodoBackend` 的 `Mutex<Vec<TodoItem>>`
   只被 `todo` 工具持有，loop 没有句柄。

第 2 条是根因。继续判据（`goal_continue_blocker`）能用的信号只有轮次预算与
`autonomous_nudge_count`，而这两个都是**过程指标**，回答不了「任务做完没有」。于是唯一
的出口是 `nudge_budget_exhausted`：预算用满就把控制权交回用户。历史数据吻合——
`goal_continuation` 触发 184 次，`nudge_budget_exhausted` 挡住 8 次。

### 实现

新增 `crates/hermix-agent/src/todo_probe.rs`：读 `$HERMIX_HOME/todos.json`，按 `session_id`
过滤，返回未完成条目与总数。接到两个地方：

- `run_loop::goal_continue_blocker`：有未完成 todo 时**直接返回 `None`**（允许继续），
  越过 `nudge_budget_exhausted`。
- `empty_response_handler`：同样的越过条件，并把**具体条目**写进 nudge 文本，
  让模型看到「还剩哪几条」，而不是被泛泛告知「可能还没做完」。

同时补了 `TODO_GUIDANCE` 的触发条件：超过 ~4 次工具调用或不止一个交付物的任务，
**必须在动手前**开清单。这是让上面那套判据有数据可读的前提。

### 为什么不通过工具层读

`FileTodoBackend` 在 registry 里按 session 构造，loop 拿不到。穿过去要把 loop 和工具构造
耦合起来。而 backend 的存储**就是** `todos.json`，直接读文件是同一份数据、零耦合。
解析失败当作「没有 todo」而不是报错——这是对停止判据的优化，绝不能让它 fail 一个 run。

## Alternatives considered

- **提高 `max_autonomous_nudges`（8 → 更大）**：只是把「预算耗尽」推后。预算不是判据，
  它只是「不知道该不该继续时」的兜底。真正的判据是「还有没有活」。
- **让模型自己判断「我做完没有」**：这正是当前失败的环节。它的总结说「完成了」，而那
  恰好是被判断的对象——用被判断对象当判据是循环论证。
- **用 `MAX_GOAL_CONTINUATIONS`（40）当主判据**：同上，过程指标。
- **让 loop 持有一个 `Arc<dyn TodoBackend>`**：更「正确」的分层，但要把 backend 从工具
  注册流程里抽出来传给 loop，改动面远大于读一个 JSON 文件，收益只是形式上的整洁。
- **强制每轮都要求 todo**：会给简单任务（一次读文件）增加无谓开销与 UI 噪音。
  所以触发条件是「预计 >4 次工具调用或多交付物」，不是「所有任务」。

## Consequences

- 未清空 todo 时 run 会一直继续，**nudge 预算不再是硬出口**。兜底还剩：
  `MAX_GOAL_CONTINUATIONS`（40）、`max_turns`（30，但实测从未触发）、成本护栏、
  用户手动停止。风险是模型写了一份永远不会清空的清单 → run 跑到 40 次续跑上限。
  这是有意的取舍：一份没清空的清单本身就是「它认为还有活」的声明。
- `todos.json` 成为 loop 的输入。若前端或手工编辑把它写坏，解析失败按「无 todo」处理，
  行为退回本次改动之前，不会更糟。
- 提示词层面要求开清单，**不保证**模型照做。若它继续不开清单，本机制读不到数据、
  行为不变（不是回归，只是没改善）。可观测指标：`tool_call_names` 里 `todo` 的出现次数
  应从 0 上升。
- 跨 session 隔离依赖 `session_id` 一致。`None` 时返回空（不做判断），避免把别的会话
  的待办当成自己的。
