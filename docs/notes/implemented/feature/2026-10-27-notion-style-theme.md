# Decision Note: Notion 风格主题（`notion`）与内容型界面风格规范

Status: implemented — 主题已上线（`themes/notion.css` + `notion.json`），门禁全绿

## Decision

Notion 风格以 **「一套可选主题 + 一组只约束内容型界面的排版纪律」** 引入，不改设计系统底座。

- **主题层**：两套成对交付 —— `themes/notion.css` / `notion.json`（亮，`colorScheme: "light"`）与 `themes/notion-dark.css` / `notion-dark.json`（暗，`#191919` 那套）。`registry.ts` 用 `import.meta.glob` 自动发现，`ThemeMode = string`，**零 TS 改动**；`ThemeToggle` 只补了两个图标映射。默认主题仍是 `light`。
- **排版层**：`styles/notion-prose.css`（LLM 输出与过程块）+ `styles/notion-app.css`（外壳：零渐变、工具面板、查看器、弹层、设置页），两者用 `[data-theme^="notion"]` 前缀作用域，亮暗共用；取值全部来自令牌。
- **规范层**：下面的排版/留白/交互规则只约束**内容型界面**（对话正文、笔记/知识条目、任务清单、设置内容区），不约束工作台等第三方嵌入面与图表密集页。
- **两件不做的事**：① 不改 `tokens.css` 的基线字号/圆角（那会波及全部主题）；Notion 的小圆角只写在 `notion.css` 自己的主题块里。② 不做只能靠 hover 才能发现的次要操作（参考页自己也把这条列为反模式）。

### 一、主题定位

**一句话**：让产品从「工具外壳」读成「一张能写字的纸」——外壳退到背景里，视觉预算全部给内容。

不是「把界面刷成灰色」，而是三件事的组合：**内容区最亮 + 灰阶承担层级 + 边界用面阶而非盒线**。参考页最准确的一句是：*"文档像一张稿纸放在灰桌上，边界很浅"*。

### 二、目标用户与核心场景

| 项 | 内容 |
|---|---|
| 主用户 | 长时间读写的知识工作者；把会话当工作记录回看的人；反感彩色仪表盘的开发者 |
| 次用户 | 需要给别人展示「我做了什么」的用户（导出干净的界面比花哨的更能交付） |
| 非目标 | 大屏演示/营销页、需要品牌彩度的活动页、以图表色彩编码为主的运营面板 |

核心场景（每条都是验收点）：

1. **S1 会话长文阅读**：正文 14/1.75，行宽 ≤ `--width-wide`(720)，工具调用行压到 `--text-meta` 的灰、不与结论抢通道。
2. **S2 会话沉淀为笔记**：左侧页面树（240px，缩进 `--space-3`/级）+ 右侧稿纸；页面身份用 emoji + 文本标题，功能操作不用 emoji。
3. **S3 任务/清单**：块级勾选，完成态用删除线 + 灰一档，不用彩色标签区分分类。
4. **S4 设置页低噪浏览**：分区靠留白与标题字重，不靠每块一个描边盒。
5. **不适用**：工作台 iframe、ECharts 数据面板（那里需要色彩编码，强行去色会丢信息）。

### 三、设计原则（可判定，七条）

1. **一屏一主角**：内容区（`--color-bg-primary`）最亮，外壳（侧栏/顶栏）后退。判定：任意截图里最亮的连续面必须是内容区。
2. **灰阶承担层级，色彩只承担语义与行动**：accent 只出现在主行动实底、链接、焦点环三处。判定：截图里出现第四处 accent 即违规。
3. **边界用面阶，不用盒线**：结构分区禁止 `border`（G11 门禁已拦），需要「框住」时用 `box-shadow` 画 0 模糊发丝环或面阶差。
4. **留白是结构**：内边距只取 `--space-1..8`，同层组件内边距必须同值。
5. **操作三入口**：hover / focus / touch 都要能发现次要操作。判定：Tab 能否走到该操作；触屏是否有常驻入口。
6. **emoji 只做内容身份，图标只做功能**：页面/条目图标可以是 emoji，按钮、导航、状态一律 Lucide + `aria-label`。
7. **动效短、无回弹**：120/160/240ms，走 `--ease-out`；尊重 `prefers-reduced-motion`。

