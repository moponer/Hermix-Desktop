# Decision Note: Agent Loop 稳定性与组装层优化方案

Status: implemented — 稳定性 P0/P1、组装层 P2 与能力线 P3 第一/二批均已交付（2026-10-17，见文末执行记录）。后续项（Tauri 内嵌浏览器面板、文档技能包四件套、dashboard stubs 清算等）由 2026-10-18 的两篇笔记接续：`2026-10-18-capability-alignment-audit.md`（能力对齐审计与四批次计划）与 `2026-10-18-mechanism-level-alignment.md`（三方机制级写法对标与抄写清单）。

## Decision

针对"对话不稳、响应慢后自行停止"的主诉，分四阶段改造 agent loop 与周边，**不更换 agent loop 底座**（Codex / ZCode 均只作参考不作引擎）。

### 已确定项（此前沟通拍板）

1. **不换底座**：Codex(codex-rs) / ZCode(zai-org/ZCode) 均不作为 loop 引擎引入；所有改动落在 hermix-agent / hermix-tools / sidecar / Tauri UI 层。
2. **Codex 降级为可选工具后端：砍掉，不做**（YAGNI）。
3. **错误可见化**：LLM 调用最终失败不再走 `graceful_interrupt_result_errored` 伪装成静默中断，向用户注入失败原因（后续一切诊断的前提）。
4. **日志驱动诊断**：日志位于 `~/.hermix/logs/`（server/agent/tools/crash 四类轮转）；crash.log 为空已排除 panic，症状指向软停止路径。
5. **浏览器自动化 = 有头可见面板**：方案一 WebView2 CDP（`CallDevToolsProtocolMethod`）从 Rust host 驱动 Tauri 可见面板 + hermix-tools 新增 browser_backend；spike 半天验证，不通过退方案二（Playwright headed + `channel: msedge`）。
6. **文档/转换 = pandoc sidecar**（html/md↔docx，走 `ensure-sidecar` 打包链）+ 开源文档 skills（docx/pptx/xlsx）接入现有 skills 机制。

### P0 — 稳定性（对齐 ZCode `docs/bigmodel-business-error-retry.md` 与 `agent-tool-relaxed-execution-policy.md` 的已验证设计）

- **错误分类表**：区分瞬时错误（可重试：超时/5xx/连接重置）与终态错误（不可重试：配额/套餐/权限/日限，即使 HTTP 429 也不重试）；`retry-after` 优先于本地退避（≤5min 封顶）。
- **重试安全边界**：已产出用户可见流式输出后不整轮重放（避免重复文本/重复工具调用）。
- **失败不级联**：工具组内失败不阻断后续工具组；基础设施性拒绝（WS 断连 ChannelClosed / 审批 600s 超时 TimedOut）不计入 ErrorLoop 的"全工具错误轮"；收敛硬停在用户侧留下可见说明。

### P1 — 关键路径纪律

- `memory_prefetch` 的 embedding/recognize 裸 await 加超时（2-3s，超时跳过不阻塞 pre-turn）。
- 高压压缩（≥85% LLM 总结路径）执行前向用户发状态事件，消除"无声卡 30s"。
- 流式看门狗参数放宽：`stream_first_chunk_timeout_secs=30` 可配置化，触发后优先重试整轮请求而非直接 finalize。
- `SimilarContent` 去误杀：长文本（>500 词）提高 Jaccard 阈值或改比"新信息增量"，模板句/固定回执不再推过 0.8。

### P2 — 组装层（对齐 ZCode 装配蓝图：组合根唯一、端口倒置、纯函数调度、方法文件拆分）

- `AgentCallbacks`（`agent_helpers.rs` 的 `Box<dyn Fn>` 字段集合）→ trait 化端口。
- `run_loop.rs`（≈2800 行）按阶段拆子模块：准入/预轮/模型步/工具调度/结算/收敛，纯搬代码不改逻辑。
- `tool_dispatch.rs` 的分组/审批判定/超时计算抽成零 IO 纯函数 + 独立测试。
- `FocusStack`/`MemoryRouter` 直焊点 → memory port trait（最后做）。

