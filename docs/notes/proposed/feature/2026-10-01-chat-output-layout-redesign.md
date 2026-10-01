# Decision Note: 对话输出排版 — 把「过程」从内容降级为旁注

Status: implemented — 2026-10-01 用户实机走查后提出，当日完成三层级落地 + 两处纯 bug 修复

> 承接 [UI_PROPOSAL_CC_HAHA_2026-09-29](../../UI_PROPOSAL_CC_HAHA_2026-09-29.md) §2
> （对话界面）。那篇 §1 的结论是「hermix 的对话界面结构已经基本对齐，不需要重做」
> —— **本篇推翻该结论**，但只推翻**对话流内部的过程呈现**这一项；侧栏、字体、
> 卡片那几批已交付且不受影响。
>
> 参照物是 minicode 实机界面（用户提供截图），不是 cc-haha 代码。

## Decision

**一个 assistant turn 是一条「正文时间线」；工具调用与思考是这条时间线上的旁注，不是内容。**

现状把过程当内容排版：每个工具调用渲染成与正文同级的行，思考是一段无边界文字墙。
结果是展开思考后正文被淹没，工具噪音淹没结论。

三条层级的硬契约（数值直接给实现者，不留发挥空间）：

| 层 | 内容 | 字号 / 行距 | 颜色 | 容器 |
|---|---|---|---|---|
| **L1 正文** | 他对你说的话 | 15px / 1.75 | `--color-text-primary` | 无背景无边框，占主栏满宽 |
| **L2 过程行** | 工具 / 读文件 / 命令 / 审批 | 13px / 1.5 | `--color-text-secondary` | 无背景，**去卡片化** |
| **L3 推理** | 思考 | 13px / 1.7 | `--color-text-thinking` | 12px 缩进 + 2px 竖线 + 独立滚动 |

层级差**不靠更浅的颜色**（深色下 tertiary 已经 4.77:1，再浅就不可读了），
**靠缩进 + 轨道 + 文字标签**。这是本方案与现状最大的分歧点，见下面 §1.2。

### 1.1 三处现状缺陷（实机截图 + 源码双证据）

**(a) 思考的「旁注」标记在深色下等于不存在。**
`components-helpers.css:295` 用 `--color-border-primary`（深色 `#2e3136`）画那条
2px 竖线，注释写明意图是「markdown 引用线的语义 —— 内部独白是'引'来的话」。

实测 WCAG 对比度（深色主题，聊天底色 `#161719`）：

| 元素 | 取值 | 对底色 | 判定 |
|---|---|---|---|
| 正文 primary | `#ededed` | **15.32:1** | AA |
| 过程 secondary | `#9a9ea6` | **6.68:1** | AA |
| 思考 thinking | `#7f848c` | **4.77:1** | AA |
| **思考竖线** | `#2e3136` | **1.37:1** | **装饰级 / 不可读** |

那条竖线是**唯一**承担"这是旁注"语义的结构标记，而它对底色只有 1.37:1 ——
这不是"浅"，是**不可见**。叠加 8px 缩进后，展开的思考与正文在视觉上是同一段文字。

**(b) 「能滚」的设计意图被同文件的后写规则静默撤销。**
`components-helpers.css:287` 定义 `.tl-reasoning-row` 时写了
`max-height: 340px; overflow-y: auto`（注释：「长思考不再把整屏撑开，展开也只占一块
固定高度」）。同一文件 `:455` 又定义了一次同名类，**后者胜出**，把
`max-height` 改回 `none`、`overflow` 改回 `visible`。截图 1 里 10+ 行思考把整屏撑开，
与 `:287` 的意图相反。**同一个类被定义两次且后写的赢，是本次排版问题里唯一一个纯 bug。**

**(c) 失败被渲染成「已完成」。**
截图 1：`搜索文件 computer use cua desktop control` 的结果文本是
`Tool 'tool_search' does not exist. Available tools: ...`，右侧徽标却是 **✓ 已完成**。
截图 2：`cua_screen_info` 的结果是
`The 600-second approval window expired before a decision was made, so the tool was not executed`
—— 同样显示 **✓ 已完成**。超时不是成功，没执行不是完成。
`messageParser.ts:264-265` 的判定链是 `detectToolError(content, is_error)` → `error|done`，
而 `detectToolError` 的纯文本模式只认 `^(Failed|Error|ERR|Tool execution failed)`
（`:336`），覆盖不到 `Tool ... does not exist` 这类由 agent runtime 产生的文案。
**根因尚需在 server 侧确认 `is_error` 是否置位**，但无论哪一环，现状的显示都是错的。

### 1.2 分层不靠更浅的颜色

