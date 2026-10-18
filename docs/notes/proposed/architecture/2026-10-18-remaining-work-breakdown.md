# 剩余工作拆分：5 张可独立执行的任务卡

Status: proposed — 每张卡自包含（范围/文件/步骤/验收/风险），一个会话只做一张。上游依据：`2026-10-18-capability-alignment-audit.md`、`2026-10-18-mechanism-level-alignment.md`。

## 任务卡 1：后台子代理（能力缺口，最高优先）

**目标**：`delegate_task` 支持 `background: true`——立即返回 `sub_agent_id`，子代理 detached 执行，`get_subagent_result` 轮询取结果。

**现状事实**：orchestrator 同步（`sub_agent_orchestrator.rs`）；`get_subagent_result` 的 "still running" 是前瞻文案；`tool_executor` 在 delegate_task 到达 backend 前拦截并同步执行。

**步骤**：
1. `SubAgentOrchestratorConfig` + `SubAgentOrchestrator` derive `Clone`（字段均 Clone/Arc）。
2. 全局注册表：`static BACKGROUND: OnceLock<Mutex<HashMap<String, BgEntry>>>`，`BgEntry{status: Running|Done(String)|Failed(String), task, started_at}`。
3. `impl SubAgentOrchestrator { pub fn spawn_background(&self, request) -> Result<String, ToolError> }`：生成 id → 注册 Running → `tokio::spawn` 执行现有 execute 路径 → 终态写注册表 + 现有 lineage JSON。
4. `DelegationBackend` trait 加 `delegate_background(...)`，默认实现返回"不支持"；`RpcDelegationBackend` 返回不支持；真正的实现走 `tool_executor` 拦截层（与同步路径同址）。
5. `delegate_task` schema 加 `background: boolean`（默认 false），返回 `{"status":"spawned","sub_agent_id":…}`。
6. `get_subagent_result` 先查注册表（内存），miss 再查 lineage 文件。

**验收**：单测——spawn 后立即返回 id；轮询到 Done 拿到结果；失败态可见；父 agent 停止不杀后台任务（或明确记录为取消）。

**验证**：`cargo test -p hermix-agent --lib sub_agent` + `cargo test -p hermix-tools --lib delegate` + workspace check。

**风险**：tokio::spawn 的 future 需 `'static` + `Send`——orchestrator 内若混入非 Send 字段（如 `rusqlite` 连接）需 clone 出 Arc 句柄；取消语义（父 interrupt 是否传播到后台）必须显式决定。

---

## 任务卡 2：PlanMode 工具对

**目标**：`enter_plan_mode` / `exit_plan_mode` 两工具 + 计划模式下的写操作拦截。

**步骤**：
1. `AgentOverrides.plan_mode: Option<bool>` → `AgentConfig.plan_mode`。
2. 两工具注册（`tools/plan_mode.rs`）：enter 只置状态并返回"已进入计划模式"，exit 返回计划摘要提示。
3. dispatch 层：`plan_mode == true` 时写类工具（write_file/patch/terminal 非只读/file_converter 写盘）拒绝并提示"计划模式只允许只读调研，先 exit_plan_mode"。
4. system prompt 加一段计划模式说明（directions.rs）。

**验收**：单测——plan_mode 下 write_file 被拒、read_file 放行；exit 后写恢复。

**验证**：`cargo test -p hermix-tools --lib plan` + agent lib。

---

## 任务卡 3：Skill 的 allowed-tools 强制（死字段激活）

**目标**：SKILL.md frontmatter 的 `allowed-tools` 从"解析但不强制"变为真实约束。

**现状**：`skill_orchestrator::SkillFrontmatter.allowed_tools: Option<Vec<String>>` 已解析；`skill_loader::match_message` 不返回它；dispatch 不知道。

