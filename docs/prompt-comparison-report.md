# 提示词系统对比报告 — Hermes Agent vs hermix-desktop

## 分析范围

对比 hermesagent.org.cn 文档中的 Python 参考实现（Hermes Agent）与 hermix-desktop 的 Rust 实现，聚焦提示词组装、工具使用指导、记忆系统等所有提示词相关内容。

---

## 一、系统提示词架构对比

### Hermes Agent 的 10 层组装

| 层级 | 内容 |
|------|------|
| 1. stable | 身份标识（SOUL.md） |
| 2. stable | 工具/模型指南 |
| 3. stable | 技能提示词 |
| 4. stable | 环境提示 |
| 5. stable | 平台提示 |
| 6. context | 调用方 system_message |
| 7. context | 项目上下文文件 |
| 8. volatile | 内存快照（MEMORY.md） |
| 9. volatile | 用户画像（USER.md） |
| 10. volatile | 时间戳/会话/模型 |

### hermix-desktop 的 3 层 + 方向系统

| 层级 | 内容 |
|------|------|
| stable | 身份 → 方向（15种DirRoute） → 工具 → 技能 → 平台 |
| context | 内存 → 技能 → 上下文文件 |
| volatile | 时间戳/会话ID |

### 差异分析

**hermix 优势**：方向系统（DirRoute）比 Hermes 的 if/else 分支更精细，支持 FirstTurn/Subsequent/HasBrowser/HasMemory/HasSkillKeyword/Autonomous/MultiStepTask 等路由门控。

**Hermes 优势**：用户画像（USER.md）作为独立层级存在，hermix 没有明确的画像快照机制。

---

## 二、提示词内容逐项对比

### 2.1 身份标识

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| SOUL.md 加载 | 有 | 有 | 一致 |
| 默认身份回退 | 有 | 有 | 一致 |
| 安全扫描 | 不可见Unicode/注入/凭证 | 有 | 一致 |
| 截断限制 | 20K字符 | 有 | 一致 |
| SOUL.md 重复防止 | skip_soul 参数 | 有 | 一致 |

### 2.2 上下文文件

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| 优先级系统 | .hermes.md→AGENTS.md→CLAUDE.md→.cursorrules | 同上 | 一致 |
| 搜索范围 | .hermes.md 遍历到 git root，其他仅 cwd | 同上 | 一致 |
| YAML frontmatter 剥离 | 有 | 有 | 一致 |

### 2.3 平台提示

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| 内置默认值 | 有（PLATFORM_HINTS） | 有 | 一致 |
| append 模式 | 有 | 有 | 一致 |
| replace 模式 | 有 | 有 | 一致 |
| 纯字符串简写 | 有（= append） | 有 | 一致 |
| 配置驱动覆盖 | config.yaml platform_hints | AgentConfig.platform_hint_override | 一致 |

### 2.4 技能系统

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| 技能索引注入 | 有 | 有 | 一致 |
| 临时技能 | 有 | 有（ephemeral_skills） | 一致 |
| 技能文件约定 | SKILL.md + frontmatter | 同上 | 一致 |

---

## 三、方向/指导系统对比

### 3.1 路由门控

| 路由 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| FirstTurn | 有 | 有 | 一致 |
| Subsequent | 有 | 有 | 一致 |
| Autonomous | 有 | 有 | 一致 |
| HasBrowser | 有 | 有 | 一致 |
| HasMemory | 有 | 有 | 一致 |
| HasSkillKeyword | 有 | 有 | 一致 |
| HasBatchKeyword | 无 | 有 | hermix 更好 |
| MultiStepTask | 无 | 有（刚添加） | hermix 更好 |
| GoogleFamily | 无 | 有 | hermix 更好 |
| OpenAiFamily | 无 | 有 | hermix 更好 |
| MiniMaxFamily | 无 | 有 | hermix 更好 |
| ToolEnforce | 有 | 有 | 一致 |
| Always | 有 | 有 | 一致 |

### 3.2 指导内容

| 指导类型 | Hermes | hermix | 状态 |
|----------|--------|--------|------|
| 格式化指导 | 有 | 有 | 一致 |
| 思考指导 | 有 | 有 | 一致 |
| 工具使用指导 | 有 | 有（更详细） | hermix 更好 |
| 计划指导 | 有 | 有 | 一致 |
| 自主执行指导 | 有 | 有 | 一致 |
| 浏览器工具指导 | 有 | 有 | 一致 |
| 内存指导 | 有 | 有 | 一致 |
| 技能指导 | 有 | 有 | 一致 |
| 上下文预算指导 | 有 | 有（compact_tool_chain） | 一致 |
| 用户风格指导 | 无 | 有（P2-6） | hermix 更好 |
| 工具失败指导 | 有 | 有 | 一致 |
| 任务焦点指导 | 无 | 有（刚添加） | hermix 更好 |

---

## 四、工具系统提示词对比

### 4.1 工具注册

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| 自注册 | 导入时 registry.register() | inventory 宏 | 一致 |
| AST 扫描发现 | 有 | 无（用 inventory 替代） | 实现不同 |
| check_fn 可用性 | 有 | 有 | 一致 |
| check_fn 缓存 | 每次调用缓存 | cached_check.rs | 一致 |
| emoji 显示 | 有 | 有 | 一致 |

### 4.2 工具集解析

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| enabled_toolsets | 有 | 无（用 tool_router 替代） | 实现不同 |
| disabled_toolsets | 有 | 无 | 缺失 |
| 平台预设 | 有 | 有（toolset_distributions） | 一致 |
| 遗留名称映射 | 有 | 无 | 缺失 |
| 动态模式修补 | 有（execute_code/browser_navigate） | 无 | 缺失 |

