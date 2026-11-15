# Decision Note: 进度早就翻页了，上面还挂着转圈的「调用工具」

Status: implemented — 2026-10-29 交付

## Decision

用户截图里有两行**没有工具名、没有入参**的「调用工具」一直在转圈，而它下面的
思考、正文、后续工具早已推进了很远。根因不是后端丢了完成事件（`sessions.db`
里 731 次调用全部 start/complete 配对），而是**前端的应用顺序**：

1. `onToolChunk`（stdout 分片）攒进闭包 Map，等 `requestAnimationFrame` 合并
   后一次 `setSessions`（这是早前为修「大输出打满主线程」特意加的批处理）；
2. `onToolComplete` 直接 `setSessions`，**当帧即生效**。

于是只要是「分片和完成事件落在同一个动画帧」的快速工具（搜索、目录、短命令），
完成事件必然先于它自己的 stdout 落进 `unified_steps`。`groupTimeline` 对这类
事件的判定是「配对键找不到 → 新开一行」，而 `tool_stdout` 事件**不带工具名也不
带入参** → 新长出来的行就是「调用工具 + 转圈」，且再没有任何事件会闭合它
（回合结束时被 stranded pass 判成「执行已中断」，仍然是一行幽灵）。

修三处，缺一不可：

- **`App.tsx::onToolComplete` 先定向 flush 这个 id 的待合并分片**，再追加
  `tool_complete`（两次 `setSessions` 由 React 按调用顺序应用，顺序即保证）。
  `flushPendingToolChunks(onlyToolCallId)` 的定向模式不动
  `toolChunkFlushScheduled`，否则同帧剩余分片会失去下一帧的 flush 机会。
- **`messageParser` 的聚合器不再给孤立的 `tool_stdout` 开新行**：补进同一次调用
  的组（`last` 表），连组都找不到就丢弃。ACUI 的 `groupEvents` 同款守卫。
- **聚合改成跨 step**：`groupTimelineSteps` 按事件真实顺序聚合整轮，再把每个组
  放回它 `tool_start` 所在的 step。此前是逐 step 聚合，同一批调用被思考/正文
  隔开时，开始那半永远等不到完成，完成那半新开空壳 —— 同一枚 bug 的另一种长相。
  `activitySummary` 改吃同一份结果，避免「摘要说 N 个、行只画 N-1 个」。

另外把回合结束时补的合成 `tool_complete` 一并写进 `unified_steps`：此前只补
`execution_timeline`，效果是「刷新后正确、当场仍转圈」。

## Alternatives considered

- **只修 `groupTimeline`（丢弃孤立 stdout）** —— 能消掉幽灵行，但真正的调用仍
  可能被拆到两个 step 而僵在 running；而且用户的 stdout 也就此丢失。治标。
- **把 `onToolComplete` 也塞进 rAF 队列** —— 顺序自然对了，但完成事件延后一帧
  会让「已完成」比实际慢一拍，且 rAF 在后台/最小化窗口会被节流（同一段代码里
  已有 `done/error` 强制 flush 的注释警告过这点）。不如在源头定序。
- **完全取消 rAF 批处理** —— 等于重新引入「大输出把 WebView 打满」的老问题。

## Consequences

- 快速工具的 stdout 与完成事件同帧到达时，UI 先画分片再画完成，不再产生幽灵行。
- 组归属以 `tool_start` 所在的 step 为准；同一批并行的调用因此稳定落在同一段，
  key 仍由 `groups.length` 派生（组数只增不减，React key 稳定）。
- 前端行为有二义时的判定收敛到一个入口：`groupTimelineSteps`
  （渲染 + 折叠头摘要共用），`groupTimeline` 保留给「整条时间线」调用方。
- 验证：`tsc --noEmit` 通过；`messageParser.test.ts`（30）+ `toolOutcome.test.ts`
  （18）+ `reasoningSegments` / `reasoningPrefixGuard` 全绿，新增 4 条用例钉住
  「迟到 stdout 不新开行」「跨 step 仍配对」。
