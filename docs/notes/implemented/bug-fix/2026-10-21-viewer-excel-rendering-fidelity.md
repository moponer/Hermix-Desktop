# Decision Note: 表格渲染保真 —— 纸面世界、HSL tint、真网格线

Status: implemented — 共享预览层与查看器表格在同一套保真规则下渲染；`packages/preview` 的 92 条前端测试与 `hermix-viewer` 编译通过

## Decision

表格是**一张白纸**，不是界面的一部分。界面主题（尤其深色）不渗进表格：纸面固定白底，单元格自带底色时按亮度选墨色（浅底黑字 / 深底白字），没有底色时黑字。这一条同时解决了"表格背景和内容都是白的"和"内容直接看不见"两类投诉。

具体落在四个地方：

1. **纸面令牌由应用侧定义**（`--sheet-bg` / `--sheet-ink` / `--sheet-grid` / `--sheet-head-bg` / `--sheet-head-fg`，见 `apps/viewer/src/styles.css` 与 `apps/desktop-tauri/src/styles/tokens.css`），共享 CSS 只使用不定义 —— 与 `scripts/check-preview-layer.mjs` 的 token 规则一致，同一事实只有一处归属。
2. **主题色 tint 走 HSL 亮度通道**（ECMA-376 §18.8.27），不再是逐通道线性插值。Accent1 `#4472C4` 的 Darker 25% / 50% 现在与 Excel 逐位相同（`#2F5597` / `#203864`），旧算法在这两档能差 4~6 个色阶 —— 用户看到的"颜色发闷"就在这里。
3. **主题色板逐个索引回退**：文件自带的主题缺了某个色（常见的是 hlink / folHlink）时，只有那个索引退回 Office 默认值。旧实现要求 12 个色齐全，否则整张色板作废，全文主题色都变成默认蓝。
4. **网格线用真边框画**（`border-collapse: collapse` + 单元格 right/bottom 边框），不再用 `box-shadow: inset` 假线：假线在有色底上几乎看不见，搜索高亮一加还会把整片线抹掉。

另外补三处忠实度缺口：渐变填充取色标均值（此前整格退回白底，配白字就是"看不见"）；行高取「源文件行高」与「装得下本行最大字号的高度」的较大值（此前 14pt 标题行被裁掉半个字）；相邻格的文字溢出真正生效（`overflow: visible` + 定位提层，此前被类名里的 `overflow: hidden` 掐死，表现为"文字到格子边缘就断"）。

## Alternatives considered

- **保留深色界面下的深色表格**：Excel / WPS / LibreOffice 都是白纸，表格是"文档"不是"面板"。深色表格会让源文件里显式写死的颜色（白底、白字）全部判若两色，保真反而更差。
- **继续用逐通道插值算 tint**：改动最小，但它不是 Excel 的算法，深色主题档肉眼可见偏色。同一函数里再补 HSL 换算比换引擎便宜得多。
- **换用支持样式的表格渲染库**（如 handsontable / luckysheet）：能把"编辑"也一起做掉，但那是引入一个几百 KB 的商业风格运行时，与"现有依赖优先"冲突；当前 ExcelJS 已经能读回全部 OOXML 样式，缺的是渲染规则不是解析能力。
- **用 `background-image` 画网格线**：不与源文件自带边框打架，但行高不均匀时渐变线会错位，只能按固定行高用 —— 而 Excel 的行高常常是自定义的。

## Consequences

- 表格在深色界面里仍然是一块白底（与 Office 一致），不再随主题变色；这是有意保留的"文档感"。
- 浅色 tint 与 Excel 仍有每通道 ≤1 的舍入差（Excel 内部用 HLSMAX=240 的整数换算，舍入点落在半整数上），肉眼不可见；深色 tint 逐位相同。
- 共享预览层多了一组必须由两端应用定义的 token —— 以后新增 `--sheet-*` 之外的令牌，两端都要跟着定义，否则 `check-preview-layer.mjs` 会拦下。
- 表格默认行高从"一律 20px"变成"按内容算"，行数多的表视觉上会比以前略高一点点。
