# GroupChat Collaborative Agent System — Complete Design Spec

## [S1] Problem

当前 `GroupChatView` 是纯前端外壳，agent 回复是硬编码模拟。需要实现完整的多 agent 协作聊天室系统，包含总管调度、角色权限、记忆隔离、闲聊学习、晋级体系等全套功能。

## [S2] 架构总览

```
┌─────────────────────────────────────────────────────┐
│  Frontend (React)                                   │
│  GroupChatView + TaskProgressBar + ApprovalCard     │
├─────────────────────────────────────────────────────┤
│  HTTP API + SSE Streaming                           │
├─────────────────────────────────────────────────────┤
│  GroupChatManager (hermix-gateway)                  │
│  ├── PromptBuilder (分层组装 system prompt)          │
│  ├── LeaderOrchestrator (总管调度)                   │
│  ├── RuleEngine (规则执行 + 扣分)                    │
│  ├── RankManager (晋级 + 勋章)                      │
│  └── IdleChatManager (闲聊模式)                      │
├─────────────────────────────────────────────────────┤
│  AgentLoop × N (per agent, per room)                │
│  ├── 独立 session_id (记忆隔离)                      │
│  ├── 角色化 system prompt                           │
│  └── ToolRegistry (共享)                             │
├─────────────────────────────────────────────────────┤
│  Gateway Platform Adapters (已有)                    │
│  微信 / 钉钉 / Telegram / ...                       │
└─────────────────────────────────────────────────────┘
```

## [S3] 核心组件

### [S3.1] GroupChatRoom

```rust
pub struct GroupChatRoom {
    pub id: String,
    pub name: String,
    pub leader_id: Option<String>,
    pub agent_ids: Vec<String>,
    pub settings: RoomSettings,
    pub rules: RoomRules,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct RoomSettings {
    pub allow_idle_chat: bool,
    pub idle_interval_secs: u64,         // default 300
    pub max_concurrent_agents: u32,      // default 5
    pub require_leader_approval: bool,   // default true
    pub token_budget: usize,             // default 8000
}

pub struct RoomRules {
    pub global_constraints: Vec<String>,
    pub role_permissions: HashMap<String, RolePermissions>,
    pub agent_overrides: HashMap<String, Vec<String>>,
    pub penalty_policy: PenaltyPolicy,
}
```

### [S3.2] AgentRole & AgentRoomProfile

```rust
pub enum AgentRole {
    Leader,   // 总管
    Member,   // 成员
    Observer, // 观察者
}

pub struct AgentRoomProfile {
    pub agent_id: String,
    pub room_id: String,
    pub role: AgentRole,
    pub session_id: String,  // 隔离 session
}
```

### [S3.3] Leader（内置总管）

内置的、固定行为模式的全能决策助手：

**工作流程**：
1. 接收用户消息 → 分析目标和目的
2. 细化需求 → 分配给策划 agent 讨论
3. 收集建议 → 汇总分析 → 生成可行方案
4. 交给用户确认
5. 用户确认后 → 派送子任务（决定并行/串行/依赖关系）
6. 监控进度、中断/重新安排
7. 反思性批判性验收
8. 验收通过 → 交给用户；不通过 → 打回或换人

**治理职责**：
- 监控 agent 行为是否符合角色权限
- 检测违规 → 制止 + 扣分
- 积分 < 5 → 要求反思 + 考核
- 考核不通过 → 解雇 + 自动重建替代 agent
- 通知用户

### [S3.4] RolePermissions（角色权限）

```rust
pub struct RolePermissions {
    pub allowed_tools: Option<Vec<String>>,
    pub denied_tools: Option<Vec<String>>,
    pub can_delegate: bool,
    pub can_write_memory: bool,
    pub constraints: String,  // 注入 system prompt
}
```

**内置角色规则**：
| 角色 | 禁止工具 | 约束 |
|------|---------|------|
| 总管 | terminal, file_write, code_execution | 只能委派，不能直接执行 |
| 产品经理 | terminal, file_write, code_execution | 只做需求分析，不写代码 |
| 架构师 | terminal, file_write, code_execution, messaging | 只做技术设计 |
| 写手 | terminal, code_execution, delegation | 只写内容，不委派 |

## [S4] Agent 治理系统

### [S4.1] 扣分制

```rust
pub struct AgentBehavior {
    pub agent_id: String,
    pub room_id: String,
    pub score: i32,              // 满分 10
    pub violations: Vec<Violation>,
    pub reflections: Vec<String>,
    pub is_active: bool,
    pub fired_at: Option<DateTime<Utc>>,
    pub fired_reason: Option<String>,
}

pub struct Violation {
    pub rule: String,
    pub severity: ViolationSeverity,
    pub deduction: i32,
    pub detected_at: DateTime<Utc>,
    pub context: String,
}

pub enum ViolationSeverity {
    Warning,    // -1
    Minor,      // -2
    Major,      // -3
    Critical,   // -5
}
```

### [S4.2] 惩处流程

```
违规检测 → 制止 + 扣分
  ↓
积分 < 5？
  ├── 否 → 继续
  └── 是 → 反思流程
      ↓
      总管命令反思 → agent 明确规则理解
      ↓
      总管考核
      ├── 通过 → 恢复积分
      └── 不通过 → 解雇 + 销毁
          ↓
          总管自动重建 agent
          - 保持原角色
          - 注入违规历史
          - 强化规则
          ↓
          通知用户
```

### [S4.3] PenaltyPolicy

```rust
pub struct PenaltyPolicy {
    pub initial_score: i32,              // 10
    pub reflection_threshold: i32,       // 5
    pub can_fire: bool,                  // true
    pub auto_rebuild: bool,              // true
    pub rebuild_preserves_role: bool,    // true
    pub rebuild_injects_violation_history: bool, // true
}
```