## 风格分析

### 参考页给了什么、没给什么（别伪造来源）

| 维度 | 参考页原文 | 性质 |
|---|---|---|
| 画布底色 | `#F7F7F5`（暖灰） | **页面明确给出**，已采用 |
| 正文文字色 | `#37352F`（灰黑 Ink） | **页面明确给出**，已采用 |
| 分割线 | `1px` 细线，强度按内容复杂度调整 | **页面明确给出**，已采用 |
| 页面图标 | emoji，「关键功能仍要有稳定、可识别的名称」 | **页面明确给出**，采用为规范 |
| 按需操作钮 | `⠿` 拖拽 / `＋` 新建 / `⋯` 更多 | **页面明确给出**，采用为规范 |
| 圆角 / 阴影 / 字号 / 类名 / 间距 | **未给出** | 通行实现值，不能当页面原文引用 |
| 反模式 | 「只在 hover 时出现、触屏无法发现」 | **页面明确给出** |

参考页同时明确两条边界：**不机械限制灰/白/黑**（品牌场景要补品牌色）、**营销页/电商详情页不适用**。

### 为什么它看起来「高级」——机制层解释

不是配色好看，是五个结构性选择叠加：

1. **只有一个主角**：侧栏与画布几乎同色、无阴影、无品牌色 → 视觉预算全给内容区。
2. **层级靠字号/字重/灰阶三件套**：不靠彩色标签，所以一屏能塞很多结构而不显乱。
3. **边界靠面阶 + 发丝线**：屏幕上的「盒子」数量 ≈ 卡片数量，而不是「区块数量」。
4. **圆角小、阴影无**：元素看起来**贴在纸上**，不是**浮在桌面上**。这是与「卡片流 App」气质分叉的那一刀。
5. **操作按需出现**：内容永远优先于控件。

### 照抄 / 改造 / 不抄

| 项 | 结论 | 原因 |
|---|---|---|
| 暖灰中性轴 + 墨色文字 | **照抄** | `#f7f7f5` / `#37352f` 直接进令牌 |
| 去阴影、小圆角 | **照抄** | 阴影归零为 0 模糊发丝环；`--radius-sm..3xl` 覆盖为 3/4/6/6/6/10 |
| 1px 发丝分隔线 | **改造** | 只用于**内容区分**（表格行、引用、列表项）；结构分区用面阶 —— 仓库 G11 门禁禁结构 `border` |
| 正文 16px / 行高 1.5 | **不抄** | 本仓库正文唯一档是 14px/1.75（全局正文字号契约） |
| 层级级差（画布与纸差 <1%） | **不抄** | 桌面端 125%/150% 缩放会吃掉 <6% 的相对亮度差，侧栏会与纸糊成一块 |
| 二级文字 `rgba(55,53,47,.65)` | **改造** | 实色换算仅 ≈4.3:1、三级 ≈3.1:1，不达 AA → 压到实色 ≥5:1，层级交给字号/字重 |
| 链接/按钮蓝 `#2383E2` | **改造** | 白字 3.88:1、压淡蓝底更低 → `#1c6ac4`（白字 5.37:1） |
| emoji 页面图标 | **照抄（限内容身份）** | 必须同时有可见文本标题与 `aria-label` |
| hover 才出现的 `⠿＋⋯` | **改造** | 保留「按需」，补 focus 与触屏入口 |

## 关键界面元素风格规范

### 1. 画布与面阶（已落地取值）

| 令牌 | 值 | 语义 |
|---|---|---|
| `--color-bg-primary` | `#ffffff` | 纸 · 内容区 |
| `--color-bg-card` | `#fbfbfa` | 面 · 卡片（**刻意与纸接近**） |
| `--color-bg-secondary` | `#f7f7f5` | 案 · 侧栏 / 顶栏 |
| `--color-bg-app` | `#efefed` | 底 · 窗底 / 列间缝 |
| `--color-bg-tertiary` / `-hover` | `#f1f1ef` | chip / 悬停 |
| `--color-bg-active` / `-quaternary` | `#e9e9e7` | 瞬时按压 / 轨道底 |

`card` 与 `paper` 只差约 2 个明度点，这是**故意**的 —— Notion 不靠卡片承载层级，块与块之间靠留白和标题字重。因此 `card` 不得用来表达「我是一张卡」；需要框住内容时用 `--shadow-card` 的发丝环。

