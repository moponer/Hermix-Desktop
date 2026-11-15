# Decision Note: toolset 可见性清理——browser/cronjob 及第四批进硬地板，send_message 去假成功

Status: implemented — 2026-10-16

## Decision

1. `cronjob` 进 `essential::HARD_ESSENTIAL_TOOLSETS`。工具链（工具 + AutomationScheduler + server 真实后端接线）一直完整，但老用户的 `enabled_toolsets.json` 快照早于它上线 → 白名单后置过滤把整个 toolset 静默关掉。这是 vault（2026-10-11）之后的第三例同型坑。
2. `browser` 同进硬地板，并移出 `os_auto::OPT_IN_TOOLSETS`（现仅剩 `desktop`）。opt-in 的原始理由是"驱动用户的真实浏览器"，但虚拟窗口 P0（2026-10-14）已消灭该担忧：恒 `--headless=new` 虚拟渲染、一次性 profile、CDP 合成事件、screencast 只进应用内小窗。副作用面与 terminal 同级（逐调用 RiskLevel 审批闸）。留在 opt-in 的实际效果是整条头牌产品线被藏死：LLM 看不见 browser_*，答「我没有浏览器能力」，也不会引导用户去设置开启。
3. 第四批盘点（同型坑一次清完）：`session_search`（跨会话召回）、`todo`（任务规划）、`clarify`（唯一提问通道）、`delegate_task` / `get_subagent_result`（子代理派单）同进硬地板。它们的系统提示曝光是 `tool_names.contains` 门控的——toolset 被旧快照关掉时提示一起消失，属静默失明。
4. 系统提示补能力地图（`directions.rs::WEB_AUTOMATION_GUIDANCE`）：`url_safety`=策略检查 / `web_search`+`web_extract`=公开页抓取 / `browser_*`=完整浏览器（可登录）/ `cronjob`=定时自动化。custom_app 开发循环纪律加第 0 步：定时/自动化需求 → cronjob，不做 App。
5. `cronjob` 工具 description 双语化并含「定时任务/自动化」关键词，对冲 custom_app 在提示中的强曝光。
6. `send_message` 去假成功：`SignalMessagingBackend` 原返回 `status: pending` 信封"等 gateway 投递"，但全仓无任何投递队列消费者——LLM 拿到假"已排队"。改为如实报错（gateway 未接线），与 bbs 缺 key 运行时报错同契约；真实投递仍走 `GatewayMessagingBackend`（宿主接线后覆盖）。

## Alternatives considered

- **browser 进 soft floor**（wildcard 才开）— 否决：目标用户正是旧快照（显式清单），soft floor 对他们无效，坑原样保留。
- **保持 opt-in，只在提示里教 LLM 引导用户开开关** — 否决：把"找到开关"的成本转嫁给用户，且 LLM 从未见过该工具时引导话术形同虚设；vault 先例已证明硬地板是唯一可靠通道。
- **仅改 description 不动 floor** — 否决：工具被静默关闭后 description 根本不进模型视野，改了也白改。
- **`send_message` 从注册表撤下** — 否决：homeassistant 先例（缺 key 仍注册、执行时如实报错）保留可发现性，宿主接线真实后端后零代码变更即恢复功能。
- **长尾 toolset（kanban/task/approval/vendor 等）一并进 floor** — 暂缓：非头牌，等出现真实用户报告再逐个裁决，避免硬地板无限膨胀。（`workflow` 2026-10-17 因用户问「LLM 知道工作流吗」而补入硬地板——它挂着「自动化 → 工作流」产品页面，属头牌相邻。）

## Consequences

- 获得：新/老用户对话里「创建自动化」「访问并登录网站」直接命中正确工具线；`desktop`（真键盘注入）仍保持 opt-in，侵入性担忧不变。
- 代价：显式 curated 清单里没勾 browser 的用户会被硬地板强制转正——与 terminal/vault 同语义，Settings UI 照常显示"开"。
- 风险：browser_* 首次进默认工具表，模型可能在不必要时调用；`browser_navigate/click/type` 为 Caution，逐调用审批闸兜底。
- 获得（第四批）：session_search/todo/clarify/delegate_task 在老安装上恢复可见；子代理剥离（sub_agent_safety）不受影响，仍按设计工作。
- 代价（send_message）：无 gateway 的安装上调用即报错（此前是假"已排队"）；LLM 会如实告知用户需先配置消息网关。
