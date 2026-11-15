# Decision Note: 组件体系多轮优化方案（R1 收敛 → R4 打磨）

Status: proposed — 基于 `2026-10-16-component-inventory.md` 基线数字与 `2026-10-16-unified-design-system.md` 宪法，分四轮收敛；**每轮结束必须重盘一次，基线数字不下降即视为未完成**

## 实施记录（R1 批次 · 2026-10-16，R1 全部完成）

- **R1-1 起排线 ✓**：新增 `--indent-thread: 32px`（tokens.css）+ `pl-thread` 工具类（globals.css）；`TextBubble:191`、`StreamingBubble:225` 的 `pl-[30px]` 已替换（头像 22 + gap 10 = 32，修掉 2px 偏差）
- **R1-2 图标尺寸 ✓**：codemod 元素级转换 42 处（8/9/10→`icon-xs`、12→`icon-sm`、18/20→`icon-lg`）；**新增第四档 `icon-xs`(10px)** 并修订设计规范 §8（40+ 处微图标强拔到 12 会撑爆紧凑行）；剩余 8 处 `size={N}` 全为豁免的特殊尺寸（空态大图标/灯箱导航/头像）；TitleBar 五个窗口按钮 md/lg 混档 → 全 `icon-md`。⚠ 教训：codemod v1 对无 className 的自闭合标签产生了非法 `<Tag / className>`，repair 脚本修复 23 处后零残留——**批处理必须 dry-run 逐行审查后再落盘**
- **R1-3 页头 ✓（5 个视图全部收敛）**：新增 `ui/PageHeader`（h-56，icon+title+count｜中部切换｜右侧动作），AgentsView / AutomationView / AnalyticsView / CustomAppsView / VaultPanel 已接入；AutomationView 的 `text-2xl` 大字 tab 与 `uppercase tracking-wider` 已废除
- **R1-4 宽度与网格 ✓**：新增 `--width-page`(672) / `--width-wide-page`(896) + `max-w-page` / `max-w-wide-page` 工具类；AnalyticsView 补 896 容器；AutomationView 的 keep-awake/模板区移入同一容器（两层宽度体系消除）、网格统一 `grid-cols-1 md:grid-cols-2 gap-4`（单页 3 种→1 种）。AgentsView 的 1/2/3 列网格暂保留（数据为空时 3 列 auto-fill 无差异，R2 顺手收敛）
- **R1-5 空态 部分✓**：AgentsView / AutomationView / AnalyticsView(暂无数据) / CustomAppsView 4 处手搓 → `ui/EmptyState`（VaultPanel 的三元串空态在列表内联，形态不同，留待 R2）
- **R1-6 元信息字号 ✓**：`--text-tiny` 11→12px（含 `--font-size-tiny` 别名），功能文字下限统一 12px；tiny 与 meta 同值，新代码应用 `--text-meta`
- **顺带完成 R3-2 ✓**：Composer 停止/发送同位同尺寸（28→32px）、补 `aria-label`、清除 5 处 `var(--token, #hex)` 兜底字面值；`Button danger` 的 `brightness-110` → `color-mix` 令牌 hover
- **顺带完成 R3-4 ✓**：`ProcessTimeline` 折叠态迁移到 `chat/activitySummary` 的 `useDisclosure`（`disclosureKey` = message.id，TextBubble 已传入）；修复 `stepGroups` memo 缺 `streamComplete` 依赖；setter 调用点从函数式更新改为值更新（新 setter 签名不接受 updater）

**基线数字更新（对照清单第六节）**：起排线 5→1 ✓｜图标野值 67→8（全部豁免）✓｜页头 4→1 ✓｜宽度 4→2 档令牌 ✓｜网格 5→2（Agents 的响应式三列待 R2）｜空态 ≥8→待重盘｜弹窗/浮层/按钮/确认 未动（R2）

**验证（两批合计）**：`check:tokens` 契约 102/102 ✓、type-debt 0；`impeccable detect src` 退出码 0；IDE LSP/lints 对全部改动文件 0 错误 0 警告；`useDisclosure`/`PageHeader` 类型解析正确。**`tsc --noEmit` 因本机 node_modules 符号链接信任问题（os error 44）无法执行**——待环境修复后必须补跑一次

## 实施记录（WorkBuddy 对照批次 · 2026-10-16）

