# Decision Note: 全库体检修复战役(分批修复方案)

Status: proposed — 2026-10-05 全库只读体检(HEAD `2d4baf09`,范围:21 crate + 桌面前端 + docs)共 50+ 项发现,本篇是其分批修复方案;纯方案,尚未实施。

## Decision

按「先安全、后整洁、批间隔离」原则分四批修复。每批独立可验证、独立成 PR;行为修复不与死代码删除混批,保证回滚粒度。体检证据(file:line)随条目内嵌,本篇自足,不依赖会话记录。

### 批次 1 — 高危三连(几行修复,最优先)

1. **`exec_command` 缺审批兜底**(`crates/hermix-server/src/routes.rs:1960`)
   - 改法:`/v1/commands` 的 `AgentOverrides` 补 `approval_fallback: Some(ApprovalFallback::Deny)`,与同文件 `send_message`(L1510)对齐,兑现 `ws.rs:92-94` 与 routes.rs 头注释宣称的架构。
   - 测试:已配 `dangeroustool` 能力位的设备经 `/v1/commands` 触发高危工具 → must deny(新增 routes 单测)。

2. **browser 会话注入键失配**(`crates/hermix-tools/src/tools/browser.rs:695-699`)
   - 改法:`session_from_params` 改读 `_session_id`(与 `registry/validation.rs:169-173` 注入键、`cua.rs:201` 一致);显式传 `"session"` 保留为 LLM 覆盖入口,注入键优先。
   - 测试:registry 注入 `_session_id` 后,browser 工具路由到指定会话而非 `"default"`(防跨会话串台)。

3. **配对码铸造 TOCTOU**(`crates/hermix-server/src/pairing.rs:498-526`)
   - 改法:`mint_code` + `insert` 移入同一锁临界区,对齐 `create_code_only`(L544)的持锁模式。
   - 测试:并发 `create()` 不产生重复 6 位码。

   验证:`cargo check --workspace --all-targets` + `cargo test -p hermix-server -p hermix-tools --lib`。

### 批次 2 — 行为/语义修复(本周;含 2 处待主人拍板)

4. **`HERMIX_CONFIRM=1` 咒语可被模型自签**(`crates/hermix-tools/src/tools/terminal.rs:479-564`)
   - 改法:审批确认不再依赖命令文本前缀——改为审批 registry 签发**一次性确认 token**,terminal 校验 token 而非前缀;删除 fallback 文本中"重试咒语"教学(L559-564)。
   - 行为变化:依赖该前缀的既有工作流退回审批卡——这正是目的。

5. **审批 family 泛化**(`approve_for_session`,terminal.rs:549;关联 engine.rs:455)——**待拍板**
   - 选项 A:记忆粒度从 rule family 降为 signature 级(参数哈希一致才放行);选项 B:维持 family 级但排除多参数高危工具。推荐 A。

6. **traversal 三套口径统一**(`crates/hermix-tools/src/tools/file.rs:239, 393`)
   - 改法:file 读/写路径从 advisory 升级为硬拒,复用 terminal.rs:586 / memory.rs:272 同一判定;`credential_guard.rs:207-221` 的"有意为之"注释同步改写。

7. **config 死字段处置**(`crates/hermix-config/src/config.rs:36-38 tools`、`:108-110 tools_config`)——**待拍板**
   - 事实:两字段全仓无消费点(真实白名单走 `AgentOverrides.tools` → `local_agent_service.rs:1115`),用户写进 config.yaml 静默不生效。
   - 推荐:删除(选项 A)。接线(选项 B)会造成与 ToolTabs/toolset 体系双轨,更糟。删除需同步清理 roundtrip 测试(python_yaml_compat.rs:311、config.rs:962)。

8. **工具取消传播**(hermix-tools 全 crate 无 `CancellationToken`)——**不在本战役,独立立项**。架构面大(dispatch 层统一 spawn+取消、terminal/browser/cua 各后端适配、spawn_blocking 不可硬中断的语义),单写一篇 proposed 笔记;本篇只留指针。

   验证:批次 1 同款 + `cargo test -p hermix-config --lib`;前端无涉。

### 批次 3 — 死代码清除(净删约 300-400 行,向 10 万行目标推进)

全部先 grep 确认零生产调用再删,每组删除独立提交:

