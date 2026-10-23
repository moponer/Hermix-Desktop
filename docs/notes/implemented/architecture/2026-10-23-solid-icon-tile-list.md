# Decision Note: 列表行改用「实心彩色方块 + 反白字形」图标

Status: implemented — 共享层已新增 `.cat-tile` 与 `--color-on-tone`，设置页 / 配置页 / 导航的列表行图标已批量改实心底；侧栏与欢迎页的偏淡文字同步提级

## 背景

参照样式是一份**设置项清单**：每行左侧一个**圆角方块色块**（饱和色填充），
块内是**反白线性字形**，右侧是中文标题，行按语义分组。

拆成可实现的参数：块 28px（圆角 `--radius-sm`）、字形 14px、底色取项目既有
8 档身份色 `--color-tone-*`、字形前景取新增令牌 `--color-on-tone`。

**关键差距**：项目原有的 `.cat-icon` 是**反过来的**——中性底
（`--color-bg-tertiary`）+ 饱和色字形（`packages/preview/src/styles/icon-tone.css`）。
所以这不是"给现有图标染个色"，而是新增一种形态：**色相落在块上，而不是落在细字上**。

仓库内已有目标样式的先例：`ImChannelsCard.tsx` 的 32px 品牌实心底 + `text-on-accent`
白字形。本轮实心块与它对齐，不另起第二套语言。

## Decision

1. **共享层新增 `.cat-tile`**（`packages/preview/src/styles/icon-tone.css`），与
   `.cat-icon` 并存：底 `var(--cat-tone)`，字 `var(--color-on-tone)`，尺寸沿用
   28 / 36 / 48 三档，不新增尺寸档；`.cat-tile > svg { stroke-width: 2 }`
   （实底上的描边要比彩字版粗，否则小尺寸下发虚）。
2. **新增令牌 `--color-on-tone`**：浅色 / ink-green = `#ffffff`，深色 = `#12151c`。
   只加 1 个令牌，不为每套主题另养 8 档 solid 色（`tokens.css` 明确反对"每套主题
   多维护 8 条"）。
3. **实心块只承载身份色（8 档 tone）+ 语义四色不承载**。语义四色做实底时，白字在
   深色主题只有 2.30~3.28:1、在 ink-green 的 warning 上只有 2.46:1（不达 3:1），
   因此语义色继续走 `.cat-icon`（中性底 + 彩字）。
4. **React 原语 `IconTile`**（`components/ui/IconTile.tsx`）：把
   `cat-tile cat-tile--{size} cat-tile--{tone}` 这串类名与"字形尺寸随块尺寸走"
   的规则收敛到一处，调用方不判断。
5. **`SettingsRow` 新增可选 `icon` 插槽**（`components/ui/SettingsRow.tsx`），
   由调用方传 `IconTile`。
6. **判据：只有"功能 / 能力型"的行给图标，参数型行不给**。参数型行（数值输入、
   下拉、状态表）配图标等于几个同义图形，信息量为零。