现状的思路是「思考用 tertiary，比正文浅两档」。实测色阶**已经够**：
正文 15.32:1、思考 4.77:1，都过 AA，再浅就跌破可读阈值。

问题不是"不够浅"，是**没有可见的结构标记**。一句话：竖线（1.37:1）比思考正文
（4.77:1）还弱 3.5 倍，**标记比被标记的内容还不显眼** —— 视觉上标记等于不存在，
只剩 8px 缩进在承担全部区分责任。一段 10 行的思考和一段 10 行的正文，
用户看到的是同样的东西。

结论：分层靠**缩进 + 轨道 + 文字标签**，颜色只做第三位的辅助。

轨道取 `--color-text-faint`（深色 `#5f636b`，**2.98:1**，是旧值 1.37:1 的两倍多）
而不是 `--color-border-primary`；缩进从 8px 提到 12px。

**「给思考块加淡背景」这条备选被实施时实测否掉了**：候选是
`--color-chat-tool-bg`（深色 `#1d1f22`），它对聊天底色 `#161719` 只有
**1.09:1** —— 几乎与底色同值，加了等于没加。所以真正扛住区分的是
**文字标签行**（`思考过程 · 共 N 段`）：标签是文字，不依赖对比度，
深色浅色、在任何显示器上都读得到。这条否掉得不亏 —— 它把方案的
重心从「调色」推到了「加一个几何 + 一个文字标记」，后者是唯一
在两个主题下都成立的方案。

### 1.3 L2 过程行：去卡片化

现状 `ToolGroupCard`（`ToolCallBubble.tsx:96-161`）每行是 `<button class="tw-msg-row">`
+ 图标块 + 动词 + 主参 + 结果预览 + 状态徽标 + 箭头。**问题不是不好看，是它和正文抢层级**。

改为：单行文字 + 12px 极小图标 + 与正文同左对齐，无边框无背景无圆角。
hover 时才浮出右侧展开箭头与耗时。状态用**图标形状**承载（✓ 完成 / ! 失败 /
⟳ 运行 / ⊘ 跳过），**不用文字标签** —— 文字标签占掉的横向空间比图标多得多，
且色觉障碍用户读不出颜色差异时图标仍可辨（无障碍基础项，不可削减）。

### 1.4 轮次聚合：过程行不逐条罗列

截图 2 里同一条消息中 `Get-Process | Select-Object -First 3 ProcessName` 出现两次、
`echo CUA_TEST_TRIGGER` 出现两次、`cua_screen_info` 出现两次 —— 是 agent 的重试循环，
但 UI 逐条渲染且形态完全相同，噪音极大。

折叠态改为**每轮一行动词计数**（沿用 `activitySummary.ts:25-34` 已有的 `TOOL_VERBS`，
并补三个动词：`思考 N 次` / `已跳过 N 次` / `N 个失败`）：

```
✓ 思考 4 次 · 读取 4 个文件 · 编辑 1 个文件 · 执行 6 条命令 · 1 个失败
```

**失败必须单独成段且不可省略**，聚合行本身在有失败时用 `--color-error`。
这条是 minicode 缺的 —— 它只聚合成功，不暴露失败。

### 1.5 行内代码芯片（提升可扫读性，不是降噪）

minicode 最有效的一招：正文里的标识符渲染成浅灰圆角芯片，
眼睛在长段落里能瞬间抓到 `patch_file_multi`、`file.rs（340 ~ 549）`。

只对**标识符 / 文件路径 / 行号区间**着色，普通中文词不着色。
连续的行内代码合并成一片，避免一段中文里 5 个芯片把句子切碎。

### 1.6 遥测移到会话级

`TextBubble.tsx:194` 的消息头现在是 `23 小时前 · 已完成 5 个工具`。
「23 小时前」是消息创建时间，和「这一轮做了什么」混在一行，两个语义无关的东西挤在一起。

- 折叠头只放**这一轮**的过程摘要（§1.4），不再放相对时间戳
- 耗时与 token 速率移到**会话级顶栏**一行极淡遥测：`已执行 1 小时 8 分 · 166 token/s`
- 行号区间必须来自 `read_file` 实际的 `offset/limit` 参数，**不许前端猜**

### 1.7 间距节奏

| 关系 | 间距 |
|---|---|
| 轮与轮之间 | 24px（唯一的强分隔） |
| 正文段之间 | 12px |
| 正文 ↔ 过程行 | 6px |
| 过程行 ↔ 过程行 | 2px |
| 缩进：正文 / 过程行 / 思考 | 0 / 0 / 12px |

