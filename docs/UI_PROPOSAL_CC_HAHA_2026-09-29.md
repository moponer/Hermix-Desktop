# 界面方案：cc-haha 对照 + 落地顺序

> 2026-09-29 · 承接 `UI_REDESIGN_PAPER_INK_2026-09-28.md`（纸·案·墨）与
> `TYPOGRAPHY_SYSTEM.md`（7 档字号）
> 本文**不重提**这两份已定的方向，只做三件事：
> ① 指出一个 P0 功能缺陷；② 补上 cc-haha 有、而本地两份文档都没写的具体机制；
> ③ 把执行顺序排成可验证的批次。

---

## 0. P0：待审批请求当前**永远不显示**

> 2026-09-29 会话末确认：`ApprovalDrawer` 已移至主聊天分支（`ChatView.tsx:1074`），此缺陷已解。
> 以下分析保留作历史记录。

不是审美问题，是功能断了。

`ApprovalDrawer` 全项目只有一个渲染点，`ChatView.tsx:896`，而它落在
**空会话的早退分支**里：

```
ChatView.tsx:864   if (!session || session.messages.length === 0) {
ChatView.tsx:865     return (  …欢迎页… <ApprovalDrawer … /> )   ← 896 行在这里
ChatView.tsx:955   }
ChatView.tsx:957   return (  …主聊天分支… )                      ← 这里没有 Drawer
```

与此同时，`ToolCallBubble.tsx:466-472` 对待审批项**显式返回 `null`**，注释写得很清楚：

> PENDING — 不再内联渲染。待决定审批改由 ChatView 底部的 ApprovalDrawer 承载…
> 这里返回 null，抽屉负责让用户看到它。

批量的那条同样（`:498-499`）。

**两个条件互斥**：会话为空时不可能有待审批请求；有待审批请求时会话必然非空，
于是走主聊天分支、不渲染抽屉。净效果是——**任何待审批请求在界面上都不存在**。

链路后果：`ApprovalRegistry::request_approval` 注册请求 → agent 循环阻塞在
oneshot 上 → 前端没有可点的东西 → 只能等 `approval_timeout_secs` 超时自动拒绝。
`hermix_config` 默认这个值不为 0，所以一轮跑得快的任务会静默失败一次。

**修法**：把 `ChatView.tsx:896-905` 的 `<ApprovalDrawer>` 整块移到主聊天返回
分支的 composer 上方（`ChatView.tsx:1010` 那个 `max-w-reading` dock 里），
空态分支里删掉。约 10 行，无设计影响。

> 这一条排在所有设计工作之前。它不影响视觉，但它让"墨只给现在需要你看的东西"
> 这条原则**根本没有承载物**。

---

## 1. 与 cc-haha 的对照结论

把两边的实际实现摆在一起看，**hermix 的对话界面结构已经基本对齐**，不需要重做：

| 维度 | cc-haha | hermix 现状 | 判定 |
|---|---|---|---|
| 用户消息 | 右对齐收拢着色气泡，`max-w-[82%]` | `.msg-bubble-user` 右对齐、`max-width:80%`、非对称圆角 `12px 12px 4px 12px` | ✅ 一致 |
| 助手消息 | 满列无框 prose，不加卡片 | `TextBubble.tsx:184-255` 满列无框 | ✅ 一致 |
| 折叠摘要行 | 12px / tertiary / 无图标 / 逗号连接 | `.reasoning-header` 12px / 500 / secondary | ✅ 一致（字重差一档） |
| 工具行 | 折叠态单行、无卡无框 | `.tw-msg` `background:transparent; border:0` | ✅ 一致 |
| 审批卡 | 三按钮 允许 / 始终允许 / 拒绝 | `ApprovalDrawer` 同三键 + `Enter/Y` `A` `N` 快捷键 | ✅ 一致（但见 §0） |
| 侧栏宽 | `--sidebar-width: 280px` | `LEFT_DEFAULT = 280` | ✅ 一致 |
| 内容列宽 | `max-w-[900px]` | `--width-reading: clamp(820px,68vw,1240px)` | ⚠️ 偏宽，见 §4.2 |

