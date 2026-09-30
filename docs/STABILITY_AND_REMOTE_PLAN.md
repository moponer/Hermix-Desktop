# 长任务稳定性与能力升级方案

# 长任务稳定性与能力升级方案

> **落地状态（2026-09-28 晚）**：阶段 1 / 2 / 3 已完成。
>
> | 项 | 落点 |
> |---|---|
> | 提示词事实纠错 | directions.rs（web_extract×6、process_registry 删、dead-route 注释×3）、domain_mode.rs（priority_tools/映射/测试全换真实工具名）、tool_meta.rs（patch）、pdf.rs |
> | 「报告必须分节」单一真相源 | 新常量 `REPORT_SECTIONING`，think.full 与 format.short 运行时拼接（省一份 ~700 字拷贝） |
> | AGENTS.md 同步 | 收敛检测硬停阈值、已删 summary guidance、nudge 一次性 |
> | 审批超时可配置 | `ApprovalConfig.timeout_secs`（默认 600，0=永不）→ 进程级全局 `hermix_config::approval_timeout_secs()`；registry / 前端倒计时 / agent 兜底三处对齐；旧"永不超时"行为保留为显式配置 |
> | 24h sleep hack 删除 | tool_dispatch 兜底 = 主超时+60s；HTTP 分支（原裸 await）补同一把网 |
> | watchdog 布防修正 | arm 点从 do_tool_turn 之后移到之前（旧位置 arm→check 之间零 await，结构上不可能触发）；默认 30s→1800s（覆盖审批+工具预算之和） |
> | 工具预算声明制 | 新 `per_tool_budget()`/`batch_budget()`（terminal 600s、browser_* 900s、delegate 1900s、文档 300s、生成类 600s；批=和+120s 封顶 3600s），tool_executor 弃用硬编码 60s/300s |
> | 子 agent 超时 | 300s → 1800s（config 可调） |
> | 验证 | cargo check --workspace ✓；hermix-agent 1062 / hermix-server 198 / hermix-tools 699 / hermix-intelligence 318 全过 |
>
> 未做（按计划继续）：P1-B 流断续跑、P1-C ContextOverflow 压缩重试、P1-D 会话单飞、
> P2 轮间落盘、阶段 5-8（模式接线 / IM 远程）。
> 2026-09-28 · 基于 4 路并行审计（agent loop / 提示词与 skill / 工作模式路由 / IM 远程调研）
> 原则：全部结论有 文件:行号 证据；方案做结构性修复，不做兜底补丁。

---

## 0. 一页结论

**agent loop 的真相**：对"短对话"防御严密（流超时、panic 捕获、去重、收敛检测齐全），
但对"长任务"存在**三个结构性上限**——

1. **时间上限**：前台工具 60s 硬顶（`tool_executor.rs:388`）、整批 300s（`:538`）、
   子 agent 300s（`sub_agent_orchestrator.rs:42`）。任何超过 5 分钟的连续执行路径不存在。
2. **人工依赖**：审批超时 = `Duration::ZERO`（`hermix-server/src/lib.rs:105`）——人不点就永久挂死；
   唯一的兜底是一个 24 小时的 sleep hack；而声称存在的 turn-transition watchdog
   是**死代码**（在循环体末尾上表、下一次迭代顶端就检查，中间没有任何 await，
   `elapsed > 30s` 永假）。
3. **中断不可续**：流断 = 整轮 run 结束（`run_loop.rs:1887-1936`）；进程崩溃 = 任务全丢
   （落盘点只有用户消息 early-persist 和 run 结束后的最终 persist，轮间无增量落盘）。

**提示词的真相**：满配 ~20-35K token/首回合，其中工具 schema 占大头；存在**引用不存在的
工具**（web_fetch、process_registry、copy_file/delete_file/create_directory、patch_file、grep）、
四处语义重复段、AGENTS.md 与代码行为相反的过时记录。

**工作模式的真相**：**不会自动切换——但整套切换引擎已经写好、没有接线**。
DomainModeDetector（Code/Research/Write/Operate/Chat 五域检测 + 域指令文案 +
priority_tools 加权）是完整的死代码；ThinkingMode Quick/Deep 每 turn 自动选择但只用来
显示一个芯片；专家（expert）的 toolset 字段在前端被显式丢弃（`ChatView.tsx:746`）。

