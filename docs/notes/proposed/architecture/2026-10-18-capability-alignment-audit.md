# Decision Note: 对齐分析报告：hermix 全量能力 vs ZCode 参照系

Status: proposed — 审计报告与处置计划（2026-10-18）。**批次 1（P0）与批次 2 部分项已于 2026-10-18 交付，见文末执行记录**；机制级写法对标另见 `2026-10-18-mechanism-level-alignment.md`。参照系：ZCode 源码（f:\<workspace>\zcode-ref，同形态产品：桌面/CLI agent + 工具 + 技能 + 插件）；辅助参照 DeepSeek harness（文档技能包/裁剪引擎路线，已另行落地 dsh 引擎）。

## Decision

判定规则（用户拍板）

1. 双方都有 → 参照 ZCode 优化或重写；
2. 仅我方有 → 评估存在必要性，明确保留/移除结论与依据；
3. ZCode 有我方无 → 必须新增。

## 一、工具面对齐（hermix-tools vs ZCode builtInTools）

### 1.1 共有 → 按 ZCode 优化

| 模块 | 对比结果 | 建议 | 优先级 |
|---|---|---|---|
| patch/write_file vs **Edit+Read file-state** | ZCode 的 Edit 依赖 Read 记录的文件状态做精确匹配（core/src/tool/handlers/index.ts:78-144），防止未读先改/内容漂移 | patch 增加"先读后改"状态校验（file-state 追踪） | P1 |
| terminal vs **Bash** | ZCode 有超时策略、只读策略、后台任务、git 安全策略子模块 | 补**后台任务**与 **git 安全策略**（保护分支/危险命令门） | P1 |
| clarify vs **AskUserQuestion** | ZCode 是结构化多选项提问（structured options） | clarify 升级结构化选项协议 | P2 |
| delegate_task vs **Agent/Task** | ZCode 支持前台/后台双模式（TaskOutput/TaskStop 取结果/停止）；hermix 仅前台 | delegate_task 增加后台模式 + 取结果/停止工具 | P2 |
| workflow vs **dynamic-workflow** | ZCode 的 dwf 是 11 工具的脚本化工作流系统（带灰度门，也未默认开）；hermix workflow 单工具 | 暂不追——ZCode 自己也未默认启用；观察其灰度结论再定 | P3 |
| browser_* 8 工具 vs **browser-use 插件** | 能力面已对齐（navigate/snapshot/click/type/wait/scroll/screenshot + headed）；ZCode 多**录屏**与 **web-gui-tester** 方法论 | 补 web-gui-tester 技能包（方法论，非代码）；录屏 P3 | P2 |
| win_* 桌面自动化 vs **zcode-cua** | 均默认关；ZCode 配 computer-use skill 教方法 | 补 computer-use 方法论 skill | P3 |
| skills_list/view/manage + skill_loader vs **Skill 工具** | 分工一致（操作 vs 注入） | 保留现状 | — |
| execute_code vs **js (node_repl)** | 形态不同（多语言后端 vs Node REPL） | 保留现状 | — |

### 1.2 仅我方有 → 必要性评估

