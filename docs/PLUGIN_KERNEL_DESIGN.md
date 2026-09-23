# Hermix 插件内核设计 v3（参考 DeepSeek Harness + Pi）

> 状态：**待主人评审**，未动业务代码。
> 基线：Rust 侧 112,337 行（含 tests）／24 crate；前端 44.5K 行。
> 取代 `FOUNDATION_REWRITE_DESIGN.md` 的架构部分（其网络安全章节继续有效）。

---

## 0. 三个必须先钉住的事实

### 0.1 Pi 没有审批还能跑，靠的不是沙箱

Pi README 原文：

> Pi does not include a built-in permission system for restricting filesystem, process, network, or credential access. **By default, it runs with the permissions of the user and process that launched it.**

沙箱在 Pi 里是**可选外挂**，四种模式（`packages/coding-agent/docs/containerization.md`）：Plain Docker / Docker Sandboxes / OpenShell / Gondolin extension。且它自己承认洞：

> Commands inside the VM inherit the host process environment. Provider keys … can therefore be visible inside the VM. **Do not use this pattern as a credential boundary.**

**真正的差异是授权模型，不是沙箱：**

| | Hermix 现状 | Pi |
|---|---|---|
| 决策时机 | 运行时，每个 tool_call | 启动时，一个边界 |
| 粒度 | 细（单条命令） | 粗（整个执行世界） |
| 人在回路位置 | **agent loop 内部**，阻塞 | **agent loop 外部**，非阻塞 |
| 后果 | `AwaitApproval` 是**跨连接、跨进程的持久中间态** | loop 里没有"等人"节点 |

**这是稳定性问题，不只是体验问题。** 只要保留 prompt-per-action，就必须实现：审批落盘 → 跨连接重投 → 超时 default-deny 调度 → 幂等去重 → 前后端定时器对齐。这五样全是复杂度、全是 bug 面——这两天所有"卡死/孤儿卡/丢卡"都源自它。Pi 用一条设计决策把它们全消掉了。

**推论：Hermix 不该再"逐命令审批"，而应"边界内放行、越界才拦"。** 边界由 `TerminalBackendType` 的既有扩展点提供（见 §5.3）。

### 0.2 Hermix 已有外部能力注入基础设施

`crates/hermix-mcp/src/client.rs:930` 已把外部 MCP server 的工具以 `server__tool` 名注册进**共享 `Arc<ToolRegistry>`**，支持 tools / resources / prompts / sampling，传输含 stdio + HTTP/SSE + OAuth/bearer。

**即"集成开源套件"的地基是现成的**，不需要新建。缺的只是把它从 `hermix-server` 手里上提到内核接缝。

### 0.3 现有 `hermix-plugin` 不是插件系统

`crates/hermix-plugin/src/lib.rs:1-16` 是 **custom app（hx-shell + WebView2 桌面小应用）**，7,450 行，与插件内核无关。真正的插件机制在两处：

- `hermix-agent/src/native_plugin.rs`（414 行）：扫 `$HERMIX_HOME/plugins/`，`hermix_plugin_init()` C ABI，但头部注释明示 **"full host process privileges，无沙箱、无签名、无权限声明"**，`PluginKind` 只有 `Tools / Memory`
- `hermix-agent/src/plugins.rs`（952 行）：进程内 Rust trait 插件，能注册工具，**无 provider 注册 API**

---

## 1. 内核只保留四件套

DeepSeek Harness 的原则：

> There is no privileged core to patch: you extend dsh by mounting a plugin beside the others, and **registrations are effects that unwind when their plugin unloads**.

模型适配器、工具注册表、会话日志、**agent loop 本身**都是插件。Rust 侧落地为四项：

| 内核件 | 职责 | Rust 形态 | 目标行数 |
|---|---|---|---|
| **C1 Context** | 服务注册表 + 依赖注入 + 可逆效果 | `Context` 持有 `HashMap<TypeId, Box<dyn Any>>`；注册返回 `EffectGuard`，`Drop` 即注销 | ~800 |
| **C2 Session** | append-only 事件日志，唯一真相源 | `SessionEventMap` + 世代文件 `session.vN.jsonl[.zst]` | ~2,500 |
| **C3 Seams** | 能力接缝：trait(定义) + provider(实现) + consumer(消费者) | 每个 seam 一个 trait + 一个 registry | ~1,500 |
| **C4 Boot** | profile/bundle 有序组合 + patch 覆盖 | YAML 层叠，按 id 整行替换 | ~600 |

**不变量（对应 dsh 的 "Model-visible means logged"）：**
> 任何进入模型请求的内容，必须能从日志重建。runtime 有断言检查。

### C1 的最小 Rust 表达

Cordis 的"可逆效果"在 Rust 里等价于：注册返回 guard，Drop 自动反注册。无需引入 JS 运行时，约 800 行可自实现：

```rust
pub struct EffectGuard { ctx: Context, key: TypeId, id: u64 }
impl Drop for EffectGuard { fn drop(&mut self) { self.ctx.unregister(self.key, self.id) } }
```

插件卸载 = drop guard 集合，注册自动 unwind。这比现在的"插件注册进去就拔不出来"强一个量级。

### C3 接缝清单（照抄 dsh 的 seam 三角色完整性）

dsh 原话：
> A **seam** is a swappable capability with three roles: a **Service Definition** declaring the interface, a **Service Provider** implementing it, and a **Consumer** using it … one role alone is not a seam.

| Seam | Definition | 内核默认 Provider | 可换 Provider |
|---|---|---|---|
| `Llm` | 流式补全 + 工具调用 | 无（必须挂插件） | OpenAI / Anthropic / MiniMax / DeepSeek |
| `Tools` | 工具注册 + 守卫执行 | `ToolRegistry`（现有） | MCP server / 外部进程 |
| `Fs` | 文件读写策略 | 本地工作区 | 只读挂载 / 远程沙箱 / 内存 |
| `Shell` | 命令执行 | `Local` | Docker / WSL2 / SSH / Daytona / Modal |
| `Sandbox` | 进程限制 | Job Object + 受限令牌 | 容器 / microVM |
| `Memory` | 记忆召回 | SQLite FTS5 | mem0 / 纯 embedding / 关掉 |
| `Skills` | 技能加载 | `FileSkillStore` | 外部目录 / hub |
| `Transport` | 对外暴露面 | 关闭 | WS / HTTP（含手机端配对） |
| **`AgentLoop`** | **"下一步干什么"的决策权** | **`DefaultLoop`** | Codex 式 / Plan-Execute / ReAct / 纯问答（见 C4） |