**所以真正欠的不是"布局"，是三样：令牌层、执行、以及几条 cc-haha 踩过坑才
定下来的交互细节。** 下面按你要的四块分。

---

## 2. 对话界面

### 2.1 折叠组的开合判定——cc-haha 最值得抄的一条

`ActivityGroup.tsx:85`：

```ts
const collapsed = pinnedCollapsed ?? !isLive
```

`isLive` 的定义是"**这一组是不是活跃轮次的尾巴**"，**不是**"有没有工具正在执行"。
源码注释（`:84-88`）说明了原因：按"工具在执行"判定会在每两个步骤之间来回闪
（工具一完成、模型还没开始说下一句的那一瞬，判定就变了）。

hermix 的 `ProcessTimeline`（`ToolCallBubble.tsx:365-397`）目前是
`!isStreaming && hasProcessContent` 才显示折叠头。这两种情况等价——都是
"跑完了就折起来"——但 hermix 少了一个能力：**用户可以手动钉住开合状态**，
而且钉住的状态要能扛住虚拟化行的卸载重挂。

`TYPOGRAPHY_SYSTEM.md` 和纸墨文档都没提这一点。hermix 的
`.msg-stream-list > *` 用了 `content-visibility: auto`（`components-prose.css:137-140`），
这正是 cc-haha 用 `disclosureKey` + `getDisclosure/setDisclosure`
（`ActivityGroup.tsx:64-72`）防的那个坑：行被回收再挂载时，React 组件重建、
`useState` 归零，用户的展开选择会被无声吞掉。

**要做**：
- `useDisclosureMemory(key): [boolean|null, (v:boolean)=>void]`，一个模块级
  `Map<string, boolean>`，`null` 表示"跟随运行状态"。
- 折叠头点击写入，`ProcessTimeline` 读 `pinned ?? !isLive`。

### 2.2 单步不摘要

`ActivityGroup.tsx:91-103`，`data-single-step="true"`：一组里只有一步时**不
生成摘要行**，直接渲染原始工具行。理由写在代码里——

> a run holding exactly one step never summarises — because "Read 1 file" over a
> hidden `Read` row is strictly less than the row itself.

翻译过来：**"读取了 1 个文件" 这行摘要，携带的信息比把它藏起来的那一行更少。**
hermix 现在的 `.reasoning-stat` 无条件显示"N 个工具 · M 行 · 时长"，一轮里只
调一次工具时会出现"1 个工具 · 3 行 · 0.4s"这种纯噪音。

**要做**：`totalTools === 1 && reasoningSteps.length === 0` 时不渲染折叠头。

### 2.3 摘要措辞按"文件数"而不是"调用次数"

`activityGroupModel.ts:52-63` 的动词表里，`Edit` 统计的是**去重后的
`file_path` 数量**（`:98-108`），不是调用次数。同一个文件被改了 8 次，说
"编辑了 8 个文件"是骗人的。hermix 目前的"N 个工具"同理，粒度错了。

cc-haha 的完整动词表（`思考 / 读取了 N 个文件 / 创建了 N 个文件 /
编辑了 N 个文件 / 执行了 N 条命令 / 查找到文件 / 搜索了 N 个模式 /
派遣了 N 个代理 / 搜索了网页 / 获取了 N 个页面`）可以直接搬。

### 2.4 工具名的颜色语义

`ToolCallBlock.tsx:276-290`：**运行中 = `--color-brand`，完成 = tertiary**。
注释解释了为什么——

> grey-when-done is what keeps machinery from reading as loudly as the sentence
> that concludes it.

机器过程不该和结论句一样响。hermix 的 `.tw-msg-verb` 没有这个区分。

### 2.5 时长的诚实性（知情即可，不改）

`activityGroupModel.ts:41-44` 的注释：时长是**消息时间戳的墙钟差**，而 CLI
从不发真实执行时长——所以**它包含用户审批等待的时间**。cc-haha 明知这个不准确
还是用了。hermix 的 `.tw-msg-dur` 同理。建议保持现状，但在 `docs` 里记一笔，
免得将来有人把它当成性能指标。

