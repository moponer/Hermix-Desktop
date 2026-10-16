# Decision Note: 全应用逐界面布局重设计方案（P0-P2 分批实施）

Status: proposed — 覆盖全局骨架、侧栏、对话主区、右栏、设置页与四个二级页的布局规范；待拍板后按批实施

Archived: 2026-10-15 — 增量式优化方案已被推翻，取代者见
[2026-10-15-cockpit-workspace-architecture.md](../../proposed/feature/2026-10-15-cockpit-workspace-architecture.md)
（用户判语："我还不如现在呢……我要的是全新的布局架构"）。本文档冻结，仅作历史论证。

## Decision

### 0. 全局规范（先立规矩，各界面照抄）

**内容宽度统一为 4 档**（现状 6 种并存，是"每页左缘对不齐"的根源）：

| 档位 | 令牌 | 值 | 用途 |
|---|---|---|---|
| prose | `--width-reading` | clamp(820px, 68vw, 1240px) | 对话正文（已有，不动） |
| page | 新增 `--width-page` | 672px（=max-w-2xl） | 设置常规页 |
| wide | 新增 `--width-wide-page` | 896px（=max-w-4xl） | 设置宽页 / Automation / Analytics |
| full | — | 100% | 仅数据表格、看板类，须注明理由 |

**页头统一 PageHeader 组件**（替代现有 4 种形态）：

```
┌──────────────────────────────────────────────────────────┐ h-14 (56px)
│ [icon 16] 标题(text-title/600) [计数badge]   [segmented]  [次动作] [主动作/accent] │
└──────────────────────────────────────────────────────────┘
```
- 左：定位信息（我是谁、有多少）；中：视图切换；右：动作，**每页至多一个 accent 实底按钮**。
- 页头下不画线，靠下方内容的组间距（24px）分层。

**间距节奏（8px 网格）**：页面容器 `px-6 py-5`；组间 `space-y-6`（24）；组内 `space-y-3`（12）；卡片内 `p-4`；列表行高 30px（侧栏）/ 40px（设置行）。

**信息三层模型**：页头=定位、主体=内容、浮动层=状态与审批。色相只给主行动/选中/语义；导航与容器保持中性。

### 1. 全局骨架（App Shell）

保留：TitleBar 40px 三区网格、三列 grid、主区通铺（2026-10-12 定）、8px 案面缝。

改动两点：

a. **右栏 overlay 化（<1140 等效视口）**：现状是"装不下就强制关闭"，产物直接不可见。改为右栏变 absolute 浮层（宽 min(420, vw−主列最小宽)）+ scrim，可手动关。右栏从"随窗口消失"变"按需浮起"。

b. **主列分档走 container query**：`computeMainMinWidth()` 的 4 档 JS 分档迁移到 `@container`（`.pane-main` 为 container），渐进执行，行为不变。

### 2. 侧栏（280px，可拖 200-360）

```
sidebar
├─ head 40px：[搜索框 flex-1] [新建＋]        ← 改：搜索框上移并入 head，
├─ 快捷导航一行（新建对话/定时任务/技能）          省掉独立搜索行 32px；
├─ tabs（对话/项目/聊天室 segmented）             版本号移入 user-card tooltip
├─ 置顶区（新增：pinned 会话，≤5 条，分隔线）
├─ section list flex-1 滚动
│   ├─ section header（折叠箭头+计数，hover 显＋）
│   └─ 行 30px：选中=accent 14% 淡染 / hover=中性 bg-hover
└─ user card 40px（头像+名+齿轮）
```

**新增折叠态 icon rail（64px）**：900-1139 等效视口自动进入；垂直排列 logo→新建→三个 tab 图标→底部头像；hover 行项弹 260px 浮层列表（渐进披露）。<900 侧栏整体改 overlay 抽屉 + scrim。折叠入口从 ModelStatusBar / SidebarRevealRow 二选一的现状，统一收敛为 rail 上的一个展开钮，状态信息不再迁移。

### 3. 对话主区（ChatView）

固定三层，自上而下：

```
chat-surface (flex-col, overflow hidden — 空态与主形态统一)
├─ ModelStatusBar 40px（渲染条件收敛：仅承担动作行）
├─ MessageList flex-1 滚动
│   └─ max-w-reading 居中列 px-4 py-5 space-y-5
│       ├─ 用户行：右对齐气泡（accent 14% 底 / radius-2xl / 右上角）
│       ├─ 助手行：22px 头像列 + 平铺无底正文（纸面直接读）
│       │   └─ 续条缩进 pl-[32px]（修正现状 30px 的 2px 偏差）
│       └─ 系统行：居中 faint 一行
└─ 底部栈（重排，见下）
```

**底部栈收敛**（现状 5-6 层堆叠、高度抖动是最大体验伤）：

```
├─ ApprovalDrawer（absolute bottom:100% 上弹，保留）
├─ 状态带（新）：压缩横幅 + 用量 合并为同一行 — "92% · 128k · 压缩中…"
│   位置：composer 卡上缘内侧（toolbar 左端 tiny 文字），不再独占一行
└─ Composer 卡（radius-2xl，聚焦态品牌绿描边已落地）
    ├─ 附件行 / 队列行（条件，border-t 分隔）
    ├─ textarea
    └─ 工具栏：chip 统一 max-width 160px；<640 由 container query 折叠为
       纯图标（替代 JS ResizeObserver）
```

空态：吉祥物 140px + 欢迎语 + 3 条建议 prompt chip + composer，容器 overflow 与主形态统一为 hidden。

### 4. 右栏 PreviewBar

- strip 40px 保留；tab 关闭热区 icon-sm→20px；tabs 溢出加滚动渐隐遮罩（`mask-image` 线性渐变），替代"隐藏滚动条且无提示"。
- 空态 2×2 卡网格改为 `grid-template-columns: repeat(auto-fit, minmax(220px, 1fr))`，max-w-360 居中，宽栏不再大片留白。

