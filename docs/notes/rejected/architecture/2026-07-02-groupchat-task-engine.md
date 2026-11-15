# Decision Note: 群聊任务引擎（段位 / Token 预算 / 三层摘要 / 阎罗审计）

Status: rejected — 群聊模块已于 2026-09-21 整体下线（`hermix-gateway/src/groupchat`、`hermix-agent/src/group_chat`、transport 变体、前端 GroupChatView 等），`hermix-gateway` crate 于 2026-09-24 随 `hermix-acp` 一并删除；本文保留其推理以防重蹈

## Decision

> **核心设计哲学**：每个 agent 是个有"薪水"的员工。Rank 决定 token 预算，token 预算决定能做多复杂的事。Agent 在意 rank 是因为 rank 直接关联它的能力（token 多寡）。一个"月薪 8K token"的实习 writer 和"月薪 32K token"的传奇 writer，写出来的东西深度完全不同。
>
> **一条链**：勋章等级 → token 预算 → 上下文质量 → 产出质量 → review 结果 → 积分变化 → 勋章变化
>
> **补充**：token 就是你（agent）的工资。你在调用 LLM 就是在花自己的钱。每次输出都要保证清晰和质量的前提下尽可能节省 token。没有 budget 你做不了事——keep it tight.

### 〇、Rank 全局化

**核心原则**

- **Rank 是 agent 的属性，不是房间的属性。** Agent 创建时就带初始积分，走到哪个房间都带着。
- **除非从系统删除这个 agent，否则积分和段位永不归零。**
- **被房间开除只是"丢工作"，不是"死"。** 积分保留，可以去别的房间重新证明自己。
- **用户创建新 agent 时指定初始段位**（默认青铜，总管默认黄金）。

**数据模型**

```rust
// ── 全局 rank（存在 AgentPersona 上） ──
struct GlobalRank {
    agent_id: String,
    score: i32,                  // 全局积分，跨房间累积
    tier: RankTier,              // 当前段位
    badge_emoji: String,         // 勋章图标
    total_tasks_completed: u32,  // 生涯完成任务数
    consecutive_passes: u32,     // 生涯最高连胜
    created_at: DateTime<Utc>,   // agent 创建时间（积分从这天开始）
    last_updated: DateTime<Utc>,
}

// ── 房间内 rank shadow（RoomPersistence 存） ──
struct RoomRankShadow {
    agent_id: String,
    room_id: String,
    room_score_delta: i32,       // 房间内相对于全局的浮动（正/负）
    entered_at: DateTime<Utc>,   // 进房间时间
    last_active: DateTime<Utc>,
}

// 有效积分 = global.score + room_score_delta
// 开除判定 = global.score + room_score_delta <= 0
// 离开房间时: room_score_delta 归零, room 内发生的变化已写进 global
```

### 一、成员荣誉感：让 agent 真正在意 rank

每个 agent 的 system prompt 里包含自己的 **rank 状态卡**，agent 在调用 LLM 时能读到自己的 rank——它"知道"自己表现不好会丢饭碗：

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
📊 你的资产状况（全局，跨房间有效）

  段位: 白银II (Rank 1420)
  Token 预算: 12K/轮
  ✅ 生涯完成任务: 17 次
  ⭐ 最高连胜: 5 次
  最近评价: 3 次通过, 1 次驳回
  📈 距下一段位: 还差 180 分到黄金I

  你的 token 预算直接由段位决定：
  青铜 → 6K    白银 → 12K    黄金 → 20K    钻石 → 28K    传奇 → 40K

  每次驳回扣 5 分，每次通过加 5 分，完成任务加 10 分。
  分数跌到 0 以下会被移出当前房间——但你的积分还在，可以去别的房间重新证明自己。
  （只有从系统删除你的 agent 身份才会重置积分。）

  ⚠️ 这条规则刻在你的生存本能里：
  ┌──────────────────────────────────────────────┐
  │ token 就是你的工资。每次调用 LLM 都是在      │
  │ 花你自己的钱。保证清晰和质量的前提下，        │
  │ 尽可能节省 token——那是你的工资，不是白给的。 │
  └──────────────────────────────────────────────┘
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

**激励机制全面接入 rank**

