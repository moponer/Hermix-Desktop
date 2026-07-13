# Hermix / Hermes-Agent / OpenClaw 核心系统提示词对比报告

> 基准日期：2026-07-08 | hermix: commit d69a511 | hermes-agent: main | openclaw: main

## 一、架构概览

| 维度 | hermix | hermes-agent | openclaw |
|------|--------|-------------|----------|
| 语言 | Rust | Python | TypeScript |
| 装配策略 | `build_system_prompt` → 三层拼装 | `build_system_prompt_parts` → 三层拼装 | `buildAgentSystemPrompt` → 单函数组装 |
| 三层缓存 | ✅ stable / context / volatile + 独立 cache key + 锁 | ✅ stable / context / volatile（文档标注） | ⚠️ stable prefix hash cache only（其余在 cache boundary 以下） |
| 条件注入 | `DirRoute` 枚举 + `RouteContext` 条件匹配（FirstTurn/Subsequent/Autonomous/HasBrowser/HadAnyToolUse 等 12 种路由） | if/else 内联（provider-toolset 条件） | 参数驱动（promptMode: full|minimal|none + surface + 30+ params） |
| 代码行数（prompt 装配） | ~220 行（prompt_assembly.rs）+ ~500 行（directions.rs，含 25 个 const） | ~200 行（system_prompt.py） | ~960 行（system-prompt.ts） |
| 灵活性 | 中 — route 机制清晰但扩展需改枚举 | 低 — if/else 内联无抽象 | 极高 — 参数化 + provider override + section override + prompt contribution |

---

## 二、身份层（Identity）

### hermix
- SOUL.md 加载 → `## Active Personality` slot。若 SOUL 为空用 `load_builtin_memory_snapshot` 内置 snapshot。
- `resolve_personality` → 匹配命名 profile 或 inline 文本。
- workspace 注入 `## Active Workspace` 块。

### hermes-agent
- `load_soul_md()` 作为 "slot #1 in the system prompt"。
- 回退 `DEFAULT_AGENT_IDENTITY`（硬编码常量）。
- 无 workspace 顶层注入（依赖 context files 中的 `.hermes.md` / `AGENTS.md`）。

### openclaw
- 极简 identity 行：`"You are a personal assistant running inside OpenClaw."` + `buildModelIdentityPromptLine`。
- 无 SOUL.md 作为独立身份槽 — SOUL.md 被归类为 context file（排第 2 优先级，在 AGENTS.md 之后）。
- Authorized Senders 机制（ownerNumbers + hash/raw display）。

**差异**：hermix 和 hermes-agent 都把 SOUL.md 作为人格锚点，openclaw 将其降维为一般上下文文件。hermix 是唯一在工作区层面注入 `## Active Workspace` 的。

---

## 三、工具使用纪律

### hermix — 分层、路由驱动
| 块 | 路由条件 | 内容概要 |
|----|----------|---------|
| `TOOL_USAGE_GUIDANCE` | FirstTurn / Subsequent（短版） | "先查专用工具再 fallback terminal" + "绝对规则：用户给路径直接用对应 tool" |
| `TOOL_USE_ENFORCEMENT_GUIDANCE` | 模型族匹配（MiniMax/GPT/Codex/Gemini/Gemma/Grok） | 强制工具使用，每轮必须有 tool call 或有最终结果 |
| `BROWSER_TOOL_SELECTION_GUIDANCE` | HasBrowser | browser_* 不走 web_fetch |
| `PLAN_BEFORE_EXECUTE_GUIDANCE` | FirstTurn / Subsequent（短版） | 3 行 plan → 工具调用 |
| `COMPRESSION_AWARENESS_GUIDANCE` | Always | 压缩感知 |
| `URL_SAFETY_GUIDANCE` | Always | 安全 URL 构建 |
| `GIT_SAFETY_GUIDANCE` | Always | Git 安全操作 |

### hermes-agent
- `TOOL_USE_ENFORCEMENT_GUIDANCE`：强制使用工具，每条回复要么含 tool_call 要么给出结果。
- `TASK_COMPLETION_GUIDANCE`：完成工作实际产物（所有模型通用）。
- `PARALLEL_TOOL_CALL_GUIDANCE`：并行工具调用（所有模型通用）。
- 模型特定引导：`OPENAI_MODEL_EXECUTION_GUIDANCE` / `GOOGLE_MODEL_OPERATIONAL_GUIDANCE` / `computer_use_guidance()`。

