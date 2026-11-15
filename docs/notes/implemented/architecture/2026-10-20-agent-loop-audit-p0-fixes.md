# Decision Note: Agent Loop 审计后的 P0 / P1 修复

Status: implemented — 见 Decision 1–27

来源：2026-10-20 对 agent 循环四维度（工具执行 / 上下文与缓存 / 资源占用 / 持久化）的源码审计。P0 是正确性缺口，P1 是一致性与体感问题。

**明确不做（含理由）**：日志层异步化（`crash.log` 必须同步落盘，异步队列会丢掉崩溃前最后几行；按 category 分流是独立决策，未做）、PTY 改 Job Object（用 `taskkill /T` 覆盖，见 Decision 11）、`.md`/trajectory 改增量写（人读导出，append 会破坏格式，见 Alternatives）、JSONL 镜像的重建入口（镜像已自愈 + fsync；从损坏 DB 恢复需要 app 根本起不来，命令行入口又因 `windows_subsystem = "windows"` 看不到输出，做出来是半成品）、把 `web_search`/`browser_*` 提为 resident（省 token 是渐进式披露的存在理由，先修反馈闭环，见 Decision 15 的 Alternatives）。**保留的既有偏差**：`approval` 行在比对集内导致该场景退化为尾部重写（效果与旧实现等价）。

## Decision

### 1. Anthropic 的 `system` 参数按消息分块，只有首块带 `cache_control`

`AnthropicProvider::convert_messages`（`crates/hermix-agent/src/provider.rs`）不再把所有 `System` 消息拼成一个字符串，而是每条消息一个 text block；`cache_control` 只加在**第一块**（稳定层：soul + directions + 工具文档）。`apply_prompt_cache_markers`（`crates/hermix-agent/src/prompt_assembly.rs`）的尾部断点预算从 4 降到 3（1 + 3 = Anthropic 的 4 个上限）。

根因：agent loop 在 `pre_turn` 每轮追加 `System` 消息（recall / 技能正文 / PLAN MODE / prefetch / 审批确认）。拼接后 `system` 每轮都变，而 Anthropic 按**前缀**匹配缓存，于是 system 断点不命中，其后的消息断点也全部失效——Anthropic 用户的缓存命中率实际趋近 0。

### 2. 上下文预算按「整条消息」计价

`context::estimate_message_tokens` 取代 `estimate_tokens(&content)`，把 `tool_calls[].arguments` 与 `content_parts` 一并计入；图片按每张 1500 token 计（视觉计费按切块，与 base64 字节数无关）。`add_message` / `recompute_total_tokens` / `truncate_to_budget` 全部走这一个函数。

根因：一个 `write_file` 或 500 行 `patch` 的参数是 25k+ token，旧计数器记 0 —— 大工具参数的会话永远不触发压缩，直接撞 provider 硬上限。

### 3. 写类工具在独立任务里执行，受 per-call 预算约束

`tool_executor` 的串行段（`execute_bounded`）（`write_file` / `patch` / `terminal` 等 `is_state_mutating_tool`）改为 `tokio::spawn` + `tokio::time::timeout(per_tool_budget)`；`clarify` 这类交互工具仍内联执行（等待用户的时间本来就没有上界）。超时与 panic 的结果文案标注「状态未知，重试前先确认影响」。

根因：该段此前直接调用同步 handler，位置又在 per-call 与 batch 两个 deadline **之前**，所以一个挂死的写工具可以冻结整个 turn，用户点停止也没有中断点。

### 4. JSONL 镜像的高水位是「最后一条消息的指纹」，不是行数

`save_message_mirror`（`crates/hermix-agent/src/session_persistence/messages.rs`）改为：读尾部 16KB 取最后一条完整行 → 先截掉半截行 → 用 (role, char 长度, tool_call_id, content 前 96 字符) 指纹在内存时间线里定位 → 从其后追加；定位不到就整份重写。追加后 `sync_all`。

根因：压缩与 `/undo` 让时间线变短，旧逻辑 `persistable.len() <= already` 直接 return，镜像从此**永久停写且无声**；进程被杀留下的半截行还会被算作一行，把后续追加的偏移整体错位。现在每种失败模式都会在下一次调用时自愈——这也是重写不采用原子替换的前提：DB 是真相源，半份镜像是可修复状态。

### 5. 权限类配置文件禁止由工具写入

新增 `hermix-tools/src/permission_guard.rs`：`approval_allowlist.json` / `session_approvals.json` / `enabled_toolsets.json` 在任何位置命中即拒绝；`config.yaml` 只在 app home 内拒绝（它是通用文件名）。接入 `write_file`、`patch`、`move_file` 的 `dst`，以及 `ApprovalEngine::check_approval` / `check_approval_for_origin_with_scopes` 的命令串筛查。判定是**硬拒绝**，不是审批卡片。

根因：这些文件决定「什么命令不需要问用户」。能写它们就能把「用户批准过一次 `cargo build`」升级为「今后所有命令预批准」，下一条 `rm -rf` 不会再到人眼前。审批卡在这里不成立——请求方正是受益方，用户看不出「真实配置变更」和「自我提权」的区别。

### 6. 一次工具结果不再独占六分之一窗口

`result_budget`：`READ_FILE_CHARS` 300 K → 80 K（≈20 K token），`TERMINAL_OUTPUT_CHARS` 100 K → 60 K（≈15 K token）。`read_file` 有 `next_offset` 分页，"多一次调用"比"吃掉窗口三分之一"便宜。

### 7. 截断只有一条规则：保头保尾

`budget::truncate_result` 改为委托 `hermix_tools::registry::truncate_middle_chars`；`dispatch_async` 的纯头部截断同步改为中段截断。此前同一份输出在同步/异步路径下被截成不同形状，而纯头部截断丢掉的正是退出码、断言、栈帧——命令输出里唯一要紧的那几行。

### 8. 网关预热安全网不再产出非法 payload

`gateway_token_warn` 的 token 估算改用 `estimate_message_tokens`（原 `len/4` 只看 content）；裁剪改为保留「前导 System 消息 + 尾部 10 条」，并用新提取的自由函数 `compression::engine::sanitize_tool_pairs` 修复配对。原先 `messages[..3]` 硬切可能截断一个 `tool_use` 组，尾切片又可能从组中间开始，产出 Anthropic 直接拒收的孤立 `tool_result`。

### 9. 持久化调用不再占住运行时 worker

`session_persistence::run_blocking`（`block_in_place`，current-thread 运行时回退直调）包住三处 `persist_session_with_compression` 调用点。该调用是同步 SQLite + 文件 IO，且 BUSY 时 `thread::sleep` 重试最长 2 s+——在 async 处理器内直接调用会占住 worker，长到错过前端 15 s 心跳。