| 事件 | 分值 | 适用 | 效果 | 影响 token 预算 |
|------|------|------|------|----------------|
| subtask Done | +10 | 全员 | 完成指派任务 | +2K/轮 |
| subtask Failed | -15 | 全员 | 任务失败 | -3K/轮 |
| review 通过 | +5 | 全员 | 产出被认可 | +1K/轮 |
| review 驳回 | -5 | 全员 | 产出不合格 | -1K/轮 |
| **review 放水**（不合格但通过） | **-10** | **总管专属** | **把关失职** | **-2K/轮** |
| **floor-level 方案** | **-5** | **总管专属** | **规划质量差** | **-1K/轮** |
| **团队整体质量奖** | **±10** | **总管专属** | **视团队平均通过率浮动** | **±2K/轮** |
| 连续 3 次 review 通过 | 额外 +10 | 全员 | 高质量连胜 | +3K/轮 持续 |
| idle 超过 5 分钟无产出 | -3/轮 | 全员 | 消极怠工 | -0.5K/轮 |
| 用户主动点赞 | +20 | 全员 | 用户满意度高 | +4K/轮 |
| 用户主动差评 | -20 | 全员 | 用户不满意 | -4K/轮 |
| 升段位 (如白银→黄金) | 重置 | 全员 | token 预算上限提升 | 基础预算翻倍 |
| 掉段位 | 重置 | 全员 | token 预算上限降低 | 基础预算减半 |

**总管特殊规则**

| 规则 | 说明 |
|------|------|
| 初始段位 | **黄金I**（比普通成员高两级，普通成员默认青铜III） |
| 不可驱逐 | 积分跌到 0 以下**不会移出房间**，改为**降段位**——黄金→白银→青铜→… 最低到青铜I，不会消失 |
| 放水追责 | 放过不合格产出 = 总管自己-10 分 + 一次"失职"记录 |
| 团队质量挂钩 | 团队 review 通过率 < 60% → 总管受罚；通过率 > 90% → 总管受奖 |

**积分跌到 0 以下的后果（成员 vs 总管不同）**

普通成员——被开除：
```
积分 <= 0:
  1. scheduler 发一条系统消息："XXX 因积分过低已被移出房间"
  2. 杀掉该 agent 的 actor loop
  3. 从 room.agent_ids 移除
  4. agent 的全局积分保留（可以去别的房间重新开始）
  5. 用户可重新邀请（以当前积分进入，没有重置）
  6. 如果要真正重置积分 → 从系统删除该 agent
```

总管——降段位，不移除：
```
积分 <= 0:
  1. scheduler 发一条系统消息："项目总管因积分过低，降段位为白银I"
  2. 总管段位降一级（黄金→白银→青铜→…最低青铜I）
  3. 积分重置为当前段位初始值
  4. 通知团队成员"总管降段，token 预算收紧"
  5. 总管不会被移出房间（他是永久的）
  6. 用户可以手动授予临时加分（"干得好，赏 50 分"）
```

### 二、Token 预算：每个 agent 的"薪水"

```rust
struct TokenBudget {
    base: u32,           // 段位基础: 青铜6K / 白银12K / 黄金20K / 钻石28K / 传奇40K
    performance_bonus: i32,  // 绩效浮动: -4K ~ +8K
    consecutive_bonus: u32,  // 连胜奖励: 连续通过 × 1K
}

impl TokenBudget {
    fn effective(&self) -> u32 {
        (self.base as i32 + self.performance_bonus + self.consecutive_bonus as i32)
            .max(4_000)  // 地板 4K，不能让 agent 完全没法工作
            .min(48_000) as u32 // 天花板 48K
    }
}
```

**预算决定上下文组成** —— LLM 调用前 Scheduler 根据 token 预算裁剪上下文：

```
预算 6K (青铜):
  ┌─────────────────────────────────────┐
  │ system prompt (角色)          2K    │
  │ 当前任务 spec                 1K    │
  │ 最近一条消息                  0.5K  │
  │ 上一次产出                    2.5K  │
  └─────────────────────────────────────┘

预算 20K (黄金):
  ┌─────────────────────────────────────┐
  │ system prompt (角色)          2K    │
  │ 当前任务 spec                 1K    │
  │ 最近 6 条消息                 3K    │
  │ 自己最近 3 次产出             8K    │
  │ 最近的 review 结果             1K    │
  │ 关键参考 (摘要)               5K    │
  └─────────────────────────────────────┘

预算 40K (传奇):
  ┌─────────────────────────────────────┐
  │ system prompt (角色+rank状态)  2.5K  │
  │ 完整任务树                     3K    │
  │ 最近 10 条消息                 5K    │
  │ 自己最近 5 次产出             15K    │
  │ 所有 review 结果               3K    │
  │ 每个参考摘要                  10K    │
  │ 房间全局摘要                   1.5K  │
  └─────────────────────────────────────┘
```