现状 `.tl-iterations` gap 14px、`.tl-iteration` gap 6px 混用，且 `.tl-text-step` 另有一套，
三处缩进意图各不相同（`UI_PROPOSAL_CC_HAHA_2026-09-29.md` §4.3 已指出过，未修）。

### 1.8 落地顺序：两个纯 bug 先于视觉重构

视觉重构会同时动十几个 class，届时**无法二分定位**是哪一处改动导致了某个回归。
所以拆成三步，每步独立可验：

**第 1 步（纯 bug，可立即做，各自带测试）**
1. 合并 `components-helpers.css:287` 与 `:455` 的 `.tl-reasoning-row` 双定义，
   恢复 `max-height` + 滚动。验证：展开超长思考时容器高度封顶且可滚。
2. 修失败判定。两条路选一：(a) 扩 `detectToolError`（`messageParser.ts:325`）的
   文本模式覆盖 `Tool ... does not exist` / `... expired` / `Missing 'x' parameter`
   这类 runtime 文案；(b) 在 server 侧确认 `tool_complete` 必带 `is_error`，
   前端只信标志位。**推荐 (b) —— 前端靠正则猜错误是这条链路的病根**，
   (a) 只是把猜的词表再扩一张。审批超时需要**第四种状态 `skipped`**
   （既非完成也非失败），这是 `ToolGroup.status` 目前没有的取值。
   验证：构造一条失败 + 一条超时的 timeline，断言渲染为 `error` 与 `skipped` 而非 `done`。

**第 2 步（可见性改造，风险最低）**
L2 过程行去卡片化 + 状态改图标 + 失败聚合行单独成段。
不动 DOM 结构的话风险很小；动的话需回归 `content-visibility: auto` 下的折叠记忆
（`activitySummary.ts:102-116` 的模块级 Map）。

**第 3 步（层级重构，风险最高）**
三层级落地、轮次聚合、行内代码芯片、遥测移到会话级。
这一步才动 `ProcessTimeline` / `TextBubble` 的渲染结构。

## 执行结果（2026-10-01）

三步全部落地。落地过程中最重要的发现不是排版本身，是**这个界面有
三份并行的错误判定实现**，靠一句注释维持一致 —— 与 H2 的形状完全相同。

### 两处纯 bug（先修，都可独立验证）

1. **`.tl-reasoning-row` 双定义**。同文件两处定义，后写的把
   `max-height: 340px` + 滚动改回 `none` / `visible`，2026-09-30 的
   「长思考封顶滚动」被静默撤销。已合并为一份，恢复封顶 + 滚动。
2. **失败被渲染成「✓已完成」**。根因比预想的更宽：`detectToolError`
   有**三份拷贝**（`messageParser` / `ToolEventCard` / `protocolAdapter`
   串的函数值），`ToolEventCard` 里那��的注释原文是
   「any change here MUST be mirrored in `messageParser.detectToolError`
   and vice versa」。三份都只认 `^(Failed|Error|ERR|Tool execution failed)`，
   全都漏判 agent runtime 的 `Tool 'x' does not exist`。

   处理方式不是扩三份正则，是**抽唯一入口** `utils/toolOutcome.ts`
   （零依赖），三处改为 import。这与 H2 同一条教训：契约类逻辑抽到
   唯一入口，靠注释同步的实现迟早会走散。

### 意外发现：两条渲染路径本就汇到同一处

用户要求「运行会话和恢复会话要一致」。查下来比预想的好：
`protocolAdapter.wireArrayToChatMessages` **不发射独立的
`role:"tool"` 消息**，而是把工具行并回 assistant 的 `execution_timeline`，
恢复后同样由 `ProcessTimeline` → `groupTimeline` 渲染。也就是说
两条路径**本来就共用同一个渲染器**，一致性由架构保证，不靠人记着。

即便如此仍发现一处真实的不一致并修掉：恢复态合成的 `tool_complete`
事件**不携带 `is_error`**（运行态一直带）。补上后两条路径拿到的是
同一份权威事实，而不是让前端去猜文本。

### 已落地

| 项 | 落点 |
|---|---|
| 三层级 | `.tl-text-inner`(L1) / `.process-line-*`(L2) / `.tl-reasoning-*`(L3) |
| 过程行去卡片化 + 状态图标形状 | 新 `ProcessLine.tsx` + `styles/process-line.css` |
| 运行态/恢复态共用渲染 | `ToolGroupCard` 与 `ToolMessageCard` 都只映射成 `ProcessLineVM`，渲染委托 `<ProcessLine>` |
| `skipped` 第四态 | `ToolGroup.status` / `ToolMessageMeta.status`，判定在 `utils/toolOutcome.ts` |
| 折叠摘要失败/跳过单列 | `buildActivitySummary` + `.tl-process-failed` / `.tl-process-skipped` |
| 思考标签行 | `ReasoningBlock` 新增 `思考过程 · 共 N 段` |
| 行内代码芯片 | `.prose code` 从「透明底 + 下划线」改为 16% 混合圆角块（实测 1.55:1） |
| 消息头去重 | `TextBubble` 移除 `已完成 N 个工具` —— 与折叠头重复且数字来源不同 |

