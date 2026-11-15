# Decision Note: CSS 层描边门禁（G11）与功能性标记的画法

Status: implemented — G11 门禁已进 ci-gate；思考块旁注轨道于 2026-10-26 按本规则恢复

## Decision

### 1. G11 禁的是「1px 中性结构描边」，不是「所有线条」

`scripts/check-design-guard.mjs` 的 **G11** 扫 `src/**/*.css`，判据是一条精确
正则：`border(-top|-bottom|-left|-right)?: 1px solid var(--color-border-*|twin-line-*)`，
基线 **0**。它拦的是**中性结构描边**——那种画不出层级、只增加视觉噪声的
1px 框线（2026-10-23 一次清掉 89 处，根因是 CSS 层从未被门禁扫过，129 处
`border: 1px solid` 全在盲区）。

功能性描边不在统计内：`2px dashed`（拖放落点）、`transparent`（hover 占位）、
语义色状态框（error / warning）。

### 2. 描边清零是规则批量带走，不是逐块的设计判断

2026-10-23 的 commit `03d327b6`（"清掉 CSS 层 89 处描边 … 并把 CSS 纳入门禁"）
在批量删除中**连带删掉了**思考块 `.tl-reasoning-row` 的
`border-left: 1px solid var(--color-border-secondary);`。该竖线是「这是旁注」
的几何标记之一，与被清的 89 处粗框不同类；但它落在规则的正则上，于是被一并
带走。删掉后缩进（18px + 12px）还在、**标记没了**，正文右边空出一截像没排完
的缩进——2026-10-26 用户实机反馈「思考过程的左边竖条没有了」。

这条竖线此前已被调细过一轮（2026-10-01：2px `--color-text-faint` → 1px
`--color-border-secondary`），理由写在当时的注释里：它对聊天底只有 1.37:1，
比它标记的思考正文（4.77:1）还弱 3.5 倍，视觉上等于没有。

### 3. 规则：功能性标记用 background 渐变画 2px 实心轨道，不写 border

旁注轨道这类**标记内容性质**（而不是划分结构）的竖条，统一用背景渐变画：

```css
background: linear-gradient(to right, var(--color-text-faint) 0 2px, transparent 2px);
```

- **不写 `border`**：G11 的判据只统计 border 声明，background 不在统计内；
  同时这正是「功能性标记」与「结构描边」在实现层的分界——前者是标记，后者是框。
- **画在 padding box 上**：思考块 `max-height: 340px` + 内部滚动，背景属于
  元素自身、不随内容滚动，轨道始终贴着可见区左缘。
- **2px + `--color-text-faint`**（浅色 `#6a6a6a` / 深色 `#8f8f8f`）：回到
  2026-10-01 之前的重量。不用 `--color-border-secondary`——它与要标记的
  思考正文色阶太近，读不出来。
- **门禁不需要改**：G11 正则不匹配 background 渐变，也不匹配 2px。

这条规则对所有块通用：将来给别的块加旁注轨道（例如引用块、旁注卡）照此办理。

## Alternatives considered

- **写回 `border-left: 1px solid var(--color-border-secondary);`** —— 否决。
  G11 基线是 0，这行会被门禁拦下（exit 1）；而且它 1.37:1 的对比度意味着
  恢复的是一条看不见的线，等于什么都没改。
- **扩 G11 白名单，把旁注轨道显式列为功能性描边** —— 否决。为一条轨道改门禁
  脚本并抬基线偏重；更要紧的是判据会从「1px 中性描边」退化成「白名单枚举」，
  白名单必然腐化（每加一条都要改脚本，不如直接用不触线的画法）。
- **用 `border-left: 2px solid var(--color-text-faint)` 绕过正则** —— 否决。
  那是钻正则空子：2px 的中性描边照样落在「结构描边」这条纪律想禁的东西上，
  只是当前正则没覆盖。规则要能被绕过就不叫规则。
- **用 `::before` 绝对定位画轨道** —— 否决。思考块内部滚动，绝对定位的伪元素
  随内容一起滚走，轨道下半截断线；背景画在 padding box 上不动。
- **只靠缩进，不加轨道** —— 否决。已被实机否决：缩进本身不携带信息，读起来
  像没排完的留白，而不是「这是旁注」。
- **画得更粗（3px+）压过一切** —— 否决。2026-10-01 的注释已写过：两处在喊
  同一句话，竖线一粗就变成主角，把真正要读的思考正文压下去。

## Consequences

- **G11 判据保持原样**，不新增白名单、不抬基线；本规则靠「background 画
  功能性标记」与门禁共存，而不是靠给门禁开口子。
- **与扁平化总则不冲突**：[2026-10-23 扁平化统一（无描边 / 彩色分类图标 / 统一动效）](../architecture/2026-10-23-flat-design-unification.md) 的「层次只由
  底色差 + 间距 + 阴影承担，不用描边」针对的是**结构框线**；旁注轨道标记的是
  内容性质（这是旁注），不承担分区职责，属于该总则未覆盖的一类。
- **思考块旁注轨道已恢复**（`apps/desktop-tauri/src/styles/components-helpers.css`
  的 `.tl-reasoning-row`）。纯 CSS 改动，dev 模式 HMR 即时生效，历史消息同样
  生效。
- **验证**：`check-design-guard`（含 G11）、`check-design-primitives`、
  `check-theme-tokens`（contrast 全过 AA）、`impeccable detect` 均通过。
- 相关笔记：[2026-10-23 扁平化统一（描边清零总则）](../architecture/2026-10-23-flat-design-unification.md)、
  [2026-10-21 前端设计审计（G11 的依据）](../../proposed/architecture/2026-10-21-frontend-design-audit.md)、
  [2026-10-24 对话正文降噪（正文侧降噪）](../simplification/2026-10-24-conversation-visual-noise.md)。