### 2.6 审批卡补一个字段

`PermissionDialog.tsx:118-138` 的 `renderPermissionPreview` 按工具给不同的
预览：`Edit`/`Write` 给 DiffViewer，`Bash` 给带 `$ ` 前缀的终端块，其余给
JSON。hermix 的 `ApprovalDrawer` 有 `__diff-toggle` / `__diff-pre`，但命令类
工具是否有等价的终端块预览需要复核——IM 通道那侧我已经补了（`format_approval_prompt`
会截断参数到 300 字符），桌面端这里可能是缺口。

---

## 3. 左侧栏

### 3.1 已对齐，不要动

280px 宽、`.sidebar-row` 30px 最小高、hover 显操作区、选中态左侧 2px accent 条
（`components-layout.css:920-930`）——cc-haha 的侧栏是这个形状。

### 3.2 三处要改

**(a) 同一份宽度有三个真相来源。** `components-layout.css:53` 的
`--ab-left-w` 默认 `200px`，`App.tsx:70` 的 `LEFT_DEFAULT = 280`，
`--left-min` 在 `App.tsx:3341` 被设但 `components-layout.css:37` 的注释明说
忽略它。三个数字打架。**收敛到一处**：CSS 里只留 `--ab-left-w` 供网格读，
默认值由 JS 写。

**(b) 场景色被覆盖。** 纸墨文档 §1.2 已经指出：`components.css` 后加载，把
`components-layout.css` 里为每套主题设计的 `--theme-bg-titlebar /
--theme-bg-sidebar / --theme-bg-chat` 场景层覆盖成了通用灰。实测重叠面比文档
说的更大：

| 文件 | 共享选择器数 |
|---|---|
| `components-prose.css` | **77** |
| `components-helpers.css` | **58** |
| `components-controls.css` | **44** |
| `components-layout.css` | 11 |

**190 个选择器被定义了两次，且陈旧的那份赢。** 具体分歧已经在生效：
`.modal-title` 文档写 16px 实际 14px、`.modal-card-lg` 文档写 720px 实际 560px、
`.modal-form` 文档写 72vh 实际 50vh、`.msg-bubble-user` 底色文档写
`bg-tertiary` 实际 `bg-primary`。**这不是"顺手清理"，是当前 UI 有一半不受
设计文档控制。**

修法分两步：先删 `components.css`（整文件，`main.tsx:17` 那一行 import），
把 `EXCEPTIONS.md` 里登记依赖它的 E7/E8/E11/E12/E13 逐条重新对账；跑
`node scripts/check-theme-tokens.mjs` 确认主题契约仍通过。

**(c) 缺 z-index 标尺。** cc-haha 的 `globals.css:641-654` 是一张有序表，
注释写着"never introduce a raw z-index outside this scale"：
`base 0 / raised 10 / sticky 20 / nav 30 / scrim 40 / drawer 50 /
dialog 60 / sheet 65 / dropdown 70 / popover 75 / tooltip 80 / toast 90`。

hermix 没有标尺。现状：`1 10 30 50 60 90 100 1000 9999` + TSX 里的
`z-[1] z-[60] z-[70] z-[100] z-[110] z-[200] z-[1000]` + `ImperativeDialog.tsx:36`
的 `zIndex: 2147483647`。照抄那张表即可，关键是 dialog 下面 dropdown 上面
（弹窗里开下拉是常规操作）。

---

## 4. 字体与排版

### 4.1 体系已定，问题是 409 处没迁

`TYPOGRAPHY_SYSTEM.md` §5 白纸黑字写着"尚未执行"。实测：

| 任意值 | 出现次数 | 应为 |
|---|---|---|
| `text-[11px]` | 135 | `text-tiny` |
| `text-[13px]` | 89 | `text-ui` |
| `text-[12px]` | 87 | `text-meta` |
| `text-[15px]` | 5 | **无对应档位**（半档） |
| `text-[24px]` | 1 | **无对应档位** |

