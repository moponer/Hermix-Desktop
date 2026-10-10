# Hermix 官网设计锚（apps/relay/public）

> 本文件是这批页面（9 个 HTML + `assets/site.css`）的**唯一事实源**。
> 改风格 = 先改本文件，再改代码。任何一轮会话、任何一个页面都以此为准。

---

## 1. 视觉主题与氛围

一个把技术克制写进纸面的产品站。底子是**奶油纸**（Parchment），不是白屏，也不是冷灰；
重音只有一个**陶土棕**（Terracotta）。全站没有第二个饱和色，没有渐变，没有玻璃拟态，
没有发光。层次全部由**明度阶梯 + 发丝线**承担，而不是色相跳跃。

气质上它应该像一份印得讲究的技术手册：可信、不吵、能被读完。
Hermix 的产品主张是「数据在本机、不设服务器」，视觉上必须兑现这个承诺——
克制、可审计、无推销感。营销页与文档页共用同一套底子，读者从首页翻到文档页不应觉得换了网站。

**记忆点**：纸底上唯一的陶土棕，以及每个产品页首屏那块**手绘感的高保真界面预览**
——它让「这是个桌面应用」这件事在三秒内成立，而不靠一句口号。

### DNA 注入（逐值可溯源）

| 偷来的值 | 出处 |
|---|---|
| Parchment `#f5f4ed` 纸底、Ivory `#faf9f5` 卡面、Border Cream `#f0eee6` 发丝边 | `design-systems/claude` §2 Surface & Background |
| Terracotta `#c96442` 家族重音、正文行高 1.60 起、whisper shadow `0 4px 24px / .05` | `design-systems/claude` §2 Primary、§3 Principles、§6 |
| 阴影栈单层透明度 ≤ 0.05、多层累积（1.04px → 24px blur） | `design-systems/notion` §2 Shadows & Depth |
| badge 12px 用正字距 `0.125px` | `design-systems/notion` §3 Badge 行 |
| 卡片圆角 12px / 按钮 8px（不是 4px 也不是 999px） | `design-systems/notion` §5 Border Radius Scale |

---

## 2. 色板与角色

```css
--paper:      #f5f4ed;  /* 页底 */
--paper-2:    #faf9f5;  /* 卡面 / 抬高面 */
--paper-3:    #efece2;  /* 次级填充：按钮底、hover 底 */
--line:       #e6e2d6;  /* 发丝线：分区、卡片边框 */
--line-soft:  #f0eee6;  /* 最轻分区：列表行分隔 */
--ink:        #24221e;  /* 主文字 14.4:1 */
--ink-2:      #5c564e;  /* 次级文字 6.6:1 */
--ink-3:      #6f6859;  /* 元信息 5.0:1 —— 全站最浅文字，不可再浅 */
--clay:       #b0513a;  /* 陶土重音：主 CTA、链接、当前项 */
--clay-deep:  #8f3f2b;  /* 按压 / hover */
--clay-soft:  #f6ece6;  /* 陶土浅底：选中态、callout */
```

**对比度底线**：`--ink-3` 是 WCAG AA 的下沿（5.0:1 on paper）。任何比它更浅的灰
都不得用于文字，只能用于边框或装饰。

**禁色**：slate / blue / indigo / violet / purple 全族禁用（P-39）。
自检法：hex 的 B 通道明显高于 R/G 即是违规，改同明度暖灰。

**语义色**（仅状态使用，不参与装饰）：成功能 `#4a6b3f`、警告 `#a8651f`、错误 `#a33a2c`。

---

## 3. 排版规则

字体：正文/UI 系统栈（`-apple-system, "PingFang SC", "Microsoft YaHei", "Noto Sans SC"`），
代码 `ui-monospace, SFMono-Regular, Menlo, Consolas`。**不引入任何 webfont**——
中文字体 5–20MB，CDN 拖慢首屏，且用户机器上未必装得下。

| 角色 | 字号 | 字重 | 行高 | 字距 |
|---|---|---|---|---|
| H1（首屏） | `clamp(29px, 3.4vw, 36px)` | 600 | 1.30 | -0.015em |
| H2（section） | 23px | 600 | 1.35 | -0.012em |
| H3 | 18px | 600 | 1.45 | -0.008em |
| 引导句 `.lead` | 16.5px | 400 | 1.70 | 0 |
| 正文 | 15px | 400 | 1.72 | 0 |
| 文章正文 | 16px | 400 | 1.85 | 0 |
| 列表描述 / 元信息 | 13.5px | 400 | 1.68 | 0 |
| 版本号 / 页脚 | 12.5px | 400 | 1.6 | 0 |
| badge | 12px | 500 | 1.33 | **+0.125px** |

规则：
- 字重只用 400 / 500 / 600。**不用斜体**（P-1；CJK 伪斜体尤其禁用），强调靠字重与颜色
- H2 与正文比值 1.53×，标题必须比正文"跳"得出来
- `.lead` 必须 ≥ 正文——它是引导句，不是注脚
- 中文正文行高 ≥ 1.5；中西文、中文与数字之间加空格
- 数字与版本号一律 `font-variant-numeric: tabular-nums`

---

## 4. 组件样式

