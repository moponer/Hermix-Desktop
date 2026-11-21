# Decision Note: 继续打磨清单（2026-11-19 全面审计）

Status: proposed — 71 条发现已验证，按 P0-P3 分波执行。第一波（A1/C1/A2/B1/A4）、第二波（T1/T2/T3）与第三波大部分（D1/D2/D3/D4/M3）已于 2026-11-19/20 完成，回归全绿（hermix-agent 1259 过 / hermix-environments 95 过 / tsc 0 错）。**M1/M2 缓做**：四处失败重试纪律与三处验证纪律分属不同路由（各自触发上下文独立），盲目去重会让某些上下文丢规则——需要「共享纪律块 + 上下文特化块」的路由层设计，不是逐处删除。

**2026-11-20 晚间增补**：X3（恢复态透传真实 status）、X1（消息列表窗口化：默认渲染末尾 30 行 +「显示更早的消息」渐进展开，带视口锚定，与服务端历史翻页正交）、A5（JobGuard 增加 disarm，成功路径不再误杀 `start /b` 后台子进程）已完成；C2 复核确认入口已在 `evtType` 单点归一。**A6 缓做**：Windows 路由下 legacy 路径只服务后台命令（立即 return，不进 wait_with_output），有界排空的实际暴露面仅存在于 UNIX 路径，Windows-only 产品暂无风险。剩余：M1/M2（路由层设计）、M4-M6、N1-N5（需产品归一决策）、X2（unified_steps 单一事实源重构）——均为需要独立会话的架构级工作。回归全绿（agent 1265 / tools 831 / core 198 / environments 92 / tsc 0 错）。

> 来源：5 个并行子系统审计（消息流/编排/工具与提示词/沙箱与终端/token 经济），
> 71 条发现全部经 grep/读源验证。按主题归并、按优先级排序。

## P0 —— 稳定性硬伤（先修，全是真实故障源）

### A. 执行与沙箱

- **A1. SANDBOX_VERDICT 缓存投毒**：spawn 阶段任何错误原文进按工作区的记忆缓存，
  若消息不含 `(Win32 error 5|1314|6)` 判据，该工作区所有后续命令**永久硬失败**、连
  非受限降级都不走——终端被砖到重启。（local.rs:971-982, 1038-1043）
  修：缓存里加「可降级」标志；或只缓存 `sandbox_unavailable` 判定为真的错误。
- **A2. pty/background 旁路沙箱**：`pty:true` 与 `background:true` 完全绕过沙箱，
  注释自认「安全语义由审批层承接」而审批层并未管。（local.rs:526, 487-507）
  修：非 danger 模式下这两类也走沙箱判定，或明确拒绝豁免。
- **A3. AppContainer 通道文档自相矛盾**：同一文件里「terminal 沙箱通道走这里」与
  「KNOWN BUG：堆崩溃、禁止接入」并存，极易被后人重新接线引爆。（appcontainer.rs:440-451）
  修：账户沙箱已替代其位置——删除该通道或改写文档为「已封存」。
- **A4. 受限令牌路每条命令先付一次注定失败的 spawn**：先试已知恒败的
  `CreateProcessAsUserW`（1314），残留 `[sbx]` 调试打印进生产。（sandbox.rs:973-1021）
  修：按账户类型直接选 API；删调试打印。
- **A5. JobGuard 无条件 terminate**：正常退出也杀整树，与「start /b 后台子进程」
  语义矛盾；杀树兜底仍按裸 PID taskkill（PID 复用误杀风险），PTY 路无 JobGuard。
  （win_job.rs:231-238, local.rs:752-763, pty_session.rs:748-766）
  修：成功路径 disarm、仅超时/错误 terminate；PTY 统一 attach JobGuard。
- **A6. 非沙箱常规路径无上限缓冲**：沙箱路径有 32MB 有界排空，常规路径拿满全部
  字节后才截断——`dir /s` 级输出先吃爆内存。（local.rs:699-726）
  修：复用沙箱路径的有界排空线程。

### B. 编排与巡检

- **B1. cron runner 无超时/取消/护栏**：`run_job` 裸跑 agent_loop，挂死调用永久
  占住任务。（hermix-cron/runner.rs:127-140）修：套与子代理相同的
  `timeout + InterruptController` 结构，或直接收编进 hermix-automation。
- **B2. 任务仓库无乐观锁**：读-改-写全行覆写，两个子代理并发回写结果互相丢失；
  状态迁移 TOCTOU（`UPDATE WHERE id=?` 不带状态谓词）。（hermix-task/service.rs:115-172）
  修：SQL 加 `AND status=?` CAS；`add_result` 改原子 append。