### 10. SQLite 连接统一出入口

`hermix-vault`、`hermix-tools`（session_search 只读）、`hermix-automation` 三处裸 `Connection::open` 改走 `hermix_store::open_connection`（WAL + busy_timeout 口径统一，读用短超时）。`automation` 保留自己的 `PRAGMA foreign_keys = ON`——那是连接级状态，本 crate schema 依赖它。

### 11. PTY 超时清整棵进程树

`pty_session` 超时路径在 `killer.kill()`（只杀顶层）之后补 `kill_process_tree(pid)`（`taskkill /T /F`）。超时的 `cargo build` 会留下 rustc 继续吃 CPU 并占着 target 目录锁，用户感受是"用久了机器变慢"，很难联想到某次工具超时。

### 12. 会话消息增量落盘（前缀合并 + watermark）

`flush_messages` 不再每轮 `DELETE` 全表 + 全量 `INSERT`，改为：

1. **快路径**：读回 `sessions.message_count`（上一轮写下的时间线长度），比对行数、以及首尾两条行的指纹。全部一致则存储时间线就是内存时间线的前缀，**不读历史**，直接追加尾部。
2. **慢路径**（任一校验失败：压缩、`/undo`、跨进程改动、volume 漂移）：全量读行、算公共前缀，只删除 `prefix` 之后的残留行，再追加。

两种路径都只动"变化了的那一段"。前缀行不重写，因此其 `id` 不变、其 `unified_steps` 不被覆盖（旧的 ordinal carry-forward hack 因此整体删除）。

**为什么 id 稳定性是硬要求（扫描结论）**：数据库 `id` 本身没有泄漏到任何 API；泄漏出去的是**位置**——wire id 是 `{session_id}-{idx}`（`routes.rs:1269`），前端拿它当 React key（`MessageList.tsx:397`）、翻页去重键（`App.tsx:1217`）、折叠态 key（`TextBubble.tsx:251`）、todo 卡片锚点（`todo.rs:38`）。所以契约是「行序 + 行数」，保持前缀不动就保住了它。

**压缩的删除语义没变**：旧实现的 DELETE 是压缩落地的唯一机制（压缩不写 `active=0`）；新实现在 prefix 之前不动、之后删除，效果与整表 DELETE 相同，只是范围收窄到真正变了的部分。

### 13. 每轮不再写两遍 markdown 导出

`save_session_atomic` 增加 `write_session_log` 开关；新增 `persist_pre_turn`（DB 事务完全相同，只跳过 `<id>.md` 全量渲染）。agent loop 的预保存改用它。

一轮会持久化两次：预保存（保证 `run()` 失败也不丢用户输入）与运行后保存。`.md` 是把整段对话重新渲染的产物，预保存写的那份在几秒后就被覆盖——这是增量持久化之后剩下的唯一一个"每轮整文件写"。

### 14. Fs 接缝的 `read_file` 拒绝二进制

接缝路径此前用 `String::from_utf8_lossy` 解码，二进制文件（PNG/docx）会变成一面 U+FFFD 墙进上下文。改为 `String::from_utf8`，失败时报错并指路 `read_image` / `read_pdf` / `file_converter`。真实文件路径早就这么做（`local.rs` 的 `read_to_string` 失败 + image magic 检测），接缝是唯一漏的口子。

### 15. 参数错误从死胡同变成可执行纠正（2026-10-20 实测回归）

实测案例：让 agent 访问超星学习通并完成学习/作业，它连续多轮调用 `web_search`、`url_safety`、`web_extract`、`browser_*`，**每次都不带必填参数**，报 `Missing 'query' parameter` 后重试还是同样错，整轮空转；期间它还在用 `search_file` 在代码里搜工具名、用 `list_dir` "查看 web_search 目录"。

三层根因，缺一层都不会这么严重：

1. **能力地图点名了 schema 不可见的工具**。deferred 工具数 ≥ 25 时 `assemble_tool_schemas` 把 `web_search` / `browser_*` / `url_safety` 等撤出本轮 `tools` 数组，只留 `tool_search` / `tool_describe` / `tool_call` 三桥；而 `directions.rs` 的能力地图仍无条件点名这些工具。模型看得见名字、看不见 schema，只能猜参数名。
2. **没有任何地方教它用桥**。`directions.rs` 此前完全没有 `tool_search` 的使用说明。
3. **错误不可执行**。`Missing 'query' parameter` 不含参数契约、不含示例、不指向桥——重试只能靠再猜。

修复（三处，都不改渐进式披露本身）：

- `hermix_tools::registry::enrich_param_error`：`InvalidParams` 错误附上该工具的必填/可选参数清单 + 调用示例 + 「若不在 tools 数组，先 `tool_describe`」指引。接入全部 5 个 dispatch 路径（`dispatch` / `dispatch_async` / `dispatch_async_with_context` / `dispatch_with_context`）与 agent 侧 `tool_executor` 的两处 handler 调用。
- `directions.rs` 新增 `TOOL_DISCOVERY_GUIDANCE`（always-on）：三步桥流程 + "调用前先 describe，参数名必须来自 schema"硬规则。
- 修正能力地图的结论句：看不到 `browser_*` 时先说"先用 tool_search 确认是否只是被桥接"，而不是直接让用户去开工具集（工具集本来就开着，这句话会把用户导向错误操作）。

### 16. 技能路由阈值：先量化，再动阈值（2026-10-21）

新增量化基准 `crates/hermix-agent/tests/skill_routing_bench.rs`（18 条确定性语料、阈值扫描 0.00–1.00、分数分离度、命中来源分解）。数据推翻了两个想当然：

- **误激活全部来自语义向量层**，不是关键词/模糊匹配。10 个负样本的 `matched_on` 清一色 `vector`，余弦 0.19–0.38；keyword / CJK / fuzzy 三路**零误激活**。
- **纯调阈值原本无法同时为 0**（正样本最低 0.25 vs 负样本最高 0.38，重叠）——但那是因为把向量噪声算进了"正样本下限"。分层看就没有重叠。

修复两处：

1. `semantic_min_score` 0.18 → **0.45**。0.18 低于该 hashing bigram embedder 对任意英文短文本的噪声底噪（~0.2–0.4），等于不设防。代价写明在注释里：CJK 释义召回退化（那些余弦本来就在 0.15–0.25），要恢复必须先量化"仅靠向量命中的正样本"分数下限。
2. `semantic_threshold` 的条件互换 → `max()`。旧写法 `if min_score > 0.3 { min_score } else { semantic_min_score }` 在 operator 取 0.30–0.45 时会把向量阈值**降**到 min_score（0.35 < 0.45），扫描在 0.35 档抓到一次误激活反弹。严格度只能加、不能减。

