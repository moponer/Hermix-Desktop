# Decision Note: 进行中回合的事件在连接断开后存活（重挂载）

Status: proposed — ①②（事件通道重挂载）待专项实施；③④ 已以评审后修订形态落地（见下）。

## Decision

把「进行中回合」的事件流从发起连接上解耦，使断线/重连后审批卡与流式输出存活。四步，按性价比排序：

1. **事件镜像移出 forward loop**：`ws.rs` 中 per-turn `mpsc` 的消费端目前独占于发起连接的 forward 循环——连接死即 `rx` drop，后续 `tx.send` 全部静默失败（coalescer 只留 debug 日志）。改为由独立 drain task 持有 `rx` 并写入 `session_broadcast`（seq 戳 `next_envelope_seq` 已存在），回合事件从此与连接生死解耦。
2. **实现订阅侧**：`session_broadcast` 注册表现在是只写的（没有订阅者取号泵送；原先引用的 `spawn_broadcast_forwarder` 从未存在，注释已于 2026-10-18 清掉）。新连接 `Connected` 后按 `since_seq` 订阅并泵送活尾巴（含 `RequestApprovalsBatch`）。
3. **新连接时推未决审批**：连接建立（或 `task_in_flight` 应答）时服务端主动推 `approval_registry.list_pending(session_id)`——注册表进程级、任意连接可解（ws.rs:2841 已支持），卡片无需依赖前端重发才补投。
4. **修 live-decline 分支的宽限竞态**：`task_in_flight` 分支不取消旧连接死亡时排的 90s 宽限定时器（zombie 分支有 `abort_pending_grace`）——用户 90s 内重连重发，审批仍被陈旧定时器自动拒绝。**已落地**：live-decline 分支入口处 `abort_pending_grace`（旧 socket 的宽限因存活重连而过时）。

**评审后已随 ④ 一并落地的第 ③ 步修订形态**：`resolveApproval` 的 stale-click 分支原无条件把卡标 `expired`——重载后点击注册表中仍存活的审批会被误标且 REST 兜底查不到会话。现改为：在本地会话状态中定位卡片元数据，deadline 未过 → 按卡片**归属会话**（非 activeSessionId）走 REST 决决并盖真实决定；deadline 已过或 REST 失败才标 expired。

前端已有的自救（本次已落地）作为第 0 步兜底保留：审批 15s 未决 REST 直查注册表重投影（App.tsx `approvalRescueTimersRef`），失焦系统通知（f7a3a677）。

## Root facts（2026-10-05 事故，另一台机器复现）

- 任务收尾清理 → `storage_clean`（Dangerous）触发审批 → 事件未达 UI → agent 干等 600s（`default_approval_timeout_secs`）自动拒绝；期间用户新消息被会话忙路径排队（ws.rs:2350）→ 感知「发了消息一直不回」。
- 三个已实锤丢点：① 连接静默断开（写不报错）时审批 envelope 写进死 socket，无宽限定时器、无 UI 信号；② 检测到断开时 forward 循环退出 → rx drop → 事件静默丢弃；③ 前端 `onConnectionLost/Restored` 从未被调用（useWsConnection.ts 头注释：stdio 桥迁移后无可观测断链周期），「网络已恢复」toast 是死代码；300s 看门狗又在审批等待期被主动挂起（useStreamParser.ts:527）。
- 叠加因素：该机器旧构建含 setState 风暴 UI 冻结 bug（0be89975 已修）——渲染冻结期间事件到达也无法成卡。

## Alternatives considered

- **只调短 600s 超时**：治标。等待本身合法（人在响应），问题是无信号；且全局值被 IM/过夜场景依赖（config.rs 注释）。
- **审批事件走轮询而非推送**（前端每 N 秒 GET pending）：桥活时已被第 0 步兜底覆盖；桥死时轮询同样死。推送+重投影仍必要。
- **前端本地重放整个流**：seq 对账端点只服务已提交消息（routes.rs:1007 明示增量重放未实现），中途 Text 不落库，重放无源。

## Consequences

- 第 1-2 步触碰 forward 循环主路径，需压测多连接/多会话扇出；订阅侧实现要处理慢消费者（broadcast channel lagged 语义）与 seq 去重。
- 事件镜像进 broadcast 后，重连补发会重放已决定卡片的原始事件——前端 step 写入按 requestId 去重已幂等（App.tsx onApprovalRequest），但 resolver 重注册语义需评审。
