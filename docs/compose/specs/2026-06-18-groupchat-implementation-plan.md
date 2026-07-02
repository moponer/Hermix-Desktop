# GroupChat Implementation Plan — Full Stack

## Phase 1: Backend Foundation (T26-T30) ✅ DONE

## Phase 2: Agent Integration

### T36: PromptBuilder — 分层提示词组装
- Role template loading from files
- Token budget management
- Context truncation
- Room rules injection
- Governance rules injection

### T37: AgentLoop Integration in GroupChatManager
- Create AgentLoop per agent per room
- Route messages to appropriate agents
- Build room context for system prompts
- Save agent responses to room

### T38: RuleEngine — 规则执行与扣分
- Violation detection (tool usage check)
- Score management (10-point system)
- Reflection trigger (score < 5)
- Agent rebuild on firing

### T39: RankManager — 段位与勋章
- Score accumulation
- Rank tier calculation
- Badge assignment
- Evaluation recording

## Phase 3: Idle Chat & Memory

### T40: IdleChatManager — 闲聊模式
- Timer-based idle chat trigger
- Agent selection for idle messages
- Memory write on idle chat

### T41: Memory Isolation
- Per-agent session management
- Room context injection
- Leader-specific memory

## Phase 4: Frontend

### T42: TaskProgressBar + ApprovalCard
### T43: AgentStatusBadge + RankDisplay
### T44: RoomSettingsDrawer (rules config)