外部对照分析（WorkBuddy 双主题截图逐层拆解）提出三件事，全部落地：

- **空态 hero 档 ✓**：新增 `--text-hero: 40px`（@theme 资产档，注释说明 700 字重命中雅黑真粗体、中文不收字距）；`ChatView` 欢迎页 h1 `text-2xl tracking-tight` → `text-hero font-bold`。"一屏一主角"从此有排版支撑。
- **唯一重色纪律 ✓**：全量盘点 26 处实色 `bg-accent` + 4 处 style 内联，逐一定性后修正 17 处违规——
  - 选中态降级实色→`accent-soft`：AIGenerateWizard 选项 chips ×4（含步骤指示）、AutomationView cron/skill 选中 ×2、InlineExpertPanel 激活专家 ×1
  - 状态点改语义 `success`：ImChannelsCard 连接状态 ×5、BackendSettings tickLoop ×1、SecurityTab autoBackup ×1
  - `text-white` → `text-on-accent`：ModelAddDialog / ModelListTab / UpdatePanel ×3
  - **修复 2 处"文字隐形"**：ChatIndicators 队列提交钮、ClarifyBubble 提交钮误用 `--color-accent-on-text`（16% 淡染色）压在实色 accent 上——tokens.css 注释里 2026-10-05 记录过的同一 bug 模式，本次扫出 2 处残留并清零
  - 豁免保留（有语义理由）：AwakeningCard 进度刻度、UpdatePanel 进度条、AnalyticsView 图例色块（数据语义）；主行动按钮 10 处（VPanel/AddDialog/ListTab/Security/Advanced/BuiltIn/ImChannels/ChatIndicators/Clarify/Sidebar 确认钮）
- **侧栏元数据纪律 ✓（部分）**：`sidebar-row-meta` 已随 R1-6 升至 12px + 三级灰 + 右对齐（flex 布局保证）；`sidebar-tab-count` 的 `font-size: 10px` 字面值 → `var(--text-micro)`。版本号位置（头部 vs logo 下）与计数括号化属交互/布局决策，**留待用户拍板**，未擅动

**验证**：`check:tokens` 契约 102/102 ✓；`impeccable detect src` 0；IDE lints 全部改动文件 0 错误。`accent-on-text` 误用清零（0 处）。

## 实施记录（R3 收尾批 · 2026-10-16 晚）+ 验证方法论

- **disclosureKey 运行时崩溃（用户报）已修**：prop 加进了类型声明却漏了解构列表（ProcessTimeline + UnifiedTimelineRenderer 两处），纯浏览器复现的报错实为 **Tauri 边界**——`App.tsx:282 getCurrentWindow()` 需要运行时注入的 `__TAURI_INTERNALS__`，headless 浏览器必死，与改动无关
- **AnalyticsView 模块 500（自测发现）已修**：加闭合时编辑锚点撞上 DebugPanel 函数的同构结尾，2 个 `</div>` 加错位置。已归位，模块转换恢复 200
- **新增验证通道（环境受限下的替代）**：提权 IDE 的 dev server（:1420）读 node_modules 正常——①逐个 fetch 改动模块，转换 200 = 语法有效；②dump 应用 DOM 只能验证到首个 Tauri API 调用为止，之后**必须由 Tauri 窗口验证**。**IDE lints 对本项目不可靠**（解构漏项这种运行时崩溃它报 0），不能作为放行依据
- **教训**：多函数文件里"同构结尾"（`}）/n);`）是编辑锚点碰撞高发区——锚点必须带函数级唯一上下文；新 prop 必须 LSP hover 验证"类型↔解构↔调用点"三点闭合

## 实施记录（连续执行批 · 2026-10-16 深夜，8 项任务）

