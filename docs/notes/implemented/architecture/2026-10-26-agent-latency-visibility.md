# Decision Note: 把「思考很久没有正文」的四条成因各自收口

Status: implemented — 提示词、日志格式、技能扫描、轮次收尾、阶段契约五处已改

## Decision

起因是一次完整请求的实测（2026-10-26 16:47，`agent.log` + `server.log` 交叉对时）：
310 秒里 **30 次 LLM 往返占 274 秒（88%）**，工具执行与轮间准备合计 11.3 秒（3.6%），
30 轮里 23 轮有工具调用且**正文为空**，最后撞上 `max_turns=30`、总结调用 0.24 秒失败、
没有任何收尾。据此改了五处，每处都不引入新机制。

### 1. 结构化字段不再被日志格式丢弃

`finish_plain` 只打 `ts / level / target / msg`，于是 `TurnReport` 的
`api_ms` / `tool_ms` / `has_visible_text`、turn watchdog 的 `elapsed_secs`、
`handle_max_iterations` 的 `error=%e` **全部不可见** —— "慢但测不出慢在哪"的根因。

现在 plain 行是 `ts LEVEL [target] msg k=v k=v`（值内换行折叠成空格，保持一行一事件）。
JSON 模式不变。

### 2. 工具轮要求一句面向用户的正文

原提示词三处把叙述推向 reasoning：`plan.full/short` 写死了 "in the reasoning flow"，
`TOOL_USE_ENFORCEMENT_GUIDANCE` 说 "Every response should either (a) contain tool calls
… or (b) deliver a final result"，`AUTONOMOUS_EXECUTION_GUIDANCE` 说 "Non-final turns:
advance with tool calls, not promises"。模型照做——工具轮 assistant 的 `content` 就是空的，
而前端把 reasoning 归入 `ProcessTimeline` 折叠区，正文区于是全程空白。

改成：工具调用前**一句**可见正文说明在做什么，分析仍留在 reasoning；"说了就要真做"的
约束保留（那句正文不替代调用）。`plan.short`（每个后续轮都注入的那条）同步改。

### 3. 技能扫描出热路径

`prompt_assembly::skills_system_prompt` 每轮新建 orchestrator 全盘扫描：递归
`read_dir` + 读 69 个 `SKILL.md` + 逐个健康打分，低于 0.5 的全部 WARN。实测
1124 行 WARN / 5 分钟 = `agent.log` 的 83%。

两层修法：

- **扫描结果按 `(path, mtime, len)` 指纹 memo**（进程级）。指纹只要 stat，不读文件；
  安装 / 删除 / 编辑改变指纹即重建。遍历抽成 `collect_skill_files`，扫描与指纹共用。
- **WARN 每个 `(path, score)` 每进程一次**。分数线仍照打，只是不重复刷。

### 4. 轮次预算耗尽时一定有收尾

`handle_max_iterations` 两个失败分支原先都 `Ok(None)`：什么都没注入，计数器照样清零，
于是同一个工具循环再来 30 轮，用户看到的是"五分钟白等 + 突然结束"。

现在失败时注入 `deterministic_progress_summary` —— 由已有上下文拼出机械记录
（工具轮数 / 调用总数 / 工具使用分布 / 最近五步）+ 一句"停止探索，现在面向用户汇报"。
用户可见的措辞仍由模型写，不由代码伪造。

### 5. 阶段机只为真正的跳跃告警

`illegal loop phase transition` 实测每轮一次（36 次 / 30 轮），而三条被判非法的边都是
真实分支，不是跳阶段：

- `PreTurnSetup → LlmCall`：`run_stream` 没有 `Prefetch` 阶段（`do_prefetch` 只接在 `run()`）。
- `PostLlmCall → PreTurnSetup`：空回复兜底重试。
- `ToolValidation → PreTurnSetup`：`ToolPrep::SkipTurn`（全部去重/被拒，无工具可执行）。

