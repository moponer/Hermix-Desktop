# Decision Note: 搜索空态与状态反馈、Word 编辑说明面板收起

Status: implemented — 详情表空状态移出表格（修表头空白）；搜索进行中改 spinner；DocxEditor 说明侧栏默认收起为浮动 ⓘ

## Decision

用户实测报告的三个查看器问题，一处布局 bug、两处交互优化。

### 1. 搜索空态表头「空白一块」（App.tsx + styles.css）

预览态窄目录槽（≤340px）下容器查询隐藏「修改时间」列，表格只剩
2 列；而空状态行是 `<td colSpan={3}>`——span 溢出列数时浏览器创建
**匿名第 3 列**，在 `table-layout: fixed` 下分走一块宽度，表头右侧
就是那块空白。搜到结果后 tbody 有真实行，列结构正常，所以「有索引
到文件后就不空白了」。

修复：空状态**移出表格**——`{visibleEntries.length === 0}` 的文案
渲染为表格后的 `.entries-empty`（居中一行），tbody 只在有行时渲染；
表头列结构恒定。大图网格的空态原本就是独立 div（`grid-column: 1/-1`
铺满），不受影响、保持原样。

### 2. 搜索状态：spinner + 底部任务条（App.tsx + styles.css）

搜索状态（进行中 / 范围 / 命中数）最初从平铺提权长文案改为 spinner，
仍挂在搜索框旁边——但标签一变长搜索框就被挤窄，文案一变宽度就跳。
最终归位：**底部新建任务条（`.statusbar`）**，动态信息统一住那里，
搜索框旁边不再有任何随文案伸缩的东西。

- 任务条固定 24px（`.app` 底部 flex 尾项，`role="status"` +
  `aria-live="polite"`）：搜索中显示 `⟳ 搜索中…`（Loader2 +
  `search-rotate` 动画，授权/装载细节挂 `title` 悬停可见）；完成显示
  「全盘索引 命中 N 条，显示前 N 条」/「仅在当前目录内搜索」；空闲
  显示「就绪」。
- 工具栏上的 `.search-scope` 标签及其样式整体删除；
- 空态文案跟随表格显隐：`.entries-empty` 从表格里挪出来后是独立
  元素，补 `.explorer[data-view="grid"] .entries-empty { display:none }`，
  大图视图下不再与网格空态重复显示。

### 3. Word 编辑说明侧栏：默认收起（DocxEditor.tsx）

`ed-side`（状态 + 四条使用说明 + 导出反馈）固定 248px，窄预览栏里
正文档被挤没——说明是读一次就够的东西，不该常驻。

- **默认收起**；右下角浮动 ⓘ（`.ed-side-fab`，absolute 圆钮，不占
  布局宽）点击展开，面板头部 × 收起；
- 有 `note`（「已导出…」等动态反馈）时 ⓘ 带**红点**（`.ed-side-dot`），
  收起不吞掉导出结果提示；
- `ed-body` 补 `position: relative` 作 fab 锚；`.img-params`
  （ImageEditor 参数面板）不受影响——它是操作参数不是说明，保留常驻。

## Alternatives considered

- **colSpan 改 2**：列数由容器查询决定，JS 不知道当前隐藏了几列，
  写死哪个数都会在另一档宽度再犯；移出表格是唯一列结构无关的解。
- **提权提示完全删除**：首次建索引确实要 UAC，静默弹窗更吓人；留
  在 title 里，想知道的人 hover 可见。
- **说明内容并进工具条 tooltip**：四条说明合计 ~150 字，tooltip
  装不下且不可滚动；浮动面板可停留细读。
- **note 改 toast**：导出反馈本是 note 的一部分，红点 + 展开可见
  足够，不再引入第二套提示机制。

## Consequences

- 搜索空态表头列结构恒定，任何宽度下不再出现匿名列空白；
- 搜索状态全部住底部任务条：高度固定，内容更新零布局位移；搜索框
  宽度稳定；工具栏少一个随文案伸缩的元素；
- Word 编辑默认全宽文档，说明按需展开；ImageEditor 参数面板行为
  不变；
- `.empty-cell` 样式仍被大图网格使用，未删；`.search-scope` 死样式
  已随迁移删除。

验证：viewer `tsc --noEmit` 0 错；`vite build` 通过；`impeccable
detect`（App.tsx / DocxEditor.tsx）exit 0；`check-preview-layer` OK；
debug 实例实测——搜索中「⟳ 搜索中…」动效出现且无提权文案、「正在
搜索…」/「无搜索结果」空态表头满宽无空白、docx 编辑说明默认收起
（ⓘ 展开出状态面板、× 收回、文档恢复全宽）。