### P3 — 能力线与方向评估

- 浏览器 spike → browser_backend + Tauri 面板（标签隔离、登录态独立实例、网页文字只作定位不作指令——对齐 ZCode browser-use 设计）。
- pandoc sidecar + 文档 skills。
- 评估项（先不做，留证据再议）：统一 SessionEvent 式事件协议（80 类型判别联合 + EventReducer，大动作）；Goal Mode 式正向完成判定（实据原则：计划/清单/像结论的话≠完成，文件/命令输出/测试结果才算；Stop 续跑上限 3 次的边界要守住，勿倒退回已删除的 nudge filler）；后台子代理 + 只读白名单；hooks 事件面扩展。

### 明确不抄项（ZCode 有但不引入）

- 原型 mixin 装配（JS 特性；Rust 用 mod 拆分等价达成）。
- 插件市场/marketplace 分发体系（当前无第三方生态需求）。
- Bot Channel / 远程开发 / 云端协同（PR 禁区：无多平台分发）。

## Alternatives considered

- **换 Codex 底座**：同是 Rust 但接口不兼容，接入需重写 hermix-server↔agent 全部契约层（审批 oneshot、流式回调、子 agent spawn、30+ 工具注册）；codex-rs 无 crates.io 发布、API 迭代快；其工具面同样没有浏览器/docx 能力，两条腿还得自己装。**击败它的证据**：改写量 >> 6 个定点 bug 的修复量，且与 10 万行目标反向。
- **换 ZCode 底座**：TS/Node monorepo，换底座 = 整个后端重写为 TS；仅 4 commits、接口无稳定性承诺；其浏览器/文档能力来自插件层（browser-use 库、document-skills）而非 loop 底座，换底座对这些能力零贡献。**击败它的证据**：能力在工具层不在底座，我们按同源路径自建即可。
- **Codex 降级为工具后端**（`codex exec` 封装 `*Backend`）：可做但无当前需求，YAGNI 砍掉；证据重现时按 `## Reintroduction conditions` 评估。
- **大爆炸重构 run_loop**（一次性重写成 ZCode 式端口架构）：风险与收益不成比，且与"先修稳定性"的主诉顺序冲突；改为纯搬运式拆文件 + 渐进端口化。
- **全套 SessionEvent 事件协议立即统一**：中等收益大动作，现有 ws broadcast + turn_report 双轨尚可用；先修 `AgentCallbacks` trait 化，事件统一留评估。

## Consequences

- 代码量：P0/P1 净增少量（错误分类表、超时包装），P2 拆文件为中性搬运；总量仍受 10 万行目标约束，符合 ci-gate 单文件 2500 行告警方向。
- 行为变化（用户可见）：LLM 调用失败会**显式报错**而非"正常 Done"；工具失败后更多后续工具会继续执行；部分收敛硬停会带说明。这些是故意的——静默失败是本次主诉的根因。
- 风险：错误分类表误判终态/瞬时边界 → 用 parity fixtures + 单测覆盖分类函数；run_loop 拆文件若夹带逻辑改动会引入回归 → 严格"纯搬运"，每步 `cargo test -p hermix-agent --lib` 验证。
- 放弃的东西：不追求 ZCode 的插件市场与多消费者事件协议（放弃分发生态与前端解耦的部分灵活性），换取当前阶段的最小改动面与稳定性优先。
- Reintroduction conditions（针对被砍的 Codex 工具后端）：当出现"用户明确要求在 hermix 会话内调用 codex 完成编码任务"的真实需求，且可复用现有 `*Backend` 模式在 <300 行内封装时重启评估。

## 执行记录（2026-10-17）

**已交付（P0/P1/P2，全部 `cargo check --workspace --all-targets` 通过、`cargo test -p hermix-agent --lib` 1077 全绿）：**