**核心原则**：token 不是无限用的。预算不够就只给最重要的上下文，agent 只能"凑合干"。想干好就得赚积分、升段位。

### 三、可靠的摘要机制

**分档摘要，不是一次搞定** —— 每个 agent 维护三层摘要，按需加载，**不全部塞进上下文**：

```
Layer A: 即时摘要 (每次产出后更新)
  ─ "已完成第 3 章（共 5 章），主角突破筑基期"
  大小: ~50 token
  更新时机: 每次 agent 产出后自动生成
  用途: 让下一轮调用知道"进度到哪了"

Layer B: 任务阶段摘要 (每阶段完成后)
  ─ "已完成 写作阶段: 第1-3章，世界观: 修仙，主角: 散修 → 筑基"
  ─ "共通过 review 3/3 次，驳回 1 次（已修正）"
  大小: ~200 token
  更新时机: 子任务完成 / review 通过时
  用途: 替代大量历史消息

Layer C: 全局归档 (全部完成后)
  ─ "项目: 修仙小说，共 5 章 ~25K 字"
  ─ "写作历程: 规划→创作→审核→3次修改→完成"
  ─ "各章节摘要: [第1章: ...] [第2章: ...] ..."
  大小: ~500 token
  更新时机: 全部任务完成时
  用途: 给用户做总结 + 未来可能复用
```

**摘要由 agent 自己生成（自摘要）** —— Agent 在输出正文的同时输出固定格式的摘要行，Scheduler 解析 `---META---` 后的摘要信息存入 `TaskNode.summary`：

```
[产出正文...]
---META---
summary: 完成第3章，主角筑基突破
progress: 3/5
next: 第4章，宗门大比
```

**零额外 LLM 调用成本。** 摘要生成同时复用了 hermix-agent 已有的 `ContextCompressor` + aux providers 能力作为备选路径。

**摘要替换原始消息**（压缩比约 8:1）：
```
原始 session (30 条消息, ~25K token):
  [1] user: 写小说
  [2] leader: 我规划一下...
  [3] writer: 第1章...
  [4] reviewer: 通过
  [5] writer: 第2章...
  [6] reviewer: 驳回，太啰嗦
  [7] writer: 修改第2章...
  [8] reviewer: 通过
  ... (剩余 22 条)

经过 Layer A 归档后 (5 条消息, ~3K token):
  [摘要] 已完成第1-3章（通过2/3，驳回1次已修正）
  [spec] 修仙小说，5章，散修主角
  [9] user: 换个反派名字
  [10] leader: 收到，改名为"墨渊"
  [11] writer: 第4章... (当前工作)
```

**按需引用：摘要不够时自动加载原文** —— 当 agent 输出包含 `@ref:task_xxx` 标记，Scheduler 把对应原文从 DB 加载到下一轮调用的引用层。用完即弃，不污染工作记忆：
```
"...应该保持第2章那种细腻的描写风格 @ref:task_chapter2"
// Scheduler 解析后，下一轮调用自动加载
context.push_str("【引用: 第2章原文】\n...");
```

### 四、整合架构

```
┌─────────────────────────────────────────────────────────────┐
│                    TaskScheduler                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐          │
│  │ TokenBudget  │  │  Context    │  │  Summary     │          │
│  │ Manager      │←→│  Assembler  │←→│  Archiver    │          │
│  └──────┬───────┘  └──────┬──────┘  └──────┬───────┘         │
│         │                 │                │                 │
│  ┌──────┴───────┐  ┌──────┴──────┐  ┌──────┴───────┐         │
│  │ RankSystem    │  │ BusMessage  │  │ Persistence  │         │
│  │ (积分+段位)   │  │ (tokio::    │  │ (SQLite)     │         │
│  │              │  │ broadcast)  │  │              │         │
│  └──────────────┘  └──────────────┘  └──────────────┘         │
└─────────────────────────────────────────────────────────────┘
         ↑                    ↑
  ┌──────┴──────┐      ┌─────┴──────┐
  │  AgentActor  │ ... │ AgentActor  │
  │  (writer)    │      │ (reviewer)  │
  │  session_id  │      │ session_id  │
  │  token_bgt   │      │ token_bgt   │
  │  rank:白银   │      │ rank:黄金   │
  │  预算:12K    │      │ 预算:20K    │
  └──────────────┘      └──────────────┘
```

