# Grok Build 借鉴移植报告

## 一、背景

基于对 [xai-org/grok-build](https://github.com/xai-org/grok-build) 仓库的架构分析，提取 4 个可借鉴的设计点移植到 hermix-desktop。参考了 Grok Build 的 Agent Runner 模型、工具系统、沙箱隔离、Computer Use 能力。

## 二、参考项目架构对比

### Grok Build（参考）
- **定位**：终端 AI 编码助手（类 Claude Code / Codex）
- **语言**：Rust（70+ crate）
- **UI**：TUI（ratatui，全屏终端）
- **核心**：Agent-Runner 声明式编排、原生 Handoff、OpenAI Responses API 搜索

### Hermix Desktop（目标）
- **定位**：桌面 AI 助手（Windows，Tauri v2）
- **语言**：Rust（21 crate）+ React 19 前端
- **UI**：GUI（Tauri WebView2，桌面窗口）
- **核心**：9 阶段状态机 Agent Loop、审批引擎、30+ 工具、群聊调度器

### 关键差异

| 维度 | Grok Build | Hermix Desktop |
|------|-----------|----------------|
| Agent 编排 | 声明式 Agent-Runner | 显式 9 阶段状态机 |
| 多 Agent | 原生 handoff/transfer | 群聊调度器（Leader + Members） |
| 工具系统 | 移植 Codex/OpenCode | 自研 30+ 工具 |
| 上下文 | 分散 | 5 层模型 |
| 记忆 | 独立 crate | FocusStack + salience scoring |
| 沙箱 | 独立 crate | 审批引擎 |
| Computer Use | 未实现 | 已有 14 个 CUA 工具 |

## 三、移植内容

### 方案一：Computer Use 能力

**目标**：补充 GUI 自动化基础能力

**Grok Build 参考**：Grok Build 未实现 Computer Use，但 hermix 已有成熟的 `hermix-cua` crate（DXGI 截图 + SendInput 键鼠模拟 + UIAutomation + OCR）。

**移植内容**：

| 文件 | 改动 |
|------|------|
| `crates/hermix-cua/src/action/sendinput.rs` | 新增 `scroll()`, `double_click()`, `move_cursor()` |
| `crates/hermix-cua/src/action/mod.rs` | 新增带安全检查+审计的封装 |
| `crates/hermix-tools/src/tools/cua.rs` | 新增 3 个工具处理器 |

**新增工具**：

| 工具 | 功能 | 风险 |
|------|------|------|
| `cua_scroll` | 鼠标滚轮（支持正负方向） | Dangerous |
| `cua_double_click` | 双击（左键） | Dangerous |
| `cua_move_cursor` | 移动光标（不点击） | Low |

**CUA 工具总数**：14 → 17

---

### 方案二：Agent Builder 模式

**目标**：消除 AgentLoop 构造的 2x 重复，支持声明式配置

**Grok Build 参考**：`xai-grok-agent` 的 Builder 模式 + Agent 定义

**移植内容**：

| 文件 | 改动 |
|------|------|
| `crates/hermix-agent/src/agent_builder.rs` | 新增 `AgentLoopBuilder` + `AgentDefinition` |
| `crates/hermix-agent/src/lib.rs` | 导出新类型 |

**核心类型**：

```rust
// 声明式 Agent 定义（支持 YAML/JSON）
pub struct AgentDefinition {
    pub name: String,
    pub model: Option<String>,
    pub instructions: Option<String>,
    pub tools: ToolSelection,           // All / Include / Exclude
    pub permissions: PermissionConfig,
    pub max_turns: u32,
    pub autonomous_mode: bool,
    pub memory: MemoryConfig,
    pub sub_agents: Vec<AgentDefinition>,
}

// 流式 Builder
pub struct AgentLoopBuilder { ... }

impl AgentLoopBuilder {
    pub fn new(config, tool_registry, provider) -> Self;
    pub fn from_config(gateway_config, model, tool_registry) -> Result<Self>;
    pub fn from_definition(def, gateway_config, tool_registry) -> Result<Self>;
    pub fn build(self) -> AgentLoop;
    // 15+ .with_*() setters
}
```

**使用方式**：

```rust
// 从 GatewayConfig 构建
let agent = AgentLoopBuilder::from_config(&config, "deepseek-chat", registry)?
    .memory_router(router)
    .compressor(compressor)
    .build();

// 从 YAML 定义构建
let def = AgentDefinition::from_yaml(r#"
name: coder
model: deepseek-chat
instructions: You are a coding assistant...
tools:
  include: [terminal, file, search_files]
"#)?;
let agent = AgentLoopBuilder::from_definition(&def, &config, registry)?.build();
```

**消除的重复**：`AgentLoop::new()` + `with_interrupt()` 的 ~40 字段初始化统一到 Builder。

---

### 方案三：工具移植策略

**目标**：增强 keyless 工具，不引入 API key 依赖

**Grok Build 参考**：`xai-grok-tools` 的 `web_search` 使用 OpenAI Responses API（需 key）

**移植内容**：

| 文件 | 改动 |
|------|------|
| `crates/hermix-tools/src/tools/web.rs` | `WebSearchHandler` 支持 domain filtering |
| `crates/hermix-tools/src/backends/web.rs` | 新增 DuckDuckGo/Bing 后端 + MultiEngine 降级 |

**新增后端**：

| 后端 | 特点 |
|------|------|
| `DuckDuckGoHtmlSearchBackend` | keyless，提取标题+URL+摘要 |
| `BingHtmlSearchBackend` | keyless，提取标题+URL |
| `MultiEngineSearchBackend` | 自动降级：DuckDuckGo → Baidu → Bing |

**新增能力**：
- Domain 过滤：`allowed_domains` 参数限制搜索结果域名
- 多引擎降级：第一个引擎失败/空结果时自动尝试下一个
- 结果去重：各引擎内部 URL 去重

**设计决策**：选择纯 keyless 方案（而非 Grok 的 OpenAI API 方案），保持 hermix 的"开箱即用"理念。

---

### 方案四：沙箱隔离

**目标**：为高风险操作提供进程级隔离

**Grok Build 参考**：`xai-grok-sandbox` 独立 crate + `xai-grok-workspace` 的文件系统隔离

**移植内容**：

| 文件 | 改动 |
|------|------|
| `crates/hermix-tools/src/sandbox.rs` | 新建沙箱模块（486 行） |
| `crates/hermix-tools/src/lib.rs` | 导出沙箱类型 |
| `crates/hermix-tools/src/registry.rs` | `ToolEntry` 新增 `sandbox_policy` 字段 |

**核心类型**：

```rust
pub enum SandboxPolicy {
    None,           // 无隔离（默认）
    Local { env_whitelist },        // 临时目录 + 环境白名单
    Workspace { workspace_root },   // 工作区边界限制
    Docker { container, run_as_root }, // Docker 容器隔离
}

pub struct SandboxManager {
    pub fn policy_for_tool(&self, name, policy) -> SandboxPolicy;
    pub fn effective_risk_level(&self, name, risk) -> RiskLevel;
    pub fn check_path_allowed(&self, path, policy) -> Result<(), PathViolation>;
}

pub struct SandboxDecision {
    pub can_skip_approval: bool;  // Docker 沙箱可跳过审批
    pub risk_adjustment: RiskAdjustment;  // 风险降级
}
```

**风险降级规则**：
- `Local` / `Workspace`：Dangerous → Caution
- `Docker`：Dangerous / Caution → Safe（可跳过审批）

## 四、编译与测试状态

| 项目 | 状态 |
|------|------|
| `hermix-cua` (lib) | ✅ 编译通过 |
| `hermix-tools` (lib) | ✅ 编译通过 |
| `hermix-agent` (lib) | ✅ 编译通过 |
| hermix-tools 测试 | ✅ 621/621 通过 |
| hermix-agent 测试 | ✅ 964/964 通过 |
| CUA 工具测试 | ✅ 17/17 通过 |
| Sandbox 测试 | ✅ 8/8 通过 |
| 前端 TypeScript | ✅ 类型检查通过 |

## 五、后续可增加的优化

### 高优先级

| 优化 | 描述 | 复杂度 |
|------|------|--------|
| read_file 增量读取 | 支持 offset + limit 参数，移植 Codex 的增量读取 | M |
| search_files 正则增强 | 支持正则表达式、上下文行、文件过滤 | M |
| editor_infra 结构化编辑 | 移植 Codex 的 replace/insert/delete 结构化编辑 | L |
| YAML Agent 目录 | 支持 `~/.hermix/agents/*.yaml` 自动加载 | S |

### 中优先级

| 优化 | 描述 | 复杂度 |
|------|------|--------|
| 多显示器支持 | `cua_screen_info` 枚举所有显示器 | M |
| 剪贴板工具 | 复制/粘贴操作 | S |
| UIA ValuePattern | 通过 UIA 设置输入框文本 | M |
| 窗口最小化/最大化/关闭 | 专用工具 | S |
| 截图区域捕获 | 支持指定区域截图 | M |

### 低优先级

| 优化 | 描述 | 复杂度 |
|------|------|--------|
| DPI 感知 | 坐标计算考虑 DPI 缩放 | M |
| 文件拖放模拟 | DnD 事件注入 | L |
| UIA SelectionPattern | 下拉框/列表选择 | M |
| UIA ScrollPattern | 滚动特定 UI 元素 | M |
| Sandbox 配置 UI | Settings 中配置沙箱策略 | M |

## 六、文件清单

### 新增文件
- `crates/hermix-tools/src/sandbox.rs` — 沙箱策略系统

### 修改文件
- `crates/hermix-cua/src/action/sendinput.rs` — 新增 scroll/double_click/move_cursor
- `crates/hermix-cua/src/action/mod.rs` — 新增封装函数
- `crates/hermix-tools/src/tools/cua.rs` — 新增 3 个工具处理器
- `crates/hermix-tools/src/tools/web.rs` — 支持 domain filtering
- `crates/hermix-tools/src/backends/web.rs` — 新增多引擎后端
- `crates/hermix-tools/src/lib.rs` — 导出 sandbox 模块
- `crates/hermix-tools/src/registry.rs` — ToolEntry 新增 sandbox_policy
- `crates/hermix-agent/src/agent_builder.rs` — 新增 Builder + AgentDefinition
- `crates/hermix-agent/src/lib.rs` — 导出新类型
