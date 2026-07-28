# Theme Hardcoded Exceptions

以下文件包含无法完全通过 CSS 变量替换的颜色硬编码，属于视觉一致性/品牌识别目的的例外。

| ID | 文件 | 用途 | 维护者 |
|----|------|------|--------|
| E1 | `apps/desktop-tauri/src/components/FileTypeIcon.tsx` | 文件类型图标的 GitHub linguist 品牌色 | 已固化，不随主题切换 |

| E2 | `apps/desktop-tauri/src/components/MessageBubble.tsx` | Agent avatar palette (model-color stripe) — user-set per conversation, not theme-bound |
| E3 | `apps/desktop-tauri/src/components/agents/types.ts` | Agent theme palette (violet/rose/cyan/...) — user-picked, theme-independent |
| E4 | `apps/desktop-tauri/src/components/ImageLightbox.tsx` | Fullscreen media viewer — by spec stays dark in both themes |

| E5 | `apps/desktop-tauri/src/components/ProviderIcon.tsx` | Provider brand logos (Anthropic / DeepSeek / Kimi / Zhipu …) - all third-party trademarks, not theme-bound |
| E6 | `apps/desktop-tauri/src/lib/codeHighlight.ts` | Code Highlighter palette (VS Code One Dark style) - intentionally fixed, contrast chosen for terminal |
| E7 | `apps/desktop-tauri/src/components/groupchat/types.test.ts` | Unit tests that intentionally verify hex passthrough |
| E8 | `apps/desktop-tauri/src/components/groupchat/RoomList.tsx` | Subtle shadow literal for light-only list-row visual |
