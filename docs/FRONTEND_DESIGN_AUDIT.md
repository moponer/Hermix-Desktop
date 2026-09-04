# 前端设计全面审查报告

> 审查日期：2026-08-30
> 审查范围：`apps/desktop-tauri/src/` 全量前端（组件 + 样式 + 主题 + token）
> 判据：qiaomu-design skill（AI 反套路规则 / engineering-checklist / chinese-typography / assets-and-readability / carbon-foundations）+ 项目铁律 P-39~P-43
> 状态：**只审查，未改任何代码**

---

## 一、执行摘要

| 项目 | 规模 |
|---|---|
| 组件文件 | 173 个（114 在 components/），48,271 行 TSX/TS |
| 样式文件 | 12 个 CSS，12,509 行 |
| 主题 | 4 套（light / dark / rainforest / sunset） |
| 设计令牌 | CONTRACT 有效令牌 72 个 |

**一句话结论**：不是"某个组件丑"，而是**四层系统性失控**——主色违反禁蓝紫铁律、字号体系崩塌、暗色双机制打架、token 治理缺位（校验器从未运行过）。

### 四个根因

| 根因 | 表现 | 影响面 |
|---|---|---|
| **① 主色踩红线** | light 的 accent 是 indigo `#4f46e5`，dark 整条 accent 是 violet | ~130 处组件消费 + 42 条令牌 |
| **② 字号体系崩塌** | tokens 定义 6 档，实际用了 **18 种**，含 10.5/11.5/12.5px 半像素怪值 | **561 处** `text-[Npx]` |
| **③ 暗色双机制打架** | `.dark` 类已废除却被 `useTheme.ts` 继续挂载，**169 条 `.dark` 规则**覆盖 `themes/dark.css` | 全站暗色不可控 |
| **④ token 治理缺位** | 校验器 `check-theme-tokens.mjs` 从未接入 CI；**562 处裸色值**、**29.6% 死代码** | 全站 |

---

## 二、关键量化指标

| 指标 | 数值 | 判读 |
|---|---|---|
| 偏蓝紫令牌 | **42 条**（light 16 / dark 24 / rainforest 2 / **sunset 0**） | sunset 是唯一干净主题 |
| 组件消费 accent | ~130 处（`text-accent`×47、`bg-accent`×32、`border-accent`×19） | 换一行 token 全局生效 |
| 字号种类 | **18 种 / 561 处**（含 9.5/10.5/11.5/12.5/13.5px 半像素） | 半像素在 Windows 非整数 DPI 下发虚 |
| 裸色值 | **562 处**（447 hex + 115 rgba），其中 **278 处是 `var(--x, #hex)` 硬编码兜底** | 兜底色多为暗色假设，与浅色冲突 |
| 死代码 | **233 个类 / 29.6%**；approval 区块整块复制 **115 行** | components.css 39%、prose 39%、controls 46% |
| `!important` | **134 处**（code-view.css 独占 **95**，占 71%） | 体系性失控 |
| `.dark` 规则 | **169 条** vs 已立 `themes/dark.css` 为真源 | 两套值互不感知 |
| 重复定义类 | **287 个**跨文件重复；853 组完全重复声明行 | 改一处漏两处 |
| emoji 当图标 | **40 处**（高危 9 + 装饰 28 + 其他 3） | 违反 P-40 |
| 硬编码宽度 | **17 种取值 / 39 处** | 违反"UI 宽度需统一" |
| 图标尺寸 | **19 种取值** | 11/12/13/14 混用 |
| 圆角 | **8 种**，含越界的 `rounded-2xl`(16px，token 上限 12px) | 接缝可见 |
| `transition-all` | **25 处** | 动画化布局属性 = 位置漂移元凶 |
| duration 档位 | tokens 定义 3 档，实际**引用 0 处**，全靠 Tailwind 默认 150ms | 令牌形同虚设 |
| 对比度不达标 | **6 个组合**（dark/rainforest 的 `text-faint`，最低 **3.32:1**） | WCAG 要求 ≥4.5:1 |
| 无 aria-label 图标按钮 | **9 处** | a11y 基础缺失 |
| `cursor-pointer` | 仅 18 处 vs 395 个 `onClick` | 可点元素无指针反馈 |
| 有 loading 态的组件 | **20/100（20%）** | 80% 组件无加载反馈 |
| 中文半角标点 | **56 处** | 文案不规范 |
| 装饰性渐变 | 组件层 **0 处** ✅；样式层 **24 处** | 样式层需清 |

