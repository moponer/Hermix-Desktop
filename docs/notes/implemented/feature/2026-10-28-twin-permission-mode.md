# Decision Note: 权限模式开关 —— 两档可视化 + 放手模式真的不再申请权限

Status: implemented — 2026-10-28 交付：值守页顶栏两档分段控件 + 二次确认 + 常驻风险条；
`PermissionLevel::Full` 贯通到工具内部确认与资源门（此前只有派发层直通）

## Decision

用户 2026-10-28 实报两件事，一件是观感，一件是行为：

1. 顶栏那颗权限按钮**两档长得一模一样**：`.twin-head__perm` 只有一条中性样式，
   `data-level` 属性写了但没有任何规则消费它，两档的区别仅剩按钮文字。用户
   看不出「现在是哪种」「还有另一种」，更看不出放手模式意味着什么。
2. 「**切到放手模式后，她还是会反复申请权限**」—— 这不是观感问题，是三条
   独立路径里只堵住了一条：

   | 路径 | 旧行为 |
   |---|---|
   | 派发层决策矩阵 `decide_pre_tool_dispatch` | `Full → Allow` 已经正确 |
   | 资源门（P6，`resource_guard`） | **不看权限级别**，越界删除照弹卡 |
   | 工具内部 Caution 确认（`terminal` 的 `request_ws_approval`） | **不看权限级别**，每条命中 CONFIRM 模式的命令照弹卡 |

   `TerminalApprovalContext` 里根本没有权限字段 —— 上下文过不来，工具只能
   按默认口径问。用户看到的「放手了还在问」就是这么来的。

修法：

- **UI**：一颗会换字的按钮换成**两档常驻的分段控件**（`默认 · 需授权` /
  `放手 · 无审批`），当前段实底上色：默认走成功档（绿），放手走警告档（橙）。
  未选中的放手档常驻弱橙，提示「那是危险的那一侧」。进入放手要**两下**：
  第一下把控件换成一句风险说明 + `确认放手` / `取消`（6s 无动作自动收回），
  第二下才生效。生效后顶栏下方挂一条**常驻风险条**，写明「命令、删除、写配置
  都会直接执行，出错由你承担」并给一键 `切回默认`。退出回安全侧一下即可。
- **状态单一归属**：这一页改用全局 `usePermissionLevel`（落 storage），与主
  对话共用同一个设置。此前是本页 `useState`，两个家各说各话。
- **后端**：`TerminalApprovalContext` 增 `permission` 字段，由
  `send_message` 按本轮 `agent_config.permission` 写入；`request_ws_approval`
  在 `Full` 下直接返回 `Approved`（不再注册待决、不再发 `request_approval`）；
  资源门在 `Full` 下不再询问。

**刻意保留的两条底线**（放手 ≠ 无底线）：

- `hardline` blocklist（`rm -rf /`、fork bomb、`mkfs` 等）在 `Full` 之前执行，
  依旧 `Deny`。本机执行没有沙箱，这一层是最后的物理防线。
- 工具内部的**参数与 schema 校验**（`schema_required_violation`）照旧 —— 它
  deny 的是"注定失败的调用"，不是权限。

## Alternatives considered

- **保持一颗按钮，只加颜色**：看不出"有另一种模式"，也无法两下确认；否决。
- **用 `window.confirm` 做风险确认**：DesignGuard 明令禁止视图里用
  `window.confirm`（阻断式原生弹窗、不可样式化、不可测）；否决。
- **用一个全局"跳过所有审批"开关（含 hardline）**：用户想要的是"别烦我"，
  不是"拆掉最后一道物理防线"；`Full` 与 hardline 的先后顺序是刻意的；否决。
- **把放手模式做成 per-session 覆盖**：权限是策略，按会话分裂会制造「这个
  会话放开了、那个没有」的不可预测性；共用全局一份；否决。
- **在工具里读一个全局 `AtomicBool`**：跨会话串扰（A 会话放手 → B 会话也
  不问）；权限必须随本轮上下文下发；否决。

## Consequences

- 放手模式下 `terminal` 与越界删除不再弹卡，`agent.log` 记一行
  `permission=full — auto-approved without prompting`；hardline 仍然拦。
- 顶栏多出约 60px 宽：窄窗口下先被截断的是 `twin-head__sub` 那句状态说明
  （本来就有 ellipsis），不是控件。
- 主对话与值守页共用权限设置：任一处切换，另一处同步。这是有意的
  「一个策略一个家」。
- 命名一处定稿：`PERMISSION_LEVELS` 的 label 改为「默认模式 / 放手模式」，
  输入栏 chip 与审批抽屉第 3 个动作同步（此前同一设置叫过「默认权限 /
  完全权限 / 完全访问」三个名字）。输入栏那条 chip 本来就在 full 档换
  `AlertTriangle` + 琥珀色，视觉口径与本页一致。
- 前端：`TwinView.test.tsx` 新增 4 条（两档可见、两下确认、可取消、风险条
  切回默认）；后端：`hermix-tools` 新增
  `full_permission_skips_prompt_and_does_not_emit_event`，钉住「注册表里没有
  待决、也不发事件」。
