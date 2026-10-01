# Decision Note: IM 渠道的工作区访问范围（三层范围 + 审批回路）

Status: proposed — 设计稿已写定，L1/L2/L3 三层均未实现

> 2026-10-01 从 `docs/CHANNEL_WORKSPACE_SCOPE.md` 迁入。迁移表原标
> "implemented/feature" 有误——本文的 L1/L2/L3 全是待做项，现状是
> `im:{platform}:{user}` 会话仍无任何工作区范围限制。

## Decision

IM 机器人接进来的会话采用**三层工作区范围**（per-bot 静态集 → 会话内
动态扩权 → 随时全权"托管机器人"），三者共用桌面端已存在的审批链路
（`ApprovalRegistry` + `approval_requester`），并保留
`PermissionLevel::Full` + hardline 黑名单作为安全地板。

扩权动作必须能在**手机上**用文字 / 语音完成，不要求坐回桌前——这是
L3 的核心约束，不是附加入门。

## 问题

IM 机器人（QQ / 企业微信）接进来的会话目前**没有任何工作区范围限制**：
`im:{platform}:{user}` 会话落在该 session 的当前/默认工作区，任务中途要碰
别的目录时全凭 LLM 自觉。两个方向的需求：

1. **收紧**：一个机器人只该干某一两个项目的事（对应 ZCode BotsDialog 的
   `WorkspaceAccessCard`——每个 bot 可选可访问的工作区集合）。
2. **灵活放开**：有时就是"啥事都想让它做"，限制太多任务干不完——放开
   动作本身要能在**手机上**完成（文字 / 语音），而不是必须坐回桌面前。

## 现状可复用的地基（别重造）

- `channels/session.rs` 的 IM 会话已接完整审批链路：`approval_requester`
  注册进桌面端共享 `ApprovalRegistry`（桌面「待审批」与 IM **同屏可见**，
  谁先解谁赢，registry resolve 幂等）；`on_event` 把审批提示文本推回 IM；
  `resolve_permission` 已支持用户在 IM 里回 **1/2/3** = 允许一次/本会话/拒绝，
  且「本会话允许」已有落库 + 副作用（`record_approval_side_effect`）。
- `pre_approval_checker`：`session_bypass`（每 session 的临时放行正则表）
  + `user_allowlist`（持久允许列表）——临时/持久两级放行**机制已经存在**，
  只差一个"工作区维度"的条目类型。
- 桌面审批抽屉 2026-09-30 已重做为 5 选项（含「始终允许」「完全访问」
  「告诉模型接下来怎么做」）；IM 侧的 1/2/3 是它的精简镜像，语义可对齐。
- `PermissionLevel::Full` + hardline 黑名单：即使 full 权限，`rm -rf /`
  这类底层拦截不动——"全权"的安全地板已经有了。

## 设计：三层范围 + 一个审批回路

### L1 · 静态范围（per-bot，持久）

- 渠道凭据（`read_creds` 的 `PlatformCreds`）新增
  `workspaces: Vec<String>`，**空 = 不限制**（现状行为，升级无破坏）。
- 渠道卡片（`ImChannelsCard`）加一行「可访问的工作区」：多选现有工作区
  + 「全部」，存回凭据文件。对应 ZCode 的 WorkspaceAccessCard。
- 执行点**必须在工具派发层**（与 `pre_approval_checker` 同层），物理检查
  工具参数的路径前缀 / terminal 的 cwd / delegate_task 的 context 路径——
  不能只写进 system prompt（LLM 可绕过，提示层约束不是边界）。

### L2 · 出范围请求 → 审批回路（不是拒绝，是升级）

- 派发层发现越界 → 不 hard fail，生成一种新审批请求
  `scope_expansion { target_workspace, task_hint }`：
  - **手机侧**（发起任务的 IM 会话）：推送文本
    `⚠️ 任务需要访问工作区「X」…回复 1 允许这一次 / 2 始终允许该工作区 / 3 拒绝`
    ——复用 `resolve_permission` 的 1/2/3 解析；「2」= 把该工作区写进
    L1 允许列表（持久），「1」= 写 `session_bypass` 维度（本轮/本会话）。
  - **桌面侧**：同一个 request_id 进共享 registry，审批抽屉/「待审批」
    直接能点（含 5 选项里的「告诉模型接下来怎么做」）；IM 回复与桌面点击
    竞争，registry 幂等先解先得。