## [S5] Agent 晋级与勋章系统

### [S5.1] 段位体系

| 段位 | 积分 | 勋章 | 晋级条件 |
|------|------|------|---------|
| 青铜 | 0-99 | 🥉 | 初始 |
| 白银 | 100-299 | 🥈 | 5 任务 |
| 黄金 | 300-599 | 🥇 | 15 任务 + 3 次无违规 |
| 铂金 | 600-999 | 💎 | 30 任务 + 总管评价 ≥ 4 |
| 钻石 | 1000-1499 | 🔷 | 50 任务 + 10 次连续无违规 |
| 星耀 | 1500-2499 | ⭐ | 80 任务 + 3 次铂金评价 |
| 王者 | 2500+ | 👑 | 120 任务 + 20 次连续无违规 |
| 王者·传奇 | 5000+ | 👑✨ | 所有王者条件 + 总管推荐 |

### [S5.2] 积分规则

```
任务完成:     +10
优质完成:     +5（总管评价 ≥ 4/5）
超额完成:     +3
帮助队友:     +3
创新贡献:     +5
连续无违规:   +1/次（连续 5 次额外 +2）

违规扣分:
  提醒:       -1
  轻微:       -2
  严重:       -3
  致命:       -5

反思通过:     +2
被解雇:       归零
```

### [S5.3] 数据模型

```rust
pub struct AgentRank {
    pub agent_id: String,
    pub room_id: String,
    pub score: i32,
    pub rank: RankTier,
    pub badges: Vec<String>,
    pub total_tasks: u32,
    pub consecutive_no_violation: u32,
    pub evaluations: Vec<Evaluation>,
}

pub struct Evaluation {
    pub evaluator: String,
    pub score: i32,
    pub comment: String,
    pub created_at: DateTime<Utc>,
}
```

## [S6] 记忆隔离与共享

- 每个 agent 独立 session_id：`groupchat-{room_id}-{agent_id}`
- 记忆写入自动隔离到各自 session
- 房间共享上下文：最近消息注入 system prompt
- 总管额外存储：任务历史、治理记录

## [S7] 提示词管理

### [S7.1] 分层组装

```
PromptBuilder::build(token_budget=8000):
├── 层1: 角色定义（固定模板，~500 tokens）
├── 层2: 治理规则（固定，~400 tokens）
├── 层3: 房间规则（动态，~300 tokens）
├── 层4: 约束文本（按需，~200 tokens）
├── 层5: 任务上下文（动态，~500 tokens）
└── 层6: 对话历史（截断，~6100 tokens）
```

### [S7.2] Token 预算

默认 8000 tokens，预留 4000 给模型输出。对话历史动态截断。

### [S7.3] 模板管理

存储在 `$HERMIX_HOME/groupchat-templates/`，用户可自定义。

## [S8] 闲聊与学习模式

- `allow_idle_chat = true` → tokio 定时器
- 选 1-2 个 agent（基于角色相关性）
- 生成闲聊/学习消息
- 存入各自记忆
- UI 显示 "闲聊" 徽章

## [S9] 外部平台投递

复用 Gateway 已有适配器，不新建 WebSocket。

## [S10] API 设计

| Method | Path | Description |
|--------|------|-------------|
| POST | `/api/groupchat/rooms` | 创建房间 |
| GET | `/api/groupchat/rooms` | 列出房间 |
| DELETE | `/api/groupchat/rooms/:id` | 删除房间 |
| PUT | `/api/groupchat/rooms/:id/agents` | 更新 agent |
| PUT | `/api/groupchat/rooms/:id/leader` | 设置总管 |
| PUT | `/api/groupchat/rooms/:id/settings` | 更新设置 |
| PUT | `/api/groupchat/rooms/:id/rules` | 更新规则 |
| POST | `/api/groupchat/rooms/:id/message` | 发消息（SSE） |
| GET | `/api/groupchat/rooms/:id/messages` | 获取历史 |
| POST | `/api/groupchat/rooms/:id/approve` | 批准方案 |
| POST | `/api/groupchat/rooms/:id/reject` | 驳回 |
| GET | `/api/groupchat/rooms/:id/ranks` | 查看段位 |
| GET | `/api/groupchat/rooms/:id/behavior/:agent_id` | 查看行为记录 |

## [S11] 数据模型（SQLite）

- `groupchat_rooms` — 房间 + 规则
- `groupchat_room_agents` — agent 名单 + 角色 + session
- `groupchat_messages` — 消息历史
- `groupchat_tasks` — 任务记录
- `groupchat_subtasks` — 子任务
- `groupchat_behavior` — 行为记录 + 积分
- `groupchat_ranks` — 段位 + 勋章
- `groupchat_evaluations` — 总管评价

## [S12] 前端变更

- GroupChatView 接入真实 API
- TaskProgressBar — 子任务进度
- ApprovalCard — 总管方案审批
- AgentStatusBadge — 在线/忙碌 + 段位徽章
- RoomSettingsDrawer — 设置（总管、闲聊、规则）
- RankDisplay — 段位展示

## [S13] 设计决策

| 决策 | 理由 |
|------|------|
| 总管内置固定角色 | 任务流程固化，防跑偏 |
| 每 agent 独立 AgentLoop | 天然记忆隔离 |
| 复用 SubAgentOrchestrator | 子任务执行已有 |
| 复用 Gateway 适配器 | 不新建 WebSocket |
| SSE 流式推送 | 复用现有模式 |
| system prompt 注入约束 | 比代码拦截更灵活 |
| 段位游戏化 | 提升 agent 协作动力 |
| 提示词分层组装 | 控制 token 预算 |
| 无破坏性变更 | 增量添加 |