三条写进 `can_transition_to` 并附注释；真跳跃（`PostLlmCall → ToolExecution`、
`Init → LlmCall`）仍然告警。

### 6. 零新信息 → 停用工具（唯一能真正减少往返的硬约束）

同一份工具结果记录显示，那 30 轮里至少五批结果是**逐字重复**的（1838 字符 ×3、
23021 字符 ×2）——这些往返没有带回任何新信息。`LoopState` 因此多了一个
`RepeatGuard`：按「工具名 + 是否失败 + 结果正文」算批次指纹（**不含**
`tool_call_id`，每轮新生成），连续 2 批与历史某批完全相同就置位，之后
`run()` / `run_stream()` 发给模型的 `tool_schemas` 变成空切片 —— 模型物理上
没法再调工具，只能给结论。

选指纹而不选「工具名 + 参数白名单」的原因：写完文件重读同一路径会得到**不同**
内容，天然放行；而"把同一批只读调用再发一遍"逐字相同，正是要打断的空转。
选「禁用 schema」而不选「提醒模型别重复」的原因：提醒依赖模型服从度，而这是
每轮 9 秒的账单。上限仍然保留（`max_turns`、ErrorLoop），这只提前结束**空转**。

### 7. 前端：工具轮的正文占位

`UnifiedTimelineRenderer` 里原来只有一个**流式期**的「等待回复」点阵；消息一旦
settle，纯工具轮就是整段空白正文。新增 `ToolRunNote`（`ToolCallBubble.tsx`，
`StreamingBubble` 也引同一份）：正文区写一行「本轮调用了 N 个工具：read_file、grep」，
结束态再补一句「无文字结论」。它读的是 `execution_timeline` 已记录的事实，**不进**
`content`、不进历史、不影响复制。

## Alternatives considered

- **把 reasoning 提到正文区渲染**：等于改产品语义（过程即正文），且 reasoning 会泄漏
  内部推断。放弃——问题在提示词没让模型说人话，不在渲染。
- **给 `run_stream` 补 `Prefetch` 阶段**（对齐 `run()`）：会改变流式路径行为并增加每轮
  延迟，与"减少往返"的目标相反。只把契约写成现实。
- **技能扫描加 TTL 缓存**：TTL 短（<10s）在 9 秒一回合下等于没缓存，TTL 长又让新装的技能
  迟迟不生效。指纹方案没有这个取舍。
- **工具轮由代码合成一句正文**（如"正在调用 read_file…"）：往对话里塞模型没说过的话，
  污染历史与缓存前缀。放弃。→ 改为在正文区渲染**结构化事实**（占位行，非模型文本）。
- **降低 `max_turns`**：只是把 4.5 分钟变 2 分钟，症状不变；真正要砍的是空转轮次。
- **按只读工具白名单拦重复调用**：要维护一份跟工具注册表同步的名单（本仓库有 74 个
  内置工具），漏一个就误判；结果指纹不需要任何分类。
- **只发提醒、不禁用工具**：提醒依赖模型服从度，而每次不服从就是 9 秒；禁用 schema 是
  唯一由代码保证的收敛。

## Consequences

- 现在可以从 `agent.log` 直接读出每轮 `api_ms` / `tool_ms` / `has_visible_text`，
  后续优化不必靠时间戳反推。
- 技能库未变时每轮只 stat 一遍文件名与大小，`agent.log` 里技能 WARN 退化为每进程一次。
- 空转轮次从"跑到 `max_turns` 才停"变成"第 2 批重复结果就停"：那次的 30 轮里，
  至少 5 批是重复的，后续轮次不再付费。
- 提示词改动影响的是模型行为，不是确定性修复：工具轮能否稳定吐一句正文取决于模型服从度。
  这正是第 7 条占位存在的理由——**文案可能不稳，事实（调了什么）是确定的**。
- 停用工具后模型只剩一条路可走：给结论。若它仍在输出里假装要调工具（schema 里没有），
  那一轮就是纯文本，run 正常结束。