**关键性质**：`Fs` 与 `Shell` 共享一个执行世界——把 provider 指向远程沙箱，Bash、文件读写、LSP 整体跟着搬走，不需要给每个工具写 fork。

### C4 `AgentLoop` 也是插件（主人 2026-09-24 明确要求）

> "agent loop 也是插件，我说了参考一切皆插件的思路。以后迭代方便。"

DeepSeek Harness 的原话就是这条：

> The model adapters, the tool registry, the session log, and **the agent loop itself** are all plugins.

**为什么这是"以后迭代方便"的关键**：agent loop 是唯一持有「下一步干什么」决策权的组件。它若硬编码，则每换一种交互范式（流式问答 / Plan-Execute / Codex 式 / 多 agent 编排）都要改内核。做成接缝后，**换范式 = 换一个 provider，内核不动**。

#### 最小契约

```rust
/// 内核只定义这一个 trait；Loop 是唯一决定"下一步"的组件。
#[async_trait]
pub trait AgentLoop: Send + Sync {
    fn name(&self) -> &str;
    /// 跑一轮；返回"下一步内核该做什么"，而非自己去做。
    fn step(&self, cx: &LoopContext) -> BoxFuture<'_, Result<StepDirective, LoopError>>;
}

pub enum StepDirective {
    CallModel(ModelRequest),        // 内核负责发请求、收流、落盘
    ExecuteTools(Vec<ToolCall>),    // 内核负责调度、守卫、写回结果
    Finish(FinishReason),           // 内核负责提交任务终态
    Suspend(SuspendReason),         // 让出（如等待外部事件），内核把它持久化
}
```

**内核给 Loop 的手柄**（`LoopContext`）——Loop **只能通过这四个句柄改世界**：

| 句柄 | 背后 seam | Loop 能做什么 |
|---|---|---|
| `session` | `Session` | 读变更线、起草新条目（**不能直接 `commit`**） |
| `model` | `Llm` | 请求一次补全（拿不到 provider 细节） |
| `tools` | `Tools` | 列举可用工具、发起调用（拿不到进程/文件系统） |
| `hooks` | `HookBus` | 发事件、订阅别人的事件 |

#### 三条硬约束（保证"换 Loop 不会破坏持久不变量"）

1. **Loop 不能直接提交**：提交权在内核。Loop 只返回 `StepDirective`，由内核执行并落盘。**好处：任何 Loop 都无法绕过"提交成功才发布"这条不变量。**
2. **Loop 不能碰 DB / WS / 裸连接**：`LoopContext` 里没有这些类型，编译期就杜绝。这也顺带消灭了现在 `local_agent_service.rs`（4,364 行）里"业务逻辑直写传输层"的坏味道。
3. **Loop 必须可中断可恢复**：`step()` 不持有跨调用的可变状态——需要跨轮记忆就写进 `session` 变更线，由内核持久化。**进程崩溃后，内核能从变更线重建出 Loop 该从哪里继续。**

#### 可选 provider（就是"以后迭代方便"的具体形态）

| provider | 适用 | 来源 |
|---|---|---|
| `DefaultLoop` | 现在的 ReAct 式（思考→工具→观察），从 `local_agent_service.rs` 拆出 | 现有代码迁移 |
| `CodexLoop` | 三档沙箱 + 审批矩阵 + 元数据目录保护 | 抄 `codex-rs` 语义 |
| `PlanExecuteLoop` | 先出计划再执行，长任务不易跑偏 | 自研 ~600 行 |
| `PlainChatLoop` | 纯问答，零工具 | 自研 ~200 行 |
| `DesktopLoop` | 截图→grounding→动作→再截图（**CUA 循环**，§3.2.3 的 L2-L4） | 抄 UI-TARS + Agent S2 |

**选择方式**：`config.toml` 里 `agent_loop = "default"`，与 `model`、`skills_dir` 同级。换一行配置就换范式，不改代码。

---

## 2. 行数账目（基线 112,337 行）

### 2.1 P0 砍死重（零引用，零风险）

| crate | 行数 | 证据 |
|---|---:|---|
| `hermix-gateway` | 27,451 | 全仓 Rust 零引用；20 个平台适配器全 `cfg(feature)` 无一启用 |
| `hermix-acp` | 2,276 | lib-only，零引用，与 `copilot_acp.rs` 重复 |
| **小计** | **29,727** | |

### 2.2 P1 三档处理：DEL / MOVE / TRIM

**口径修正（主人 2026-09-24 追问）**：此前写的"外置为插件"是含糊的——留在仓库子目录只是**不编译**，行数一分没减。真正让主仓变瘦只有三条路：

| 档位 | 含义 | 主仓行数 |
|---|---|---|
| **DEL** | 直接删源码，功能消失；将来需要时从开源项目抄回或由插件补 | **真减** |
| **MOVE** | 移出 workspace 到独立 repo（`hermix-contrib`），需要时 clone 挂载 | **真减** |
| **TRIM** | 砍到最小内核，只留接缝与必需实现 | 真减（差额） |

**默认按 DEL 处理**，不从 DEL 降级为"移到子目录"。