### 2. 文字（四级墨 · 实色 · 全部 ≥4.5:1）

| 令牌 | 值 | 用途 |
|---|---|---|
| `--color-text-primary` | `#37352f` | 正文、标题（参考页原文值） |
| `--color-text-secondary` | `#605c54` | 次级说明、工具名 |
| `--color-text-tertiary` | `#6b6862` | 三级 |
| `--color-text-faint` | `#706c65` | 占位、时间戳 |
| `--color-text-hero` | `#1f1e1b` | 空态大标题（28px/600，不用 700） |

- **本主题正文 16px / 1.65**（Notion 取值）：`--text-body` / `--leading-reading` 在 `notion.css` 里覆盖，其余主题仍是 14/1.75。界面文本 `--text-ui` 14，元信息 12。
- 一屏 ≤3 档、禁半档；标题比例见「LLM 输出适配」，不在正文档位上另开新档。
- 行宽 ≤708px（`--width-wide` 在本主题被覆盖为 Notion 的 708）。
- 标题层级靠**字重 500/600 + 字号**，不靠颜色变深。

### 3. 分割与边界

- 发丝线：`1px solid` + `--color-border-subtle`，**仅**用于表格行、引用块、列表项之间；与所压底色亮度比 ≥1.10。
- 结构分区（页头/区块/侧栏）**禁止**描边，改面阶差或 0 模糊 shadow 环。
- 控件边界保留 `--color-border`（`#8a8782`，3.58:1，WCAG 1.4.11）。

### 4. 卡片与内容块

- 无投影：`--shadow-card: 0 0 0 1px rgba(55,53,47,.06)`、`--shadow-paper` 同族更淡。
- 圆角只用 `--radius-sm`(3) / `--radius-md`(4)；内容块不得用大圆角。
- 块内 padding `--space-3`/`--space-4`，块间 `--space-4`；同一列表内 padding 必须同值。
- 浮层保留 `--shadow-pop`/`--shadow-modal` 的柔和中性投影，否则菜单与弹窗会和纸面糊在一起。

### 5. 列表行 / 侧栏

- 侧栏 240px，一级项高 32，树缩进 `--space-3`/级。
- 悬停 `--color-bg-hover-subtle`（accent 7%），持久选中 `--color-bg-selected`（accent 14%），**不用**中性灰死块表达「选中」。
- 列表行图标走 `IconTile` 实心色块；**导航栏保持单色描边**（侧栏是「位置」不是「内容」）。
- 行尾 chevron 12px、`--color-text-faint`。

### 6. 按钮与输入

- 主行动：实色 `#1c6ac4` + 白字，**每屏最多一个**。
- 次要操作：`chip-flat`/`btn-flat`（灰底、无边框、无阴影）。
- 图标按钮必须有 `aria-label`（`icon-btn`/`titlebar-btn` 类族受门禁管）。
- 输入框：`#f7f7f5` 底、小圆角、聚焦 `2px` accent outline + `offset: 1px`。

### 7. 图标与 emoji

- 功能图标：Lucide，尺寸只取 `--icon-size-xs/sm/md/lg`（10/12/14/16）。
- emoji：只用于**页面/条目身份**，且必须同时有可见文本名与 `aria-label`。
- 禁止：用 emoji 当按钮/导航/状态图标（跨平台字形不同、对比度不可控、读屏念不出语义）。

### 8. 空态与文案

- 空态 = 28px 标题 + 一句话 + 一个主行动。**禁止**「标语 + 插图 + 说明段 + 两个按钮」四件套。
- 删掉「点击这里」「这块能干什么」这类说明文字；看不懂就重排结构，不加字。

## LLM 输出与过程块适配（`styles/notion-prose.css`）

对话里模型产出的 Markdown 是这个主题的主战场，规则全部挂在 `:root[data-theme="notion"]` 下，**其余主题逐字节不变**。

