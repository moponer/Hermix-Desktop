# Decision Note: Hermix 前端全量组件清单（唯一账本）

Status: proposed — 2026-10-16 全库盘点结果（143 个 tsx），本文件是组件账本的唯一入口，新组件必须先登记再实现

## Decision

以**五层模型**登记全部组件。账本的作用是让"同一个职责有几套实现"变成可度量的数字，后续所有优化以本文件的统计为基线（改完再盘一次，数字必须下降）。

| 层 | 职责 | 数量 |
|---|---|---|
| L0 原语 | 无业务语义的最小控件 | 14 个（另有 **12 个缺失**） |
| L1 组合 | 由原语拼成、可跨页复用 | 约 30 个 |
| L2 页面 | 路由级视图与设置 Tab | 12 个 Tab + 5 个视图 + 1 个浮窗页 |
| L3 覆盖 | 浮层/弹窗/抽屉/提示 | 约 22 个（**6 套弹窗并存**） |
| L4 视觉单元 | CSS 类承载的小单元（气泡/过程行/chip/工具卡） | 约 40 组 |

### 一、L0 原语（`src/components/ui/`，index.ts 导出 14 个）

| 组件 | 状态覆盖 | 问题 |
|---|---|---|
| `Button` | rest/hover/active(scale .97-.98)/focus-visible(ring2+offset2)/disabled(两套：填色档 vs 透明档)/loading(aria-busy)/block；10 variant × 5 size × 2 shape | `danger` hover 用 `brightness-110`（唯一非 token hover）；无 selected/pressed 态；`icon` 槽无尺寸约束 |
| `IconButton` | 继承 Button，`label` 约定必填 | 类型层不强制；与 Button 同文件 |
| `Input` | default/focus(仅描边)/error(aria-invalid+role=alert)/disabled/hint/required | **无 focus ring**（与 `.input:focus` 的 3px 晕不一致）；4 档尺寸与 CSS `.input` 不互通 |
| `TextArea` | 同上 | **唯一没有 `size` 档**的字段，写死 `px-3 py-2` |
| `SelectField` | + labelHidden(sr-only) | 原生 `<select>`，箭头色跨主题不可控 |
| `Modal` | open/Esc/焦点陷阱/焦点归还 | **无入场动画、无 backdrop-blur**；`aria-labelledby` 未设；`z-[60]` 与 PopoverShell 同层；width 是数字 props |
| `ConfirmDialog` | — | 与 ToolTabs 私有版并存 |
| `Card` | 6 surface × 3 border × 4 padding × 2 lift | 默认 `border="solid"` 是全站"满屏盒子"的源头 |
| `Badge` / `EmptyState` / `SegmentedControl` / `SettingsRow` / `SettingsGroup` / `Switch` / `Spinner` | — | `EmptyState` **仅 9 处使用、全部在 settings/**；5 个页面级视图 0 处使用 |

**缺失原语（12 个，直接导致下方"6 套弹窗/4 套浮层"的乱象）**：Tooltip、Popover、Menu、Tabs、Checkbox、Radio、Slider、**Progress**、Skeleton、Avatar、Divider、Alert、Breadcrumb。

### 二、对话链路（L1+L4）

**容器**：`ChatView`(53KB，4 个早退分支各自复制 `<Composer>` 的 30+ props)、`ModelStatusBar`(`h-10` 硬编码、`bg-green-400/red-400` 绕过令牌、工作台会话整行不渲染→对话区比其他会话高 40px)、`MessageList`(`space-y-5` 一刀切；`content-visibility:auto` 会卸载折叠状态)、`SidebarRevealRow`(与 ModelStatusBar 同款按钮重复实现)。

**气泡**：`MessageBubble` 分发器（113-116 连续两个 `return null` 死分支；已决议的审批/已答复的询问整条消失不留痕）、`TextBubble`(用户气泡 `max-width:80%` 被下一行改 100% 成死规则；助手续条 `pl-[30px]`)、`StreamingBubble`(同 30px)、`ToolCallBubble`(与 StreamingBubble 共用)、`AutomationBubble`(全宽卡+边框，比正文层级重)、`ClarifyBubble`(已答复 pill 是死代码)、系统消息行(200 字截断无 title/无展开)、`MessageActions`(正文为空时整行 `return null`，而 CSS 常驻 `min-height:24px` → 间距塌陷；hover-only 触屏不可达；赞/踩只写 localStorage 不上报)。

**过程**：`ThoughtStreamCard`(role=status/aria-live) 与 `.tl-reasoning-wrap` 是**两套"正在思考"表示**；`ProcessTimeline`(折叠态 `userOverride` 仍是本地 `useState`，未换成 `chat/activitySummary.ts:174` 的 `useDisclosure` → 虚拟化回收后展开态丢失；`stepGroups` memo 依赖缺 `streamComplete`)；`.process-line-*`(状态靠图标形状 ✓/!/⟳/⊘，chevron 仅 hover 显形)。

**输入区**：附件行（图 56px/文档 `max-w-[220px]`，删除钮命中区约 14px 且压住被截断的文件名）、消息队列行（`max-h-32`）、`InlineExpertPanel`、`AppMentionInput`(透明 textarea 叠高亮层；`COMPOSER_MAX_HEIGHT=200`、`min-h-[72px]`；@ 面板 320×260)、工具栏 chip（**max-width 三种：140/160/160，模型 chip 是 `max-w-narrow`** 且不受 compact 影响）、compact 折叠（`ResizeObserver` 阈值 700，落到 `data-compact` 属性但**没有任何 CSS 消费它**——属性悬空）、发送 32px vs 停止 28px（**互斥切换会跳版**，停止无 disabled/aria-label）、拖拽遮罩。

**审批**：`ApprovalDrawer`（一次一项 + `1/N` 计数，5 个选项，键盘 1–5/Esc；**批量分支 risk 恒 caution**）、内联审批单元（2–3 项决策面，批量列表在 `components-tool-card.css:852` 与抽屉各写一遍）。

**内容渲染**：代码块（复制钮 hover-only；头两行+统计行在窄容器占三行）、prose（`blockquote` 用 `--color-bg-tertiary` 当边框色，跨主题对比度不可控）、`file-search.css`(header 与 row **两份 grid-template 重复**)、图片灯箱（入口分散，180px 缩略与大图断层）。

### 三、导航与外壳

| 单元 | 问题 |
|---|---|
| `TitleBar` | 5 个窗口按钮用了 **md/lg/md/lg 三种图标档**（全站唯一"同组按钮不同档"）；品牌/mascot 三份资产各自引用；截图入口有 3 处 |
| `TitleBarConnectionIndicator` | `Loader2 size={8}` 字面值，全站最小图标 |
| `Sidebar` | 导航只有 4 项（**"专家""统计"无入口**）；`⌘K` 硬编码（Windows 应显示 Ctrl+K）；状态点 `bg-green-400/red-400` 裸色；`is-removing` 与 `is-deleting` 两个动画态；`Pin size={10}` 字面值；版本号与 user card 争夺底部注意力 |

### 四、页面层（L2）

**settings/ 24 个文件**：12 个 Tab（Advanced/Appearance/Archive/Backend/Devices/Diagnostics/General/Memory/ModelList/Runtimes/Security/Voice）+ 12 个辅助件。
- `SettingsGroup/SettingsRow` 采用率 **3/12（25%）**，9 个 Tab 手写 div 容器
- 仅 8 个文件引用 `../ui` 原语，其余 16 个零复用
- `ImChannelsCard.tsx` **60.4KB**——只是设置页里的一张卡片，比 config/ 目录里 3 个文件的总和还大
- `FileAssocTab` 未拆文件，内联在 `SettingsShell.tsx:456`

**5 个页面级视图**：

| 视图 | 页头 | 内容宽度 | 网格 | 空态 | 破坏性确认 |
|---|---|---|---|---|---|
| AgentsView | `<h2>`+text-title | max-w-2xl | 1/md:2/lg:3 gap-4 | 手搓 | **无** |
| AutomationView | 3×`<h2>`，其一仍 `uppercase tracking-wider` | max-w-4xl | **单页 3 种** | 手搓纯文本 | 1 处 |
| AnalyticsView | `<h2>`+字面 `text-lg` | 无（仅 200px 截断） | 3 种 | 手搓 | 0 |
| CustomAppsView | `<h1>` | 无 | — | 三元串 | 1 处 |
| VaultPanel | `<h2>`+字面 `text-lg` | 弹窗 `max-w-md rounded-3xl` | 列表 | 三元串 | **11 处** |

**config/ 4 文件 78KB**：`ToolTabs.tsx` **54.1KB God Object**（4 个 Tab + 12 个内部件 + 2 个解析器 + 3 张常量表，含**私有 ConfirmDialog**）、`McpCatalogGrid`(16.7KB，13 处确认)、`MgmtTag`(1.56KB，规范本身正确但同一反模式在 AutomationView:276 仍存活)、`ProviderIcon`(**27 处硬编码 hex**)。

**工作台**：`FloatingChat` 球 48px、浮窗 440×560（min 320×320）、边距 16、停靠宽 clamp(320, w, innerWidth-320)；**3 个记忆 key 两套坐标系**（`.orbPos` 比例 / `.winBox.v2` 像素 / `.docked` 布尔）。

### 五、图标与资产（硬数字）

- **lucide-react**：93 个文件引入；抽样 28 个文件得到 201 处引用、124 个 distinct，**全量 distinct 估计 180–220**；单次最大引入 `iconResolver.tsx` 74 个
- **尺寸**：`size={N}` 字面值 67 处（跨 32 文件），落进 `icon-sm/md/lg` 三档的仅 **≈3 处（4.7%）**，**95% 是野值**；最常见 `size={10}` 有 **31 处（46%）**、其次 9（8 处）。对照：`icon-*` 令牌使用 ≈390 处/83 文件
- **自绘 SVG 硬编码 hex**：`FileTypeIcon` 97 处、`ProviderIcon` 27 处、`agents/types.ts` 8 处、`ScreenshotEditor` 2 处 ≈ **134 处**
- **品牌资产重复**：`src/assets/` 6 个 png 实为 **4 份独立**（2 组字节相同）；`public/` 28 个 png 有 **5 组重复**（favicon-16≡hermix-icon-16、favicon-32≡hermix-icon-32≡mascot-avatar-32、favicon-48≡hermix-icon-48、hermix-icon-24≡mascot-avatar-24、hermix-icon-64≡mascot-avatar-64）

### 六、重复与不一致总账（优化基线）

| 维度 | 现状 | 目标 |
|---|---|---|
| 弹窗实现 | **6 套**（ui/Modal、ToolTabs 私有、各页手写） | 1 套 |
| 浮层/弹出 | **4 套** | 1 套 |
| 页头形态 | **4 种**（h1 / h2+title / h2+text-lg / h2+uppercase） | 1 种 PageHeader |
| 内容宽度 | **4 种**（2xl / 4xl / 200px 截断 / 无约束） | 2 档令牌 |
| 卡片网格 | **5 种**（AutomationView 单页就有 3 种） | 1 种 auto-fill |
| 空态写法 | **≥8 种变体**（共享组件仅 9 处且全在 settings） | 1 种 |
| 破坏性动作确认 | **≥4 种**（ConfirmDialog×2 / `confirming` state×34 处 / 无确认） | 1 种 |
| 起排线（缩进） | **5 个值**：30px(续条×2) / 32px(头像列、Clarify) / 21px(过程行) / 18px(思考) | 1 个令牌 |
| 按钮体系 | **2 套**（ui/Button 与 `.btn` CSS 类） | 1 套 |
| 图标尺寸 | **双轨**（390 处令牌 vs 67 处野值，野值 95% 不合规） | 单轨三档 |

### 七、最该修的六条（按影响面排序）

1. **折叠态被虚拟化吞掉**：`ToolCallBubble.tsx:141` 未迁 `useDisclosure` + `stepGroups` 依赖缺失（用户展开的过程一滚就没）
2. **起排线五套值** → 统一为一个令牌
3. **停止/发送互斥且不同尺寸**（32/28px）会跳版 → 同位同尺寸 + 补 aria-label
4. **hover-only 操作**（MessageActions、复制钮）触屏不可达且空正文间距塌陷 → focus-within 兜底
5. **审批两套决策面**（抽屉 5 项 vs 内联 2–3 项）+ 批量恒显示"中风险"
6. **God Object**：`ToolTabs`(54KB)、`ChatView`(53KB)、`ImChannelsCard`(60KB) 拆分

## Alternatives considered

- **只盘"有名字的组件"**：否——L4 视觉单元（`.tl-*`、`.process-line-*`、`.approval-pill-*`、chip）才是观感的实际载体，不进账本等于没盘
- **用 Storybook 自动抽取**：否——仓库没有 Storybook，引入是重资产；静态盘点 + 本文件账本足够，且能与 `check-theme-tokens.mjs` 同风格做脚本校验
- **边盘边改**：否——用户要求先有完整账本再谈优化；改了基线数字就失真

## Consequences

- 本文件是"活账本"：每轮优化结束必须重盘一次并更新数字，数字不下降就是没做完
- 盘点口径为 ripgrep 逐文件计数（含少量注释命中），**数量级准确、个位数可能 ±3**——引用时按量级看，不按精确值验收
- 后续新建组件若不在本账本登记，等于制造新的不一致；`scripts/check-theme-tokens.mjs` 未来可扩一条"新 tsx 是否已登记"的软校验