| 模块 | 结论 | 依据 | 优先级 |
|---|---|---|---|
| task_manager（kanban 后台任务） | **移除或接持久化** | 内存态 Vec 重启即失（task_manager.rs:68-69），残品级；与 todo/task_tracker/kanban 三套任务机制重叠 | P1 |
| kanban / task_tracker / todo 三套并存 | **收敛为一套**（保留 todo + task_tracker，kanban 降为纯 UI 视图） | ZCode 只有 TodoRead/TodoWrite 一套；四套任务机制是维护税 | P2 |
| hermix_achievements | **撤出模型工具面**（UI 保留） | 游戏化功能对 agent 无价值，白占工具 schema 与 token；ZCode 无此类工具 | P2 |
| bbs_read/bbs_post | **默认不注册**（HERMIX_BBS_KEY 缺失时） | 依赖外部 key，ZCode 无对应物；现状"注册了但执行必错"是谎报可用性 | P1 |
| homeassistant 4 工具 | **保留，但 HASS_URL 缺失时不注册** | 智能家居是产品差异化（ZCode 无）；谎报可用性要修 | P1 |
| image_generate/gpt_image_generate/video_generate/video_status | **保留**（key 缺失时报错可接受，与 image_gen.rs:139-164 现状一致） | 媒体生成是产品差异化 | — |
| tts | 保留 | 后端可配置，实现完整 | — |
| disk_cleanup/storage_analyzer/storage_clean 三件套 | **保留，内部合并**（analyzer+clean 重叠） | 桌面用户价值；ZCode 无对应物但非 agent 主线 | P3 |
| storage-analyzer skill | **移除**（与工具重复且教的是绕路——用 execute_code 跑脚本而工具可直接做） | SKILL.md:74-76 vs 工具实现 | P3 |
| url_safety 独立工具 | 保留（ZCode 内嵌于 WebFetch 域名白名单，形态不同价值相同） | — | — |
| model_providers/platforms/security_guidance | 保留 | 信息类防幻觉，token 成本低 | — |
| memory/daily_memory/session_search 三层 | 保留，**补"何时用哪个"仲裁说明**（系统提示或工具描述） | ZCode 仅 memory files；三层是我们差异化但边界靠约定 | P2 |
| vault/custom_app_manager/workbench_manager/vendor | 保留（vault 条件注册已正确；vendor Phase 4.5 未完成部分按 YAGNI 维持现状） | 自有差异化能力 | — |
| aliases（list_files/edit_file/list_tools） | 保留 | ZCode 同有 compat.ts 别名机制，做法一致 | — |

### 1.3 ZCode 有、我方无 → 必须新增

| 模块 | 内容 | 优先级 |
|---|---|---|
| **文档技能包四件套**（documents/pdf/spreadsheets/presentations） | SKILL.md 方法论（routes/scenes/设计系统/封面配方）+ python-docx 等脚本 + env_check；PDF 渲染用已落地的 dsh 引擎（render 能力） | P1 |
| **后台子代理 + 取结果/停止** | delegate_task 后台模式 + TaskOutput/TaskStop 等价物 | P2 |
| **PlanMode 工具对**（EnterPlanMode/ExitPlanMode） | 计划模式：只读调研→方案确认→执行 | P2 |
| **superpowers 工作流技能**（14 个：TDD/系统化调试/验证完成/代码评审等） | 需核对其许可（obra/superpowers）；自写或适配 | P2 |
| **web-gui-tester 技能** | 配合现有 browser 线的 GUI 黑盒测试方法论 | P2 |
| **自诊断技能**（zcode-guide 的 diagnosing-mcp/skills/hooks/plugins） | 面向用户的排障引导 | P3 |
| **visualize / image-search / plugin-creator** | 视产品方向，暂缓 | P3 |

## 二、残品/兜底/一致性问题（独立于对比，必须处理）

| 项目 | 证据 | 处置 | 优先级 |
|---|---|---|---|
| file_converter PDF→Image 假输出（把占位文本写进"图片"文件） | file_converter.rs:6,522-539 | **重写**：dsoffice render 真实现（引擎已在位，契约同 convert） | P0 |
| file_converter image→PDF 是最小占位 PDF | file_converter.rs:275 | 重写或如实标注低保真 | P1 |
| office_export 描述滞后（宣称"docx/pptx planned and rejected"，实现已支持） | office_export.rs:53-58 vs 127-134,486 | 改描述，一行修复 | P0 |
| dashboard stubs.rs 8 个 seeded 未接后端插件（context_engine/dashboard_auth/google_meet/observability/spotify/teams_pipeline/video_gen/example_dashboard） | stubs.rs:118-328 | 逐个决策：下架或标 unavailable，禁止谎报 | P1 |
| modelscope-dev 4 个 skill 只打包主文本、首启后永不更新 | lib.rs:197-199 | 补全资源或下架；至少修"永不更新" | P2 |
| BedrockUnsupportedProvider | agent_builder.rs:548-560 | 保留（显式报错优于静默，自注 parity gap） | — |
| voice_mode 模块残留（无 register，纯死代码） | mod.rs:6-11,65 | 清理 | P3 |
| cron CLI stub LLM / stub_provider / automation 占位数 | 测试基建或诚实标注 | 保留 | — |