| crate | 现行数 | 档位 | 主仓减少 | 删除阻力（实测引用面） |
|---|---:|---|---:|---|
| `hermix-tools` | 52,144 | **TRIM + DEL** | **−33,000** | 内核留 file/terminal/exec/delegate + registry/approval ≈19,000；browser(1361)+cdp(688)+office(1320)+storage(1763)+todo/task(2460)+integrations(3735) 等 **DEL** |
| `hermix-intelligence` | 16,521 | **DEL** | **−13,600** | 只留 `model_metadata`/`auxiliary`/`prefetch` ≈2,900；`tool_router`(1262)、`session_insights`(1300+)、`usage_pricing`(664)、`credential_pool`(535)、`thinking`(509) **零外部引用，直接删** |
| `hermix-focus` | 8,204 | **KEEP→TRIM**（改判，见 §2.2.1） | **−3,000** | 引用面 = `hermix-agent` 的 11 个源文件。**但深挖后发现它是一套完整的分层记忆系统**（`embedded/` 4,607 行本地向量+CJK 分词、`fact_extractor` 642、`time_aware_recall` 560、`memory_layer` 455、`user_style` 302、`salience` 223、`focus_stack` 694），**与 mem0 同构且更细**，删掉等于放弃中文优化的时间感知记忆 |
| `hermix-plugin` | 7,450 | **DEL / MOVE** | **−7,450** | 实为 custom app（hx-shell + WebView2），**不是插件内核**。与真正插件机制（`native_plugin.rs` 414 + `plugins.rs` 952）无关 |
| `hermix-cua` | 4,980 | **MOVE**（改判，见下方修正） | **−6,900** | 引用面实测 19 文件：自身 8 + `tools/cua.rs`(1933) + `tools/backends/cua_backend.rs` + `server/dashboard/cua.rs` + `src-tauri/commands_cua/`(17 个 tauri command) + 3 处 Cargo.toml。前端仅 `ToolCallBubble.tsx` 约 30 行按 `cua_*` 前缀渲染的分支 |
| `hermix-skills` | 2,650 | **TRIM** | **−2,000** | 留 loader ≈650；hub/validator/version 删，格式改抄开放标准 |
| **小计** | | | **−65,950** | focus 改判后由 −70,650 上修 |

**回答主人的追问：cua 和 focus 不是"没法砍"，是我上一版没写清档位。focus 可砍到 ~500。**

#### ⚠️ cua 档位改判（2026-09-24 深挖后自我推翻）

上一版把 `hermix-cua` 判为 **DEL（可砍到 0）是错的**。深挖模块后发现它不是"可有可无的 RPA 玩具"，而是**Windows 原生深水区能力的唯一实现**，且质量高于同类开源方案的底层：

| 模块 | 行数 | 能力 | 开源同类是否更优 |
|---|---:|---|---|
| `capture/dxgi.rs` | 316 | **Desktop Duplication API 截图**，注释自证比 BitBlt 快 5–10×，且**自动回退 BitBlt**（单块坏屏不砖）。⚠️ **多屏未实现**：`CaptureConfig.monitor` 被忽略，永远只截主屏（实测 2026-09-24，见 §3.2.5.1） | ❌ UI-TARS / Agent S2 都用 Pillow/pyautogui 截图；多屏处理双方都不完整 |
| `uia.rs` | 880 | **UI Automation 语义树**（`ControlType` 枚举、`UiaCondition` 过滤、`UiElement` 查询） | ❌ 开源同类几乎全靠**纯截图标坐标**猜，UIA 是降维打击 |
| `ocr.rs` | 273 | 本地 OCR 兜底 | ⚠️ 与 UI-TARS 的 VLM 定位互补 |
| `action/sendinput.rs` | 406 | `SendInput` 键鼠注入 | ≈ 等价 |
| `safety.rs` | 303 | `ApprovalMode` / `RiskLevel` / 按键黑名单守卫 | ❌ 开源同类基本没有 |
| `trace.rs` | 250 | **结构化轨迹录制与回放**（`TraceKind`/`TraceRecorder`/JSON 渲染） | ❌ 只有 trycua/cua 的 bench 有类似物 |
| `browser/*`（CDP） | 1,189 | 浏览器 CDP 直控 | ≈ Playwright MCP 等价 |

**结论：删 = 从零重写 DXGI + UIA + SendInput + OCR，成本远超 6,900 行，且不可能写得比现在更好。**

改判为 **MOVE**：移出主仓到 `hermix-contrib` 独立 repo，作为 **`Desktop` seam 的默认 provider 插件**。

- **主仓行数真减 6,900**（满足瘦身目标）
- **代码不删**（保住 Windows 原生资产，需要时 clone 挂载即恢复）
- **内核只加 `Desktop` seam ≈600 行**（定义 screenshot / a11y_tree / input / window_list / ocr 五个能力面）

### 2.3 P2 内核重写瘦身

| 目标 | 现行数 | 目标 | 说明 |
|---|---:|---:|---|
| `hermix-agent` | 73,259 | ~15,000 | `local_agent_service.rs` 4,364 行单体 → agent loop 作为**插件**实现 `Agent` trait，内核只留调度 |
| `hermix-server` | 21,332 | ~6,000 | ws/state/approval 重写为 Session 投影 + Transport seam |
| 新增 `hermix-store` | 0 | ~2,000 | 41 处裸 `Connection::open` 收口 |
| 新增 `hermix-session` | 0 | ~2,500 | C2 |
| 新增 `hermix-context` | 0 | ~800 | C1 |
| **内核总量** | | **~30,000** | |

### 2.4 结果

```
112,337  ──P0 砍死重──▶  82,610  ──P1 外置插件──▶  16,660  ──P2 内核重写──▶  ~30,000
```

**内核 ~3 万行（可独立运行、可审计），业务按 profile 挂载。** 前端 ≤300 行适配（契约冻结，见 §4）。

> ⚠️ **账目待重核**：P1 段 16,660 与 P2 段 30,000 不自洽（P2 会新增 store/session/context ≈5,300 行，且 `hermix-agent` 73,259 + `hermix-tools` 52,144 之和已超过总量 112,337，说明之前的 crate LOC 统计口径不一致）。**F0 开工时用统一口径实测重算一次**，此处数字只用于表达量级与方向，不作为承诺。

---

## 3. 能抄就抄 + 开源套件集成

### 3.0 扩展机制定位（主人 2026-09-24 明确）

> "我不想用 MCP，我希望是插件形式。MCP 功能可以有，是以后需要的情况下用户自己去配置连接的。默认是没有 MCP。"

**插件是唯一一等扩展机制；MCP 不是默认机制。**

- **默认 profile 不含 MCP**：不编译、不启动、不注入任何 `server__tool`。
- MCP 降级为**一种插件实现**（`mcp-bridge` 插件）：用户显式配置 `mcp_servers.json` 才激活；未配置时该插件不存在，零成本。
- 内核只认 `Tools` seam，**不认 MCP**。`hermix-mcp`（3,584 行）从"server 的内核依赖"降为"可选插件"，不计入默认二进制。
- 插件形态优先级：**进程内 Rust 插件**（性能/权限可控）> **外部进程插件**（隔离）> **MCP bridge**（兼容生态，默认关）。

