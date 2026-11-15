# Decision Note: 审批抬升前的 schema-required 校验门

Status: implemented — 三个审批抬升点（Tirith RequireApproval / 风险级门 / 资源守卫）在 `request_approval` 之前做 schema `required` 字段在位检查，缺参直接 Deny 回模型。

## Decision

`local_agent_service` 的审批决策路径新增 `tool_dispatch::schema_required_violation` 门：仅检查**字段在位**（不做类型判断——类型转型归 registry 的 `coerce_args`，取值判断归 handler），schema 未声明 `required` 的工具零影响。缺参时返回 `Deny { reason }`，reason 复用 "Invalid tool parameters: 缺少必需参数 …" 句式——模型立即拿到可修正的反馈并重试，**用户永远不会被叫来批准一个注定失败的调用**。

## Root facts（另一台机器的活体复现，2026-10-05）

`storage_clean` 报 "必须要有 confirm: true"（handler 的 `None` 分支 = 参数对象里**根本没有** confirm 键），而审批卡已点允许。对执行链全程追参（流式组装 → AL-7 校验 → dedup/repair → JoinSet → registry coerce_args → handler）后确认**参数搬运是忠实的**——抵达 handler 的参数本来就缺 confirm（上游成因：模型重试漏发 / 流式组装截断，需事故机日志定论）。另一台机器上 agent 的"审批与参数传递是两条独立路径"归因不成立；真正的缺陷是**审批门控不做参数校验**：缺参调用照样弹卡、照样消耗用户的批准与 600 秒窗口，失败后才由 handler 报错。

## Alternatives considered

- **门控全部工具调用**（不限审批路径）：行为面变大——schema `required` 与 handler 默认值不一致的工具会在非审批流提前失败。收益为零（非审批流的失败反馈本来就是即时的）。否。
- **handler 侧放宽 confirm（如缺省视为 true）**：删除互锁是安全边界，绝不允许推断。否。
- **只给 storage_clean 加特判**：治标。任何 Dangerous 工具的 required 字段都有同样的浪费审批问题。按 schema 通用解决。

## Consequences

- schema `required` 从此具有真实语义（此前仅用于 provider 工具定义展示）：声明了 required 但 handler 实际容忍缺省的工具会在审批门被拦——审计义务转移到 schema 声明侧，这本来就该准确。
- `confirm` 互锁本身不变（布尔 true / "true"/"1" 字符串接受，见 storage_clean.rs:367-421）；本门只拦**键缺席**。
- 事故的上游成因（为何模型重试时丢了 confirm）仍需事故机日志定论；若坐实流式组装截断，另开修复。