7. **设置左导航参与实心块**（见 [与既有决策的冲突](#与既有决策的冲突)）；
   **主侧栏 5 项不加块**（行高 30px 与会话行的基线网格是既有决策），
   改为把功能入口的字色提到 primary。
8. **`.cat-icon` 本身语义不变**，继续由头像（字母）与品牌 logo 消费。

## 实测：对比度

WCAG 1.4.11（非文本内容）要求图形与背景 ≥3:1；1.4.3（文字）要求 ≥4.5:1。

**实心块上的字形（图形，3:1）**

| | 浅色主题 白字 | 深色主题 白字 | 深色主题 深墨 |
|---|---|---|---|
| 8 档 tone 区间 | 3.17 ~ 4.82 ✓ | 2.00 ~ 2.61 ✗ | 6.99 ~ 9.14 ✓ |

这就是 `--color-on-tone` 必须分主题的原因，不能图省事全用白字。
这 8 档 × 3 主题的配对已写进 `scripts/check-theme-tokens.mjs` 的 contrast 规则，
把"写死白色"从一次肉眼发现变成一次门禁报错。

**文字（4.5:1）**

| 对象 | 改前 | 改后 | 说明 |
|---|---|---|---|
| 侧栏功能入口 `.nav-item` | 6.73:1（secondary） | 16.29:1（primary） | 入口是主操作，不是次级说明 |
| 侧栏会话标题 `.sidebar-row` | 6.73:1 | 16.29:1 | 会话名是侧栏正文；时间戳仍为 muted |
| 侧栏视图 tab `.sidebar-tab` | 5.86:1（muted） | 6.73:1（secondary） | 次级但可读 |
| 药丸 / 筛选 chip `.chip-flat` | 6.27:1（secondary） | 15.17:1（primary） | 控件文字要"读得清才算能用" |
| 命中率语义色（AnalyticsView） | 约 1.8:1（`text-green-400` 等原色） | 5.20 / 6.12 / 5.86:1（`--color-text-*` 文字安全档） | 原色在浅纸面上几乎读不出来 |
| 头像字母（AgentAvatar） | 2.71 ~ 4.12:1 | 4.84 ~ 6.23:1 | 色相 70% + 主题墨 30% 现算 |

**为什么字号这么小也敢提**：这些数值都达 WCAG AA，"偏淡"来自**同色系相邻**
（浅灰底 #ededed 配灰字 #565656，明度差小），不是色值不达标。所以修法是
**定向提级**（把"正文级"的东西升到正文级），而不是全局改令牌——全局改会
毁掉四级墨的语义分层。

## 已交付清单

**共享层**

- `packages/preview/src/styles/icon-tone.css`：`.cat-tile` + 8 档色相变体
- `apps/desktop-tauri/src/styles/tokens.css` / `themes/dark.css` / `themes/ink-green.css` / `apps/viewer/src/styles.css`：`--color-on-tone`
- `apps/desktop-tauri/src/components/ui/IconTile.tsx`（新）+ `components/ui/index.ts`

**设置页功能行（`IconTile` + `SettingsRow` 图标插槽）**

| 页面 | 组 | 行 | 图标 | 色调 |
|---|---|---|---|---|
| GeneralTab | 输入 | 发送快捷键 | Keyboard | amber |
| GeneralTab | 系统 | 开机自启动 | Power | slate |
| GeneralTab | 系统 | 显示在菜单栏（托盘） | PanelTop | slate |
| GeneralTab | 系统 | 关闭按钮行为 | LogOut | slate |
| GeneralTab | 系统 | 回复完成时通知 | BellRing | amber |
| GeneralTab | 数据 | 数据目录 | FolderOpen | green |
| AppearanceTab | 强调色 | 主题默认色 | Palette | violet |
| SecurityTab | 备份 | 启用自动备份 | ShieldCheck | amber |
| VoiceTab | — | 开启手机端语音识别 | Mic | orange |
| BackendSettingsTab | — | 后端状态 | Server | blue |
| BackendSettingsTab | — | TickLoop 后台心跳 | Activity | cyan |
| DevicesTab | — | 渠道标题 / 设备行 | Building2·MessageCircle·Smartphone | slate |
| RuntimesTab | — | Node / Python / Git | FileCode2·FileCode2·GitBranch | green·blue·orange（品牌主色） |
| ArchiveTab | — | 归档会话行 | Archive | slate |
| ModelListTab | — | 模型行 | ProviderIcon（中性底块内） | 品牌色 |
| MemoryTextPanel | — | MEMORY.md / USER.md | FileText / Users | green / rose |
| DailyMemoryPanel | — | 目标 / 进展 / 结论 | Target / TrendingUp / CheckCircle2 | amber / cyan / green |
| DiagnosticsTab | — | 日志目录 | FileText | cyan |
| SettingsShell（截图页） | — | 启用系统截图 | Camera | orange |

**已有色块改为实心底**：`ToolCardIcon`（插件 / 技能 / 工具集 / 连接器 / MCP 五页共用）、
`ChoiceCard`、`TwinView` 栏目标题、`AutomationView`（经 `ToolCardIcon`）。

**导航**：设置左导航 18 项改实心块（取用早已定义、却一直未被渲染的 `PageDef.tone`）；
"通用"与"高级"此前共用 `SlidersHorizontal`，后者换成 `FlaskConical`。

**选择卡色调改身份色**：外观三选与工作模式二选此前用 success / warning / info，
违反本项目自己的"语义四色表达状态、身份色表达类目"纪律，也导致实底白字在暗色
主题下不达标。现为 浅色=amber / 深色=violet / 跟随系统=slate / 编程开发=blue /
日常办公=green。

**统计卡**（AnalyticsView）：6 张卡的裸图标改实心块；命中率那栏的语义色从
Tailwind 原色（`text-green-400` 等，约 1.8:1）换成 `--color-text-*` 文字安全档。

**文字对比度**：`.nav-item` / `.sidebar-row` 提到 primary，`.sidebar-tab` 提到
secondary，`.chip-flat` 提到 primary；欢迎页的"办公 / 开发"药丸与能力 chip 从
手写底色+字色归并到 `chip-flat` 类族（新增 `chip-flat--md` 32px 档）。

## 未采纳的清单项（及原因）

| 清单项 | 决定 | 原因 |
|---|---|---|
| AgentAvatar 头像改实心底 | 不改，改压暗字形 | 字母是**文字**：8 档色相里没有任何单一前景能让字母在所有实底上过 4.5:1（白字在 amber/orange 上 3.49/3.17，深墨在 slate/violet/cyan 上约 3.8）。改用"色相 70% + 主题墨 30%"现算，8 档全过。 |
| ProviderIcon 改实心品牌底 | 不改，模型行改 28px **中性**底块 | 品牌 logo 自带多色 SVG，铺在同色实底上是"logo 压 logo"；中性底 + 品牌 logo 才是行业惯例。 |
| 文件关联行加块 | 不加 | 该行标题里已有 `FileTypeIcon`，再加一块就是双图标。 |
| ComputerTab 视图切换加块 | 不加 | 那是三个图标按钮，不是清单行。 |
| PersonasPanel / DailyMemoryPanel 的列表行加块 | 不加 | 行高只有 30px（与基线网格绑定），28px 块放不下。 |
| 主侧栏 5 项导航加块 | 不加 | `.nav-item` 的 30px 行高与会话行同高是 2026-09-30 明确记录的基线决策；实心块会把行推到 40px，整栏错位。改为提字色。 |
| 语义四色实心底 | 不做 | 见 Decision 第 3 条。 |

## 与既有决策的冲突

**设置左导航去色（2026-10-23，`SettingsShell.tsx`）** —— 那条决策的理由是
"18 项彩色底会变成一条彩虹"。现在改回实心块，理由有三条：

1. 用户明确要求参照该样式；
2. 18 项的色相**不是随机彩虹**：每个页面的色相按类目定（偏好组
   slate/rose/violet/orange，插件组 slate/violet/blue/cyan，管理组
   blue/cyan/slate/orange/amber/green/slate/rose，诊断组 cyan/slate），
   同组内反复出现两三种色，扫视时是"有条理的彩色"；
3. 侧栏原本是"灰底 + 灰字"，加色块后视觉重心回到左侧导航。

块用 28px（sm）而非 36px：44px 高的行里 36px 的块会压过文字。选中态仍靠行底色
+ 文字墨色区分，块色不参与。

**`icon-tone.css` 原有的"色相只出现在图形上"论证** —— 该论证针对"淡染底"，
与"实心底"不是同一件事。本轮保留的底线：色块只承载"这是什么"，不承载状态；
状态语义继续走 `--color-success / warning / error / info`。

**`ToolListRow` 的"36px 色块压过标题"教训** —— 列表行统一 28px，网格卡片 36px；
设置行行高从 44px 抬到 52~56px。

## 顺带修掉的既有问题

- `themes/dark.css` 与 `themes/ink-green.css` 缺 `--color-bg-selected` /
  `--color-bg-hover-subtle`（`tokens.css` 的 @theme 契约里有）→ **main 上
  `check-theme-tokens` 的 contract 规则本来就是红的**，本轮补齐后转绿。
- `check-theme-tokens` 的孤儿引用检查只扫 `apps/desktop-tauri/src/styles`，
  看不到共享包 `icon-tone.css` 里定义的 `--cat-tone`，会把引用它的桌面样式
  误报成孤儿 → 改为同时把共享包的 CSS 计入"已定义"。

## Alternatives considered

**改 `.cat-icon` 本身而不是新增 `.cat-tile`** —— 否决。`.cat-icon` 被头像与品牌
logo 消费，语义是"中性底 + 彩字"；改它等于把现有视觉一起推翻。两种形态并存、
各用各的，成本最低且可单独回退。

**深色主题另立 8 档 deep solid 值配白字** —— 否决。要维护 16 条主题色且必然与
本体漂移；实测深墨字形 6.99~9.14:1 已完全够用。

**参数型行也配图标** —— 否决。6 个数值参数配 6 个同义图形，信息量为零。

## Consequences

**得到**

- 设置页从"一列文字 + 右侧控件"变成"一列可扫视的能力图标"；
- 与参照样式一致，且与仓库内 `ImChannelsCard` 品牌块统一；
- 色相全部复用既有 8 档 `--color-tone-*` 与 `lib/categoryTone.ts`，不新增颜色系统；
- 实心块的对比度从"靠自觉"变成门禁强制（8 档 × 3 主题）。

**付出**

- 深色主题下色块是"暗底提亮档"的亮色块 + 深墨字形，视觉偏亮；
- 设置行行高 44 → 52~56px，一屏可见行数减少约 15%；
- `--color-on-tone` 必须 desktop 与 viewer 两侧同步定义，漏一边即静默失效
  （`var()` 解析失败 → 整条声明被丢弃 → 图标突然全灰但不报错）。

**回退成本**：低。`.cat-tile` 是并列新增类，`.cat-icon` 一行不动；任何一处不满意，
去掉该处的 `cat-tile` 类即可回到现状。

## Reintroduction conditions

不适用（本决策是新增视觉能力，不是删除既有能力）。