| # | 任务 | 结果 | 验证 |
|---|---|---|---|
| T1 | 欢迎语按会话稳定 | ✓ `pickStableGreeting`（sessionId 哈希），同会话不换句 | transform 200 |
| T2 | ui/Modal 加固 | ✓ `modal-scale-in` 入场、遮罩 backdrop-blur、`aria-labelledby`(useId)、焦点陷阱补 details/summary/contenteditable + 排除 disabled/不可见 | transform 200 |
| T3 | VaultPanel 确认归一 | ✓ 行内两步删除按钮（列表+详情两处）→ `ui/ConfirmDialog`，`confirmingId` 状态清零 | refs 0 + transform 200 |
| T4 | 资产去重 | ✓ 删 14 个文件：src/assets 6 个（**全目录零引用**，Avatar.tsx 过时注释修正）、public 8 个字节重复（favicon-16/32/48/180 与 Avatar 引用的保留）；Tauri 打包走 src-tauri/icons 不受影响 | 全库 0 悬空引用 |
| T5 | ProviderIcon 软底 | ✓ `softBg` 拼 hex8 → `color-mix`；兜底灰 `#6B7280` → `var(--color-text-faint)`；**新建 `themes/EXCEPTIONS.md`** 登记三处品牌色豁免与使用纪律 | transform 200，hex8 残留 0 |
| T6 | ToolTabs 拆分（第一步） | ✓ 私有 ConfirmDialog 删除（busy 态收编进 ui/ConfirmDialog）；`toolTabsData.ts` 独立模块（BUILTIN_PLUGINS/DEFAULT_PLUGIN_SETTINGS/CATEGORY_LABELS/CONNECTOR_ICONS/pickIconByName/parseMcpJson，零 React 依赖可单测）；清理 7 个未用导入。54KB→约 50KB，四 Tab 组件文件化留作 R2 下一步 | 两模块 200 + 契约 ✓ |
| T7 | MessageBubble 死分支 | ✓ 行为等价简化（冗余双 return → 单 return）。**修正盘点误判**：这不是死代码而是文档化的设计决策（待决定走抽屉/已决议不留痕），已按实际语义重写注释 | transform 200 |
| T8 | 统一终审 | ✓ detect 0｜契约 102/102｜type-debt 0｜12 个改动模块转换全 200｜改动文件 lints 0 | 全绿 |

**修正盘点结论**：组件清单 §二 中"MessageBubble 双 return null 死分支"一条作废——实为文档化设计决策；"已决议审批卡留痕"如要做属新功能（工具卡已有 approval-pill 承载决议结果），不再是"清死代码"。

**R2 剩余**（仍需 tsc 环境）：四 Tab 文件化、ChatView/ImChannelsCard 拆分、浮层 4→1、按钮单轨、SettingsGroup 采用率。R4 剩余：FileTypeIcon 97 处 hex 分批令牌化（已登记 EXCEPTIONS）、动效统一、125% 缩放走查。

## 实施记录（墨与纸 v4 · 2026-10-16 深夜第二波）

用户实测否决染色轴（"没看出大气，只看到欢迎字大了"）后，按 WorkBuddy 逐像素采样分析落全量：

- **tokens.css 亮色**：中性轴脱色（app #e3e8e4→#ececec、侧栏 #eaeeea→#f5f5f5、文字 #1f2622→#181818、边框/阴影/遮罩/玻璃全部中性化）；新增 `--color-text-hero: #000`；status chip 三色中性化
- **dark.css 整表重写**：画布 #141414 最深、外壳 #1f1f1f 反提（WorkBuddy 同向）；hero #fff；正文 #d6d7da→#e9e9e9；**清掉两处陶土红残留**（link #d07b52、focus-ring rgba(208,123,82)→品牌绿）
- **ink-green**：补 hero 令牌，保留绿相作为品牌主题（三主题分工：亮/暗纯中性、ink-green 带色）
- **组件层色相清退（约 25 处）**：侧栏选中行、AgentsView/AutomationView 分段选中、向导 chips ×4、InlineExpertPanel 激活、IM 徽章 ×3（ChatView/Sidebar ×2）、AgentCard 徽标、ToolTabs 高亮、workbench 当前项/停靠手柄、prefetch/thought-stream hover 边框、tool-card 运行徽章×2、PageHeader 图标座、pane-resizer —— 全部转中性；accent 仅存：主/次行动按钮、链接、焦点环、Badge brand、Button tonal
- **验证**：契约 **103/103**（hero 令牌进 CONTRACT）✓｜detect 0｜18 个改动模块转换全 200（ChatView/Sidebar 首测 FAIL 为 HMR 竞态，复测通过）｜lints 0
- **契约快照**：CONTRACT 从 102→103（+text-hero）——这是本轮唯一令牌增项

## 实施记录（WorkBuddy 空态批 · 2026-10-16 深夜第三波）

用户确认墨与纸 v4 后，按第三份逐元素采样分析落空态层：

