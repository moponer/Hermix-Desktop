# Decision Note: 侧栏「远程助手」入口 —— IM 会话从对话列表里独立出来

Status: implemented — 侧栏「远程助手」分组与 ChatView 通道标题栏已交付；`/api/channels/status` 的 `name` 契约同步落地。

## Decision

远程会话的会话 id 是确定性的 `im:{platform}:{user}`（`channels/session.rs::session_id`），一个渠道**永远只有一条**会话，不会开新的。已交付：

1. **侧栏「远程助手」分组**（会话列表上方）：条目 = 机器人名（`QQ · 小成子`，来自 `/api/channels/status` 的 `name`，QQ 网关 READY 事件上报）+ 通道连接状态点（`useChannelsStatus` 30s 轮询），点击打开该渠道唯一会话——复用现有 ChatView，session_id 不变，历史、上下文、审批中继全部沿用既有机制。`im:` 会话从对话/项目列表与计数中隐藏。
2. **ChatView 标题栏徽标**升级为「渠道 · 机器人名 + 连接状态点」，悬浮展示故障原文（`detail`），连接断开时状态点转红。
3. **数据契约**：`GET /api/channels/status` 的 `platforms[]` 增加 `name: Option<String>`（`ImRuntime` 从网关 READY 捕获；未连上/未运行为空，企业微信暂缺，回退显示渠道名）。
4. 分组头的「通道管理」按钮跳设置页——配对码、设备撤销、通道绑定都在设置里，不在这里复制第二份。

## Alternatives considered

- **混在对话列表加「远程」徽标** — 否决：入口不显性，且远程会话与普通会话的生命周期语义完全不同（固定单条 vs 随开随建），混排让用户在列表里找不到"那台机器人"。
- **独立顶层页面复刻一套对话 UI** — 否决：远程会话对 AgentService 来说就是普通会话（同审批表、同持久化），复刻一份视图是双份维护，改一处漏一处。
- **只做侧栏筛选 tag** — 否决：固定单会话语义下"列表"没有存在感，一个 tag 承载不了通道状态和设置入口；"远程指挥"值得一个一级入口。

## Consequences

- 侧栏多一个常驻分组，会话区可视高度被压缩一点（仅当存在 `im:` 会话时渲染，未绑通道的用户看不到）。
- openid 不出现在任何 UI 上——它是 bot 维度的随机 id，对人无意义；人看的是渠道名 + 机器人名。
- 通道状态靠 30s 轮询，极端情况下状态点滞后半分钟；不做 WS 推送（为一个状态点引入订阅通道不划算）。
- 实现落点：`Sidebar.tsx`（分组 + 会话过滤）、`ChatView.tsx`（ModelStatusBar 徽标）、`api/rest.ts` + `api/types.ts`（status 契约）、`hooks/useChannelsStatus.ts`。