结果：误激活 **3/10 → 0/10**（且 0.00–0.95 全档位为 0），漏激活 1/8（纯 CJK 查询，`cjk_bigram_hit` 只与含 CJK 的 tag/name 比对，而语料技能 tag 是英文——设计限制，不是阈值问题）。`p2_skill_activation_test` 的历史失败随之转绿。

### 17. terminal 前台预算 120s → 600s

内部硬上限 `TERMINAL_HARD_TIMEOUT_SECS` 120 一直比 `per_tool_budget` 给的外部 600s 先到期：跑构建/装依赖/长测试总在第 120 秒被掐断，外层还有 480s 余量用不上。改为对齐外部预算，schema 描述同步。更长任务仍走 `background` + 轮询。

### 18. 自动沉淀 skill：把写好的代码接进 run loop

`skill_auto_create::{should_auto_create_skill, write_auto_skill}` 早就写好（slug 清洗、不覆盖手动 skill、16KB 上限齐全），但**从未被调用**——"总结经验为 skill"因此完全依赖模型自愿调 `skill_manage`，用户明确要求沉淀时经常什么都不留下。

接到 `finalize_agent_result`（run / run_stream 唯一收尾点）：

- 门槛：自然完成 **且** 全程无工具错误 + `should_auto_create_skill`（≥5 次调用、≥2 种工具、主导 ≤80%）。带错的序列不是可复用配方。
- 正文用**确定性模板**从 assistant 消息的 `tool_calls` 渲染（顺序 + 工具名 + 参数截断到 200 字符），**不追加 LLM 调用**：收尾多一次 round-trip 直接加到用户等待上，而"按序调了哪些工具"本身就是比 LLM 摘要更可靠的 runbook。
- 修掉一个真问题：中文任务名过不了 ASCII slug 规则，全部退化成 `auto` → 第二个中文任务就撞名静默丢失。改为退化时用**工具序列**命名（`read-file-terminal`）。
- 落盘后发一条 lifecycle 状态，用户能看到"已沉淀为技能 `/<slug>`"。

端到端证明在 `crates/hermix-agent/tests/auto_skill_e2e.rs`：6 次调用 / 2 种工具 → `~/.hermix/skills/auto/read-file-terminal/SKILL.md` 真的生成，含任务原文与编号步骤；单工具会话反向不生成。

### 19. `serde_json::Map` 索引赋值 panic（2026-10-21 实测崩溃）

用户报 `Tool execution error: internal error (panic): no entry found for key`，连续三次。panic hook 落盘的 backtrace 直接指到 `local_agent_service.rs:1307` 的 `apply_overrides`。

根因是一个**容易踩且踩了很难自查**的 API 语义差异：

- `serde_json::Value` 的 `IndexMut` = `index_or_insert` → key 不存在时插入 Null，**不 panic**。所以 `body["tools"] = …` 这类写法（`provider.rs`、`api_bridge.rs` 里大量存在）一直是安全的。
- `serde_json::Map<String, Value>` 的 `IndexMut` = `get_mut(k).expect("no entry found for key")` → key 不存在就 **panic**。

`apply_overrides` 把 `extra_body` 解构成了 `&mut Map`，于是只要 `extra_body` 里已有别的键（如 `reasoning_effort`）再开 `enable_thinking`，100% 崩。改 `Map::insert`。

全仓扫描后确认：**只有这一处**是 Map 上的索引赋值；其余 `as_object_mut()` 之后全是 `.insert()` / `.entry().or_insert()`，安全。新增测试 `apply_overrides_merges_enable_thinking_into_existing_extra_body` 钉住"合并进已有 extra_body"这个形状。

顺带修了误导性的错误归类：`ws.rs` 的 catch_unwind 把**整个 agent task** 的 panic 一律包成 `AgentError::ToolExecution`，Display 出来就是 "Tool execution error"——于是 override 阶段的崩被当成工具/MCP 问题查了三轮。文案改为明确"agent 管线内部 bug，不一定是工具失败，去 `<hermix_home>/logs/` 看 panic/crash dump"。

### 20. 浏览器工具退役：查看器统一承载（2026-10-21）

内置无头浏览器线（`os_auto/browser`，8 个工具）从注册表下线，浏览器自动化由查看器的 MCP server 承载（`viewer__browser_*`，27 个 MCP 工具）。

动因有三：① 两边**同名不同参**——`browser_click(ref)` vs `viewer__browser_click(x,y)`，正是"参数猜测失败"这一惯性事故的温床；② 查看器是有头的、用户可见可登录可接管（超星场景的硬需求），内置是强制无头，能力是子集；③ 查看器的 `browser_open(headed)` 已同时覆盖无头（抓数据，默认 false）与有头（登录交互）两种模式，内置线不再有独立价值。

改动：`os_auto::register` 移除 `browser::register`（模块代码保留，viewer 的坑位文档引用它，回滚一行）；`essential.rs` floor 移除 `browser` 并把断言反转为"钉住缺席"；`plan_mode` 名单改为枚举查看器全部 17 个非只读动作（其只读动作在计划模式仍可用）；`directions.rs` 能力地图改为三层递进：`web_extract`（首选，不启浏览器）→ 查看器无头 → 查看器有头（登录/验证码）。工具总数 76 → 68。

同时记录用户的两个产品决定：**`write_file` 不加"默认拒绝覆盖"**——覆盖保护只对用户文档有意义，记录/记忆/markdown 类不需要；**网页数据获取可以无头**——查看器的 `headed=false` 就是这个路径，无需新开发。

### 21. 真实技能库基准 + approval 行豁免（2026-10-21）

**approval 行豁免落地**（Decision 20 之前的遗留）：增量 flush 的四个 SQL 点（锚点读、全量读、COUNT、尾部 DELETE）统一 `role NOT IN ('automation', 'approval')`。旧行为是 blanket DELETE 每轮销毁 approval 行而文档注释声称它存活——注释与实现不一致的问题至此修正。新增测试 `flush_preserves_approval_rows`。

**真实技能库基准**：`skill_routing_bench` 新增 `bench_against_real_library`（ignored，跑真实 `~/.hermix/skills`，36 个技能）。合成语料验证打分器性质，真实库验证阈值在真实分布上是否成立——两边的结论不一样：

