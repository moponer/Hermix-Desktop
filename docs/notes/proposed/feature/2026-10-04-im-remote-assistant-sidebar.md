# Decision Note: 侧栏「远程助手」入口 —— IM 会话从对话列表里独立出来

Status: proposed — 设计定稿，前端待实现；后端数据契约（`/api/channels/status` 增加 `name`）已随本提案先行落地

## Decision

远程会话的会话 id 是确定性的 `im:{platform}:{user}`（见 `channels/session.rs`），一个渠道**永远只有一条**会话，不会开新的。它现在混在普通对话列表里，既看不出身份也看不出死活。方案：

1. **侧栏新增「远程助手」分组**，位于会话列表上方。可见性 = 会话列表存在 `im:` 前缀会话，或 `/api/channels/status` 任一平台 `bound`。条目 = 渠道图标 + 机器人名（如「QQ · 小成子」）+ 连接状态点。点击打开该渠道的唯一会话——复用现有 ChatView，session_id 不变，历史、上下文、审批中继全部沿用既有机制，不新建任何会话模型。
2. **ChatView 标题栏分支**：`im:` 会话显示 `QQ · 小成子`（数据 = status 的 `name`，QQ 网关 READY 事件里的 `user.username`）+ 连接状态点（`connected`）+ 故障原文悬浮（`detail`）+「通道管理」入口（跳设置页的通道绑定区）。
3. **普通对话列表隐藏 `im:` 会话**，避免同一会话出现两个入口。
4. 分组头部提供速查入口：生成配对码、通道状态（bound/connected/last_error）、`/help` 命令表。

数据契约（已落地）：`GET /api/channels/status` 的 `platforms[]` 增加 `name: Option<String>`，由 `ImRuntime` 从网关 READY 事件捕获；未连上/未运行为空。

## Alternatives considered

- **混在对话列表加「远程」徽标** — 否决：入口不显性，且远程会话与普通会话的生命周期语义完全不同（固定单条 vs 随开随建），混排会让用户在列表里找不到"那台机器人"。
- **独立顶层页面复刻一套对话 UI** — 否决：远程会话对 AgentService 来说就是普通会话（同审批表、同持久化），复刻一份视图是双份维护，改一处漏一处。
- **只做侧栏筛选 tag** — 否决：固定单会话语义下"列表"没有存在感，一个 tag 承载不了渠道状态和设置入口；"远程指挥"值得一个一级入口。

## Consequences

- 侧栏多一个常驻分组，会话区可视高度被压缩一点（分组在无绑定平台时隐藏，代价可控）。
- openid 不出现在任何 UI 上——它是 bot 维度的随机 id，对人无意义；人看的是渠道名 + 机器人名。
- 企业微信的机器人名暂缺（其 READY 帧结构未核对），标题栏回退显示「企业微信」，name 为空时不阻塞其余信息。
- 实现落点：侧栏分组（App.tsx 会话列表渲染分支）+ ChatView 标题栏（`im:` 前缀检测）+ `api` 层 status 类型加 `name` 字段。
