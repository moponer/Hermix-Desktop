# Decision Note: Hermix 统一设计规范（组件层宪法）

Status: proposed — 与 `2026-10-16-component-inventory.md` 配套：清单回答"有什么"，本文回答"该长什么样"

## Decision

所有组件遵守同一套宪法。**宪法优先于组件现状**——现存的 6 套弹窗、5 种起排线、4 种页头都是违反本宪法待收敛的对象，不是可参照的先例。

### 一、分层与职责边界

```
L0 原语（ui/）   → 无业务语义，只管状态与几何
L1 组合          → 用 L0 拼装，可跨页复用（TaskCard、SessionRow、ApprovalCard）
L2 页面          → 只做布局与编排，不得自带控件样式
L3 覆盖          → 浮层，必须走统一的 Portal + z 阶 + 焦点归还
L4 视觉单元      → CSS 类承载，只消费令牌，不得声明私有色值
```
硬规则：**L2 不得出现 `bg-[var(--color-…)]` 以外的色值**；**L4 不得出现字面 px 色值**；**L0 不得知道业务概念**（如"会话"）。

### 二、令牌纪律（沿用并加严）

1. **改值不改键**：切换主题只换取值，`@theme` 里的键名是硬性契约
2. **新增色彩令牌必须三处同步**：`tokens.css` + `themes/dark.css` + `themes/ink-green.css`（校验器会自动失败）
3. **组件禁裸色**：tsx 内不得出现 `#hex`；图标品牌色走 `themes/EXCEPTIONS.md` 登记
4. **结构性令牌（字号/间距/圆角/动效/几何）跨主题恒定**，主题文件禁止覆盖

### 三、颜色

> **2026-10-16 墨与纸 v4（WorkBuddy 逐像素采样对照后定稿，推翻 2026-10-15 染色轴）**：
> 亮/暗两套主题的中性轴回到**纯中性灰**（R=G=B），绿意只保留在 ink-green 主题。
> 四条硬规律（采样反推）：
> ① **组件层零品牌色**——accent 只给主行动按钮、链接、焦点环；选中态/chip/徽章/状态点全部中性；
> ② **hero 满对比**——空态大标题纯 #000/#fff（新令牌 `--color-text-hero`），正文降一档；
> ③ **接缝靠发丝线不靠深缝**——亮色窗底 #ececec（原 #e3e8e4 大幅变浅），暗色画布 #141414 最深、外壳 #1f1f1f 反提；
> ④ **阴影零色相**。
> 附带修正：dark.css 的链接色与 focus ring 仍是陶土红残留（#d07b52 / rgba(208,123,82)），本次清零。

**四级墨**（全部 ≥4.5:1 压在本主题最深案面）：primary / secondary / tertiary / faint。
代价说明：AA 达标会压缩灰阶，**层级因此主要靠字号与字重承担**，不再靠"更灰"。

**唯一重色纪律（2026-10-16，源自 WorkBuddy 对照分析）**：实色 accent 只允许出现在**每屏的主行动按钮**上；选中态一律 soft/tint 档；连接/运行等**状态语义一律走 success/warning**，不得借用品牌绿；装饰性圆点一律中性灰；进度条与图表刻度属数据语义，可走 accent（豁免登记）。违规样例已全部修正：AIGenerateWizard 选中 chips ×4、AutomationView cron/skill 选中 ×2、InlineExpertPanel 激活专家 ×1（实色→accent-soft）；ImChannelsCard 连接状态点 ×5、BackendSettings/SecurityTab 运行点 ×2（accent→success）。

**面阶**（纸最亮 → 卡 → 案 → 底），方向统一为"抬起"：

| 角色 | 语义 |
|---|---|
| `--color-bg-primary` | 纸·对话与主区底（最亮） |
| `--color-bg-card` | 面·卡片 |
| `--color-bg-secondary` | 案·侧栏/顶栏 |
| `--color-bg-app` | 底·窗底/列间缝（最深） |

**语义四色**（success/warning/error/info）色相彼此拉开 ≥30°，且**不与品牌绿同区**；品牌绿只给"主行动 / 当前选中 / 需要你决定"三件事。

**桌面端呈现纪律**（WebView2/Windows 专用，写进每条取值决策）：
1. 相邻明度级差 **≥4%**（125%/150% 缩放不吃掉层次）
2. 面间**亮度比 ≥1.06**，不靠 1px 发丝线单独分隔（重采样会吞掉发丝线）
3. **全实色，不用 alpha 叠层**（缩放重采样易出带状条纹）
4. 不用纯黑纯白（Windows gamma 2.2，纯黑阴影发死）
5. 阴影一律中性偏品牌色、低扩散（`0 1px 2px` 最稳），禁用大扩散彩色光晕

### 四、字排

沿用已落地的 7 档字号（`--text-micro…display`）、4 档行高、3 档字重；新增角色约束：

**hero 资产档（2026-10-16，源自 WorkBuddy 对照分析）**：`--text-hero: 40px` + 700——空态首屏标题专用的一次性资产档，是全屏唯一的视觉焦点；不进正文档位系统，中文不收紧字距（负字距负优化结论同样适用于 40px）。

| 内容 | 字号 | 字重 | 颜色 |
|---|---|---|---|
| 用户意图（对话） | title 16 / display 20（空态） | 600 | primary |
| 对话正文 | body 14.5 | 400 | primary |
| 过程/思考 | ui 13 | 400 | thinking |
| 工具名 | meta 12 mono | 600 | primary |
| 参数/路径 | tiny 11 mono | 400 | tertiary |
| 区块标题 | meta 12 | 600 + 字距 .04em | tertiary |
| 按钮 | ui 13 | 500 | 按 variant |
| 元信息/计数 | tiny 11→**统一 12** | 400 | faint |