- **中文 CJK 触发词质量极高**：「帮我写一份周报」→ document-authoring=1.00、「帮我把这个表格里的重复行删掉」→ spreadsheet-analysis=1.00，全是有用的正确激活（`cjk_bigram_hit` 的"tag 含完整 CJK 短语"路径）。
- **英文 vector 噪声带随库变宽**：合成 3 技能库的噪声上限 0.38，36 技能真实库上 "the printer is not responding" 冲到 **0.50**（frontend-architecture，纯噪声）——0.45 挡不住。`semantic_min_score` 0.45 → **0.52**。调整后中性查询 10 条里 3 条激活且全部人工判读为正确（另 2 条中文 + 1 条 PR 评审→code-review），0 条纯噪声。
- 已知边界：真实库上"仅靠 vector 的正样本"分数下限仍未观测到（没有出现过纯靠语义召回的真实需求）。若未来出现漏召回，降这个数——不动 CJK 那个。
- **库显著增长后必须重测**（`bench_against_real_library --ignored --nocapture`），噪声带是库规模的函数。

### 22. Harness²（google-research）能力差距分析与落地路径

Harness² 的核心：**模型权重固定，自动优化模型外围系统**（harness = 提示词/工具/记忆/技能/编排的总体）。两个递归：任务级（Propose 提出 harness 编辑 → Probe 同一任务跑 K 次对比行为差异 → Compose 组合精炼，**无需标准答案**）；Harnessing 级（跨任务持久层，保留被采纳的编辑与有证据的教训）。

对照：Hermix 已有全部 8 个可编辑组件的**静态形态**（directions→system prompt、approval/permission_guard→guardrails、memory/daily_memory、directions→tool guidance、delegate_task、skill 体系、execute_code、hermix-plugin）——**缺的不是组件，是优化循环**。现有 auto-skill 沉淀是"单次成功即留存"，没有对比，没有证据分级。

落地路径（按投入递增）：

1. **教训分级（小改，建议下一批）**：auto-skill 沉淀时区分两类——"顺利成功"（现骨架）与"错误后恢复成功"（新增：从 `tool_errors` 与 ErrorLoop 记录提取"哪里失败过、怎么绕过的"，写进正文新增的「踩过的坑」节）。错误-恢复对是最有价值的教训，而我们已经有全部原料。这正是 Propose-Probe-Compose 的单步退化版：失败轨迹 = 上一个 harness 的 Probe，成功轨迹 = 对比基准。
2. **跨会话教训注入（中）**：失败任务的轨迹已在 `experience_archive` 归档。把"上次同类任务在此处失败"接入 `memory_prefetch`，新会话开工前看到。避免同一个坑掉两次。
3. **完整 Harness² 循环（大，不建议现在做）**：需要任务级成功判定器（我们没有 judge）、K 次并行执行的预算控制、以及组合编辑的冲突消解。等前两步的教训数据积累出规模再说。

### 23. Harness² 八组件对齐（2026-10-21）

目标：不是"这些组件有没有"，而是"agent 能不能编辑它们"——Harness² 的全部主张就是优化器编辑模型外围系统。核查后 8 项里 **3 项不达标**，全部补齐：

**① Tool guidance 编译期写死 → 磁盘可覆盖**（最大的一个）。`directions.rs` 的 20 个 guidance 块原本只有 `const &str`，无任何覆盖路径，是"有内容但改不了"的组件。现加 `apply_guidance_overrides`：`$HERMIX_HOME/guidance/<name>.md` 存在则**整体替换**该块（不做合并——半编译半文件的文本无法推断模型实际看到什么），缺失回退编译期默认（新装行为零变化，删文件=完整回滚）。文件名取 tag 首段（`web_automation.always` → `web_automation.md`，路由后缀是注入条件不是身份）。空文件忽略并告警（防误 `touch` 抹掉行为指令）。每轮一次 `stat`/块，命中才读——加减缓存的 mtime 失效判定成本与 stat 相同，故不加。

**② Scripts 无持久库 → `$HERMIX_HOME/scripts/`**。`execute_code` 新增 `script` 参数：按文件名跑已保存的脚本，语言由扩展名推断（py→python / js→node / ps1→powershell / sh→bash），`language` 显式传时优先。`code`/`script` 二选一，都不传报错并点名两者。脚本名必须是**裸文件名**（拒绝 `/`、`\`、`..`、绝对路径）——扁平命名空间是刻意的，嵌套树需要额外浏览工具才能用，而 `list_files` 已经在。schema 的 `required` 置空（二选一无法用 JSON Schema 的 `oneOf` 表达而不成为全工具面唯一特例）。

**③ SOUL/personalities/agents/skills 等 harness 文件 → 可写 + 自动快照**。原先这些文件**可写但只有提示词软约束**（`config.yaml`/allowlist 是硬拒），既妨碍 Harness² 的编辑循环，又没有回滚。改为：允许写，但 write_file/patch/move_file 落盘前调用 `permission_guard::snapshot_harness_component`，旧内容存 `$HARNESS_HOME/harness-history/<rel>-<ms>.bak`，每文件保留 40 份。**为什么不是硬拒**：改写 SOUL 不授予新权限（不像 allowlist），只改行为，而行为变化下一轮就可见——缺的不是墙，是回退路径。快照失败只记日志、不阻断写入（用户的改动比我们的记录更重要）。

**④ 优化循环：错误-恢复对成为教训（judge 最小形态）**。auto-skill 沉淀门槛从"自然完成 **且** 全程无错"放宽为"自然完成"。理由：顺利跑通只证明"这样做可以"，失败后绕通才说明"哪条路不通"——后者信息量更大，旧门槛恰好把最有价值的会话整条丢弃。带错的 skill 多出两节：`## 证据`（调用数/失败数/是顺利跑通还是试错绕通）与 `## 踩过的坑`（前 8 条错误的轮次+工具+摘要）。这就是 Harness² judge 的最小形态：不判断"任务是否做对"（需 LLM 评测或人工反馈），只如实登记可核验事实，读者据此决定是否信任这份 skill。

对齐后的完整度：

| # | 组件 | 状态 |
|---|---|---|
| 1 | System prompt | ✅ `SOUL.md`/`personalities/` 可写（API + 工具），有快照 |
| 2 | Guardrails | ✅ `approval_allowlist`/`session_approvals`/`permission_profile` 用户可写、agent 硬拒（自我提权无回滚可能，必须硬拒）；`DENIED_PATTERNS` 保持编译期 |
| 3 | Memory | ✅ `MEMORY.md`/`USER.md`/`daily_memory.db`，有快照 |
| 4 | Tool guidance | ✅ **`guidance/*.md` 覆盖**（本轮新增） |
| 5 | Subagents | ✅ `agents/<name>.md`，有快照 |
| 6 | Skills | ✅ `skill_manage` 全功能，有快照 |
| 7 | Scripts | ✅ **`scripts/` 库**（本轮新增） |
| 8 | Plugins | ✅ custom-app 目录 + 原生 trait |
| — | 优化循环 | ✅ **错误-恢复教训 + 证据节**（本轮新增）；跨任务注入与 K 次 probe 仍未做 |

