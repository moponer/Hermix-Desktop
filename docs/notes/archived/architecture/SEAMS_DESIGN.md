# C3 接缝设计（已拍板 S1–S5，已落地）

Archived: 2026-09-30

> 状态：**已落地**。`crates/hermix-core/src/seams.rs` + `seams/local.rs`。
> 这一层定错，后面 ~15 万行业务代码要跟着返工，所以先出签名过目再动手。

## 0. 拍板结果（2026-09-24 产品方"按你建议的全来"）

| # | 决策 | 结果 |
|---|---|---|
| **S1** | 流式 delta 是唯一易失例外 | **接受**，写进 `seams.rs` 模块级不变量，禁止扩大 |
| **S2** | Fs/Shell 合并为一个执行世界 | **合并**，新增 `ExecWorld { fs(), shell() }` |
| **S3** | 接缝放 `hermix-core` | **放 core**，不新建 `hermix-seams` crate |
| **S4** | `ToolHandler` 返回值改结构化 | **改**：新增 `ToolOutput`（text + is_error + attachments）；`execute_structured` 走默认方法桥接，30+ 老工具**零改动**，迁移期内逐个覆盖 |
| **S5** | MCP 作为 `Tools` 的一种实现 | **是**，MCP 不是独立接缝，不在内核留概念 |

## 0.1 现状修正（落地时实测，推翻草案 §1 两处）

草案说"Llm / Memory / Skills 没有 trait"——**错了**。`hermix-core/src/traits.rs`
里三个 trait 早就存在且是真接缝（`agent_loop` 已经用 `Arc<dyn LlmProvider>`）：

| 接缝 | 复用现有 trait | 是否新建 |
|---|---|---|
| Llm | `LlmProvider` | ❌ 不新建 |
| Memory | `MemoryProvider` | ❌ 不新建 |
| Skills | `SkillProvider` | ❌ 不新建 |
| 执行世界（遗留字符串面） | `TerminalBackend` | ⚠️ 迁移期保留，新代码走 `Fs`/`Shell` |
| Fs / Shell / Sandbox / Transport | — | ✅ 新建 |

**不重复定义**优先于"名字好看"：多一个 `Llm` trait 就多一处可以漂移的实现。

---

## 1. 现状映射（实测，非推测）

| Seam | 现状 | 位置 | 结论 |
|---|---|---|---|
| **Tools** | 已有 trait，**仅两个方法** | `hermix-core/src/traits.rs:50` `ToolHandler { execute(params)->Result<String>, schema() }` | ✅ 形状够薄，可直接上提 |
| **Llm** | **没有 trait**——是具体 struct + `chat_completion()`，按 model 名路由 | `hermix-agent/src/provider.rs:922` | ❌ 需从零建 |
| **Fs** | 无统一抽象，散在 30+ 工具各自的 `Backend` trait 里（`PatchBackend`/`SearchBackend`…） | `hermix-tools/src/tools/file.rs:355,532` | ❌ 需建 |
| **Shell** | 后端枚举已存在但默认 `Local` | `hermix-config/src/config.rs:675` `TerminalBackendType {Local,Docker,Ssh,Daytona,Modal,Singularity}` | ⚠️ 有插槽，需提为 seam |
| **Sandbox** | 无独立概念；策略散在 `approval/engine.rs` 的三张正则表里 | `hermix-tools/src/approval/engine.rs:16,66,156` | ❌ 需建 |
| **Memory** | 完整实现但无 seam | `hermix-focus`（8,204 行） | ⚠️ 有实现，缺接口 |
| **Skills** | 有 loader，无 seam | `hermix-skills`（2,650 行） | ⚠️ 同上 |
| **Transport** | 两套并存 | `hermix-transport`（类型）+ `server/transport.rs`（trait） | ⚠️ 需合一 |

**一句话现状**：`ToolHandler` 是唯一现成的好接缝；Llm/Fs/Sandbox 三个核心 seam **根本不存在**。

---

## 2. 八个接缝签名草案

设计原则：**薄**。只定义内核需要的最小接口，不搬现有实现，不预设调用方。

```rust
// ---- 已有，直接上提 ----
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, params: Value) -> Result<String, ToolError>;
    fn schema(&self) -> ToolSchema;
}

// ---- 新建 ----

/// 模型调用。内核只认这个，不认 provider。
#[async_trait]
pub trait Llm: Send + Sync {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError>;
    /// 流式。delta 是唯一允许的易失路径（见 §3 决策 A）。
    fn stream(&self, req: CompletionRequest)
        -> Pin<Box<dyn Stream<Item = Result<StreamDelta, LlmError>> + Send>>;
}

/// 文件世界。与 Shell 共享执行世界（见 §3 决策 B）。
#[async_trait]
pub trait Fs: Send + Sync {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError>;
    async fn write(&self, path: &Path, contents: &[u8]) -> Result<(), FsError>;
    async fn list(&self, path: &Path) -> Result<Vec<DirEntry>, FsError>;
    async fn remove(&self, path: &Path) -> Result<(), FsError>;
}

/// 命令执行世界。
#[async_trait]
pub trait Shell: Send + Sync {
    async fn exec(&self, cmd: &str, cwd: &Path) -> Result<ExecOutput, ShellError>;
    /// 结构化错误码，禁止返回裸 stderr 让模型猜（见 §3 决策 C）。
    fn classify(&self, out: &ExecOutput) -> Option<ShellErrorKind>;
}

/// 能力边界。内核强制，模型无法争辩。
pub trait Sandbox: Send + Sync {
    fn allows(&self, op: &Op) -> Verdict;   // Allow | Deny | Escalate
}

#[async_trait]
pub trait Memory: Send + Sync {
    async fn recall(&self, query: &str, budget_tokens: u32) -> Result<Vec<MemoryItem>, MemoryError>;
    async fn record(&self, item: MemoryItem) -> Result<(), MemoryError>;
}

#[async_trait]
pub trait Skills: Send + Sync {
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError>;
    async fn load(&self, name: &str) -> Result<Skill, SkillError>;
}

/// 对外暴露面。默认关闭（见 §3 决策 D）。
#[async_trait]
pub trait Transport: Send + Sync {
    async fn publish(&self, session: &str, release: &Release) -> Result<(), TransportError>;
}
```

