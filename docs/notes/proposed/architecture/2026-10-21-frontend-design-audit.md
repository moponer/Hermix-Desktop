# Decision Note: 前端设计全面审计（2026-10-21）

Status: proposed — 对 256 个 tsx / 19 个 css / 3 套主题做全量设计审计：既有宪法（[2026-10-16-unified-design-system.md](../feature/2026-10-16-unified-design-system.md)）的执行度实测 + 5 条宪法未覆盖的新约束

## Decision

本次审计**不取代** [2026-10-16-unified-design-system.md](../feature/2026-10-16-unified-design-system.md)（下称「宪法」）与 [2026-10-16-component-inventory.md](../feature/2026-10-16-component-inventory.md)（下称「清单」）。那两篇定义「该长什么样」，本文只回答两件事：

1. **宪法与清单在真实代码里的执行度是多少**（实测，不是印象）；
2. **宪法漏掉了哪几条约束** —— 本次审计发现 5 类问题，宪法的令牌纪律、组件分层、图标单轨、响应式分档全部无法拦截它们。

新增 5 条约束写入宪法（见 §7），并把其中 4 条做成 CI 门禁（见 §8）。

### 审计方法与可信度分级

| 级别 | 含义 | 本文的用法 |
|---|---|---|
| **实测** | 脚本跑出来的数字 | 对比度（自写 WCAG 计算脚本，解析 3 套主题令牌）、门禁退出码、grep 计数、文件覆盖率 |
| **源码核验** | 读过源码并交叉验证过 | 代码缺陷、令牌违规、组件复用统计、文案不一致 |
| **未核验** | 只做了静态推断 | 图标视觉重量对比、未跑 `impeccable detect`、未跑 `tsc --noEmit`、未实机渲染 |

已实跑的检查：`check-theme-tokens.mjs`（exit 0）、`check-design-guard.mjs`（exit 0，从 `apps/desktop-tauri/` 下跑）、`check-preview-layer.mjs`（exit 0）。
**未跑**：`npx impeccable@4.1.0 detect`、`tsc --noEmit`、实机渲染、屏幕阅读器走查。

### §1 执行度记分卡

| 宪法条款 | 实测执行度 | 证据 |
|---|---|---|
| 令牌契约（三套主题齐备） | 🟢 **完全达标** | `dark 103/103`、`ink-green 103/103`，孤儿令牌 0，裸色 0 |
| 字号 7 档（`text-[Npx]` 归零） | 🟢 **完全达标** | `type-debt` = 0；CSS 层仅 4 处（`base.css` 1 + `components-helpers.css` 2 + `inline-approval-card.css` 1） |
| Tailwind 原生色阶重定向 | 🟢 **达标** | 在用 21 槽全部重定向到语义令牌 |
| 禁 `dark:` 变体 / 禁字面色 / 禁 `window.confirm` | 🟢 **达标** | DesignGuard G1/G2/G5 均 0 命中 |
| 共享预览层归属 | 🟢 **达标** | preview-layer 5 类名全有规则、7 令牌两侧齐备 |
| **组件层零品牌色** | 🟡 部分 | 4 处 `bg-black/30` / `bg-black/40` 是**真裸黑**（`black` 不在重定向清单）；`ProviderIcon` 24 个品牌 hex 未登记豁免 |
| **弹窗收敛到 1 套** | 🔴 **未执行** | 12 个文件各自实现弹窗；`ui/Modal` 之外**零处**焦点陷阱 |
| **图标单轨（`size={N}` 野值归零）** | 🟡 部分 | 21 处 `size={N}` 中 4 处离档（18/20/26/20）；8 个文件用裸 Tailwind 尺寸绕过 `icon-*` |
| **每个概念一个图标** | 🔴 **未执行** | 「设置」4 个字形、「错误」3 个、「复制成功」2 个、「编辑」2 个 |
| **对比度 ≥4.5:1** | 🔴 **未执行且宪法无检查** | 3 套主题共 **8 处 FAIL**（详见 §4.3） |
| **空态唯一组件 `ui/EmptyState`** | 🟡 部分 | 4 套并存、15+ 调用点；`EmptyState` 默认 `variant="dashed"`（描边）与用户偏好相悖 |
| **控件高度档（24/30/32/40/44）** | 🔴 **未执行** | 同屏实测 **6 种**表单控件高度（28/30/32/36/37/40/44） |
| **状态模型 8 态齐备** | 🟡 部分 | 焦点态 **5 套**并存；`ToggleSwitch` 焦点环用 `success` 绿而非 accent |
| **可访问名（`IconButton` 强制 label）** | 🔴 **未执行** | `ui/IconButton` **零调用**；415 个裸 `<button>` |
| **reduced-motion 降级** | 🟢 **达标** | `base.css:96` 全局 `animation-duration: 0.01ms !important` 覆盖所有 Tailwind `animate-*` |

**结论**：令牌层（颜色/字号/色阶）**已基本收口**；组件层（原语复用/弹窗/图标/文案/对比度）**基本未开始**。
宪法目前是一份**未被机械执行的设计意图**，不是约束。

### §2 整体布局结构

#### 2.1 三列骨架 — 合规且有深度

`.app-body`（`components-layout.css:33-90`）用 `grid-template-columns: var(--col-left) minmax(0,1fr) var(--col-right)`，
`--col-left/--col-right` 唯一定义在 `.app-shell`（:96-99），折叠时归零 track 而不是移走元素——这是正确的做法，
源码注释也写明了「本地再写一次会让壳上的开合覆盖遮蔽掉，侧栏折叠随之失效」。
`minmax(0, 1fr)` + `min-height: 0` 的组合在流式输出时不会撑破高度。

**问题 1（真实缺陷，中）**：`TitleBar`（`TitleBar.tsx:79-164`）与 `.app-body` 共用 `--ab-left-w/--ab-right-w`，
但 **`.app-body` 消费的是 `--col-left/--col-right`（壳上定义），标题栏消费的是 `--tb-left-w/--tb-right-w`**。
折叠时 `.app-shell[data-left-open="false"]` 同时覆盖了 `--col-left` 和 `--tb-left-w`（:102-105），
两处靠**人工同步**保持一致。**规范建议**：把 `--tb-left-w/--tb-right-w` 改为 `--col-left/--col-right` 的别名声明，
或让 `.titlebar-zone--left/right` 直接消费 `--col-*`，消除第二组变量。

**问题 2（中）**：`.app-body` 注释（:42-49）明确说「最小可读宽度改由下方 `@container` 规则在窄视口下接管」，
但 **`components-layout.css` 里不存在任何 `@container` 规则**（实测 grep：0 命中）。
宪法 §十 也写「主列分档从 JS 渐进迁移到 `@container`」——迁移**尚未开始**，注释描述的是计划而非现状。
**规范建议**：要么现在补 `@container` 规则，要么把注释改成「当前由 `App.tsx` 的 `computeMainMinWidth` 分档承担，
`@container` 迁移待办」，避免下一位维护者按注释去找不存在的规则。

#### 2.2 响应式断点 — 事实上的「四档等效视口」

宪法 §十 定义 ≥1400 / 1140–1399 / 900–1139 / <900 四档。代码里这套分档**存在，但活在 JS 里**：

- `App.tsx:122-134` `computeMainMinWidth` / `computeLeftMin`（按 `innerWidth / zoom` 等效视口分档）
- `App.tsx:219-231` resize + `UI_SCALE_EVENT` 双监听
- `App.tsx:96-113` 三列共存临界视口推算

**问题 3（中）**：这套分档只管**外壳三列**。**页面内部**几乎全是固定栅格：
`grid-cols-4`（`AgentForm.tsx:77`）、`grid-cols-3`（`AppearanceTab.tsx:29`）、
`grid-cols-2`（`GeneralTab.tsx:47`、`VoiceTab.tsx:105`、`AnalyticsView.tsx:88,226,357` 等 8 处）——
TSX 里 `md:/lg:/xl:` 响应式前缀只有 **20 处**，且 12 处是 `grid-cols-1 md:grid-cols-2` 这一种形态。
在 780px 窗口 + 150% 缩放（等效 520px）下，`AgentForm` 的 4 栅格会挤成 130px 一列。
**规范建议**：把「一屏内字号档位 ≤3」的同款约束升级为**「一屏内栅格档位 ≤2，且任何固定 `grid-cols-N`（N≥2）必须配一个 `md:`/`lg:` 降档」**，
并写进 `check-design-guard.mjs` 作为 grep 门禁。

**问题 4（低）**：`components-prose.css:552-557` 有两条 `@media (min-width: 1024px / 1280px)`，
但它们改的 `.footer-block { max-width: 688px → 768px → 888px }` **永远不生效**——
`.footer-block` 的祖先是 `.msg-stream-list.max-w-reading`（`MessageList.tsx:354`，`--width-reading = min(780px, 100%)`），
780px 的上限先把 888px 夹掉了。**规范建议**：删掉这两条 media query，把 `.footer-block` 的宽度改为
`calc(100% - 32px)` 跟随父容器；若确实要随视口放宽，正确的做法是让 `--width-reading` 在宽屏下分档，
而不是在子元素上写一个够不着的 max-width。

#### 2.3 间距与对齐

- 间距基本走 Tailwind 默认 4px 档（= `--space-*` 8 档），**48 处**任意值（`[Npx]`）散布在 29 个文件，
  最高频是 `h-[420px]`×5、`max-w-[160px]`×4——**不是间距越界，是"该用 `--width-*` 档却写了字面值"**。
- `min-w-0` 用了 69 处（TSX）/ 0 处（CSS）：flex 子项收缩在 TSX 层处理得比 CSS 层好，这是对的。
- `truncate` 108 处、`overflow-x-auto` 仅 4 处 → **长文本的截断策略远多于滚动策略**。

#### 2.4 页面层级与导航逻辑

- 主导航只有 4 项（`Sidebar.tsx:69-74`：应用/工作台/自动化/凭据库），「搜索」和「对话」被有意从导航移除
  改为动作（:64-68 注释说明理由）——**这是好的决策**：动作不占位置。
- 侧栏折叠后展开入口沉到 `ChatView` 的 `ModelStatusBar`（`ChatView.tsx:87,105-127`），与「侧栏折叠时不能把入口一起折没」
  的约束一致（:84-86 注释）。
- 设置从整页改成浮层（`App.tsx:146-170`），关闭即回原视图——**降低了导航层级成本，方向正确**。
- **问题 5（低）**：`aria-current` 只有 3 处，而导航有 4 项 + 多个二级页签。
  `PreviewBar.tsx:189-196` 的侧栏标签是 `role="tab"` 的普通 `<div>` + `onClick`：
  **不可聚焦、无 `tabIndex`、无键盘处理** → 键盘用户完全无法切换右栏标签。
  **规范建议**：`role="tab"` 容器必须配 `tablist` + `tabpanel` 关系，标签元素改为 `<button>`（自带可聚焦与 Enter/Space）。

### §3 组件体系

#### 3.1 `ui/` 原语的真实采用率（实测）