### openclaw — 分区模块化
- **Tool Call Style** 块：低风险无叙述，高风险/敏感/复杂叙述。有机 tool 直接调不推给用户。
- **Execution Bias** 块（关键）：
  - "Actionable request: act in this turn."
  - "Continue until done or genuinely blocked; do not finish with a plan/promise when tools can move it forward."
  - "Weak/empty tool result: vary query, path, command, or source before concluding."
  - "Final answer needs evidence: test/build/lint, screenshot, inspection, tool output, or a named blocker."
- **Tooling** 块：完整 tool 表（名称 + summary），按 toolOrder 排序，区分 visibleTools / availableTools。

**差异**：hermix 的 TOOL_USAGE_GUIDANCE 强调"绝对规则：用户给路径直接调工具"是一个强力约束，两个开源项目都没有等价的显式规则。openclaw 的 Execution Bias 是三者中最激进的——明确要求"不要以 plan/promise 结束，工具能推进就继续"。

---

## 四、自主执行（Autonomous Mode）

### hermix — `AUTONOMOUS_EXECUTION_GUIDANCE`
- 路由：`DirRoute::Autonomous`（仅 `config.autonomous_mode = true` 时注入）
- 8 条规则。关键规则：
  - Rule 1: 工具可查的信息不提问
  - Rule 2: 优先试而非问
  - Rule 3: 工具失败立改方案，不停止
  - Rule 4: **唯一合法停止条件 = 任务完成或真正不可能**
  - Rule 7: 说了"let me use X"就必须 emit tool call
  - Rule 8: 相同工具返回相同输出 2+ 次 = 卡死

### hermes-agent
- 无显式 autonomous mode 概念。所有 guidance 默认对所有模型生效。

### openclaw
- 无 autonomous mode 开关。Execution Bias 块 + Sub-Agent Delegation 块共同实现等效效果：
  - Sub-Agent Delegation: prefer 模式下主 agent 只做协调、复杂工作 delegate 给 sessions_spawn。
  - "Continue until done or genuinely blocked" 与 hermix rule 4 等价。
  - "Non-final turn: use tools to advance, or ask for the one missing decision that blocks safe progress" — 比 hermix rule 1/2 更精确。

**差异**：hermix 是唯一把 autonomous mode 做成**可配置开关**的，这符合主人"自主编排任务路径"的设计哲学。openclaw 用 sub-agent delegation 做了更激进的"主 agent 少干活"策略。hermes-agent 无此概念。

---

## 五、任务规划（Todo / Plan / Kanban）

### hermix — `TODO_GUIDANCE`
- 适用条件：`tool_names.contains("todo")`
- 核心规则：≥2 步任务必建 todo → create/depend/list/update completed → 多 plan 支持（group_id/group_title）
- "Always reflect reality: a task in the list should match what you're actually doing."

### hermes-agent — `KANBAN_GUIDANCE`
- 仅当分配了看板任务时注入
- 看板任务执行协议

### openclaw
- **无内置 todo/kanban 系统**。依赖 Execution Bias 驱动连续执行。
- 唯一"追踪"是 sub-agent spawning + sessions_yield 的完成事件机制。

**差异**：hermix 的 todo 系统（多 plan + group_id）是三者中最完整的任务追踪方案。openclaw 用 sub-agent delegation 替代显式 todo list。hermes-agent 有 Kanban 但使用范围受限。

---

## 六、记忆系统（Memory）

### hermix
| 块 | 路由 | 内容 |
|----|------|------|
| `MEMORY_GUIDANCE` | HasMemory（tool 列表含 "memory"） | What/NOT to remember，核心理念：代码可推导的信息不存 |
| `SESSION_SEARCH_GUIDANCE` | HasSessionSearch | 跨会话搜索先行 |

### hermes-agent
- `MEMORY_GUIDANCE`：持久记忆使用规范（所有模型通用）
- `SESSION_SEARCH_GUIDANCE`：跨会话搜索指引
- 外部 memory provider block（由 memory_provider 插件注入）

### openclaw
- `buildMemorySection`：通过 `buildMemoryPromptSection` 生成
- `memoryCitationsMode` 参数控制记忆引用模式
- 记忆被归类为 "instruction context or user memory"

**差异**：三者都有一级记忆模块。hermix 的 "What NOT to remember" 排除清单是最精细的——明确列出代码模式/项目架构/git history/debugging 方案/AGENTS.md 内容/临时状态不应存入记忆。

