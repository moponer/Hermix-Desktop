# Decision Note: 机制级写法逻辑对标：hermix vs ZCode vs Codex（vs DeepSeek harness）

Status: proposed — 三方源码级机制对比与抄写清单（2026-10-18）。源码位置：hermix（仓库内）、ZCode（f:\<workspace>\zcode-ref）、Codex（<refs>/ref-codex）、dsh-libreoffice-kit（<refs>\dsh-spike\kit，已另有笔记）。每项给出：各方写法 → 判定 → hermix 应抄的具体机制（文件+符号）。

## Decision

以 ZCode（最强项）与 Codex（最强机制）为参照系，逐项判定 hermix 的写法差距，并把**可直接抄的机制**列成清单（文件 + 符号级）。本篇只做机制层判定，不含产品形态与工具数量层面的差距（那部分见 `2026-10-18-capability-alignment-audit.md`）。

判定结论：13 项机制差距中，hermix 侧最需要补的是**文件编辑的先读后改状态校验**、**工具输出截断的真实性**、**会话日志的完整入盘**、**模型侧强制 MCP 注入**；其余为增强项。

## Alternatives considered

- **以 Codex 为唯一参照系**：否决——Codex 的强项在插件/事件机制，ZCode 在桌面交互与工具策略覆盖面更全，单选一方会漏项。
- **直接引入 Codex 的插件运行时**：否决——hermix 自有 plugin kernel 已覆盖同类需求，两套运行时并存会让生命周期与权限模型分裂。
- **把机制级差异并入能力审计那篇**：否决——两篇粒度不同（能力层 vs 写法层），混写会让落地清单无法按「文件+符号」执行。
- **只列差距不写抄写清单**：否决——「知道差在哪」不产生改动，清单必须能直接开工。

## Consequences

- 差距清单进入实现队列，按下方优先级建议与落地清单分批推进。
- 每条抄写清单都指到具体文件与符号，避免「照着做」时二次翻源码。
- 机制改动会触及工具执行链与上下文组装，落地时必须同步更新既有单测与工具描述。

## 1. 文件编辑可靠性 —— 判定：ZCode 最强，hermix 零状态是硬伤

- **ZCode**：会话级 `ReadFileStateMap`（tool/read-file-state.ts）：Entry{path/content/offset/limit/isPartialView/readAt/revisionId/mtimeMs/sizeBytes}；Edit 前校验"读过 + 未变"（mtime 整毫秒推进或 size 变化即 stale，严格全读且一致才原谅）；`isPartialView`（token 截断视图）直接拒绝编辑；CRLF 统一+写回还原；写回 `atomic:true + expectedRevision` CAS 乐观锁；模糊匹配 8 级链（edit-matchers.ts：exact→引号→行号前缀→转义→\uXXXX→line_trimmed→缩进→block_anchor≥0.8），歧义即报错。
- **Codex**：apply-patch/src/seek_sequence.rs `seek_sequence` 四级容错定位（exact→trim_end→trim 双侧→Unicode 标点归一）+ `eof` 锚定；`ApplyPatchFailure::delta()` 失败时保留已提交 hunk 清单（审计哪些已落盘）。
- **hermix**：patch 的 H2 契约（字面唯一才改、歧义拒绝、multi-edit 内存原子）不差；但**零状态**——没读过也能改、无 stale 检测（read→外部改→盲写覆盖）、非 UTF-8 直接失败、无 BOM/CRLF 保留、v4a_patch.rs 的 hunk 应用退化为纯字面搜索。

**抄**：① ZCode read-file-state 全套（Entry + findLatestReadFileState + resume 持久化 metadata）；② ZCode edit.ts 的 CAS 写回；③ Codex `seek_sequence` 接到 `v4a_patch.rs::hunk_to_search_replace` 应用路径；④ Codex `ApplyPatchFailure::delta` 失败审计。

## 2. Shell 策略层 —— 判定：Codex 策略模型最强（正交可组合）

