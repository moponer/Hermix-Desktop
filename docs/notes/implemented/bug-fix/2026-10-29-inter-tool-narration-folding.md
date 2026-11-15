# Decision Note: 工具之间的"我接下来去查…"不该进正文区

Status: implemented — 2026-10-29 交付

## Decision

用户看着一轮长任务说「思考跑到正文了，没有折叠」。查库（`sessions.db` 的
`unified_steps`）后确认：思考本身是**正确地**折叠着的（`reasoning` step，
英文/中文独白，1500–17000 字符），跑出来的是另一类东西 ——

模型在两次工具调用之间写进 `content` 的短旁白：

```
REAS(1542) → TEXT(30)  "这个错误值得查一下，不是随机的。先看那个沙箱目录和它的来源。"
TOOLS(6)
REAS(3921) → TEXT(77)  "沙箱目录在 <TMPDIR>\hermix-sbx-<hash>——这个报错来自 hermix 自己的沙箱实现…"
TOOLS(6)
```

这类句子落库是 `text`（不是 `reasoning`），而渲染层此前把**所有** text step 都
按正文待遇（13px 正文字号 + 正文色）。于是一场 30 次工具调用的任务下来，正文区
就是一串 18–125 字的旁白，读起来正是一段没折叠的思考。更糟的是它会被工具调用
从中间截断：`TEXT(3) "查"` → TOOLS → `TEXT(50) " TEMP 指向 F 盘是全局环境变量还是…"`
—— 单独一段"查"出现在正文里没有任何意义。

改：

1. **正文 = 收尾那一段 text。** `messageParser.lastTextStepIndex(steps)` 返回最后
   一段 `text` 的下标，其余 text step 判定为过程旁白。判据用**位置**而不是长度：
   旁白的定义是"说完还要继续干活"，也就是后面还跟着步骤。
2. **旁白随过程折叠。** 轮次结束且总折叠头收起（默认态）时旁白不渲染 —— 它和
   `reasoning` / `tools` 一样属于过程区；用户展开折叠头时按原位置显示。
3. **旁白退到次级色**（`.tl-text-step--narration .tl-text-inner` → `--color-text-secondary`）：
   运行中它仍然实时可见（用户要看 agent 在干什么），但不再和答复抢同一档视觉权重。

顺带：`toolVerb("tool_call")` 不再原样吐出英文工具名。`tool_call` 是运行时的
**桥接工具**（模型要调的工具不在 tools 数组里时经它转发，见 `directions.rs`），
动词退回「调用工具」，具体调的是谁由 `toolPrimaryArg` 从 `args.name` 取，行读作
「调用工具 execute_code」。

## Alternatives considered

- **改 prompt，禁止在工具之间写旁白** —— 能从源头减少，但模型换一个还是会写；
  而且旁白本身是有用的实时进度，删掉会让长任务看起来像卡住。渲染层兜底更稳。
- **按长度折叠（旁白 ≤ 160 字才折叠）** —— 阈值要么误折叠一段真正的中间答复，
  要么漏掉一条长的旁白；而"后面还有没有步骤"是能直接从数据结构读出来的事实，
  不需要猜。
- **运行中也不渲染旁白** —— 运行中的实时反馈就只剩折叠头的计数，用户此前反馈过
  「像卡住」（`ToolRunNote` 就是为这个加的）。保留但降权。

## Consequences

- 结束的一轮，正文区只剩结论；过程（思考 + 旁白 + 工具）收在顶部一行折叠头里。
- 展开折叠头时旁白仍在时间位置上（次级色），不丢信息。
- 一轮只有旁白、没有收尾正文时，正文区为空、只剩折叠头 —— 与"模型没写结论"的
  既成事实一致（`ToolRunNote` 的空白占位仍只覆盖"完全没有 text step"那种情况）。
- 旁白仍然是 `content` 的一部分：复制、搜索、下一次上下文都照旧包含它，只是不
  在正文区显示。
- 验证：`tsc --noEmit` 通过；`messageParser.test.ts`（33）+ `toolOutcome.test.ts`
  （18）全绿（新增 3 条 `lastTextStepIndex` 用例）；DesignGuard `clean`、
  theme-tokens 全项 ✓、`impeccable detect` 两个文件 exit 0。