- **去吉祥物，留空** ✓：欢迎区 140px 吉祥物删除，hero 独当焦点（留白即大气，不补任何装饰）；`/mascot-256.png` 仅剩启动 splash（index.html，非 UI 层）在用
- **建议行** ✓：4 条快捷入口（差异审查 / 会议纪要 / 9 点日报自动化 / 新人指南落盘）——**内容全部对应 Hermix 真实能力**，点击 `setInput` + 聚焦输入框；样式 = 左对齐自适应宽度圆角矩形（radius-lg 非药丸）+ 右侧 ↗，bg-card/hover 抬色
- **发送钮去强调色** ✓：空态 `bg-quaternary`、有内容 `inverse-surface`——**永不变绿**，主行动靠位置表达（WorkBuddy 规律 4）；队列角标同步中性化
- **文字 Logo** ✓：TitleBar 品牌区删吉祥物图版，纯文字 "Hermix"（升 text-primary 满对比）；版本号由侧栏头部承担不重复；`.titlebar-logo` CSS 清除
- **模型 chip 彩色小 logo**：核实已有 ProviderIcon（分析允许的两个彩色点之一），零改动
- **Composer 三层结构**：现状已是"输入 / 操作行（含模型选择器）"，底部副条（工作空间·权限独立变暗条）属结构性重排，记为 P2 未动

**验证**：detect 0｜契约 103/103｜三模块转换 200｜lints 0。

## 实施记录（WorkBuddy 空态批 II · 能力行 + Composer 三层 · 2026-10-16）

- **暗色失效修复**：dark.css 整表重写时选择器误写双冒号 `::root[` → 整条规则被浏览器丢弃、暗色静默失效为浅色。已修；**校验器新增 Check 1a**（主题文件必须精确包含 `:root[data-theme="<id>"]`，此前只数令牌的盲区堵上）
- **能力 chip 行** ✓：独立应用 / 工作流 / 代码开发 / PDF 转换 / 文档编写 / 图片处理（全单色 lucide 图标 ≤16px），点击**填入能力专属 prompt 模板**并聚焦输入框——对齐"每个点击都会加载不同能力"的要求，用的是真实机制（setInput + 模板）
- **场景切换药丸** ✓：办公 / 开发两枚独立药丸（非连体），接**真实机制 domainMode**（办公=auto 自动识别 / 开发=钉代码域），选中态中性提亮
- **Composer 三层材质** ✓：工作区 + 权限两个 chip 从工具行**移入新底部副条**（行级手术迁移 133 行，带断言）；副条 = 1px 分隔线（与外框描边共用 `--color-border-primary`）+ **渐变底**（`linear-gradient` 从 app/card 混合色到 card，非平涂）；发送钮 `rounded-lg` → **圆形**，且恒为中性（inverse/quaternary），永不变强调色
- **domain 按钮中性化** ✓：编码模式激活态从 accent 色改为 text-primary（选择态不占用强调色）
- **chip 令牌分层** ✓：模型文字满对比（#181818/#e9e9e9），工作区/权限副条文字极暗（#6f6f6f/#a3a3a3）——对齐参考的"副条文字极暗"纪律
- **免责微文案** ✓：欢迎页底部居中"内容由 AI 生成，请核实重要信息"（12px faint——参考规格 10-11px，取仓库可读性下限 12px，有意偏差）
- **验证**：detect 0｜契约 103/103｜type-debt 0｜Composer/ChatView 转换 200｜lints 0
- **遗留 P2**：副条 chip 尚无小图标（spec 有 图标+文字+箭头 形制）；场景药丸的"设计"第三档无对应机制未加

## 实施记录（弹层纪律 + R2 拆分 · 2026-10-16 晚间）

- **弹层纪律落地 ✓**（WorkBuddy 设置弹层/用户菜单逐像素采样）：`ui/Modal` 去遮罩去描边、圆角 24→17px，背景保持原亮度靠亮度差浮起；`--shadow-modal` 重定义为底部单向投影（大 y 偏移 + 负扩散，亮/暗双主题）；用户菜单弹层 bg elevated + 同款投影 + 去描边 + min-width 220px；危险项（重新连接）前加独立分隔线
- **R2 ToolTabs 拆分（三步，tsc 解锁后全程编译器把关）**：
  - 第一步 `toolTabsData.ts`（常量表 + parseMcpJson）
  - 第二步 `toolTabsShared.tsx`（SearchBar/SummaryBar/Section/Grid/EmptyHint/SkeletonGrid，136 行）
  - 第三步 `SkillCard.tsx`（SkillCard + 内容阅读弹窗，182 行）
  - **ToolTabs 1330 → 913 行（-31%）**，每步 tsc exit 0 + detect 0
