# Decision Note: 以「世界状态是否推进」判定空转（L1 状态探测 + L2 进展账本）

Status: implemented — 工具执行出口把可观测世界状态写进 `ToolResult::state`（`hermix_tools::tool_state`），`hermix_agent::progress_ledger` 按「本 run 是否见过这个签名」判零推进，连续 4 轮注入诊断且不停止 run。

## Decision

空转的判据只用一个，与工具、模型、任务类型无关：**循环在转，但外部世界状态没有变化**。据此落两层：

**L1 状态探测（`hermix-tools/src/tool_state.rs` → `ToolResult::state`）** — 签名在**工具执行出口**计算，与工具名、原始参数、返回内容同处一地；写进 `ToolResult` 的显式字段，agent 只消费字段，不再解析文本。按族取信：

| 族 | 签名来源 |
|---|---|
| 文件（read/write/patch/move/delete/list/search） | 磁盘 `stat`：路径 + 存在性 + 大小 + mtime(ms) |
| shell（terminal / execute_command / run_command） | 命令本身 + 输出指纹（抹掉数字抖动） |
| 浏览器 / 内嵌视图（webview_* / browser_* / web_open） | 落点 URL + 标题；内嵌视图工具走 MCP `_meta.hermixState` 结构化通道，系统浏览器工具从返回文案解析落点 |

两个执行出口都填：生产批路径 `hermix-agent/src/tool_executor.rs::run_tool_handler`（脱敏重建时保留 `state`），以及 `hermix-tools/src/dispatch.rs` 的 `dispatch_tools` / `dispatch_single`（经 `DispatchedResult.state` 透传）。其它工具为 `None`，不参与判定 —— 宁可漏判，不可把正常干活误判成空转。

**MCP 的结构化通道**：viewer 的 `webview_navigate` 把实际落点作为独立返回值交给 `call_tool`，`tools/call` 的响应放进 `_meta.hermixState`（MCP 规范留给实现放元数据的扩展位，第三方客户端可忽略）；`hermix-mcp` 客户端读它，生成带唯一键 `hermix_tool_result` 的信封（正文 + state 分离）；两个执行出口用 `tool_state::unwrap_envelope` 在**截断之前**剥开。模型只看正文，零推进账本只看字段 —— 全链路没有「解析对方中文文案」的约定。

**L2 进展账本（`hermix-agent/src/progress_ledger.rs`）** — 每轮把签名与「本 run 已见集合」比对：无任何新签名 = 零推进 +1，出现新签名立即清零。连续 4 轮零推进时向上下文注入一条诊断（前置条件是否成立 / 目标是否已不可达 / 换入口换工具换作用域 / 等待改用等待机制），**每个 run 只报一次，不停止 run，也不禁用工具**。

同一次改动里，`webview_navigate` 改为**回报实际落点**（导航后等待并回读 URL/标题，识别登录页/提示页并明确写「未到达目标页」）。此前它返回的是请求的 URL，工具在替模型宣布成功 —— 这是 L1 拿不到真实世界状态的上游原因。

## Alternatives considered

- **让每个工具 handler 各自返回 `state`**：最精确，但要改 30+ handler 的返回类型，且同一份规则会在每个工具里重复。改为「执行出口统一探测 + `ToolResult::state` 字段承载」：单一事实来源，只改两个出口。
- **让 agent 从 `content` 里猜状态**（先前的中间版本）：不用改 `ToolResult`，但把「世界是什么样」寄托在解析工具文案上，工具一改文案就静默失效。否决。
- **重复调用物理拒绝（同工具 + 同参数第 3 次直接拒执行）**：对「同一动作反复做」最有效，但会误伤轮询等待与合法复查，且它只覆盖「动作重复」，覆盖不了「换了动作但世界没变」。本轮不做。
- **继续用整批结果逐字指纹（`RepeatGuard`）**：动作微变就漏（每轮换一段 JS → 结果逐字不同），本次事故正是如此。保留它作为「完全相同的批次」的强信号，但不再作为主判据。
- **用助手正文相似度**：thinking 模型正文恒空，判不了。正文相似度只保留给「真的在说话」的场景。
- **轮次上限**：与推进无关，既误杀长任务又漏杀快速空转。降级为账单兜底（`max_turns: 0` = 不限）。

## Consequences

- 状态签名是**归一化后的字符串**，不是结构化 schema：数字抖动被抹平（`09:56:12` → `#`），但字符串型随机 id 会让同一状态看起来不同 —— 误差方向是「漏报」，不会造成误停。
- 覆盖范围只到文件 / shell / 浏览器三族。memory、todo、delegate 这类不改变外部世界的调用不参与判定；若未来出现新的「动作型」工具族，需要同时补 `tool_state::signature` 的规则。
- `ToolResult::state` 只在本进程内使用：`skip_serializing_if = "Option::is_none"` 让它不出现在持久化的 `tool_result` 行与 provider 请求里，也不改变任何上游协议。
- 零推进只注入诊断、不施加硬约束，因此它依赖模型服从；真正的硬兜底仍是 `ErrorLoop`（工具连续全错）、`RepeatGuard`（批次逐字相同 → 停用工具）与成本/轮次上限。
- `webview_navigate` 现在会等页面加载（600ms + 最多 7s），单次调用比原来慢；换来的是模型第一次就能看见「我被踢到登录页了」。