## 二.5 逐项对标补充（2026-10-18 深挖，类别级审计漏标的 3 项）

- **Explore 型只读子代理档案**：ZCode 有具名档案 + 声明式工具白名单（core/src/subagent/explore.ts + explore-tools.ts）；hermix 仅 BLOCKED_TOOLS_FOR_SUBAGENT 硬裁剪。→ 新增具名只读档案（P2）。
- **EnterPlanMode/ExitPlanMode**：已列入 1.3 缺失表（P2）。
- **ReadSessionContext**：确认 session_search 已等价覆盖，无需新增。
- 其余逐项映射：工具 19 项对齐、7 项共有-差距、插件 4 对齐/4 缺技能包/3 暂缓/1 不适用（restore-legacy）、iOS 模拟器不适用（Windows-only）、Android 模拟器 P3 评估。

## 三、执行顺序（建议批次）

- **批次 1（P0，纯修复）**：file_converter PDF→Image 换 dsoffice render；office_export 描述一行修；bbs/HA 可用性如实化。
- **批次 2（P1）**：patch file-state 校验；terminal 后台任务+git 安全；task_manager 持久化或移除；dashboard stubs 清算；文档技能包骨架（docx 先行，dsh render 补 PDF→图）。
- **批次 3（P2）**：clarify 结构化、delegate_task 后台模式、PlanMode、superpowers 适配、任务三套收敛、记忆仲裁说明、modelscope skills 处置。
- **批次 4（P3）**：存储三件套合并、visualize 等 P3 新增、voice_mode 清理、录屏。

## Alternatives considered

- **整体对标 ZCode 的产品形态**（桌面/CLI 双形态、插件市场、iOS 端）：否决——hermix 是单形态桌面应用，移植成本远大于收益，逐项对齐即可。
- **引入 Codex 插件层作为运行时**：否决——hermix 自有 plugin kernel 已覆盖同类需求（见 `2026-09-24-plugin-kernel-design`），引入第二套运行时等于两套生命周期。
- **直接搬 dsh 的文档技能包/裁剪引擎**：采纳为工具层实现（document-skills-pack + document 工具），不作为对标参照系。
- **保留 pandoc 做文档转换**：已出局（dsh 引擎替代），见 Reintroduction conditions。

## Consequences

- 差距清单按批次 1~4 排期落地，批次 1（P0）与批次 2 部分项已于 2026-10-18 交付（见文末执行记录），批次 3~4 仍在 proposed 状态。
- 对齐按「先补能力、再改写法」两阶段推进：能力缺口进实现队列，写法差异进机制级笔记（`2026-10-18-mechanism-level-alignment.md`）。
- 工具层新增能力（后台任务、git 安全策略、file-state 校验）会改变既有工具语义，需要对应的回归验证。

## Reintroduction conditions

- dynamic-workflow 类脚本化工作流：ZCode 灰度验证默认开启后再评估。
- plugin-creator/plugin 市场：hermix 建立插件分发机制时。
- pandoc：已出局（dsh 引擎替代），不再 reintroduce。

## 执行记录（2026-10-18，批次 1 全量 + 批次 2 部分）

**验证基线**：cargo check --workspace --all-targets 零错误零警告；cargo test --workspace --lib（除 hermix-environments）全部通过（agent 1083 / tools 755+ / core 181 / server 416 等，合计 ≈2700）；tsc --noEmit exit 0。

**已交付（本报告条目 → 实现）**：