**步骤**：
1. `skill_loader` 匹配结果携带 `allowed_tools`（附加字段，不破坏现有调用）。
2. `LoopState` 加 `active_skill_allowed_tools: Option<HashSet<String>>`，pre-turn 自动激活时写入（取并集；空集=不限制）。
3. dispatch 前过滤：存在限制时仅放行白名单内工具 + 元工具（skills_list/skill_view/todo）。
4. 明确优先级：专家 toolset ∪ 用户白名单 ∩ skill 限制？**按 ZCode 语义：skill 限制只做减法，不扩大既有白名单。**

**验收**：单测——限制生效、元工具不受影响、无 allowed-tools 的 skill 不改变行为。

**验证**：`cargo test -p hermix-agent --lib skill` + tools lib。

---

## 任务卡 4：文档技能包（docx 先行）

**目标**：`skills/docx/SKILL.md` 方法论 + Python 脚本 + env_check；PDF 渲染复用已落地的 dsh 引擎。

**步骤**：
1. 自写 SKILL.md（**不逐字搬运 ZCode**——其 frontmatter 标 Proprietary）：结构 = 快速开始 / 任务路由（新建/编辑/导出 PDF/视觉校验）/ 排版规则 / 生成后检查清单。
2. `scripts/docx_build.py`（python-docx）：标题层级、列表、表格、页眉页脚、目录占位；`scripts/docx_check.py` 生成后自检（段落数、缺字体、超宽表格）。
3. `env_setup`：python 检测 + `pip install python-docx`（清华镜像 fallback），缺 python 时给出安装指引。
4. 与工具的分工写进 SKILL.md：小转换用 `html_to_docx`，高质量排版用本技能；PDF 导出用 `file_converter`（dsh 引擎）。
5. 打包：`tauri.conf.json` resources 加该 skill 目录。

**验收**：技能可被 skill_loader 激活；脚本在装有 python 的机器上生成可打开的 docx。

**验证**：手工跑生成脚本 + `cargo test -p hermix-tools --lib html_to_docx` 回归。

---

## 任务卡 5：superpowers 适配 + 任务面收敛

**目标**：引入工作流方法论 skill（TDD/系统化调试/完成前验证）+ 厘清 todo/task_tracker/kanban 边界。

**步骤**：
1. **许可核查**：`obra/superpowers`（若 CC-BY / MIT 可改写；不确定则自写）。先只做 3 篇：`test-driven-development`、`systematic-debugging`、`verification-before-completion`。
2. 自写版本放进 `builtin-skills/hermix/workflow/`，与 ponytail 同机制（include_str! + 首启写入）。
3. 任务面收敛**不删功能**：`kanban.rs`（有 kanban.json 持久化）与 `task_tracker.rs`、`todo.rs` 三者描述互加边界说明（TodoWrite=会话内清单；kanban=跨会话看板；task_tracker=后台任务生命周期）。

**验收**：3 篇 skill 可激活且不超元数据预算；三工具描述不再语义重叠。

**验证**：`cargo test -p hermix-tools --lib` + skill_loader 单测。

---

## 执行顺序建议

1 → 3 → 2 → 4 → 5（按“能力缺口价值 / 改动风险”排）。每张卡完成即更新本笔记状态并写 `implemented/` 对应笔记。

## 状态更新

- **任务卡 1（后台子代理）：已完成（2026-10-18）**。
  - SubAgentLineage 新增 esult / 	runcated / inished_naturally（此前 get_subagent_result 的 esult 恒为 null——真 bug 修复）。
  - SubAgentOrchestrator::spawn_background()：先写 started lineage（poll 立即可见）→ 	okio::spawn detached 执行 → 终态含结果文本落盘；返回 BoxSendFuture 打破递归 future 环。
  - xecute_inner 拆为 xecute_inner_with_id(req, id, announce)，同步路径行为不变。
  - 	ool_executor 拦截层识别 ackground: true → 立即返回 {"status":"spawned","sub_agent_id":…}。
  - delegate_task schema 新增 ackground 参数；get_subagent_result 输出补 	runcated/inished_naturally。
  - 测试：spawn_background_returns_id_immediately_and_records_result（id 立返 + started 文件即存 + 轮询到 completed + result=="done"）；agent 1085 / tools 759 全绿。
  - 已知边界：父代理中断时 parent_interrupt watcher 仍会打断后台子代理（沿用现有取消语义，未改）；后台任务的 focus 栈帧由终态事件弹出（与同步路径一致）。