- **验证**：全部门禁绿；tsc-unlock 基础设施已提交（05a505bb）

## 实施记录（R2 ToolTabs 拆分收官 · 2026-10-16 深夜）

- **God Object 退役 ✓**：ToolTabs.tsx **1330 → 8 行 barrel**（-99%），实现分布七个文件：
  `PluginsTab.tsx`(148) / `SkillsTab.tsx`(188) / `ConnectorsTab.tsx`(310) / `ToolsetsTab.tsx`(276) / `toolTabsShared.tsx`(142) / `toolTabsData.ts`(常量+解析器) / `SkillCard.tsx`(技能卡+阅读弹窗)
- SettingsShell 导入路径不变（barrel 再导出，零调用方改动）
- 方法论：行级手术脚本 + 断言 + **tsc 当依赖分析器**（搬移后按 TS6133/TS2304 精确修导入，三轮收敛）
- 验证：tsc exit 0｜detect 0｜契约 103/103×2
- R2 剩余：ChatView（53KB）与 ImChannelsCard（60KB）同法拆分、弹窗浮层归一、SettingsGroup 采用率

## 实施记录（R3 剩余小项批次 · 2026-10-16）

- **R3-5 部分 ✓**：**修掉「批量审批恒显示中风险」**——批量项自带 `risk` 字段，现按「最险者胜」推导（`ApprovalDrawer.tsx`，reduce + rank 表）；`MessageBubble:113-116` 的双 `return null` 死分支**未动**（已决议卡片是否留在流里属交互决策，待用户拍板）
- **R3-3 ✓**：`MessageActions` 空正文（纯工具轮）不再整行消失——保留行容器避免间距塌陷，只隐藏对空内容无意义的复制/赞踩；重新生成与时间戳仍可用
- **R3-7 部分 ✓**：`blockquote` 边框从背景变量改 `--color-border-secondary`；`file-search.css` 表头/结果行两份 grid-template 合一（列骨架单点定义）；代码块复制钮常驻**未做**（hover 显形是刻意的低打扰设计，改常驻需用户拍板）
- **R3-8 部分 ✓**：`ModelStatusBar` 的 `bg-green-400/red-400` → `bg-success/bg-error`；欢迎语随机改稳定**未做**（行为变更）
- **验证**：detect 0、契约 102/102、type-debt 0、改动文件 lints 0

**R3 剩余（需拍板或 R2 先行）**：审批决策面二合一（涉及交互重构）、`content-visibility` 保留策略微调、欢迎语稳定性、VaultPanel 11 处确认归一。**R2 / R4 整轮未动**（见上：R2 需 tsc 环境，R4 含需拍板项）。

## Decision

优化按"先去重、再拆结构、再补体验、最后打磨"的顺序推进。**顺序不可颠倒**：在 6 套弹窗并存时做视觉打磨，等于把同一件事打磨六遍。

### 优先级矩阵（影响面 × 成本 → 排期）

| 项 | 影响面 | 成本 | 轮次 |
|---|---|---|---|
| 起排线统一 / 图标尺寸收敛 | 全站 | 低 | R1 |
| 页头 / 宽度 / 网格 / 空态 / 确认 收敛 | 全站 | 中 | R1–R2 |
| 弹窗与浮层归一 | 全站 | 中 | R2 |
| 拆 God Object（ToolTabs/ChatView/ImChannelsCard） | 局部但深 | 高 | R2 |
| 状态补全（hover/focus/disabled/loading） | 全站 | 中 | R3 |
| 审批一致性、过程折叠持久化 | 对话区 | 中 | R3 |
| 品牌 hex / 资产去重 | 图标层 | 中 | R3–R4 |
| 动效、密度、对齐的最后打磨 | 全站 | 低 | R4 |

---

### Round 1 · 收敛（把 5 套变 1 套，不动结构）

目标：只统一"值"，不碰组件结构，风险最低、收益最大。