| 元素 | Notion 做法 | 落地 |
|---|---|---|
| 标题 | 1.875 / 1.5 / 1.25 / 1.125 / 1 / .875 em | 同上（相对 16px = 30/24/20/18/16/14）；h6 降一档墨色 |
| 段落 | 块间距紧凑 | `margin-bottom: 0.75em`，`p + p` 不再叠加负边距 |
| 列表 | 缩进 1.5em、项距 0.2em | 同上；marker 走次级墨 |
| 引用 | 左侧 3px 墨轨、无底色、正文字色 | 轨道用 `box-shadow: inset 3px 0 0` 画（实色、零渐变）；颜色取 `--color-border-secondary`（#d3d1cb / 暗 #3d3d3d），不用 border-strong —— 后者在 16px 正文旁是两根粗黑竖线 |
| 表格 | 行分隔 + 表头淡底、无竖线 | `inset 0 -1px 0` 画行线、`th` 用 `--color-bg-secondary` |
| 行内代码 | 暖灰芯片 + 4px 圆角 + 暖红字 | `--color-code-inline-bg` + `--color-inline-code-fg`(#b03a2e) + 4px |
| 任务清单 | 16px 空心方块，勾选实心 | 16px + 2px 圆角 + inset 环；勾选实心 accent |
| 代码块 | 同一底色走到底 | `.md-codeblock` 头栏不再另起一层色 |
| 分隔线 / 链接 | 1px 线；下划线 + 2px 偏移 | `hr` 用背景画；`a` 去掉底线边框，改 `text-decoration` |
| 工具调用行 | Notion 没有对应物（取「安静的一行」气质） | 彩色实底方块 → 单色字形（`!important` 压 inline style）；悬停只给淡染；`--radius-sm` |
| 时间线轨 | 发丝线，不做竖条 | `.tool-rail::before` → `--color-border-primary`（1.5px） |
| 思考旁注 | 旁注 = 缩进 + 竖线 + 小一号字 | 标签 12px faint；正文 14/1.6；2px `--color-border-secondary` 轨（inset 阴影，实色） |
| 审批卡 | Callout：淡底、无描边、小圆角 | `--color-bg-secondary` 底 + `--radius-md`；主行动实色 accent、次行动纯文字 |

顺带修了一处基线缺陷：`.prose .md-task-box` 此前**只有勾选态有外形**，未勾选的任务项渲染出来只是一段空隙（三套主题都受影响）。现补 `inset 0 0 0 1px` 描边环（不用 border，避开 G11），`light`/`dark` 同步受益。

## 零渐变与组件统一（`styles/notion-app.css`）

Notion 的界面里没有渐变。以下每一处都在主题作用域内换成实色（其余主题不动）：

| 位置 | 原写法 | 现在 |
|---|---|---|
| 输入框底部副条 `.composer-subbar` | `linear-gradient(180deg, tertiary, quaternary)` | 实色 `--color-bg-input` |
| 代码块「还有更多」遮罩 `.code-fade` | 压向代码底色的渐变 | 隐藏 |
| 工具行进度 `.tw-msg-progress-fill` | accent 渐变 | 实色 accent |
| 骨架屏 `.m-shimmer` | 扫光渐变 + 动画 | 实色 tertiary，无动画 |
| 文件查看器顶线 `.preview-body::before` | 渐隐细线 | 隐藏 |
| 查看器加载 `.preview-loading` | 径向光晕 | 实色 `--color-bg-secondary` |
| 错误卡顶线 `.stream-error-card::before` | 渐隐细线 | 隐藏 |
| 图片预览棋盘格 `.preview-image-stage` | 四条 45° 渐变拼的 alpha 棋盘 | 实色 `--color-bg-secondary` |
| 引用 / 思考轨 | 渐变画的竖条 | `inset` 阴影（实色） |

其余组件不新增第二套画法，只做取值收敛：

- **工具结果面板**：`.tw-msg-output` → Notion 代码块（`--color-bg-code` 底、4px 圆角、12px 等宽、无描边无投影）。
- **文件查看器**：头栏 `--color-bg-primary` + 底部 1px 发丝线；代码区 `--color-bg-code` + 4px 圆角；语言/统计走 faint 12px。（独立查看器进程是另一个 app，不共享主题。）
- **消息提示 / 弹层**：`.toast` 圆角收到 `--radius-sm`，阴影走 `--shadow-pop`；审批 pill 走 `--radius-sm`；语义色只出现在状态点。
- **文件图标**：`.cat-tile` 仍走 `--color-tone-*`（身份色），不借用语义四色。

## 独立查看器（`apps/viewer`）

查看器是**独立进程**（`hermix-viewer.exe`，可被文件关联直接拉起），不共享桌面端的主题注册表与 localStorage，所以主题要自己带一套：

- `src/notion.css`（新文件，`main.tsx` 里在 `styles.css` 之后导入）：`:root[data-theme="notion"]` / `[data-theme="notion-dark"]` 两套取值表（与 `themes/notion*.css` 同源），外加外壳规则 —— 侧栏/工具条/标签栏/预览头都改面阶 + 发丝线、圆角收到 3–6px、正文 16/1.65、暗色补 tone 八档与焦点淡晕。
- `src/theme.ts`：`ThemeMode` 从三态扩成五态（`system | light | notion | dark | notion-dark`），`THEME_CYCLE` 决定工具条循环顺序，新增 `isDarkResolved()` 作为**唯一暗底判定**（此前写死 `=== "dark"`）。
- 三处消费点同步：工具条图标/标签、右键菜单 `THEME_ITEMS`、`PinWindow` 的 `isDark`（钉住浮窗之前同样写死 `=== "dark"`，Notion 暗色会拿到亮色语法高亮）。
- 零渐变：图片预览的 alpha 棋盘格同样压成实色。系统面板的环形仪表是 conic-gradient 数据图，保留（它是数据编码，不是装饰）。

## 手机端（`apps/hm`，uni-app）

手机端是独立工程（uni-app + 自己的 `--c-*` 令牌），**没有**桌面端的主题注册表，所以主题要自己接：

- `App.vue`：旧色系（墨绿 / 夜墨）已删除，只剩 `notion` / `notion-dark`。默认值直接写在 `:root, page`（= Notion 亮色），暗色只在 `:root.theme-notion-dark` 覆盖一层；`THEME_CLASS` 负责类名，`normalizeTheme()` 把老存储值 `light`/`dark` 迁到 `notion` / `notion-dark`。
- **一个必须记住的坑**：默认值同时声明在 `:root` 与 `page` 上，而 uni-app H5 把 `page` 编译成 `uni-page-body` —— 只给 `<html>` 挂主题类时，页体上的默认值会把主题值盖回去。现象很迷惑：**算出来的 `--c-accent` 是蓝的，画出来的按钮还是绿的**。所以 `paint()` 要挂三处（`documentElement` / `body` / `uni-page-body`），并在下一帧补一次（页体晚于 `onLaunch` 挂载）。
- `pages/settings/index.vue`：新增「外观」药丸行（只剩 Notion / Notion 夜），写入 `hm-theme` 后调 `__hmSetTheme`。
- `uni.scss` / `styles/shared.scss`：形状从移动端卡片流收到 Notion —— 卡片 12→6、气泡 15→8、按钮 999（全圆角）→4，提示条 18→8rpx。药丸按钮因此变成 Notion 的方角主按钮。

## 旧色系删除（2026-10-27 收尾）

产品只保留 Notion 亮/暗两套，其余色系一律删除（不是隐藏）：

| 端 | 删掉的 | 连带改动 |
|---|---|---|
| 桌面端 | `themes/light.css` + `light.json`（中性灰/墨蓝）、`themes/dark.css` + `dark.json`（夜墨） | `registry.ts` 去掉两条显式 import（glob 仍自动发现剩余主题）；`useTheme` 的默认值与「跟随系统」改指 `notion` / `notion-dark`；`index.html` 的启动引导与启动遮罩只认这两个 id（顺手清掉早已失效的 `rainforest`/`sunset`） |
| 查看器 | `[data-theme="dark"]` 令牌块与 4 条暗色类例外；菜单里的「浅色/深色」 | `theme.ts` 三态化（`system/notion/notion-dark`）；亮色取值直接落在 `styles.css` 的 `:root`（删掉与它重复的 notion 色表） |
| 手机端 | 墨绿 `--c-*` 默认套、夜墨 `:root.theme-dark` 块、`uni.scss` 的 `$hm-*` 变量组 | 默认块改成 Notion 亮色；`normalizeTheme()` 迁移旧存储值 |

`scripts/check-theme-tokens.mjs` 的对比度名单同步收敛到 `["notion", "notion-dark"]`（此前那串 `light/dark/ink-green` 会让已删除的主题继续被"测量"，制造全绿假象）。

**`tokens.css` 的 `@theme` 保留原中性灰字面量**：它现在是契约清单的出处（主题必须覆盖 119 个键），不再有任何主题选择它；改它等于动全部主题的兜底默认值，与本次「删主题」不是一件事。

## 暗色版（`themes/notion-dark.css`）

Notion 暗色取值：页面 `#191919`、侧栏 `#202020`、浮层 `#252525`、悬停 `#2c2c2c`、发丝线 `#2f2f2f`；正文 `#d4d4d4`。

- 强调蓝 `#6ba6ff`、`--color-on-accent: #0d1520`（Notion 的 `#2383E2` 配白字只有 3.9:1，提亮后深墨字过 AA）。
- 选中/悬停同样走中性灰，蓝色只给主行动、链接、焦点环。
- 排版与小圆角与亮色版**同一份**：规则住在 `notion-prose.css` / `notion-app.css` 的 `[data-theme^="notion"]` 前缀作用域，两套主题共用，令牌各写各的（119/119 全覆盖）。
- `shadow-card` 在暗色也是一枚 1px 白描环（`rgba(255,255,255,.06)`），不放投影。

## 反面示例

| # | 反模式 | 为什么坏 | 正确做法 |
|---|---|---|---|
| 1 | 灰底页面 + 白底卡片（层级反向） | 每张卡都要靠边框撑存在感 → 满屏盒子 | 纸最亮、外壳后退；卡片与纸接近，靠留白分块 |
| 2 | 窗底 `#F7F7F5` + 侧栏 `#F5F5F5` 直接搬 Notion 取值 | 相对亮度差 <1%，125% 缩放下侧栏与纸糊成一块 | 按 ≥6% 相对亮度差重排面阶（见「画布与面阶」表） |
| 3 | 每个区块加 `1px solid` + 阴影 | 盒子数量 = 区块数量，界面读成「一堆盒子」 | 面阶差 + 留白；要框住时用 0 模糊 shadow 环 |
| 4 | 8 种彩色分类标签 | 色相被读成状态（「数据库」看成「成功」）；与 tone 色块冲突 | 中性底 + 图标 + 文本；身份色只走 `--color-tone-*` |
| 5 | `⠿` / `⋯` 只在 mouseover 出现 | 触屏与键盘不可达（参考页点名的反模式） | hover + focus + touch 三入口，或常驻低对比入口 |
| 6 | emoji 当功能图标（导航项、按钮、状态点） | 平台字形不同、对比度不可控、读屏无名称 | Lucide + `aria-label`；emoji 只做内容身份 |
| 7 | 出现 11/11.5/12.5 这类半档字号 | 一屏四级字号，层级失效 | 只在既有档位（本主题正文 16）里取；一屏 ≤3 档 |
| 8 | 内容块用大圆角（此前的 24px）+ 投影 | 变成移动端卡片流，与「贴在纸上」气质相反 | 内容块 3–6px，去投影 |
| 9 | 把 accent 也一起去掉，做到「全灰」 | 主行动不可发现（参考页也明说不机械限灰白黑） | accent 保留三出口：主行动实底、链接、焦点环 |
| 10 | 用 `rgba()` 叠层做灰阶 | 桌面端重采样出带状条纹（既有纪律：全实色） | 灰阶写实色；透明度只用于 accent 淡染 |
| 11 | 直接抄 Notion 的 `.65`/`.45` 文字透明度 | 实色换算后 ≈4.3:1 / ≈3.1:1，不过 AA | 压暗到实色 ≥4.5:1，层级交给字号字重 |
| 12 | 空态写成「标语 + 插图 + 说明段 + 两个 CTA」 | 违反「先做减法」纪律，用户仍不知道能做什么 | 28px 标题 + 一句话 + 一个主行动 |
| 13 | 把链接蓝 `#2383E2` 直接当文字色 | 白底 ≈3.4:1（大按钮实底配白字也仅 ≈3.9:1） | `#1c6ac4`（实底与链接同色），hover `#175b9e` |

## 交付与验证

1. `themes/notion.css` / `notion.json`（亮）与 `themes/notion-dark.css` / `notion-dark.json`（暗）：各 119/119 契约令牌 + 阴影归零 + 字号/行高/列宽覆盖 + `--radius-*` 私有覆盖。
2. `styles/notion-prose.css`（新文件，globals.css 末尾 `@import`）：LLM 输出与过程块（正文、表格、引用、行内代码、任务清单、工具行、思考旁注、审批卡）。
3. `styles/notion-app.css`（新文件）：零渐变、工具结果面板、文件查看器、弹层/toast、设置页分区。
4. `components/settings/SettingsSection.tsx`：补 `settings-section` / `__head` / `__title` / `__hint` 四个类名钩子（主题靠它们改排版，不靠结构选择器）。
5. `components/ThemeToggle.tsx`：`ICONS` 补 `notion` / `notion-dark` → `FileText`（未登记 id 会落到 `Monitor`）。
6. `components-prose.css`：修未勾选任务方块无形（`inset` 环），所有主题同步受益。
7. **修了一处静默盲区**：`scripts/check-theme-tokens.mjs` 的 contrast 循环此前硬编码 `["light","dark","ink-green"]` —— 新主题根本不会被测量。现已改为含 `notion` / `notion-dark`，并留注释要求新增主题同步加名单。
8. 验证（本机实测）：`check-theme-tokens` → contract `notion 119/119 ✓` / `notion-dark 119/119 ✓`、orphan ✓、bare-color ✓、`[contrast] 全部配对达 WCAG AA ✓`；`check-design-guard` → `DesignGuard: clean`；`check-design-primitives` → exit 0；`tsc --noEmit` → 0；`impeccable detect` → 0。
9. 浏览器实测（系统 Edge，暗色主题下取值）：`.tw-msg-output` `#202020` / 4px / 无投影；`.toast` `#252525` / 3px / pop 阴影；`.settings-section__title` 18px/600；`.composer-subbar` 无渐变 + `#202020`；`blockquote` 无渐变 + `inset 3px #3d3d3d`；`.preview-pane .code-viewport` `#202020` 无渐变；`.preview-head` `#191919` + 1px 发丝线。
10. 截图（临时，不入库）：`tmp-notion-app.png`、`tmp-notion-dark.png`、`tmp-light-app.png`（桌面端）、`tmp-viewer-notion.png`、`tmp-viewer-notion-dark.png`（查看器）。
11. 查看器：`apps/viewer/src/notion.css`（新）+ `main.tsx` 导入 + `theme.ts` 五态 + `App.tsx`（标签/图标/菜单/isDark）+ `contextMenuModel.tsx`（菜单项）+ `PinWindow.tsx`（钉住浮窗的 isDark）。`apps/viewer` 的 `tsc --noEmit` → 0；`check-preview-layer` → OK（65 类名 / 47 token 两侧齐备）。
12. 查看器实测（浏览器 + 真实 styles.css/notion.css）：亮色 sidebar `#f7f7f5`、tabbar `#efefed`、accent `#1c6ac4`、radius 4、正文 16；暗色 sidebar `#202020`、tabbar `#151515`、accent `#6ba6ff`。
13. 手机端：`apps/hm/App.vue`（两态 + 默认 Notion 亮色 + 三处挂类 + 旧值迁移）、`pages/settings/index.vue`（外观药丸，两项）、`uni.scss` / `styles/shared.scss`（删 `$hm-*`，形状收口）。`vue-tsc --noEmit` → 0；`UNI_PLATFORM=h5` 构建 exit 0，产物中旧色（`#2f6b4f` / `#6fae94`）出现 0 次；实测亮 `--c-paper #ffffff` / `--c-accent #1c6ac4`，暗 `#191919` / `#6ba6ff`，外观药丸只剩 `Notion/Notion 夜`。截图：`tmp-hm-notion.png`、`tmp-hm-notion-dark.png`（390×844 @2x）。
14. 旧色系删除：桌面端 `themes/light.*` / `themes/dark.*`、查看器 `[data-theme="dark"]` 令牌块、手机端墨绿/夜墨两套。删除后三端 `tsc`/`vue-tsc` 均 0，`check-theme-tokens` 只剩 `notion 119/119` 与 `notion-dark 119/119`，`DesignGuard: clean`。

## 风险与失效条件（对抗性自检）

- **结构性覆盖是主题私有的**：`notion.css` 里改 `--radius-*` 只影响该主题（Tailwind v4 工具类引用 `var(--radius-*)`）。若有人把同样的值搬进 `tokens.css` 基线，会波及 `light`/`dark` —— 那是另一件事，需要逐组件验收。
- **暖灰在低色域屏 / 夜间偏黄**：暖轴 Δ≤3 点时观感差异明显，必须靠 contrast 门禁兜底；用户可以一键退回 `light`。
- **无阴影后浮层层级削弱**：`--shadow-pop`/`--shadow-modal` 已保留；把这两档也归零会让菜单看起来像贴在纸上的文字。
- **emoji 进数据层**：会话标题前置 emoji 有字形宽度差异与清洗成本（长度限制、非法字符剥离），要在写入侧处理而非渲染侧。
- **两套主题共用一份排版层**：作用域是 `[data-theme^="notion"]`，所以新增第三个 notion 系主题会自动继承排版；代价是这一层不能为单个主题写例外（要例外就改令牌，别改规则）。
- **零渐变由主题承担**：渐变散在 6 份基线 CSS 里（副条、遮罩、进度、骨架、查看器、棋盘格），Notion 主题逐个压成实色。新增渐变装饰时，这里要同步补一条遮蔽规则，否则主题里会冒出渐变。
- **两处主题选择器互不同步**：桌面端存 localStorage（`STORAGE_KEYS.theme`），查看器存 `hermix.viewer.theme`，两个进程各选各的 —— 主程序切到 Notion，独立查看器不会自动跟随。要联动得落一个共享文件（如 `hermix_home/ui-theme.json`）供两侧读取，那是独立改动。

## Alternatives considered

- **A. 全面照抄 Notion 数值**（16px 正文 / 层级差 <1% / `#2383E2` 原色）：否决。正文字号是全局契约；级差过不了桌面端缩放；`#2383E2` 两处门禁配对都不达 AA。
- **B. 只做主题、不写规范**：否决。没有规范的主题会立刻漂移；仓库历史上 409 处 `text-[11px]` 任意字号就是「只有令牌没有用法规则」的产物。
- **C. 只写规范、不做主题**：否决。规范无法验收，容易变成又一份没人读的设计文档。
- **D. 另起一套「Notion 风」前端/组件库**：否决。第二套设计系统 = 长期双倍维护。
- **E. 把 Notion 蓝 `#2383E2` 原样作为 accent 与链接色**：否决。白字实底 ≈3.9:1、链接压 chip 底更低，必须压暗一档。
- **F. 把文字透明度（`.65`/`.45`）照搬**：否决。实色换算后不达 AA，且仓库纪律是全实色。
- **G. 只改色彩、不动圆角**（本轮之前的方案）：否决。Notion 的气质有一半在小圆角与无阴影上，只改色彩落地观感只到七成；圆角可以按主题覆盖，代价可控。
- **H. 顺带把 `tokens.css` 基线圆角也下调**：否决。会让 `light`/`dark` 一起变，属于全局结构变更，与「加一个主题」不是同一件事。

## Consequences

- **新增维护面**：119 个契约令牌的取值表，`tokens.css` 以后新增色彩令牌必须补 `notion.css`（契约校验会失败，这部分安全）。
- **主题机制多了一个用法**：结构性令牌可按主题覆盖（`--radius-*`）。这扩大了一点「主题」的语义，所以写进了 AGENTS.md，避免下次有人直接改基线。
- **门禁盲区已收口**：contrast 名单现在含 `notion`，以后新增主题必须同步加进去，否则又是一次「全绿但没测」。
- **灰阶层级变浅**：为过 AA，三级/四级墨与二级几乎同值，层级完全交给字号与字重 —— 与仓库既有取舍一致。
- **主题的语义变大了**：这套主题不只有颜色，还覆盖了字号阶（正文 16）、行高、列宽、圆角与 LLM 输出的排版。好处是「像 Notion」靠主题就能达成大半；代价是主题换色时不能只想着色板，`--text-body` 一改，全 app 的正文都会跟着变。新增主题若要动这些，抄 `notion.css` 的写法，别动 `tokens.css` 基线。
- **留下的资产**：一套可判定的内容型界面规范（七条原则 + 13 条反例）与一张「照抄/改造/不抄」对照表，后续任何主题都可复用。