### 3.1 抄 / 接 对照表

| 能力 | 现有自研 | 行数 | 抄 or 接 | 来源 | 形态 |
|---|---|---:|---|---|---|
| **核心文件工具** | `file.rs` | 1,266 | **抄** | **pi-coding-agent 内置 read/write/edit/grep/find/ls** | 内核 |
| **代码编辑** | `patch` | — | **抄** | **Aider search/replace block**（比整文件重写省 token、冲突少） | 内核 |
| 代码搜索 | `search_files` | — | 抄（已在用） | ripgrep | 内核 |
| 文档转换 | `markitdown`+`file_converter`+`pdf` | 1,240 | 抄 | microsoft/markitdown（主人已有 `markitdown-rs`） | 插件 |
| Office/PPT | `office_cli`+`create_presentation` | 1,320 | 接 | python-pptx / docx 脚本 | 插件 |
| 浏览器自动化 | `browser`+`cdp_bootstrap` | 2,049 | 接 | Playwright MCP / chrome-devtools-mcp | 插件（默认关） |
| **桌面 UI 操作** | `hermix-cua`+`cua.rs` | 6,913 | **MOVE + 抄上层策略** | 驱动 MOVE 到 contrib；**智能体策略抄 Agent S2 分层规划 + UI-TARS 动作空间**（详见 §3.2） | 插件（默认关） |
| 记忆 | `hermix-focus` | 8,204 | 抄 | 极简 SQLite FTS5 + 可选 embedding（或 mem0） | 插件 |
| **技能加载** | `hermix-skills` | 2,650 | **抄格式** | **pi 的 SKILL.md（frontmatter + 正文）/ agentskills.io 开放标准** | 内核接缝 |
| 调度 | `cron`+`automation`+`task` | 5,477 | — | 三套合一，`tokio-cron` | 插件 |
| 语音 | `voice`+`voice_mode` | 1,542 | 接 | whisper.cpp / 云端 ASR | 插件 |
| 终端沙箱 | `TerminalBackendType` | 已存在 | 抄 | pi Gondolin / Anthropic sandbox-runtime / WSL2 | 内核接缝 |
| 图像/视频 | `image_gen`+`video_gen`+`tts` | 658 | 接 | 本来就是外部 API | 插件 |

**抄的部分 ≈ 3 万行自研被替换；DEL 的部分 ≈ 7 万行直接消失。**

### 3.2 桌面操作（Computer Use）能力：开源调研与选型

> 主人 2026-09-24：「还有一个类似 codex 的电脑操作功能，找找有没有优秀的开源项目可以抄。」
>
> 先把概念拆清楚——**这是两件事，别混着抄**：
>
> | | **Codex 式**（命令执行 + 沙箱） | **Computer Use 式**（看屏幕 + 动鼠标） |
> |---|---|---|
> | 作用面 | 进程/文件系统/网络 | 图形界面、像素、控件树 |
> | 代表 | `openai/codex`（codex-rs） | UI-TARS / Agent S2 / trycua |
> | Hermix 现状 | `terminal` 工具 + `TerminalBackendType` | `hermix-cua`（已实现，且质量高于同类底层） |

#### 3.2.1 开源全景（实测，非印象）