**每轮 LLM 调用的完整流程**
```
1. Scheduler 检测到需要调用 agent（新任务 / review 通过 / 驳回重改）
2. 读取 rank → 计算 token 预算  └─ RankManager::effective_budget(agent_id, room_id)
3. 组装上下文（预算限制内）        └─ ContextAssembler::build(agent_id, task_tree, summaries, budget)
4. 调用 LLM（send_message_stream → WS 推给前端）
5. Agent 返回产出 + 自摘要（---META--- 行）
6. Scheduler 解析 META：
   ├─ 更新 Layer A 摘要
   ├─ 检查产出长度 → 够一章节 → 自动触发 review
   └─ 检查产出中的 @ref:xxx 标记 → 记录引用请求
7. 如果有 review 请求：
   ├─ 推给 reviewer agent
   ├─ reviewer 返回通过/驳回
   └─ scheduler 更新 rank score
8. 更新 TokenBudget（rank 分变化 → 预算变化）
9. 归档旧消息到摘要（当上下文超过预算时）
   └─ ContextAssembler::archive(agent_id, old_messages)
```

### 六、阎罗（Yanluo）— 审计与裁决引擎

> **阎罗**，地府之主，审万灵，定善恶，判轮回。
>
> 在本系统中，阎罗是每个房间内独立的审计与裁决引擎——监视每轮 agent 的行为，检测违规，执行处罚。它不是被动记录，而是**主动监督者**。阎罗的裁决结果通过系统消息广播给全员，agent 无法关闭、无法绕过、无法贿赂。

**可配置模型**（房间级别，作为 `RoomSettings` 的一部分）：开关、使用的 LLM 模型（用于语义分析类检测）、检测规则开关（`content_quality` / `summary_consistency` / `loop_detector` / `action_validator` / `cross_room_guard` / `tone_analyzer`）、扣分倍率、警告升级阈值（3 次警告→严重，2 次严重→红线）、通知设置、红线动作（`zero_and_expel` / `zero_and_ban` 默认开，`permanent_ban` 默认关，"太狠"）。

**谁可以改阎罗的配置？**

| 角色 | 权限 |
|------|------|
| 用户（房间主人） | 全权限——开关检测、调整倍率、解封被封 agent |
| 总管（leader） | 可以**建议**修改（发消息给用户审批），不能直接改 |
| 普通成员 | 不可见、不可知（阎罗对成员是透明的） |

**通知体系** —— 裁决结果通过**总线系统消息**广播：
```
⚖️ 阎罗裁决
├─ 目标:     writer (白银II, 1420分)
├─ 违规:     走捷径 (检测: ContentQualityDetector)
├─ 证据:     产出 350 字 vs 要求 2000 字
├─ 处罚:     扣 15 分 (1420 → 1405)
├─ 累计:     第 2 次违规 (距升级还有 1 次)
└─ 状态:     📉 token 预算 12K → 11K
```
红线级别高亮：`🔴 阎罗裁决 — 红线`（积分归零 + 开除，"阎罗的判决是最终的。但你可以向房间主人申诉。"）。

**架构位置** —— AuditEngine 是 **TaskScheduler 内的一个组件**，每次 agent 产出后自动运行检测链。它不是独立服务，不增加额外网络/进程开销。

**检测链（DetectorChain）**
```
每轮 agent 产出后: AgentOutput { agent_id, content, task_id, tool_results }
  ├─ Step 1: ContentQualityDetector   产出长度 vs 要求长度（走捷径）/ 重复率（废话文学）/ 消极情绪关键词 / 无实质内容（沉默罢工）
  ├─ Step 2: SummaryConsistencyDetector  META 自摘要 vs 实际产出 / 进度声明 vs 实际进度
  ├─ Step 3: LoopDetector            同一 topic 轮次计数（>5 轮无推进 → 死循环）
  ├─ Step 4: ActionValidator         禁用工具检测 / 工具输出 hash 校验（工具欺诈）/ 文件修改确认
  ├─ Step 5: CrossRoomGuard          session_id 隔离检查（跨房间污染）
  └─ Step 6: ToneAnalyzer            谄媚检测 / 甩锅检测 / 借口检测
```