**远程的真相**：微信个人号接入 2025-2026 系统性劣化（gewechat 死、padlocal 收费有封号
风险）；**QQ 官方 bot 平台是国内唯一零风险 IM 通道**（C2C 单聊 + 富媒体，个人可注册，
OpenClaw 已有官方插件先例）；最务实的路径是 **Tailscale + 现有 Dashboard**，IM 做轻量
指令与**远程审批**——而这正好依赖第一部分修复的审批队列。

---

## 1. Agent Loop 稳定性

### 1.1 事实清单（按严重度）

| # | 问题 | 证据 | 触发条件 | 影响 |
|---|---|---|---|---|
| S1 | 审批无超时 | `lib.rs:105` ZERO；`tool_dispatch.rs:231-238` 注释自认"blocks HERE forever"；WS 路径唯一兜底是 24h sleep（`:256-258`）；HTTP 路径连兜底都没有（`routes.rs:1472,1900` 传 `shutdown: None` → `:277` 裸 await） | 审批卡丢失/用户离开 | 会话永久挂死；审批串行阻塞（`tool_executor.rs:106` 逐个 resolve），无队列 |
| S2 | watchdog 死代码 | 上表 `run_loop.rs:1569/2459`，检查在 `:1046/:1716` | 任何 | 主循环对卡死零检测，S1 无兜底 |
| S3 | 工具 60s/批 300s | `tool_executor.rs:388,538`（硬编码非配置） | >60s 的前台构建/浏览器任务 | 超时→重试→超时，配合收敛检测（连续 3 轮全错硬停，`convergence_detector.rs:271-286`）演变成整次 run 被杀 |
| S4 | 子 agent 300s | `sub_agent_orchestrator.rs:42,812` | 长子任务 | "派子 agent 干 30 分钟"不可能 |
| S5 | 流断即终止 | `run_loop.rs:1887-1936` Interrupted → return | 一次网络抖动 | 任务中断需手动重发（半截消息的 tool_calls 已正确剥离，无毒历史） |
| S6 | ContextOverflow = Fatal | `agent_helpers.rs:610-618` → `llm_retry.rs:239-241` 不重试 | 逼近真实窗口 | 本地估算与 provider 实际不符时硬错误收场 |
| S7 | 错误分类靠子串匹配 | `agent_helpers.rs:598-657` 默认 Fatal | 未知错误文案 | 一次即终止（重试 3 次 + fallback 已有） |
| S8 | 并发去重按 (session,text) | `ws.rs:168-174,542-556` | 同会话发两条不同消息 | 两个并行 agent、DB 后写者覆盖、interrupt 控制器错乱 |
| S9 | 崩溃丢任务 | 落盘仅 `local_agent_service.rs:1797-1832`（early）与 `:2876`（final） | kill -9/断电 | 已完成的多轮工作全丢 |
| S10 | 畸形输入 | panic 有 JoinError+catch_unwind 双层（`tool_executor.rs:595-625`、`ws.rs:1567-1605`）；畸形 JSON 有重试后 ReturnFatal | — | 不致命，部分静默吞 |

已有且有效的保护（不重复建设）：流式外层 180s + 首 chunk 30s、LLM 重试 3 次 + 退避 +
fallback_model、WS ping/心跳/90s 宽限、收敛检测、max_turns 软上限、工具结果预算 rollup、
clarify 300s 超时、优雅关机全量落库。

### 1.2 优化方案（结构性，非兜底）

**P0-A 审批从"阻塞点"改成"队列 + 可配置超时 + 可远程决定"**（解决 S1）
1. `DEFAULT_APPROVAL_TIMEOUT` 从 ZERO 改为配置项（默认 10 分钟），到期自动 **deny**
   并把超时事实写进时间轴（不是静默吞）。删除 `tool_dispatch.rs:256-258` 的 24h sleep hack。
2. HTTP 路径补上 shutdown 通道，消灭裸 await（`routes.rs:1472,1900`）。
3. 审批对象持久化到 SQLite：**审批队列**成为会话状态的一部分，进程重启后待审批
   仍在（配合 P0-C）。这是远程审批（第 4 部分）的前提。