- `hermix-server/src/security.rs:131-215` `PolicyGuardConfig` 整组 + `lib.rs:23` 的 `pub use`;`security.rs:424` `effective_decision_for_paired`
- `hermix-agent` 死路由 `DirRoute::MultiStepTask`/`HadAnyToolUse`(context.rs:851/857 + matches 分支 L1005/1009);`convergence_detector.rs:175 apply_domain_max_stall`、`:342 reset()`;`skill_orchestrator.rs:448 scan_directory`;`agent_loop/mod.rs` 中 `RetryReasoning` 描述已删行为的文档(L404-408,**保留枚举本身**——穷尽性防御)
- `hermix-tools`:`backends/browser/a11y.rs:257 indent_check` 空函数、`tools/vendor.rs:418 _typecheck_` 占位;`terminal.rs:940-1081 ProcessHandler/ProcessBackend` **先确认**仅测试引用后整删(~150 行)与 `lib.rs:103` 导出
- 前端:`api/rest.ts` persona 全家桶 12 导出(getSoul/putSoul/getMemory/putMemory/getUserProfile/putUserProfile/listPersonalities/getPersonality/putPersonality/deletePersonality/getPersonaOverrides/putPersonaOverrides)+ 支撑类型(types.ts:412/421/429)+ `getPluginManifests`/`inspectAllPlugins`/`rescanPlugins`/`resetAutomationTasks`/`resetMcpServersToOfficial`/`searchSessions`/`deleteEnvVar`/`fetchCustomProviderModels`/`deleteCustomProvider`/`updateCustomProviderModelContexts`/`getSessionArchiveRefs`/`getModelInfo` 12 个死导出;`api/types.ts:40 DashboardSession`;`api/ws.ts:74 sendMessageStream`;`context/StreamChatContext.tsx:20 StreamChatProvider`(陷阱导出)
- `rest.ts:252 isCircuitTripped` 永假 stub:连同 3 处调用方(useBackendConnection.ts:36、useCustomApps.ts:21、CustomAppsView.tsx:41/70)的无效熔断分支一并清除

   验证:`cargo check --workspace --all-targets` + 全量 `cargo test --workspace --lib` + `pnpm tsc --noEmit`;CI gate 的 `TODO: pre-existing` 扫描不新增。

### 批次 4 — 注释/文档同步 + 择要整洁(随手)

注释修正(逐条给出改法):

- `docs/HERMIX_AGENT_SYSTEM_PROMPT.md:140/144/151/172`:已删常量 `TOOL_EXECUTION_SUMMARY_GUIDANCE`/`STOPPING_SUMMARY_GUIDANCE` 改述为"已删除,由 `REPLY_TERMINATION_GUIDANCE` 承接"(与 AGENTS.md:116 对齐);`:147` 删除 `TASK_FOCUS_*` 引用
- `local_agent_service.rs:57-59`(同文案 1753/3051/3800):"the loop retries" → "立即 break 交外层 `decide_empty_response`"(对齐 session_lifecycle.rs:639)
- `context.rs:849-856`:死路由 doc 改为过去时/删除(随批次 3 删除则此项消失)
- `pairing.rs:113` scopes 注释改"默认 `["chat"]`";`:838` 删除"指纹在 /pair 页核对"旧注释(实际在本 handler L775-779)
- `routes.rs:1958-1959` 占位注释删除;`agent_loop/mod.rs:404-408` 所有权描述删除
- `AGENTS.md:121`:收敛检测补双阈值说明(15/5 advisory WARN → 25/10 hard stop),行号改 95-121
- `App.tsx:1404/1534`:删除 `useStreamChat.ts:1175 liveWs` 失真引用(stdio bridge 时代)

择要整洁:`ws.rs:75` 毒锁改 `into_inner` fail-open、`:300-312` dedup 日志 info→debug(兑现"生产零输出")、`:12` 乱码注释;`channels/mod.rs:331 install_runtime` 同毒锁;`backends/memory.rs:675/763/809` std::fs→tokio::fs;`App.tsx:594` 粘连 `};`;`App.tsx:62-64` 墓碑注释。

**明确不修(防范围蔓延)**:防御性 dead_code(`message_cache.rs:47`、`turn_report.rs:34`、convergence `new()/with_config()`)保留;前端流式状态三轨合并、时间线分组三份合一、`send_message`/`send_message_stream` 重复块提取、App.tsx(3799 行)拆分——均为结构性重构,另立 simplification 笔记,不混入本战役。

### 流程约束

每批一个 PR,按仓库制度附测试证据(`cargo check` / `cargo test` / `pnpm tsc`)、pre-existing 清理状态、`ci-gate.sh` 通过;批次 2 两处拍板项未决前不进入该条实施。

## 评审修订(2026-10-06,逐条证据核实后)

