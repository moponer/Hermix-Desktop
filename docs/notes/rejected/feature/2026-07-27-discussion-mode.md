# Decision Note: 多 Agent 讨论模式（Discussion Mode）

Status: rejected — 讨论模式随群聊模块于 2026-09-21 整体下线（`hermix-gateway/src/groupchat`、transport 的 `Discussion*` 变体、前端 `DiscussionPanel` 等）；本文保留其第一性原理推导与失败模式枚举

## Decision

### 问题陈述

当时的群聊架构是**任务委派模式**（Task Delegation），不是讨论模式：

```
用户 → 总管 → (并行派发子任务) → 成员各自执行 → 总管验收
```

**核心限制**：

1. **Session 隔离**：每个 agent 有独立 `session_id`，`ServerAgentDispatcher` 直接传给 `agent_service.send_message(session_id, ...)`。成员 A 看不到成员 B 说了什么。
2. **无跨 Agent 上下文注入**：派发时 `system_prompt` 为空（`runtime.rs:374`），`message` 只是任务描述。成员不知道用户原话、不知道其他成员的产出。
3. **一次性派发**：调度器对每个子任务只 Dispatch 一次。成员回复后直接进入 Review，没有"回应彼此"的循环。
4. **无自发消息**：Agent 无法主动发言。唯一的"自发"机制是 `IdleChatManager`，但它的 prompt 是通用的（"分享一下最近学到的技巧"），不是讨论。

用户期望：4 个成员在房间内**自发进行多轮讨论**，互相看到消息、回应彼此、最终产出共识。

### 第一性原理推导

**什么是"讨论"？** 讨论是一个**序列化的回合制信息交换过程**：

```
回合 1: A 发言 → B 读到 A 的发言 → B 回应 → C 读到 A+B 的发言 → C 回应 → ...
回合 2: A 读到 B+C 的回应 → A 再次发言 → ...
...
结束:  主持人总结
```

**物理约束**：

- 每次 LLM 调用 = 1 次 API 请求 = 固定 token 成本 + 延迟（2-10 秒）
- 4 个 agent × 5 轮 = 20 次 LLM 调用 ≈ 60-200 秒
- 上下文窗口有限（32K-128K tokens），对话历史线性增长
- LLM 不能"监听"——只能被调用时传入 prompt
- 真正的并行讨论不可能——A 的回应必须在 B 发言之后才能引用 B

**LLM 能力边界** —— 单次调用能做的：读取一段对话历史、生成一条回应、做一个决策（继续/停止/转话题）。**不能**做的：主动发起消息（必须被外部调用）、持续监听（没有事件循环）、真正"思考"是否要回应（只能被 prompt 引导）。

**结论**：讨论的"自发性"必须由**外部编排引擎**模拟——引擎决定何时调用哪个 agent，注入什么上下文，以及何时停止。

**核心需求推导**

| 约束 | 推导出的需求 |
|------|-------------|---|
| Agent 看不到彼此的消息 | **共享上下文注入**：dispatch 时把房间最近消息打包进 prompt |
| Agent 不能主动发言 | **编排引擎**：外部循环决定调用顺序和轮次 |
| 上下文线性增长 | **滑动窗口**：只注入最近 N 条消息，超限时摘要压缩 |
| 讨论可能无限循环 | **终止条件**：max_rounds 硬上限 + leader 裁决 |
| 用户可能想插话 | **中断机制**：用户消息打断讨论，重新进入正常流程 |
| 并行回应无法互相引用 | **序列化发言**：同一轮内按顺序逐个调用，每人看到前面人的发言 |

### 架构设计

**新增模块**

```
crates/hermix-gateway/src/groupchat/
  ├── discussion.rs          ← 新增：讨论引擎核心
  └── discussion_prompt.rs   ← 新增：讨论专用 prompt 构建
```

**新增数据结构**