- **R1-1 起排线**：新增 `--indent-thread: 32px`；`TextBubble:191`、`StreamingBubble:225` 的 `pl-[30px]` → `var(--indent-thread)`；`.process-line-body` 21px、`.tl-reasoning-row` 18px 同步对齐
- **R1-2 图标单轨**：67 处 `size={N}` 全部落到 `icon-sm/md/lg`（`size={10}` 的 31 处 → `icon-sm`）；TitleBar 五个窗口按钮统一 `icon-md`；`Loader2 size={8}`、`Pin size={10}` 一并收敛
- **R1-3 页头**：新增 `PageHeader`（h-56：icon + title + 计数 | segmented | 至多一个 accent 主动作），替换 AgentsView / AutomationView / AnalyticsView / CustomAppsView / VaultPanel 的 4 种写法；`uppercase tracking-wider`（AutomationView:276）彻底删除
- **R1-4 宽度与网格**：新增 `--width-page`(672) / `--width-wide-page`(896)；AnalyticsView 与 CustomAppsView 补容器；全站网格统一 `repeat(auto-fill, minmax(280px,1fr))`（AutomationView 单页 3 种 → 1 种）
- **R1-5 空态**：5 个页面视图的手搓空态 → `ui/EmptyState`（基线：共享组件仅 9 处 → 目标 ≥20 处）
- **R1-6 元信息字号**：功能文字 11px → 12px；`text-micro` 仅留角标

**验收**：起排线值 5→1；图标野值 67→0；页头形态 4→1；网格 5→1；空态变体 ≥8→1。重盘更新清单数字。

---

### Round 2 · 拆结构（去掉重复实现，补缺失原语）

- **R2-1 补齐 L0 缺失原语**：Tooltip、Popover、Menu、Tabs、Progress、Skeleton、Avatar、Divider、Alert、Breadcrumb、Checkbox、Radio、Slider（12 个）——这是"6 套弹窗"的根因
- **R2-2 弹窗归一**：`ui/Modal` 补入场动画（复用 `.modal-scale-in`）、backdrop-blur、`aria-labelledby`、焦点陷阱补 `details/summary/[contenteditable]`、z 阶入表；删除 `ToolTabs` 私有 `ConfirmDialog`；各页手写弹窗改用统一件（基线 6 套 → 1 套）
- **R2-3 浮层归一**：4 套浮层 → 1 套 `PopoverShell`（统一 z 阶、Esc、焦点归还、定位策略）
- **R2-4 拆 God Object**：
  - `ToolTabs`(54KB) → `config/plugins/`、`config/skills/`、`config/connectors/`、`config/toolsets/` 四个 Tab 文件 + 共享件
  - `ChatView`(53KB) → 4 个早退分支抽出 `ChatEmptyState` / `ChatErrorState` / `ChatRestoring`；`<Composer>` 的 30+ props 收敛为一份调用
  - `ImChannelsCard`(60KB) → 按渠道拆分为多个子组件
- **R2-5 按钮单轨**：CSS `.btn*` 类（components-controls.css §8、`.wb-btn`）全部改走 `ui/Button`，无法直改的登记迁移清单（基线 2 套 → 1 套）
- **R2-6 设置页原语化**：`SettingsGroup/SettingsRow` 采用率 3/12 → 12/12；`FileAssocTab` 从 `SettingsShell.tsx:456` 拆出成独立文件

**验收**：弹窗 6→1；浮层 4→1；按钮 2→1；SettingsGroup 采用率 25%→100%；三个 God Object 均 <25KB。重盘。

---

### Round 3 · 体验（状态、可达性、正确性）

