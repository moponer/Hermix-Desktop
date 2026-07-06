# Harness Engineering 分析 — 基于参考项目实际代码

## 参考来源的实际代码

### 1. Hermes Agent: ToolGuardrailController (tool_guardrails.py)

**核心机制**: 检测工具调用循环，防止 Agent 陷入死循环

```python
# 三类检测：
# 1. 精确失败: 同一工具 + 相同参数 失败 N 次
# 2. 同工具失败: 同一工具（不同参数）失败 N 次
# 3. 幂等无进展: 幂等工具返回相同结果 N 次

@dataclass
class ToolCallGuardrailConfig:
    exact_failure_warn_after: int = 2      # 精确失败警告阈值
    exact_failure_block_after: int = 5      # 精确失败阻断阈值
    same_tool_failure_warn_after: int = 3   # 同工具失败警告阈值
    same_tool_failure_halt_after: int = 8   # 同工具失败停止阈值
    no_progress_warn_after: int = 2         # 无进展警告阈值
    no_progress_block_after: int = 5        # 无进展阻断阈值
```

**关键设计**:
- `ToolCallSignature`: 工具名 + 参数哈希，用于精确匹配
- `IDEMPOTENT_TOOL_NAMES`: 只读工具集合（read_file, search_files 等）
- `MUTATING_TOOL_NAMES`: 修改性工具集合（terminal, write_file 等）
- 决策类型: allow / warn / block / halt

### 2. Hermes Agent: conversation_loop.py

**核心循环结构**:
```python
while api_call_count < max_iterations:
    # 1. 中断检查
    if agent._interrupt_requested: break
    
    # 2. /steer 预排空
    _pre_api_steer = agent._drain_pending_steer()
    
    # 3. 消息修复（角色交替、工具调用参数）
    repaired_tool_calls = agent._sanitize_tool_call_arguments(messages)
    repaired_seq = repair_message_sequence_with_cursor(agent, messages)
    
    # 4. API 调用
    response = call_llm_with_retry(...)
    
    # 5. 工具调用守卫
    guardrail_decision = guardrail_controller.after_call(tool_name, args, result)
    if guardrail_decision.should_halt: break
    
    # 6. 验证停止检查
    if verify_on_stop_enabled():
        _verify_nudge = build_verify_on_stop_nudge(...)
        if _verify_nudge: continue  # 多跑一轮验证
    
    # 7. 上下文压缩
    if should_compress: compress_context()
```

### 3. claw-code: Rust 实现

**特性列表**:
- Tool guardrails (ToolGuardrailDecision)
- Permission system
- Session persistence + resume
- Cost/usage tracking
- Skills inventory
- Hooks system
- MCP server lifecycle

---

## hermix 缺失的关键机制

### 1. 工具调用循环守卫 (ToolCallGuardrailController)

**Hermes 有，hermix 无**

hermix 只有基础的重试计数器（`RetryCounters`），缺少：
- 精确失败检测（相同工具+相同参数）
- 幂等工具无进展检测
- 可配置的警告/阻断阈值
- 工具签名（工具名+参数哈希）

### 2. 验证停止机制 (verify_on_stop)

**Hermes 有，hermix 无**

当 Agent 声称完成任务时，Hermes 会：
1. 检查是否有文件被修改
2. 如果有修改，注入验证提示让 Agent 再跑一轮
3. 防止 Agent 过早声称完成

### 3. 消息序列修复

**Hermes 有，hermix 无**

Hermes 在每轮 API 调用前修复：
- 角色交替违规（tool → user 或 user → user）
- 工具调用参数损坏
- 临时脚手架消息清理

---

## 基于实际代码的改进方案

### P0: 工具调用循环守卫

**目标**: 防止 Agent 陷入死循环调用同一工具

**实现**:
```rust
// crates/hermix-tools/src/tool_guardrails.rs

pub struct ToolCallGuardrailConfig {
    pub exact_failure_warn_after: u32,    // 默认 2
    pub exact_failure_block_after: u32,   // 默认 5
    pub same_tool_failure_warn_after: u32, // 默认 3
    pub same_tool_failure_halt_after: u32,  // 默认 8
    pub no_progress_warn_after: u32,      // 默认 2
    pub no_progress_block_after: u32,     // 默认 5
}

pub struct ToolCallGuardrailController {
    config: ToolCallGuardrailConfig,
    exact_failure_counts: HashMap<ToolCallSignature, u32>,
    same_tool_failure_counts: HashMap<String, u32>,
    no_progress: HashMap<ToolCallSignature, (String, u32)>,
}

impl ToolCallGuardrailController {
    pub fn before_call(&self, tool_name: &str, args: &Value) -> ToolGuardrailDecision;
    pub fn after_call(&self, tool_name: &str, args: &Value, result: &str, failed: bool) -> ToolGuardrailDecision;
}
```

### P0: 验证停止机制

**目标**: 防止 Agent 过早声称完成

**实现**:
```rust
// crates/hermix-agent/src/verification.rs

pub fn verify_on_stop_enabled(config: &AgentConfig) -> bool;
pub fn build_verify_nudge(changed_files: &HashSet<String>, attempt: u32) -> Option<String>;
```

### P1: 消息序列修复

**目标**: 修复角色交替违规

**实现**:
```rust
// crates/hermix-agent/src/message_repair.rs

pub fn repair_message_sequence(messages: &mut Vec<Message>) -> usize;
pub fn repair_tool_call_arguments(messages: &mut Vec<Message>) -> usize;
```

---

## 实施优先级

| 优先级 | 改进项 | 复杂度 | 预期效果 |
|--------|--------|--------|----------|
| P0 | 工具调用循环守卫 | 中 | 防止死循环，节省 token |
| P0 | 验证停止机制 | 低 | 防止过早完成 |
| P1 | 消息序列修复 | 低 | 提高 API 调用成功率 |
| P1 | 幂等工具检测 | 低 | 减少无效调用 |

---

## 验证方案

1. 编写单元测试验证守卫逻辑
2. 模拟死循环场景测试阻断行为
3. 模拟验证停止场景测试多轮验证
4. 集成测试验证完整循环