### 5. 设置页（SettingsShell）

- 左导航 232 保留；顶条改"← 设置 / 当前页名"面包屑（信息密度从 2 字提到可用）。
- 内容宽度：常规页 672（page 档）、runtimes/devices/security/diagnostics 896（wide 档）；**砍掉 max-w-5xl**；ModelListTab 补 PageHeading，与其余页同节奏。
- 分组统一 `SettingsGroup`(title+desc) > `SettingsRow`(左文右控件)，行高 40px。

### 6. AgentsView

- 页头换 PageHeader：标题+计数 | segmented(我的/内置) | 主动作「新建」唯一 accent。
- 卡片网格 `repeat(auto-fill, minmax(280px,1fr))`（与 CustomApps 同一网格语言）。
- 创建/编辑从"整页替换+右上角返回"改为 **drawer 480px 右侧滑入**，返回路径与内容同居一处。

### 7. AutomationView

- 大字 tab（text-2xl）改 segmented（与 Agents 同语言）。
- **全内容收进同一 max-w-4xl 容器**（现状 keepAwake 条与模板区挂在容器外，两层宽度）；keepAwake 与模板并入 SettingsGroup 卡。
- 编辑 modal-card-lg 改 drawer 560px（长表单不再受 max-h 90vh 压迫）。

### 8. AnalyticsView

- 从唯一通栏页改为 wide 档 896 容器。
- 统计卡 `repeat(auto-fit, minmax(180px,1fr))`，6 卡在窄列自动 2×3/3×2。
- 硬编码 tailwind 色板（text-green-400 等）改语义 token——顺带清 palette-debt。

### 9. CustomAppsView

- 工具行改两栏：搜索 flex-1 + 右侧刷新/视图切换；删内联 `margin-right:8px` 与 `var(--color-accent, #b8860b)` 硬编码兜底。
- 网格已 auto-fill 280，保留为全应用网格基准。

### 10. VaultPanel

- 行内 4+ 个 14px 动作钮收敛：常驻「显示密码」+ `…` overflow 菜单（复制用户名/复制密码/删除）。
- 删除确认统一为 modal（废弃行内二次点击 confirmingId 模式，与 CustomApps 一致）。
- 两栏布局保留，列表列 min-width 320。

### 11. Workbench 浮窗

- 浮窗内 ChatView（variant="workbench"）：正文去 max-w-reading（440px 浮窗全宽阅读）；composer compact 折叠改 container query。
- 浮窗 440×560 / 停靠 min 360 保留；两套坐标记忆（比例/像素）维持现状不改。

### 12. 视觉引导与交互分区（全局）

- **z-index 语义阶梯**（统一登记）：resizer 10 / drawer 40 / popover 50 / modal 60 / toast 9999；新代码不得出现 999/9999 字面值以外的即兴值。
- **动效**：入场统一 opacity + translateY(8px)，duration-base + ease-out；禁 bounce（已有规范）。
- **破坏性动作模式**：入口 hover 显现，确认一律 modal，两处不一致（Vault 行内确认 / CustomApps modal）收敛为后者。
- **焦点**：全局 focus-visible accent ring（已落地），键盘路径完整保留。

### 13. 响应式总表（等效视口 = vw / zoom）

| 等效视口 | 左栏 | 主列最小宽 | 右栏 |
|---|---|---|---|
| ≥1400 | 280（可拖 200-360） | 580 | 640（可拖 360-840），常驻 |
| 1140-1399 | 240 | 480 | 默认关；开启时 ≤420 常驻 |
| 900-1139 | 64 icon rail（hover 浮层） | 400 | overlay 420 + scrim |
| <900 | overlay 抽屉 | 360 | overlay |

（主列分档数值沿用现有 `computeMainMinWidth`，仅迁移实现方式。）

## Alternatives considered

- **主列恢复卡片化圆角**：否——2026-10-12 已定通铺，卡片隐喻只留右栏，不翻案。
- **侧栏默认 icon rail**：否——280 默认宽是 2026-10-15 刚定的（对齐 CodeBuddy 参考），rail 只做窄视口档。
- **右栏 <1140 直接 overlay**（而非"默认关+可选 overlay"）：中间档保持常驻开关，只有更窄才强制浮层，避免中档窗口频繁 scrim。
- **统一 drawer 引第三方库**：否——不引依赖，用现有 modal/absolute 模式派生（六步阶梯第 4 步）。
- **消息流加宽到 720**：否——640≈50 汉字是行长上限约束，加宽伤阅读。
- **底部状态堆栈全部移入 composer**：否——ApprovalDrawer 需要贴输入框上方的强提示位，保留 absolute 上弹。

## Consequences

- 需新增 `--width-page` / `--width-wide-page` 容器令牌 + PageHeader / Drawer 两个通用组件（一次性成本，之后每页省结构代码）。
- icon rail 与右栏 overlay 是**新交互行为**，需用户验收，不可静默上线。
- 分批边界：
  - **P0（零风险对齐，可一次提）**：宽度令牌统一、PageHeader 替换 4 种页头、30px 缩进修正、Analytics 色板逃逸清理、CustomApps 内联样式清理、PreviewBar 空态/热区。
  - **P1（行为变更）**：composer 底部栈收敛、侧栏搜索上移、Analytics 容器化。
  - **P2（新交互）**：icon rail、右栏 overlay、Agents/Automation drawer 化、Vault 动作收敛。
- 现有 localStorage 记忆宽度/开合的键不受影响（只动表现层）。
- container query 迁移期间 JS 分档与 CSS 分档并存，需以 feature 检测防双跑。
