# Decision Note: `--color-accent` 归位 `@theme`，修复 117 处静默失效的 `*-accent` 工具类

Status: implemented — 主题层与使用层合并为单层字面量；`bg-accent` / `text-accent` / `border-accent` 及其透明度变体已在构建产物中生成；`check-theme-tokens.mjs` 五项全绿

## 现象

模型配置页的「添加模型」按钮不可见，但 DOM 里存在。根因不是这个页面：`bg-accent` / `text-accent` / `border-accent` 在构建产物中出现 **0 次**，而源码有 **117 处**调用（bg 44 / text 59 / border 14）。全应用的主按钮、强调色文字与徽章全部失效——按钮是 `bg-accent text-white`，底色缺失后成为白底白字。

同一现象的旁证：默认模型那颗星用 `fill-current text-accent`，因 `text-accent` 缺失回落成继承色而呈深灰；「默认」徽章 `bg-accent/15 text-accent` 没有底色。

## Decision

**`--color-accent` 作为字面量直接声明在 `@theme` 内，`themes/dark.css` 覆写同一个键，删除 `base.css` 的桥接声明，`--color-accent-base` 一并清除。**

Tailwind v4 只为注册在 `@theme` 颜色命名空间里的键生成工具类。原先的 `--color-accent` 只是一个普通 `:root` 自定义属性，扫描器看不见它。

分层语义**逐层不变**：

| 层 | 改前 | 改后 |
|---|---|---|
| 主题层 | `@theme: --color-accent-base` + `dark.css: --color-accent-base` | `@theme: --color-accent` + `dark.css: --color-accent` |
| 桥接层 | `base.css :root { --color-accent: var(--color-accent-base) }` | 删除 |
| 用户层 | `useTheme` 行内 `setProperty("--color-accent")` | 不变（行内优先级最高） |
| 派生家族 | `base.css` 用 `color-mix(… var(--color-accent) …)` 派生 hover / soft / soft-strong / on-text | 不变 |

只少了一跳 CSS 间接，级联结果与改前一致。

## 同步改动（不可省略）

- **`scripts/check-theme-tokens.mjs`**：把 `--color-accent` 从 `ALIAS_NAMES` 移除。改前它作为别名层被排除在契约外（"主题对它没有决定权"）；改后 `themes/dark.css` 重新对它有决定权，**必须**进契约——否则有人日后从 dark.css 删掉这行，门禁不会报，暗色主题会静默回落成亮色松墨绿。
- **注释**：`useTheme.ts` / `accentPalette.ts` / `base.css` / `dark.css` 里描述旧链路的段落全部重写。
- **同日补完（2026-10-04 晚）**：本文只搬了**本体** `--color-accent`，四档**派生**（`--color-accent-hover` / `-on-text` / `-soft` / `-soft-strong`）当时仍留在 `base.css` 的别名层。Tailwind v4 只从 `@theme` 生成工具类，于是这四类工具类同样一条规则都不生成，症状与 117 处本体失效同源（少了一层底色 / hover 无反馈）。这四档已一并移入 `tokens.css` 的 `@theme`，`base.css` 别名层只留 `--color-on-accent`（字面量而非派生，自由取色下无法用 `color-mix` 推导，留待 L3.5）。
  - **为何不构成原决策所否决的"绿配绿"回归**：原文反对把派生档放进 `@theme`，理由是静态值会让用户注入的绿 accent 配主题自带的绿 hover。这里写的是 `color-mix(… var(--color-accent) …)` 而非静态字面量，设计上仍跟随运行时 accent。
  - **但实测发现该设计意图早已落空**：构建产物里派生档被内联成 `color-mix(in srgb, #1e4a3c 10%, white)` —— `var()` 在构建期就被求值成字面量。改动前后输出**逐字节相同**，所以本次搬移是取值中性的；而"运行时注入的 accent 流不进派生档"是搬移前就存在的问题，不在本次范围。
  - **契约**：这四档是派生量，主题对它们没有决定权（混白还是混黑由派生式本身决定），故与 `--color-accent` 合并前一样列入 `ALIAS_NAMES`，不进 CONTRACT，`dark.css` 无需声明。契约规模不变（101/101）。


## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 改用 `bg-[var(--color-accent)]` 任意值 | 要改 117 处调用点，且把令牌名在每个组件里重复一遍。`check-theme-tokens` 的 Check 2 会把 `var(--color-accent)` 计入引用集，改动面巨大而收益为零。 |
| 保留桥接，手写 `.bg-accent` 等工具类 | 能修症状，但等于绕开 Tailwind 的按需生成机制；将来新增 `--color-foo` 会重蹈覆辙，且手写类名与 `@theme` 发出的同名声明可能打架。 |
| `@theme` 里写 `--color-accent: var(--color-accent-base)` | 2026-10-01 已试过并失败：`@theme` 发出的 `:root` 与别名层同源，var() 间接形成循环依赖，整条声明 invalid。用字面量彻底绕开。 |
| 恢复 `--color-accent-base` 为别名但同时在 `@theme` 声明 `--color-accent` | 两处都定义同一个键，值需要长期保持同步；`dark.css` 也要改两遍。无收益，纯负担。 |
| 只改模型配置页 | 页面级补丁。117 处调用仍在静默失效，别的页面换个按钮同样隐形。 |

## Consequences

- **117 处调用一次性恢复**，无需改任何组件代码。这是本次改动的主要价值。
- **契约门禁变严**：`--color-accent` 进入 CONTRACT，`dark.css` 必须定义它。`styles/themes/` 下只有 `dark.css` 与 `light.css`，Check 1 跳过 light，故实际只需 `dark.css` 一处。
- **`--color-accent-base` 已无定义者**。注释里**不得**再写它的完整 `var(--…)` 形式——Check 2 的孤儿扫描不跳过注释，会把说明文字当成活跃引用而误报。这是本次实际踩到的坑，已在 `tokens.css` 就地标注。
- **CSS 注释里禁止出现 `*/`**。本次因在注释中写 `bg-*/text-*/border-*` 描述工具类，第一个 `*/` 提前闭合注释，导致 `postcss-import` 报 `Unknown word border-*`、构建失败。已改写措辞。
- **验证方式**：`pnpm build:vite` 后在 `release/frontend/assets/index-*.css` 里数工具类出现次数——`.bg-accent{background-color:var(--color-accent)}`、`.bg-accent\/15`、`.text-accent`、`.border-accent` 均在。构建产物同时带 `color-mix` 动态值与静态回退（如 `.bg-accent\/15{background-color:#1e4a3c26}`），不依赖浏览器的 `color-mix` 支持。
- **未验证**：未在真机窗口逐一目视 117 处调用点。产物层已证明规则存在且取值正确，但"某处是否本来就该用别的类"属于设计判断，不在本次范围。
- **派生档补完后同样未目视**：`bg-accent-soft` / `bg-accent-soft-strong` / `hover:bg-accent-hover` / `text-accent-on-text` 四类工具类现在开始生成，约 40 处调用点的外观会从"少一层底色/无 hover 反馈"变为设计意图的样子。这些页面大多在本次范围外，建议开一轮目视走查。