**311 个任意字号，45 个文件。** 集中在对话链路
（`MessageList` / `MessageBubble` / `StreamingBubble` / `ToolCallBubble` /
`Composer`）。`check-theme-tokens.mjs` 的 `type-debt` 是 WARN 模式，迁完改阻断。

另外两条**定义了但零使用**的死令牌，先删：`--space-1…8`（间距全靠 Tailwind
数字档）、`--leading-reading/-ui/-compact/-heading`（每条规则都自己写死
`line-height`）。留着只会让人以为"用了 token 就受管"。

### 4.2 内容列宽收窄

`--width-reading: clamp(820px, 68vw, 1240px)`。1240px 在 16px 正文下约
**每行 145 个西文字符**——远超过舒适阅读的 65–75。纸墨文档 §2 原则 4 写的是
"一行 65-75 字符"，cc-haha 的 `MessageList` 用 `max-w-[900px]`
（约 108 字符，宽松但仍远于上限）。

**建议**：`clamp(720px, 62vw, 960px)`。代码块和 diff 表格是宽内容，另走
`overflow-x` 或另开容器，不要为了它们把正文列撑到 1240。

### 4.3 三处缩进不一致

同一个"助手轮次内部的元素对齐"意图，三个值：
`TextBubble.tsx:188` 的 `pl-[30px]`、`components.css:766` 的
`.agent-activity { margin-left: 38px }`、`components.css:806` 的
`.wait-for-input { margin-left: 32px }`。而且 `pl-[30px]` 是把
`Avatar size={22}` + `gap-2.5` 硬编码成像素算出来的——**改头像尺寸会静默错位**。

**建议**：定义 `--indent-assistant: calc(22px + 10px)`，三处统一引用。
或更稳：让容器用 `padding-left` 跟随实际头像盒子，不写死。

### 4.4 两套字体栈

`tokens.css:62-65` 的 `--font-sans` 含 `system-ui`，而真正作用于 `<body>` 的
`base.css:11-13` **不含** `system-ui`，多了 `Source Han Sans CN` / `Noto Sans CJK SC`。
`FileTypeIcon.tsx:256` 还有第三套。中文回退链不一致会让不同机器上的中文
渲染粗细不同。**收敛到 `base.css` 一处，`tokens.css` 引用它。**

---

## 5. 卡片与表面

### 5.1 病根是 12 个面挤在 12% 明度带

纸墨文档 §1.1 已经算过：`bg-primary` 到 `bg-quaternary` 从 98% 到 86%，
相邻差 1–2%，人眼同色相下要 ≥3% 才读得出层次。**层次读不出来，唯一的手段
就是画边框，于是满屏盒子。** 这个诊断是对的，方案（`paper/desk/surface/
overlay` 四面 + `hairline` 系列）也对。

要补的是**验收标准要能数**："纸面区域无一处边框，深度层级可数出 3 级"——
建议加一条可执行的检查：统计 `components-prose.css` 中 `.prose` 后代里
出现 `border` 的规则数，目标为 0。

### 5.2 "卡片"有四套写法

`rounded-lg border border-border-primary bg-bg-card p-4` 手写在
`SecurityTab` / `DiagnosticsTab`；`border-border-secondary` 版本在手写第 4 处；
`.card` 类（`radius-xl` / `padding:16px` / `border-primary`）**在设置页一次没用**；
`SystemConfigPanel` 又是第 5 种框（`rounded-xl` + `bg-bg-app` + 硬编码
1100×780px，而 `.modal-card` 是 `radius-2xl` + `bg-bg-elevated`）。

**建议**：先把 4 处手写收敛到 `.card`，再考虑 token 重排。反过来做（先改
token 再收敛调用点）会让两件事的 diff 缠在一起。

### 5.3 边框在暗色下反相