| 原语 | 出现文件数 | 判定 |
|---|---|---|
| `Button` | 108 | ✅ 广泛 |
| `Card` | 85 | 🟡 85 个文件「出现」≠ 85 个文件 import（多为 `Card` 子串误命中） |
| `Switch` | 35 | 🟡 |
| `Input` | 49 | 🔴 **`ui/Input` 零 import**，49 是 `Input` 子串（含 `AppMentionInput` / `useInput` 等） |
| `Modal` | 20 | 🟡 `ui/Modal` 仅 9 文件 |
| `Skeleton` | 20 | ✅ |
| `Badge` | 13 | 🔴 `ui/Badge` 零 import |
| `Spinner` | 11 | 🔴 `ui/Spinner` **仅 1 处**使用，25+ 处手写 `Loader2 animate-spin` |
| `EmptyState` | 6 | 🟡 |
| `PageHeader` | 6 | 🟡 |
| `SegmentedControl` | 5 | 🟡 |
| `SettingsRow` | 5 | 🟡 |
| **`IconButton`** | **1（= 0 次实际使用）** | 🔴 **零调用** |

**问题 6（高）**：`ui/IconButton`（`Button.tsx:159-167`）是**唯一强制要求可访问名**的原语
（`label: string` 是必填 prop），它的存在正是为了「图标按钮必须有 aria-label」这条规则。
但它**零调用**，而仓库里有 **415 个裸 `<button>`**、82 个裸 `<input>`、10 个 `<select>`、16 个 `<textarea>`。
DesignGuard 的 G3 规则（图标按钮 aria-label 检查）在 `scripts/check-design-guard.mjs:16-20` 被移除，
理由是「regex 启发式误报率不可接受，需要 TS AST」——
**结果不是「等 AST 工具」，而是「这条规则从此无人执行」，而 `AGENTS.md` 至今仍宣称它被强制。**
**规范建议**：
(a) 把 `ui/IconButton` 定为**图标按钮的唯一合法来源**，裸 `<button className="icon-btn">` 全部迁移；
(b) 在 `check-design-guard.mjs` 恢复 G3，但**限定检查范围为 `className` 含 `icon-btn` / `titlebar-btn` / `icon-button` 的按钮**
（这类可从属性串可靠判定，且覆盖面正是高风险的图标按钮），避免重蹈 270 条误报。

#### 3.2 重复实现清单（应抽共享件）

| 重复物 | 套数 | 位置 |
|---|---|---|
| 弹窗壳 | **12 个文件各自实现** | `ui/Modal` / `AutomationView`×3 / `SkillCard` / `AppDetailModal` / `ModelAddDialog` / `McpCatalogGrid`×2 / `CustomAppsView` / `SessionPickerDialog` / `EditProfileDialog` / `TestDialog` / `UpdatePanel` / `PreviewPanel` / `FloatingChat` / `PopoverShell` |
| 开关 | 4 | `ui/Switch` / `ToggleSwitch` / `BackendSettingsTab` 手写 `role="switch"` / `SecurityTab` 手写 `role="switch"` |
| 空态 | 4 | `ui/EmptyState` / `toolTabsShared.EmptyHint` / 裸 `<p>` / 裸 `<div>` |
| 药丸标签 | 8+ | `ui/Badge`（零使用）/ `MgmtTag` / 各页手写 `rounded-full px-1.5 py-0.5` |
| 分段控件 | 5 | `ui/SegmentedControl` / `AgentsView` / `AutomationView` / `McpCatalogGrid` / `SkillsTab`（chip 版） |
| 设置行 | 6 | `ui/SettingsRow` / 5 个 Tab 各自写行布局 |
| 卡片容器 | 16+ | `ui/Card`（零使用）/ 15+ 处手写 `rounded-[var(--radius-lg)] bg-bg-card p-4 shadow-[var(--shadow-paper)]` |
| 按钮 | 6 | `ui/Button` / `ui/IconButton`（零使用）/ `ModelListTab.IconBtn` / `ImChannelsCard.ActionBtn` / `.modal-btn` / `.btn` |

**规范建议**（与清单一致，此处补**执行顺序**）：
先做 **`ui/Modal` 收编**（1 → 1 的收益最大：一次性补齐 12 处弹窗的焦点陷阱/Esc/焦点归还/aria-modal），
再做 `ui/Input`（补 `aria-invalid` + `role="alert"` + 错误文案），
最后 `ui/IconButton` / `ui/Card` / `ui/Badge`（纯样式重复，无障碍收益低）。

#### 3.3 状态与交互反馈

- **焦点态 5 套**：`ui/Input` 改边框色（无 ring）、裸 input 普遍 `ring-2 ring-accent/40`、
  `ui/Button` `ring-2 + ring-offset-2`、`ui/Switch` `ring-2 ring-accent/40`、
  **`ToggleSwitch` `ring-2 ring-success/40`（焦点环是成功绿）**、`ui/SettingsRow` `ring-1 ring-inset`。
  **规范建议**：统一「2px accent ring + 2px offset」（宪法 §七.2 已定），`ToggleSwitch` 的 success 绿焦点环是违规项。
- **`ui/Button` 的焦点环 offset 固定为 `--color-bg-card`**（`Button.tsx:97`）：
  按钮若坐在 `--color-bg-primary`（白）上，offset 圈会出现一道浅灰接缝。**规范建议**：offset 颜色取当前表面色，不写死。
- **`ui/EmptyState` 默认 `variant="dashed"`**（`EmptyState.tsx:46`）用 `border border-dashed`——
  与用户明确的「UI 一律不用描边，层次靠底色差+阴影+间距」相悖。
  且 5 个调用点里有 3 个显式改成 `variant="plain"`（`AgentsView.tsx:212`、`CustomAppsView.tsx:222`、`AutomationView.tsx:300`），
  **说明默认值与团队实际偏好相悖**。**规范建议**：默认改 `plain`，`dashed` 保留为显式可选项。
- **描边用量实测**：TSX 中 201 处 `border*` + CSS 中 155 处 `border:` 声明 = **约 356 处**。
  按用户偏好，这批描边应逐类替换为「底色阶差 + 阴影 + 间距」。

### §4 视觉细节

#### 4.1 图标

**基础盘面是健康的**（实测）：

| 项 | 实测 | 判定 |
|---|---|---|
| 图标库 | 仅 `lucide-react`，0 个竞争库 | ✅ |
| 不同字形 | 199 个，分布在 102 个文件 | ✅ |
| `icon-xs/sm/md/lg` 档位使用 | 502 处（41/244/166/51） | ✅ 单轨已建立 |
| 数字 `size={N}` | 21 处 | 🟡 4 处离档 |
| 裸 Tailwind 尺寸绕过 `icon-*` | 8 个文件 ~30 个 | 🔴 |
| 手写内联 SVG | 17 处 / 11 文件 | 🔴 7 处是 Lucide 重复实现 |
| emoji 当 UI 图标 | ~45 处 | 🔴 |
| 文字字形当图标（⚙ ✕ ✓ ➜） | ~12 处 | 🔴 |
| `strokeWidth` 取值 | **9 种**（0.5/1.4/1.5/1.6/1.75/1.8/2/2.2/2.5） | 🔴 |
| 图标按钮缺可访问名 | 0（但规则已无门禁） | 🟡 |

**问题 7（高）**：`VaultPanel.tsx:50-186` 手写了 **7 个 Lucide 图标的复制品**
（`IconEye`/`IconEyeOff`/`IconUser`/`IconKey`/`IconCheck`/`IconGlobe`/`IconX`），
而该文件 `:13` 已经从 `lucide-react` import 了 `ShieldCheck`。**规范建议**：直接 import，删掉约 130 行。

**问题 8（高）**：**同一个对勾有 5 种笔画粗细**——`ToggleSwitch.tsx:63`(1.6)、`ChoiceCard.tsx:108`(1.8)、
Lucide 默认(2)、`FloatingChat.tsx:417`(2.5)、`VaultPanel.tsx:140`(2.5)；
`ToastContainer.tsx:47` 还显式写了 `strokeWidth={2.2}`（全 app 仅此一处，Lucide 默认是 2）。
**规范建议**：删掉所有显式 `strokeWidth`，交回 Lucide 默认。

**问题 9（中）**：离档尺寸 4 处——`ui/Modal.tsx:181` `size-[18px]`（**比旁边 16px 的弹窗标题还大**）、
`AnalyticsView.tsx:122` `w-5 h-5`（20px）、`ErrorBoundary.tsx:43` + `ChatView.tsx:1137` `size={26}`、
`VaultPanel.tsx:614` `size={20}`。
**规范建议**：18/20/26 分别归到 `icon-lg`(16) 或 `size={24}`。

**问题 10（高）**：**一个概念多个图标**——

| 概念 | 字形 | 数量 |
|---|---|---|
| 设置 | `Settings` / `Settings2` / `SlidersHorizontal` / `Cog` | **4** |
| 错误 | `AlertCircle` / `AlertTriangle` / `ShieldAlert` | **3** |
| 编辑 | `Edit3` / `Pencil` / `FilePen` / `FilePenLine` | **4** |
| 复制成功 | `Check` / `CheckCheck` | 2（6 处 vs 3 处） |
| 删除 | `Trash2` / `FileX2` | 2（语义可区分，**但无成文规则**） |

**规范建议**：写一张**概念 → 字形**的定稿表（宪法 §八只规定了尺寸，没规定语义唯一性）。
`AlertCircle` vs `AlertTriangle` 的区别（阻断 vs 提示）**必须写进表里**，否则收敛时无从取舍。

**问题 11（中）**：emoji 与文字字形当图标——`AutomationView.tsx:48,61,73,85,427`（模板图标）、
`FileSearchWindow.tsx:410`（`📁`/`📄` 与 `FileTypeIcon` SVG 系统并存）、
`ScreenshotOverlay.tsx:283-298` / `ScreenshotEditor.tsx:396-400`（`▭ ⛶ ✓ ✕ ➜ ✎ ✂`）、
`MarkdownBody.tsx:839,852`（`✓ ! ★ i`）。
**规范建议**：**模板/头像 emoji 可保留**（那是身份不是 chrome），**文件类型与按钮字形一律换 Lucide**。
`FileSearchWindow` 与 `FileTypeIcon` 两套文件图标系统必须合一——这是同屏可见的不一致。

#### 4.2 文案

| 项 | 实测 | 判定 |
|---|---|---|
| `加载中…` 字面量 | 0 | ✅ G4 门禁有效 |
| `window.confirm` | 0 | ✅ G5 门禁有效 |
| 重复中文串（≥3 次） | **24 条 / 150+ 次** | 🔴 |
| 保存动词变体 | 按钮 4 种、**成功提示 6 种**、进行中 3 种 | 🔴 |
| 省略号 | ASCII `...` 22 处 vs 全角 `…` 43 处 | 🔴 |
| 中英混排行 | 155 | 🔴 |
| 错误文案缺「下一步」 | ~8 处 | 🔴 |

**问题 12（中）**：重复字面量既是可见文案**又是 aria-label**——
`关闭`×21、`删除`×14、`已复制`×10、`新建对话`×9、`保存`×7、`已启用`×6、`失败`×6。
**规范建议**：建 `components/strings.ts` 常量模块，一次改动同时修好可见文案与可访问名。

**问题 13（中）**：**保存这一个动作有 4 种按钮文案 + 6 种成功提示 + 3 种进行中文案**。
`保存`/`保存设置`/`保存并连接`/`确认保存`；`已保存`/`保存成功`/`已保存。…`/`已保存，…`/`已保存覆盖路径…`；
`保存中...`/`保存中…`/`保存中`（连省略号都没有）。
**规范建议**：按钮一律「保存」，进行中「保存中…」，成功「已保存」。**一次动作一个词**。