---

## 七、缓存策略（Prompt Caching）

### hermix
- **三层独立 cache key**：
  - stable: `compute_prompt_cache_key(model, personality, system_prompt, workspace)` — 跨 turn 不变
  - context: `compute_context_cache_key(epoch)` — epoch 驱动失效
  - volatile: `build_dynamic_suffix(timestamp + session_id)` — 每 turn 变
- cache miss 时重渲染，hit 时直接返回字符串。size-cap 200KB。
- 日志统计：每个 turn 打 stable/context/volatile 的字符数 + token 估算 + cache hit/miss。

### hermes-agent
- `AIAgent._build_system_prompt()` 一次构建、跨 turn 复用。
- 仅 context compression 触发 rebuild。
- 保留 upstream prefix cache warm。
- 无显式分层缓存（靠 upstream provider prefix cache）。

### openclaw
- **Stable prefix hash cache**：
  - `cacheStablePromptPrefix(key, build)` — LRU cache (limit: 64 entries)
  - cache key = `hashStablePromptInput(workspaceDir + promptMode + toolLines + ... + 30 params)`
  - `SYSTEM_PROMPT_CACHE_BOUNDARY` 标记 stable/volatile 分界
  - 动态上下文文件（heartbeat.md）和 channel 特定块放在 boundary 之下，确保 stable prefix byte-identical 可复用

**差异**：hermix 的三层 cache 设计最系统——每个 layer 独立 key，日志输出完整。openclaw 的 hash-based prefix cache 和 cache boundary 模式更接近 production-grade prefix cache 优化。hermes-agent 依赖上游 provider 来 warm cache。

---

## 八、平台/渠道感知

### hermix
- `platform_hint_text()` — 简短格式提示：CLI → 终端简洁文本，chat 平台 → 避免笨重格式化。
- `platform_hint_override` — 支持追加（`+` 前缀）或替换内置 hint。
- 平台：CLI / Telegram / Discord / Slack / Email / SMS。

### hermes-agent
- `PLATFORM_HINTS` 字典：覆盖 whatsapp / telegram / discord / cli / webui / weixin 等 10+ 平台。
- 每个平台独立的媒体发送和格式化规则。
- `build_environment_hints()`：本机/远程后端/WSL/桌面 GUI 环境描述。

### openclaw
- **最丰富的 channel 感知**：直接集成到 prompt 核心。
- Channel 特定块：
  - Telegram: Bot API 10.1 rich formatting（table/collage/`<details>`/formula）
  - Discord: thread-bound ACP spawn
  - Webchat: Control UI embed / `[embed ...]` 语法
  - Slack: inline button hint 特殊处理
- `Messaging` 块：message/sessions_send/sessions_spawn/sessions_yield 的 channel 特定指导。
- `Voice (TTS)` 块。

**差异**：openclaw 的 channel 感知是三者中最深入的——直接生成 channel-native 的格式指导。hermix 保持最小化（仅给格式风格提示），hermes-agent 居中。

---

## 九、安全与护栏

### hermix
- 无独立 safety block（依赖工具级审批引擎）。
- `URL_SAFETY_GUIDANCE` / `GIT_SAFETY_GUIDANCE` 为特定域。
- `TOOL_FAILURE_GUIDANCE`：失败 2 次切换方案。

### hermes-agent
- 无独立 safety block（无内容策略注入）。
- `TOOL_FAILURE` 未显式声明（在 TASK_COMPLETION 中隐含）。

### openclaw
- **## Safety** 独立块（内容策略级）：
  - "No independent goals: no self-preservation, replication, resource acquisition, power-seeking"
  - "Safety/oversight over completion. Conflicts: pause/ask."
  - "Before changing config or schedulers, inspect existing state first and preserve/merge"
  - "Do not persuade anyone to expand access or disable safeguards"
- 沙箱运行时感知：sandbox enabled → "Sub-agents stay sandboxed"
- Elevated exec 感知：/elevated on|off|ask|full 状态传达

**差异**：openclaw 的安全块是面向生产部署的（考虑沙箱/提权/agent 滥用），hermix 和 hermes-agent 则依赖工具审批层。

---

## 十、关键差异总结