**未做**：会话级遥测条（`已执行 X · N token/s`）。`token/s` 需要
流式速率累计，当前只有 `contextUsage` / `usageSnapshot` 的**总量**，
没有速率信号 —— 硬做只能显示一个编出来的数字。耗时已经在折叠头
（`reasoning-stat`）里，重复信息已清理。

### 走过的弯路

1. **「给思考块加淡背景」** —— 实测 `--color-chat-tool-bg` 对聊天底
   只有 1.09:1，加了等于没加。改用文字标签行，见 §1.2。
2. **行内代码芯片的档位** —— 先试 12%（1.09:1，与底同值，不可见），
   提到 16%（1.55:1）才读作「块」。**背景是分组信号，不是对比度
   载体**：文字对比度保持 primary 15.32:1 不动，动的只是底。
3. **测试里 `tool_complete` 用了不同的 tool 名** —— 被
   `groupTimeline` 的配对键判成断流 → error。是 helper 写错不是代码
   错，但正好证明了「配对键不一致会被误判为中断」这条路径存在。

### 自查

`ProcessLine` 与 `LiveTerminal` 必须拆成独立文件：前者被
`ToolGroupCard` / `ToolMessageCard` 用，后者原本在 `ToolCallBubble`
内部 —— 不拆就形成循环依赖。`LiveTerminal` 的 class 名保持不变，
所以 CSS 无需跟着改。

新增 18 条测试（`utils/toolOutcome.test.ts`），其中 4 条是**四入口
一致性**断言：同一份 `tool_complete`，`groupTimeline`（运行态）/
`groupEvents`（ACUI）/ `expandTimelineToMessages`（恢复态）/
`protocolAdapter` 投影必须给出同一个状态桶。

## Alternatives considered

- **只调颜色，把思考再浅一档** —— 否决。见 §1.2：色阶已经够，再浅跌破可读阈值，
  而真正的缺口是结构标记。截图里问题不是"浅"，是"看不出这是另一类东西"。
- **给每个工具调用保留卡片外壳，只改配色** —— 否决。卡片化本身就是在宣告
  "这是内容"，与"过程是旁注"的目标直接冲突。`.chat-tool-call` 已经有
  `--tool` 背景 token（`components-helpers.css:552-557`），要去掉的是整个外壳不是颜色。
- **只把思考折叠起来，不动过程行** —— 否决。截图 2 的噪音主要来自重复的过程行，
  不是思考。折叠思考解决不了重试循环刷屏。
- **照抄 minicode 的聚合行** —— 否决。minicode 只聚合成功、不暴露失败，
  直接抄会让失败静默消失。本方案保留动词计数但强制失败单独成段。
- **把 L2 过程行全部删掉，只留折叠头** —— 否决。用户在"它到底改了什么文件"上
  需要明细。折叠头给概览、展开给明细，两层都要留。
- **在 `ProcessTimeline` 里重写整个 timeline 渲染** —— 否决（作为第一步）。
  §1.1(b) 的双定义和 §1.1(c) 的状态判定是两个可以独立验证的独立 bug，
  应各自单独提交、各自带测试，再做视觉重构。混在一起改无法二分定位。

## Consequences

- **获得**：正文在展开状态下重新可扫读；失败与超时不再伪装成成功；
  一轮 8 个工具调用从 8 行降到 1 行；行内标识符可被眼睛瞬间定位。
- **放弃**：逐条工具行的默认可见性（收进折叠）；"已完成/失败"文字标签
  （换成图标形状）；折叠头里的相对时间戳。
- **代价**：`tw-msg-*` 系列 class 需重排；`TOOL_VERBS` 需补动词；
  `detectToolError` 的文本模式或 server 的 `is_error` 置位逻辑要改；
  过程行的 DOM 结构变了，`content-visibility: auto` 下的折叠状态记忆
  （`activitySummary.ts:102-116`）需回归验证。
- **风险**：折叠态聚合可能隐藏"具体哪个失败了"。缓解见 §1.4 —— 失败单独成段
  且聚合行变红，展开即见明细。这是本方案最需要真机走查验证的一条。
- **前置依赖**：本篇不覆盖 L2/L3 设置中心重构（Switch / SettingsRow）。
  过程行若要加"始终展开思考"开关，宿主组件在那批里。