- **错误可见化**：核查发现已在早前迭代落地（`graceful_interrupt_result_errored` → `finish_reason="error"` + `last_error` → 前端红条；server 侧 `automation_trigger.rs` 已消费）——豁免。
- **错误分类表**：`classify_error` 新增 `ErrorClass::Quota` 终态类（配额/套餐/计费短语，优先于限流判定，即使 HTTP 429 也不重试）；`llm_retry.rs` 将 Quota 归入立即返回分支（不烧重试预算、不试同账号 fallback）。含 3 个单测。
- **失败不级联**：`ToolResult::is_infra_denial()`（core）+ `run_loop/tool_turn.rs` 收敛检测剔除 `timed_out`/`channel_closed` 拒绝——断连/审批超时不再制造 ErrorLoop 硬停。含单测。
- **memory_prefetch 超时**：`embed` 3s / `recognize` 5s 包装，超时降级文本召回，不再裸 await 阻塞每轮 pre-turn。
- **看门狗可配置**：`agent.stream_first_chunk_timeout_secs` / `stream_overall_timeout_secs` 进 `~/.hermix/config.yaml`（serde default 向后兼容），经 `build_agent_config` 桥接。
- **SimilarContent 去误杀**：新增"新词占比 <15%"门槛 + token 标点规范化；模板句/批量回执不再推过 0.8，纯标点复读仍可判。含 2 个回归测试。
- **run_loop 拆分**：`run_loop.rs` 2825 → 1830 行，抽出 `run_loop/{pre_turn,tool_turn,verification}.rs`（纯搬运，方法改 `pub(super)`），全部文件低于 2500 行告警线。
- **调度纯函数化（第一步）**：`tool_dispatch::approval_wait_bounds()` 抽取 + 2 个单测。
- **日志验证**：`~/.hermix/logs/agent.log` 命中 `hit hard timeout` → 空回复 → 重试恢复，确认 P0/P1 修的正是真实停止路径。

**豁免项（核查后已存在）：** 压缩状态事件（`CompactionStatusBanner` + `in_progress` 快照已实现）；空回复重试已扩至 3 次。

**缓议项：** AgentCallbacks trait 化——run_loop 拆分已解决结构瓶颈，30 个访问点 diff 无用户可见收益，违背六步阶梯；多消费者/测试组合需求出现时重启。

**P3 修正：** 探查发现浏览器线已存在（2026-10-13 决策：`os_auto/browser` 经 chromiumoxide CDP 驱动系统 Edge 有头窗口，navigate/snapshot/click/type/screenshot/screencast/a11y 全有）。P3 焦点从"新建 browser_backend"修正为"增强现有线 + Tauri 内嵌可见面板（WebView2 CDP）"。

## P3 增量（2026-10-17 第二批，`cargo test -p hermix-tools --lib` 753 全绿、workspace check 通过）

- **browser_wait**：CSS 选择器轮询等待（200ms 间隔、默认 5s/上限 30s），修 SPA/懒加载页"导航后立刻快照拿到半渲染页"的主诉；风险分级归 Safe（只读）。
- **browser_scroll**：`by` 像素 / `target` top|bottom / `ref`|`selector` scrollIntoView 四选一，返回滚动位置与剩余高度；折叠下方内容进入快照范围。
- **html_to_docx 工具**：补齐"模型输出 HTML 但要交付 .docx"的转换缺口。手写迷你 HTML 块解析器（h1-h6/p/ul|ol+li/table/blockquote/hr + 内联 strong|em|a + 实体解码含数值实体），经 docx-rs（已在依赖树）产出，**零新依赖**；畸形输入降级为纯文本段落，不静默丢内容。5 个单测。
- **未做（记录在案）**：pandoc sidecar（file_converter 已有 LibreOffice 路径，pandoc 仅当 LibreOffice 缺失成为实际痛点时引入——YAGNI）；Tauri 内嵌浏览器面板（需 shell 进程↔server 进程新通道，独立 spike）。