```rust
// discussion.rs

/// 讨论模式配置 — 由 Leader 在 LeaderDecision 中指定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscussionConfig {
    /// 讨论话题
    pub topic: String,
    /// 最大轮次（硬上限，默认 5，最大 10）
    pub max_rounds: u8,
    /// 参与者 agent_id 列表（不含 leader）
    pub participants: Vec<String>,
    /// 每轮上下文窗口（保留最近 N 条消息）
    pub context_window: usize,  // 默认 8
    /// 每条消息最大 token
    pub max_message_tokens: usize,  // 默认 512
}

impl Default for DiscussionConfig {
    fn default() -> Self {
        Self { topic: String::new(), max_rounds: 5, participants: Vec::new(),
               context_window: 8, max_message_tokens: 512 }
    }
}

/// 讨论状态 — 运行时跟踪
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscussionState {
    pub discussion_id: String,
    pub room_id: String,
    /// 关联的根任务节点 ID
    pub node_id: String,
    pub config: DiscussionConfig,
    /// 当前轮次（从 1 开始）
    pub current_round: u8,
    /// 讨论中的所有消息（按时间顺序）
    pub messages: Vec<DiscussionMessage>,
    pub status: DiscussionStatus,
    /// 触发讨论的用户消息 ID
    pub triggered_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DiscussionStatus {
    Pending,              // 等待开始
    Active,               // 进行中
    AwaitingModeration,   // 轮次间 — 等待 leader 裁决
    Concluded,            // 已结束 — 正常完成
    Cancelled,            // 已取消 — 用户打断或错误
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscussionMessage {
    pub id: String, pub agent_id: String, pub agent_name: String,
    pub content: String, pub round: u8, pub timestamp: DateTime<Utc>,
}

/// Leader 的轮间裁决
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationDecision {
    /// 继续下一轮 / 结束讨论 / 调整话题
    pub action: ModerationAction,
    /// 如果继续，下一轮的话题引导（可空）
    pub guidance: Option<String>,
    /// 本轮摘要（给用户看）
    pub round_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ModerationAction {
    Continue,
    Conclude,
    Redirect(String),  // 新话题
}
```

**新增调度器事件与动作**

```rust
// scheduler.rs — 新增 SchedulerEvent 变体
pub enum SchedulerEvent {
    // ... 现有变体 ...
    DiscussionStarted { room_id: String, node_id: String, config: DiscussionConfig },
    DiscussionMemberSpoke { room_id: String, discussion_id: String, agent_id: String,
                            content: String, round: u8 },
    DiscussionRoundCompleted { room_id: String, discussion_id: String, round: u8 },
    DiscussionModerated { room_id: String, discussion_id: String, decision: ModerationDecision },
    DiscussionConcluded { room_id: String, discussion_id: String, summary: String },
}

// scheduler.rs — 新增 SchedulerAction 变体
pub enum SchedulerAction {
    // ... 现有变体 ...
    /// 派发讨论发言给指定成员
    DispatchDiscussion { discussion_id: String, agent_id: String, round: u8, context: String },
    /// 请求 leader 做轮间裁决
    RequestModeration { discussion_id: String, round: u8, round_messages: Vec<DiscussionMessage> },
    /// 讨论结束，leader 生成最终总结
    FinalizeDiscussion { discussion_id: String },
}
```

**新增 TaskPhase**

```rust
// task_types.rs
pub enum TaskPhase {
    Idle, Planning, Executing, Review, Revising, Completed, Cancelled,
    Discussing,  // ← 新增
}
```

**LeaderDecision 扩展**

```rust
// agent_contract.rs
pub struct LeaderDecision {
    pub analysis: String,
    pub subtasks: Vec<SubtaskAssignment>,
    pub announcement: Option<String>,
    pub needs_user_confirmation: bool,
    pub handoff: Option<HandoffDirective>,
    /// 新增：发起讨论模式
    /// 当 leader 认为需要团队讨论而非直接执行时填写
    #[serde(default)]
    pub discussion: Option<DiscussionConfig>,
}
```

