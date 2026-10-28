# Decision Note: 值守页「说的和实际不一致」六处 + 会话行不再像灰表格

Status: implemented — 2026-10-28 交付：状态口径纳入待办、环与流水不再用现在时说
上一轮、计数与行数同源、日志行剥掉落库前缀、桥接工具名不进流水、面板统一纸面；
同日第二轮补四处（环上 0% 的假进度点、会话表启动快照、编造的 `CH` 读数、
错误提示挤掉历史提问的转录合并缺陷）

## Decision

用户 2026-10-28 上传整页截图，要求「找出与实际不符合的，分析原因并先修复」。
逐条对代码后确认六处，全部是**同一屏上互相矛盾的读数或原文泄漏**：

1. **顶栏写「静默观察 · 0 个任务待触发」，右翼写「等你决定 1 件」。**
   根因：`stateText` 只看 `busy`（自动化在途 + 她的轮次 + 主对话轮次），
   完全没把 `pending` 计入。她明明停在原地等人授权，页面却说没事可做。
   改：`pending.length > 0` 排最前 → `等你决定 · N 件待办`，状态灯同时多一档
   `is-hot`（警告色）、文案 `待你决定`。
2. **顶栏「在跑 00」旁边，中轴环写「她正在用 task_create」。**
   根因：`twin.turn.activity` 是内存态，流在工具中途断掉时 `onToolComplete`
   不会再来，`activity` 永远停在最后一个工具名上；而环的取值顺序把
   `activity` 放在 `streaming` **之前**判断 → 已经不在跑也照样用现在时说。
   改：先判 `streaming`；另在 `twinApi` 出口按 `streaming` 归一 `activity`
   （非 streaming 一律 null），消费侧不必各自防。
3. **右翼「刚才发生 3 条」，屏幕上 9 行。** 根因：面板头数的是
   `workDays.length`（日志天数），渲染的是"本轮流水 + 8 天日志"。
   改：抽出 `happenedRows` 一个数组，**渲染源即计数源**。
4. **同一批日志在左翼「最近工作日志」与右翼「刚才发生」各印一遍**，而且
   两处取的字段顺序不同（左翼 `conclusions` 优先、右翼 `progress` 优先）——
   同一天能显示两段不同的话。改：日志归左翼（04），右翼只放"她做了什么"
   （本轮/上一轮流水）；没动作时给一句说明。
5. **日志行原文泄漏落库格式**：`- 10:59 [automati] USER: 请查一下…`。
   根因：`daily_memory` 的 `conclusions` 存的是写入格式
   （`hermix-agent/src/memory_prefetch.rs::append_daily_memory_conclusion`
   生成 `- HH:MM [sid8] 摘要`），前端把首行原样上屏 —— 行首日期列之外又出现
   时刻、`[automati]` 是被截断的 `automation:…`。改：`lib/readout.ts` 新增纯
   函数 `dailyLine()` 逐段剥前缀（项目符号 / 日期 / 时刻 / `[sid]` / 角色词），
   剥空才退回原文；带 7 条单测。
6. **流水里有三行同名 `tool_call`。** 根因：`tool_call` 是 `stream_collect`
   在模型没给出工具名时的**桥接占位名**，不是用户认识的工具。改：App 侧
   `BRIDGE_TOOL_NAMES` 过滤 `tool_call` / `tool` / `function` / 空串，既不记
   流水也不当"正在用 X"的 X。

另有两处"难看"是结构性的，一并收口：

- **次要面板透明 → 字浮在点阵上**：`.twin-box--quiet` 把 03/04 的背景设为
  transparent，于是同一翼里 01/02 是实心卡、03/04 直接铺在页面点阵上，一列
  面板像没画完。删除该变体，主次改由字重与墨色承担。