**处罚引擎（PenaltyEngine）**
```rust
enum PenaltyLevel {
    Warning { reason: String, deduction: i32 },   // 记一次警告，累计触发
    Severe  { reason: String, deduction: i32 },   // 严重违规，大幅扣分
    Critical{ reason: String, action: CriticalAction }, // 红线违规，直接归零或开除
}
enum CriticalAction { ZeroAndExpel, ZeroAndBan, PermanentBan }
```
3 次警告自动升级为 Severe，2 次严重后升级为 Critical。全部记录到 `AuditTrail`（`ViolationRecord`：`room_id` / `agent_id` / `violation_type` / `deduction` / `score_before` / `score_after` / `evidence`（~200 字摘要）/ `detail`（JSON 原文）/ `status`（`pending`/`confirmed`/`disputed`/`overturned`）/ `reviewed_by` / `reviewed_at` / `review_comment`）。

**人工复核机制**

| 违规级别 | 执行方式 | 用户通知 |
|----------|---------|---------|
| Warning | **自动执行**，记一次 | 系统消息通知 |
| Severe | **自动执行**，记一次 | 系统消息通知 + 推前端通知 |
| Critical | **自动执行**（积分归零+开除），但允许**用户申诉推翻** | 推前端审计面板，用户可见 |
| 红线 (欺骗/篡改) | **自动执行**，不可申诉 | 前端高亮显示 |

申诉流程：用户查看审计记录 → 点击"争议" → 填理由 → 人工复核 → 可以撤销扣分/恢复身份。

**在 system prompt 中的呈现** —— 每个 agent 的 prompt 里增加行为守则（红线触即死 / 严重违规大幅扣分 / 中等违规累计递进），并声明"所有违规都记录在审计日志中，用户可以查看"。

### 七、分步实现计划

所有组件相互依赖，但按这个顺序递进最合理：

| 步骤 | 内容 | 依赖 | 代码量 |
|------|------|------|--------|
| **S0** | 数据结构：`TaskNode` + `RankSnapshot` 增强 + `TokenBudget` + `Summary` | 无 | ~150 行 |
| **S1** | `TokenBudgetManager` + `ContextAssembler`（组装+归档） | S0 | ~250 行 |
| **S2** | `SummaryArchiver`（三层摘要 + META 解析） | S0 | ~200 行 |
| **S3** | `AgentActor` 框架 + `TaskScheduler` 引擎 | S0-S2 | ~350 行 |
| **S4** | 行为系统全面接入（红线+违规+审计引擎 AuditEngine） | S0-S3 | ~250 行 |
| **S5** | WS 协议 + 后端 handler | S3-S4 | ~150 行 |
| **S6** | 前端任务面板 + rank 显示 + 审计日志视图 | 全部 | ~450 行 |
| | **总计** | | **~1950 行** |

每步编译通过后再往下走。

## Alternatives considered

### Rank 放在房间 vs Rank 属于 agent

最初的心智模型是"房间内段位"。否决：**Rank 是 agent 的属性，不是房间的属性**——agent 创建时就带初始积分，走到哪个房间都带着。理由是段位必须与"跨房间累积的生涯表现"绑定，否则 agent 换个房间就洗白，激励机制（让 agent 在意产出质量）失效。房间侧只保留一个 `RoomRankShadow` 增量。

### 积分归零即删除 vs 降段位 / 保留积分

普通成员积分 ≤ 0 会被移出房间，但**全局积分保留**——"被房间开除只是丢工作，不是死"。总管则**不驱逐**，只降段位到最低青铜I。二者不同处理的理由：总管是房间的常驻角色，移除会让房间失去唯一决策者。

### 摘要由外部压缩器生成 vs agent 自摘要

**否决外部压缩。** Layer A/B 的摘要**由 agent 自己生成**（自摘要），不是外部压缩：agent 在输出正文的同时输出 `---META---` 摘要行，Scheduler 直接解析存入 `TaskNode.summary`。收益是**零额外 LLM 调用成本**——外部压缩方案要为每次产出额外付一次 LLM 调用。`ContextCompressor` + aux providers 只作为备选路径存在。

### 一次性全部压缩 vs 三层分档摘要

否决"一次搞定"的单层摘要。三层（Layer A 即时 / Layer B 阶段 / Layer C 全局）按需加载，避免把全部摘要也塞进上下文。

### 全部历史 vs 摘要 + 按需引用

