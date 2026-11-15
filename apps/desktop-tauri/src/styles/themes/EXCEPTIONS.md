# 品牌色豁免登记（EXCEPTIONS）

规范：组件内禁止裸 `#hex`；但**品牌识别色**是数据不是装饰，允许在登记后使用。
新增品牌色必须先在本文件登记（名称 + 用途 + 色值来源），再写代码。

## 已登记

| 文件 | 常量 | 用途 | 说明 |
|---|---|---|---|
| `src/components/ProviderIcon.tsx` | `BRAND_COLOR`（26 项） | 厂商 logo 软底/前景 | lobehub/官方站点主色；软底已改 `color-mix` 派生（2026-10-16），禁拼 hex8 |
| `packages/preview/src/FileTypeIcon.tsx` | 按扩展名的色表（130 处） | 文件类型图标底色 | 2026-10-19 从 `src/components/` 搬进共享包（desktop 与 viewer 共用同一份图标体系）。本表只扫 `apps/desktop-tauri/src`，共享包不在其范围内——登记保留以备该表将来扩围 |
| `src/components/agents/types.ts` | 8 处 hex | 专家头像色板 | 后续并入 ColorPicker 档位 |
| `src/lib/codeHighlight.ts` | 高亮色板（约 40 处） | 代码语法高亮 token 色 | 第三方高亮库（highlight.js 主题）的输入数据，非界面装饰；主题切换时由调用方整体换肤 |
| `src/styles/themes/accentPalette.ts` | `ACCENT_PALETTE`（5 组） | 强调色候选色板（用户选色数据） | 与 ProviderIcon 同类：色值是数据不是装饰；swatch/on 成对登记 |
| `src/main.tsx` | 第 120 行 `%c` 样式串 | devtools console.info 的构建标识样式 | 只影响开发者工具输出，不进界面；跟随 dev 分支存在 |
| `src/components/ImageLightbox.tsx` | 暗室底色/工具条（约 10 处） | 图片查看器全屏暗室 | 看照片必须压暗环境光（照片还原本色），功能性恒暗，同 screenshot-overlay 类 |
| `src/components/ScreenshotEditor.tsx` | `COLORS` 调色板（6 色） | 截图标注画笔颜色 | 用户选色数据（画笔要覆盖任意底色），非界面装饰 |
| `src/components/custom-apps/iconResolver.tsx` | 图标 tile 白字（1 处） | 自定义应用图标底色上的文字 | 用户自选图标底色的配套前景色，与 ProviderIcon 同类 |

## 独立窗口恒暗皮肤（结构性例外）

以下三个 CSS 由 `main.tsx` 的 hash 路由分流加载，运行在**独立 Tauri webview 窗口**（截图覆盖窗 / 浏览器小窗 / 文件搜索小窗）。这些入口不挂载 `useTheme`、不加载 themes/*.css，`data-theme` 在它们的文档上不存在——语义令牌全部取不到值。它们是系统级工具浮层（Alt+Q 截图、Alt+F 搜索、浏览器 screencast），恒暗是产品决策而非疏漏：

| 文件 | 窗口 | 说明 |
|---|---|---|
| `src/styles/screenshot-overlay.css` | `#/screenshot-overlay` | 全屏幕布 + 选区，恒暗保证选区对比度 |
| `src/styles/browser-view.css` | `#/browser-view` | 浏览器 screencast 帧流容器 |
| `src/styles/file-search.css` | `#/file-search` | Everything 式搜索面板 |

若未来要让小窗跟随主题：在各自入口补 `applyTheme(readStored())` 并加载 themes css，届时本登记作废，按主窗同规迁移。

## 使用纪律

1. 品牌色只许做**图标前景 / 软底**，不许做按钮、边框、大面积底色
2. 软底一律 `color-mix(in srgb, <色> 12%, transparent)`，禁止字符串拼 hex8
3. 与语义色（success/warning/error/info）冲突时语义色优先