---

## 三、P0 阻断项（必须改）

### P0-1 · 主色违反禁蓝紫铁律（P-39）

| 位置 | 值 | 问题 |
|---|---|---|
| `tokens.css:111` | `--color-accent: #4f46e5` | indigo-600，B 显著高于 R/G |
| `tokens.css:118,121` | `--color-link: #4f46e5`、`link-soft-hover: #3730a3` | 同上 |
| `themes/dark.css:50,51,53,54,57,58,75,86,88` | `#7c5cf5`/`#8f74ff`/`#9c85ff`/`#b3a2ff`/`#c3b4ff` | **violet 整条**，9 条高危 |
| `themes/dark.css:12,18,29,36,37,38` | `#2c2c34`/`#2a2a33`/`#3a3a44`/`#383842` | 中性轴建立在 slate 而非 zinc |
| `CompactionStatusBanner.tsx:117/124/131/162` | `text-sky-700 bg-sky-50 border-sky-200` | sky 蓝；且 4 处值完全相同 = 四状态无区分 |
| `agents/types.ts:50,56` | `violet: #6d5ad6`、`indigo: #5b58c9` | Agent 卡片 banner 大面积蓝紫 |
| `base.css:66` | `var(--color-accent, #475569)` | focus ring 兜底是 slate-600 |

**建议**：accent 轴换暖中性重音色（琥珀棕 `#8a5a1a` / 或 teal）；dark 中性轴改纯 zinc（R=G=B 偏差 ≤2）；sky → token 语义色；violet/indigo 键保留但替换色值（避免破坏已存数据）。

**ROI 最高**：一行 token 改动 → ~130 处组件全局生效。

### P0-2 · 字号体系崩塌

18 种字号并存，tokens 只定义 6 档。分布：
`12px`×128、`13px`×105、`11px`×87、`10px`×64、**`10.5px`×62**、**`12.5px`×54**、**`11.5px`×34**、`14px`×12、`15px`×10、**`9.5px`×5**、`8px`×1、`9px`×2、`18px`/`20px`/`28px` 各 1。

**问题**：半像素值在 Windows 非整数 DPI 下渲染发虚、行高错位；8/9/9.5px 低于可读下限；相邻字号差 0.5px 视觉无法感知，只制造层级噪声。

**建议**：砍到 5 档（11 meta / 12 body-sm / 13 body / 15 h2 / 18 h1），半像素全部归整；层级改由**字重（400/500/600）+ 颜色（primary/secondary/tertiary/muted）**承担，而非靠字号砸。

### P0-3 · 暗色双机制打架（P-42）

`tokens.css:11-13` 明确声明 `.dark` 已废除（改用 `:root[data-theme="<id>"]`），但：
- `useTheme.ts:23-24` 仍在 `root.classList.toggle("dark", wantDark)`
- **169 条 `.dark` 规则**散落各 CSS（components.css:2955-3235、helpers:591-915 等）
- `PreviewPanel.tsx:217` 用 `classList.contains("dark")` 判断 → **永远 false** → 深色下代码高亮走亮色配色

结果：暗色令牌先被 `themes/dark.css` 赋值，再被 169 条 `.dark` 规则就地覆盖，两套值互不感知。

**建议**：逐条把 `.dark` 规则的值迁进 `themes/dark.css` → 确认无视觉差异 → 删 `.dark` 规则 + `useTheme.ts:23-24` + `globals.css:23` 的 `@custom-variant dark`。