4. 多个待审批按 FIFO 排队，`resolve_pre_dispatch` 从"逐个串行阻塞"改为"先收集
   本批全部待审批 → 一次性呈现"（前端已有批量审批形态）。

**P0-B watchdog 正确布防**（解决 S2）
不是删掉，而是把 deadline 的 arm/check 挪到真正的 await 点：
- arm：进入 pre-turn setup / LLM 调用 / 审批等待**之前**；
- check：每个 await 恢复处；
- 超时动作不是杀 loop，而是把该 await 中断为可恢复错误（走既有 graceful 路径落库）。
同时给"审批等待"单独一个更长的预算（来自 P0-A 的配置），两个 deadline 职责分开。

**P1-A 时间上限改为按工具声明的预算**（解决 S3/S4）
1. `per_call_timeout` 不再硬编码：以工具元数据声明的预算为准（terminal 前台默认
   提到 600s，browser 900s，普通文件工具保持 60s），支持 per-call 覆盖
   （`hermix-tools/src/dispatch.rs` 已有一套未接线的 60/300 常数，统一到一处）。
2. 整批超时 = 各 call 预算之和（带全局上限，可配置），不再是固定 300s。
3. 子 agent 超时可配置且默认提高到 1800s，并允许 delegate_task 的 args 里
   传预算覆盖——**这决定"派子 agent 干长活"是否成立**。
4. 提示词侧配合：明确"预计 >5 分钟的命令必须用 terminal background + 轮询"
   （见 2.3）。

**P1-B 流断自动续跑**（解决 S5）
流中断后不直接 return，而是把 partial message 落库后**自动重试本轮一次**
（fresh turn，带上已落库的 partial 上下文），次数可配置（默认 1）。理由：
半截 tool_calls 剥离已做对（`run_loop.rs:1902-1924`），重试不会毒化历史；
网络抖动是长任务的最大概率中断源，一次自动重试消掉大部分人工介入。

**P1-C ContextOverflow 压缩重试**（解决 S6）
分类为可恢复：收到 context_length_exceeded → 立即执行一轮激进压缩
（保留系统提示 + 最近 N 轮 + 所有未完成 todo/task 状态）→ 重试同一请求。
这是压缩机制已有的能力，缺的只是"provider 拒绝后触发"的路径。

**P1-D 会话级单飞锁**（解决 S8）
去重键从 `(session_id, text)` 改为 `session_id`：同会话已有活跃 run 时，
新消息进入**队列**（run 结束后自动作为下一轮输入——前端已有 messageQueue 概念，
后端补齐即可），而不是并行 spawn。

**P2-A 轮间增量落盘**（解决 S9）
每轮 finalize 后把本轮新增消息追加落盘（现在有 `session_persistence` 层，
加一个 per-turn flush），崩溃后重启显示"上次任务在第 N 轮中断"，用户一键续跑
（续跑 = 把中断事实 + 已完成进展注入新 turn，不是回放旧 turn）。

**P2-B 错误分类收紧**（解决 S7）
保留子串匹配但把默认类从 Fatal 改为 Retryable-with-budget（同文案连续 2 次
才升 Fatal），429 解析 Retry-After 头。此项风险最低、收益中等，放 P2。

---

## 2. 提示词与 Skill

### 2.1 事实

- 满配 ~20-35K token/首回合；工具 schema 是最大头（44 常驻 + 文本块双份）。
- **引用不存在的工具**：`web_fetch`（directions.rs:104,111,226 + domain_mode.rs:270，
  实际只有 `web_search`/`web_extract`）；`process_registry`（directions.rs:29，已删除）；
  `copy_file/delete_file/create_directory`（domain_mode.rs:303-305）；`patch_file`
  （tool_meta.rs:306，实际叫 `patch`）；CODE 域的 `grep`（实际叫 `search_files`）。
- **语义重复**：「报告必须分节」两份逐字拷贝（directions.rs:210-216 vs :407-413）；
  并行纪律三处；停止/总结纪律四处；失败换路两处。合并可省 ~1K token/回合。
- **AGENTS.md 与代码相反**：AGENTS.md 说收敛检测"只有 ErrorLoop 才 hard stop"，
  代码已回退为 no-progress 25 轮 / SimilarContent 10 轮硬停（`convergence_detector.rs:101-108,322-334`，
  commit d4d7bc9f）；AGENTS.md 还引用早已删除的 TOOL_EXECUTION_SUMMARY/STOPPING_SUMMARY_GUIDANCE。