仍未做（Decision 22 已记）：跨会话教训注入（`experience_archive` → `memory_prefetch` 的桥）、完整 Propose-Probe-Compose（需任务级 judge 与预算控制）。

### 24. `tool_describe` 报“不存在”而烧轮次（2026-10-21 实测回归）

现象：模型连着四轮调 `tool_describe`，每轮被校验器判 `Tool 'tool_describe' does not exist`，整轮空转。

排查顺序与结论：

1. **不是被清理掉的**。`tool_search.rs::register` 无条件注册三个桥（同一循环、`override_existing=true`），`tools/mod.rs:218` 调用它；新增诊断 `dump_bridge_state` 实测 `tool_search` / `tool_describe` / `tool_call` 三个都 `present ... enabled=true` 且都在 `get_definitions()` 里。校验器用的又是裸 registry（`run_loop.rs:661` `self.tool_registry.get(name)`），所以 **registry 侧无缺失**。
2. **根因一：提示词宣传了本轮不存在的能力（本轮之前由本会话引入）**。`TOOL_DISCOVERY_GUIDANCE` 以 `DirRoute::Always` 无条件注入，但它描述的三步桥只在 `assemble_tool_schemas` 激活时（deferred ≥ `DEFERRED_THRESHOLD`=25）才进 `tools` 数组。桥没激活，guidance 仍教模型"先 tool_describe 再 tool_call"——模型照做，必然被拒。这是 Decision 15 里同一类错误的复发：**提示与可见性脱节**，只是方向相反（那次是能力地图点名不可见工具，这次是我新加的桥指引）。
3. **根因二：三个桥不是原子可见的**。它们不在 `acui_dispatch` 的 always_include 白名单里，只能靠 `KeywordRouter` 的关键词打分进 top-N（`max_tools` 默认 5）：查询里有 "search"/"call" 时 `tool_search`/`tool_call` 按名字命中，而 `tool_describe` 分数≈0 被截断——正是"两个在、一个缺"的形状。

修复两处：

- `directions.rs`：`TOOL_DISCOVERY_GUIDANCE` 改为**条件注入**，仅当 `tool_names` 含 `tool_describe`（链条中间步骤，它的缺席即链条断裂）时才 push。桥不可见时不再注入任何桥说明。
- `acui_dispatch.rs`：`tool_search` / `tool_describe` / `tool_call` 加入 always_include 白名单——桥是**一个流程**，留住两步丢掉第三步严格劣于全丢：模型看到流程被宣传却有一环不可达。

新增回归：`tool_discovery_guidance_only_appears_when_the_bridge_is_visible`（两个方向都钉）、`dump_bridge_state`（ignored 诊断，与 `dump_all_tool_names` 同款）。

### 25. 跨会话教训注入 + `harness_self_edit` 集成闭环（2026-10-21）

Decision 22 列的"仍未做的两件"之一。

**发现（比预期更严重）**：`harness_self_edit.rs` 的三段 `classify_tool_failures → generate_experience → persist_experience` 与总入口 `run_self_edit` 都在，但：

- `run_self_edit` 带着 `#[allow(dead_code)]`，注释写着 *"will be called from `run_loop.rs` once the harness self-edit integration is complete"*——**作者自己标注了没接完**；
- `classify_convergence` 同样标着 *"once the harness self-edit integration is complete"*；
- 读侧 `experience_archive::experience_prompt_for_tools` **零生产调用点**。

即：经验档案的写入端与读取端都写好了，中间没人调用。这与 `skill_auto_create`（Decision 18）、`permission_guard`（Decision 20 前）是同一个模式——**能力存在但不在路径上**。

**接线**：

- **写入**：`AgentLoop::maybe_record_experience`，在 `finalize_agent_result`（`run()`/`run_stream()` 的唯一收尾点）里 `drain_tool_errors_into_counter` 之后调用。现场构造 `FeedbackLog` / `SelfRuleStore`（两者的 `open_at` 都自建父目录），然后调 **`run_self_edit`**——不是手工调那三个 helper：手工版会漏掉 feedback 通道和规则提议，正是"半接线"的形态。
- **读取**：`run_loop::pre_turn` 中 `experience_prompt_for_tools(archive, 本轮工具名, 1200)`，作为 System 消息注入（与 recall / skills 块同一机制，压缩会自然折叠旧块）。
- 摘掉两处 `#[allow(dead_code)]`，注释改成准确描述"哪一半还缺"而不是"将来会接"。

**仍未接（明确缺口，非静默 no-op）**：`run_self_edit` 的 `convergence` 参数恒为 `None`——convergence 停止发生在**循环内部**，`finalize` 时拿不到 `ConvergenceReason`。后果：`ErrorLoop` 与 `ConvergenceStuck` 两类教训不会产生（`ToolBurst` 与 `FeedbackPattern` 正常）。补它需要把 convergence reason 从循环里带出来，属独立改动。

**测试**：`failure_counter_reaches_the_prompt_block_via_the_archive`（计数器 → 分类 → 落盘 → 读取 → 进提示词，整条闭环；并验证无关工具不会带出该教训）、`below_threshold_failures_are_not_archived`（阈值以下不写）。

### 26a. Harness² 任务级递归真正落地：Judge → Probe → Compose（2026-10-21）

Decision 26 的审查结论是"缺对比式精炼"。这一项把它补齐，新增 `harness_probe.rs`。

**三段在本项目的形态**（与 Harness² 的对应关系）：

| Harness² | 本项目 | 关键取舍 |
|---|---|---|
| Judge（基准打分） | 可核验信号合成：自然完成 / 末轮仍有错 / 用户重发 / 中断 / 循环收敛 | **不调 LLM**——多一次 round-trip 且不可复现 |
| Probe（同任务跑 K 次） | 同一[任务签名](归一化请求)的多次**真实**执行，跨会话累积到 `probes/<sig>.jsonl` | **不自动重放**：任务通常带副作用，自动跑 K 次会真的执行两次。用户的重试就是天然 probe |
| Compose（对比行为差异） | 同一签名 ≥2 条记录时，对比成功与失败：有效路径 + 失败的错 + **分歧点**（失败路径独有、成功路径没有的工具） | 不推断"为什么"失败（那容易编），只陈述可核验差异 |