### P0-4 · token 校验器从未运行

`tokens.css:19` 声明"校验器：`node scripts/check-theme-tokens.mjs`"，但该文件**零引用**于任何 npm script / CI。所谓"完整性/孤儿/裸色三项"检查从未执行——这直接解释了 562 处裸色值能长期堆积。

**建议**：`apps/desktop-tauri/package.json` 加 `check:tokens` script 并挂进 `scripts/ci-gate.sh`。**这是成本最低、优先级最高的一步**（防止后续修复引入新问题）。

### P0-5 · 组件错位三件套

| 问题 | 位置 | 现象 |
|---|---|---|
| 开关 4 套手写实现 | `config/ToolTabs.tsx:335-338` / `819-822` / `1078-1081` / `1243-1246` | 滑块 16/20/24px 三种尺寸、行程三种，并排看停位不齐；已有 `ToggleSwitch.tsx` 却未引用 |
| `.preview-pane` 141 条后代规则 | 散落 `code-view.css` + `components.css` + `components-prose.css` | 改动必漏 |
| `code-view.css` 95 处 `!important` | `code-view.css:54-57, 94, 99-131` | 压制 Tailwind 而非解决层序 |

**建议**：开关全量替换为 `<ToggleSwitch>`；`.preview-pane` 统一迁 `code-view.css`；`!important` 改用 `@layer components` 或 `:where()` 降特异性。

---

## 四、P1 影响观感

| # | 问题 | 位置 | 建议 |
|---|---|---|---|
| P1-1 | 宽度 17 种 / 39 处 | 全局 | 收敛 4 档令牌：`--w-xs/sm/md/lg` |
| P1-2 | 圆角 8 种，含越界 `rounded-2xl`(16px) | `Sidebar.tsx:1054/1132/1181` 等 9 处 | 收敛 3 档（sm=4/lg=8/xl=10），2xl→xl |
| P1-3 | 图标尺寸 19 种 | 全局 | 定 3 档（12 行内 / 16 卡片 / 20 空态） |
| P1-4 | emoji 当图标 40 处 | 高危 9 处见 B3 | 换 lucide 内联 SVG |
| P1-5 | 对比度不达标 6 组 | dark/rainforest `text-faint` 最低 3.32:1 | dark `#6d6d78`→`#81818c`；rainforest `#66796c`→`#728678` |
| P1-6 | 阶梯契约自相矛盾 | light 的 hover/active 比 app 更暗；dark 的 card < secondary | 统一一份阶梯定义并写进 checker |
| P1-7 | 裸色值 562 处 | helpers/prose/inline-approval 为 Top3 | 先补主题令牌覆盖，再删兜底色 |
| P1-8 | 死代码 233 类 | components 135 / prose 100 / controls 43 | 分批删，每批跑视觉回归 |
| P1-9 | 287 类跨文件重复 | `.btn`/`.modal-*`/`.tw-msg-*` 各 3 处 | 按职责唯一化归属 |
| P1-10 | 装饰性渐变 24 处 | `components.css:3036` 与 `helpers:715` 逐字节相同 | 改纯色 + 明度阶梯 |
| P1-11 | `transition-all` 25 处 | 全局 | 改 `transition-colors`/`transition-transform` |
| P1-12 | 无 Badge/Chip 共享组件 | 22 个文件各自手写胶囊 | 抽 `<StatusChip tone size>` |
| P1-13 | 语义类低覆盖 | `.btn` 13/100、`.card` 19/100、`.input` 4/100 | 新代码强制走语义类 |
| P1-14 | 中文半角标点 56 处 | `ModelSettingsTab:393` 等 | 全角化 + lint 规则 |
| P1-15 | 加载态 4 种叫法 | `加载中`8/`处理中`5/`正在加载`2/`请稍候`2 | 收敛 2 种 |
| P1-16 | `base.css` 职责越界 | Toast 全套 156 行塞在 reset 文件里 | 拆 `components-toast.css` |

