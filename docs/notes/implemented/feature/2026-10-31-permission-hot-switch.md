# Decision Note: 放手模式当前轮热生效（会话级权限档热更）

Status: implemented — 2026-10-31 落地

## Decision

「放手模式」切换（审批卡选项 3 / Composer 开关）从「只对下一条消息生效」改为**当前轮内立即生效**。实现汇点是 `ApprovalRegistry`（crates/hermix-server/src/approval.rs）——派发层（`decide_pre_tool_dispatch`）、资源门、工具内部确认（terminal 的 Caution 确认）三条审批路径最终都经 `request_approval` 拿裁决：

1. `ApprovalRegistry` 增加 `full_sessions: HashSet<session_id>` + `set_full_mode` / `is_full_mode`。置位后 `request_approval` 不登记、不发卡，直接预解析 `Approved` 返回。
2. 新端点 `POST /v1/sessions/{session_id}/permission`（body `{level: "full"|"default"}`，routes::set_session_permission）：置位/清除开关；切到 full 时 `resolve_all` 把该会话已挂起的全部 pending 按Approved 放行。
3. 前端 `ChatView::changePermissionLevel` 成为切换唯一入口：写 localStorage 偏好 + POST 热更 + （切到 full 时）对 pending 卡逐个走既有 approve 结算路径清抽屉。`App::onApprovalRequest` 顶部读同一 localStorage 键，放手模式下直接以已批准结算、不渲染新卡（后端预解析后 agent 侧不会有等待）。
4. **重武装**：WS 流式/非流式与 HTTP 消息路径组装 overrides 时，若请求显式携带 `permission` 字段则按其重置开关。桌面端每条消息都带当前档位，因此重启后第一条消息即恢复；不带字段的调用方（自动化调度等）不会悄悄清掉用户刚点开的放手模式。

硬底线（hardline blocklist）不受影响：它在派发层先于一切审批路径执行，`Full` 本就绕不过。

## Alternatives considered

- **热更本轮捕获的权限档**（给 `AgentOverrides`/`TerminalApprovalContext` 加共享句柄，派发闭包与 terminal 每次读句柄）：要动 hermix-core/hermix-tools 两个 crate 的公共结构 + 23 处 `AgentOverrides` 字面量构造点，而三条路径中任何一条漏改都会留下「还弹卡」的暗道。注册表汇点一处覆盖全部。
- **只在注册表 `request_approval` 预解析、前端照常渲染卡片**：会产出发出即已决定的幽灵卡。前端在放手模式下直接批准结算，幽灵卡不存在。
- **WS 控制消息**（`set_permission` 帧）：需要 transport/协议三端同步加帧型；REST 经既有 stdio 桥 HTTP 通道，零协议改动。

## Consequences

- 当轮内语义与下轮略有差异：当轮热生效不绕过 Tirith `Block`（结构政策仍然拒绝），下轮起随消息 `permission: full` 完整绕过。这是有意保留的安全余量。
- 开关是进程内存态，随会话存在；重启由前端首条消息重武装（前端 localStorage 是持久真值）。
- `resolve_all` 整表摘除会话的 pending；`batches` 索引可能留下指向已放行 id 的陈旧条目，属既有文档确认的有界泄漏，`resolve_batch` 对消失 id 幂等返回 false。
- 手机端/远程路径：端点带 `require_own_session` 闸，行为一致。