- **ZCode**：bash.ts 策略子模块约 15 个文件（超时 policy env 可调 default 120s/max 600s；只读判定 argv 级按 dialect；后台任务 registry + 自动转后台；git 安全校验真仓库含 commidir 逃生舱判定）；输出 inline 30KB + 持久化 artifact 5GiB（persistOutput: on_truncate/always，截断后给可读文件路径而非丢弃）。
- **Codex**：`AskForApproval`（4 档）× `SandboxMode`（3 档）正交；per-call `prefix_rule`（如 ["git","pull"] 审批一次前缀族复用）+ `require_escalated` 单命令越权；`unified_exec/stdin_approval.rs::TerminalPermissions` 每次 write_stdin 与 launch 快照比对（policy drift 复核——"更早的批准不改变已运行进程的沙箱"）；无硬超时，yield 250-30000ms + 后台 300s 默认 + 进程上限 64。
- **hermix**：硬顶 120s 但 schema 写 30000（口径不一致）；审批是布尔整串放行；无 sandbox/只读判定/后台注册表。

**抄**：① ZCode `bash-timeout-policy.ts::resolveBashTimeoutMs`（env 可调+clamp，顺带修 schema 口径）；② ZCode `bash-output.ts` + `background-tasks.ts` 的 artifact 机制（截断给路径不丢内容）；③ Codex `shell_spec.rs::create_approval_parameters` 的 `prefix_rule`（替代整串 approve_for_session）；④ Codex `stdin_approval.rs` 的 policy 快照复核。

## 3. 读取安全 —— 判定：ZCode 三环相扣最强

- **ZCode**：token 预算 → partial view → isPartialView 阻止编辑（`createTokenCapPartialView` 85% 目标二分截断 + nextOffset 续读）；图片统一 processor（maxBase64Bytes/maxDimension，自动 resize）。
- **Codex**：`view_image` 先查 `model_info.input_modalities`，模型不支持直接拒——能力协商防线独有。
- **hermix**：read_file **无 binary 探测**（from_utf8_lossy 乱码照样进上下文，300K chars 才截）；GIF/WEBP/BMP 不压缩原样 base64（>5MB vision API 限制风险，注释自认）；terminal 拦 `cat` 图片引导 read_image 是亮点。

**抄**：① ZCode `read-text.ts::createTokenCapPartialView`（token 二分截断 + nextOffset）；② ZCode `read-image.ts` 统一 processor 覆盖 GIF/WEBP/BMP；③ Codex 的模型能力协商思路。

## 4. 输出截断 —— 判定：Codex 最强（头尾保留 + token 预算）

- **Codex**：`truncate_middle_with_token_budget`（保留头尾、标记"…N tokens truncated…"、UTF-8 安全、响应带 original_token_count）；unified_exec 1MiB ring buffer + 全量 transcript。
- **ZCode**：ToolEntry **声明式 resultBudget**（maxInlineBytes/maxModelBytes/strategy: truncate|artifact/preview head|tail）executor 单点执行。
- **hermix**：10+ 处魔数常量各 handler 自己截；dispatch 纯 head 截断（stderr 摘要常在尾部，全丢）。

**抄**：① Codex `truncate.rs::truncate_middle_with_token_budget` 替换 dispatch 纯 head 截断；② ZCode 声明式 resultBudget 收敛魔数进 registry 元数据。

## 5. Turn 循环 —— 判定：hermix 双 900 行平行循环是结构性债务

- **ZCode**：单 while + TurnPhase 状态机（canTransitionTo 非法迁移抛错，不仅记录）；micro-compact 分 PreRequest/MidTurn 两相。
- **Codex**：steering first-class——`input_queue.rs` InputQueue、`steer_turn(expected_turn_id)` 乐观并发（turn 活跃才注入）、start/recover_turn_if_idle、`inject_if_running(ResponseItem)`。
- **hermix**：run/run_stream 两套平行 while；steering 把 "User guidance:" **追加进 tool result 文本**（污染工具结果）；LoopPhase 枚举只是记录器。

**抄**：① ZCode `turn-machine.ts::canTransitionTo/isTerminalPhase`（给 LoopPhase 加迁移校验）；② Codex `input_queue.rs` 的 expected_turn_id 竞态防护；③ steer 注入改独立消息（ZCode attachment 模式），停止污染 tool result。

## 6. 上下文压缩 —— 判定：hermix 内容级手段最强，触发分层有四个盲区