---

## 3. 五个关键决策（每条都有代价，需要你定）

### 决策 A：流式 delta 是否进持久日志？

- **建议：不进**（易失例外）。逐字落盘是性能自杀。
- 代价：这是唯一违反"只有已提交才可观测"的路径。
- 缓解：整条消息完成后按 entry 提交一次；断线时该条消息**明确标记为不完整**，不伪装成完整。

### 决策 B：Fs 与 Shell 是否共享一个"执行世界"？

- **建议：共享**（抄 PI）。一个 provider 同时提供两者。
- 理由：换 provider 指向远程沙箱时，Bash 和文件读写**整体跟着搬走**，不用给每个工具写 fork。
- 代价：本地文件 + 远程 shell 这种混搭组合无法表达。

### 决策 C：Shell 错误是否结构化？

- **建议：必须**。返回 `ShellErrorKind`（`COMMAND_NOT_FOUND`/`ENCODING_MISMATCH`/`ACCESS_DENIED`/`FILE_LOCKED`/`SHELL_SYNTAX`/`TIMEOUT`），每条带"给模型的可执行指令"。
- 现状病灶：`terminal.rs:485` 让模型"用 `HERMIX_CONFIRM=1` 重试" → 诱导循环重试；且裸 stderr 回灌，Windows 同一类失败有 N 种面，模型只能盲试。

### 决策 D：Transport 默认开还是关？

- **建议：默认关**。本地 stdio 不经过它；网络暴露面需要显式启用 + 配对。
- 依据：现有 `security.rs` 只有静态 Bearer key + IP 白名单，远程与本地请求**无差别对待**。

### 决策 E：接缝放哪个 crate？

- **建议**：全部放 `hermix-core`（已是 leaf，被所有 crate 依赖，无循环风险）。
- 备选：新建 `hermix-seams`。代价：多一个 crate，且 core 与 seams 职责重叠。

---

## 4. 待你拍板

| # | 问题 | 我的建议 |
|---|---|---|
| **S1** | 决策 A：接受"流式 delta 是唯一易失例外"吗？ | 接受，但写进不变量文档，禁止扩大 |
| **S2** | 决策 B：Fs/Shell 合并为一个执行世界？ | 合并 |
| **S3** | 决策 E：接缝放 `hermix-core` 还是新建 `hermix-seams`？ | 放 core |
| **S4** | `ToolHandler` 返回值是 `String`（现状）还是改成结构化 `ToolResult`？ | **建议改**——现状丢信息（工具想返回图片/表格只能塞进字符串） |
| **S5** | MCP 是否作为一个 `Tools` 的实现（而非独立 seam）？ | 是，符合你"默认没有 MCP"的要求 |

---

## 5. 落地顺序

- [x] 1. `hermix-core` 加接缝（Fs/Shell/ExecWorld/Sandbox/Transport 新建，Llm/Memory/Skills 复用）→ 编译验证
- [x] 2. 默认 provider：`LocalFs` / `LocalShell` / `LocalExecWorld` / `LocalSandbox` / `NoopTransport` / `InMemoryFs`
- [x] 3. 换 provider 集成测试：`swapping_fs_provider_is_invisible_to_callers`（内存 Fs 替换磁盘 Fs，上层函数不变）
- [x] 4. C4 Boot/profile：`hermix-context::boot` 把接缝装配成进程级 provider 表（9 个测试）
- [ ] 5. 业务侧逐个迁移，**不一次性切换**

## 6. 落地中的两个实测发现

1. **Windows 中文系统上，缺命令的报错会被误判成"编码不匹配"**——
   `from_utf8_lossy` 把 CP936 的"不是内部或外部命令"解码成 U+FFFD。
   修法：`LocalShell` 改用 core 已有的 `console_decode::decode_console_output_owned`，
   解码正确后 `classify` 能命中 `CommandNotFound`，模型拿到的是"换个命令"而不是"编码有问题"。
2. **Transport 默认关闭必须是显式状态而非故障**：`NoopTransport::publish`
   返回 `TransportError::Disabled`，与网络故障的 `Failed` 区分开，避免上层把
   "没开"当"出错"去重试。
