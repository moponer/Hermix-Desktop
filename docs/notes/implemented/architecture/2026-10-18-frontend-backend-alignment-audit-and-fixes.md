# Decision Note: 前后端对接审查与 P0/P1 修复批次

Status: implemented — 2026-10-18 当日完成审查、用户拍板、修复、验证全流程。

## Decision

对前后端做了一次全量对接审查（后端 axum 路由 ↔ 前端 REST/WS 封装逐条对照 + Tauri command 注册面 ↔ invoke 调用点对照 + 契约行为核验 + 设计规范符合性审查），产出三类清单：前端未对接、后端无入口、行为不符。用户逐项拍板后，按 P0→P1 顺序修复。**所有改动均保持现有传输/存储架构不变**——stdio 桥、localStorage 镜像、wire 协议形状只修不换。

### 用户拍板的 5 项方向性决策

1. **自定义专家 wire schema：前端迁就后端**。`components/agents/storage.ts` 加 `toWireAgent`/`fromWireAgent` 双向映射（snake_case wire），保存/读取/删除走映射层。理由：后端是持久层、wire 形状已在 DB 落盘，前端映射层是一次性成本。
2. **编排总线 7 类事件：补前端**。真缺口在后端——`OrchestrationEventBus` 无任何生产订阅者，事件死在总线上。`ws.rs` 补 orchestration→WS 桥（仿既有 todo 桥，`LoopPhaseChanged` 按 session 过滤防串扰）；前端 `useStreamParser` 补分支：自动化触发弹 toast、LoopPhase 显示中文状态 chip、计划确认渲染只读 `PlanConfirmCard`（后端无确认回传通道，不做假交互按钮）。
3. **图片生成设置页：删除**（后端无能力，纯 localStorage 摆设）。
4. **消息赞/踩：保留**为本地功能，`hermix.messageVotes` 登记进 `STORAGE_KEYS`（此前未注册违反 canonical source 约定）。
5. **hero 字号：40px→28px**（`--text-hero`，字重服从 600 上限），消除 tokens.css 与 字体规范（已并入 tokens.css）.md 的规范冲突——按文件来，tokens 为准并更新注释。

### 契约修复（P0）

- **自动化 422**：`AutomationTask` 四个 `Option` 字段补 `#[serde(default)]`；`update_task` 改 `UpdateTaskRequest` 局部 merge patch——单键 patch（启停开关只发 `{enabled}`）不再 422，且 prompt/schedule 校验在 **merge 后**进行，单键 patch 无法绕过不变量。
- **错误信封**：`rest.ts` 同时读 `{error}`（后端 `HttpError` 形态）/`{message}`，toast 与 `localizeStreamError` 分类恢复工作。
- **润色假功能**：`api/polish.ts` 裸 `fetch` 改走 stdio 桥 `fetchJSON`（TCP 时代无 host:port，旧路径 100% 走空白折叠降级）。
- **事件链断裂**：`useStreamParser` status 分支解包 `section_gate`/`prefetch_intent`/`acui` 三类被 ws.rs 包装成 `Status{content:<raw JSON>}` 的结构化事件，三个死分支恢复可达、状态 chip 不再闪原始 JSON。

### 契约修复（P1）

- `onReconciled` 复用 `wireArrayToChatMessages`（此前 `as ChatMessage[]` 断言绕过协议适配，重连后 tool/approval 行渲染异常）。
- `buildConnectedPayload` 补 `attached_file_paths` 映射（后端真实消费该字段，路径附件此前被静默丢弃）。
- `patchTodo` 带必传 `session_id`（后端注释明示缺省产生孤儿行、重连快照不含）。
- 批量审批「拒绝」补 1s REST fallback（与 approve 路径同构；丢帧后 Agent 不再挂到 600s 超时）。
- `SkillMeta.enabled` 真值：store 层按 `.disabled` 路径组件判定，`GET /api/skills` 不再恒 true。
- `StreamEvent::SubAgentEvent` 加 `recent_steps`，ws.rs 透传（子代理面板滚动日志恢复）。
- 专家保存/删除改走 `fetchJSON`（`apiInvoke` 不判状态码导致后端 404/500 被当成功、localStorage 兜底成死代码）。

### 设计修复（P1/P2）与 DesignGuard

