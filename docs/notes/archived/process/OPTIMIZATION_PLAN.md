# Hermix Desktop 优化计划

Archived: 2026-09-30

> 审查日期: 2026-07-11
> 审查范围: Agent Loop 核心引擎、前端渲染、会话持久化、WebSocket 传输、上下文管理、错误处理

---

## 一、已确认的问题与优化项

### P0: 高优先级 (影响用户体验或正确性)

#### P0-1: `GenericProvider` 中不必要的 `Mutex` 包裹

**文件**: `crates/hermix-agent/src/provider.rs` (line 40)

**问题**: `client: Arc<Mutex<Client>>` — `reqwest::Client` 本身已经是 `Arc` 内部包裹且线程安全的，额外的 `Mutex` 会引入不必要的锁争用。在高并发工具执行（多个 `delegate_task` 并行）时，所有线程争抢同一把锁。

**修复**: 改为 `client: reqwest::Client`（直接 clone，内部已有 `Arc`）。

**收益**: 消除锁争用，简化代码。

---

#### P0-2: 前端 `mergeConsecutiveAssistantMessages` 未被 `useMemo` 包裹

**文件**: `apps/desktop-tauri/src/components/ChatView.tsx` (line 83)

**问题**: 该函数在每次渲染时都重新执行，遍历全部消息并构建 `unifiedSteps`。在长会话（100+ 条消息）中，每次流式 chunk 到达都会触发完整的消息合并计算。

**修复**: 使用 `useMemo` 缓存计算结果，依赖项为 `[session?.messages]`。

**收益**: 减少 50-80% 的渲染期间计算开销。

---

#### P0-3: 自动滚动 effect 依赖项过宽

**文件**: `apps/desktop-tauri/src/components/ChatView.tsx` (line 1472)

**问题**: `useEffect` 依赖 `[session?.messages]`，在流式传输期间每秒触发 30+ 次。虽然使用了 rAF 节流，但每次都会创建新的 effect 闭包。

**修复**: 改为依赖 `[streamingText, session?.id]`，或使用 `useRef` 跟踪流式状态，减少 effect 重建。

**收益**: 减少 GC 压力，降低 CPU 使用率。

---

#### P0-4: `is_busy_error` 错误匹配范围过窄

**文件**: `crates/hermix-agent/src/session_persistence.rs` (line 59)

**问题**: 只检查 `AgentError::Io(msg)` 变体中是否包含 `SQLITE_BUSY`。如果错误被包装在其他变体（如 `AgentError::Other` 或 `AgentError::Provider`）中，重试逻辑不会触发，导致静默写入失败。

**修复**: 对所有 `AgentError` 变体做 `to_string()` 后检查是否包含 `SQLITE_BUSY` / `database is locked`。

**收益**: 避免静默消息丢失。

---

### P1: 中优先级 (影响性能或可维护性)

#### P1-1: `TurnReport` 模块完全未使用

**文件**: `crates/hermix-agent/src/turn_report.rs`

**问题**: 整个模块（`TurnReport`、`TurnToolCall`、`TurnOutcome`）都标记了 `#[allow(dead_code)]`。这是 AL-10 计划的一部分，但从未被接入 agent loop。增加了约 110 行维护负担。

**修复方案 A**: 在 `run_loop.rs` 的每轮结束时构造 `TurnReport` 并通过 `tracing::info!` 发射。
**修复方案 B**: 移除该模块，减少死代码。

**建议**: 方案 A — 已有完整结构定义，接入成本低，可显著提升可观测性。

---

#### P1-2: `convergence_detector.rs` 中的 `extract_visible_text` 是死代码

**文件**: `crates/hermix-agent/src/convergence_detector.rs` (line 268)

**问题**: 该函数只是 `content.trim().to_string()` 的包装，注释说"真正实现在 `AgentLoop::assistant_visible_text_after_think_blocks`"，但从未被调用。

**修复**: 删除该函数，或将其实现为调用真正的 think-block 解析器。

---

#### P1-3: `empty_response_handler.rs` 的 `last_content_with_tools` 字段废弃

**文件**: `crates/hermix-agent/src/empty_response_handler.rs` (line 96)