| 项目 | Star | 语言/协议 | 它是什么 | 对 Hermix 的价值 |
|---|---:|---|---|---|
| **`openai/codex`** | 97k+ | **Rust**（2026 已重写，60+ crate） | CLI 编码 agent；`codex-core` 是 library，多 UI 复用 | ⭐⭐⭐⭐⭐ **架构范式直接照抄**（见 3.2.2） |
| `bytedance/UI-TARS-desktop` | 31.4k | TypeScript / Apache-2.0 / Electron | 截图→VLM→动作→执行的**单循环**；依赖 UI-TARS-2B/7B/72B 或 Seed-1.5-VL | ⭐⭐⭐ **动作空间 + 循环结构照抄**；模型要 GPU，与我们 Rust/无 GPU 场景不合 |
| `simular-ai/Agent-S` (S2/S2.5) | 12.3k | Python / Apache-2.0 | OSWorld-Verified 100 步 **56.0% SOTA**；Mixture-of-Grounding + Proactive Hierarchical Planning | ⭐⭐⭐⭐⭐ **上层策略照抄**（这是"聪明"的部分） |
| `trycua/cua` | 18.1k | 混合 | Cua Drivers（Win/Mac/Linux 后台操控）+ Lume（Apple Silicon VM）+ Cua-Bench（OSWorld/ScreenSpot/**Windows Arena**） | ⭐⭐⭐ **Windows Arena 评测 + 沙箱评测体系**可借鉴 |
| `uiautomation` crate | — | Rust / Apache-2.0 / 9K SLoC | Windows UI Automation 封装（元素树、matcher、send_keys、control patterns、screenshot feature） | ⭐⭐ 比 `hermix-cua/uia.rs` 通用，但**我们已有更贴合的实现**，不需替换 |

#### 3.2.2 Codex 范式：三个必须抄的架构决策

**① 沙箱与审批是两个独立旋钮**（这是对我 §0.1 论断的工业级验证）：

> Two dials, not one … **Sandbox mode** — what a command is *physically capable of*, once it is running. Enforced by the kernel. The model cannot argue with it. **Approval policy** — what happens when the agent wants to do something the sandbox would block: *ask you*, or *refuse and feed the failure back to the model*.

三个 sandbox 档：`read-only` / `workspace-write` / `danger-full-access`。关键细节：

- `read-only` **不是"不能跑命令"**：Seatbelt 策略 `(deny default)` 起手，但显式 `(allow process-exec)` `(allow process-fork)`——**只读的 Codex 依然能全速探索仓库，只是留不下痕迹**
- `workspace-write` 下 `.git` / `.agents` / `.codex` 三个元数据目录**额外保护**，理由是提权而非整洁：**可写的 `.git/hooks` = 可写的任意代码执行路径**
- Windows 实现：`CreateRestrictedToken`（`DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED` + capability SID）降权 + **ACL/DACL** 管文件系统 + **WFP（Windows Filtering Platform）** 硬断网络

→ **这三条直接落地为 Hermix 的 `Sandbox` seam 默认 provider**（§5.2 已列 Windows 四层，此处补 WFP 与元数据目录保护两项）。

**② 网络管控不靠防火墙，靠环回代理**：沙箱内只放行 loopback，注入 `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` 强制走代理；代理按**域名白名单逐连接裁决（deny 优先）**，并拦截非公网 IP 直连。跨平台一致，**不需要 root / 不改宿主机防火墙**。

→ Windows 上等于绕开"改 iptables 不现实"的问题，**这是我们能直接复用的最省事的一条**。

**③ `codex-core` 是 library，UI 只是消费者**：`tui` / `exec` / `app-server`（JSON-RPC，供 IDE 与桌面 app 集成）/ `cli` 四个前端，**共用同一个 core**。另有 `state` crate 专门做 **SQLite -backed session/thread 持久化**。

→ 这条正是我们"内核 + 多展示端（桌面/手机）"的同构范式，且**它的 `app-server` 走 Unix domain socket**——而我们要求走 TLS + 配对（见 `FOUNDATION_REWRITE_DESIGN.md §网络安全`），这是我们要比它更严的地方。

#### 3.2.3 分层结论：底层不抄，上层照抄

| 层 | 谁来做 | 依据 |
|---|---|---|
| **L1 驱动层**（截图 / 控件树 / 键鼠注入 / OCR / CDP） | **用自研 `hermix-cua`（MOVE 到 contrib）** | `uia.rs` 语义树（实测 77 元素、中文名正确）、`trace.rs` 回放——**两项同类开源确实没有**，删了抄不回来。⚠️ 截图只支持主屏 |
| **L2 动作空间**（click / double_click / right_click / drag / type / key / scroll / wait / screenshot 的统一 schema） | **照抄 UI-TARS** | 事实标准，9 个动作覆盖全部 GUI 操作；我们只需把 `sendinput.rs` 的能力映射成这套 schema |
| **L3 定位（Grounding）** | **抄 Agent S2 的 Mixture-of-Grounding** | 通用模型负责"要做什么"，**专用 grounding 模型负责"点哪个像素"**；UIA 命中时优先用 UIA（零成本），未命中才降级 VLM/OCR——**这条路只有我们有 UIA，开源同类做不到** |
| **L4 规划** | **抄 Agent S2 的 Proactive Hierarchical Planning** | 多时间尺度动态重规划，**不等失败后才调整**；这是 S2 打过 SOTA 的核心 |
| **L5 内核接缝** | **新增 `Desktop` seam ≈600 行** | 只定义 5 个能力面：`screenshot` / `a11y_tree` / `input` / `window_list` / `ocr` |

**净效果**：电脑操作能力**不进内核**，默认 profile 不编译；启用时 = `Desktop` seam + contrib 里的 cua 驱动 + 抄来的 L2-L4 策略插件。

#### 3.2.5 诚实交代：CUA 到底能不能打（主人 2026-09-24 追问）

> 主人：「CUA 这玩意跟开源的有的比吗？我从来没用过。」

#### 3.2.5.1 实测结果（2026-09-24 02:44，`cargo run -p hermix-cua --example cua_smoke`）

探针脚本留在 `crates/hermix-cua/examples/cua_smoke.rs`（只打印元数据，**不落盘截图**）。实机输出：

```
[MONITORS]   count=1  idx=0 primary 1920x1080 @(0,0) primary=true
[SCREENSHOT] ok 1920x1080 monitor=0 name=primary png_bytes=666372 b64_len=888496
[UIA]        ok elements=77
[UIA]          depth=0 Pane    name="桌面 1"          class="#32769"
[UIA]          depth=1 Pane    name="任务栏"           class="Shell_TrayWnd"
[UIA]          depth=2 Button  name="开始"             class="Start"
[UIA]          depth=2 Button  name="在此键入进行搜索"  class="TrayButton"
[UIA]          depth=3 ToolBar name="语言栏"           class="CiceroUIWndFrame"
[WINDOWS]     ok count=531
```

**结论：驱动层不是纸面资产，感知层实测可用。** 三条硬证据：

| 验证项 | 结果 | 说明 |
|---|---|---|
| 截图真出图 | ✅ 666,372 字节 PNG | 1920×1080 全黑/空白帧只会是几 KB，666 KB 说明是真实桌面内容 |
| UIA 控件树非空 | ✅ 77 个元素 | 且**中文名全部正确**（"开始"/"任务栏"/"语言栏"/"在此键入进行搜索"）——CJK 路径无乱码 |
| 窗口枚举 | ✅ 531 个 | `element::enumerate_windows` 正常 |

**但同时推翻了我上一轮的两个说法（必须修正）：**

1. **「DXGI 多显示器正确」是错的。** `capture/mod.rs:73` 注释原文：`pub monitor: usize` → *"Currently ignored — the primary monitor is always captured"*；`ScreenshotResult.monitor` → *"currently always 0"*。**多屏 capture 根本没实现**——`screen_info::enumerate()` 能列出多屏，但 `screenshot()` 不用它。实测机只有 1 块屏，这条也无从验证。所以 §2.2/§3.2 里"多显示器正确"的表述要划掉，**这是未实现项，不是优势**。
2. **每轮截图开销比我估的严重得多。** 单帧 666 KB PNG / **888 KB base64**。原估"≈1000+ token"只算了模型侧，没算传输侧——手机端远程场景下每轮 888 KB 上行是硬伤（这正是 §9 传输层要 seq 对账 + 差分的理由）。

**第 3 步（键鼠动作闭环）本次未跑**——它会真的打开记事本并注入按键，干扰当前桌面。动作层是 `SendInput` 标准 Win32 API，风险低于感知层，但**"能感知"≠"能操作"**，启用电脑操作前必须补这一步。

**档位据此定为 MOVE（不是 DEL，也不是"更强"）**：驱动层可用已证实，但优势只有「UIA 语义树 + trace 回放」两项，「多屏」那条撤回。

### 3.3 记忆系统：开源调研 + 咱家家底（主人追问"短期的、长期的、总结的"）

#### 3.3.1 开源全景（2026 现状）

| 方案 | 定位 | 数据模型 | Star | Lock-in | Token 效率 | 可审计性 |
|---|---|---|---|---|---|---|
| **mem0** | Memory service，**drop-in** | 向量 + 图，**三层分区**（User / Session / Agent） | 65k+ | **低（改 3 个调用点）** | 高（~7K/次召回） | 高（规则显式） |
| **Letta（原 MemGPT）** | **完整 stateful agent runtime** | 虚拟内存分页：Core / Recall / Archival | 25k+ | **高（需重建 loop，2–6 周）** | 低（每次操作都耗推理 token） | 低（agent 决策不透明） |
| **Zep / Graphiti** | 时序知识图引擎 | **Bi-temporal 图**，矛盾时**边失效而非硬删** | 30k+ | 中 | 极低（600K+ footprint） | 高（全留痕） |
| Cognee | graph-native | 实体关系图，可嵌入默认启动 | — | 中 | 中 | 中 |

**几个必须抄下来的工程实践**（业界共识，与选谁无关）：

1. **记忆抽取必须与响应关键路径解耦**——绝不能在生成回复时同步跑图抽取；流式返回后丢后台队列异步处理
2. **注入 token 硬预算**（200–500 tokens 上限），否则记忆会吃掉推理空间
3. **冷存储压缩**——60–90 天未激活的 episodic 节点压成摘要嵌入
4. **防记忆投毒**——外部输入 sanitize 后才能写持久记忆（防间接 prompt injection 打穿长期状态）

#### 3.3.2 咱家家底（第二次自我推翻：focus 不该砍）

深挖 `hermix-focus` 后发现它**不是"打分器"，是一套完整分层记忆系统**：

| 模块 | 行数 | 对应能力 | mem0 有没有 |
|---|---:|---|---|
| `embedded/`（store/mod/retrieve/embed/cjk/score） | 4,607 | **本地向量存储与检索**，含 **CJK 中文分词** | ❌ mem0 无中文分词优化 |
| `focus_stack.rs` | 694 | **短期/工作记忆**（焦点栈） | ≈ Session-level |
| `memory_layer.rs` | 455 | **记忆分层** | ≈ 三层分区 |
| `fact_extractor.rs` | 642 | **事实抽取** | ✅ mem0 的自动提取 |
| `time_aware_recall.rs` + `time_recall.rs` | 1,118 | **时间感知召回** | ❌ 仅 Graphiti 有 bi-temporal |
| `user_style.rs` | 302 | 用户画像 | ✅ User-level |
| `salience.rs` | 223 | 显著性评分 | ❌ 自研独有 |
| `memory_router.rs` | 726 | 召回路由 | — |

工具层另有三件：`memory.rs`（MEMORY.md / USER.md 持久记忆）、`daily_memory.rs`（**每日工作日志**，SQLite，prompt 里强制 agent 记目标/进展/结论）、`session_search.rs`（FTS5 会话搜索）。

**结论：与 mem0 同构，且在中文分词、时间感知、显著性三个点更细。** 删掉 = 放弃中文优化的时间感知记忆系统。

**档位改判 DEL→500 为 KEEP→TRIM**：保留 `Memory` seam + 上述核心，TRIM 掉与 mem0 重复且无本地优势的部分（约 −3,000 行）。

#### 3.3.3 建议：不换，补三点

不接 mem0/Letta——**lock-in 与重复建设都不划算**（Letta 要求重建 loop，与我们 AgentLoop 插件化冲突）。只需补三点，都是 mem0 的成熟实践：

1. **异步化**：现在 `fact_extractor` / `time_aware_recall` 是否在响应关键路径上同步跑？若在，**移到后台**——这是 mem0/Graphiti 都强调的头号实践
2. **注入预算硬上限**：召回结果入 prompt 前卡 200–500 token 上限（现在是否有上限需核实）
3. **记忆写入 sanitize**：防止间接 prompt injection 写脏长期记忆

**可选 provider 挂在 `Memory` seam 上**：默认 `FocusMemory`（自研），可选 `mem0`（需 Python 运行时，见 §3.4）、`Fts5Lite`（极简）、`Off`。

### 3.4 Python 运行时：有插槽，未内置（主人追问）

> 主人：「现在有没有内置 python 运行时？自己写 python 脚本可以自己跑吗？」

**直接回答：没有内置，但能跑——前提是用户机器装了 Python。**

`crates/hermix-tools/src/backends/code_execution.rs:1-24` 已实现**三级解析**，最高优先在前：

| 优先级 | 来源 | 路径 |
|---|---|---|
| 1 | **显式 override** | `<hermix_home>/python_override.txt` 或 `HERMIX_PYTHON` 环境变量 |
| 2 | **sidecar 兄弟目录**（**内置插槽**） | `<dir_of_hermix>/python/python.exe`（Win）／`python/bin/python3`（Unix） |
| 3 | **PATH 探测** | Windows `python → py → python3`；Unix `python3 → python` |

源码注释自陈决策理由：

> We deliberately do **NOT** auto-bundle Python by default (the bundled interpreter is ~30MB and most users have Python already). The sibling slot is reserved so the installer can opt into bundling later via `tauri.conf.json::bundle.resources` + the `BUNDLED_PYTHON` flag in `build-tauri.bat`.

**实测：`BUNDLED_PYTHON` 在 `tauri.conf.json` 与构建脚本中均不存在 → 插槽预留了，但从没启用过。**

所以：
- **能自己写 Python 脚本跑吗？** 能——通过 `code_execution` 工具（或 `terminal`），**但依赖系统 Python**
- **能内置吗？** 能，成本很低：`python-build-standalone` 解压到 `<dir_of_hermix>/python/` 即可，**代码路径已通，只差打包脚本**（注释说 "single `tar -x`"），代价 **+30MB 体积**

**决策点**：内置与否直接决定 §3.1 那批 Python 套件（mem0 / markitdown / python-pptx / whisper.cpp 生态）能不能"接"，还是只能"抄成 Rust"。**建议默认不内置**（多数开发者机器有 Python，30MB 换不到刚需），把 `BUNDLED_PYTHON` 做成安装器可选项。

#### 3.2.4 一条重要提醒：Computer Use 的代价

OSWorld 上最好的 agent（Agent S2.5，56%）**距人类仍有约 40 个百分点差距**。每一轮 GUI 操作都要付一次全屏截图的 token（1920×1080 图 ≈ 1000+ token）。

**所以默认 profile 不带电脑操作**，只在用户显式开启时挂载——这既省体积，也避免模型在能用 `terminal` 解决时误入高成本、低成功率的 GUI 路径。

---

## 4. 前端契约冻结

沿用 `FOUNDATION_REWRITE_DESIGN.md §6`：35 出站帧 + 5 入站帧 + 全部 REST 端点 + `WsEnvelope` 形状全保，适配层在服务端边界做投影。

**唯一新增**：cua 若外置，`ToolCallBubble.tsx:1108-1138` 的 8 个 `cua_*` 预览组件需改为通用预览兜底（约 40 行）。

---

## 5. 安全与边界（回答"PI 没审批为啥能跑"的落地版）

### 5.1 授权模型切换

| 层 | 行为 | 触发条件 |
|---|---|---|
| L1 边界 | 启动时划定一次 | profile 决定 `Shell`/`Fs`/`Sandbox` provider |
| L2 边界内 | **全放行**：写/读/编辑/跑脚本/跑命令 | 默认，不弹卡 |
| L3 越界 | **硬拒**或**极少数仍需人** | 出工作区、提权、拉远程脚本执行、开网络监听、远程来源的高危调用 |

**审批卡从"每次都问"退化为"只在越界时问"**，频次降一个量级，且多数越界是硬拒（不阻塞）。

### 5.2 Windows 边界四层（无 Docker 依赖）

1. **受限令牌** `CreateRestrictedToken`：去 Administrators SID、加 deny-only SID、去 `SeDebugPrivilege`
2. **Job Object**：`LIMIT_KILL_ON_JOB_CLOSE` + 内存/CPU/进程数上限（项目已有手写实现，见 sidecar 清理）
3. **低完整性级别 / AppContainer**：写只允许指定目录
4. **可选 WSL2 后端**：真正的 Linux 边界，等价于 Pi 的 Gondolin；`TerminalBackendType` 已有扩展点

### 5.3 现有资产复用

- `crates/hermix-config/src/config.rs:675` `TerminalBackendType { Local, Docker, Ssh, Daytona, Modal, Singularity }` —— **Gondolin 模式已实现一半**，默认 `Local`
- `config.rs:718` 默认容器名 `hermix-sandbox`
- `hermix-agent/src/plugins.rs:421` `PluginSandbox`（当前 `#356` 是 "No sandbox = all permissions granted"，需收紧为默认最小权限）

### 5.4 网络安全

沿用 v2 的五层：暴露面默认关闭 → TLS1.3 + 二维码指纹固定 → 设备配对(TTL 120s) → 凭证层次 → **来源感知权限**（远程来源的高危工具强制桌面端人工确认，不可被"本次会话总是允许"覆盖）。作为 `Transport` seam 的一部分实现。

---

## 6. 迁移路径

| 阶段 | 内容 | 可独立交付 |
|---|---|---|
| **F0** | 砍 `gateway`+`acp`（-29,727 行） | ✅ 零风险，立刻可验 |
| **F1** | `hermix-context`（C1）+ `hermix-store` + `hermix-session`（C2） | ✅ |
| **F2** | Seams（C3）：把 tools/memory/skills/mcp 上提为接缝 | ✅ |
| **F3** | Boot（C4）+ profile 组合，首批插件外置（tools 非核心 33K） | ✅ |
| **F4** | agent loop 改插件实现，拆解 `local_agent_service.rs` | 风险最高 |
| **F5** | 边界模型替换逐命令审批 + Windows 四层边界 | ✅ |
| **F6** | `Transport` seam + 手机端配对 | ✅ |

F0-F3 解决 80% 的实际症状（卡住/孤儿/重复）；F4 单独评估。

---

## 7. 待主人拍板

| # | 决策 | 建议 |
|---|---|---|
| **D1** | 内核范围 | **先做 F0-F3**，F4-F6 后续定 |
| **D2** | Cordis 是否自实现 | **自实现最小版 ~800 行**（Rust 无对应物；引入 TS 运行时自毁） |
| **D3** | cua 去留 | 主人已说砍。建议**外置为插件**（留可挂载），而非删源码 |
| **D4** | focus 去留 | 外置；`Memory` seam 默认 provider 用 SQLite FTS5 精简版 |
| **D5** | 审批模型切换时机 | **F5 做**，与边界模型同批（否则白做两遍） |
| **D6** | agent loop 是否改插件 | F4 单独评估，风险最高 |

---

## 8. 对抗式失败模式

| 失败模式 | 触发场景 | 缓解 |
|---|---|---|
| **效果泄漏** | 插件 Drop 未注销服务 → 悬垂引用 | guard 必须在 `Drop` 断言；内核提供 `ctx.assert_clean()` 测试钩子 |
| **接缝三角色不全** | 只写 provider 没写 definition → 换不了 | CI 检查每个 seam 目录必须含 `def.rs`/`provider.rs`/`consumer.rs` |
| **外置插件失联** | MCP server 挂了 → 工具消失但 prompt 还在 | 工具 schema 随注册/注销同步；prompt 由 registry 派生，不缓存 |
| **边界绕过** | 插件直调 `std::fs` 绕过 `Fs` seam | 内核禁止插件 crate 直接依赖 `std::fs` 的路径写（lint 规则） |
| **迁移期双写** | 新旧 registry 并存 → 状态分叉 | **每阶段完成即删旧路径**，不并行 |
| **日志膨胀** | append-only 无压缩 | 世代文件 + zstd；`session_insights` 类派生数据不入日志 |
| **profile 组合爆炸** | patch 层层覆盖难推理 | `dsh --dump-config` 式的 `--dump-profile` 命令，必须可打印最终树 |

---

## 9. Windows 命令执行层（主人痛点：命令反复重试、错误类型杂）

### 9.1 现状病灶（带行号）

1. **`HERMIX_CONFIRM=1` 重试诱导** — `crates/hermix-tools/src/tools/terminal.rs:485-495`：命令被安全策略拦下时，返回给模型的错误里写着「告诉用户并请求许可，批准后用 `HERMIX_CONFIRM=1 <cmd>` 重试」。模型看到"可以这样重试"，就会**循环重试**。这是反复尝试的直接来源之一。
2. **裸 stderr 回灌** — 失败时把原始 stderr 丢回模型。Windows 下同一类失败有 N 种面：

   | 现象 | 实际类别 |
   |---|---|
   | exit 9009 / `'xxx' is not recognized` | 命令不存在 |
   | stderr 是 GBK 乱码 | 编码问题，重试无意义 |
   | exit 5 / `Access is denied` | 权限，需越界判定 |
   | `The process cannot access the file because it is being used` | 文件被占用 |
   | PowerShell 语法喂给了 `cmd.exe` | shell 选错 |

   模型分不清"该改参数"还是"该换 shell"，只能盲试 → 多轮重试 → 每轮都付 token。

3. **shell 不确定** — 同一条命令在 `cmd.exe` / PowerShell / Git Bash 下行为不同，未显式指定时靠猜。

### 9.2 底座要做的三件事

**(1) 错误分类前置（内核做，不交给模型猜）**

Windows 特有失败映射为**结构化错误码 + 给模型的可执行指令**：

| 错误码 | 判据 | 给模型的指令 |
|---|---|---|
| `COMMAND_NOT_FOUND` | exit 9009 / `not recognized` | 换命令或换 shell，**禁止重试同一条** |
| `ENCODING_MISMATCH` | stderr 解码失败 | 已代为解码；重试无意义 |
| `ACCESS_DENIED` | exit 5 / `Access is denied` | 属越界 → 硬拒，不重试 |
| `FILE_LOCKED` | `being used by another process` | 换路径或稍后，**限一次** |
| `SHELL_SYNTAX` | shell 与语法不匹配 | 指定 shell 重发，**只允许一次** |
| `TIMEOUT` | 超时 | 拆小或转后台任务 |

**(2) 重试纪律内核化**

同一 `(command, workdir, shell)` 的失败在内核记一次**指纹**；相同指纹再次出现时直接回 `DO_NOT_RETRY` + 原因，不再执行、不再消耗一轮模型。

> 判据：LLM 重试之所以失控，是因为它拿不到"这次失败是确定性还是偶发"的判据。内核有这个信息，就该由内核给。

**(3) shell 选择确定性**

工具参数显式带 `shell: cmd | powershell | bash`，不允许默认值靠猜。Windows 默认 `cmd`，Git Bash 只在用户显式启用时存在。

### 9.3 可抄与已有资产

- `crates/hermix-shell-parser`（307 行，已有）保留做命令切分
- `crates/hermix-tools/src/approval/engine.rs` 的 `DENIED_PATTERNS` / `CONFIRM_PATTERNS` 改由 `Sandbox` seam 承接（见 §5.2），**不再靠"报错让模型重试"来兜底**
- pi-coding-agent 的 `bash` 工具错误处理语义
- **`codex-rs` 的三档沙箱语义**（`read-only` / `workspace-write` / `danger-full-access`）——尤其 `.git`/`.agents`/`.codex` 元数据目录保护（理由：可写 `.git/hooks` = 任意代码执行路径）
- **`codex-rs` 的 `network-proxy` 思路**：环回代理 + 域名白名单 deny-优先 + 拦截非公网 IP 直连，**不依赖改宿主机防火墙**，Windows 上可直接落地

---

## 10. 待主人确认（本轮新增）

| # | 问题 | 影响 |
|---|---|---|
| **N1**（已改判） | `hermix-cua` 从「砍到 0」改为「**MOVE 到 contrib 保留为插件**」？ | 主仓仍减 6,900 行，但**代码不删**（DXGI/UIA/SendInput/OCR 是 Windows 深水区，删了重写不回来）。需要时 clone 挂载即恢复 |
| **N1b** | `Desktop` seam（screenshot/a11y_tree/input/window_list/ocr，≈600 行）是否进内核？ | 进 = 电脑操作可作为一等插件；不进 = CUA 只能通过外部进程插件绕，能力打折 |
| **N2** | `hermix-focus` 砍到 0 还是留 ~500 行极简版？ | 砍到 0 = 无长期记忆；留 500 行 = SQLite FTS5 关键词召回 |
| **N3** | `hermix-plugin`（custom app / hx-shell）砍到 0 还是移出主仓？ | 砍到 0 = custom app 功能消失（若有在用用户需先确认） |
| **N4** | 默认 profile 是否含浏览器能力？ | 建议不含（默认只有 file/terminal/exec/delegate） |
| **N5** | `AgentLoop` 接缝是否照 C4 落地（Loop 不能直接提交 / 不能碰 DB·WS / 必须可中断恢复）？ | 落地 = 换交互范式只改一行配置；不落地 = 换范式仍要动 4,364 行单体 |
| **N6** | 电脑操作是否默认关闭？ | 建议关闭——OSWorld SOTA 距人类仍差 ~40 个百分点，且每轮全屏截图 ≈1000+ token，模型容易在能用 `terminal` 时误入高成本低成功率路径 |
| **N0**（✅ 已完成） | 实测 cua | **2/3 通过**（见 §3.2.5.1）：截图真出图 666 KB、UIA 树 77 元素中文名正确、窗口枚举 531。**不是纸面资产 → 档位定为 MOVE**。同时撤回「多屏」优势（未实现）；新增发现：单帧 base64 达 **888 KB** ⇒ 手机端远程每轮开销是硬伤，强化 N6 默认关闭的理由。第 3 步（键鼠闭环）未跑，启用前必须补 |
| **N7** | 记忆系统接 mem0 / Letta，还是保留自研 focus？ | 建议**保留自研**（与 mem0 同构，中文分词+时间感知更细；Letta 要求重建 loop，与 AgentLoop 插件化冲突）。只需补异步化 + 注入预算 + 写入 sanitize 三点 |
| **N8** | Python 运行时是否内置？ | 插槽已预留（`code_execution.rs` 三级解析的"兄弟目录"档，代码已通，只差打包脚本）。内置 = +30MB，换来 §3.1 那批 Python 套件能直接"接"。建议**默认不内置，做成安装器可选项** |
