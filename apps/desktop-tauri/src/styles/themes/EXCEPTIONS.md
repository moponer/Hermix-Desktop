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

| E9  | `apps/desktop-tauri/src/components/ClarifyBubble.tsx` | 临时历史遗留 —— 评审项，迁移到令牌后撤销 |
| E10 | `apps/desktop-tauri/src/components/custom-apps/iconResolver.tsx` | 临时历史遗留 —— tile 品牌色 |
| E11 | `apps/desktop-tauri/src/components/CustomAppsView.tsx` | 模态遮罩 rgba —— 应改用 --color-overlay-scrim |
| E12 | `apps/desktop-tauri/src/components/EditProfileDialog.tsx` | 模态遮罩 rgba —— 同上 |
| E13 | `apps/desktop-tauri/src/components/MessageList.tsx` | 阴影 rgba —— 应改用 --shadow-pop 变体 |
| E14 | `apps/desktop-tauri/src/components/TextBubble.tsx` | 用户图片边框色 —— 应新增令牌 |
| E15 | `apps/desktop-tauri/src/lib/appMention.ts` | @ 提醒色 —— 应改用 --color-link |
| E16 | `apps/desktop-tauri/src/main.tsx` | 启动横幅 inline style —— 临时过渡色 |

> E9–E16 是 [check-theme-tokens.mjs](../../../../../../../../scripts/check-theme-tokens.mjs) 抓到的历史债项。
> 评审时机：每次处理"主题体系"或"组件清理"PR 时优先消化。

## 已登记债项（仅告警，不阻断）

### Tailwind 原生调色板类（已登记债项 → 待迁移到语义令牌）

`scripts/check-theme-tokens.mjs` 的 `[palette-debt]` 当前为 WARN_ONLY 模式。目标状态：
每处 `text-red-400` / `bg-emerald-500` / `border-amber-700` 等 Tailwind
原生调色板类，最终都要替换为对应语义令牌（如 `--color-error` /
`--color-success-soft` / `--color-warning` 等），或在 `@theme` 中以
`--color-red-400: var(--color-error)` 形式重定向该语义槽。

完成迁移后把脚本里的 `palette-debt` 输出段切换为 `process.exit(1)`
（当前为 `console.warn` 不退出），开启强约束。

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