**问题**: `last_content_with_tools` 字段标记了 `#[allow(dead_code)]`，注释说"kept for API compat, no longer used by PatchLastAssistant"。PatchLastAssistant 已被移除（2026-07-09），该字段不再有调用者。

**修复**: 从 `EmptyResponseInput` 中移除该字段，更新所有构造处。

---

#### P1-4: `tool_call_dedup_key` 仅测试使用

**文件**: `crates/hermix-agent/src/tool_dispatch.rs` (line 71)

**问题**: `#[allow(dead_code)]` 注释说"only used in tests after removing cross-turn dedup"。跨轮去重已被移除，该函数只存在于测试中。

**修复**: 移到 `#[cfg(test)]` 块中，或删除。

---

#### P1-5: `run()` 和 `run_stream()` 仍有结构重复

**文件**: `crates/hermix-agent/src/run_loop.rs`

**问题**: 尽管 O1.1 重构提取了 `do_pre_turn_setup`、`do_prefetch`、`do_post_tool_iteration`，两个主循环仍有约 200 行相似的结构（LLM 调用 → 分类 → 工具执行 → 空响应处理 → 收敛检查）。

**修复**: 提取一个 `run_shared_loop(state, llm_caller)` 方法，通过函数指针区分 streaming/non-streaming 的 LLM 调用和 chunk 分发。

**收益**: 约 200 行代码去重，未来修改只需改一处。

---

#### P1-6: 上下文压力触发比率过于激进

**文件**: `crates/hermix-agent/src/context.rs` (line 87)

**问题**: `CONTEXT_PRESSURE_TRIGGER_RATIO = 0.50` — 在上下文窗口使用到 50% 时就触发自动压缩。对于 128K token 的模型，这意味着 64K 就开始压缩，可能过早丢失上下文细节。

**修复**: 将默认值调整为 `0.70`（与大多数 agent 框架一致），或改为可配置项。

**注意**: 需要验证 `CompressorConfig.pressure_threshold_percent` 在运行时是否正确覆盖此值。

---

### P2: 低优先级 (代码质量改进)

#### P2-1: 前端未实现的 TODO 功能

**文件**: `apps/desktop-tauri/src/components/RightSidebar.tsx`

以下功能仅有占位代码：
- line 1135: `TODO: Open terminal at file location`
- line 1139: `TODO: Open file preview`
- line 1245: `TODO: Call browser_navigate tool via Tauri`
- line 1255: `TODO: Call browser_snapshot tool`

**修复**: 实现这些功能，或在 UI 中隐藏未实现的按钮。

---

#### P2-2: `SettingsView.tsx` 配置未保存

**文件**: `apps/desktop-tauri/src/components/SettingsView.tsx` (line 1189)

**问题**: `TODO: Save to backend config` — 设置界面的修改不会持久化。

**修复**: 接入后端配置 API。

---

#### P2-3: Nudge 计数器过多

**文件**: `crates/hermix-agent/src/empty_response_handler.rs`

**问题**: 三个独立计数器（`codex_ack_continuations`、`autonomous_nudge_count`、`post_tool_nudge_count`）各自有独立的最大值常量和递增逻辑。虽然逻辑正确，但增加了认知复杂度。

**修复**: 考虑统一为一个 `NudgeState` 枚举（`None` / `CodexAck(u32)` / `Autonomous(u32)` / `PostTool(u32)`），通过模式匹配保证互斥性。

---

#### P2-4: `LoopPhase` 枚举未程序化读取

**文件**: `crates/hermix-agent/src/agent_loop/types.rs` (line 154)

**问题**: `#[allow(dead_code)]` 注释说"emitted via tracing; not all variants read programmatically"。9 个阶段中只有部分被 `tracing::debug!` 使用。

**修复**: 要么移除未使用的变体，要么接入 `TurnReport`（见 P1-1）使其被实际消费。

---

#### P2-5: `do_prefetch` 中用户消息收集效率

**文件**: `crates/hermix-agent/src/run_loop.rs` (line 168-174)