### 按钮
- 主：底 `--clay`，字 `#fff`，8px 圆角，`padding: 9px 20px`，13.5px/600
- 次：透明底 + `--line` 边框 + `--clay` 字；hover 填 `--clay-soft`
- **禁用态**：底 `--paper-3`，字 `--ink-2`（不用白字——白字在浅灰底上 1.48:1 不可读）。
  禁用必须同时给出**原因与解锁路径**，不能只变个颜色
- 反馈：hover 换底 140ms；`:active { transform: scale(.97) }`；无 `transition: all`

### 容器
- 单位容器 `--container: 1160px`，`margin-inline: auto`，横向 `clamp(20px, 4vw, 48px)`
- section 纵向 `clamp(40px, 5vw, 64px)`
- 卡片：底 `--paper-2`，`1px solid var(--line)`，12px 圆角，内距 20–24px
- **卡片不是万能分组**（P-38 同级规则）：列表、特性、日志优先用 `border-top` +
  留白分区；卡片只留给下载区、CTA 与需要 z 轴层级的地方

### 选中态 / 强调
- **禁止左侧竖线**（P-38 硬禁令）：侧栏当前项、callout、note 一律改用
  **整块底色（`--clay-soft`）+ 全边框 + 600 字重 + 陶土字色**
- 焦点环：`2px solid var(--clay)`，`outline-offset: 2px`，仅 `:focus-visible`

### 徽标 / 标签
12px / 500 / +0.125px 字距，`--paper-3` 底 + `--ink-2` 字 + 6px 圆角。不用 emoji（P-40）。

---

## 5. 布局原则

- 基准 8px：4 / 8 / 12 / 16 / 24 / 32 / 48 / 64
- 组内间距 < 组间间距
- **首屏非对称**：`minmax(0,1.05fr) / minmax(0,.95fr)` 文字 + 界面预览；
  禁止居中 Hero，禁止三等分卡片横排
- 不等比优先：2fr 1fr、编辑式双列
- 文档/阅读页：侧栏 236px + 正文 `minmax(0, 1fr)`，容器居中，**不用空 div 占位**
- 窄屏（<900px）非对称一律塌缩单列

---

## 6. 深度层级

| 层 | 处理 | 用途 |
|---|---|---|
| 0 平 | 无边框无阴影 | 页底、纯文本 |
| 1 含 | `1px solid var(--line)` | 卡片、section 分区 |
| 2 抬 | 两层栈 `0 1px 2px rgba(36,34,30,.04), 0 2px 8px rgba(36,34,30,.03)` | 可交互卡片 hover |
| 3 浮 | 两层栈 `0 2px 6px rgba(36,34,30,.05), 0 8px 24px rgba(36,34,30,.05)` | 界面预览、下拉、置顶浮层 |

阴影一律染底色色调（暖近黑 `rgba(36,34,30, …)`），不用中性黑。单层透明度 ≤ 0.05。

---

## 7. Do's / Don'ts

**Do**
- 纸底 + 单一陶土重音，层次靠明度阶梯
- 每个产品页首屏给一块**高保真界面预览**（深度 ≥ 3 层、元素 ≥ 8、有活细节）
- 版本、SHA、日期一律现取现填；取不到就说"未读到清单"，不编一个像的
- 动效只做反馈级：120–240ms，`cubic-bezier(.23,1,.32,1)`，只动 `transform` / `opacity`

**Don't**
- 不用蓝紫（P-39）、不用渐变、不用玻璃拟态、不用外发光（P-41）
- 不用斜体（P-1）、不用 emoji（P-40）
- 不用左侧竖线做选中或强调（P-38）
- 不用三等分卡片横排、不用卡片当分组万能药
- 不用编号式 meta-label（"SECTION 01"）、眉批 ≤ ⌈section 数 / 3⌉
- 不写"革命性 / 颠覆 / 无缝 / 赋能"；感叹号一页 ≤ 1
- 不用 `transition: all`、不给 UI 动效加 `ease-in`、不省略 `prefers-reduced-motion`

---

## 8. 响应式

| 断点 | 变化 |
|---|---|
| <640px | 单列；H1 落到 29px；导航折汉堡；下载区二维码居中 |
| 640–900px | 特性双列；客户端区单列；文档侧栏转为页面内横排目录 |
| 900–1200px | 完整非对称；侧栏 224px |
| >1200px | 容器锁 1160px 居中，两侧留白 |

触控目标 ≥ 44×44（导航汉堡、筛选 pill、下载按钮）。
移动端禁横向滚动：表格包 `overflow-x:auto`，长串 SHA 用 `word-break: break-all`。

---

## 9. Motion 哲学

动效强度 4/10 —— 只做反馈，不做表演。

- 缓动：`--ease-out: cubic-bezier(0.23, 1, 0.32, 1)`
- 时长：按钮 120–140ms · 卡片 hover 160ms · 浮层/侧栏 240ms；**上限 300ms**
- 按压：`transform: scale(0.97)`，50ms 内必须看得见
- hover 一律包 `@media (hover: hover) and (pointer: fine)`
- 全局 `@media (prefers-reduced-motion: reduce)` 兜底：动画与过渡归零，变换也撤掉
- 只动 `transform` / `opacity`；不动画 `width/height/margin/padding/top/left`
- 键盘高频操作（导航、筛选）不加动画