Leader prompt 扩展（在 `build_leader_decision_prompt` 中新增）：

```
6. `discussion` 通常为 null。仅当你认为需要团队成员**多轮讨论**某个话题
   才得出结论时填写：{{"topic":"讨论话题","max_rounds":5,"participants":["agent_id1","agent_id2"]}}
   - discussion 和 subtasks 互斥：有讨论就不派发子任务
   - 讨论结束后 leader 会自动收到讨论记录并做总结
```

**讨论引擎核心流程**

```
┌─────────────────────────────────────────────────────────────────┐
│  DiscussionEngine::run()                                        │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  1. 初始化 DiscussionState                                │   │
│  │     - 生成 discussion_id                                   │   │
│  │     - status = Active                                      │   │
│  │     - current_round = 1                                    │   │
│  └────────────────────────┬─────────────────────────────────┘   │
│                           ▼                                     │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  2. Round Loop (while current_round <= max_rounds)        │   │
│  │  ┌────────────────────────────────────────────────┐      │   │
│  │  │  2a. 对每个 participant (序列化):               │      │   │
│  │  │      - 构建上下文: 话题 + 最近 N 条讨论消息 + 发言指令      │   │
│  │  │      - dispatch_subtask_stream(agent, context)  │      │   │
│  │  │      - 流式输出 → RoomEvent::AgentThinkingChunk  │      │   │
│  │  │      - 保存为 DiscussionMessage                 │      │   │
│  │  │      - 广播给房间（前端实时渲染）                 │      │   │
│  │  └────────────────────────────────────────────────┘      │   │
│  │  ┌────────────────────────────────────────────────┐      │   │
│  │  │  2b. 轮间裁决: 收集本轮所有发言 → dispatch to leader       │   │
│  │  │      "请决定: 继续/结束/转话题" → 解析 ModerationDecision   │   │
│  │  └────────────────────────────────────────────────┘      │   │
│  │  ┌────────────────────────────────────────────────┐      │   │
│  │  │  2c. 根据裁决:                                    │      │   │
│  │  │      Continue  → current_round += 1, 回到 2a    │      │   │
│  │  │      Conclude  → 跳到 3                          │      │   │
│  │  │      Redirect → 更新 topic, current_round += 1   │      │   │
│  │  └────────────────────────────────────────────────┘      │   │
│  └──────────────────────────────────────────────────────────┘   │
│                           ▼                                     │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  3. 最终总结                                                │   │
│  │     - dispatch to leader: 全部讨论记录 + "请总结"          │   │
│  │     - 保存为房间消息 → status = Concluded                  │   │
│  │     - scheduler: TaskPhase::Completed                     │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**上下文构建（关键）**

```rust
// discussion_prompt.rs

/// 为讨论中的某个 agent 构建消息（作为 dispatch_subtask_stream 的 message 参数）
pub fn build_discussion_message(
    agent_name: &str, agent_role: &str, topic: &str,
    discussion_messages: &[DiscussionMessage],
    context_window: usize, round: u8,
) -> String {
    let mut parts = Vec::new();

    // 1. 话题
    parts.push(format!("📋 讨论话题：{}", topic));

    // 2. 最近对话（滑动窗口）
    let recent: Vec<&DiscussionMessage> = discussion_messages
        .iter().rev().take(context_window).rev().collect();

    if !recent.is_empty() {
        parts.push("━━━ 讨论记录 ━━━".to_string());
        for msg in &recent {
            parts.push(format!("[{}] {}", msg.agent_name, msg.content));
        }
        parts.push("━━━━━━━━━━━━━━".to_string());
    }

    // 3. 发言指令
    parts.push(format!(
        "你是{}，这是第{}轮讨论。请针对上述话题发表你的观点。\n\n\
         要求：\n\
         - 直接陈述你的观点，不要复述别人的话\n\
         - 如果你同意或不同意某人的观点，说明理由\n\
         - 保持简洁（不超过 {} 字）\n\
         - 如果你确实没有要补充的，回复「PASS」",
        agent_name, round, 800,  // ~512 tokens
    ));

    parts.join("\n\n")
}
```

轮间裁决消息要求 leader 严格返回 JSON：

```json
{
  "action": "continue" | "conclude" | "redirect",
  "guidance": "下一轮引导（仅 continue/redirect 时填写，否则 null）",
  "round_summary": "本轮讨论摘要（1-2 句话，给用户看）"
}
```
规则：`continue` = 讨论还有价值；`conclude` = 已达成共识或充分讨论；`redirect` = 需要转向新话题（在 guidance 中写明新方向）；**如果成员们只是互相附和没有新观点，选 `conclude`**。

最终总结消息要求 leader 提炼核心观点和共识、列出分歧点、给出结论和建议，直接输出总结内容（不要 JSON 格式）。

**DiscussionEngine 核心控制流**

```rust
// discussion.rs
pub struct DiscussionEngine { manager: Arc<GroupChatManager> }

