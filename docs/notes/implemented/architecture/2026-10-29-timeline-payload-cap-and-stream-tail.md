# Decision Note: 单回合过程数据封顶 + 流式长文只解析已闭合前缀

Status: implemented — 2026-10-29 交付

## Decision

### 问题（实测，2026-10-29 本机 sessions.db）

| 指标 | 实测 |
|---|---|
| 最大会话 | 390 条消息 / 正文 17.5 万字符 |
| 该会话 `unified_steps` | **2.77 MB** |
| 该会话 `execution_timeline` | **2.90 MB** |
| 单条消息最大 | steps 1.10MB + timeline 1.14MB = **2.2MB** |
| 单条 `tool_stdout` 事件 | 平均 **50KB**（16KB 分片逐个成事件） |

不是消息条数多（最多 390 条、窗口 60 条），而是**一带工具输出的回合无限膨胀**，
且同一份输出存了两遍（`tool_stdout` 累积 + `tool_complete` 预览）。后果：切会话
要 `JSON.parse` 5.7MB（parsed 对象 3–5 倍膨胀 → 20–40MB 常驻），之后每次渲染
碰到这条消息都要走一遍。

### 修法

**1）写侧封顶（两端一致：每次调用只留一个 stdout 事件，超限留头尾）**

- 后端 `crates/hermix-agent/src/local_agent_service/timeline_limits.rs`：
  `push_stdout_capped` + `STDOUT_CAP_PER_CALL = 24K 字符`。同 `tool_call_id` 的
  分片合并进**一个**事件；超出上限保留头尾（各 12K）并插一行
  `…（中间已省略 N 字符，保留头尾）`。接入 `code_execution` 逐行回调与
  常规工具的 16KB 分片循环。
- 前端 `apps/desktop-tauri/src/utils/timelineLimits.ts`：`capStdoutText`（实时
  累积也封顶，见 `app/streamEvents.updateToolStepStdout`）+
  `compactStepsForPersist`（落库前幂等压缩，接在 `api.saveUnifiedSteps` 的
  唯一出口上）。

选**字符**不选字节：24K 字节只够 8K 汉字，会把中文输出过度截断。

**2）流式长文只解析已闭合前缀**

`MarkdownBody` 新增 `streaming` prop：正文超过 6000 字符时按 `splitStreamTail`
切成「最后一个空行前的稳定前缀（正常解析）+ 正在长的最后一块（纯文本，
`.md-stream-tail`）」。原来每 120ms 对整段正文跑一遍
「归一 → 补围栏 → 软换行 → satteri 解析 → hast→JSX」，成本随正文线性增长；
切成前缀后完整解析的次数从"每个节流窗口一次"降到"每出现一个新块一次"。
定型（`streaming=false`）后恢复整段高质量渲染。

顺带：`StreamingBubble` 的 `parseContent(deferred)` 改为**只在没有
`unified_steps` 时才跑**（正常路径每帧白跑一遍全套正则 strip）；占位条
`ToolRunNote` 的判定改用 `hasBodyText`（看 steps 里的 text step），避免
"走了 steps 路径就误判成没有正文"。

**3）常驻定时器补可见性守卫**（4 处，窗口在后台时不再发 IPC）

`App.tsx` 手机活动 15s、`TokenUsagePopover` 3s、`settings/ModelListTab` 3s
离线自愈、`settings/ImChannelsCard` 2s 配对轮询。

## Alternatives considered

- **只做后端封顶** —— `unified_steps` 是**前端**写的
  （`POST /v1/sessions/:id/messages/unified_steps`），只封后端等于漏掉 2.77MB
  的那一半。必须两端同一套规则。
- **只做前端** —— `execution_timeline` 由后端积累，长回合在落库前就已经是几 MB。
- **只留头部（最省事）** —— 错误信息通常在输出尾部（stderr 最后几行），
  头尾都留才不至于把用户最想看的失败原因丢掉。
- **在读取侧裁（每次加载时压缩）** —— 省的是前端解析，服务端每次仍要
  parse+re-serialize 5.7MB，且老数据会反复被裁；写侧一次到位更干净。
- **流式期间整段降级为纯文本** —— 观感断崖（流式全程没有格式）。只降最后一块、
  且只在 >6000 字符时才启用，短回复行为完全不变。

## Consequences

- 新回合的 steps/timeline 体积与工具输出长度解耦：单次调用最多约 24K 字符 × 2
  （stdout + complete 预览 2K）。
- **老数据不变**：已落库的 2.2MB 消息不会自动变小；它们在新回合覆盖前仍是旧体积。
  需要的话可加一次性压缩脚本（按同样的幂等规则重写 `unified_steps`），本轮未做。
- 展开工具输出时，超过 24K 的部分看不到中段（有省略说明）；完整输出仍可重新执行
  命令获得。
- 流式长文（>6000 字）的最后一块以纯文本呈现，定型后变回 Markdown —— 会有一次
  视觉切换，短回复无感知。
- 验证：`cargo test -p hermix-agent --lib timeline_limits` 4 项通过；
  `timelineLimits.test.ts` 8 项通过；`tsc --noEmit` 通过；DesignGuard `clean`、
  theme-tokens 全项 ✓。