- **B3. 并发上限可被实例分裂绕过**：`live_sub_agents` 是实例字段而 background_tasks
  是进程全局——reconnect 后新实例从 0 起计数。（sub_agent_orchestrator.rs:444-488）
  修：运行态计数挪进进程全局。
- **B4. 崩溃后自动化 Task 永挂 Pending**：reap 只收 run 行不收 Task。
  （automation_trigger.rs:204-230）修：收割时把关联 Task 一并 fail。
- **B5. stop 覆写已完成任务**：stop 在任务已完成未清理的窗口里把真实工作报成
  cancelled。（sub_agent_orchestrator.rs:764-809）修：覆写前读 lineage，终态直接拒。

### C. 消息流

- **C1. 批量审批的点击可被静默丢弃**：批量路径还留着 `wsRef` 存在性闸门（单条路径
  已改为无条件发送），流已 settle 时点「允许」只打 warn 就丢——agent 循环冻死在
  oneshot 上。（useStreamParser.ts:858-864）修：与单条路径对齐。
- **C2. 协议事件双命名**：每个事件分支写 `snake_case || CamelCase` 两判，10+ 处，
  新事件漏一判静默丢弃。（useStreamParser.ts:702-1118）
  修：入口统一 normalize 一次，分支只认一种。

## P1 —— token 经济（缓存命中率三大杀手）

- **T1. directions 块插在缓存前缀中间**：每轮按 user_message/tool_call_count 变化，
  却夹在 stable 层与 context 层之间——任一 direction 触发变化 = 其后 35K+ 字符
  （context+suffix+tools+全部历史）重新计费。（prompt_assembly.rs:934-936, 839-877）
  修：directions 挪到系统提示**尾部**，stable+context 连成连续可缓存前缀。
- **T2. Anthropic 路径 Tool 消息拿不到 cache_control**：多工具轮次的尾部 tool_result
  链落在最后一个断点之后，逐次调用全价重付。（prompt_assembly.rs:753-765, provider.rs:1472-1482）
  修：断点计入 tool_result；provider.rs Tool 分支透传 cache_control。
- **T3. chars÷4 估算系统性偏差 1.8 倍**：context.rs 里已有 CJK 感知的 estimate_tokens，
  但日志与 turn_report 全用 len()/4——运营按日志调阈值会误判。（prompt_assembly.rs:942-945）
  修：统一改 estimate_tokens。
- **T4. memory/salience 与缓存 epoch 的契约缺失**：内容每轮重算但 epoch 不变 → 模型
  读过期记忆；epoch 每轮 bump → context 层缓存归零。（prompt_assembly.rs:1417-1467）
  修：memory 块移出缓存层、追加尾部；或定义为「渲染一次、会话内冻结」。
- **T5. provider 判定只看字符串包含 anthropic/claude**：provider 留空 + claude 模型
  → 整个断点策略静默失效。（prompt_assembly.rs:732-740）
  修：provider 为空时看 model 名，或 Provider trait 声明缓存能力。
- **T6. 压缩/回收后缓存 reset 无观测信号**：prefix_reuse 日志无法区分「压缩导致」
  与「真回归」。（result_budget.rs, prompt_assembly.rs:681-693）
  修：PrefixState 加 generation 计数，日志标注 post_compaction。

## P2 —— 冗余与误导（删/合并，直接省 token）

### 可直接删（引用已不存在机制的陈旧物）

- **D1.** 提示词推荐已停用工具：`storage_analyzer`/`disk_cleanup` 已停止注册，
  ENVIRONMENT_SHELL_GUIDANCE 仍列为「结构化首选」；storage-analyzer 技能同死。
  （directions.rs:585-588, tools/mod.rs:110-111）
- **D2.** 三个工具 schema 引用已删除的 `computer_use`；`computer_use`/
  `process_registry` 还留在 tool_call_parser 的 known_tools 名单。
- **D3.** 死组件：settings/EditProfileDialog.tsx（0 引用）、ApprovalCard（legacy
  死路径）、closeOpenReasoning（被 closeAllOpenReasoning 取代的旧版）。
- **D4.** ConfirmRule 文档与实现相反（「同家族全放行」vs 已改为命令签名记忆）。

### 需合并（同一件事多处各说一遍——stable 层每请求重复付费）

- **M1.** 「同一动作失败别试第三次」在 4 处重复（directions.rs:31/149/516/577）。
- **M2.** 「完成前必须验证」在 3-4 处重复（VERIFICATION_HONESTY/PHASE/CODE_DEV/TOOL_FAILURE）。
- **M3.** 技能指导双块（SKILLS_GUIDANCE + SKILL_USAGE_GUIDANCE）各解释一遍机制，
  后者还用 Python 假语法。
