# Hermix Desktop 架构问题清单

> 分析时间：2026-07-17
> 覆盖范围：hermix-agent、hermix-tools、hermix-config、hermix-cua
> 分析方法：静态代码审查 + 架构模式检查

---

## 一、严重问题（HIGH）— 需立即修复

### H1. `std::env::set_var` 在 async 上下文中的数据竞争
- **文件**: `local_agent_service.rs:847`
- **问题**: `set_var("TERMINAL_CWD", ws)` 在 async 函数中调用，可能被多个 session 并发执行。`set_var` 修改全局状态，在多线程程序中是 unsafe 的（Rust 1.66+）。两个 session 不同的工作区会互相覆盖。
- **影响**: 文件操作可能指向错误的目录
- **修复**: 移除此 `set_var`，仅通过 `ToolRegistry::set_terminal_workspace` 路径传递工作区

### H2. UTF-8 字节/字符边界 panic
- **文件**: `compression.rs:162-179`
- **问题**: `truncate_summary_at_boundary` 用 `max_chars` 做字节切片 `&text[..max_chars]`。如果 `max_chars` 落在多字节 UTF-8 字符中间（如中文字符占 3 字节），会 panic
- **影响**: 进程崩溃
- **修复**: 用 `char_indices()` 找到正确的字符边界

### H3. Session 审批绕过 Denied 模式
- **文件**: `approval.rs:380-384`
- **问题**: `check_approval` 先检查 session 级审批，再检查 denied 模式。如果 `rm -rf /` 被错误加入 `session_approved`，会直接返回 `Approved` 而跳过 catastrophic denial 检查
- **影响**: 安全策略被绕过
- **修复**: 将 denied 模式检查移到 session 审批之前

