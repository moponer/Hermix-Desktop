# Decision Note: 文档预览统一为连续滚动（PDF / PPTX / Word）

Status: implemented — PDF / PPTX / DOCX 三种预览都是连续纵向滚动 + 惰性渲染，左侧可选缩略图侧栏

## Decision

所有"有页"的文档预览（PDF、PPTX、DOCX）一律**连续纵向滚动**，不再提供
"一页一页翻"的主路径：

- 所有页铺成一列，正文区自己滚；页码指示器由滚动位置反推（视口 40% 高度处
  落在哪页）。前后按钮退化为"滚到上一页/下一页"。
- 每页只有进入视口 ±800px 才渲染（PDF/PPTX 是 canvas，Word 是库生成的
  DOM 节）。占位页按页面字典算出的比例先占高度，滚动条不会随渲染一截一截长。
- 左侧可选缩略图侧栏：宽面板（≥520px，全屏 / 独立查看器）默认打开，窄栏
  （桌面右栏 ~380px）默认关闭。缩略图同样惰性——侧栏滚到哪画到哪。
- PDF 保留缩放（`fit × scale`，凡 1.0 = 适应宽度）与 90° 旋转；缩放/旋转前
  记「顶部那一页 + 页内相对位置」锚点，重排后还原，不跳回顶部。

实现分层（事实单一归属）：

| 文件 | 职责 |
| --- | --- |
| `packages/preview/src/lib/usePageScroll.ts` | 滚动位置 ↔ 页码、跳页、视口锚点。三种渲染器共用 |
| `packages/preview/src/viewers/DocPager.tsx` | 工具条 + 页列 + 缩略图侧栏 + 惰性渲染；PDF/PPTX 共用 |
| `viewers/PdfView.tsx` `viewers/PptxView.tsx` | 只回答"某页在给定宽度下怎么画" |
| `viewers/DocxView.tsx` | docx-preview 一次出整篇 DOM，自己那套滚动；缩略图用 `scale()` 克隆 |

Word 的"页"不能按页重画（库没有单页渲染），所以它不进 `DocPager`；缩略图
用克隆 + `transform: scale()`，只为侧栏里看得见的那几页做。

## Alternatives considered

- **保持一页一页翻，只加键盘/滚轮翻页**：用户第一次拿着 7 页 PDF 就撞上
  "翻页"这件事，加键位只是把问题藏起来；连续滚动是阅读器（Chrome、Acrobat、
  Word）的共同形态。
- **Word 也塞进 `DocPager`**：需要把整篇 DOM 拆成"单页可重画"的单元，等于
  重写 docx-preview 的分页；克隆缩略图成本低一个数量级。
- **缩略图常开**：桌面右栏只有 ~380px，常开会把正文挤到一半宽；改为按
  容器宽度决定默认值，用户仍可手动开关。
- **一次性渲染所有页**：50 页 PPT 每页都要解包 + 解析 slide XML，首屏会被
  拖到几秒；惰性渲染把首屏成本压到一屏的页数。
- **缩略图用截图（html2canvas）**：新依赖 + 对 canvas/DOM 混合内容不可靠。

## Consequences

- 三个渲染器不再各自持有"第几页"状态：页码、跳页、锚点只有一份实现
  （`usePageScroll`），新增文档类型只需提供 `sizes` 与两个渲染回调。
- 画过的 canvas 不回收（滚回来不重绘），几百页的超大文档内存会上去；
  真遇到再加"远离视口降级成占位"。
- PDF 铺版前要为每页取一次页面字典（只取尺寸、不解码内容），是一次性开销。
- 编辑面（`PdfPageEditor` / `PptxEditor`）仍是单页：编辑的对象就是当前这一页/
  这一张幻灯片，页列表本身就是导航；这次不改它们。