### 4.3 工具调度

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| 双重错误包装 | registry + handle_function_call | registry + execute_tool_calls | 一致 |
| 代理循环工具拦截 | todo/memory/session_search/delegate_task | 有 | 一致 |
| 异步桥接 | _run_async() | run_async_in_blocking() | 一致 |
| 并行执行 | ThreadPoolExecutor | JoinSet | 一致 |

### 4.4 危险命令审批

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| DANGEROUS_PATTERNS | 有 | 有 | 一致 |
| 会话级审批记忆 | 有 | 有（刚添加） | 一致 |
| 永久允许列表 | 有（config.yaml） | 有（approval_allowlist.json） | 一致 |
| 智能审批 | 辅助LLM自动批准 | 无 | 缺失 |

---

## 五、记忆系统提示词对比

### 5.1 记忆指导

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| MEMORY_GUIDANCE | 有 | 有 | 一致 |
| session_search 指导 | 有 | 有 | 一致 |
| 记忆写入指导 | 有 | 有 | 一致 |
| 用户风格检测 | 无 | 有（P2-6 user.style） | hermix 更好 |

### 5.2 记忆召回

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| 首轮召回预算 | 50条 | 50条 | 一致 |
| 后续召回预算 | 5条 | 5条 | 一致 |
| 字符预算 | 4000 | 4000 | 一致 |
| 显著性阈值 | 0.30 | 0.30 | 一致 |
| 去重 | 按内容 | 按内容+access_count | hermix 更好 |
| 向量召回 | 无 | 有（EmbeddingProvider） | hermix 更好 |
| 任务上下文召回 | 无 | 有（刚添加） | hermix 更好 |
| MEMORY.md 注入 | 有 | 有 | 一致 |
| 事实提取 | user turn | user turn | 一致 |
| 审计日志 | 无 | 有（recall_audit） | hermix 更好 |

---

## 六、压缩系统提示词对比

### 6.1 压缩指导

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| SUMMARY_PREFIX | 有 | 有（刚增强） | 一致 |
| 任务恢复指令 | 基础 | 详细（In Progress/Blocked/Next Steps） | hermix 更好 |
| 压缩摘要模板 | 6节 | 7节（+Tools & Patterns） | hermix 更好 |
| 反抖动 | 连续2次<10%跳过 | 有 | 一致 |
| 安全网 | 敏感数据+注入+截断 | 有 | 一致 |

### 6.2 压缩配置

| 特性 | Hermes | hermix | 状态 |
|------|--------|--------|------|
| Agent层阈值 | 50% | 50%（刚调整） | 一致 |
| Gateway层阈值 | 85% | 无 | 缺失 |
| 可插拔引擎 | ContextEngine ABC | 无 | 缺失 |
| 迭代压缩 | 有 | 有 | 一致 |

---

## 七、发现的优化机会

### P0 — 高优先级

| 编号 | 优化项 | 说明 |
|------|--------|------|
| P-1 | USER.md 画像快照 | Hermes 有独立的用户画像层级，hermix 缺少 |
| P-2 | 智能审批 | Hermes 用辅助LLM自动批准低风险命令 |
| P-3 | Gateway 85%安全网 | Hermes 在Gateway层有预压缩安全网 |
| P-4 | 可插拔ContextEngine | Hermes 支持替换压缩引擎 |

### P1 — 中优先级

| 编号 | 优化项 | 说明 |
|------|--------|------|
| P-5 | 工具集显式启用/禁用 | Hermes 的 enabled_toolsets/disabled_toolsets 更灵活 |
| P-6 | 遗留工具集名称映射 | Hermes 有向后兼容的名称映射 |
| P-7 | 动态模式修补 | Hermes 会动态调整 execute_code/browser_navigate 的 schema |
| P-8 | AST扫描工具发现 | Hermes 用AST解析自动发现工具模块 |

### P2 — 低优先级

| 编号 | 优化项 | 说明 |
|------|--------|------|
| P-9 | Honcho静态块 | Hermes 支持 Honcho 人格数据注入 |
| P-10 | pre_llm_call插件上下文 | Hermes 支持插件在用户消息中注入上下文 |

---

## 八、hermix 已超越 Hermes 的领域

| 领域 | 说明 |
|------|------|
| 方向系统 | 15种DirRoute路由门控，比Hermes的if/else更精细 |
| 用户风格检测 | P2-6 user.style 事实自动检测 |
| 任务焦点指导 | MultiStepTask路由，长任务保持焦点 |
| 向量召回 | EmbeddingProvider支持语义搜索 |
| 任务上下文记忆 | 从消息中提取文件路径和错误信息增强召回 |
| 审计日志 | recall_audit表追踪召回质量 |
| compact_tool_chain | 批量工具调用减少上下文消耗 |
| 会话级审批记忆 | 已批准命令不再重复提示 |

---

## 九、总结

hermix-desktop 在以下方面已与 Hermes Agent 对齐或超越：
- 系统提示词层级架构
- 上下文文件加载和安全扫描
- 平台提示自定义
- 技能系统
- 记忆召回（额外支持向量召回和任务上下文）
- 压缩摘要质量
- 工具调度和错误处理

主要差距在于：
- 缺少 USER.md 用户画像快照机制
- 缺少智能审批（LLM自动批准）
- 缺少 Gateway 级安全网
- 工具集解析不如 Hermes 灵活