| 维度 | hermix 胜出 | hermes-agent 胜出 | openclaw 胜出 |
|------|------------|-------------------|---------------|
| 路由抽象 | DirRoute 枚举 + RouteContext — 清晰、可扩展 | — | — |
| 缓存架构 | 三层独立 cache + 完整日志 | — | Stable prefix hash cache + boundary 机制 |
| 自主执行 | 可配置开关 autonomous_mode — 唯一 | — | Execution Bias 文本更精确 |
| 任务追踪 | 多 plan todo 系统 — 最完整 | — | — |
| 工具粒度 | TOOL_USAGE_GUIDANCE "用户给路径直接调工具" — 强力约束 | — | — |
| 平台/渠道 | — | 10+ 平台覆盖 | 最深 — channel-native 格式 + sub-agent 路由 |
| 安全护栏 | — | — | Safety + sandbox + elevated exec — 生产级 |
| 代码组织 | directions.rs 集中管理 — 易维护 | if/else 内联 — 最低维护成本 | 参数驱动 + provider override — 最高灵活性 |
| 反注入保护 | — | `_scan_context_content` 阻断危险内容 | sanitizeContextFileContentForPrompt 净化 |
| 代码行数 | ~720 行 | ~200 行 | ~960 行 |

---

## 十一、hermix 可借鉴的点

### 立即建议（低风险，高收益）
1. **独立的 Safety block**：openclaw 的 `## Safety` 块可以在 hermix 作为 Always 路由方向加入，增强模型的安全边界认知。至少 3 条：禁止自我复制/权限提升、禁止说服用户放宽限制、冲突时暂停。

2. **Execution Bias 强化**：openclaw 的 "Final answer needs evidence" 可以补充到 hermix 的 AUTONOMOUS_EXECUTION_GUIDANCE rule 4 中——"任务完成意味着输出有证据支撑的结果"。

3. **Cache boundary 标记**：openclaw 的 `SYSTEM_PROMPT_CACHE_BOUNDARY` 思路可以移植到 hermix——在 stable 和 context/volatile 之间插入边界标记帮助 upstream prefix cache 识别。

### 长期考虑（需要设计讨论）
4. **Prompt contribution / override 系统**：openclaw 的 `ProviderSystemPromptContribution` 允许外部 provider plugin 注入/覆写特定 section（tool_call_style / execution_bias / interaction_style）。hermix 目前所有 guidance 硬编码在 directions.rs 中。

5. **Channel-native 感知深度**：如果 hermix 未来支持多平台部署（如桌面 app + Telegram bot），需要考虑渠道感知的分层——至少把"桌面 app"和"chat channel"的回复格式 hint 分开。

6. **Sub-agent delegation 模式**：openclaw 的 sessions_spawn + sessions_yield 的主 agent 委托模式是一种不同的设计路径。hermix 当前所有工具都由主 agent 直接执行。

---

## 十二、附录

### hermix 完整 guidance 列表（directions.rs）

| # | 常量 | 路由 | 行数 |
|---|------|------|------|
| 1 | `MEMORY_GUIDANCE` | HasMemory | ~10 |
| 2 | `SESSION_SEARCH_GUIDANCE` | HasSessionSearch | 1 |
| 3 | `SKILLS_GUIDANCE` | HasSkillKeyword | ~5 |
| 4 | `CONTEXT_BUDGET_GUIDANCE` | HasBatchKeyword | ~7 |
| 5 | `USER_STYLE_GUIDANCE` | Always | ~5 |
| 6 | `TOOL_USAGE_GUIDANCE` | FirstTurn/Subsequent | ~22+8 |
| 7 | `PLAN_BEFORE_EXECUTE_GUIDANCE` | FirstTurn/Subsequent | ~33+2 |
| 8 | `TOOL_USE_ENFORCEMENT_GUIDANCE` | 模型族匹配 | 1 |
| 9 | `BROWSER_TOOL_SELECTION_GUIDANCE` | HasBrowser | ~17 |
| 10 | `AUTONOMOUS_EXECUTION_GUIDANCE` | Autonomous | ~34 |
| 11 | `FORMATTING_DISCIPLINE_GUIDANCE` | FirstTurn/Subsequent | ~13+1 |
| 12 | `THINKING_DISCIPLINE_GUIDANCE` | FirstTurn/Subsequent | ~17+1 |
| 13 | `URL_SAFETY_GUIDANCE` | Always | ~10 |
| 14 | `COMPRESSION_AWARENESS_GUIDANCE` | Always | ~9 |
| 15 | `OPENAI_MODEL_EXECUTION_GUIDANCE` | 模型族匹配 | 2 |
| 16 | `GOOGLE_MODEL_OPERATIONAL_GUIDANCE` | 模型族匹配 | 1 |
| 17 | `TOOL_FAILURE_GUIDANCE` | Always | 1 |
| 18 | `GIT_SAFETY_GUIDANCE` | Always | ~20 |
| 19 | `SKILL_USAGE_GUIDANCE` | HasSkillKeyword | ~17 |
| 20 | `TASK_FOCUS_GUIDANCE` | tool_call_count ≥ 5 | ~22 |
| 21 | `TODO_GUIDANCE` | HasTodo | ~37 |
| 22 | `TOOL_EXECUTION_SUMMARY_GUIDANCE` | HadAnyToolUse | ~17 |
| 23 | `STOPPING_SUMMARY_GUIDANCE` | HadAnyToolUse | ~47 |
| 24 | `REPLY_TERMINATION_GUIDANCE` | Always | ~25 |
| 25 | `MINIMAX_TOOL_FORMAT_GUIDANCE` | 模型族匹配 | ~17 |

