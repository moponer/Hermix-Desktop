# Theme Hardcoded Exceptions

以下文件包含无法完全通过 CSS 变量替换的颜色硬编码，属于视觉一致性/品牌识别目的的例外。

| ID | 文件 | 用途 |
|----|------|------|
| E1 | `apps/desktop-tauri/src/components/FileTypeIcon.tsx` | 文件类型图标的 GitHub linguist 品牌色 |
| E2 | `apps/desktop-tauri/src/components/MessageBubble.tsx` | Agent avatar palette — user-set per conversation |
| E3 | `apps/desktop-tauri/src/components/agents/types.ts` | Agent theme palette — user-picked, theme-independent |
| E4 | `apps/desktop-tauri/src/components/ImageLightbox.tsx` | Fullscreen media viewer — 规范要求双主题恒暗 |
| E5 | `apps/desktop-tauri/src/components/ProviderIcon.tsx` | Provider brand logos — 第三方商标 |
| E6 | `apps/desktop-tauri/src/lib/codeHighlight.ts` | Code Highlighter 调色板 — VS Code One Dark 风格固定 |
| E7 | `apps/desktop-tauri/src/components/groupchat/types.test.ts` | 单元测试故意验证 hex 直传 |
| E8 | `apps/desktop-tauri/src/components/groupchat/RoomList.tsx` | 仅亮色 list-row 阴影字面值 |

| E9  | `apps/desktop-tauri/src/components/ClarifyBubble.tsx` | ~~临时历史遗留~~ **已迁** → `--color-info` 12% 混合 |
| E10 | `apps/desktop-tauri/src/components/custom-apps/iconResolver.tsx` | **保留**：tile 色是**用户自己配置的应用品牌色**（`app.tile_color`）+ 其上的 `#ffffff` 对比色，属品牌组合，映射到四色组会丢失用户意图 |
| E11 | `apps/desktop-tauri/src/components/CustomAppsView.tsx` | ~~模态遮罩 rgba~~ **已迁** → `--color-overlay-scrim` |
| E12 | `apps/desktop-tauri/src/components/EditProfileDialog.tsx` | ~~模态遮罩 rgba~~ **已迁** → `--color-overlay-scrim` 137%（原 0.55 比 scrim 的 0.4 更深，保留深浅差异） |
| E13 | `apps/desktop-tauri/src/components/MessageList.tsx` | ~~阴影 rgba~~ **已迁** → `--shadow-pop` |
| E14 | `apps/desktop-tauri/src/components/TextBubble.tsx` | ~~用户图片边框 ~~ **已迁** → `--color-border-secondary`（原 `#2e2e34` 是写死的深色，亮色主题下突兀） |
| E15 | `apps/desktop-tauri/src/lib/appMention.ts` | ~~@ 提醒色~~ **已迁** → `--color-link`（原 `#3b82f6` 的注释还在解释"深蓝在暗色下会不会看不见"，交给令牌后该问题消失） |
| E16 | `apps/desktop-tauri/src/main.tsx` | **保留**：dev-only 的 `console.info` 徽章（`import.meta.env.DEV` 门内，输出到 devtools，不进 UI），与主题体系无关 |
| E17 | `apps/desktop-tauri/src/styles/themes/accentPalette.ts` | **保留**：用户可选强调色的**策展清单本体**。这些 hex 是"可选值的数据"，不是"某个主题该用哪支颜色"——token 表达不了"用户还没选"，`--color-accent` 由用户在设置页注入，清单只负责枚举候选 + 预置 on-accent。与 E5（供应商品牌 logo 同理）同类。**重新引入条件**：若改为自由取色器（`<input type="color">`），本文件即成死数据，届时应删除并让 on-accent 改走运行时亮度计算。 |
| E18 | `apps/desktop-tauri/src/components/settings/AppearanceTab.tsx` | **保留**：明暗三选卡的缩略图**示意图**。`themes/*.css` 按 `:root[data-theme]` 作用域提供 token，无法把"另一套主题"的取值摘出来内联渲染，故用固定明/暗色画出结构示意。它表达的是"亮底还是暗底"，不是像素级预览——真要像素级预览得给每套主题加一个可脱离作用域的采样通道，属另一件事。 |

> E9–E16 是 [check-theme-tokens.mjs](../../../../../../../../scripts/check-theme-tokens.mjs) 抓到的历史债项。
> 2026-10-01（L0）：6 条已迁（E9/E11/E12/E13/E14/E15），2 条判定为真例外保留（E10/E16）。
> E17/E18 为 L3.5 强调色调色板与 L3.4 明暗三选卡新增，均为"数据/示意"而非"主题取值"。

## 已消化债项（L0，2026-10-01）

### Tailwind 原生调色板类 — 已清偿

**状态：已清偿，门禁已收紧。**

101 处 `text-red-400` / `bg-emerald-500` / `border-amber-700` 等原生色阶类
**一行未改**，而是在 `tokens.css` 的 `@theme` 内把**实际在用的 21 个色槽**
重定向到语义令牌（`--color-error` / `--color-warning` / `--color-success` /
文本灰阶）。组件里的类名照旧，解析后跟随主题与强调色。

用到的 21 槽（按色相族）：

| 族 | 槽 | 指向 |
|---|---|---|
| error | `red-200/300/400/500`、`rose-200/300/500` | `--color-error`（200/300 按 shade 降 alpha 保留浓淡层次） |
| warning | `amber-200/300/400/500`、`yellow-400` | `--color-warning`（同上） |
| success | `green-400/500`、`emerald-200/300/400/500` | `--color-success`（同上） |
| neutral | `zinc-300/400/500` | `--color-text-tertiary` / `-faint` / `-secondary` |

同族不同 shade 在本项目承载**真实的视觉层次**（如 AnalyticsView 的
`red-300` 是标题、`red-200` 是次要说明），所以按 shade 映射到不同
alpha，而不是全部塌成同一个 token。

**必须放在 `@theme` 内**（已实测）：Tailwind v4 只从 `@theme` 生成工具类，
放在别名层（`:root`）里 `.text-red-400` 规则**根本不会生成**，重定向
会静默落空。

**门禁已收紧**：`check-theme-tokens.mjs` 的 `[palette-debt]` 从 WARN 改为
阻断，且语义变成"**未登记的色阶类才拦**"——在用的 21 槽是合法的，
新增一个 `text-sky-500` 这类未在 `@theme` 声明的会直接让 CI 变红。
扩展槽位需同时改三处：`tokens.css @theme`、各主题 css、`PALETTE_ALIASES`。

### components.css / components-prose.css 中残留的 `[data-theme]` 字面色覆盖块

历史 file-pill 两块已迁语义令牌；其余状态色覆盖（34 处）体量大且需逐
个视觉审查，本次仅登记作为"已知债项"。后续清理原则：能映射到四色组
(success/info/error/warning) 软硬版本的，迁语义令牌；不能的保留并在
此处逐条编号。

### 主题体系设计语录（避免再次腐化）

- 每个组件只写 `var(--color-*)`，不出现 `#hex` / `rgba()` / Tailwind 原生色类。
- 每套主题 css 必须声明 CONTRACT 全部色彩令牌；缺一 checker 即失败。
- 暗色族判定统一来自 `<id>.json` 的 `colorScheme` 字段，组件不写主题名单。
- 新增主题流程：drop `<id>.css` + `<id>.json` 进 `themes/`，填齐全部 CONTRACT
  变量即可上线，registry 自动发现。