- **file_converter PDF→Image 残品重写**（P0）：dsoffice.exe --operation render-images 交互式 line 协议（ready→per-page P …→paint RGBA tile→DONE），BGRA/RGBA 按 documentType 区分，image crate 编码 PNG/JPG，MAX_PAGES=8/dpi=144；端到端 smoke（txt→pdf→png）实测通过。占位符假输出删除。
- **office_export 描述滞后**（P0）：描述改为 xlsx/docx/pptx 三格式现状 + 指路 html_to_docx/file_converter。
- **terminal timeout 毫秒/秒混用洞**（P0，机制对标发现）：旧 schema 写"毫秒 default 30000"，实际单位秒且无 clamp——LLM 按旧文档传 30000 = 8.3 小时超时。schema 改秒语义 + 参数 clamp(1, 120)。
- **Retry-After 解析**（P0，机制对标）：parse_retry_after_ms（header/prose 双模式、retry 动词上下文防"timed out after 30s"误配、5min 上限即视为缺失），重试延迟优先采用。3 测试。
- **流式半截 tool-call 门控补全**（P0，机制对标）：bump_truncated_tool 计数器+配置上限此前从未在生产代码接线（死字段）；确认 run_stream 已有门控、run() 缺失——补齐同款（撤回 assistant 消息 + 计数重试）。
- **read-file-state 会话级读状态链**（P1，ZCode 机制移植）：新模块 file_read_state.rs（mtime/size/partial 指纹，纯函数可测）；read_file/write_file 记录、patch 前门控（NotRead/Partial/Stale 三拒，仅 agent 派发生效、直调兼容）、patch 成功刷新指纹。107 测试过。
- **truncate_middle 头尾保留截断**（P1，Codex 移植）：替换 registry 纯 head 截断（stderr 摘要不再丢失），标记保留头尾。2 测试。
- **rapid-refill 压缩风暴熔断**（P1，ZCode evaluateRapidRefill 移植）：ContextPressureMonitor::record_compaction（5 轮窗口 ≥3 次 → 硬停 + 明确状态文案）。审计"分母未扣 output_reserve"与"无 min-savings 回滚"两盲区经核实为误报（现有代码已覆盖）。
- **HA/bbs 可用性**（评估后维持现状）：照常注册+执行时报缺 env 是有文档先例的刻意设计（homeassistant.rs:15-17 env_deps 机制）——审计判断过重，撤回。
- **workspace 编译修复**：tauri.conf resources glob ** → **/*（tauri bundler 语义）；并行会话 ws.rs 的 json! 缺 import 最小修复。

**未执行（留后续批次）**：文档技能包四件套骨架、superpowers 适配、PlanMode/后台子代理/Explore 档案、任务四套收敛、JSONL 镜像、prefix_rule 审批粒度、claude 侧 P2/P3 全部。

**已知非本 diff 问题**：hermix-environments 5 个 sandbox 测试失败（Win32 error 5 ACL grant，源自并行会话提交 2237ea57 的进程启动方式变更，该 crate 无未提交改动）——需其会话或管理员环境复验。

## 执行记录（2026-10-18 续作增量）

- **JSONL 会话镜像已交付**：session_persistence::save_message_mirror（messages.rs）——DB COMMIT 后增量 append sessions/<sid>.jsonl，行数即高水位，O(delta)；sessions.rs 的 best-effort 文件写入块挂接。测试 jsonl_mirror_is_incremental_and_idempotent 过，session_persistence 43 测试全绿。
- **kanban 裁决修正**：审计称"降为纯 UI 视图"过重——kanban.rs 有 kanban.json 持久化后端（FileKanbanBackend），是完整工具；四套任务机制的真正问题是边界说明缺失，改为"补工具描述边界"而非删功能（待办）。
- **prefix_rule 评估结论（不做）**：hermix 会话放行已按**确认规则族**键控（del / 	askkill-force，engine.rs match_confirm_rule），粒度比 Codex 的 argv 前缀更宽但与族级审批模型一致——非正确性缺口；细化到 argv 前缀属 UX 偏好，记录不实施。
- **后台子代理（真缺口，未实施）**：get_subagent_result 的 "still running" 为前瞻文案；orchestrator（SubAgentOrchestrator）为同步执行。实施路径已定位：① orchestrator 加 spawn_background（cfg 全 Arc/Clone，可 detached）② 全局 BACKGROUND_SUBAGENTS 注册表（id→status/result）③ DelegationBackend trait 加 delegate_background 默认不支持 ④ Signal/Rpc backend 实现 + agent loop delegation 处理改 detached ⑤ delegation 工具加 ackground 参数。跨三层，需单点专项会话完成。