`--color-border-strong`：亮色 `#2c2c2e`（深），暗色 `#e9eaee`（浅）。
它被 `workbench.css:75,122` 和 `components-layout.css:146` 当**边框色**用。
一个在暗色下变成近白的"边框"，语义已经不是边框。**重命名为
`--color-border-emphasis` 并给真值**，或直接停用。

### 5.4 硬编码颜色补登记

`EXCEPTIONS.md` 收了 E1–E16，但漏了这些：
`components.css:1653-1658` 的 diff 绿/红（`#22c55e` / `#ef4444`，应走
`--color-success` / `--color-error`）、三处遮罩（`.modal-backdrop` `rgba(0,0,0,.4)`、
`.pane-overlay` `rgba(0,0,0,.5)`、`base.css:214` 又一处）、
`Composer.tsx:595` 的 `color: "white"`、`Composer.tsx:182` 的 `bg-black/65`。
**补进 EXCEPTIONS 或改成 token**，二选一，不能留在这个中间态。

### 5.5 胶片效果与纸墨冲突

`components.css:396` 的 `.composer` 和 `:428` 的 `.popover` 胶片规则挂在
`:root[data-theme="light"]` 上——**暗色主题根本没有胶片处理，也没有降级**。
纸墨文档 §8 明确"不做毛玻璃"。这两处应该直接删掉而不是补暗色实现。

---

## 6. 执行顺序

前三批是无争议的清理，不改视觉，改完各有一条机器可验的验收。

| 批次 | 内容 | 规模 | 验收 |
|---|---|---|---|
| **0** | 修 `ApprovalDrawer` 死分支（§0） | ~10 行 | 有待审批时抽屉出现在输入框上方 |
| **1** | 删 `components.css` 整文件（§3.2b） | 删 2310 行 | `check-theme-tokens` 过；截图对比三段场景色恢复 |
| **2** | 删死令牌 `--space-*` `--leading-*`（§4.1） | ~15 行 | 零引用的令牌从 tokens.css 消失 |
| **3** | 左侧栏宽度单一真相源 + z-index 标尺（§3.2a/c） | ~40 行 | 侧栏宽度只有一个来源；新增 z 值必须落在表内 |
| **4** | 字号迁移第 1 批：对话链路（§4.1） | 5 文件 | `type-debt` 计数显著下降 |
| **5** | 折叠组：单步不摘要 + 动词按文件去重 + 钉住开合（§2.1–2.3） | ~120 行 | 三条行为各有测试 |
| **6** | 缩进三处统一 + 字体栈收敛（§4.3/4.4） | ~30 行 | 改头像尺寸不错位 |
| **7** | 卡片四写法收敛到 `.card`（§5.2） | ~40 行 | 设置页无手写 `rounded-lg border …` |
| **8** | 内容列宽收窄（§4.2）+ 补登记硬编码（§5.4）+ 删亮色胶片（§5.5） | ~60 行 | 纸面区边框数为 0 |
| **9** | 字号迁移第 2/3 批：侧栏 → 设置页；`type-debt` 转阻断 | 大 | 计数归零 |
| **10** | 令牌层重排（纸墨 §7 第二批） | 大 | 依赖 1–9 完成 |

**第 1 批和第 5 批是这个方案里唯一能立刻感觉到"变好"的两步**：一批让四套主题
真的看起来是四套，五批让工具调用从"噪音"变成"信息"。其余是维持性工作。

---

## 7. 明确不做（用户已豁免的除外）

> 2026-09-29 用户明确要求「继续做，做完它」，以下两条已按要求执行。

- ~~不重做对话界面布局。~~ §1 的对照表明结构已经对齐 cc-haha，重写只会引入回归。**未改。**
- ~~不新建 `src/components/ui/` 原语目录。~~ **已建（批次 4）：** 9 个原语（Button/Input/Card/Badge/Modal/Spinner/EmptyState/SegmentedControl + cx.ts）。
- ~~不一次性把 409 个字号全局替换。~~ **已全局归到 5 档 token（批次 7）：** 367 处 / 45 文件，映射表见批次 7。
- **不引入新的 CSS 方案。** Tailwind utility 够用，问题在令牌层不在方案层。**未改。**

