# Decision Note: IM 远程任务生命周期（P1）

Status: implemented — 七个 PR 全部落地，`ci-gate.sh` 11/11 通过（2026-10-04）

> 上游：[手机端伴侣客户端 + 自建 Relay（2026-10-04）](../../proposed/feature/2026-10-04-mobile-relay-selfbuilt.md)（本篇是它的 P1 子项目，手机端部分仍未开工）
> 相邻：[IM 远程指挥三层递进（2026-09-29）](../../implemented/process/2026-09-29-stability-and-remote-plan.md)（本篇补的是它未做的「远程第 1 层」里与 IM 通道重叠的部分）、[IM 会话工作区范围（2026-09-30）](../../proposed/feature/2026-09-30-im-channel-workspace-scope.md)（仍 proposed，本篇不得与之矛盾）

## Decision

把「IM 用户发完消息就可能走开」这个物理约束补齐，分四件事：

1. **按会话中断**。`/stop` 从 `interrupt_all()`（波及本机所有会话）改为只中断发起者自己这一轮。关机路径**保持** `interrupt_all`——关机语义本就是全停，两者是不同的杠杆。
2. **三态收尾回执**。成功 / 失败 / 中断各有回执（原来只有 `Err` 分支，成功什么都不发）。用户自己叫停的**不报成「出错了」**。
3. **长任务心跳**。pump 只在有正文或工具事件时才发消息，所以「模型在想 / 工具在跑但没输出」整段静默需要外部补。
4. **per-session 队列**。轮次在跑时进来的消息排队而不是挤聊天锁，用户看得见自己的位次。

配套：`/tasks` / `/queue` / `/queue steer` / `/queue interrupt` / `/继续` 五个命令，加一组单字母快捷菜单。

## 落地形态与关键约束

以下是实施中查实、并已写进代码注释的约束——**它们不是设计偏好，是踩过才知道的**：

- **中断判据必须配对两个字段**。`AgentReply::interrupted` 单独用太粗：致命工具参数耗尽、后端 5xx 也置位（`graceful_interrupt_result_errored` 走同一条落盘路径），单看它会把后端故障报成「用户按了 /stop」，等于让用户背一个 5xx 的锅。正解是 `interrupted && finish_reason == "interrupted"`，判据本体收在 `hermix_core::traits::is_user_interrupted`，IM 回执与轮次记录共用一份。
- **终态必须在 `let result = result?` 之前落**。`Err` 会提前 return，否则失败 / 叫停的轮次永远停在 `Running`，被下一节的判据误读成「进程崩了没跑完」。
- **`send_message_stream` 返回 `AgentReply`，但 `run` / `run_stream` 返回 `AgentResult`**——后者是前者转换出来的。写代码时看错这两个类型会连环出错。
- **入队判定是 `running.contains(key) || queues.contains_key(key)`**。只看 `running` 不行：`run_turn` 结束会把自己摘掉，于是**排水期间新到的消息会插到已排队消息前面**。让队列 key 活到排空为止，既堵住乱序，也让新消息正确续在队尾，还省掉一个 `draining` 集合。
- **心跳不去查 `running`**。这个 task 的生命周期就是本轮，`send_message_stream` 一返回就 `abort()`；查 `running` 是多余的一层，且 `ChannelRuntime.running` 不是 `Arc`，跨 task 借不到。
- **`ToolTrace` 的锁不能跨 `.await`**。为让心跳读到轨迹它被提为 `Arc<Mutex<_>>`，粒度必须是「改完立刻放」——guard 跨 await 既非 `Send` 也会把 await 点锁死。
- **steer 不需要拦截 builder**。早先设想要在 builder 里 clone 一份 `pending_steer` 出来；实际只要在 `AgentLoop` 上加一个 3 行访问器 `steer_buffer()` 交出那块 `Arc` 即可（agent 侧 drain 的和外部写的是同一块），也就不必把整个 `AgentLoop` 存进 service（那会要求 `AgentLoop: Send + Sync`）。
- **中断控制器与 steer 缓冲生命周期一致**，故合并为 `register_active` / `unregister_active`——两份注册表分开维护必然漂移出一半。`interrupt_session` 命中后顺手摘掉 steer 缓冲，否则用户 steer 的文本会静静躺着永不落地。
- **主对话轮次现在也写 `TaskRecord`**（`turn-{sid}-{nanos}` 前缀与 sub-agent 记录区隔）。此前 `task_records` 只记 `delegate_task` 的 sub-agent，「上一轮跑完没有」只能靠启发式猜。前缀常量 `TURN_ID_PREFIX` 是跨 crate 契约，放在 `hermix_core::traits`——`hermix-agent` 不能依赖 `hermix_task`。
- **判「未跑完」用 `TaskState::outcome().is_none()`**。不是 `is_terminal`——那方法挂在 `TaskOutcome` 上且恒为 `true`。用户按 `/stop` 落的是 `Terminal(Aborted)`，所以**主动叫停不算未跑完**。
- **菜单用字母 a-d，不用 1-4**。审批回复就是 1/2/3，两套语义靠 `pending` 计数切换同一串数字，正是 cc-haha 那条闸门要防的事。字母与数字不同形后菜单任何时候都能用，不需要 pending 闸门；裸数字只在 `pending == 1` 时是审批。