- **hermix 强项**（保持）：prune 六段管线（MD5 去重/json_crusher/Jaccard/一行摘要/参数截断/base64 剥离）、spill 落盘、熔断器、archives 可检索。
- **四个盲区**：① 分母未扣 output_reserve（0.90×context 压完可能仍不够输出）；② 无 min-savings 回滚（压了不够等于白压还烧 ctx）；③ 无压缩风暴防护（rapid-refill 熔断）；④ 压缩破坏 prompt cache 基线。
- **ZCode**：effective window 先扣输出预留 32K；minTokenSavings=256 不足即回滚不写；`evaluateRapidRefill`（3 turn 内 3 次 compact → 硬错）；TimeBased 触发（60min idle）。
- **Codex**：`BodyAfterPrefix` 压缩范围（只统计 prefill 基线后的增量，保 prompt cache）；"换窗式"压缩（不调 LLM 摘要直接开新窗，走同一 hook 生命周期）；turn 结束才压不打断。

**抄**：① ZCode `policy.ts::getEffectiveContextWindowSize`（分母扣输出预留）；② `microcompact.ts` minTokenSavings 回滚；③ `turn-loop-state.ts::evaluateRapidRefill`；④ Codex `context_window.rs::AutoCompactTokenLimitScope::BodyAfterPrefix`；⑤ Codex pre/post compact hook 生命周期。

## 7. 错误分类与重试 —— 判定：hermix 字符串分类最弱，无视 Retry-After 是实锤缺陷

- **ZCode**：`ClassifiedModelFailure{code, reason, retryReason, retryable, retryAfterMs, statusCode}` 字段分离；cause 链递归解包业务码；`parseRetryAfterMs` 贯穿三条路径；maxAttempts 11、cap 60s、env 覆盖；`isRetrySafePreludeStreamEvent` 首 token 后禁重放。
- **Codex**：`retry_after()` sleep_until 优先；`ExhaustedResponseRetry` 耗尽登记防立刻复撞；传输降级（WS→HTTPS）重置重试计数；ContentFilter 先注入 guidance 再重试。
- **hermix**：字符串匹配分类（已有 Quota 终态改进）；**完全不解析 Retry-After**；无流式重放安全边界。

**抄**：① 结构化 ClassifiedModelFailure（四字段分离）；② parseRetryAfterMs；③ 首 token 后禁重放（`stream-retry-boundary.ts`）；④ Codex 耗尽登记。

## 8. 流式消费 —— 判定：ZCode 最精细

- **ZCode**：`readNextWithStreamIdleTimeout`（每次 read 重置 idle + 超时随重试递增 + **abort 参与 race 并真断流**）；`streaming-tool-call-assembler.ts` 的"已进入流式 input 未收到 end 时 final call 不许顶替"门控（防半截 JSON 被执行，事前防护）。
- **hermix**：stall 只发通知不 abort（上游挂死时不恢复）；truncated_tool 计数是事后补救。

**抄**：① per-read idle timeout + abort 竞速真断流；② 流式 tool-call end 门控。

## 9. 会话持久化 —— 判定：Codex 双轨最强，ZCode 恢复语义最细

- **Codex**：append-only JSONL rollout（crash-safe 可回放可 diff）+ SQLite 索引；**pending input 也持久化**；迁移带回滚计划。
- **ZCode**：恢复重建细节——中断 tool result 补占位符、`persistedTokenUsageBaseline` 保留（恢复前后 token 估算不漂移）、分支选择。
- **hermix**：SQLite 单点（损坏即全丢）；恢复整块反序列化；pending steer 中断即丢。

**抄**：① JSONL 镜像（SQLite 损坏可重建）；② pending input 落盘；③ ZCode 恢复期 token 锚点 + 中断占位符。

## 10. 子代理 —— 判定：各有最强面（hermix 截断/预算/封锁最强）

- **hermix 强项**（保持）：SUB_AGENT_RESULT_MAX_CHARS=8000 CJK 安全截断、预算继承取父剩余最小值、物理+逻辑双层工具封锁、timeout 协作中断+abort 防孤儿。
- **ZCode 强项**：markdown frontmatter 档案（AgentProfile：tools/disallowedTools/skills/mcpServers/permissionMode/maxTurns/background/modelSelection）、user/project 覆盖内建、autoBackgroundMs 定时转后台、**agent listing 增量播报**（新增/移除只播一次）、plan 工具强制剔除（子代理无恢复面）。
- **Codex 强项**：子代理=独立完整 thread；`wait_agent/send_message/interrupt_agent/followup_task` 工具对（同步派发可升级为可交互）；不用工具白名单用 sandbox+approval 继承；`SubagentDefaultContextLimits` 重置子代理上下文窗。