- **hero 28px 裁决**落地 `tokens.css`；`.claude/rules/frontend-design.md` 写入中文渲染硬约束（J字规范、双层语义、CSS `--text-*` 令牌、语义色、`text-on-accent` 压色块、骨架屏替代加载文字、内联标注优先、一屏字号 ≤3 档）。
- **DesignGuard**（`scripts/check-design-guard.mjs`，`npm run check:design`，进 ci-gate [4c/14] 硬门禁）：TSX 字面色值（色板数据行/`design-guard: allow` 显式例外豁免）、`dark:` 变体、「加载中…」字面量、`window.confirm`。G3（图标按钮 aria-label）因 regex 启发式误报率不可接受未入门禁，靠评审 + impeccable detect 覆盖。
- **13 处「加载中…」换 `SkeletonCard`**（ui/Skeleton 规范：骨架屏替代转圈/文字）；`window.confirm` 全清（DevicesTab/ImChannelsCard 3 处改 `ConfirmDialog`）；`VaultPanel` 手写模态改 `ui/Modal`（获得 role/aria-modal/Esc/焦点陷阱/token 遮罩）；AnalyticsView/CustomAppsView 图标按钮补 `aria-label`。
- **死分支/死代码清理**：`NavPage "agents"` 死路由删除（类型与 routes case）；`/api/logs/*` 三端点 + `log_view.rs` 删除（前端走 Tauri command，重复实现）；`memory_inspect.rs` 未挂载的重复 `router()` 删除；Tauri `get_settings_dir` 移除、`custom_app_{list,package,run,open_dir}` 移除注册（REST 面已全面取代），`custom_app_init` 保留并注释为排障入口；`cdp_n0_probe` 注释修正（前端无调用点，是 N0 真机验证工具）。
- **保留但注释定位**的端点：`/api/{mcp/servers,automation}/reset`（用户改乱后的恢复手段，待前端入口）、`/v1/commands`（排障/脚本面）、`/v1/filesearch/shutdown`（helper 卡死恢复）、`/v1/vault/*`（手机面能力就绪等 `apps/hm` 接线）。

## Alternatives considered

- **专家 wire schema 让后端迁就前端**（`rename_all="camelCase"`）：会改变落盘 JSON 形状，需要迁移既有 `agents/*.json`，且 `AgentPersonaJson` 兼具存储与传输两职——前端映射层把转换约束在边界一处，风险更小。
- **G3 图标按钮检查入门禁**：regex 解析跨行 JSX 的 attributes/children 误报率不可接受（首轮 270 条中 G3 占九成且多为文本按钮误报）；真正的结构检查需要 ts-morph AST——项目无此依赖，按六步决策阶梯不为此引入。
- **删除 `/api/{mcp/servers,automation}/reset`**：能力本身有价值（改乱后的恢复手段），删了要重写；保留 + 注释定位（同 `relay/phone-credential` 既有处置先例）成本最低。
- **Plan 确认做成可交互确认**：后端 `PlanConfirmationRequired` 无上行回传通道（无 WsClientMessage 变体、agent loop 无等待方），做假按钮误导用户；只读卡片 + 状态 chip 是当前协议下的诚实解。
- **CSS 层违规全量清欠**：130+ 处 px 字号/暗色兜底逐条改属独立批次；本次落门禁防增量复发，存量清欠单独立项。

## Consequences

- 前后端契约从此有单一权威注释源：本笔记 + `rest.ts` 头注释。新增端点时 wire 字段命名**默认 snake_case**，前端在封装层映射（对齐拍板 1）。
- `UpdateTaskRequest` merge patch 成为后端部分更新的模板；`create_task` 仍是整体 POST（前端 `blankDraft` 现在能通过）。
- DesignGuard 进 ci-gate 后，字面色值/加载文字/confirm 在 TSX 侧零增量复发；CSS 层量化门禁仍是空白（`check-theme-tokens.mjs` 只扫 tsx），存量清欠未做。
- 编排事件桥使 `orchestration_bus` 的事件首次端到端可达；`PlanConfirmationRequired` 的确认回传仍缺（只读展示），后续做 Plan 模式交互时需先定上行协议。
- `apps/hm` 未接线的 `/v1/vault/*` 保持实现；若移动端最终不做该场景，应连同 relay E2EE 路径一起评估下线。

## Verification

- `cargo check --workspace --all-targets` 零错误零警告；`cargo test -p hermix-server -p hermix-skills --lib` 507 全绿；`cargo test -p hermix-tools --lib` 768 全绿。
- `tsc --noEmit` 0 错误；`npm run check:design` clean；impeccable detect 受影响改动文件 exit 0。
- vitest 1 个既有失败（`MessageActions` 空正文渲染），已用 `git stash` 对照验证为改动前既存，非本批引入。
