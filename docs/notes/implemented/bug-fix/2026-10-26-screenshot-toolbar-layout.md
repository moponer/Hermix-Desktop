# Decision Note: 截图工具条中文标签逐字竖排 —— 居中写法砍掉了工具条一半宽度

Status: implemented — 2026-10-26 交付：工具条拿回整幅可用宽度、按钮不许被压扁、装不下就整组换行；图标换 lucide；覆盖窗补上缺失的强调色令牌

## Decision

现象（用户 2026-10-26 截图）：截图编辑器底部工具条里，「框选 / 箭头 / 画笔 /
文本 / 涂抹 / 裁剪」的中文标签被排成一列一个字，工具条整体被压扁成一团。

### 根因（实测，不是推测）

1. **居中写法砍掉一半可用宽度**：`.se-toolbar` 用
   `position: fixed; left: 50%; transform: translateX(-50%)` 居中。固定定位
   只给了 `left` 没给 `right` → 元素是**收缩盒**，它的可用宽度 = 100% − left
   = 视口的一半。实测：1200px 视口下工具条只有 **600px**，420px 视口下只有
   **210px**。同源的 `max-width: calc(100% - 12px)` 在这一条面前形同虚设。
2. 内容自然宽度 764px（6 工具 + 6 色 + 3 笔宽 + 3 动作）> 可用宽度 →
   flex 子项默认可压缩（`flex-shrink: 1`）→ 按钮被压到 min-content 宽度，
   而中文没有词内换行点 → **每个字各占一行**（竖排）。
3. 原来的 `overflow-x: auto` 救不了：横向滚动只在子项**不肯**收缩时才生效，
   而 flex 子项默认就肯收缩。

### 修法

- `width: max-content` —— 显式宽度拿回整幅可用宽度，`max-width` 再负责封顶。
  `.so-toolbar`（选区阶段的提示条）同源同坑，一并修。
- 按钮 `flex: 0 0 auto`（不许被压扁）+ `white-space: nowrap`（不许逐字换行）。
- 工具条 `flex-wrap: wrap`，且**分组内部也允许换行**（`flex: 0 1 auto` +
  `min-width: 0`）—— 否则窄选区下整组（6 个工具）作为一个 flex 项超出容器，
  右侧工具直接被切掉（实测 360px 视口时「裁剪」被切）。
- 分组靠淡底块（`.se-group`）分隔，**不画分隔线**（长期偏好：层次靠底色差 +
  阴影 + 间距，不要描边）；工具条本体去掉 1px 描边，改用阴影。

### 顺带修掉的第二个问题：覆盖窗根本没有强调色令牌

实测（dev server + Chrome 探测覆盖窗 `index.html#/screenshot-overlay`）：
本窗的 `--color-accent` / `--color-on-accent` / `--color-accent-line` **全为空**。
原因：覆盖窗是独立入口，只加载 `screenshot-overlay.css`，不加载 `tokens.css`
（那里的令牌由 Tailwind `@theme` 生成）。后果是选中态工具、「复制」按钮、
选区边框、文本框 outline 全部退化成「无背景 / 无边框」——用户截图里「复制」
就是个白字，主按钮完全没有分量。

修法：`screenshot-overlay.css` 自带一份与主题同源的令牌层（亮色 `#2f4a63` +
`#ffffff`，暗色 `#9dbdd8` + `#0b1230`），强调色底上的文字色改走
`var(--color-on-accent)`（不再写死 `#fff`）；用户自选强调色由 `main.tsx` 的
覆盖窗分支在首帧前从 localStorage 读回并注入（口径与主窗 `useTheme` 一致）。

### 排版整理

- 工具图标从 Unicode 字形（▭ ➜ ✎ T ▓ ✂）换成 lucide（Square / MoveUpRight /
  Pencil / Type / Grid2x2 / Crop），16px 档，继承 `currentColor`（选中态自动变色）。
  字形在不同字体下粗细不一、含义靠猜。
- 撤销 / 放弃改成纯图标按钮（Undo2 / X，28×28），保留 `title` + `aria-label`。

## Alternatives considered

- ~~**把窗口撑到工具条需要的宽度**（Rust 侧 `place_window` 给最小宽度）：窗口透明，
  多出来的边会露出桌面，而且透明区会吃掉鼠标点击（窗口不可穿透）—— 用「看得见的
  空地」换「一屏放下」，不划算。~~ **此结论已被推翻**（2026-10-28）：透明与点击
  两条代价都可以消掉（编辑器自带暗底 + 版面重排），见
  [2026-10-28-screenshot-editor-min-window.md](2026-10-28-screenshot-editor-min-window.md)。
- **横向滚动代替换行**：能显示全部按钮，但要用户左右拖，且滚动条压在图像上。换行更好。
- **纯图标、去掉文字标签**：宽度能压到 ~500px，但六个模式（框选/箭头/画笔/文本/
  涂抹/裁剪）生僻，丢了标签就得靠悬停猜 —— 可发现性代价太大。
- **给覆盖窗整体加载 globals.css**：能一次拿到全部令牌，但把整份应用 CSS（含
  Tailwind）拉进独立入口，正是 2026-10-10 「覆盖窗独立小图」要避免的首帧开销。

## Consequences

- 工具条自然宽度 764px。实测七档视口（1200 / 900 / 700 / 520 / 420 / 360 / 300px）：
  ≥900px 单行；700~520px 两行；≤360px 三行；**任何宽度都无内容裁切、无竖排**
  （playwright 量测：溢出元素 0 处、按钮高度全部 ≤34px）。
- 工具条当时仍浮在画布底部（那一轮不扩窗口），窄选区时依然会遮住图像下缘。
  2026-10-28 起工具条改为流内一条、窗口有最小尺寸，这条代价已消除：见
  [2026-10-28-screenshot-editor-min-window.md](2026-10-28-screenshot-editor-min-window.md)。
- 覆盖窗现在自带 4 个颜色字面量（强调色两套 + on-accent 两套）。它们在
  `check-theme-tokens` 里属于「定义侧」不计裸色债，但**确实是一处令牌重复**：
  主题改强调色时要同步改这里（`themes/dark.css` 是另一处）。
- 截图覆盖窗是独立入口，不加载 Tailwind 与 globals.css —— 图标尺寸在本地 CSS
  里给死，不依赖 `icon-md` 档位类。