---

## 五、P2 打磨项

- **P2-1** 命名三套并存：`tw-msg-*` / `msg-*` / `tool-*` 与 `tool-msg-*` / BEM `__` → 统一 kebab + BEM
- **P2-2** 注释与实现打架：`tokens.css:11` 说 `.dark` 已废除 vs `useTheme.ts` 仍挂载；`components.css:2380` 说 "No gradient" 紧邻处就是 `radial-gradient`
- **P2-3** 重复声明 853 组；`font-family: ui-monospace` 硬编码 53 处（应用 `var(--font-mono)`）
- **P2-4** `components-prose.css` 2848 行里 prose 只占 27 个选择器，名不副实
- **P2-5** `prefers-reduced-motion` CSS 有 13 处，TSX 内 **0 处** → 加 `useReducedMotion()` hook
- **P2-6** duration 令牌三档 0 引用 → 注册为 Tailwind `duration-fast/base/slow`
- **P2-7** `cursor-pointer` 18 处 vs 395 个 onClick → `base.css` 加 `[onclick], [role="button"]` 兜底
- **P2-8** 空态文案 31 处措辞不一（`暂无`/`还没有`/`无数据`/`空空如也`）→ 抽 `<EmptyState>` 三件套
- **P2-9** 9 处图标按钮无 `aria-label`
- **P2-10** `z-index` 9 个取值无规范 → 定 5 档 token
- **P2-11** 阴影 4 套含 Tailwind 原生 `shadow-2xl`/`shadow-xl` → 统一 `shadow-pop`/`shadow-modal`
- **P2-12** `EditProfileDialog.tsx:86-236` 整个弹窗用内联 `CSSProperties`，与 Tailwind 体系割裂
- **P2-13** 图表硬编码 `width=720 height=220` + `w-full h-auto` → 窄容器下坐标轴文字缩到不可读
- **P2-14** `EXCEPTIONS.md` 16 条例外中 8 条（50%）是"可自愈技术债却长期挂账"
- **P2-15** 响应式断点仅 30 处 / 100 组件

---

## 六、已合规项（不用动）

| 项 | 状态 |
|---|---|
| CONTRACT 令牌覆盖 | dark / rainforest / sunset **均 72/72**，缺失 0、孤儿 0 |
| `colorScheme` 声明 | 4 套主题**全部匹配**，无误判 |
| sunset 主题 | **0 条**蓝紫令牌，唯一干净主题 |
| 组件层装饰性渐变 | **0 处** ✅ 符合 P-41 |
| 负 margin 偏移 hack | **0 处** |
| 品牌识别色（FileTypeIcon / ProviderIcon） | 合理豁免，不算硬编码 |
| `StreamErrorBanner.tsx:57/64` | 错误码未泄漏给用户，UI 显示中文「网络中断/请求超时」，错误码仅进日志 —— **错误文案正面样板** |
| `inline-thinking-dot.css:119` 的 linear-gradient | 属动画 mask 实现，可保留 |

---

## 七、完善方案（分阶段路线图）

> 原则：**P-43 存量优化走打磨，不推倒重来**。每阶段独立可验收，失败可回滚。

### 阶段 0 · 治理基建（约 0.5 天）
1. `package.json` 加 `check:tokens` script，挂进 `scripts/ci-gate.sh`
2. 跑一次全量校验，产出「缺失令牌 / 孤儿令牌 / 裸色值」基线报告
3. 后续所有阶段以该报告为准，**防止边改边坏**

**验收**：`pnpm check:tokens` 在 CI 中运行，基线数字入库。

### 阶段 1 · 换色（约 1 天，ROI 最高）
1. `tokens.css:111` accent → 暖中性重音色；`:118` link 跟随；`:131` info → 中性石墨
2. `themes/dark.css` accent 轴 9 条 → 非蓝紫 hero；中性轴 6 条 → 纯 zinc
3. `CompactionStatusBanner` 4 处 sky → token 语义色（顺便拉开四状态明度）
4. `agents/types.ts` violet/indigo 色值替换（**保留键名**避免破坏已存数据）
5. 全局扫剩余的 42 条偏蓝紫令牌，逐条归位