- **任务卡 3（Skill allowed-tools 强制）：已完成（2026-10-18）**。
  - SkillCommandInfo / SkillMatch 携带 llowed_tools（frontmatter 已解析字段，此前从未向下传递）。
  - SkillLoader::build_injection_and_tools()：一次匹配同时产出注入文本与 allowed-tools 并集；uild_injection 委托它保持兼容。
  - un_loop::build_active_skills_block_with_tools()；LoopState.active_skill_tools 每轮重算（不会串轮）。
  - 强制执行：do_tool_turn 过滤非白名单工具（返回 skill_scope 拒绝信封）；	ool_executor::tool_allowed_by_active_skill 纯函数 + SKILL_SCOPE_ALWAYS_ALLOWED 元工具豁免（skills_list/view/manage、todo、clarify、get_subagent_result）。
  - 语义：**只做减法**（空声明=不限制；不扩大既有会话白名单）。
  - 测试：skill_scope_is_subtract_only_and_exempts_meta_tools、uild_injection_and_tools_surfaces_allowed_tools、uild_injection_and_tools_is_none_without_declaration；agent 1088 全绿。

- **任务卡 2（PlanMode）：已完成（2026-10-18）**。
  - hermix-tools/src/tools/plan_mode.rs：nter_plan_mode / xit_plan_mode（RiskLevel::Safe）+ is_blocked_in_plan_mode() 拒绝清单（terminal/execute_code/write_file/patch/move_file/file_converter/office_export/html_to_docx/cronjob/workflow/custom_app_manager/kanban/win_*/browser_click/browser_type/vault_*）。
  - 共享句柄：ToolRegistryInner.plan_mode: Arc<RwLock<bool>> + plan_mode_handle() / plan_mode_active()；轻量 gent_helpers::ToolRegistry 加同名 handle（Clone/without_tools/retain_only 共享同一 Arc），ridge_tool_registry 桥接时 set_plan_mode_handle。
  - 派发门禁：do_tool_turn 与 skill scope 合并为一次分区（plan_mode 优先），返回 plan_mode 拒绝信封，不弹审批卡。
  - 每轮提示：pre_turn 注入 [PLAN MODE] 系统提醒（近 8 条消息已有则跳过）。
  - 设计取舍：**拒绝清单而非允许清单**（新工具不会被静默锁死）；不新增 AgentOverrides.plan_mode（前端开关留待需要时接）。
  - 测试：locked_list_covers_mutators_but_not_readers、nter_and_exit_toggle_the_shared_flag；tools 761 / agent 1088 全绿。

- **任务卡 4（文档技能包 docx 先行）：已完成骨架（2026-10-18）**。
  - uiltin-skills/hermix/docx/SKILL.md（include_str! 内置，首启写入 HERMIX_HOME/skills）：三路线选择表（html_to_docx 快路径 / office_export 结构化 / python-docx 全量）、环境检查（python-docx + 清华镜像）、可直接改用的 python-docx 模板（含 w:eastAsia 中文字体双设置、A4 页边距、首行缩进、定宽表格）、六条硬规则（字体/分页/TOC 域/F9/表格宽度/图片宽度/备份）、**渲染验证闭环**（dsh 引擎 docx→pdf→png + read_image 目检）。
  - 取舍：脚本不随包分发（agent 落工作区生成），避免打包链引入 python 依赖；docx/pptx/xlsx 的脚本化生成留待 v2。