**问题**:
```rust
let user_text: String = ctx
    .get_messages()
    .iter()
    .filter(|m| m.role == MessageRole::User)
    .filter_map(|m| m.content.as_deref())
    .collect::<Vec<_>>()
    .join("\n");
```
每轮都遍历全部消息收集用户文本。在长会话中这是 O(n) 的重复工作。

**修复**: 在 `ContextManager` 中缓存最后一条用户消息的文本，或维护一个增量索引。

---

#### P2-6: `recent_msgs` clone 开销

**文件**: `crates/hermix-agent/src/run_loop.rs` (line 175-181)

**问题**:
```rust
let recent_msgs: Vec<Message> = ctx
    .get_messages()
    .iter()
    .cloned()
    .rev()
    .take(10)
    .collect();
```
每轮 clone 最多 10 条完整消息（包括 content、tool_calls、reasoning_content）。对于大消息（如长工具输出），这是显著的内存分配。

**修复**: 使用 `Arc<Message>` 或只传递消息引用/摘要。

---

## 二、优化优先级排序

| 优先级 | 编号 | 描述 | 预估工时 | 影响面 |
|--------|------|------|----------|--------|
| P0 | P0-1 | 移除 Provider Mutex | 0.5h | 后端并发性能 |
| P0 | P0-2 | useMemo 包裹消息合并 | 1h | 前端渲染性能 |
| P0 | P0-3 | 优化自动滚动 effect | 1h | 流式传输流畅度 |
| P0 | P0-4 | 扩大 busy error 匹配 | 0.5h | 数据持久化可靠性 |
| P1 | P1-1 | 接入或移除 TurnReport | 2h | 可观测性 / 代码清洁 |
| P1 | P1-2 | 删除死代码 extract_visible_text | 0.25h | 代码清洁 |
| P1 | P1-3 | 移除 last_content_with_tools | 0.5h | 代码清洁 |
| P1 | P1-4 | 移动 tool_call_dedup_key 到 cfg(test) | 0.25h | 代码清洁 |
| P1 | P1-5 | 提取 run_shared_loop | 4h | 代码可维护性 |
| P1 | P1-6 | 调整上下文压力阈值 | 1h | 长会话质量 |
| P2 | P2-1 | 实现/隐藏 RightSidebar TODO | 4h | UI 完整性 |
| P2 | P2-2 | 接入设置保存 | 2h | UX |
| P2 | P2-3 | 统一 Nudge 计数器 | 2h | 代码可维护性 |
| P2 | P2-4 | 清理 LoopPhase 变体 | 0.5h | 代码清洁 |
| P2 | P2-5 | 缓存用户消息文本 | 1h | 长会话性能 |
| P2 | P2-6 | 减少 recent_msgs clone | 1h | 内存分配 |

**总计**: 约 21.5 小时

---

## 三、建议执行顺序

### 第一批 (立即执行 — P0)
1. P0-1: 移除 Provider Mutex → `cargo check`
2. P0-4: 扩大 busy error 匹配 → `cargo test -p hermix-agent`
3. P0-2: useMemo 包裹消息合并 → `pnpm tsc --noEmit`
4. P0-3: 优化自动滚动 effect → `pnpm tsc --noEmit`

### 第二批 (本周 — P1 代码清洁)
5. P1-2: 删除 extract_visible_text
6. P1-3: 移除 last_content_with_tools
7. P1-4: 移动 tool_call_dedup_key
8. P1-1: 接入 TurnReport（可选方案 A）

### 第三批 (下周 — P1 性能/架构)
9. P1-6: 调整上下文压力阈值（需 A/B 测试）
10. P1-5: 提取 run_shared_loop（大型重构，需充分测试）

### 第四批 (按需 — P2)
11. P2-5 + P2-6: 消息收集优化
12. P2-3: 统一 Nudge 计数器
13. P2-1 + P2-2: 前端功能补全

---

## 四、验证清单

每个修改完成后执行:

```bash
# 后端修改
cargo check --workspace --all-targets
cargo test --workspace --lib

# 前端修改
cd apps/desktop-tauri && pnpm tsc --noEmit

# 完整 CI
./scripts/ci-gate.sh
```