**验收**：全站 0 条 HSL 的 B 明显高于 R/G 的令牌；sunset 主题视觉无回归。

### 阶段 2 · 收字号与间距（约 1.5 天）
1. 字号砍到 5 档，561 处中半像素与越界值（9.5/10.5/11.5/12.5/13.5/8/9px）全部归整
2. 层级改由字重 + 颜色承担，不再靠字号差
3. 图标尺寸定 3 档（12/16/20），19 种 → 3 种
4. 圆角 8 种 → 3 档，`rounded-2xl` 降级
5. 宽度 17 种 → 4 档令牌
6. 裸 px（2402 处）逐步归入 spacing scale

**验收**：`text-[Npx]` 中半像素值归零；同类组件字号/图标/圆角/宽度完全一致。

### 阶段 3 · 修暗色与错位（约 1.5 天）
1. `.dark` 169 条规则逐条迁进 `themes/dark.css`，确认无视觉差异后删除
2. 修 `useTheme.ts:23-24` 停止挂载 `.dark`；`globals.css:23` 删 `@custom-variant dark`
3. `PreviewPanel.tsx:217` 改读 `document.documentElement.dataset.theme`
4. 修 6 个对比度不达标组合（dark/rainforest 的 `text-faint`）
5. 开关 4 套 → 全量 `<ToggleSwitch>`
6. `code-view.css` 95 处 `!important` → `@layer` / `:where()` 降特异性
7. `.preview-pane` 141 条规则统一迁 `code-view.css`

**验收**：暗色主题下无亮色残留；开关并排停位对齐；`!important` 数量下降 80%+。

### 阶段 4 · 组件与文案收敛（约 2 天）
1. emoji 40 处 → lucide 内联 SVG（先高危 9 处，再装饰 28 处）
2. 抽 `<StatusChip tone size>`，替换 22 个文件的手写胶囊
3. 抽 `<EmptyState>/<LoadingState>/<ErrorState>` 三件套，统一空态文案
4. `transition-all` 25 处 → 具体属性
5. duration 令牌三档注册为 Tailwind 工具类并替换
6. 加 `useReducedMotion()` hook，JS 动画分支读取
7. 中文标点 56 处全角化 + lint 规则
8. 加载态 4 种叫法 → 2 种
9. 补 9 处图标按钮 `aria-label`；`base.css` 加 `cursor-pointer` 兜底

**验收**：0 处 emoji 图标；同类组件形态完全一致；a11y 基础项全部通过。

### 阶段 5 · 清理与防腐（约 1 天）
1. 删 233 个死类（分批，每批跑视觉回归）
2. 删 `components-prose.css` 中 approval 区块 115 行（与 `inline-approval-card.css` 重复）
3. 287 个跨文件重复类按职责唯一化
4. `base.css` Toast 拆出 `components-toast.css`
5. 消化 `EXCEPTIONS.md` 的 E11–E15（5 条有明确令牌映射）
6. 统一命名（废 `tw-` 前缀，`tool-*` 与 `tool-msg-*` 合并）

**验收**：CSS 总量下降 20%+；校验器基线数字持续下降不反弹。

---

## 八、总计

| 分级 | 项数 |
|---|---|
| P0 阻断 | 5 类 |
| P1 影响观感 | 16 类 |
| P2 打磨 | 15 类 |
| **整改工作量估算** | **约 7.5 人天**（分 6 阶段，每阶段独立可验收可回滚） |

**建议起步动作**：先做**阶段 0（治理基建，0.5 天）** + **阶段 1（换色，1 天）**——1.5 天投入即可消除"丑"的第一观感（蓝紫主色）并建立防腐机制，剩余阶段按 P-43 逐屏打磨。