- **任务卡 5（superpowers 适配 + 任务面收敛）：已完成（2026-10-18）**。
  - 自写三篇工作流 skill（**不搬 obra/superpowers 原文，规避许可不确定性**）：workflow-tdd（红-绿-重构 + 本仓快循环命令 + 反模式）、workflow-debugging（复现→定位→解释→修因→验证 + 本机日志关键字）、workflow-verification（证据阶梯 + 完成前清单 + 诚实规则 + 输出模板）。均已 include_str! 注册。
  - 任务面边界（不删功能）：	odo（会话内任务图）、	ask_create（持久任务记录+子任务+交付物）、kanban（用户跨会话看板）三处描述互加 BOUNDARY 仲裁条款；并修正 	odo 描述与实现不符（原写"task orchestration"却只有会话态）。
  - 验证：cargo check --workspace --all-targets 零错误零警告；tools 761 / agent 1088 全绿；desktop crate 重新编译通过（include_str 校验）。

## 全部任务卡状态

1. 后台子代理 — ✅ 完成
2. PlanMode — ✅ 完成
3. Skill allowed-tools 强制 — ✅ 完成
4. 文档技能包 docx — ✅ 骨架完成（脚本化 v2）
5. superpowers 三篇 + 任务面边界 — ✅ 完成

## 遗留项第二批（2026-10-18 夜）

- **C1 流式 idle 真中断：已完成**。新增 AgentConfig.stream_idle_timeout_secs（默认 120，用户可在 gent.stream_idle_timeout_secs 配置，gent_builder 桥接）。stream_collect.rs 两个分支（activity=0 / 有通知）均：每 chunk 重置 idle 定时器；首个 chunk 后静默超过期限 → 返回 Interrupted(finish_reason="stream_idle") 并**保留已到达的部分内容**（旧行为：24h 空转 → 外层 180s → 空回复丢弃用户已见文本）。测试 idle_after_first_chunk_aborts_with_partial_content。
- **C2 read_file 分页精度：已完成**。抽出纯函数 paginate_read_body(content, start_line, budget)：按**整行**截断（旧版切在半行中），
ext_offset = 调用方坐标系下的下一条行号（旧版错误地报总行数，指到已读内容之后）；超长单行回退字符级截断。3 个测试（含 offset 坐标系偏移与未超限直通）。count_lines 降为 #[cfg(test)]。
- **B1 dashboard seeded 假插件清算：已完成**。新增 SEEDED_PLUGIN_IDS: &[(id, 原因)]（8 项逐条写明缺什么），并加不变量测试 seeded_ids_match_the_catalog：catalog 标 seeded 却不在清单 → 失败；清单项变 eady → 失败。挂账从"沉默"变为"CI 可见"。
- **A1 docx 脚本随包分发：已完成**。uiltin-skills/hermix/docx/scripts/build_docx.py（spec JSON → docx，内化中文字体双设置/页边距/首行缩进/定宽表格/页码域；支持 --check）；lib.rs 新增 BUILTIN_SKILL_EXTRA_FILES 未存在即写（不覆盖用户改动）；SKILL.md 改为"优先用随包生成器"。
- **A2 前端 PlanMode 开关：不做（记录理由）**。egistry.plan_mode 是进程级共享句柄（子代理与桥接共享同一 Arc）；per-session override 在两会话并发时会串扰（A 开计划模式 → B 的写操作被拒）。正确实现需要 per-session registry inner 拆分（结构性改动），当前 agent 工具自服务路径已覆盖实际需求。
- 顺带修复并行会话引入的门禁阻断：pairing.rs 测试缺 ToEncodedPoint import（E0599）、
et_watch.rs 对 Copy 句柄 mem::forget 警告（改 let _ = handle; 并注明原因）。

**验证**：cargo check --workspace --all-targets 零错误零警告；tools 763 / server 443 / agent 1089 全绿；sealed-pairing 测试单跑通过。