- **死路由**：`DirRoute::MultiStepTask`/`HadAnyToolUse` 无任何 guidance 使用，
  三处注释还在解释它们。
- **skill 体系健康**：24 个内置 skill、三层触发（索引注入 / keyword 自动匹配 /
  手动指定）、hub 市场、健康分。workbench 路由已修（commit 5cb43246）。
- **缺失主题**：todo 与 task_create 两套任务系统无使用时机判据；delegate_task 只
  绑定在"读大文件"场景，编排场景无指导；后台 terminal 使用时机无指导（子 agent
  已封 terminal 但父侧无补偿说明）；tool_search 桥（>25 工具时启用）零解释；
  memory 与 daily_memory 两套写入纪律互不引用。

### 2.2 方案

**P0 事实纠错**（半天，纯文字，零风险）：
1. 全部不存在工具名改为真实名（web_fetch→web_extract、patch_file→patch、
   grep→search_files、process_registry→删除该句、copy_file 系列→terminal 或补建真工具）。
2. 删除三处死路由注释；`context.rs:857` 的 USER_STYLE_GUIDANCE 引用清理。
3. AGENTS.md 同步：收敛检测硬停阈值、TOOL_EXECUTION_SUMMARY 已删除、nudge 一次性。
4. 重复段合并：分节文案单一来源（format.short 引用同一常量）、并行纪律并入
   EXPLORE_EFFICIENCY、停止纪律四处合成一段"完成与停止"、失败换路合成一段。

**P1 补缺失指导**（每个 ~10 行，进 directions.rs 对应路由）：
1. 任务系统判据：**todo = 当前任务的执行清单（本轮/本会话粒度），task_create =
   跨会话目标树**；多步执行用 todo，需要跨会话追踪的目标才建 task。
2. delegate_task 编排判据：≥3 个文件的调研 / 可并行的独立子任务 / 输出 >4K 字符
   且不需要中间决策 → 派发；需要用户交互或连续决策 → 自己做。
3. 后台命令判据：预计 >2 分钟的命令一律 `background` + 轮询；前台只跑 <2 分钟的。
4. tool_search 桥说明：一句"列表里没有的工具用 tool_search 找、tool_call 调"。
5. 记忆判据一句话：持久事实进 memory，今天做了什么进 daily_memory。

**P2 结构**（可选）：directions.rs 按"铁律（Always）/ 场景（路由）/ 家族（模型特定）"
重排文件分区，18.5K 字符目前是按历史顺序堆叠的。

---

## 3. 工作模式：接线，而不是重建

> **2026-09-29 完成记录**：第 1 根线（DomainMode 接线）已落地——
> `prompt_assembly::resolve_domain_mode` 在三处 `RouteContext` 构造点接入
> 检测/覆盖，域指令与工具加权真正生效。另按用户明确要求，**加了手动
> 「工作/编码模式」开关**（Composer chip → `SendMessageRequest.domain_override`
> → `AgentConfig.domain_override` → 检测器 pin），推翻本节原有的「明确不做
> 手动切换 UI」——用户偏好优先。
>
> **2026-09-29 追加**：第 2 根线也已落地——`ChatView` 不再丢弃
> `expert.toolset`，经 `SendMessageRequest.expert_toolset` 传后端，
> `send_message`/`send_message_stream` 用 `ToolRegistry::toolset_members`
> 展开成组成员并进本轮白名单（`allowed` 是过滤集，扩大只加不禁——
> floor 与用户已启用集合不受影响，未知组名 no-op）。
>
> **2026-09-29 第 3 根线：按「删假信号」处理**。ThinkingMode Quick/Deep 算完
> 只进日志、不改任何行为，而它的组合器 `ModeComposition` 整模块从无非测试调用方
> ——按本节自己的建议（「若近期不做，删除芯片展示避免假信号」）直接删除。连带清掉
> 同一簇死代码：`ModeComposition`（378 行）、`DomainPreset` 的 5 个无人读字段
> （`priority_tools` / `safe_write_override` / `convergence_signals` /
> `max_stall_turns` / `output_style`）+ `ConvergenceSignal` 枚举、
> `ModeGate` / `ModeGateDecision` / `is_write_tool`（`orchestrator.rs` 600→约
> 150 行）、`DomainModeDetector` 的惯性 / 工具历史机制（检测器每 turn 新建，
> 那些状态永远攒不起来）。
>
> **三根线至此全部了结**：第 1、2 根已接线，第 3 根已删除。