- 超时（沿用 300s 审批 TTL）未决 = 自动拒绝，任务带着拒绝理由继续
  （`DeniedWithFeedback` 回路，模型换方案）。

### L3 · 临时全权（"啥事都想让它做"的逃生门）

- IM 命令 `/全权`（或文字"进入全权模式"）：本会话 `permission=full`，
  跳过 L1/L2 的常规确认；**hardline 黑名单仍然生效**（不是真 yolo）。
- 会话空闲 30 min 或 `/清空` 后自动回默认档——全权永远是**会话级**的，
  不做持久全权（持久放开走 L1 显式加工作区）。

### 手机侧语音确认

- IM 语音消息 → 服务端解码（QQ silk / 企微音频帧，具体 codec 需确认）
  → 现有转写管线（dashboard/transcribe）→ 文本。
- **严格门控**：转写文本只有在该用户**有待决审批**时才走
  `resolve_permission` 的决策解析，否则一律当普通聊天消息（避免语音
  误识别在闲时误触发"允许"）。
- **权限分级**：语音只识别「允许这一次 / 拒绝」；「始终允许」「全权」
  这类持久/宽动作**必须**手机打字或桌面点——转写误差授予持久权限是不可
  接受的，物理约束下宁可少一句语音功能。

## 失败模式（对抗性审查）

| 场景 | 后果 | 对策 |
|---|---|---|
| 范围检查只进 prompt | LLM 换姿势绕过 | L1 检查在派发层，物理路径前缀 |
| 语音转写把"拒绝"听成"允许一次" | 误放行 | 高危档（持久/全权）不接语音；数字+关键词双确认 |
| 手机断网/机器人掉线时待决审批 | 任务卡死 | 桌面同屏可点 + 300s TTL 自动拒绝兜底 |
| 多个待决时回"1" | 歧义放行 | 沿用 `pending_count` 守卫（>1 时拒绝数字回复，要求指名） |
| L1 配空=全放行后没人管 | 范围形同虚设 | 这是显式选择（空=不限制）；卡片上把"全部"标出来 |

## 分期

- **Phase A（最小，全复用）**：`scope_expansion` 审批 kind + IM 1/2/3
  文本确认 + `session_bypass` 加工作区维度。不动凭据文件，无新 UI。
- **Phase B**：L1 静态范围（凭据字段 + 渠道卡片「可访问的工作区」行 +
  派发层检查）。
- **Phase C**：语音消息解码 + 转写（最重，codec 需逐个平台验证），
  独立排期。

## Alternatives considered

- **给每个 bot 配 ACL 列表、运行时校验路径前缀** —— 否决（见正文 L1
  理由：IM 任务形态不稳定，静态 ACL 会在第一次跨目录时卡死）。
- **沿用桌面审批抽屉原样**（5 选项全量搬到 IM）—— 否决：IM 输入成本
  高，5 选项在手机上不可用；IM 侧的 1/2/3 是它的**精简镜像**，语义对齐
  但形态收窄。
- **不复用 `pre_approval_checker` 的 session_bypass / user_allowlist，
  新建一套 IM 专属放行表** —— 否决：临时/持久两级放行机制已存在，只差
  一个"工作区维度"的条目类型；另建一套会产生两套语义近似的放行状态。

## Consequences

- **获得**：IM 会话有可收紧可放开的作用域，且放开路径手机可完成。
- **放弃**：per-bot 静态集不覆盖任务中途的跨目录需求（由 L2/L3 动态
  补齐，代价是动态层需要自己的状态与回收策略）。
- **前置**：本文的 L1/L2/L3 全部未实现；`im:{platform}:{user}` 会话
  现状仍无范围限制，依赖 LLM 自觉。