**问题 14（中）**：**实现细节名直接暴露给用户**——
`AutomationView.tsx:722-723` 下拉框显示 `default（危险工具需审批）` / `full（自动批准）`（裸 enum 值）；
`DailyMemoryPanel.tsx:74-76` 显示 `目标（goals）` / `进展（progress）` / `结论（conclusions）`；
`AnalyticsView.tsx:478` 显示 `迁移 (schema_migrations)`（数据库表名）。
**规范建议**：界面上说**概念**（标准 / 完全），不出现字段名。

**问题 15（低）**：`ErrorBoundary.tsx:48` 的说明「请尝试点击下方按钮重新加载此区域」**指错了按钮**——
第一个按钮是「重试」（只重渲染子树），第二个才是「重新加载应用」。
**规范建议**：「点「重试」重新加载此区域；问题持续请点「重新加载应用」重启。」

**规范建议（整体）**：
- 省略号**统一 `…`**（禁 ASCII `...`）；冒号/括号**统一全角**；
- 错误文案模板固定为「<发生了什么>，<下一步做什么>」——仓库里已有正面样本
  （`App.tsx:3602`「这条消息属于另一个会话，请切回该会话后再发送」、`AutomationView.tsx:162`「保存失败，请检查 schedule 表达式或后端日志」、
  `McpCatalogGrid.tsx:131-143` 给出原因 + 替代路径 + 重试按钮），**把这三处当模板**。

#### 4.3 字体、配色、对比度

**字体家族**：唯一真相源在 `tokens.css:113-121`（`--font-sans` / `--font-serif` / `--font-mono`），
`body` 与 SVG 均引用它——2026-09-29 之前的三套互不相同的字体栈问题已解决。✅

**问题 16（低）**：`code-view.css:196-198` **写死一整条等宽字体栈**，与 `--font-mono` 顺序不同且多了 `"Cascadia Mono"`：
```css
font-family: "JetBrains Mono", "JetBrainsMono NF", "Cascadia Mono", "Cascadia Code", Consolas, ...
```
→ **聊天代码块与预览代码块可能用不同字形，同一文件两处字符宽度对不上**。
**规范建议**：改 `var(--font-mono)`。

**字号 / 行高 / 字重**：主干合规，但**存在与 `--text-*` 平行的影子刻度**（em 倍率）：

| 位置 | 现值 | 实际像素 | 应走 |
|---|---|---|---|
| `components-prose.css:161-164` `.prose h1-h4` | `1.55em/1.33em/1.14em/1em` | 21.7/18.6/16/14 | `--text-display`/`--text-title`/`--text-title`/`--text-body` |
| `components-prose.css:251` `.prose table` | `0.95em` | 13.3 | `--text-ui` |
| `components-prose.css:298` `.prose code` | `0.9em` | 12.6 | `--text-ui` |
| `components-prose.css:768` `.md-path-link--bare` | `0.94em` | 13.16 | `--text-ui` |
| `components-helpers.css:352,358` 思考层标题/code | `0.95em`/`0.92em` | — | 同上 |

令牌层明禁半档（`tokens.css:32`），这些 em 值全部越界。

**问题 17（中）**：**四条阅读行高并存**——`1.75`（`--leading-reading`）、`1.72`（`.msg-bubble-user`，`components-prose.css:392`）、
`1.65`（`.tl-text-inner`，`components-helpers.css:406`）、`1.35`（标题）。
而 `docs/字体规范（已并入 tokens.css）.md:160` 角色表写的是 **1.68** ——**文档与代码已三方不一致**。
`.msg-bubble-user:391` 还用了已废弃的 `var(--font-size-body)`（`tokens.css:54` 明写「禁止新代码使用」）。
**规范建议**：`.msg-bubble-user` 与 `.tl-text-inner` 的行高统一到 `var(--leading-reading)`，
同步修 `字体规范（已并入 tokens.css）.md` 的角色表（文档只描述现状）。

**对比度（实测，WCAG 相对亮度公式，解析三套主题令牌）**

| 配对 | light | dark | ink-green |
|---|---|---|---|
| 正文 / 纸面 | 17.76 ✅ | 15.17 ✅ | 16.05 ✅ |
| 次级文字 / 纸面 | 7.34 ✅ | 9.39 ✅ | 6.27 ✅ |
| 三级文字 / 纸面 | 6.39 ✅ | 7.30 ✅ | 5.62 ✅ |
| 弱化文字 / 纸面 | 5.41 ✅ | 5.70 ✅ | 5.46 ✅ |
| 思考正文 / 纸面 | 6.39 ✅ | 6.63 ✅ | 5.62 ✅ |
| 链接 / 纸面 | 10.00 ✅ | 8.88 ✅ | 6.22 ✅ |
| success / 纸面 | 4.70 ✅ | 7.68 ✅ | **3.26 ❌** |
| error / 纸面 | 4.87 ✅ | 5.61 ✅ | **3.87 ❌** |
| warning / 纸面 | **4.02 ⚠** | 7.99 ✅ | **2.43 ❌** |
| info / 纸面 | **4.29 ⚠** | 6.38 ✅ | **4.24 ❌** |
| link-soft / 纸面 | 6.39 ✅ | 6.63 ✅ | **3.78 ❌** |
| success 文字 / success-soft 底 | **4.10 ⚠** | — | **2.87 ❌** |
| warning 文字 / warning-soft 底 | **3.54 ⚠** | 6.60 ✅ | **2.20 ❌** |
| error 文字 / error-soft 底 | **4.23 ⚠** | 4.88 ✅ | **3.43 ⚠** |
| **hero 标题 / 纸面** | 21.00 ✅ | 18.42 ✅ | **1.01 ❌❌** |
| **控件描边 / 纸面**（注释声称 ≥3:1） | **1.48 ❌** | **1.70 ❌** | **1.54 ❌** |
| **控件描边 / 卡片** | **1.37 ❌** | **1.52 ❌** | **1.44 ❌** |

**问题 18（阻断级）**：`ink-green.css:71` `--color-text-hero: #ffffff`，
而该主题的 `--color-bg-primary: #fdfefd`（近白）→ **对比度 1.01:1，hero 标题完全不可见**。
注释写的是「hero 满对比（**暗底主题**用白）」——**从 dark 主题复制过来忘了 ink-green 是浅色主题**。
消费点：`ChatView.tsx:1216` 欢迎页大标题（28px，`--text-hero`）。
**规范建议**：`ink-green` 的 `--color-text-hero` 改为 `#000000`（与 light 基线一致），注释同步修正。

**问题 19（高）**：`tokens.css:267` 给 `--color-border-secondary` 的注释是「控件描边（**WCAG 1.4.11 3:1**）」，
但实测 **1.37–1.70:1**，三套主题全部远低于 3:1。**这条注释是一句事实错误**。
**规范建议**：要么把值压深到真正 3:1（如 light 需 `#949494` 量级），
要么把注释改成实际语义（发丝分隔线）并把「控件描边」职责另立令牌。**注释里的合规声明必须为真**。

**问题 20（高）**：**语义色没有「文字安全档」**。
light/dark 两套主题里 `warning`(4.02) / `info`(4.29) 只够大字；
ink-green 的 warning/success/error/info **全部**低于 4.5:1。
而代码里这些**原始语义色被直接当文字色用**——`text-warning`（`TodoListCard.tsx:45`、`McpCatalogGrid.tsx:257`、
`ImChannelsCard.tsx:651,655,657,821`、`ChatView.tsx:1456`）、`text-success`（`BackendStatus.tsx:76`、
`ModelListTab.tsx:283`、`ImChannelsCard.tsx:650,666`）、`text-error`（`VaultPanel.tsx:466,592` 等 20+ 处）、
`text-info`（`AutomationView.tsx:466,1116`）——**12px 的 chip 文字用 3.26:1 的色**。
仓库其实已有 `--color-text-warning`（light 6.12 / ink-green 4.99 ✅），但**只有 warning 一个**，
且 `text-warning` 与 `text-text-warning` **两个名字在代码里混用**。
**规范建议**：
(a) 语义四色各补一个 `-text` 文字档（`--color-text-success/error/info/warning`），规则「≥4.5:1 压在本主题 bg-primary 与对应 soft 底」；
(b) 组件里**禁止**把原始语义色当文字色，chip 一律用 `-text` 档 + `-soft` 底；
(c) `text-warning` / `text-text-warning` 的双命名必须消歧——建议原始档改名 `--color-warning`（保留，图标/描边用），
文字档统一 `--color-text-warning`，并在门禁里禁掉 `text-warning` 这个类名（改用 `text-text-warning`）。

#### 4.4 无障碍

| 项 | 实测 | 判定 |
|---|---|---|
| `:focus-visible` 规则 | CSS 50 处 + TSX 25 处 | ✅ |
| 全局 reduced-motion 降级 | `base.css:96` 覆盖所有 `animation/transition` | ✅ |
| `role="dialog"` | 9 处（8 个自定义文件 + `ui/Modal`） | 🟡 多个弹窗无 role |
| `aria-modal` | 8 | 🟡 |
| `aria-live` | 6 | ✅ |
| **`aria-describedby`** | **0** | 🔴 |
| `aria-labelledby` | 3 | 🔴 |
| `aria-current` | 3 | 🟡 |
| 焦点陷阱 | **仅 `ui/Modal` 一处** | 🔴 |
| 焦点归还 | `ui/Modal` + `FloatingChat` | 🔴 |

**问题 21（高）**：**12 个文件各自实现弹窗，只有一处有焦点陷阱**。
实测 8 个自定义弹窗文件里**没有一个**含 `querySelectorAll`（焦点陷阱的实现特征），
其中 `CustomAppsView` / `EditProfileDialog` / `ModelAddDialog` **连 Esc 都没有**。
**规范建议**：`ui/Modal` 收编（见 §3.2）。

**问题 22（中）**：`aria-describedby` **零使用** → 表单的帮助文字与错误文字**不与控件关联**，
读屏用户只听到 label，听不到「这个字段要填什么格式」和「为什么错了」。
**规范建议**：`ui/Input` 的 `hint` / `error` 必须生成 id 并挂 `aria-describedby`（现 `Input.tsx` 已有 `role="alert"`，缺关联）。

**问题 23（中）**：`ui/Switch` 的 `label` 是可选 prop（`Switch.tsx:43` 默认「切换设置」），
`settings/SettingsShell.tsx:497` 未传 label → **文件关联页每一行（`.md`/`.pdf`/`.png`…）在读屏里都叫「切换设置」**。
**规范建议**：`label` 改必填。

**问题 24（中）**：`settings/BackendSettingsTab.tsx:65` 与 `settings/SecurityTab.tsx:113`
两个手写 `role="switch"` 按钮**完全没有可访问名**。
`agents/AgentForm.tsx:79` 的 emoji 选择按钮无可访问名；`:116` 的颜色方块只有 `title={k}`（`k` 是 `violet` 这类英文内部键）。
**规范建议**：全部换 `ui/Switch`（label 必填）或补中文 `aria-label`。

**问题 25（低）**：`code-view.css:210-240` 预览复制按钮 `opacity: 0`，仅 `:hover` 显形，
**无 `:focus-visible` 规则** → 键盘 Tab 聚焦到一个完全不可见的按钮。
**规范建议**：补 `:focus-visible { opacity: 1 }`。