### 3.1 事实

| 机制 | 自称 | 实况 |
|---|---|---|
| DomainMode 五域检测 | "不需要用户手动切换"（domain_detector.rs:4） | **整套死代码**：detect 只在自己的测试里被调用；`RouteContext.domain_mode` 所有构造点都是 None；域指令、priority_tools 加权、收敛信号全部未生效 |
| ThinkingMode Quick/Deep | 自动选择 | 每 turn 跑一次，结果只用于日志 + 前端芯片展示；不改 prompt 不改预算；无手动入口 |
| 专家 Expert | 个性化 | 纯手动；**toolset 字段被显式丢弃**（`ChatView.ts:746 void expert?.toolset`）；不选专家无任何兜底推断 |
| 工具路由 | 省 92% token | KeywordRouter + AlwaysInclude 在跑；中文 query 打分≈0 是已知问题，白名单已打到 18 个；其余 4 个路由器未启用 |
| Toolset floor | — | 真实且最新（essential.rs HARD floor + SOFT 尊重 curated） |

### 3.2 方案：把已建成的引擎接进 run_loop，用户无感

回答用户的问题：**不需要显式的模式切换 UI**——切换引擎已经写好（五域检测 +
预设指令 + 工具加权），缺的只是三根线：

1. **接线 DomainMode**（P1）：在 run_loop 组装 prompt 处调用
   `DomainModeDetector::detect`（输入：用户消息 + 本轮工具调用历史），把结果写入
   `RouteContext.domain_mode`。效果自动获得：Code 域注入代码纪律 + grep/search 加权，
   Write 域注入内容创作指令 + office 工具加权，Operate 域注入整理指令 + storage 加权。
   检测错了也不致命——priority_tools 只是 +5.0 加权，工具仍全在 floor 内。
2. **消费 expert.toolset**（P1）：`ChatView.tsx:746` 不再丢弃，把 toolset 作为
   `ChatOverrides` 字段传后端，映射到 toolset_distributions 的白名单并叠加在
   HARD floor 之上（floor 不可削减的原则不变——专家只能**加**工具，不能删保底）。
3. **ThinkingMode 落地或降级**（P2）：Deep 模式至少要做一件事——提高
   `invalid_tool_json_max_retries` 预算与收敛检测阈值（深任务天然轮数多）；
   Quick 模式收紧 max_turns。若近期不做，删除芯片展示避免"假信号"。

**明确不做**：手动"模式切换"UI。现有专家（手动）+ 五域（自动）+ toolset 开关
（手动）三层已覆盖，再加一层只会让用户搞不清谁生效。

### 3.3 工具缺口（写作/设计/整理任务）

有 office_cli / create_presentation / image_generate / markitdown / storage_analyzer /
move_file，基本盘够。真缺口两个：**OCR 已被移除**（扫描件只能靠多模态模型读图），
**无全文内容搜索**（search_files 是文件名/内容搜索但无 ripgrep 级性能，大批量整理
靠 terminal 兜底）。建议：OCR 用"读图走多模态"的现状写进提示词说明即可；
全文搜索接 ripgrep（Windows 下有预编译二进制）作为 search_files 的加速后端（P2）。

---

## 4. IM 远程指挥

### 4.1 事实（2026-09 调研，带来源见附录）

| 通道 | 结论 |
|---|---|
| 微信个人号（gewechat/iPad 协议） | gewechat 已死（2025 停维护）；padlocal 收费 ~200/月且有封号风险。**没有免费合规的双向通道** |
| 微信官方（公众号/企微） | 合规但只能被动回复/48h 窗口，做不了双向遥控；可做**单向通知** |
| QQ 官方 bot 平台 | **国内唯一零风险 IM**：C2C 单聊 + 富媒体 + 个人可注册，OpenClaw 有官方插件先例 |
| Telegram Bot | 接入最简单、能力最全；手机端需代理 |
| Tailscale + Web Dashboard | 零风险、全双向、**改造成本最低**（hermix-server 已有 HTTP/WS + 会话存储），OpenClaw 官方把这条路当合法远程形态 |
| ntfy 自托管 | 单向推送任务进度/完成通知，极轻 |