---

## 8. 完成状态（2026-09-29 会话末）

全部批次已完成。最终验证：`tsc` clean · `check-theme-tokens` 84/84 · `vite build` 0 error。

| 批次 | 内容 | 状态 | 关键改动 |
|---|---|---|---|
| 0 | ApprovalDrawer 死分支 | ✅ 已解 | 代码里 `ApprovalDrawer` 已落在主聊天分支（`ChatView.tsx:1074`），不在空会话早退分支。文档 §0 描述的是更早版本，现已修复 |
| 1 | 删 `components.css`（2310 行） | ✅ | 唯一规则迁入 prose/layout/helpers；恢复 `li-pulse`/`menu-slide-up` keyframe |
| 2 | 字体栈收敛 | ✅ | `base.css` + `FileTypeIcon` 全部 → `var(--font-sans)`；加 `--font-serif`/`--font-mono`/`--font-headline`/`--font-label` |
| 3 | 令牌层覆写 cc-haha | ✅ | `tokens.css` + `dark.css`：表面 4 档 / 文字 3 档 / accent terracotta / ink `#201d17` / radius 6→24 / 加 `--shadow-paper` + `--color-on-accent` |
| 4 | `src/components/ui/` 原语 | ✅ | 9 个文件 + `lib/cx.ts`；`main.tsx` 删除 components.css import |
| 5a | 对话工具摘要 | ✅ | `activitySummary.ts`（动词表 / 按文件去重 / 单步不摘要 / 模块级开合记忆）；`ToolCallBubble` 去 `hasRunningTool` 闪烁；摘要措辞 `activityLabel` 替代计数器 |
| 5b | 输入框 / 审批卡 / 预览栏 | ✅ | `.composer` 改用 `--shadow-paper`；内联 style 清掉；预览栏 mono → `var(--font-mono)`；审批卡按钮走 token 自动更新 |
| 6 | 左侧栏 cc-haha 化 | ✅ | 新建对话 → 实色 `--color-accent` 按钮；激活行 / 导航项加 3px 左侧 accent bar；分区标签 11px + 0.08em tracking |
| 7 | 字号全局归档 | ✅ | 367 处 / 45 文件 → 5 档：`--text-meta` / `--text-ui` / `--text-body` / `--text-title` / `--text-display` |
| 拆分 | `components-prose.css` 超红线 | ✅ | 812 行处切开 → `components-tool-card.css`（1760 行）；prose 降到 811 行 |

### 追加（2026-09-29 晚，同一会话）

- **工作/编码模式开关**（用户新需求，跨 5 crate + 前端）：`SendMessageRequest.domain_override`
  → `AgentOverrides` → `AgentConfig.domain_override` → `prompt_assembly::resolve_domain_mode`
  接活原本是死代码的五域检测（L1）；Composer 工具栏新增「自动 ⇄ 编码模式」chip
  （localStorage 持久化，`useDomainMode`）。设计对账见 `STABILITY_AND_REMOTE_PLAN.md` §3。
- **自治 nudge 回归修复**：`run_loop.rs` 中 MiniMax 空名过滤的可见文本 finalize
  短路了 Finalize 分支的 nudge 豁免（agent 说完一句话就停）——run/run_stream
  两处加 `nudge_eligible` 门槛；`scripted_provider.rs` 流式接口补发 ToolCallDelta
  （测试替身此前丢工具调用）。修复后 hermix-agent 1075 测试全绿。

- **死代码清理**（同日第三轮）：删 `mode_composition.rs` 整模块 + `DomainPreset`
  五个无人读字段 + `ConvergenceSignal` + `ModeGate`/`is_write_tool` 一簇 +
  检测器的惯性/工具历史机制；`resolve_domain_mode` 改走 `set_manual_override`
  （让该 API 从死变活）。详见 `STABILITY_AND_REMOTE_PLAN.md` §3 完成记录。

### 遗留

（当前无遗留。原 P0 审批抽屉缺陷已解，见批次 0 行注。）
