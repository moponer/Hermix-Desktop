# Decision Note: 阿黎改走团队通道，兼修 `delegate_task` 假死

Status: implemented — 2026-11-06

三条实报，一条因果链：

1. 一调 `delegate_task` 就卡死；
2. 阿黎拿子代理跑任务，而不是建团队、跟进、验收、汇报；
3. 她的对话栏只有六行纯文本，看不到派了谁、做到哪、结果是什么——「不存在产物，结果该怎么让用户看？」

## Decision

### 一、`member` 进硬地板（根因）

`hermix-tools/src/essential.rs` 的 `HARD_ESSENTIAL_TOOLSETS` 里有
`delegate_task` / `get_subagent_result`（2026-10-16 第四批盘点），**没有
`member`**。`member` 是 P1 才上线的新 toolset，用户更早那次「在设置里点过
一次」写下的 `enabled_toolsets.json` 快照里不可能有它 → 启动时白名单后置过滤
把整个 toolset 关掉 → `dispatch_member` / `get_member_result` /
`review_member` 对阿黎完全不可见。

两个静默后果叠加：

- 她视野里只有 `delegate_task`（硬地板常驻），于是所有活都走同步子代理；
- `MEMBER_ORCHESTRATION_GUIDANCE` 的注入条件是
  `tool_names.contains("dispatch_member")`，分轨纪律也一并不出现。

这与 `vault`（2026-10-11）、`cronjob`（2026-10-16）、`workbench` 三次事故
同型，修法一致：把 `member` 加进硬地板，并更新钉住列表的测试。

新增 `EXTERNALLY_REGISTERED_TOOLSETS = ["member"]`：`member` 由
`hermix_agent::member::runner::register_member_tools` 在 server 构建期注册
（hermix-tools 不能反向依赖 hermix-agent），所以「地板名单必须被某工具
tagged」那条守卫必须把它排除，否则 `registry_with_builtins()` 里查不到。

**顺序要求**：`register_member_tools` 必须在 `apply_persisted_toolset_state`
之前跑（server 现状如此）——否则地板那次 `set_toolset_enabled` 是 no-op。

### 二、人格 prompt 分轨

`context.rs::BUILTIN_PERSONALITY_ALI` 原文写着「可以随时派出子 agent 分头
干活（delegation），用 get_subagent_result 收结果」——这是她"用子代理跑
任务"的直接指令来源，工具可见性只是让这条路成为唯一选项。

改为三条通道按活的性质选：

- **自己动手**：几秒到一两分钟能收尾的，直接做；
- **建团队（`dispatch_member`）**：多轮自驱、几十分钟以上、要产出交付物的；
  带 `acceptance_criteria`，派完**立即返回**，`get_member_result` 跟进，
  `review_member` 独立验收，再汇报；
- **取数据（`delegate_task`）**：只读大文件/多文件检索/PDF 图片分析，
  **明确标注它是同步的、会卡住这一轮、绝不用它跑任务**。

### 三、`delegate_task` 的等待不再无声

`tool_executor.rs` 里 `orch.execute_async(req).await` 是一行裸 await：子代理
最长跑 1800s，期间零事件；前端 300s 静默判超时（有工具在跑时抬到 600s），
屏幕上留下一个永不结束的「已委派子agent」卡片 + 一句假超时，而父 turn /
该会话仍停在那里。`per_tool_budget("delegate_task")` = 1900s，但那段循环
在 `:545` 就把 delegate 排除了，护栏从没生效。

现在等待被包进 `select!`：每 5s 发一条 `status`（前端按帧重置 idle，不再
假超时），并用 `per_tool_budget` 兜底；超时返回明确错误，指向
`dispatch_member`。中断传播仍由编排器内部负责（`run_child` 已把父
`InterruptController` 与子代理 join 竞速），这一层不重复。

### 四、审批卡没有落点时会永久挂死

`App.tsx` 的 `onApprovalRequest` / `onApprovalBatchRequest` 在找不到
assistant 消息可挂卡时返回 `new Promise(() => {})`——永不 resolve，前端不回包，
服务端停在审批等待。父等子代理、子代理等审批、审批等一张画不出来的卡，是
真死锁（命中条件：会话刚切换 / 历史未装载 / 重连后状态丢失）。
改为**默认拒绝**并带 note（note 会作为拒绝理由进模型上下文）：拒绝是可见的，
挂起是无声的。

### 五、对话栏新增部署 / 汇报卡

新增 `components/twin/TwinReportCards.tsx`，挂在阿黎对话流末尾：

- 数据源 `GET /api/members`（8s 轮询）与 `/api/members/{id}`（展开时拉，
  运行中展开期间跟轮询）；
- 一行状态：角色 · 状态 · 验收判定 · 轮数 · 相对时间；下行是工单正文；
- 展开就地读：验收判据、最近步骤、最终报告全文、验收证据；
- **不解析她的回复去推断状态**——她说什么是一回事，后端记的工单状态是
  另一回事；卡片只认后者。

同时把「发布到团队」里的 `delegate_task` 拆出去，单独一行「取数」：两者
混在一行时，用户无法判断这一轮是在派人还是在取数。

## Alternatives considered

1. **只在 prompt 里劝她别用子代理**——否。工具不可见时，再好的纪律也执行
   不了；先修可见性，再说分轨。
2. **把 `delegate_task` 从硬地板移除**——否。它是零上下文膨胀约束的执行
   机制（AGENTS.md Core principles 3），图片/大文件/PDF 都靠它；问题不在
   它存在，而在它是唯一存在的通道。
3. **让 `delegate_task` 变成后台派发（`background: true`）作为默认**——
   否。同步语义是它的契约（schema 与提示词都写死），改默认会让"取数"变成
   轮询，模型要多记一套；长任务本来就该走 `dispatch_member`。
4. **在 hermix-tools 里注册 member 工具**——否，反向依赖不成立；
   `EXTERNALLY_REGISTERED_TOOLSETS` 只放宽守卫的检查范围，不放宽地板语义。
5. **对话栏改成完整对话页（工具卡 + 子代理面板）**——否。她的页面是信息板，
   六行文本的克制是刻意的；缺的是**结果**，不是流水。用一张工单卡承载
   部署与交付，比把主对话的卡片语言整搬过来更省也更准确。

## Consequences

- 阿黎的默认行为从"派子代理"变成"建团队工单"，长任务不再阻塞她的轮次；
- `member` 从此不可被设置页关掉（硬地板语义），这是续 vault/cronjob 之后
  的第四次同类取舍——新 toolset 上线后必须显式决定是否进地板，否则旧白名单
  快照会静默关掉它；
- `delegate_task` 仍会卡住一轮（同步契约），但现在有心跳、有 1900s 护栏、
  超时提示指向正确通道；
- 审批卡在"无处渲染"时默认拒绝，可能让一次合法操作被判失败——这是刻意
  取舍：无声挂起无法诊断，拒绝至少留下原因；
- 前端类型检查未能在本机验证：`apps/desktop-tauri/node_modules` 的符号
  链接在当前机器上不可解析（`Cannot read package config ... unknown error`，
  `type node_modules\react\package.json` 也失败），需要重新安装依赖后再跑
  `tsc --noEmit` 与四道 UI 门禁。