- **R3-1 状态补全**：`Button` 补 selected/pressed；`danger` hover 的 `brightness-110` → 令牌；`Input` 补 focus ring（与 `.input:focus` 一致）；`TextArea` 补 `size` 档；`IconButton` 类型层强制 `label`
- **R3-2 停止/发送跳版**：停止与发送**同位同尺寸**（32px），停止补 `disabled` 与 `aria-label`；修正"注释说放输入框右上角、实现在工具栏"的实现偏差
- **R3-3 hover-only 可达性**：`MessageActions` 与 `.code-copy-btn` 补 `focus-within`/常驻低对比；`MessageActions:74` 空正文不得整行消失（保留占位高度，与 CSS `min-height:24px` 对齐）
- **R3-4 过程折叠持久化**：`ToolCallBubble:141` 的 `userOverride` 迁到 `chat/activitySummary.ts:174` 的 `useDisclosure`；补 `stepGroups` 的 `streamComplete` 依赖；解决 `content-visibility` 回收丢状态
- **R3-5 审批一致性**：抽屉与内联统一为一套决策面（建议内联 2–3 项 + "查看全部"进抽屉）；**修掉批量恒 `caution`**；清 `MessageBubble:113-116` 双 `return null` 死分支；批量列表两处实现合一
- **R3-6 破坏性确认归一**：`confirming` state（WorkbenchView 10 / McpCatalogGrid 13 / VaultPanel 11 / ToolTabs）→ `ui/ConfirmDialog`
- **R3-7 内容渲染**：`blockquote` 边框改用 `--color-border-*`；`file-search.css` 两份 grid-template 合一；代码块复制钮常驻；系统消息截断补 title/展开
- **R3-8 对话区细节**：`ModelStatusBar` 的 `h-10` 与 `bg-green-400/red-400` → 令牌；工作台会话不渲染该行导致的 40px 跳动用占位解决；`ChatView` 欢迎语随机改为按会话稳定取值

**验收**：破坏性确认 4→1；审批决策面 2→1；无 hover-only 不可达操作；过程折叠态滚动不丢；重盘。

---

### Round 4 · 打磨（最后 10%）

- **R4-1 动效**：全部入场统一 `opacity + translateY(8px)` / base 160 / ease-out；补 `prefers-reduced-motion` 分支；删除残留 bounce
- **R4-2 密度与对齐**：侧栏行 30、设置行 40、日志行 28 三档落地；chip 的 max-width 统一 160（模型 chip 的 `max-w-narrow` 改为固定值并纳入 compact 折叠）；修掉"已附加 N 个"重复渲染两遍
- **R4-3 图标与资产**：`ProviderIcon` 软底改 `color-mix`（禁拼 hex8）；`FileTypeIcon` 的 97 处 hex 逐步令牌化（先登记 EXCEPTIONS，再分批替换）；品牌资产去重（`src/assets` 6→4、`public/` 5 组合并）
- **R4-4 死代码清理**：`data-compact` 悬空属性（要么让 CSS 消费它、要么删）；`TextBubble` 用户气泡 `max-width:80%` 死规则；`ClarifyBubble` 死 pill；`sidebar` 版本号与 user card 的注意力竞争（版本号移入 tooltip）
- **R4-5 视觉走查**：在桌面端（真实窗口、125% 缩放）逐屏走查一次，专查三条：发丝线是否消失、染色灰是否发闷、缩放是否出条纹

**验收**：`impeccable detect` 全库 0；`check:tokens` 全绿；125% 缩放下无发丝线消失与带状条纹。

---

### 每轮统一验收（四轮都要跑）

1. `npm run check:tokens`（契约/孤儿/裸色/色阶/字号）
2. `tsc --noEmit`（直接调二进制，勿用 `pnpm tsc`）
3. `npx -y impeccable@4.1.0 detect src/`
4. 重盘基线数字并更新 `2026-10-16-component-inventory.md`
5. 桌面端实机走查（缩放 100% / 125%）

## Alternatives considered

- **一次性大重构**：否——143 个 tsx 同时改无法验证，且用户无法分批验收；四轮每轮都可独立发布
- **只做 R3/R4（跳过收敛）**：否——不先去重，体验修补要在 6 套弹窗里各做一遍，成本翻六倍且必然遗漏
- **先拆 God Object 再统一值**：否——结构变动期改值会产生大量冲突与回归，先定值再动结构
- **用 codemod 全自动替换图标尺寸**：部分可行，但 `size={10}` 的语义并非全是 icon-sm（少量是头像/图表字号），需人工判定后批量——列为 R1-2 的执行手段而非替代判断

## Consequences

- 总工期长：四轮覆盖约 143 个文件，**建议按轮提 PR**，每轮独立可回滚
- R2 拆 God Object 是唯一高风险项：需先补测试或至少锁定行为快照（`ToolTabs` 的 346 个符号节点）
- R3-4（折叠持久化）涉及状态管理改动，可能暴露既有 bug（注释已承诺迁移但代码未做），需单独验证
- 每一轮的"验收"都包含**重盘数字**，意味着文档维护是硬性成本，不是可选的收尾
- 若中途停止，至少 R1 已把"5 套变 1 套"的观感收益拿到手；R1 是收益/风险比最高的一轮