**接线**：`AgentLoop::maybe_run_task_recursion`，在 `finalize_agent_result` 里先于 `maybe_write_auto_skill` 调用。分工是：**首次执行**（1 条记录）由 auto skill 给"步骤骨架"；**第二次同任务**由 Compose 覆盖成"有效路径 + 分歧点"的对比版（`skills/auto/probe-<sig>/SKILL.md`，每次覆盖自己，不产生新技能）。

**收敛教训就地记录**：`ConvergenceSignal::Converge` 分支（`run_loop/tool_turn.rs`）在 finalize 之前直接 `classify_convergence → persist_experience`。Finalize 的入参里没有 convergence reason，传它要改签名与两处调用点；"发生地记录"语义更准，也不会因为将来多一条停止路径而漏传。至此 `classify_convergence` 与 `run_self_edit` 都不再是 dead code。

**端到端暴露并修正的一个语义错误**（值得记）：`judge` 早先把 `repeated_request` 直接压成 `Failed`。这条规则在"重发后仍然失败"时对，在"**重发后成功**"时错——而后者恰恰是 Compose 最需要的那种执行（失败→成功的对比）。如果没有写那条两次真实 run 的端到端测试，Compose 会永远只看到"两次都失败"，输出负面教训，而且从单测里看不出来（单测只验"重发→Failed"这一半）。修正为：`repeated_request` **只作信号记录、不参与判决**；判决顺序是 末轮有错 → Failed，打断 → Unknown，未完成 → Failed，否则 → Succeeded。

**端到端证据**：`two_runs_of_the_same_task_compose_a_contrastive_skill`——第一次 run 永不收敛（`max_turns` 耗尽 → Failed），第二次正常成功；断言探针文件累积 2 行、`probe-<sig>` 技能落盘、正文含"有效的路径"且点名失败路径独有的 `list_files`。

### 26b. Harness² 能力集成审查（2026-10-21）

对 8 组件与两级递归逐项核对**代码证据**（不是文档说法）：

| 能力 | 状态 | 载体 / 证据 |
|---|---|---|
| 1 System prompt | ✅ 可编辑 | `$HERMIX_HOME/SOUL.md` + `personalities/*.md` + `PUT /api/persona/soul`；写入前快照 |
| 2 Guardrails | ✅ 用户可编 / agent 硬拒 | `approval_allowlist.json`、`session_approvals.json`、`permission_profile`；`permission_guard` 硬拒工具与 shell；`DENIED_PATTERNS` 编译期 |
| 3 Memory | ✅ | `memories/MEMORY.md` + `daily_memory.db` |
| 4 Tool guidance | ✅ | `guidance/<name>.md` 覆盖 `directions.rs` 的 20 个常量块 |
| 5 Subagents | ✅ | `agents/<name>.md`（frontmatter: role/tools/background）+ `delegate_task` 运行期覆盖 |
| 6 Skills | ✅ | `skills/<cat>/<name>/SKILL.md`，`skill_manage` 全动作 + 自动沉淀 |
| 7 Scripts | ✅ | `$HERMIX_HOME/scripts/` + `execute_code(script=)`，扩展名推断语言 |
| 8 Plugins | ✅ | custom-app 目录（manifest+HTML/JS）+ 原生 trait 插件 |
| 任务级 Propose | ✅ | 成功序列 → auto skill；失败-恢复 → 「踩过的坑」；收敛停止 → 经验条目 |
| 任务级 Probe | ✅ | `harness_probe::ProbeStore`：同签名多次真实执行，跨会话累积到 `probes/<sig>.jsonl`（Decision 26a） |
| 任务级 Compose | ✅ | `harness_probe::compose`：成功 vs 失败对比出「有效路径 + 分歧点」，覆盖写 `skills/auto/probe-<sig>`（Decision 26a） |
| Harnessing 级持久层 | ✅ | `skills/` + `experience/` + `self_rules/` + `probes/` + `harness-history/` |
| Harnessing 级跨任务注入 | ✅ | `pre_turn` 注入经验块（Decision 25）；skill 按查询激活 |
| **Judge（任务级成败）** | ✅ 可核验信号 | `harness_probe::judge`：自然完成 / 末轮有错 / 用户重发 / 中断 / 收敛，合成 Succeeded/Failed/Unknown（Decision 26a） |
| K 次**并行**预算 | ❌ 刻意不做 | 自动重放同一任务会**真的产生第二次副作用**（重复写文件/提交表单）。Probe 改为累积真实执行；需要并行沙箱试错是另一个功能，须先有隔离环境 |

**结论**：组件层 8/8 可编辑、两级递归的写入与读取都闭环、Judge/Probe/Compose 三段落地，**Harness² 的主张在本项目里已经走通**——只是 Probe 采用"真实重试累积"而非"主动 K 次并行"。

**唯一的刻意偏离**：不做自动并行 probe。这不是没做完，是**不能这么做**：Harness² 在基准沙箱里跑 K 次，我们在用户的真实机器上跑，重放的副作用是真的。要支持它必须先有可丢弃的沙箱环境——那是另一个前置工程，不是本模块能顺手加的开关。

### 27. 自动沉淀技能的路由治理（2026-10-21）

**问题（我自己引入的）**：Decision 18/26a 把自动沉淀接进 finalize 后，`skills/auto/` 会随使用线性增长。它和手写技能**同构**——都是 `<skills>/<category>/<name>/SKILL.md`，都被 `scan()` 收进匹配池。跑几十个任务就多几十个候选，而 Decision 16 刚把误激活从 3/10 修到 0/10，这个改动会把它推回去。

**两层治理**（缺一不可，第二层是实测逼出来的）：

1. **降权**：`AUTO_SKILL_SCORE_FACTOR = 0.75`。同分时手写技能胜出；沉淀 0.8（→0.6）仍能独立胜出，不把"下次复用"打死。降权在阈值判断**之前**应用——否则它仍会占 top-N 名额。
2. **门槛下限不软化**：`AUTO_SKILL_MIN_SCORE = 0.3`，取 `effective_min.max(0.3)`。`effective_min` 对多词查询会软化到 **0.15**（为保手写技能的释义召回），那个软化对"人挑的几十个"合理，对"质量参差的沉淀产物"是灾难。

**为什么必须两层**：只做降权时，测试实测 `raw 0.25 × 0.75 = 0.1875` 仍然 > 0.15 而入池——降权只改排序、不改门槛，名义上治理了、实际没堵住。这条只有写测试才会发现（先按直觉只做降权，测试红在"应该落选却入池"）。

向量层同样处理：中文向量阈值 0.18 低于 0.3，也要抬到 0.3，否则中文查询下沉淀技能仍能从最松的门进来。