- **M4.** todo/task_tracker/kanban 三套任务工具 + 两块 guidance 有逐字重复句。
- **M5.** browser-automation 技能（268 行）与 WEB_AUTOMATION_GUIDANCE 大面积重叠
  且已分叉（技能说内嵌 web_open 是主路径，提示词说 web_extract 首选）。
- **M6.** 错误文案与错误分类两套并行正则表（streamEvents.ts:43-179）——抽单表。

### 需归一决策（先定方向再删）

- **N1.** 三条目录列举通路：list_dir / list_files（自认防幻觉 alias）/ search_files。
- **N2.** 两条内省机制：list_tools（全量 dump）vs tool_search/tool_describe 渐进披露。
- **N3.** 同名技能双源分叉：custom-app-development（v16 vs v11，225 行 vs 683 行）。
- **N4.** 审批 UI 四处实现：ApprovalBubble / InlineApprovalCard / ApprovalBatchBubble
  / ApprovalDrawer——收敛到一个决议面 + 一个容器。
- **N5.** 工具结果截断标记 5 种格式、错误载体 3 种风格——统一协议。

## P3 —— 体验与性能

- **X1.** 消息列表无虚拟化：全部历史行 + �条 1019 行 MarkdownBody，长会话全量提交 DOM。
  （MessageList.tsx:382-443）修：基于已有分页做窗口化。
- **X2.** 双份时间线（execution_timeline + unified_steps）+ 流中/落定/重载三条渲染缝
  靠镜像函数缝合——unified_steps 应成唯一事实源，其余派生。
- **X3.** 恢复态把所有 tool_calls 强制改写 done——被中断/失败的历史调用重载后显示
  已完成（误导）。修：透传真实 status，修 StatusPill 的 undefined 分支。
- **X4.** 前端启发式正则判「未打标签推理」会吞用户正文——判定应只在后端落一次，
  前端只信 wire 字段。（protocolAdapter.ts:344-444）
- **X5.** 排队态只有一次性 toast，用户误读为卡死——streamingStatus chip 渲染持续可见
  的「已排队」态。
- **X6.** MemoMessageBubble 手工比对 15 个 prop 已漏两次——收敛 props 或用默认浅比较。
- **X7.** 列表 10 处 index 作 key——改稳定业务 id。
- **X8.** Done 收尾 145 行堆在 App.tsx 单回调（3883 行根组件）——抽 finalizeTurn()
  并按步骤容错。

## 建议执行顺序

1. **第一波（稳定性，1-2 天）**：A1 → C1 → A2 → B1 → A4（全是小改动大收益）。
2. **第二波（token 经济，2-3 天）**：T1 → T2 → T3 → T5 —— 前两个直接决定每次
   API 调用的计费结构。
3. **第三波（删冗余，1 天）**：D1-D4 直接删（省 token 不损能力）；M1-M6 合并；
   N1-N5 逐个做归一决策（每个都需要一次产品判断）。
4. **第四波（体验）**：X2（单一时间线）→ X1（虚拟化）→ X3/X4（去误导）。

## 量化预期

- token：T1+T2 落地后，多工具轮次的 API 计费结构从「每轮全价」变为「增量计费」，
  叠加 D 类删除（stable 层预计缩 15-25%），单会话综合节省 30-50%。
- 稳定性：A1/A2/C1/B1 消掉四类「需要重启才恢复」的故障。
- 一致性：M/N 类合并后，系统提示单一真相源，模型不再读到互相矛盾的指令。

## Alternatives considered

- **只修稳定性不删冗余**：D 类陈旧提示每次请求都付费且误导模型，只修不删会让
  token 经济目标落空——故 D/M/N 与稳定性并列为独立波次。
- **先做体验（X 类）再做 token 经济**：X 类改善可感但 token 结构问题（T1/T2）
  决定每次调用的计费形态，修复窗口更小、收益更大，故排前。
- **一次性重写系统提示**：风险远大于逐块合并；且 directions 路由体系刚建好，
  重写会拆掉其验证结构。选逐块收敛。

## Consequences

- P0/P1 落地前，内置管理员机器上的终端在「缓存投毒」触发后仍需重启恢复（A1）。
- N 类归一（目录列举/内省/审批 UI）会改变部分模型可见的工具面，需要一并更新
  提示词与技能引用，工作量大于表面删除。
- 测试环境的 DSH Low 标签与 tempfile 链属于环境属性，相关断言已按真实产品语义校准。