## 刻意不做的事

- **✅ 完成回执只给跑够 60s 的轮次**。每轮都发的话，用户盯着屏幕看完答案、紧接着又收到一条重复的「完成」，是纯噪音，且每轮多烧一条 IM 消息。而这轮回执的用途（人已走开、回来要知道结果）只对长任务成立。阈值 `LONG_TURN` 与心跳同源。
- **心跳降频时明说一次**，不静默拉长间隔——用户看到的最后一条心跳可能比真实进度落后很多。`HEARTBEAT_MAX_BEATS` 是泄漏兜底，不是产品决策。
- **中断提示挂 `/tasks`，不做启动时主动推送**。启动时用户多半不在，只会收到一条无人认领的消息；且枚举「所有配对过的 IM 身份」要另写一遍。
- **`/继续` 不做盲重放**。取回原文后加「请先检查现状再决定怎么做，不要假定任何操作都没做过」；原样重发会让上一轮已完成的副作用执行两次。`dedup_key` 必须重新生成，否则会被去重表当成重投丢掉。
- **超队列上限时挤掉最旧并把摘要并进下一轮**，不静默丢。摘要只并一次，不粘到每一条上。

## Alternatives considered

- **保留 `interrupt_all()` 语义，只改进回执**：不做。`session.rs` 旧注释已记录这个隐患——一个闲着的 IM 用户发 `/stop` 就能掐掉本机正在跑的会话。
- **`/stop` 继续按 `running` 预查再决定要不要中断**：不做。先查后动的两步之间轮次可能刚好结束，会把「中断成功」误报成「没有在跑」。直接用 `interrupt_session` 的返回值判定，顺带消掉 TOCTOU。
- **数字菜单 + `pending == 0` 闸门**（本文早期方案）：不做。同上，与审批 1/2/3 同形。
- **心跳查 `running` 决定退出**（本文早期方案）：不做。多余的一层，且跨 task 借不到那个非 `Arc` 的字段。
- **steer 在 builder 里拦截 `pending_steer`**（本文早期方案，标注为「唯一需现场确认的接线点」）：不做。接线点不存在，加个访问器即可。
- **用 `TaskRecordSink` 既有轮次记录判断中断**（本文早期方案）：否决并撤回。该 sink 只在 `SubAgentOrchestrator` 里被调用，记的是 sub-agent；主对话轮次当时不写记录。`list_by_conversation` 虽存在且按时间倒序，但拿它判「上次对话中断」会双向出错：没用过 `delegate_task` 的会话查出来是空的（漏报），用过的会把子任务误当对话轮次（误报）。
- **给主轮次补终态记录 vs. 继续用启发式**：选了补记录。既然要动就动到根因；启发式那个「首次落盘前重启与真中断同形」的误报源会一直留着。
- **中断提示在网关启动时主动推送**：否决。见「刻意不做的事」。
- **Resume 原样重发上一条 user 消息**：否决，副作用会执行两次。

## Consequences

What we give up:

- **`/stop` 是对已上线 QQ / 企微用户的可见行为变更**：从「波及本机所有会话」改为「只中断发起者会话」。这是修正既有隐患，但用户能感知到，release note 须写明。
- **IM 通道现在每次长轮次会多产生心跳消息**，且降频后每 5 分钟一条。极端情况下（30 分钟以上的任务）IM 消息量明显上升——这是「人不在电脑前」场景必须付的税。
- **每会话多两个内存注册表**（`active_interrupts` / `steer_buffers`），且必须与轮次同生命周期。漏注销就是跨会话串指令（已由「steer 按会话隔离」测试钉住）。
- **主轮次每次都多写一条 `TaskRecord`**（running + terminal 各一条），effects.db 增长加快。换来的是崩溃恢复有真实输入。
- **未测项**：`enqueue` / `drain_queue` 挂在 `ChannelRuntime` 上，构造它需要 `HttpServerState` fixture，本轮未搭；「不跨 await 持锁」靠代码审查保证，没有自动化断言。
- **测试规模**：`hermix-agent` 1045 / `hermix-server` 284，全绿；本轮新增 13 个。