**数量治理**：软上限 80，超过时 `warn` 一次（进程内 `OnceLock` 防刷屏），**不自动删除**——删掉的可能正是下次要复用的那条，而磁盘代价每个约 2KB。降权已解决竞争，数量只影响扫描成本与目录可读性。

**判定**：`category == "auto"`（frontmatter 派生）**或** 路径含 `auto` 段（`AUTO_SKILL_DIR` 的硬约定）。任一命中即算——宁可多降权一个手写技能，也不要漏掉一个沉淀产物。

**验证**：`hand_written_skill_outranks_an_equally_matching_auto_skill`（同分时手写第一）、`an_auto_skill_alone_still_activates`（降权不是封杀）、`a_weak_auto_match_is_dropped_by_the_discount`（0.1875 必须落选）。真实库（44 个技能）中性查询仍 3/10 且全部为正确激活。

## Alternatives considered

- **动态 `System` 块改成 `User` 角色塞进 messages**（审计报告最初建议）。否决：会触发 `messages_for_api_call` 的连续 user 合并，把注入块粘到用户真实输入后面；插在 assistant `tool_use` 与 `tool_result` 之间还会破坏配对。分块方案改在序列化层，消息历史与语义零变化。
- **只给 system 打点、不拆块**。否决：拼接方式下 `system` 每轮都变，打点无效；必须让稳定段成为一个独立且不变的块。
- **让 per-call 超时真正取消同步 handler**（如 `spawn_blocking` + 进程级 kill）。本轮否决：handler 是同步 `Fn`，内部经 `block_in_place` 桥接，搬到 blocking 线程会 panic，改造面覆盖全部 30+ 工具。当前先解决「不冻结整轮」；「超时后仍可能在跑」写进文案，由模型自查状态。
- **给 JSONL 镜像加 `version` 列 / 用 `session_seq` 记账**。否决：需要 schema 迁移与迁移期双写，而镜像本身是非权威导出；指纹锚点用现有信息即可自愈。
- **权限配置改成「需要审批」而非拒绝**。否决：理由同上，审批卡挡不住自我提权。
- **对 shell 命令做路径规范化后再比对**（模拟 `$HOME` 展开、`~`、变量）。否决：无法穷举；`command_touches_permission_config` 明示为「挡明显形态」，真正的边界仍是 ACL 沙箱。
- **超时改为 `spawn_blocking` + 强制取消 handler**（P1-3 的彻底版）。否决：handler 是同步 `Fn` 且内部经 `block_in_place` 桥接，搬到 blocking 线程会 panic；要真取消得把 30+ 工具全部异步化。
- **网关安全网整段删掉，交给 agent loop 自己的压缩器**。否决：压缩在 `tool_turn` 里跑，即首轮 LLM 调用之后；一个已经超窗的会话在压缩有机会运行之前就会被 provider 拒绝。
- **PTY 用 Job Object 而不是 `taskkill /T`**。否决（本轮）：Job 需要进程句柄，`portable-pty` 只暴露 pid；为一个超时清理路径去改 PTY 抽象不划算。`local.rs` 非 PTY 路径本来就跑 `taskkill /T /F`，这里只是补齐同款。
- **Vault/automation 各自维护 pragma**。否决：三份 pragma 就是三种 journal_mode 漂移源，而 `hermix-store` 的存在理由正是消除这种漂移。
- **给 `messages` 加稳定 `message_uid` 列，把 wire id 换成把 uid**。否决（本轮）：那会同时改 schema、`routes.rs` 的三处 id 构造、以及前端所有 key 用法——一次性大改，收益只是让 id 不再依赖行序。先让行序真的稳定（本项），uid 可以日后单独做。
- **把 `web_search` / `browser_*` 提升为 resident（进 `DEFAULT_RESIDENT_TOOLS`）**。否决（本轮）：那能治本，但 12 个 schema 常驻每轮约 3–5K token，而渐进式披露的存在理由就是省这个。先修反馈闭环；若实测仍频繁踩桥，再按"哪些工具真正高频"做数据驱动的 resident 名单扩充。
- **给浏览器做跨会话连接池 / 常驻 Edge**。否决（2026-10-21 实测后）：`crates/hermix-tools/tests/browser_timing.rs` 测得冷启动+首导航 **1.61s**、snapshot **15.4ms**、同会话第二次导航 **151ms**（复用收益 **10.7×**）。会话内复用已经生效，池化只能省掉每个会话一次性的 1.6s，而代价是常驻一个 Edge 进程的内存与"用户关了又自动起来"的惊扰。数据不支持这笔投入。该测试针对的内置线已退役，测试保留（browser.rs 模块代码仍在，且时序结论对 viewer 侧同样成立）。
- **`write_file` 加"默认拒绝覆盖"**（借查看器 `viewer_write_text` 的 overwrite 语义）。否决（用户决定）：覆盖保护只对用户文档有意义；记录、记忆、markdown 类内容本就允许重写，默认拒绝会把每次正常写入都变成一次确认往返。
- **浏览器退役后同时删除 `browser.rs` 模块代码**。否决：查看器的 browser 模块注释明确引用了这里的三个坑位实测（F1 端口 / F2 用户 profile / F3 视口），代码是文档的一部分；且注册下线可一行回滚，删码则不可逆。
- **内部工具冗余合并（edit_file→patch、list_files→search_files、image_generate→gpt_image_generate、disk_cleanup、kanban、文档线 6→3）逐项核查引用面后全部否决**。这是我上一轮分析的一个错误修正：名单是按工具描述的表面相似度列的，逐项查引用后每一项都是有意的設計——`edit_file`/`list_files` 是有意的兼容别名（模型先验 + `acui_dispatch` always-include、子代理名单、审批 diff、WRITE_TOOLS 六处引用）；`image_generate`/`gpt_image_generate` 是双供应商适配器（FAL_KEY vs OPENAI_API_KEY，能力差异：后者有 `input_image` 编辑）；`disk_cleanup` 有 dashboard UI（`stubs.rs`）和权限画像引用（`permission_profile.rs`）；`kanban` 有看板 UI；`security_guidance` 是运行时守卫的策略读写接口而非静态文本；`read_pdf`（纯 Rust 分页、零依赖）与 `markitdown`（15 种格式）可靠性特征互补。**真正的重复只有浏览器线（已退役），"工具数少"本身不是目标**——重复的实际伤害是同名不同参导致的参数混淆，这一点已经消除。
- **在系统提示里动态列出 deferred 工具名清单**。否决：清单本身长（60+ 个名字）且仍不含 schema，模型照样要 describe；不如直接教它 describe。
- **把错误文案改成中文**。否决：问题在信息量不在语言；且错误串被多处测试按子串断言，改语言会连带破坏一批无关断言。
- **把 `.md` 会话日志改成"内容未变则跳过写"**。否决：判定"未变"要把文件读回来（等量 IO），净收益接近零；而预保存的那份必然与运行后不同，照样两次都写。真正省掉重复写入的办法是预保存干脆不写，而不是写前比较。
- **把 `.md` / trajectory 改成增量 append 或轮末后台 flush**。否决（本轮）：`.md` 是渲染后的完整文档（带头部、日期、分隔线），append 会破坏格式；后台 flush 需要调度器，而这两份都是"人读导出"，不是权威数据。省下的是 IO，不是正确性。
- **二进制判定用"非 ASCII 比例"或扩展名名单**。否决：非 ASCII 比例会把 CJK 文本误杀，扩展名名单管不住 `.jpgg` 这类写错的。NUL 字节是唯一可靠的单一信号（真实文件路径的 `read_to_string` 失败检测与 `local.rs` 的 image magic 检测都建立在同一前提上）。
- **改用 `INSERT OR REPLACE` 实现 upsert**。否决：它同时触发 `_ad` + `_ai` 两个触发器（等于删除再插入，rowid 会变），而 `_au` 才是正确的更新路径；且 REPLACE 会丢掉客户端写入的 `unified_steps` 与 `extra`。