**抄**：① ZCode `subagents.ts::loadZCodeAgentProfiles`（markdown 档案发现 + RESERVED_AGENT_NAMES 仲裁）；② `listing.ts::collectAgentListingAttachment` 增量播报；③ autoBackgroundMs 后台模式；④ Codex `multi_agents_v2/{wait,send_message}.rs` 工具对。

## 11. Skill 系统 —— 判定：hermix 匹配层最强，预算与解析是盲区

- **hermix 强项**（保持）：多层匹配（keyword+CJK bigram+fuzzy+向量）、章节骨架降级、健康分、quality gate、`.hermes-skill` 离线包。
- **盲区**：① `<available_skills>` 注入块**无总预算**（ZCode 20K chars 超限降级 names-only）；② **frontmatter 的 allowed-tools 解析了但从未强制**（实锤死字段）；③ YAML 解析无修复式重试。
- **Codex**：`SkillPolicy.allow_implicit_invocation` per-skill 关自动触发；裸名歧义仲裁（count==1 才解析）；修复式 YAML parser（不吞真错误）。

**抄**：① ZCode `sections/skills.ts` 元数据总预算 + names-only 降级；② Codex per-skill 隐式调用开关；③ Codex 歧义仲裁；④ **把已有的 allowed-tools frontmatter 接进 ToolRegistry::retain_only**（死字段激活，成本最低收益立现）。

## 12. 专家机制 —— 判定：hermix expert 目前只是 UI 选型器

- **ZCode**：markdown 档案 + user/project 覆盖 + agents-state.json 启停/模型覆盖 + 插件命名空间。
- **Codex**：agent-roles TOML 任意配置层叠（无工具白名单，靠 sandbox）。
- **hermix 独有优势**：`SubAgentRole::return_contract`（角色→结构化返回契约）——ZCode/Codex 都没有。

**抄**：① ExpertEntry 档案化（markdown frontmatter + 正文，内置项代码兜底）；② 把 return_contract 从 SubAgentRole 搬进 ExpertEntry（自有能力升级，别人没有的东西）；③ Codex nickname_candidates 可读标识。

## 13. 插件系统 —— hermix 无对应面；未来实现直接按 Codex 注入协议

- 抄：Codex `render.rs` 4KB 注入上限 + mention 驱动；`PluginLoadOutcome` 统一诊断（hermix skill 扫描静默跳过坏技能，应收集诊断回 UI）；ZCode `inferComponentTypes` 一清单多能力。

## 执行优先级（机制抄写清单，按收益/成本比排序）

1. **P0**：Retry-After 解析 + 流式 tool-call end 门控（防半截 JSON 执行）+ office_export 类描述修正。
2. **P1**：read-file-state 全套（编辑可靠性）；truncate_middle 替换 head 截断；压缩触发四盲区修复（扣输出预留/min-savings 回滚/rapid-refill 熔断/BodyAfterPrefix 基线）；allowed-tools 死字段激活。
3. **P2**：TurnPhase 迁移校验 + steer 独立消息 + expected_turn_id；per-read idle timeout 真 abort；token partial view + 图片统一 processor；声明式 resultBudget；markdown 子代理档案 + 后台模式 + wait/send_message；JSONL 镜像 + pending input 持久化。
4. **P3**：prefix_rule 审批粒度、policy 快照复核、artifact 持久化、agent listing 增量播报、ExpertEntry 档案化 + return_contract 搬迁、skill 诊断回传、插件注入协议（未来）。

## 执行记录（2026-10-18 第三批，cargo test tools 763 / agent 1096 全绿，workspace check 零错误零警告）