证据核实结果:批次 1-2(browser 会话键)、1-3(pairing 两段锁)、批次 2-7(死字段零消费)、批次 3 抽查(isCircuitTripped 永假 stub 及调用方、persona 导出无外部引用)全部属实。修订如下:

1. **批次 1-1 定性降级:高危 → 显式化加固**。`exec_command` 的 `AgentOverrides::default()` 虽未显式设 `approval_fallback`,但闭包侧 `unwrap_or(ApprovalFallback::Deny)`(local_agent_service.rs:2900)已 fail-closed——Dangerous 工具经 `/v1/commands` 本就被拒。修复保留(一行,可测试性 + 与 send_message 对齐),但 PR 描述按"加固"写,不按"修复高危"。
2. **批次 2-5 拍板:选 A**(signature 级记忆,与 `extract_trust_signature` 既有设计一致)。
3. **批次 2-7 拍板:选 A(删除),且免 alias 预案**。已验证 hermix-config 无 `deny_unknown_fields`——serde 对未知键是忽略语义,删除后旧 config.yaml 不报错,Consequences 里"若报错则加 alias"的预案整个省掉。
4. **批次 2-6 实施前置**:先枚举 terminal.rs:586 / memory.rs:272 的判定语义与合法穿越场景(junction、子目录内 `..`),确认 file 读写合法场景不被新门误拒后再动手;本条是全方案描述最模糊的一条,禁止直接照行号改。
5. **批次 3 拆两个 PR**:Rust 清理、前端清理各自独立(方案已有"每组独立提交",再加 crate 侧/前端侧批间隔离)。
6. **行号漂移警告**:本篇基于 HEAD `2d4baf09`,此后 main 已前进(approval schema gate 等,local_agent_service 漂移约 ±30 行);实施按符号定位,不按行号。
7. **实施前提(硬门槛)**:main 工作区当前存在并行会话的未提交改动——任一批次开工前必须先与该会话收敛提交,否则批次 1 的 routes.rs 编辑必然撞车。
8. **并行轨**:手机端产品化(多用户/多客户端/托管 relay)已独立成篇 → [relay 托管多租户(2026-10-06)](../feature/2026-10-06-relay-hosted-multi-tenant.md)(修订 2026-10-04 mobile-relay-selfbuilt §待拍板 3),与本战役并行推进,互不混批。


## Alternatives considered

- **一次性大 PR 全修**:评审不可行、回滚粒度粗、高危修复被死代码删除的体量淹没。弃。
- **cancellation 并入批次 2**:跨三后端 + dispatch 层的架构改动,混入会拖住"几行修复"的批次 1/2 落地。改为独立立项。弃(本战役内)。
- **config 死字段接线而非删除**:现有 toolset/ToolTabs 体系已是唯一真实现,接线造双轨、违反 YAGNI。推荐弃(待拍板)。
- **只修 bug 不修注释**:reasoning-retry 误导性注释已实际构成错误修改诱因,AGENTS.md 制度依赖注释可信;且注释修正是零风险变更。弃。
- **前端结构重构(状态三轨/时间线三份/App.tsx 拆分)顺势做**:体检报告已给拆分方向(App 内联事件回调并入 useStreamParser),但与"高危先落地"冲突,单独立项评审。弃(本战役内)。

## Consequences

What we give up:
- `config.yaml` 的 `tools:`/`tools_config:` 字段失去"看起来可配"的表面(它们本就无效);roundtrip 兼容测试同步删除,旧配置文件中的这两个键将随 serde deny_unknown_fields 与否表现为忽略或报错——实施时先确认 loader 的 unknown 字段策略,若报错则加 `#[serde(alias)]` 过渡一个版本。
- `HERMIX_CONFIRM=1` 前缀语义消失:绕过审批的脚本化用法(若有)退回审批卡。
- `ProcessHandler` trait 若属未竟规划,删除后复活需按 simplification 制度走 Reintroduction。
- 前端删除 24 个死导出:若有外部分支未合引用,合并期会编译报错——暴露即修复,属良性。

## Reintroduction conditions

(批次 3/4 属 simplification 性质,按制度补记)
- `ProcessHandler/ProcessBackend`:出现真实的进程列管产品需求(前端进程面板立项)时,以新笔记复活并以当时 registry dispatch 架构重新设计,不原样恢复。
- `config.tools` 字段:仅当出现"服务端配置文件(非桌面 ToolTabs UI)驱动工具白名单"的真实需求时回来,且必须接线到 `AgentOverrides.tools` 并带端到端测试。
- `isCircuitTripped`:真实熔断需求出现时按新语义实现(真实状态源),不再提供永假 stub。