**问题 26（高）**：`codeHighlight.ts:361` 的 `readIsDark()` 读 `document.documentElement.classList.contains("dark")`，
但 `useTheme.ts:41-44` **只设 `dataset.theme` 与 `style.colorScheme`，从不挂 `.dark` 类**
（`globals.css:23-24` 明写「`.dark` 类已废除」）。
实测：桌面端 `src/` 与 `packages/preview/src/` 内**所有 `.dark` 出现处都是注释，没有一条活的 CSS 选择器**
（`components-helpers.css:777-778`、`:890`、`tokens.css:11`、`registry.ts:8`、`globals.css:23` 全是注释）。
→ **`readIsDark()` 恒返回 false**。受影响的**两处活跃调用点**：

| 调用点 | 后果 |
|---|---|
| `MarkdownBody.tsx:802` `const isDark = readIsDark();` | **聊天代码块恒用亮色语法配色**。暗色代码底 `--color-bg-code: #1a1a1a` 上，`punctuation` 仅 **1.53:1**、`keyword` **2.85:1**、`string` ≈2.9:1 |
| `CodeView.tsx:217` `const dark = isDark ?? readIsDark();` | 兜底分支失效。`custom-apps/AppDetailModal.tsx:333` 的 `<CodeView source={codeSource} language={codeLanguage} />` **未传 `isDark`** → **自定义应用的代码编辑器在暗色主题下同样用亮色配色** |

**对照面是正确的**：`PreviewPanel.tsx:71` 显式传 `isDark={isDark}`，其值来自 `PreviewPanel.tsx:275-277` 的
`document.documentElement.style.colorScheme === "dark"`（该处注释 `:274` 已记录「`.dark` 类已废除，contains 检测永远 false」）。
→ **三个代码面（聊天 / 预览 / 应用详情）用了三套主题判据，其中两套是错的。**

`packages/preview/src/lib/codeHighlight.ts` 与桌面版 **SHA256 完全相同**（`90CF1788…AAAE1`），
但实测该包内 **`readIsDark()` 无任何调用点**（共享包的 `lib/CodeBlock.tsx:54` 用 `isDark` prop，默认 `false`，
由 `PreviewSurface.tsx:142` / `MarkdownView.tsx:210` 从调用方透传）→ **共享包侧是「坏掉的死导出」，
不是活跃缺陷**。两处仍应同改（避免下一个调用方踩进去），但**真正在出错的只有桌面端两处**。

**规范建议**：
(a) `readIsDark()` 改读 `document.documentElement.style.colorScheme === "dark"`，或直接复用
`isDarkTheme(themeId)`（`themes/registry.ts:53`）——**并让 `CodeView` 的 `isDark` 改必填**，
消灭 `?? readIsDark()` 这个兜底分支（兜底分支正是缺陷藏身处）；
(b) **同时修掉 3 处造成该 bug 的过期注释**：`codeHighlight.ts:358-360`（「`useTheme()` toggles a `.dark` class」）、
`CodeView.tsx:20`（「未传时自动从 DOM .dark class 读取」）、
`themes/registry.ts:8`（「useTheme 据此设置 `.dark` 类与 colorScheme」）——
**不删注释，下一个人会照着它再写一个 `readIsDark`**；
(c) `components-helpers.css:777-778` 那句「必须显式加 `.dark` 选择器才能覆盖」是同类过期指引，一并更正。

**这是本次审计发现的唯一「功能性」缺陷**，其余是观感与一致性。

### §5 LLM 对话输出的排版

#### 5.1 消息气泡

用户气泡右对齐 + `color-mix(accent 8%, bg-card)` 底 + 左下收口；助手正文直排、头像 22px、
续条用 `--indent-thread`(32px) 对齐首条正文——**这套系统是清楚的**。✅

**问题 27（中）**：**用户侧无头像**。`Avatar.tsx` 的 `role="user"` 分支在对话流中**从未被调用**
（全仓 `Avatar` 只出现在 `TextBubble.tsx:193` 与 `StreamingBubble.tsx:223`，两处都是 assistant）。
用户气泡靠右对齐 + 底色区分，**没有形状对称性**——一屏里两种角色的气泡几何完全不同。
**规范建议**：要么给用户侧一个 22px 头像（与助手对称），
要么明确把「用户 = 无头像 + 右对齐 + 圆角收口」写进设计宪法（当前的隐式状态不可依赖）。

**问题 28（低）**：`.msg-bubble-user { max-width: 80% }`（`components-prose.css:394`）被 `:406-408` 的
`max-width: 100%` 覆盖 → **死声明**。删掉，否则下一个人会以为它在生效。

#### 5.2 跨气泡的视觉系统不一致

| 组件 | 容器 | 排版 | 动作行 |
|---|---|---|---|
| `TextBubble`（定稿） | 头像 + 正文 | body 14 / 1.75 | `MessageActions`：13px，5 个按钮 + 时刻，图标 `opacity: 0.45` |
| `StreamingBubble` | 同上 | 同上 | `MessageCopyAction`：**12px，1 个按钮**，无 opacity 规则 |
| `ThinkingBubble` | `.tl-reasoning-wrap` | ui 13 + thinking 色 + 1px 左轨 | — |
| `ToolCallBubble` | `.tl-process-timeline` | 工具行 12px | — |

**问题 29（高）**：**流式 → 定稿时操作行形态突变**：
1 个 12px 按钮 → 最多 5 个 13px 按钮 + 时刻。
`StreamingBubble.tsx:4-14` 自己声明了「本组件与 `MessageBubble` **layout-equivalent**」——
**契约被违反**，落定瞬间可见跳变。
**规范建议**：`StreamingBubble` 渲染与 `TextBubble` **同尺寸**的操作行（同样 13px、同样 5 个位置、只是 disabled/隐藏），
或两者都收敛到单一尺寸的 `MessageActions` 变体。

#### 5.3 流式输出

**问题 30（高）**：**流式正文违反了 `MarkdownBody` 自己的 `interactive=false` 契约**。
契约写在 `MarkdownBody.tsx:731-735`：「流式渲染传 false：每个 chunk 重组小节树会让展开态反复重建」。
但流式正文实际渲染于 `ToolCallBubble.tsx:389-403` ——
`<MarkdownBody content={parsed.body} pathIcons onPathClick={…} />` **没有 `interactive` prop** → 默认 `true`（`MarkdownBody.tsx:799`）
→ 每个 chunk 重跑 `groupSections`（:941-950）→ `<section class="md-section">` 包装 + 复制/折叠按钮反复挂载卸载
→ **小节树在流式过程中持续重构，折叠态被重置，产生可见跳变**。
全仓只有 `StreamingBubble.tsx:158` 的 `StreamingReasoningBlock` 传了 `interactive={false}`。
**规范建议**：流式路径统一传 `interactive={false}`；把该 prop 改为**必填**（或按 `isStreaming` 自动推断），
让契约无法被静默违反。

**问题 31（中）**：**流式正文末尾无光标**。
`StreamingBubble.tsx:24-27` 注释：「我们刻意不在气泡内加动画点/微光，唯一的运动提示是底部的 `AgentActivityIndicator`」。
但 `AgentActivityIndicator` 在**气泡下方**——用户读文字本身时无法判断「还在写」。
**规范建议**：正文末尾加一个 2px 宽的 accent 光标（`caret` 形态，非三点微光），
**只在流式期间存在，定稿即消失**（用 `caret-color` 或内联块实现，不引入动画库）。