- **会话行读成灰表格**：时间占着行首列（每条都不同，根本不构成列）、标题是
  12px 的 secondary、行间无悬停底。改成与主侧栏 `.sidebar-row` 同一套语法：
  13px 主墨标题 + 行尾弱墨时间 + 悬停铺 `--color-bg-hover-subtle`
  （`margin: 0 -6px` 让底色吃到面板内边距，文字位置不动）；行内的消息条数
  移除（与面板头的「N 条」同单位不同含义，摆一行里必被读错），改挂 `title`。

## 第二轮（同日，同一张截图复核出来的三处）

7. **环上 0% 却亮着一点**：中轴读数写「00 个任务在跑」，环的 12 点方向却有一个
   强调色圆点。根因：进度弧恒挂 `strokeDasharray="0 C"` + `strokeLinecap="round"`，
   零长度 dash 的圆帽在 Chromium 里会画成实心点。改：进度弧只在
   `runningRatio > 0` 时渲染；`TwinView.test.tsx` 用环上 circle 数（3 / 4）
   钉住。
8. **「最近会话」是启动那一刻的快照**：面板行写着「New chat · 19h · 2 条」，
   而同一屏的工具流水正在跑。根因：宿主的 `api.getSessions()` 只在
   `useEffect(..., [])` 里拉一次，她跑完一轮不会刷新 → 标题 / 相对时间 / 条数
   全是旧的。改：App 侧在 `twin.streaming` 由 true → false 时补一次
   `refreshSessionMeta()`，只覆盖 `title / message_count / updated_at` 并追加
   服务端新出现的行，**已载入的 `messages` 一律不碰**（清空会引发主对话重拉）。
9. **底条 `CH ALI-01` 是编的读数**：遥测条注释声称「全部是真值」，但 `CH` 是
   写死字符串，页面里根本没有"通道"这个概念。改：删掉该格，保留
   SID / TICK / RUN / Q（这四格分别对得上会话 id、轮询常量、在跑数、待办数）。

10. **本轮只剩一条错误提示时，连刚才问的话都消失**：`mergeTranscript` 无论
    live 有没有 user 行都先裁掉历史尾部的用户行，于是
    `live = [assistant("出错了")]` 时面板只剩一句无头无尾的「出错了」
    （单测在 HEAD 上本就是红的：`twinTranscript.test.ts` 2 条失败）。
    改：live 非空但**没有 user 行**时历史原样保留；有 user 行才去找切点、
    连同前面的孤儿提问一起交给 live 顶替。同时把「超出上限」那条用例的输入
    从 10 条连续 user（撞孤儿规则的病态形状）改成成对历史 —— cap 的断言
    保留，输入回归真实落库形状。

## Alternatives considered

- **把「静默」改成五态枚举**：`idle / running / waiting / error / loading`
  五个名字对用户没有增量信息；三档（待你决定 / 值守中 / 静默）+ 一句
  `stateText` 已经说清；否决。
- **保留右翼的日志回退（日志只显示一行）**：仍是同一批事实两个家，且"取哪个
  字段"的口径还得在两处对齐；否决。
- **在前端给 `activity` 加超时（如 30s 后清空）**：把"流没有正常结束"这个
  事实藏进计时器，治不了根；按 `streaming` 归一才是同源；否决。
- **把 `dailyLine` 放到 Rust 侧清洗落库格式**：那会改变模型读到的 daily_memory
  内容（时间与 sid 对模型有用），显示层清洗才是对的一层；否决。

## Consequences

- 一屏之内不再出现互相矛盾的读数：待办、在跑、静默只有一个出处
  （`stateText`），右翼流水与面板头计数同源。
- 左翼 01 的读数改成 `启用 N / M`（配合 02 的「任务总数」）—— 只写 `0 个`
  会被读成"一条任务都没有"，实际是 7 条全停用。
- 值守页四块面板统一纸面，跨主题自动跟随（颜色全来自令牌）。
- `firstLine()` 从 `TwinView.tsx` 移除（被 `dailyLine` 取代），逻辑收进
  `lib/readout.ts` 便于单测；`TwinView.tsx` 不再有未测的格式化分支。