OpenClaw 的教训必须吸取：4.2 万实例暴露公网、skills 市场 ~15% 恶意内容。
安全三件套 = **只绑 localhost/Tailnet + 发送者 allowlist + 敏感操作私聊审批**。

### 4.2 方案：三层递进，第一层几乎免费

**第 1 层（P1，成本最低，先做）：Tailscale + Dashboard 远程审批闭环**
- hermix-server 已有完整 REST/WS + Dashboard。补两件事：
  1. Dashboard 移动端可用的审批入口（ApprovalDrawer 的移动形态，第 1 部分 P0-A
     把审批做成了持久化队列，这里直接消费）；
  2. 一个极简的 token 认证（Dashboard 目前无鉴权，Tailscale 网络内可接受，
     但加一层更稳）。
- 效果：手机上看到任务进度、点掉审批卡、发新指令——这就是"人不在电脑前"的
  核心场景，且零封号风险。

**第 2 层（P2）：IM 指令通道（QQ 官方 bot + Telegram Bot 二选一起步）**
- 在 hermix-server 里加一个 `channels` 模块（OpenClaw 的 Gateway+Channel 架构的
  极简版）：channel = 一个轮询/websocket 连接 + 消息 ↔ 会话的映射。
- 消息进入 = 在指定会话发一条用户消息；回复 = 会话新消息推回 IM。
  **不需要新协议**——现有 send/subscribe API 就是全部所需。
- 安全：allowlist（QQ OpenID / TG user id）+ pairing 码 + 敏感审批只在白名单
  私聊生效（回复"允许"即远程点掉审批卡，依赖 P0-A 的审批队列）。
- 通知回推：任务完成/失败/需要审批 → ntfy 或 IM 主动消息。

**第 3 层（P3，可选）：微信单向通知**
- 企微/公众号推送"任务完成/需要审批"的通知，点击链接跳回 Tailscale Dashboard
  处理。微信永远不做双向遥控。

**明确不做**：微信个人号协议接入（封号风险 + 维护成本 + 合规风险三杀）。

---

## 5. 实施顺序

| 阶段 | 内容 | 规模 | 依赖 |
|---|---|---|---|
| 1 | P0 事实纠错（2.2 提示词）+ AGENTS.md 同步 | ~1 天 | 无 |
| 2 | P0-A 审批队列/超时/持久化 + P0-B watchdog 正确布防 | ~3-4 天 | 无 |
| 3 | P1-A 工具预算声明制 + 子 agent 超时 | ~2 天 | 无 |
| 4 | P1-B 流断续跑 + P1-C 压缩重试 + P1-D 会话单飞 | ~3 天 | 阶段 2 |
| 5 | 3.2 模式接线（DomainMode + expert.toolset） | ~2 天 | 无 |
| 6 | 第 1 层远程：Dashboard 移动审批 + token 认证 | ~2 天 | 阶段 2 |
| 7 | 第 2 层远程：QQ/TG channel 模块 | ~4-5 天 | 阶段 2+6 |
| 8 | P2 杂项（轮间落盘、错误分类、Thinking 落地、ripgrep） | 按需 | — |

阶段 2 是全局枢纽：审批队列同时解开"人不在挂死"（S1）和"远程审批"（第 4 部分），
watchdog 布防覆盖其余卡死路径。

---

## 附录：调研来源（远程部分）

- OpenClaw 文档与安全声明：docs.openclaw.ai（gateway/security、channels/qqbot）
- OpenClaw 安全事件：conscia.com、barracuda.com（2026-04）、github.com/zast-ai/openclaw-security
- 微信通道现状：wechatapi.net（2026 方案盘点）、github.com/wechaty/puppet-padlocal、
  github.com/Devo919/Gewechat（已归档）、AstrBot issue #1443
- QQ 通道现状：bot.qq.com 运营规范、LangBot 文档（Lagrange 归档标注）、
  sealdice/go-cqhttp-lagrange（go-cqhttp 已死）
- 替代通道：open.feishu.cn、xtom.com（ntfy 自托管）、blog.kulman.sk（Tailscale 实践）