## Consequences

- Anthropic 缓存自 system 断点起恢复命中，但**动态块仍在增长**：system 参数随会话变长，缓存块之后的内容每轮都要重新计费。后续可选「动态块滚动替换末条」而非一直追加。
- token 计数变准 = 压缩触发更早。大工具参数的会话会看到更多压缩事件（这是正确行为，但用户观感变化需要前端文案配合）。
- 串行段的超时是「不再等待」而非「已取消」：一个挂死的写工具仍占着一个 worker 线程，直到它自己返回。真正的取消需要 handler 异步化，属于 P1。
- 镜像每次追加多一次 `sync_all`；压缩或 `/undo` 后触发一次整文件重写（O(n) 写，但只在该场景发生）。
- `config.yaml` 的 home 归属由 `hermix_config::paths::hermix_home()` 决定；portable 模式下 `HERMIX_PORTABLE_DATA_DIR` 改变 home，保护范围随之移动（符合预期）。
- shell 侧是子串筛查，编码/变量拼接可绕过。这一点在模块文档里写明，避免下一个人误以为它是沙箱。
- 工具结果预算变小 = 模型看到的大文件更少。`read_file` 靠分页补回来（多一次调用、多一次往返），这是有意的交换：窗口是稀缺资源，往返不是。
- 中段截断的标记比原来的头截断多约 20 字符，`budget` 的两个测试断言因此放宽到「cap + 标记长度」。
- `run_blocking` 只在多线程运行时生效；`#[tokio::test]`（current-thread）走直调分支，因此单测覆盖不到真实阻塞行为。
- 会话持久化的稳态成本从「读 n + 写 n + 两套 FTS 索引 n」降到「1 次 count + 2 行读 + 写 k 行」。**剩余成本是 `sessions.db` 的写事务本身**（`BEGIN IMMEDIATE` + WAL 提交），与消息条数无关；`message_count`/watermark 一旦漂移，代价是那一轮退化为全量扫描（正确，只是慢），不会损坏数据。
- `.md` 会话日志与 trajectory JSON 仍是每轮全量重写。它们没纳入本项：前者是人读导出、后者是训练格式，都属于"非权威镜像"，重写只是浪费 IO，不涉及一致性。要省这块应改成轮末或后台 flush。
- `message_count` 现在承担两个语义（UI 显示条数 + 增量水位）。任何把它改成"本次插入数"的改动都会静默让快路径失效（不会报错，只是慢回 O(n)）——`flush_keeps_message_count_equal_to_the_timeline` 就是为此存在的。
- 日志层仍是每条事件 `write` + `flush`。未改动是因为 crash.log 必须同步落盘（异步队列会丢掉崩溃前最后几行，而那正是最需要的一行）；要做就得按 category 分流，属独立决策。
- 增量 flush 的 fast path 依赖一条不变量：**两次 flush 之间时间线只会被截断或从前缀处重写**（压缩折叠、nudge 剥离、相邻 assistant 合并、`/undo` 都会改变 tracked 条数）。若将来出现"原地改写中间某条且条数不变"的路径，条数与首中尾三锚点都可能通过，那一行会漏更新。加到循环里前请先看 `flush_messages` 的注释。
- 预保存不再写 `.md`：如果 `run()` 中途失败，磁盘上的 markdown 会比 DB 少最后一轮用户消息（DB 有）。这是有意的取舍——DB 是权威源，`.md` 是导出。
- `read_file` 接缝路径现在与真实文件路径一致地拒绝二进制。副作用：一个"其实是 UTF-16 的文本文件"在两条路径上都会失败（`read_to_string` 本来也失败），没有变差，但也没有更好；要支持 UTF-16 需要额外决定解码，未做。
- `save_session_atomic` 增加了一个参数（`write_session_log`）。它是 `pub`，但本工作区内只有本文件内的三个包装器调用它——若将来有外部调用者，那是编译错误，不会静默改变行为。
- 参数错误现在更长（契约 + 示例 + 桥指引，约 200–400 字符）。它只在**已经失败**的调用上出现，所以不影响成功路径的 token；但如果某个工具被模型反复错调，上下文中会累积多条这类错误——正是它要消灭的场景，累积几条换来纠正是值得的。
- 渐进式披露的门槛（25）与 resident 名单都没动。这意味着"猜参数"仍可能发生在**没读错误**就重试的模型上；`TOOL_DISCOVERY_GUIDANCE` 是预防，`enrich_param_error` 是补救，两者都依赖模型真的读反馈。
- `automation` 行仍是"只追加、不删除"，但它们**不会**重复累积：`load_session` 过滤 automation，agent loop 的内存时间线里没有历史 automation 行，所以每轮只有**当轮新投递**的那一条被插入。这一点我最初的判断是错的，`keep-sess` / `auto-sess` 两个既有测试证伪了它（`persist_session` 的测试表明 flush 之后 automation 行仍存在且只有一份）。
- `approval` 行位于比对集内但内存里没有对应消息，遇到时会拉低 prefix、退化为尾部重写（删除效果与旧行为等价）。它们本该像 automation 一样被豁免——同样是既有偏差，未在本轮改动。已有测试（`s4` 的 "Only 3 entries total"）表明当前行为是可接受的。
