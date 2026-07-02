# 群聊"不回复"真正的根因分析

> 结论：**后端整个 dispatch 链路的所有错误都被静默吞没**，前端看到消息发出但没有任何回复。

---

## 核心链路

```
用户发消息 → POST /api/groupchat/rooms/{id}/message
  ├─ Step 0: 保存用户消息到数据库 ✅
  ├─ Step 1: dispatcher.dispatch_to_agent(leader) ❌   ← 只打 warn
  │   └─ 失败 → 降级为 broadcast_to_agents()
  │       └─ 每个 agent dispatch 失败 → 也只打 warn
  └─ Step 2: 返回 Json(SendMessageResponse { ..., agent_messages: [] })
```

---

## 根因一：所有错误被静默吞没（P0）

**`groupchat_api.rs` 中 6 处 `match { Err(e) => tracing::warn!(...) }`，全部只写日志不反馈到前端**：

| 位置 | 行号 | 失败时的行为 |
|------|------|-------------|
| Leader 调度失败 | 978 | `warn!(...)` → 降级 broadcast |
| 子任务分发失败 | 934 | `warn!(...)` → 继续下一个 |
| Leader 汇总失败 | 972 | `warn!(...)` → 静默 |
| 公告决策失败 | 862 | `warn!(...)` → 不发布 |
| Broadcast 分发失败 | 1026 | `warn!(...)` → 继续下一个 |
| Leader dispatch_subtask 失败 | 见 dispatcher | 返回 `Err(String)` → 被 caller 的 warn 吞掉 |

**效果**：所有 agent dispatch 都失败了，但 `agent_responses` 为空数组，用户看到的回复是空的。

## 根因二：LLM Provider 未配置或不可用（P0）

**`local_agent_service.rs:902-908`**：
```rust
None => {
    return Err(AgentError::Config(
        "No LLM provider has been configured..."
    ));
}
```

当用户没有配置 API Key，或者配置的 provider 无法连接时，`send_message` 返回 `AgentError::Config` / `AgentError::Provider`，但 groupchat 层只打 `warn!`，用户看不到。

## 根因三：leader_agent.session_id 可能为空或未初始化

**`groupchat_sqlite.rs:250`**：
```rust
"INSERT INTO groupchat_room_agents (room_id, agent_id, role, session_id, model_override, display_name)
 VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
params![room.id, LEADER_AGENT_ID, leader_session_id],
```

**问题**：SQLite 的 `create_room` （第 230-252 行）中，leader 的 `session_id` 是单独写的，但 `groupchat_room_agents` 表的 schema 可能缺少默认值或约束。如果 SQLite DB 文件损坏或 schema 不对，INSERT 会失败。

更重要的是，**Tauri webview 的 `agent_service` 可能在某些启动路径下为 `None`**（见 `lib.rs:1298`）：
```rust
if let Some(svc) = agent_service {
    let dispatcher = ...;
    mgr.set_dispatcher(dispatcher);
}
```
如果 `agent_service` 为 `None`，`mgr.dispatcher()` 返回 `None` → `send_message` 跳过整个 dispatch 块（第 798 行），直接返回空 agent_messages。

## 根因四：dispatch_subtask session_id 与 AgentService 不匹配

在 `groupchat_dispatcher.rs:20-40` 中，`dispatch_to_agent` 使用 `&profile.session_id`（来自 `AgentRoomProfile`）作为 `AgentService::send_message` 的 session_id。

但 `AgentService::send_message` 期望这个 session_id 是 **session_persistence 中已存在的 session**。groupchat 使用的合成 session_id（`groupchat-leader-<uuid>`）可能被 `session_persistence.load_session()` 返回空（不存在该 session），然后：

1. 创建空 session
2. 添加系统 prompt（通过 overrides）
3. 添加用户消息
4. 调用 LLM
5. persist_session_with_summary() 保存

这个过程本身没问题，但如果 `persist_session_with_summary` 失败（比如文件锁），会打 warn 但不会中断回复。所以这只是一个间接影响。

## 根因五：前端没有后端失败的可视化

前端 `GroupChatView.tsx` 的 `handleSendMessage`（第 525-560 行）：

```tsx
const resp = await api.sendGroupChatMessage(activeRoomId, text);
setMessages((prev) => {
    const without = prev.filter((m) => m.id !== localId);
    const agentMsgs = resp.agent_messages.map(apiMsgToChatMsg);
    return [...without, apiMsgToChatMsg(resp.user_message), ...agentMsgs];
});
```

当 `resp.agent_messages` 为空数组时，前端只是去掉了 optimistic message 后什么都不加，用户看到的是 **对话清空 + 空返回**。

---

## 修复方向建议

### P0：让 dispatch 失败展示到前端（10-20 行改动）

在 `send_message` 中，当所有 dispatch 都失败或 `agent_responses` 为空时，往其中插入一条系统消息：

```rust
// 在返回 Json 之前
if agent_responses.is_empty() {
    // 保存一条系统提示消息，前端展示为灰色提示
    let _ = manager.save_agent_message(
        &room_id,
        "__system__",
        "系统",
        "所有成员暂时无法回复。请检查 LLM Provider 是否已配置。",
        MessageType::System,
    );
}
```

同时前端需要处理 `__system__` sender_id 的消息，显示为灰色提示气泡。

### P1：启动时检测 dispatcher 空指针

在 `init_groupchat_manager` 中，如果 `agent_service` 为 `None`，打一条显眼的启动警告并写日志到文件，同时创建一条房间系统消息"群聊功能尚未就绪，请先配置 LLM Provider"。

### P2：dispatch 成功/失败计数

给 `SendMessageResponse` 加 `dispatched: usize, succeeded: usize, failed: usize` 三个字段，前端可以在 debug 模式下展示，方便用户/开发者排查。

---

## 相关文件

| 文件 | 作用 |
|------|------|
| `crates/hermix-server/src/dashboard/groupchat_api.rs` | send_message 编排，6 处 silent error |
| `crates/hermix-server/src/dashboard/groupchat_dispatcher.rs` | dispatch_to_agent 桥接 AgentService |
| `crates/hermix-agent/src/local_agent_service.rs` | send_message 真正发 LLM 请求 |
| `crates/hermix-server/src/lib.rs` | init_groupchat_manager，agent_service 注入 |
| `crates/hermix-gateway/src/groupchat/manager.rs` | GroupChatManager 统一入口 |
| `apps/desktop-tauri/src/components/GroupChatView.tsx` | 前端消息显示逻辑 |