**硬性修正**：功能文字**不得小于 12px**（现行 11px 在高分屏/125% 下偏小；`text-micro` 只留给角标计数）。

### 五、间距与几何（8px 网格）

- 页面容器 `px-6 py-5`；组间 24；组内 12；卡片内 16
- **控件高度档**：24（chip/小钮）/ 30（侧栏行）/ 32（按钮与表单）/ 40（标题栏、状态条）/ 44（设置行）
- **行高**：侧栏 30、设置行 40、日志行 28
- **起排线统一为一个令牌** `--indent-thread: 32px`（现行 30/32/21/18 四个值全部废止）
- **内容宽度两档**：`--width-page` 672（常规）、`--width-wide-page` 896（宽页/数据）；对话正文 `--width-reading`

### 六、圆角 / 描边 / 阴影 / 动效 / z 阶

| 维度 | 取值 |
|---|---|
| 圆角 | sm 6（角标）/ md 10（控件）/ lg 13（内卡）/ xl 17（卡片）/ 2xl 20（输入区、弹窗）/ full 999 |
| 描边 | 与所压底色亮度比 ≥1.10；**禁用单侧色条**（side-tab 反模式），语义靠整圈描边、染底或图标色 |
| 阴影 | card / paper(输入区) / pop(浮层) / btn(主按钮) 四档，中性偏墨绿 |
| 动效 | 时长 fast 120 / base 160 / slow 240；曲线 `cubic-bezier(.16,1,.3,1)`；**禁 bounce**；只动 transform 与 opacity，禁动画布局属性 |
| z 阶 | resizer 10 / sticky 20 / drawer 40 / popover 50 / modal 60 / toast 9999——**禁止即兴值** |

### 七、状态模型（每个可交互组件必须齐备）

`rest / hover / active / focus-visible / disabled / loading / selected / error`

硬性要求：
1. **每一态必须有可感知差异**，hover 与目标色同值视为失效（已修 `Button primary`）
2. **focus-visible 必须存在且可见**：统一 `2px accent ring + 2px offset`
3. **disabled 必须表达"不可用"而非"加载中"**：实底按钮走填色档，幽灵按钮走降透明度
4. **hover-only 的操作必须补 `focus-within`**（触屏与键盘可达），空内容时不得整行消失（应保留占位高度）
5. **loading 带 `aria-busy`**，且不得因文案替换导致宽度跳变

### 八、图标

- **尺寸单轨三档 + xs 微档**（2026-10-16 修订）：`icon-xs 10 / icon-sm 12 / icon-md 14 / icon-lg 16`。xs 专用于 11px 文本行内的状态微图标（spinner/状态点/行内删除钮）——实测 40+ 处 9-10px 微图标强拔到 12 会撑爆紧凑行，故补第四档。字面 `size={N}` 野值已全部收敛（42 处转换）；`size={22/26/28/32/36/48/72}` 属 tokens 注释豁免的特殊尺寸（空态大图标/灯箱导航/品牌），保留；**同组按钮不得混档**（修 TitleBar 的 md/lg/md/lg）
- **语义图标一律 `currentColor`**；品牌色（`FileTypeIcon`、`ProviderIcon`）走 `EXCEPTIONS.md` 登记，但其**软底必须改用 `color-mix`**，禁止字符串拼 hex8（不支持 CSS 变量、不可暗色适配）
- **品牌/mascot 资产去重**：`src/assets` 6→4、`public/` 5 组重复合并，统一引用入口

### 九、交互与可达性

- **破坏性动作唯一模式**：`ui/ConfirmDialog`。现存的 `confirming` 局部 state（WorkbenchView 10、McpCatalogGrid 13、VaultPanel 11、ToolTabs）与 ToolTabs 私有 ConfirmDialog 全部收敛
- **键盘**：弹窗 Esc 关闭 + 焦点归还；审批支持数字键选择；列表支持上下键
- **对比度**：正文 ≥7:1、次要 ≥4.5:1；**不再新增 <4.5:1 的灰字**
- **reduced-motion**：所有动效有降级分支
- **空态唯一组件**：`ui/EmptyState`（3 档 size × 3 variant），禁止手搓

### 十、响应式

四档**等效视口**（vw / zoom）：≥1400 / 1140–1399 / 900–1139 / <900，每档三列有明确行为（见 `2026-10-15-cockpit-workspace-architecture.md` §5 补丁 P-1 的副屏规则仍适用于右栏）。主列分档从 JS（`computeMainMinWidth`）渐进迁移到 `@container`。

## Alternatives considered

- **放宽到"两套主题各自定层级"**：否——跨主题一致性是本项目的核心资产，暗色/墨绿只是取值表
- **允许 11px 元信息**（现行）：否——桌面端 125% 缩放下 11px 已是下限，`--text-tiny` 保留但功能文字统一 12px
- **图标保留字面尺寸（作者更自由）**：否——双轨体系已经造成 95% 野值，单轨才有约束
- **破坏性确认允许多种（按场景选择）**：否——用户无法学习四种确认方式，一致性优先于场景适配

## Consequences

- 本宪法与现状差距很大：**6 套弹窗→1 套、5 种起排线→1 个、4 种页头→1 种、67 处野值图标→0**。收敛期会有大量文件改动，必须分批（见优化方案文档）
- 严格的"每态可见"要求会暴露一批**缺失状态**（如 Modal 无入场动画、IconButton 无 pressed），这些是待办不是可选
- `text-tiny`(11px) 从功能文字降级为仅角标，`check-theme-tokens.mjs` 的 `type-debt` 规则需同步收紧
- 品牌图标去重要动 `public/`，涉及打包产物，需在 `ensure-sidecar` 之外的构建校验里加一条资产唯一性检查