### H4. 终端工具命令注入无纵深防御
- **文件**: `terminal.rs:357-364`
- **问题**: LLM 生成的命令字符串直接传给 shell 执行，唯一的防御是审批模式的正则匹配。`&&`、`||`、`;`、反引号等 shell 元字符没有通用检测
- **影响**: 恶意/幻觉命令可绕过审批执行
- **修复**: 添加通用 shell 元字符检测（`&&`、`||`、`;`、`` ` ``、`$(`），标记为 RequiresConfirmation

---

## 二、中等问题（MEDIUM）— 应尽快修复

### M1. 生产代码中残留 debug eprintln
- **文件**: `local_agent_service.rs:1380-1397`
- **问题**: `[!!!SEND_MSG_STREAM_ENTRY!!!]` 和 `[!!!SEND_MSG_STREAM_LOADED!!!]` 在每次流式消息时输出到 stderr
- **修复**: 替换为 `tracing::debug!` 或移除

### M2. 硬编码 200K 上下文窗口
- **文件**: `local_agent_service.rs:544`, `compression.rs:1387`
- **问题**: `gateway_token_warn` 和 `context_collapse` 硬编码 200K tokens，不感知实际模型的上下文窗口
- **修复**: 从 `model_metadata::get_model_context_length` 获取实际值

### M3. `config.lock().unwrap()` 会因 mutex 中毒而 panic
- **文件**: `local_agent_service.rs:229,348,356,370,438,473,485,673`
- **问题**: 8 处 `.lock().unwrap()`，任何一处 panic 都会导致后续所有调用 panic
- **修复**: 替换为 `.lock().unwrap_or_else(|e| e.into_inner())` 或使用 `parking_lot::Mutex`

### M4. 进程全局工具状态跨 session 泄漏
- **文件**: `local_agent_service.rs:1164-1198`
- **问题**: `set_terminal_approval_ctx`、`set_active_session_for_tool` 等修改进程全局状态。如果两个 session 并发，状态会混乱
- **修复**: 长期方案：将 session ID 线程化到工具分发中；短期：添加并发断言

### M5. Token 估算不准确（CJK 文本）
- **文件**: `context.rs:128-131`
- **问题**: `estimate_tokens` 用 4 字符/token 启发式，CJK 文本实际 1 字符 ≈ 1 token。可能导致上下文溢出
- **修复**: 对压力检查使用更准确的估算器

### M6. `cached_total_tokens` 可能漂移
- **文件**: `context.rs:306-313`
- **问题**: `get_messages_mut()` 允许直接修改消息向量而不更新缓存，调用者可能忘记调用 `recompute_total_tokens`
- **修复**: 使 `get_messages_mut` 私有，添加类型化的修改方法

### M7. `HERMIX_CONFIRM=1` 绕过 Denied 模式
- **文件**: `terminal.rs:279-280`
- **问题**: 该标记在调用 `check_approval` 前被剥离，导致即使是 denied 命令也能绕过
- **修复**: 如果剥离后的命令匹配 denied 模式，仍然拒绝

### M8. `workdir` 无路径遍历验证
- **文件**: `terminal.rs:340-348`
- **问题**: LLM 传入的 `workdir` 未检查 `..` 遍历或符号链接
- **修复**: 使用 `has_traversal_component` 或 `validate_within_dir` 检查

### M9. Session 审批无空白标准化
- **文件**: `approval.rs:381`
- **问题**: 审批使用精确字符串匹配，`sudo apt install nginx` 和 `sudo  apt  install  nginx` 不匹配
- **修复**: 标准化空白（trim + 合并内部空白）

### M10. `max_turns` 无边界验证
- **文件**: `config.rs:30`
- **问题**: 接受 0（agent 无法完成任务）和 `u32::MAX`（无限循环）
- **修复**: 添加范围验证（1-1000）

### M11. `temperature` 接受 NaN/Inf
- **文件**: `config.rs:427`
- **问题**: `Option<f32>` 反序列化接受任何值
- **修复**: 添加验证拒绝 NaN 和 Inf，限制范围 [0.0, 2.0]

### M12. `CompactionConfig` 无交叉验证
- **文件**: `config.rs:256-321`
- **问题**: `summary_target_ratio` 可以大于 `threshold_percent`
- **修复**: 添加 `validate()` 方法检查比率关系

### M13. 原生插件无沙箱
- **文件**: `native_plugin.rs:88-143`
- **问题**: 从 `$HERMIX_HOME/plugins/` 加载的插件有完整主机权限
- **修复**: 文档化信任模型，考虑签名验证

### M14. 后台任务无优雅关闭
- **文件**: `local_agent_service.rs:471-528`
- **问题**: `warm_up_default_provider` 和 `spawn_background_review` 的 JoinHandle 未存储，进程关闭时被取消
- **修复**: 存储 JoinHandle 并在关闭时 abort

### M15. `dispatch_single` 无超时
- **文件**: `dispatch.rs:318-353`
- **问题**: 单次工具调用可无限挂起
- **修复**: 添加可选超时参数

---

## 三、低级问题（LOW）— 可在空闲时修复

### L1. AgentLoop 25+ 字段 + 多个 `Arc<Mutex<...>>`
- **文件**: `agent_loop/mod.rs:102-255`
- **影响**: 认知负荷高，锁顺序难以推理

### L2. `context_epoch` 是死代码
- **文件**: `agent_loop/mod.rs:249-250`
- **影响**: prompt cache 的上下文层失效从未触发

### L3. `merge_consecutive_assistant_messages` 克隆过多
- **文件**: `local_agent_service.rs:96-143`
- **影响**: 性能微优化

### L4. 工具结果双重截断
- **文件**: `dispatch.rs:271` + `registry.rs:810`
- **影响**: 浪费 CPU

### L5. `remove_file` 错误在 Windows 上被静默忽略
- **文件**: `approval.rs:607, 809`
- **影响**: 文件锁定问题难以诊断

### L6. `format_command_output` 无输出大小检查
- **文件**: `terminal.rs:428-446`
- **影响**: 大输出的中间分配

### L7. Semaphore panic 而非优雅错误
- **文件**: `dispatch.rs:174-177`
- **影响**: 调试困难

### L8. `send_message` 和 `send_message_stream` 共享 ~300 行重复代码
- **文件**: `local_agent_service.rs`
- **影响**: 维护负担

### L9. MCP server name 可以为空
- **文件**: `config.rs:748-755`
- **影响**: 连接时错误信息不清晰

### L10. `truncate_to_budget` 仅保留尾部，丢失中间上下文
- **文件**: `context.rs:359-414`
- **影响**: 无压缩器时丢失对话中间部分

---

## 四、统计汇总

| 严重级别 | 数量 | 关键领域 |
|----------|------|----------|
| **HIGH** | 4 | 数据竞争、UTF-8 panic、安全绕过、命令注入 |
| **MEDIUM** | 15 | 配置验证、状态管理、安全加固、性能 |
| **LOW** | 10 | 代码质量、维护性、微优化 |
| **总计** | 29 | — |

---

## 五、修复优先级建议

### 立即修复（本周）
1. **H1** — `set_var` 数据竞争（移除即可）
2. **H2** — UTF-8 panic（一行修复）
3. **H3** — 审批绕过（调整检查顺序）
4. **M1** — 移除 debug eprintln

### 短期修复（两周内）
5. **H4** — 终端命令注入防御
6. **M2** — 移除硬编码 200K
7. **M3** — Mutex 中毒处理
8. **M7** — `HERMIX_CONFIRM` 安全加固
9. **M10-M12** — 配置验证

### 中期修复（一个月内）
10. **M4** — 全局状态 session 隔离
11. **M5-M6** — Token 估算准确性
12. **M13-M15** — 插件安全、关闭处理、超时

### 长期改进
13. **L8** — 消除 `send_message` 代码重复
14. **L1** — AgentLoop 字段重组
15. **L2** — 实现 `context_epoch` 写入端