impl DiscussionEngine {
    pub fn new(manager: Arc<GroupChatManager>) -> Self { Self { manager } }

    /// 运行完整讨论流程。这是一个长异步函数，会持续到讨论结束或被取消。
    pub async fn run(self: Arc<Self>, room_id: &str, node_id: &str,
                     config: DiscussionConfig, trigger_message: &str)
        -> Result<DiscussionState, String> {
        let discussion_id = uuid::Uuid::new_v4().to_string();
        let mut state = DiscussionState { /* … current_round: 1, status: Active … */ };
        let dispatcher = self.manager.dispatcher().ok_or("no dispatcher available")?;

        // 主循环
        while state.current_round <= state.config.max_rounds
            && state.status == DiscussionStatus::Active {
            // ── 2a. 成员发言（序列化）──
            let round_messages = self.run_round(&dispatcher, &mut state, room_id).await?;

            // 检查是否所有人都 PASS
            let all_pass = round_messages.iter().all(|m| m.content.trim() == "PASS");
            if all_pass {
                // 无人发言 → 提前结束
                tracing::info!(discussion = %discussion_id, round = state.current_round,
                               "all members passed, ending discussion early");
                break;
            }

            // ── 2b. Leader 裁决 ──
            state.status = DiscussionStatus::AwaitingModeration;
            let decision = self.request_moderation(&dispatcher, &state, &round_messages).await?;
            self.publish_round_summary(room_id, &state, &decision).await;

            // ── 2c. 根据裁决行动 ──
            match &decision.action {
                ModerationAction::Continue  => { state.current_round += 1; state.status = Active; }
                ModerationAction::Conclude  => { state.status = Concluded; break; }
                ModerationAction::Redirect(t) => { state.config.topic = t.clone();
                                                   state.current_round += 1; state.status = Active; }
            }
        }

        // 硬上限触发
        if state.status == DiscussionStatus::Active { state.status = DiscussionStatus::Concluded; }

        // ── 3. 最终总结 ──
        let conclusion = self.request_conclusion(&dispatcher, &state).await?;
        self.save_conclusion(room_id, &state, &conclusion).await;
        Ok(state)
    }
```

`run_round` 的要点：每个参与者使用 `session_id = format!("{}-discussion-{}", room_id, state.discussion_id)`（独立 session，不污染原 session）；构建上下文后走 `dispatch_subtask_stream`，流式 chunk 经 `manager.publish_discussion_chunk` 实时推前端；成功后存为 `DiscussionMessage` 并 `save_agent_message(..., MessageType::Discussion)` 持久化到房间消息表；**F7：dispatch 失败（401/超时）时 `tracing::warn!` 后跳过该 agent，继续轮次**。

`parse_moderation_decision` 容忍 ```json fence 和前后文字：取首个 `{` 到末个 `}` 之间的块反序列化；`action` 未知值直接返回 `Err`。

**集成到 runtime.rs**

在 `dispatch_action` 的 leader 路径（`agent_id_owned == "__leader__"`）解析出 `LeaderDecision` 后新增讨论分支：若 `decision.discussion` 为 `Some`，则 `DiscussionEngine::new(self.manager.clone())` 并 `tokio::spawn` 后台跑 `engine.run(...)`，成功发 `RoomEvent::DiscussionConcluded`，失败发 `RoomEvent::SoftError { code: "discussion_failed" }`，随后**提前返回 `emitted`**（讨论在后台运行，不阻塞当前 dispatch）；否则走既有的 subtask / handoff / direct answer 路径。

**新增 RoomEvent**

```rust
// runtime.rs — RoomEvent 新增变体
pub enum RoomEvent {
    // ... 现有变体 ...
    DiscussionStarted { room_id: String, discussion_id: String, topic: String,
                        participants: Vec<String>, max_rounds: u8 },
    /// 讨论中的发言（区别于普通 AgentThinkingChunk）
    DiscussionMessage { room_id: String, discussion_id: String, agent_id: String,
                        agent_name: String, content: String, round: u8 },
    /// 一轮讨论结束，leader 的轮次摘要
    DiscussionRoundSummary { room_id: String, discussion_id: String, round: u8,
                             summary: String,
                             next_action: String },  // "continue" / "conclude" / "redirect: 新话题"
    /// 讨论已结束
    DiscussionConcluded { room_id: String, discussion_id: String, total_rounds: u8 },
}
```

**完整流程**

```
用户发送消息
    ▼
on_user_message → TaskCreated → Dispatch{leader}
    ▼
Leader LLM 思考 → LeaderDecision JSON
    ├── 有 subtasks → 现有流程（并行派发子任务）
    ├── 有 handoff → 现有流程（转交/委托）
    ├── 有 discussion → ★ 新流程 ★
    │   │   ▼
    │   DiscussionEngine::run()
    │   ┌── Round 1 ──────────────────────────────┐
    │   │  Alice ← [topic + context]              │
    │   │  Alice: "我认为应该用 OAuth..."          │
    │   │  Bob   ← [topic + Alice's message]      │
    │   │  Bob: "同意 OAuth，但还需要 2FA..."      │
    │   │  Charlie ← [topic + Alice + Bob]        │
    │   │  Charlie: "2FA 会对转化率有影响..."      │
    │   │  Leader ← [all round 1 messages]        │
    │   │  Leader: {action: "continue", summary: "讨论了 OAuth+2FA"} │
    │   └─────────────────────────────────────────┘
    │   ┌── Round 2 ──────────────────────────────┐
    │   │  Alice ← [topic + R1 messages]          │
    │   │  Alice: "2FA 可以用可选模式..."          │
    │   │  Bob   ← [topic + R1 + Alice R2]        │
    │   │  Bob: "可选 2FA + 渐进引导比较好"        │
    │   │  Leader: {action: "conclude", summary: "达成共识: 可选2FA"} │
    │   └─────────────────────────────────────────┘
    │   ▼
    │   Leader ← [全部讨论记录] → "讨论总结: 团队建议采用 OAuth + 可选 2FA..."
    │   ▼
    │   保存总结 → DiscussionConcluded → TaskPhase → Completed
    └── 都没有 → 直接回答（现有流程）
```

**用户中断机制**

```
讨论进行中 ← 用户发送新消息
    ▼
on_user_message 检查 room 的 discussion_active 标志
    ├── true  → 1. 取消正在进行的讨论 (abort tokio task)
    │           2. DiscussionState.status = Cancelled
    │           3. 广播 DiscussionCancelled 事件
    │           4. 正常处理用户新消息
    └── false → 正常流程
```

`RoomRuntime` 新增 `pub active_discussion: Option<ActiveDiscussion>`，`ActiveDiscussion { discussion_id: String, cancel_token: tokio::sync::CancellationToken }`。

**实现路径**

Phase 1 — 核心引擎 (MVP)：`task_types.rs` 新增 `TaskPhase::Discussing`；`discussion.rs` 新增 `DiscussionConfig`/`DiscussionState`/`DiscussionMessage`/`ModerationDecision`；`discussion_prompt.rs` 新增三个 builder；`discussion.rs` 实现 `DiscussionEngine::run()`/`run_round()`/`request_moderation()`/`request_conclusion()`；`agent_contract.rs` 的 `LeaderDecision` 新增 `discussion` 字段 + prompt 说明；`runtime.rs` 的 `dispatch_action` leader path 新增讨论分支 + `RoomEvent` 讨论变体；`mod.rs` 导出新模块；`persistence.rs` 新增 `MessageType::Discussion`。

Phase 2 — 用户中断 + 状态管理：`RoomRuntime` 新增 `active_discussion`；`on_user_message` 检查并取消活跃讨论；`discussion.rs` 支持 `CancellationToken` 优雅取消；`groupchat_api.rs` 新增 `GET /api/groupchat/rooms/:id/discussion` 查询当前讨论状态。

Phase 3 — 前端适配：`useGroupChatWebSocket.ts` 处理 `discussion_started` / `discussion_message` / `discussion_round_summary` / `discussion_concluded`；`MessageList.tsx` 讨论消息特殊渲染（轮次分隔线、话题标签）；`MessageBubble.tsx` 讨论消息样式区分（不同轮次颜色渐变）；新增 `DiscussionPanel.tsx` 显示当前讨论状态、轮次进度、话题。

Phase 4 — 高级特性：滑动窗口上下文压缩（超过窗口时摘要旧消息）；PASS 检测 + 提前结束；`yanluo.rs` 讨论质量检测器（跑题检测、附和检测）；讨论结果写入 agent 记忆。

**测试策略**：单测覆盖 `discussion_prompt.rs` 的上下文构建/滑动窗口/PASS 指令，`discussion.rs` 的 Mock dispatcher 轮次流转/裁决解析/提前结束，`parse_moderation_decision` 对 JSON fence、无效 action、缺失字段的容忍。集成测试覆盖 4 agent 讨论 3 轮后 conclude、第 2 轮用户打断、全部 PASS 提前结束、Leader redirect 后话题切换、Dispatcher 失败跳过继续。前端测试覆盖 WS 事件触发 UI 更新、三种风格（modern/classic/compact）下讨论消息渲染、轮次分隔线。

## Alternatives considered

### D1: 序列化 vs 并行发言

| 方案 | 优点 | 缺点 |
|------|------|------|
| 序列化（**选中**） | 每人能看到前面人的发言，真正"讨论" | 慢（4人×5轮=20次串行调用） |
| 并行 | 快（4人同时调用） | 无法互相引用，本质是各自独立回答 |

→ **选择序列化**。讨论的本质就是"听到别人说什么再回应"，并行只是"同时回答同一个问题"。这条直接由物理约束推出：LLM 不能"监听"，A 的回应必须在 B 发言之后才能引用 B。

### D2: Leader 每轮参与 vs 仅在结束时总结

| 方案 | 优点 | 缺点 |
|------|------|------|
| 每轮参与 | Leader 可以引导方向，防止跑题 | 多 1 次 LLM 调用/轮 |
| 仅总结 | 省调用 | 讨论可能失控 |

→ **Leader 仅在每轮结束后做裁决**（继续/停止/转话题），不参与轮内发言。这样每轮 = N 个成员 + 1 个 leader 裁决。

### D3: 共享上下文如何注入

```
方案 A: 把房间消息拼进 system_prompt
方案 B: 把房间消息拼进 message（user turn）    ← 选中
方案 C: 使用独立 session_id，通过 AgentService 的消息历史机制
```

→ **方案 B**。`dispatch_subtask_stream` 的 `message` 参数已经支持任意文本。把"讨论话题 + 最近 N 条消息 + 发言指令"拼成 message，system_prompt 放 agent 身份 + 讨论规则。这样**不需要改 `AgentDispatcher` trait，也不污染 agent 的原 session**。

### 与既有群聊模式的关系

讨论模式与任务委派互斥而非并存：`LeaderDecision` 中 `discussion` 与 `subtasks` 二选一，由 leader 在单次决策中拍板。对应的代价是多了一个 `TaskPhase::Discussing` 状态与一条独立引擎路径。

## Consequences

### 对抗性审查：失败模式枚举

| # | 失败场景 | 触发条件 | 后果 | 缓解方案 |
|---|---------|---------|------|---------|
| F1 | **无限循环** | Leader 持续说"继续讨论"，agents 互相附和 | token 烧光，用户等死 | 硬上限 max_rounds（默认 5，最大 10） |
| F2 | **一人独霸** | Agent A 每轮都长篇大论，其他人被淹没 | 讨论失衡 | 每条消息 max_tokens 限制（512）；轮内每人最多发言 1 次 |
| F3 | **上下文爆炸** | 5 轮 × 4 人 × 500 字 = 10K 字历史 | 单次 prompt 超窗口 | 滑动窗口：保留最近 8 条消息 + 前 N-8 条的摘要 |
| F4 | **Agent 拒绝发言** | LLM 返回空或"我没有要补充的" | 讨论停滞 | 跳过该 agent，继续轮次；如果整轮无人发言 → 提前结束 |
| F5 | **跑题** | Agent 开始讨论无关话题 | 讨论无效 | Leader 每轮结束后注入"当前话题"提醒；跑题检测器 |
| F6 | **用户打断** | 用户在讨论中发送消息 | 讨论流程被打断 | 用户消息优先：暂停讨论 → 处理用户消息 → leader 决定是否恢复讨论 |
| F7 | **Agent 离线/报错** | dispatch 失败 (401, 超时) | 讨论中断 | 跳过该 agent，记录错误，继续轮次 |
| F8 | **所有 agent 附和** | 每人都说"同意上一位" | 无实质讨论 | 轮次摘要检测：如果 N 轮内无新观点 → leader 终止讨论 |
| F9 | **session 污染** | 讨论消息写入 agent session，影响后续任务 | Agent 记忆混乱 | 讨论使用**独立 session_id**（`{room_id}-discussion-{uuid}`），不污染原 session |
| F10 | **并发讨论** | 用户快速发两条讨论请求 | 两个讨论循环交叉 | 每房间同时只允许一个活跃讨论（`discussion_active` 标志） |

### Token 成本：4 agent × 5 轮讨论

| 环节 | 调用次数 | 输入 tokens | 输出 tokens | 总计 |
|------|---------|------------|------------|------|
| 成员发言 (R1) | 4 | ~500/次 | ~400/次 | 3,600 |
| Leader 裁决 (R1) | 1 | ~800 | ~200 | 1,000 |
| 成员发言 (R2) | 4 | ~1,000/次 | ~400/次 | 5,600 |
| Leader 裁决 (R2) | 1 | ~1,200 | ~200 | 1,400 |
| 成员发言 (R3) | 4 | ~1,500/次 | ~400/次 | 7,600 |
| Leader 裁决 (R3) | 1 | ~1,600 | ~200 | 1,800 |
| 成员发言 (R4) | 4 | ~2,000/次 | ~400/次 | 9,600 |
| Leader 裁决 (R4) | 1 | ~2,000 | ~200 | 2,200 |
| 成员发言 (R5) | 4 | ~2,500/次 | ~400/次 | 11,600 |
| Leader 裁决 (R5) | 1 | ~2,500 | ~200 | 2,700 |
| 最终总结 | 1 | ~3,000 | ~800 | 3,800 |
| **总计** | **25** | | | **~50,800** |

约 50K tokens，按 GPT-4o-mini 价格（$0.15/1M input, $0.60/1M output）约 $0.02；按 GPT-4o 价格（$2.50/1M input, $10/1M output）约 $0.15。**同时也是 60-200 秒的墙钟延迟。**

**滑动窗口的必要性**：如果不截断，R5 的输入会达到 ~12K/次（4轮×4人×500字），25 次调用总计 ~150K tokens。滑动窗口将旧消息压缩为摘要，能节省 ~60% 成本。

### 与现有架构的兼容性

| 现有功能 | 影响 | 兼容方案 |
|---------|------|---------|
| 任务委派（subtasks） | 无影响 | discussion 和 subtasks 互斥，leader 二选一 |
| Handoff（Transfer/Delegate） | 无影响 | discussion 优先于 handoff 检查 |
| Yanluo 审计 | 兼容 | 讨论消息也走审计，`MessageType::Discussion` 标记 |
| Rank 积分 | 兼容 | 讨论发言 +2 分/条（鼓励参与），PASS 不加分 |
| IdleChat | 无影响 | 讨论期间暂停 idle chat |
| 前端三种风格 | 兼容 | 讨论消息用同样的 `MessageSaved` 机制持久化，风格渲染自动适配 |

### 代价

- **每次讨论 25 次 LLM 调用 / ~50K tokens / 60-200 秒**。相比任务委派（一次派发 + 一次 review）成本高一个量级，只有在"需要多轮才能收敛的开放话题"上才划算。
- **LLM 产出的裁决 JSON 是新的解析失败面**：`parse_moderation_decision` 必须容忍 fence、前后文字、未知 action，任一解析失败即整场讨论失败（`Err` 上抛 → `SoftError`）。
- **新增 `TaskPhase::Discussing` 与 4 个 `RoomEvent` 变体**，前端 WS、持久化、审计三条链路都要跟着长。
- **F3 的滑动窗口与"按需引用原文"存在张力**：引用原文会把已归档内容重新拉回上下文，滑动窗口的压缩收益被部分抵消。

### 结局：随群聊模块整体下线

2026-09-21 `refactor(groupchat): 删除群聊模块 —— 前后端全部群聊能力下线`，讨论模式与之同批删除：`hermix-gateway/src/groupchat`（11 文件，含 `discussion.rs` / `discussion_prompt.rs`）、transport 的 `GroupChat*`/`Discussion*` 变体与类型、前端 `GroupChatView`/`DiscussionPanel`/`TaskPanel` 等组件与 api 方法。2026-09-24 `hermix-gateway` crate 本身随 `hermix-acp` 一并删除（-30,419 行）。

**因此本文中的 `DiscussionConfig` / `DiscussionState` / `DiscussionEngine` / `TaskPhase::Discussing` 在当前代码库中均不存在**（`DiscussionEngine` 全仓零匹配）。保留本笔记是因为"讨论必须由外部编排引擎模拟、且必须串行"这条推导，以及 F1-F10 的失败模式枚举，在任何多 agent 协作模式重做时仍然成立。

## 附录 A：LeaderDecision prompt 扩展（完整版）

```
你是群聊 "{room_name}" 的项目总管...

## 你的任务
分析用户的需求，决定如何分配任务给团队成员。

你可以选择以下三种模式之一：

### 模式 1：直接回答
当你自己就能回答用户时，subtasks 为空数组，discussion 为 null。

### 模式 2：任务委派
当任务可以拆解为独立子任务时，填写 subtasks。

### 模式 3：团队讨论
当你认为需要团队成员多轮讨论才能得出结论时，填写 discussion：
{
  "topic": "讨论话题",
  "max_rounds": 5,
  "participants": ["agent_id1", "agent_id2"]
}

讨论模式和子任务模式互斥：有讨论就不派发子任务。

严格按以下 JSON 格式回复：
{
  "analysis": "...",
  "subtasks": [],
  "announcement": null,
  "needs_user_confirmation": false,
  "handoff": null,
  "discussion": null
}
```
