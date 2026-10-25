# Decision Note: 预览交互补齐 —— PDF 旋转与快捷缩放、图片拖动提示、图标渐变 id 唯一化

Status: implemented — PDF 加旋转按钮与 Ctrl+滚轮 / Ctrl ±/0 快捷缩放；图片加双击适应↔100% 与 grab 光标；FileTypeIcon 渐变 id 改为每实例唯一

## Decision

用户实测报告的三个预览问题，逐一根因修复（全部在共享包 `@hermix/preview`，
viewer 与 desktop 同时受益）。

### 1. PDF：旋转 + 快捷缩放（PdfView）

- **旋转**：工具条加「顺时针 90°」按钮。pdf.js 的 viewport `rotation`
  是**绝对角度**（替换页面自带 `/Rotate`），自转量必须叠加
  `page.rotate`；fit 基准也按旋转后的宽算（转 90° 后宽高互换，
  不换基准会超宽）。Ctrl+0 与缩放一起复位。
- **快捷缩放**：Ctrl+滚轮 / Ctrl+= / Ctrl+-（±0.25，0.5~3 clamp）。
  滚轮必须**原生 `addEventListener({ passive: false })`**——React 的
  wheel 委托是 passive 的，`preventDefault` 拦不住浏览器页面缩放。
  键盘挂 window（canvas 无焦点概念，焦点在列表里也应可用）；
  缩放前记录滚动容器中心的内容比例锚点，渲染完成后恢复——否则
  放大直接跳回顶部，连续放大读长文档没法用。

### 2. 图片：拖动提示 + 双击 100%（ImageView）

实测滚轮缩放与拖动平移本来就正常，用户觉得「放大只能在区域内」的
根源是**没有任何可拖的提示**：光标是默认箭头，放大后不知道能拖着看
边缘。修复：CSS 光标 `grab` / `:active grabbing`；再加双击切换
（放大态→适应，适应态→100% 实际像素——img 被 max-width 压小过，
实际大小是 `naturalWidth / clientWidth` 之比，不能写死 1）。

### 3. 图标变白：渐变 id 每实例唯一（FileTypeIcon）

预览态下左侧详情列表的文件图标只剩白色字母（浅色主题 = 看不见）。
根因不是 CSS：FileTypeIcon 的渐变 id 按 `(variant,label,bg)` 生成，
同一目录十个 PDF 就是十个相同 id 的 `<linearGradient>`，`url(#…)`
全部解析到**文档顺序第一个**实例；而大图标网格与详情列表同时挂在
DOM（CSS 切显隐），预览态下网格被 `display:none`——Chromium 里被
引用的渐变定义随之失效，`fill` 整体丢弃，只剩白色 label。浏览态
网格可见所以正常，「预览态才变白」正暴露了这一点。修复：id 改用
`useId()`（去掉返回值里的 `:`）保证每实例唯一，引用永远解析到
自己的 defs。

## Alternatives considered

- **PDF 缩放锚点不做、只改 scale**：实现省 10 行，但每次放大视口
  跳回顶部，快捷键连续放大形同虚设。
- **图片去掉 max-width/max-height、初始按原始像素显示**：大图会
  溢出需要手动缩小，违反「打开就能看全」的预期；保留适应 + 双击
  100% 是通用查看器的标准折中。
- **给旋转/缩放加文字标签**：图片工具条四个小图标已拥挤，title
  提示已含快捷键说明；PDF 工具条同理。
- **FileTypeIcon 改为纯 fill 色值不用渐变**：能绕开 id 问题，但
  丢掉图标深度；useId 一行解决且不改变视觉。

## Consequences

- `FileTypeIcon` 是两端共用组件，desktop 的列表/正文图标同样修复；
  useId 要求组件必须是 hook 上下文（该组件本就是函数组件，无影响）；
- PdfView 新增 window 级 keydown，仅在 PDF 预览挂载期间生效，
  Ctrl+0/± 会 `preventDefault`（拦下 WebView 页面缩放，这正是目的）；
- preview-doc.css 新增两条图片光标规则，`check-preview-layer` 通过。

验证：viewer 与 desktop `tsc --noEmit` 均 0 错；viewer `vite build`
通过；`check-preview-layer` OK；`impeccable detect` 三文件 exit 0；
debug 实例实测——预览态列表图标恢复彩色、Ctrl+= 连按 100%→150% 且
滚动锚定、旋转按钮转 90° 后宽度基准正确、Ctrl+0 复位。