- **steer 污染修复（C6b）**：steer.rs 不再把指引追加进最后一条 tool result（旧行为让工具输出变成 A\n\nUser guidance:…，破坏结果等价与缓存对比）；改为独立 user 消息，带 [User guidance — mid-run instruction…] 头。2 测试。
- **pending steer 落盘（C4）**：AgentLoop::steer 成功后镜像到 <home>/pending_steer/<session>.txt；drain 后删除；AgentLoop::new/with_interrupt 启动时恢复。测试覆盖「落盘 → 重启恢复 → 清理删除」。
- **LoopPhase 迁移校验（C6a）**：LoopPhase::as_index/from_index/can_transition_to（允许 PostToolExec→PreTurnSetup 唯一回边、同相位重入）；
otify_phase 对非法迁移 warn（AgentLoop.last_loop_phase: AtomicU8）。1 测试。
- **读图统一 processor（C5）**：
ead_image 的缩放/重编码从「仅 JPEG/PNG」扩展到解码器支持的任意格式（GIF/WEBP/BMP），调色板/alpha 源先 	o_rgb8 再 JPEG；解码失败仍回退原始字节。4 测试。
- **记忆三层仲裁（B3）**：memory / daily_memory / session_search 三处工具描述互加 BOUNDARY 段（写什么、何时读、谁只读）。
- **modelscope skills 版本刷新（B2）**：nsure_bundled_skills 由「组存在即跳过」改为版本感知逐文件刷新（xtract_skill_version_ord 支持 1.0.0 与整数两种写法；SKILL.md 仅在 bundled 版本更高时覆盖，其余文件只补缺失）。
- **新增技能（B4/自诊断）**：web-gui-tester（snapshot→act→wait→verify 循环、懒加载、截图取证、注入防护）、self-diagnosis（日志位置、症状→关键字表、配置项、报告格式），include_str! 注册。
- **Explore 档案（B5，规则 3 新增项）**：新模块 gent_profiles.rs（<home>/agents/*.md frontmatter：name/description/role/max_turns/tools/background + 正文）；delegate_task 新增 gent 参数，档案提供 role（决定只读白名单）、轮数预算、prompt 正文；未知档案报明确错误。内置 xplore.md / general-purpose.md 随包首启写入。
- **撤回误标**：oice_mode 经核实为 server→hermix-voice 真实链路（非死代码），审计结论撤回。

## 执行记录（2026-10-18 第四批，tools 765 / agent 1097 全绿，workspace 零错误零警告）

- **TaskStop（#3）已完成**：stop_subagent 工具（背景派发的配套，ZCode TaskStop analog）。进程级 ackground_tasks() 注册表保存 AbortHandle；SubAgentOrchestrator::stop_background() 中止任务并把 lineage 改写为 cancelled（error="stopped by request"），get_subagent_result 立即看到终态；重复停止返回 
ot_running（非错误）。测试 stop_background_cancels_and_marks_lineage。
- **声明式 resultBudget（#1）第一增量已完成**：新模块 hermix-tools/src/result_budget.rs 收敛四处散落魔数——READ_FILE_CHARS 300k / TERMINAL_OUTPUT_CHARS 100k（terminal.rs 内原本两处独立 100_000 已统一）/ PDF_TEXT_CHARS 50k / DESKTOP_SNAPSHOT_CHARS 20k，并加单调性测试（预算顺序即意图）。
- **存储三件套（#2）收敛结论**：核实后 **disk_cleanup 与 storage_clean 并非完全重复**——storage_clean 的 safelist 已含 hermix_home()，但 disk_cleanup 有独立"扫描→候选→清理"模型，且 stubs.rs::disk_cleanup_manifest_marked_ready 是既有治理测试（钉死该 seed 为 ready）。本轮采取**边界收敛**：三处描述互加 BOUNDARY（谁只读/谁需审批/谁只管 HERMIX_HOME）。**物理删除需同时改治理测试与 dashboard seed，属产品决策，未擅自执行。**

## 未开始

- **Tauri 内嵌浏览器面板（#4）**：需要 WebView2 CDP spike + 前端面板组件 + shell↔server 通道，是多日 UI 工程，非一次代码编辑可完成。
## 执行记录（2026-10-18 第五批：浏览器面板第一步）

- 决策：disk_cleanup 保留（用户拍板），与 storage_analyzer/storage_clean 的边界按 BOUNDARY 描述分工。
- 面板后端链路（可用）：新模块 tools/os_auto/browser/panel.rs（PanelState 落 <home>/browser_panel.json，register/current/clear + 测试；discover_ws_url 用裸 HTTP GET /json/version 取 webSocketDebuggerUrl，64KB 读取上限，parse_ws_url 独立可测）；browser/mod.rs::ensure() 面板优先（注册过面板 → 发现 ws → Browser::connect → 复用面板标签页 → 设下载目录/启动 screencast；失败 warn 并回退独立 Edge），Session._launched 改 Option（面板会话不拥有 profile）；Tauri 命令 browser_panel_open/close（label=browser-panel 的 WebView2 窗口，additional_browser_args --remote-debugging-port=9333，登记/清理端口，已进 generate_handler，编译通过）。
- 验证边界（诚实标注）：本机无法 GUI 运行时验证；additional_browser_args + CDP 附着仅编译级确认，需真机一次手动验证（开面板 → browser_navigate → 快照/截图落在面板）。
- 面板未完成：①主窗口内嵌子 WebView（现为独立面板窗口，需 tauri unstable child-webview + 布局）；②前端入口按钮（命令就绪，UI 未接）；③面板关闭时主动 browser_close 释放会话（当前靠 idle 回收）。
## 执行记录（2026-10-18 第六批：面板第 2/3 步完成）

- 前端入口：`BrowserView.tsx`（既有浏览器小窗，帧流）工具条新增「交互面板 / 关闭交互面板」按钮，调用 `browser_panel_open/close`，状态行显示结果；`tsc --noEmit` 通过。
- 关闭即释放：`Session` 新增 `attached_to_panel`；`ensure()` 在 `panel::current()` 消失时 retain 掉面板会话并重建（避免对已销毁 webview 的 Page 继续下命令）。tools 767 全绿。
- 现状分层：①帧流小窗（无头 Edge + screencast，已稳定，默认）②交互面板（WebView2 窗口 + CDP 直驱，需手动开启，用户可直接点击/输入）。
- 真机验证清单（本机无法 GUI 验证）：打开交互面板 → `curl http://127.0.0.1:9333/json/list` 应只列出面板页（**不得**出现主窗口页面，否则说明主 app webview 也被开了调试端口，属安全问题需改用独立 WebView2 环境/进程）；随后 browser_navigate/snapshot/screenshot 应落在面板窗口；关闭面板后一次工具调用应自动回到独立 Edge。
## 提交前审查（2026-10-18）：发现 14 项，修复 13 项

提交 48a42201 / e2207577 / 8f62d527（main）。

已修：①panel discover_ws_url 无超时（改 800ms deadline，避免持 sessions 锁永久挂死）②stream_idle 在 activity 通知路径被 24h 重置覆盖（改按 last_chunk 剩余时间重装，否则整个中止是死代码）③spawn_background insert-after-spawn 竞态（spawn+insert 同锁，避免完成态被改写成 cancelled）④file_converter stderr 未 drain 会阻塞子进程（改 Stdio::null）+ 所有退出路径统一 kill/wait ⑤html_to_docx 跨 run 空格被吞（`This isimportanttext`）+ 十六进制实体永远解析失败 ⑥agent_profiles frontmatter 行内 `#` 注释未剥离（示例自身解析错）+ BOM + tools/background 声明真正生效 ⑦steer restash 覆盖后来指令（改 concat）⑧file_read_state 锁 poisoned 后每次调用 panic（改 into_inner）⑨note_delegation_start 在错误路径不弹栈（移到全部校验后）⑩plan_mode 拒绝清单补齐（disk_cleanup/storage_clean/skill_manage/send_message/bbs_post…）并把 win_* 精确为写动作（只读的 win_find_elements/win_list_windows 保留）⑪面板残留登记导致死 page（discover 失败即丢弃面板会话）⑫refresh_skill_group 版本不可解析时不再覆盖用户文件 ⑬persist_lineage 改 tmp+rename 原子写 ⑭paginate_read_body 字节/字符混淆（预算按字符，含中文回归测试）。

未修（记录在案，非本次引入或需结构改造）：file_read_state 对超大文件永远 partial 导致 patch 死循环（需"已覆盖行区间"模型）；file_read_state 会话表无回收（常驻进程内无界）；plan_mode 是进程级 bool（per-session 需改 registry 结构）；html_to_docx 嵌套 table 覆盖；prompt_assembly 超 80 技能无提示；面板多会话共用同一 page；面板端口固定 9333 未校验占用者身份；stream_idle 中止时未过滤残缺 tool_call；file.rs 单行超预算时 offset 提示跳过该行剩余部分。