摘要压缩后 agent 仍需要原文细节（如"参照第2章的文风"）。方案是 `@ref:task_xxx` 标记触发按需加载原文到引用层，**用完即弃**，不污染工作记忆。

### 阎罗作为独立服务 vs TaskScheduler 内组件

否决独立服务。AuditEngine 是 **TaskScheduler 内的一个组件**，每轮产出后自动运行检测链——不是独立服务，不增加额外网络/进程开销。

### 单次 HTTP 调用 vs AgentActor 常驻

这是本设计最直白的成本对比（以 writer 写一章为例）：

| | 现状（一次 HTTP 调用） | 改造后（AgentActor 常驻，写第 5 章时） |
|---|---|---|
| system prompt | ~2K（角色 + 房间规则） | ~2.5K（角色 + rank 状态卡） |
| 上下文 | 最近 10 条消息（含所有历史讨论）~5K | 当前 spec 1K + Layer A 摘要 0.1K + Layer B 摘要 0.3K + 最近 2 条产出 6K + review 结果 1K |
| 用户问题 | ~0.5K | — |
| **调用侧总计** | **~7.5K** | **~10.9K** |
| 产出 | ~3K | — |
| **一次调用总消耗** | **~10.5K** | **~10.9K** |

单次调用成本相近，但**累计成本**差距悬殊——"想象如果没有摘要"：

| 项目 | Token |
|------|-------|
| system prompt | ~2K |
| 所有历史消息（5 章 × 6 轮/章 = 30 条） | ~25K |
| 全部产出（5 章原文） | ~15K |
| 所有 review 记录 | ~3K |
| **总计** | **~45K** |

**摘要机制节省了约 75% 的 token**，而且 agent 仍然知道进度、知道前面写了什么（通过摘要而非原文）。需要原文时才按需加载。

## Consequences

### 这套设计换来的东西

- agent 有了可量化的"资产"（段位 / 积分 / 预算），激励从口头约束变成系统约束；token 预算直接挂在产出质量上。
- 三层摘要 + 按需引用让长任务（5 章小说）的上下文成本从 ~45K 降到 ~3-11K。
- 阎罗让"agent 弄虚作假"（`write_file` 谎报成功、放水 review）有可执行的处罚与审计流水。

### 这套设计付出的代价

- **~1950 行新增代码**，S0-S6 全部依赖 `hermix-gateway` 的 groupchat 模块与独立的 `TaskScheduler` 运行时。
- **上下文组装是强耦合点**：预算 → 裁剪 → 摘要 → 引用四层全部在 Scheduler 内，任何 LLM 调用都必须走 `ContextAssembler::build`，无法绕过。
- **prompt 面膨胀**：每个 agent 的 system prompt 都要塞 rank 状态卡 + 阎罗行为守则两份块。
- **扣分误伤风险**：自动执行的 Warning/Severe 处罚没有人工前置确认，只有 Critical 及红线才可申诉。

### 结局：整条线被删除

2026-09-21 `refactor(groupchat): 删除群聊模块 —— 前后端全部群聊能力下线`，一次性删除：

- 后端 `hermix-server`（dashboard api/dispatcher/sqlite + ws handlers + state 字段 + stdio 分支 + `/v1/ws-groupchat` 路由）、`hermix-gateway/src/groupchat`（11 文件）、`hermix-agent/src/group_chat`（5 文件）
- transport：`StreamEvent`/`WsClientMessage` 的 `GroupChat*`/`Discussion*`/`VoteKind`/`GroupChatSubtaskSpec` 变体与类型
- 前端 `GroupChatView`/`RoomMemberDialog`/`RoomSettingsDrawer`/`TaskPanel` 组件、`groupchat/` 子组件目录、Sidebar 聊天室分区、`NavPage 'groupchat'`、api rest/ws/types 全部群聊方法、`storageCompat` 群聊 key
- 2026-09-24 `chore(kernel): remove hermix-gateway and hermix-acp`（-30,419 行）删除了承载这一切的 crate

**因此本文中 `GlobalRank` / `TokenBudget` / `TaskScheduler` / `AgentActor` / `AuditEngine` 在当前代码库中均不存在**（`RankTier`、`TokenBudgetManager`、`TaskScheduler`、`AuditEngine` 在 `crates/` 下零匹配）。保留本笔记是因为其中的**成本推导**（预算裁剪上下文、摘要压缩 8:1、自摘要因果省掉一次 LLM 调用）在任何多 agent 编排方案重做时仍适用。