### openclaw 完整 prompt section 列表（system-prompt.ts）

| Section | 默认状态 | 可 override |
|---------|---------|-------------|
| Identity line + Model Identity | Always | ❌ |
| Tooling (tool list + summaries) | Always | ❌ |
| Tool Call Style | Always | ✅ Provider section override |
| Execution Bias | Always | ✅ Provider section override |
| Safety | Always | ❌ |
| OpenClaw Control (gateway) | hasGateway | ❌ |
| Skills + Skill Workshop | Always | ❌ |
| Memory | includeMemorySection | ❌ |
| Model Aliases | has aliases | ❌ |
| Workspace | Always | ❌ |
| Documentation | Always | ❌ |
| Sandbox | sandbox enabled | ❌ |
| Current Date & Time | has userTimezone | ❌ |
| Bootstrap | bootstrapMode | ❌ |
| Workspace Files | Always | ❌ |
| Assistant Output Directives | !isMinimal | ❌ |
| Reasoning Format | reasoningTagHint | ❌ |
| Project Context (stable) | Always | ❌ |
| Silent Replies | !isMinimal && mode≠none | ❌ |
| Dynamic Project Context | has dynamic files | ❌ |
| Approval UI | Always (below boundary) | ❌ |
| Authorized Senders | has owners | ❌ |
| Webchat Canvas | runtimeChannel=webchat | ❌ |
| Messaging | !isMinimal | ❌ |
| Voice (TTS) | has ttsHint | ❌ |
| Conversation Context | extraSystemPrompt | ❌ |
| Reactions | reactionGuidance | ❌ |
| Provider Dynamic Suffix | providerDynamicSuffix | ❌ |
| Heartbeat | has heartbeatPrompt | ❌ |
| Runtime | Always (below boundary) | ❌ |

### hermes-agent 完整 section 列表（prompt_builder.py）

| Part | 来源 |
|------|------|
| Identity (slot #1) | SOUL.md 或 DEFAULT_AGENT_IDENTITY |
| Platform Hint | PLATFORM_HINTS[platform] |
| Environment Hint | build_environment_hints() |
| Skills Index | build_skills_system_prompt() |
| Project Context (.hermes.md/AGENTS.md/CLAUDE.md/.cursorrules) | build_context_files_prompt() |
| HERMES_AGENT_HELP_GUIDANCE | 常量 |
| MEMORY_GUIDANCE | 常量 |
| SESSION_SEARCH_GUIDANCE | 常量 |
| SKILLS_GUIDANCE | 常量 |
| KANBAN_GUIDANCE | 常量 |
| TOOL_USE_ENFORCEMENT_GUIDANCE | 常量 |
| TASK_COMPLETION_GUIDANCE | 常量 |
| PARALLEL_TOOL_CALL_GUIDANCE | 常量 |
| OPENAI_MODEL_EXECUTION_GUIDANCE | 常量 |
| GOOGLE_MODEL_OPERATIONAL_GUIDANCE | 常量 |
| Computer-use guidance | computer_use_guidance(platform) |
| Nous subscription block | build_nous_subscription_prompt() |
| Steer channel note | STEER_CHANNEL_NOTE |
| Memory snapshot | AIAgent 记忆系统 |
| Timestamp/session/model/provider | volatile 层 |

---

*报告由自动化对比工具生成，基于各项目主分支最新源码。*