**问题 32（中）**：**围栏翻转跳变**。`markdownFence.ts:22-30` `autoCloseUnbalancedFences` 每个 chunk 补一个闭合 ```；
流式中未闭合围栏之后的散文**先被渲染成代码**，闭合围栏到达后整块翻回散文 → 大幅重排 + 重新高亮。
**规范建议**：未闭合围栏在流式阶段**按纯文本渲染**（不猜语言、不高亮），定稿时才做围栏解析。

**问题 33（低）**：`StreamingBubble` 落定后 meta 行也从「正在思考…」变成 model + stop chip，
两行内容与宽度都不同 → 第二处跳变。与问题 29 同源，一并修。

#### 5.4 代码块

**问题 34（高）**：**代码块宽度被 `--width-reading` 卡死，与令牌文档相反**。
`tokens.css:186-188` 明写「代码块 / 表格 / 长命令行**不跟随此档**，各自按内容需要放宽」，
实际 DOM 链 `MessageList.tsx:354 .max-w-reading` → `MarkdownBody.tsx:952 .prose.max-w-none`
→ 代码块最宽 780px，超长行在块内横向滚动。
**规范建议**：代码块容器解除 `--width-reading` 约束（用 `max-w-none` + 负 margin 溢出到消息列之外），
或**改令牌注释承认现实**——二者必须取一，不能文档一套实现一套。

**问题 35（中）**：**横向滚动容器与设计意图不符**。
`components-prose.css:337-354` 的 `.code-block-wrapper` / `.prose pre.code-block-body` 规则
**在全仓无任何 markup 使用**（类名只存在于 CSS）→ 本该重置代码块内层 `<code>` 的规则**从未生效**，
导致 `.prose pre code`（:355-368）的 `overflow-x: auto` / `display: block` **意外命中组件内部的 `<code>`**。
真正的滚动容器变成内层 `<code>` 而不是设计的外层 wrapper，
`MarkdownBody.tsx:311-312` 关于「行号列 sticky 左侧不跟着跑丢」的注释因此**不成立**。
**规范建议**：要么给组件加上 `.code-block-wrapper` 类名让规则生效，要么删掉这三条死 CSS。

**问题 36（中）**：**聊天代码块无换行开关，预览侧有**。
预览有 `WrapText` 按钮（`PreviewPanel.tsx:354-364` → `code-view.css:308-321` `.is-wrap`），
聊天侧 `white-space: pre` 硬走。同一段代码在两个界面观感不同。
**规范建议**：聊天代码块也提供「换行」开关（**默认关**，与预览一致），或明确接受差异并写进规范。

**问题 37（低）**：三个按钮缺 `type="button"`——`MarkdownBody.tsx:294`（复制）、`:390`（展开全部）、`:416`（收起），
`ToolCallBubble.tsx:349`（折叠头）、`:891`（记忆沉淀开关）。
**规范建议**：全部补 `type="button"`（丢进 `<form>` 会被误提交）。

**问题 38（低）**：**悬空类名**——`MarkdownBody.tsx:906` 的 `md-inline-code` 在全部 CSS 中**无任何规则**；
`MarkdownBody.tsx:261,597` 的 `not-prose` 是**空类**（项目未装 `@tailwindcss/typography`），
本意「重置 prose 样式」的意图完全没实现。
**规范建议**：删掉 `not-prose`（或装插件并真正使用）；`md-inline-code` 要么给规则要么删。

#### 5.5 列表 / 表格 / 引用 / 链接

- **列表**：padding-left 1.4em、li 间距 0.35em，marker 用 `--color-text-secondary` — ✅ 合理，但全用 em 而非 `--space-*` 档。
- **表格**：`th` 用 `--text-meta`(12px) 而 `td` 是 0.95em(13.3px) → **表头比表体小**，层级倒置。
  宽表横滚 ✅ 但**无滚动提示、无首列 sticky**。
- **问题 39（中）**：**引用块命中仓库自己的头号反模式**——
  `components-prose.css:208` `border-left: 3px solid var(--color-border-secondary)`，
  正是 `impeccable-design.md:36` 点名的「side-tab：卡片单侧 3px 实色彩条（AI 生成 UI 的最典型标志）」。
  颜色是中性灰、语义是引用，几何形态与被禁模式完全一致。
  **规范建议**：引用改为「底色阶 + 左侧 2px 中性轨 + 缩进」或「整体淡底 + 左侧圆角块」，
  **不得用 3px 实色条**。这条要同时写进 `impeccable detect` 的豁免说明（当前是误报还是真违规，要给出裁定）。
- **问题 40（中）**：`.md-callout`（`components-prose.css:213-223`）用 `border: 1px solid` + `bg-secondary`
  在正文里做**整圈描边卡片**——与用户「UI 一律不用描边」偏好相悖，且与正文同级、视觉重量偏重。
  **规范建议**：改 `bg-bg-tertiary` 淡底 + 圆角，无描边。
- **链接**：`border-bottom` + hover 转 `underline`（宽度不变、无 1px 抖动）✅ 做法正确。
  但 transition 里含 `opacity`（`:26`）而 opacity 恒为 1 → 无效声明，删。
- **行内代码**：`color-mix(in srgb, var(--color-bg-tertiary) 45%, transparent)`——**45% 是手调魔数**
  （`:293-294` 注释自述「上一版实测选的档位」），既不是令牌也不是 spacing 档，换主题后未再验证。
  **规范建议**：提升为 `--color-code-inline-bg` 令牌。

#### 5.6 长文本与多轮

- 超长助手答复**不截断、不折叠**，整段渲染（`MarkdownBody.tsx:798` 无长度分支）——对 LLM 输出是**正确**的。
- 超长代码块 >30 行折叠 + `maxHeight: 640px` + 渐变遮罩 + 「展开全部」✅（640/30 行/56px 遮罩是魔数，建议入档）。
- 超长思考 `max-height: 340px` 内部滚动 ✅（340px 魔数）。
- **问题 41（低）**：**轮次间距有三套值**——消息间 `space-y-5`(20px)、轮次内 `.tl-iterations { gap: 14px }`、
  用户气泡 `mt-5`(20px)。14px 不在任何 4px 档上。
  **规范建议**：轮次内间距统一到 `--space-3`(12) 或 `--space-4`(16)。

### §6 子程序界面组件

#### 6.1 弹窗

| 实现 | 宽度 | 焦点陷阱 | Esc | 焦点归还 | `role="dialog"` |
|---|---|---|---|---|---|
| `ui/Modal` | 560（裸数字） | ✅ | ✅ | ✅ | ✅ |
| `ui/ConfirmDialog` | 460 | ✅ | ✅ | ✅ | ✅ |
| `ModelAddDialog` | `min(94vw,560px)` | ❌ | ❌ | ❌ | ✅ |
| `SkillCard` | 720 | ❌ | ✅ | ❌ | ❌ |
| `AutomationView`×3 | 720 / 420 | ❌ | ❌ | ❌ | ❌ |
| `agents/TestDialog` | 448 | ❌ | ❌ | ❌ | **❌** |
| `SessionPickerDialog` | 720 | ❌ | ✅ | ❌ | ✅ |
| `EditProfileDialog` | 420 | ❌ | ❌ | ❌ | ✅ |
| `CustomAppsView` | 448 | ❌ | ❌ | ❌ | ✅ |
| `McpCatalogGrid`×2 | 448 | ❌ | ❌ | ❌ | **❌** |
| `AppDetailModal` | 800 | ❌ | ❌ | ❌ | ❌ |

**问题 42（高）**：**8 种宽度（420/448/460/560/720/800/1080）全部脱离 `--width-*` 档**，
而令牌层已定义 narrow 240 / base 420 / wide 720。
**规范建议**：`Modal` 的 `width` prop 改接受 `"narrow" | "base" | "wide" | "full"` 并映射令牌；
迁移期至少在 `ModalProps` 上标注「必须取自 `--width-*`」。

**问题 43（中）**：**面板高度上限 6 种**——85vh（`ui/Modal`）/ 90vh（`AutomationView`×3）/ 80vh（`TestDialog`）/
70vh（`.modal-form`）/ 60vh / 40vh（`DiagnosticsTab`）。
**规范建议**：新增 `--dialog-max-h` 令牌统一。

**问题 44（中）**：**同一「关闭」动作 5 种按钮尺寸**——`ui/Modal` `size-9`(36) / `SettingsShell` `size-7`(28) /
`SkillCard` `.modal-close`(24) / `ModelAddDialog` `h-7 w-7`(28) / `TestDialog` `p-1.5`(≈24)。
**规范建议**：统一走 `ui/IconButton`（0 使用，见问题 6）。

**问题 45（中）**：**背景点击即关闭，没有未保存保护**。
`ui/Modal.tsx:139` 外层 `onClick={onClose}`——设置类弹窗里改了东西，点遮罩就没了。
Esc 有 `closeOnEscape` 开关可关，**背景点击没有对应开关**。
**规范建议**：给 `Modal` 加 `closeOnBackdrop`（默认 true），表单类弹窗传 false；
或弹窗内存在未保存改动时，自动忽略遮罩点击并给出提示。

**问题 46（低）**：`AutomationView.tsx:583` 用**原生 `alert()`** 做表单校验——不跟随主题、
与全仓「确认一律走 `ConfirmDialog`」的约定冲突（`ImChannelsCard.tsx:240-242` 注释已记录过这个教训）。
**规范建议**：改弹窗内联错误条。

**问题 47（低）**：`AppDetailModal.tsx:461` 底栏把「编辑代码/保存/取消」放左、「运行/重新打包/打开目录」放右，
**同一底栏出现两套方向**，且「保存」与「运行」**都是实心主按钮**（`:467` 与 `:495`）。
**规范建议**：一个底栏只允许一个主按钮，其余降级为 secondary/ghost。

#### 6.2 表单

**问题 48（高）**：**表单层几乎没有错误提示能力**。
`ui/Input` 已内置 label/hint/error（`role="alert"`）/required/4 档尺寸——**但零 import**。
实测 **56+ 个裸 `<input>`** 分布在 15 个文件，全部手写 label/hint/error 布局，
**无 `htmlFor`/`id` 关联、无 `aria-invalid`、无错误态**。
**规范建议**：`ui/Input` 落地到这 15 个文件（`AdvancedSettingsTab` 6 个、`VoiceTab` 7 个、
`ModelAddDialog` 5 个、`ConnectorsTab` 6 个、`AgentForm` 7 个、`AutomationView` 7 个、`SecurityTab` 2 个、
`ImChannelsCard` 2 个、`McpCatalogGrid` 2 个）。

**问题 49（中）**：**表单控件高度同屏 6 种**——`ui/Input` 28/32/40/44、`.modal-input` ≈36、
`AdvancedSettingsTab` `px-3 py-2` ≈37、`ConnectorsTab` `px-2.5 py-1.5` ≈30、`ImChannelsCard` `h-9`=36、`AgentForm` `py-1.5` ≈30。
宪法 §五已定档（24/30/32/40/44），**规范存在但未执行**。
**规范建议**：控件高度只剩 `--control-h-sm/md/lg` 三档，其余一律迁移。

**问题 50（中）**：**必填标记 2 套**——`ui/Input.tsx:50` 用红色 `*`，
`AgentForm.tsx:92,102` 用字面 `"名称 *"`（黑色星号 + 空格）。
**规范建议**：统一走 `ui/Input` 的 `required` prop。

**问题 51（中）**：**`uppercase tracking-wider` 出现在 13 处**——
`AgentForm.tsx:92,102,113,131,142,154,179,188,200,224`（10 处）+ `AIGenerateWizard.tsx:321,469,534`（3 处）。
这正是 `config/MgmtTag.tsx:6-9` 注释里点名的「AI 生成 UI 廉价感来源」——
**同一个仓库里自己禁止的样式在自己代码里用了 13 处**。
**规范建议**：删 `uppercase tracking-wider`（中文标签大写无意义，tracking 也无必要）。

**问题 52（中）**：**错误文案把真实原因吞进 console**。
`AdvancedSettingsTab.tsx:60-61` 与 `SecurityTab.tsx:89` 只 toast「保存失败」，
真实错误只写 `console.error` → **用户看不到原因，也不知道下一步**。
**规范建议**：把 `e.message` 展示出来 + 给出下一步（参考 `McpCatalogGrid.tsx:131-143` 的模板）。

**问题 53（低）**：`<select>` 全部裸写（10 处），`ui/SelectField` 零使用；
原生 `<input type="checkbox">` 未样式化（`VoiceTab.tsx:75`、`ModelAddDialog.tsx:432`、`AutomationView.tsx:775`）。
**规范建议**：迁移到 `ui/SelectField` + `ui/Switch`。

**问题 54（低）**：`ui/SegmentedControl` 实现了方向键 roving tabindex（`SegmentedControl.tsx:65-73`），
但 `ChoiceCard.tsx` 的 radio 组没做 → Tab 会逐个停在每个选项上。
**规范建议**：roving tabindex 抽成 hook，两处共用。

#### 6.3 表格

**问题 55（中）**：全仓只有 **3 个 `<table>`**（`MarkdownBody.tsx:867` 正文渲染、`Sidebar.tsx:729`、
`DiagnosticsTab.tsx:261` 审计事件）。唯一的业务表格 `DiagnosticsTab.tsx:261-284` 缺三要素：
- **表头不吸顶**：`<thead>` 无 `sticky`，而外层是 `max-h-[40vh] overflow-auto` → 滚动后表头立即消失；
- **加载态被显示成空态**：`audit === null && !auditError` 时落到「暂无审计事件。」→ **「正在加载」显示成「没有数据」**；
- **空态无下一步动作**（纯文本），**错误态无重试按钮**。
**规范建议**：补 `sticky top-0`、补加载分支（`Skeleton`）、空态接 `ui/EmptyState` + 动作、错误态加「重试」。

**问题 56（中）**：`AnalyticsView.tsx:277-344` 三组统计行用 `flex justify-between + gap-4`，
**数值列不右对齐、不共用左边缘**，位数一变就左右跳；且该处**漏了 `tabular-nums`**
（本仓 `toolTabsShared.tsx:69,118,132` 已正确使用）。
**规范建议**：数值列固定宽 + `text-right` + `tabular-nums`。

**问题 57（低）**：`DiagnosticsTab.tsx:276` 「设备」列只显示 id 前 8 字符 + `…`，
而「来源 IP」不截断 → **同一表内长短策略不一致**。
**规范建议**：统一「截断 + `title`」策略。

#### 6.4 加载态 / 空态 / 错误态

**问题 58（高）**：**加载态 7 种措辞 + 4 套实现**——
骨架 10 文件用 `ui/Skeleton` ✅；`toolTabsShared.tsx:130` 私有 `SkeletonGrid` 用 `bg-bg-card/40`
（**40% 透明的卡片 = 看不见的骨架**，正是 `PluginsTab.tsx:124-126` 注释里点名并「已删除」的那个坑，又回来了）；
`ui/Spinner` 仅 1 处使用，25+ 处手写 `Loader2 animate-spin`；11 处纯文字加载态用了 7 种措辞
（「读取中…」「读取系统状态中…」「检测中...」「检测中」「正在读取…」「正在读取模型配置…」「正在读取内置目录…」）。
**规范建议**：
(a) 删 `SkeletonGrid`，统一 `ui/Skeleton`；
(b) `ui/Spinner` 或 `Loader2` **二选一**（推荐后者 + 保留 `Button` 的 `loading` prop），
并让**按钮内 loading 统一用 `Button` 的 `loading`**（现在 3 个文件手写 `Loader2` 而不用 prop）；
(c) 纯文字加载态收敛成一种措辞 + `aria-busy`。

**问题 59（中）**：**错误横幅 5 套**——`bg-red-500/12 text-red-400` / `bg-bg-card text-error` /
`bg-error/10 text-error` / inline style / `bg-error-soft`。
**规范建议**：抽 `ui/Alert`（tone × 可关闭 × 可带 action），统一走语义令牌。

**问题 60（中）**：**空态 4 套实现、15+ 调用点**——
`ui/EmptyState`(5) / `toolTabsShared.EmptyHint`(3) / 裸 `<p>`(5) / 裸 `<div>`(3)。
**规范建议**：统一 `ui/EmptyState`，默认 `variant="plain"`（见问题 3.3）。

**问题 61（低）**：`settings/PersonasPanel.tsx:189` 空态文案「选择**左侧**角色查看提示词全文。」
与实际布局不符——容器是 `md:grid-cols-[220px_1fr]`，窄屏纵向堆叠时「左侧」指错。
**规范建议**：改「在上方或左侧选择角色…」。

#### 6.5 令牌违规（子程序层）

**问题 62（中）**：**4 处真裸色**——
`McpCatalogGrid.tsx:282,337` `bg-black/30`（`black` **不在** `tokens.css:421-444` 的重定向清单里 → 真裸黑）、
`SessionPickerDialog.tsx:68` `bg-black/40`、`CustomAppsView.tsx:292` `var(--color-error, #dc2626)`（内联 hex 兜底）、
`CustomAppsView.tsx:291` `text-white`（应走 `--color-on-error`）。
**规范建议**：前两处改 `bg-[var(--color-overlay-scrim)]`（`ui/Modal.tsx:135` 注释已记录「三份不同深度的遮罩」这个历史问题，
现在又出现了第 4、5 份）；后两处去兜底。

**问题 63（中）**：**2 处字重越界**——`toolTabsShared.tsx:54,59` `text-xl font-bold`（700 >
`tokens.css:67` 的 600 上限，中文会糊成块）、`AnalyticsView.tsx:694` `text-lg`（18px，不在七档内）。
**规范建议**：`font-semibold`；18px → `var(--text-display)`。

### §7 宪法需要补的 5 条约束

宪法覆盖了令牌/组件分层/图标尺寸/响应式分档，但**漏掉了下面 5 类**——它们全部逃过了现有三道门禁：

| # | 新约束 | 理由 |
|---|---|---|
| **A** | **对比度是硬约束**：任何前景/背景配对 ≥4.5:1（正文级）/ ≥3:1（大字与控件边界）；每套主题必须提供 `-text` 文字档 | 实测 8 处 FAIL，包括 hero 标题完全不可见（1.01:1） |
| **B** | **语义色不得直接当文字色**：12px 文字必须用 `--color-text-*` 档 | `text-warning`/`text-success`/`text-error` 在 20+ 处当小字用，ink-green 下只有 3.26:1 |
| **C** | **注释里的合规声明必须为真** | `--color-border-secondary` 注释写「WCAG 1.4.11 3:1」，实测 1.37–1.70:1 |
| **D** | **可访问名与焦点管理是弹窗的准入条件**，不是可选项 | 12 个弹窗实现只有 1 个有焦点陷阱；`aria-describedby` 零使用 |
| **E** | **组件原语的「零调用」与「旁路」是违规** | `ui/IconButton`/`ui/Input`/`ui/Card`/`ui/Badge` 零调用，而 415 个裸 `<button>` |

### §8 新增门禁（把「靠自觉」变成「靠门禁」）

按 `AGENTS.md`「新增约束优先写进门禁而不是靠自觉」与 `scripts/check-design-guard.mjs` 的既有模式：

| 门禁 | 规则 | 阻塞级别 |
|---|---|---|
| `check-theme-tokens.mjs` 加 `contrast` 规则 | 解析三套主题令牌，对 §4.3 的 12 组固定配对算 WCAG 比值，<4.5:1 退出 1 | **阻断**（先修完 ink-green 再开） |
| `check-theme-tokens.mjs` 加 `semantic-text` 规则 | 禁 `text-warning`/`text-success`/`text-error`/`text-info` 四个类名（改用 `text-text-*`） | 阻断 |
| `check-design-guard.mjs` 恢复 **G3（限定版）** | 只查 `className` 含 `icon-btn`/`titlebar-btn`/`icon-button` 的按钮是否有 `aria-label`/`aria-labelledby` | 阻断 |
| `check-design-guard.mjs` 加 **G6 描边检查** | TSX 里 `border-dashed` / `border-l-[2-4px]` / `border-l-4` 一律禁（side-tab 与虚线空态） | 阻断（`EmptyState` 改默认值后） |
| `check-design-guard.mjs` 加 **G7 图标尺寸** | `size={N}` 的 N 必须在 {10,12,14,16,22,24,28,32,36,48,72}；裸 `size-[Npx]`/`w-N h-N` 上的图标一律禁 | 阻断 |
| `check-design-guard.mjs` 加 **G8 控件高度** | 表单控件的 `h-*`/`py-*` 只能是 24/30/32/40/44 五档 | 告警 → 收敛后阻断 |
| 新 `check-design-primitives.mjs` | 统计 `ui/` 各原语的 import 数与旁路数，**零调用原语**告警 | 告警 |

### §9 收敛批次

**P0（真缺陷，一周内）**
1. `readIsDark()` 改判据（读 `colorScheme` 或复用 `isDarkTheme()`），`CodeView` 的 `isDark` 改必填
   —— 修**两处活跃出错点**（`MarkdownBody.tsx:802` 聊天代码块、`AppDetailModal.tsx:333` 应用详情代码编辑器）；
   同步修 `codeHighlight.ts:358`、`CodeView.tsx:20`、`registry.ts:8` 三处过期注释；
   `packages/preview` 侧同改（该处是死导出，非活跃缺陷）
2. `ink-green` 的 `--color-text-hero` 改 `#000000` —— 欢迎页标题目前完全不可见
3. `MemoMessageBubble` 比较器补 `hasRunningTool` / `onRetryLastMessage`
   （`MessageList.tsx:90-106`）—— 工具运行时折叠态不更新、「重新生成」按钮不出现
4. 流式路径传 `interactive={false}`（`ToolCallBubble.tsx:389-403`）—— 流式期间小节树持续重构
5. 补 `type="button"`（`MarkdownBody.tsx:294,390,416`、`ToolCallBubble.tsx:349,891`）
6. `AutomationView.tsx:583` 的原生 `alert()` 换内联错误条

**P1（一致性收敛）**
7. `ui/Modal` 收编 12 个弹窗实现（一次性补齐焦点陷阱/Esc/焦点归还/aria-modal + 宽度归 `--width-*`）
8. `ui/Input` 落地 15 个文件（补 `aria-describedby` / `aria-invalid` / 统一控件高度）
9. `ui/IconButton` 落地 + 恢复 G3 门禁
10. 图标语义定稿表 + 删手写 SVG（`VaultPanel` 7 个）+ 删显式 `strokeWidth`
11. 删 `uppercase tracking-wider`（13 处）+ 改 `EmptyState` 默认 `plain` + 引用块去 3px side-tab
12. 文案常量模块（`strings.ts`）+ 省略号统一 `…` + 保存动词归一

**P2（体系化）**
13. 三套主题的 `-text` 文字档补齐 + 对比度门禁阻断
14. 语义色做「填充/图标」与「文字」两套
15. `--color-border-secondary` 的注释与取值二选一对齐
16. 栅格降档门禁 + `@container` 迁移（或修正注释）
17. 加载/空/错误三态各收敛到一套

## Alternatives considered

- **新建一份顶层 `docs/DESIGN_SPEC.md` 全面重写规范**：否决——`docs/` 顶层是闭集白名单
  （`scripts/check-notes.sh` 第 5 条会 fail），且会与宪法形成第二份真相源。改为向宪法补 5 条。
- **直接改代码而不写笔记**：否决——本次结论里有 5 条是「宪法未覆盖的持久约束」
  （对比度硬约束、语义色不得当文字色、注释合规声明必须为真、弹窗可访问名准入、原语零调用即违规），
  这些不写进宪法，下次会重新争论。
- **把审计结论散落成多篇分类笔记**：否决——5 条新约束彼此耦合（都出自「令牌层已收口、组件层未收口」这一判断），
  拆开会导致跨文件引用，反而增加维护面。
- **用 `impeccable detect` 代替人工审计**：否决——`AGENTS.md` 已记录「detect 对**解析不了的文件会静默返回 0**」，
  且 detect 只覆盖代码型反模式，不覆盖对比度、原语采用率、文案一致性。本次审计的价值恰在 detect 覆盖不到的维度。
- **本篇取代 [2026-10-16-unified-design-system.md](../feature/2026-10-16-unified-design-system.md)**：否决——
  宪法的方向全部正确，只是没被执行；本篇是它的**执行度报告 + 补丁**，不是替代品。两篇互链。

## Consequences

- **本篇不产出代码改动**，只产出约束与批次。P0 的 6 项都是「改 1–3 行、无设计裁量」的缺陷，可以直接开工；
  P1/P2 涉及组件收敛与门禁改造，需要单独的 PR 序列。

### 2026-10-21 执行记录（同日，按 §9 批次推进）

**全部完成。最终验证（一次连跑）**：
`tsc --noEmit` exit 0 · `check-theme-tokens` exit 0（契约 106/106、contrast 全过）·
`check-preview-layer` exit 0 · `check-design-guard` exit 0 ·
`check-design-primitives --strict` exit 0（7 条规则全清）·
`impeccable detect` exit 0 · `vitest` 35 文件 / 333 测试全过。

#### P0 六项真缺陷（已修）

| 项 | 改动 | 文件 |
|---|---|---|
| 暗色代码块错色 | `readIsDark()` 改读 `root.style.colorScheme`（旧版读已废除的 `.dark` 类，恒为 false）；新增响应式 `useIsDarkTheme()`（`useSyncExternalStore` + **单一共享监听**，避免一个对话里上百个 MarkdownBody 各挂一个 window 监听）；同步修 `codeHighlight.ts` / `CodeView.tsx` / `registry.ts` 三处过期注释 | `lib/codeHighlight.ts`（桌面 + `packages/preview` 两份）、`hooks/useTheme.ts`、`components/CodeView.tsx`、`components/MarkdownBody.tsx` |
| ink-green hero 不可见 | `--color-text-hero` 由 `#ffffff` 改 `#000000`（**1.01:1 → 20.77:1**） | `styles/themes/ink-green.css` |
| memo 比较器漏 prop | 补 `hasRunningTool` / `onRetryLastMessage` | `components/MessageList.tsx` |
| 流式小节树持续重构 | 流式路径传 `interactive={!isStreaming}`（`ProcessTimeline` 同时服务流式与定稿两条路径） | `components/ToolCallBubble.tsx` |
| 按钮缺 `type="button"` | 复制/展开/收起/折叠头/记忆沉淀 5 处补齐 | `MarkdownBody.tsx`、`ToolCallBubble.tsx` |
| 原生 `alert()` 做校验 | 换成弹窗内联错误条（`role="alert"`，字段一改即撤销） | `components/AutomationView.tsx` |

#### P1 四个批次（弹窗 / 表单 / 视觉 / 文案）

- **弹窗收编**：9 个自建弹窗改用 `ui/Modal` / `ConfirmDialog` —— 一次性补齐焦点陷阱、Esc、
  焦点归还、`role="dialog"` + `aria-modal` + `aria-labelledby`；宽度收进 `--width-*` 语义档；
  新增 `closeOnBackdrop`（表单类传 `false`）；真裸黑 `bg-black/30` 消失。
- **表单收编**：**37 个裸控件**（33 个 input/textarea/select + 3 个 switch/checkbox + 1 个搜索框）
  迁到 `ui/Input` / `ui/TextArea` / `ui/SelectField` / `ui/Switch`；
  `ui/Input` 补 `useFieldIds()`（label/hint/error 全部 `htmlFor`/`id`/`aria-describedby` 关联）
  → **全仓 `aria-describedby` 由 0 变为有**；`ui/Switch` 的 `label` 改必填；
  焦点态统一为「2px accent 环 + 2px 偏移」；控件高度收进 `--control-h-{sm,md,lg}` 三档。
- **视觉细节**：确立**「概念 → 字形」定稿表**（设置 = `Settings`、错误 = `AlertCircle`、
  提示 = `AlertTriangle`、安全 = `ShieldAlert`、复制成功 = `Check`、批量完成 = `CheckCheck`）；
  删掉 `VaultPanel` 7 个手写 Lucide 副本（-156 行）；`ToggleSwitch` / `FloatingChat` / `ChoiceCard`
  的手写勾叉换成 Lucide；引用块去掉 3px side-tab（改淡底 + 2px 中性轨 + 缩进）；
  `.md-callout` 去掉整圈描边；`EmptyState` 默认 `variant` 由 `dashed` 改 `plain`；
  `components-prose.css` 的 em 影子刻度（h1-h4 21.7/18.6/16/14px、table 13.3px、code 12.6px）
  全部收进 `--text-*`；4 条阅读行高统一到 `--leading-*`。
- **文案一致性**：新建 `components/strings.ts`（25 个常量 + `failure(what, next, detail?)` 模板）；
  保存动作三态归一（保存 / 保存中… / 已保存）；省略号统一全角 `…`（ASCII 22 处清零）；
  冒号括号统一全角；实现细节名（`goals`/`progress`/`schema_migrations`/裸 enum `default`/`full`）
  从界面上移除；错误文案统一成「<发生了什么>，<下一步做什么>」。

#### 新增令牌

- `--color-text-{success,warning,error,info}` —— 语义四色的**文字安全档**，
  **保持色相、只压暗**解出来，纸面与各自 `-soft` 底都 ≥4.5:1
  （保留宪法的「语义四色色相彼此拉开 ≥30°」纪律，而不是换成另一套色）。
- `--control-h-{sm,md,lg}`（24/32/40）—— 控件高度三档。
- `--dialog-max-h`（85vh）—— 弹窗最大高度，此前 85/90/80/70/60/40vh 六种并存。
- `--color-code-inline-bg` —— 行内代码底色，此前是 `components-prose.css` 里手调的 45% 魔数。

#### 两条新门禁

1. `check-theme-tokens.mjs` 加 **contrast 阻断规则** —— 29 组前景/背景配对实测 WCAG AA
   （半透明底**合成**到纸面上再算）。这是「取值必须真的能读」的机械断言。
2. 新增 `scripts/check-design-primitives.mjs`（进 ci-gate 的 `[4e/14]`，**已转阻断**）：
   G3 图标按钮无障碍名 / G6a 虚线描边 / G6b side-tab / G7 图标尺寸档位 /
   G8 表单控件高度 / G9 原始语义色当文字色 / P0 `ui/` 原语零调用。
   路径从脚本自身位置推导（不像 guard 那样按 `cwd` 拼 `src/`，在仓库根跑会 ENOENT 崩）。

#### 修正了一条事实性错误注释

`--color-border-secondary` 原注释写「控件描边（WCAG 1.4.11 3:1）」，实测三套主题只有
1.37–1.70:1。1.4.11 的 3:1 实际落在 `--color-border` 上（3.54–4.17:1 ✓），
`--color-border-secondary` 是发丝分隔线，真实契约是「与所压底色亮度比 ≥1.10」。
注释已改正，门禁也按真实契约分别判两个令牌。

#### 执行过程中的一次事故与教训

主控的 G9 语义色迁移脚本**两次**写坏文件：第一次切片错误截断了 15 个文件；
第二次把 `className="` 的属性包裹弄丢（171 条 TS 错）。两次都是**长度中性或错位**的
重建错误，靠肉眼看不出来 —— 靠 `tsc` 才暴露。

最终版脚本改用**往返校验**：把结果里的 `text-text-X` 全部还原成 `text-X`，
必须与输入**逐字节相同**，外加引号数 / className 数 / 花括号数 / 长度增量四道闸门；
不满足就**放弃写入**。脚本用完后已删除（`mavis-trash`），不入库。

**教训（对所有批量改写脚本都成立）**：批量改写前端源码时，**必须有一道能证明
「除了目标，别的什么都没动」的校验**；只靠 `tsc` 是事后补救，不是防线。

#### 第二轮：把「诚实清单」里剩下的项做掉

| 项 | 处理 |
|---|---|
| 4 个手写弹窗 | 分类后只有一个是**真模态框**：`UpdatePanel`（居中、`role=dialog` + `aria-modal`）→ 改用 `ui/Modal`（`width` 走语义档、`zIndex` 10000、`closeOnBackdrop/Escape` 按 forced 与 busy 状态关）。另三个结构上不是模态框，**不收编**：`PreviewPanel` 是全屏覆盖层（补焦点陷阱）、`FloatingChat` 是可拖浮层（补 `aria-modal="false"`）、`PopoverShell` 是锚定浮层（去掉描边） |
| Esc 保护 | 7 个表单弹窗加 `closeOnEscape={false}`（与已有的 `closeOnBackdrop={false}` 对齐：**表单只能通过显式的「取消」/「×」/「保存」离开**，避免静默丢数据） |
| 弹窗宽度裸数字 | 新增结构性令牌 `--width-panel: 1080px`（设置主面板）与 `ModalWidth` 的 `panel` 档；`SettingsDialog` 1080 → `"panel"`、`VaultPanel` 480 → `"base"`。**全仓再无裸数字弹窗宽度** |
| 死 CSS | 删掉 **307 行**遗留的 `.modal-*` / `.app-detail-btn*` / `.app-detail-iconbtn` 等规则（逐条按「选择器里每个类都零引用」判，删后反向验证活类仍有规则） |
| AutomationView 表单 | 15 个字段（任务/工作流/填参数三处弹窗）从手写 `<Field>` + `.modal-input` 迁到 `ui/Input` / `ui/TextArea` / `ui/SelectField`；`ui/Input` 新增 `rightAction`（AI 生成按钮挂在 label 行右侧）；剩下唯一的「技能多选」用新的 `FieldGroup`（无单一控件，不需要 label↔控件关联） |
| 输入框描边 | `ui/Input` / `ui/TextArea` / `ui/SelectField` **去掉描边**（用户明确否过「输入框描边」）：层次由底色差承担 —— 控件是 `--color-bg-tertiary`，比卡片面深一档、比纸面深两档。错误态改用**淡红底**（`--color-error-soft`）而不是红描边。`Button` 的 `secondary` 变体同步去描边（它有 `bg-card` 底，边框本来就没贡献层次）。**明确命名的 `-outline` 变体（`tonal-outline` / `danger-outline`）保留描边** —— 它们的语义就是「描边」，去掉会名不副实 |
| 焦点陷阱 | 抽成 `hooks/useFocusTrap.ts`（`ui/Modal` 与 `PreviewPanel` 全屏覆盖层共用），解决「声明了 `aria-modal="true"` 却没有陷阱」的最后一处 |
| `ImChannelsCard` 加载文案 | 「正在读取绑定状态…」→ `{LOADING}` |
| `PopoverShell` 描边 | 去掉 `border border-border-primary`，层次改由 `bg-bg-elevated` + `shadow-pop` 承担 |

**仍然没做的**：全仓 200+ 处一次性 `border` 工具类（feature 代码里的局部描边）没有全量清除 ——
那是另一次扫面，不在本次范围。设计原语层（`ui/*`）已按「不要描边」对齐。

#### 第三轮：描边长尾清理（用户明确「UI 一律不用描边，层次靠底色差+阴影+间距」）

先分类再动手，不做无差别删除：

| 类别 | 数量 | 处理 |
|---|---|---|
| 中性结构描边（`border` / `border-border-*` / `border-primary` 等） | 238 | **清到 27** |
| 语义色描边（红/琥珀/绿/强调色） | 35 | **保留** —— 它是状态信号，不是层次 |
| 分隔线（`border-t` / `border-b`） | 18 | **保留** —— 它是线，不是框 |

清理的做法分两步：
1. **冗余的**（元素已经有底色，又画了一圈中性描边）→ 直接删描边，底色已经把盒子区分出来了 —— **67 处**。
2. **描边是唯一可辨识手段的**（按钮/输入框/浮层）→ 不是删，是**换成填充**：
   按钮 `border + hover:border` → `bg-bg-secondary + hover:bg-bg-hover`；浮层 → `bg-bg-elevated + shadow`；
   输入框 → `bg-bg-tertiary`。**9 处**。

**剩下的 27 处是刻意保留的**：复选框自身的方框（2）、spinner 圆环（1）、状态横幅的语义描边（1）、
图片边框（1），以及 14 条分隔线。

**新增门禁**：G6 已有的虚线描边 / side-tab 规则之外，本轮把「结构描边不得增加」写进
`check-design-primitives.mjs` 的基线思路 —— 存量清到 27 后，任何新加的中性结构描边都会让门禁变红。

#### 第五轮：把门禁本身跑准（门禁也会骗人）

第四轮的门禁全绿是「单独跑」的结论。第五轮把仓库的完整 CI 门槛
`scripts/ci-gate.sh` 跑起来（此前一直被「找不到 Git Bash」的误判挡着 ——
本机 Git Bash 装在 `F:\Program Files\Git\`，不在 C 盘默认路径），结果暴露了
**另一类问题：门禁自身的误报。**

**`check-css-var-refs.cjs` 把合法的 `var(--x, fallback)` 判成「未定义引用」。**
它的判定只看「这个 var 名有没有被定义」，完全不看有没有 fallback —— 而
CSS 规范里 `var(--x, 280px)` 在 `--x` 未定义时用 fallback，**声明不会失效**；
只有「未定义**且**无 fallback」才会让整条声明在计算值阶段掉链子，那才是
这个脚本要抓的 bug。实测 19 处命中**全是这类误报**（集中在 `design-previews/`
的存档 HTML），硬门禁直接变红。

修法（`apps/desktop-tauri/scripts/check-css-var-refs.cjs`）：

1. 解析整个 `var(...)` 调用并取出 fallback：未定义**且**无 fallback 才算违规；
2. 对带 fallback 的引用**递归检查 fallback 内部** —— `var(--a, var(--b))`
   若 `--b` 未定义且无 fallback，整条声明仍然失效，仍要报；
3. 补齐「运行时定义」的识别：HTML 内联 style（`style="--tc:var(--t1)"`）与
   HTML 内联脚本里的 `setProperty("--tc", …)` 原先都不算定义（只扫 TS/TSX），
   补上后已定义数 287 → 291。

**门禁自测（改完必须证明没把门禁改弱）**：临时塞入两处无 fallback 的未定义
引用 → 门禁精确到行报出、exit 1；再塞 `var(--a-missing, var(--b-missing))`
→ 递归报出；`var(--c-missing, #888)`（字面量 fallback）→ 不误报。确认后删除自测文件。

**CI gate 的环境约束（如实记录，本轮拿不到「ci-gate 全绿」）：**

- `tauri-build` 编译桌面 crate 时要把 sidecar 复制成 `target\debug\hermix.exe`。
  只要**运行中的** `hermix.exe` 占着这个文件，Windows 文件锁就让 build script
  panic（`PermissionDenied / os error 5`），`cargo check` / `cargo test` 全都跑不动。
  这是「后端常驻 + 同目录同名 sidecar」的结构性冲突，与本轮改动无关
  （本轮一行 Rust 都没碰）。解除占用后第 1 步 `cargo check --workspace --all-targets`
  即通过（`Compiling hermix-desktop` 成功）。
- 第 3 步 `cargo test --workspace --lib` 的**首次**失败是
  `crates/hermix-server/src/dashboard/automation.rs:1142: unexpected closing delimiter`
  —— 并行工作线未完成的改动，本轮改动范围不含 `crates/`。该错误随后被对方修好，
  重跑后第 3 步推进到 1591 个测试通过（1138 + 64 + 133 + 15 + 181 + 39 + 21），
  最终卡在 `hermix-environments` 的 **5 个环境相关失败**：
  `sandbox: SetNamedSecurityInfoW failed (Win32 error 5) granting <TMP>\hermix-sbx-*`
  —— 沙箱给临时目录设 ACL 被拒。**换 `TMPDIR` 到普通临时目录后 5 个失败降为 2 个**
  （`test_execute_command_timeout` 期望超时未超时、
  `powershell_route_unknown_command_reports_nonzero_exit` 期望 stderr 非空却为空），
  剩下两个是本机 PowerShell 行为差异。失败文件 `crates/hermix-environments/src/local.rs`
  **本轮未修改**（该 crate 本轮只被并行线改过 `pty_session.rs`）。
- 第 11 步 `check-hygiene.sh` 报 3 处「新建顶层目录」：`branding/`（2026-10-16 引入）、
  `patches/`（10-15）、`hermix-logo-concepts_assets/`（10-19）—— 都是别的会话引入的，
  本轮**没有新建任何顶层目录**。顺带修了该门禁自身的一处漏项：`packages/`
  （monorepo 既定结构，与 `crates/`/`apps/` 同级，10-13 起就在）不在白名单里，
  导致它在干净仓库上恒红 —— 已补进 `TOP_ALLOWED`（4 处 → 3 处）。
- 综上：**本轮不宣称 ci-gate 全绿**。本轮自行验证并全绿的部分：第 1/2 步
  （cargo check 全 workspace + clippy await_holding_lock）、第 6/10/15 步
  （主题令牌、笔记结构、CSS undefined-var 守卫）+ 第 14 步死块守卫
  （7 处 vs 基线 5，其中新增的 2 处是 `components-prose.css` 里带 `!important`
  的旧块被启发式脚本误判，且该块本轮未改动）。

#### 验证证据（每轮结束时各一次连跑；末次 = 第五轮之后）

```
tsc --noEmit                                  exit 0
check-theme-tokens.mjs                        exit 0   契约 107/107 · contrast 34 组 × 3 主题全测、0 跳过
check-preview-layer.mjs                       exit 0
check-design-guard.mjs                        exit 0
check-design-primitives.mjs --strict          exit 0   G3/G6a/G6b/G7/G8/G9/G10/P0 全清
check-css-var-refs.cjs                        exit 0   287→291 个定义 · 4062 个引用点全测
impeccable detect src/components src/styles   exit 0
vitest run                                    36 文件 / 338 测试 全过
check-notes.sh                                exit 0   决策笔记结构
ci-gate.sh 第 1/2 步                            通过     cargo check --workspace --all-targets + clippy 守卫
ci-gate.sh 第 3 步                             1591 通过 / 5 环境失败（hermix-environments 沙箱 ACL）
```

`ci-gate.sh` 的完整结论见「第五轮」一节：**第 1/2 步通过，第 3 步被环境相关的
沙箱测试失败与并行工作线引入的顶层目录挡住**，本轮不宣称全绿。

**真实渲染验证**（不是 mock）：`vite build` 产出生产包 → 静态服务器托管 → 真实浏览器加载
`http://localhost:4173/`，**HTTP 200、标题「Hermix Desktop」、控制台零错误**，
DOM 里是完整的启动页结构（进度条 / 四阶段 / 窗口按钮，`aria-label` 齐全）。

启动页之后的**主界面**需要 Tauri 后端（前端 API 走 `invoke()` IPC，浏览器里没有 Tauri 运行时
就无法越过启动页 —— 这不是可以 mock 掉的差距）。本轮尝试启动真机：
`cargo tauri dev` → sidecar 构建被另一条并行工作线**正在运行的 `hermix.exe`** 占住文件
（`os error 5`，dev.mjs 里已有同名问题的处理先例）。

**关键突破：GUI 二进制根本不需要重编。** dev 模式下 GUI 从 vite :1420 加载前端，
本轮只改了 TS/CSS，Rust 侧一行没动 —— 直接 `Start-Process target\debug\hermix-desktop.exe`
就能跑真机。唯一的技术坑是 **vite 必须绑 IPv4**（默认只听 `[::1]:1420`，
而 WebView2 解析 `localhost` 走 127.0.0.1 → 「拒绝连接」），加 `--host 127.0.0.1` 解决。

于是走查闭合了（见下文「第四轮」）：主界面、设置页、模型页、添加模型表单弹窗、对话排版、
右侧面板/文件浏览器、浅色与暗色两套主题，都用眼睛看过并逐张读过图。

`vitest` 在并行 cargo 构建占满 CPU 时偶发 `workbook.test.ts` 的 60s hook 超时；
**单独跑该文件 29/29 全过**，安静时整轮 36/36 全过 —— 是负载 flake，不是逻辑失败，
且该文件属于本轮范围外的 `packages/preview`（另一条并行工作线在改 xlsx 还原）。

#### 第四轮：真机视觉走查，抓出一个静态分析看不见的暗色缺陷

前三轮的证据都来自静态分析与令牌计算。第四轮用**真机**（`target\debug\hermix-desktop.exe`
+ vite :1420）逐屏走查，**抓到一个静态分析漏掉的真缺陷**：

**`--color-accent-soft` / `--color-accent-soft-strong` 在暗色主题下从未被覆盖。**
tokens.css 里它们是「accent 混白」（为白纸面调的），`themes/dark.css` 只覆盖了
success / error / warning / info 四色组的 soft 档，**唯独漏了 accent 这两档** ——
于是暗色下它们仍是「accent 10%/18% 混白」= 近白亮块。真机表现：

| 位置 | 暗色下（修前） | 后果 |
|---|---|---|
| 外观页选中的选择卡 | 白底卡片 | 违反「层次靠底色差」，一片白块 |
| `preview-doc.css` 行内代码 | 白底 + `--color-text-primary` 字 | **白底浅字，直接读不出来** |
| `preview-doc.css` 引用块 | 白底 + `--color-text-secondary` 字 | 同上 |
| `::selection` 选区 | 白底 + text-primary 字 | 同上 |
| `Badge brand` / `Button tonal` / `SubAgentPanel` 状态 | 白底绿字 | 绿字压白底 |

修法：`themes/dark.css` 把这两档改为「**accent 混纸面底**」（与同文件里
`--color-primary-fixed-dim` 既有写法一致），保持同样的极淡染色语义而不反色；
`ink-green.css` 补上同名令牌以满足三套主题契约。

**顺带拆开了一枚语义混装的令牌。** 截图覆盖窗（恒暗底）此前取
`--color-accent-soft-strong` 当「暗底上看得见的亮线」—— 这与 soft 档「纸面上的淡染色」
语义相反，两者共用一个令牌必然有一处出错。新增 `--color-accent-line`
（accent 混白，三套主题取值一致，因为它的语义就是「暗底上的亮线」），
截图浮层改用它。这条「恒暗底浮层」的知识此前只写在 `screenshot-overlay.css` 的注释里
和 `components-layout.css:997` 的一处局部规避里（那里已经用 alpha 混合绕开），
**没有收口在令牌层** —— 这正是「事实单一归属」要治的病。

**门禁补强（把这次的教训变成机械断言）：**

1. `check-theme-tokens.mjs` 的 contrast 解析器此前只认 `#rrggbb` 与 `rgba()`，
   **不认 `color-mix()`** —— 于是所有以 color-mix 派生的底色被**静默跳过**，
   门禁显示全绿，实际根本没测。现在支持 `color-mix(in srgb, …)` / `var()` 递归 /
   `white`/`black`/`transparent`，并且**解析不了的配对会打印 WARN**（不再静默跳过）。
2. 新增 5 组 accent-soft 配对（accent 文字、text-primary、text-secondary × soft/soft-strong 底），
   三套主题共 34 组 × 3 = 102 次测量全部真的被测到，0 处跳过。

**走查覆盖的屏（每张都读图确认过）：** 浅色主界面（hero 标题可见、能力 chip 与建议行
无描边、输入区无描边 + 柔和投影）、设置页三个分类（通用 / 外观 / 模型）、添加模型两步弹窗
（遮罩 + 焦点壳 + provider 卡片）、添加 OpenAI 模型表单（**三个输入框全是浅灰底无描边**、
每字段带帮助文字 → `aria-describedby` 真的接上了）、暗色主界面、对话 markdown 排版
（行内代码 / 表格 / 列表 / 水平线）、右侧面板四个入口卡、工作目录浮层、文件浏览器。

`preview-doc.css` 在桌面端拿不到文件预览入口（地址栏不可输入、SendKeys 的焦点不在输入框），
改用 **headless Edge 加载真实 CSS**（`<link>` 真实 `dark.css` + 真实 `preview-doc.css`）截图 ——
行内代码与引用块在暗色下是深绿微染底 + 浅字，可读 ✓（修前是白底浅字）。

- **§8 的 7 条门禁一旦全部阻断**，CI 会从「绿」变成「红」——必须先完成对应收敛再开阻断。
  建议顺序：先开 `contrast` 的一半（只查 ink-green）→ 修完 → 全量开。
- **「组件层零调用即违规」这条会牵动 `ui/Card` 与 `ui/Badge`**：它们零使用不是错误，
  而是「清单里存在但没人要」——要么落地，要么从 `ui/index.ts` 移除。**保留一个没人用的原语，
  比没有这个原语更糟**：它假装体系里有一种统一做法。清理本身也是本篇的一项决定。
- 宪法 §十 承诺的 `@container` 迁移在本篇被标记为**未开始**（注释描述的是计划）。
  要么排期，要么改注释——两者不能都留着，否则下一个读注释的人会去找一条不存在的规则。
- 审计结论基于静态分析 + 令牌计算，**第四轮已补真机走查**（见上）。仍未实机核验的只剩：
  **图标视觉重量对比**（同一屏里多个图标的实际视觉重量）——它需要像素级比对，
  本轮只确认了「语义 → 字形」定稿表在代码层已统一。
  `impeccable detect` 与 `tsc --noEmit` 每轮都跑（各轮验证证据一节有连跑记录）。
