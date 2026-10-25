# Decision Note: 查看器预览区顶部合行 + 铺满窗口态

Status: implemented — 标签条吸收「编辑 / 钉住 / 视图切换」动作区，文件名工具行删除；布局状态机新增 `preview-full` 铺满态（目录 / AI / 驱动器栏让位，Esc 退出）

## Decision

用户实测反馈的两个预览区体验问题：图片放大后被左侧目录栏挡住四分之一
窗口「铺不满」；预览区顶部标签条 + 文件名工具行叠了两行（文件名显示
两遍）。

### 1. 顶部合行：动作区 portal 进标签条

预览区顶部原是三行：标签条（App 渲染）、preview-toolbar（文件名 +
编辑 / 钉住 / 视图切换，PreviewHost 渲染）、缩放分页条（各渲染器自带）。
中间一行与标签条的重复只有「文件名」。

- `PreviewHost` 把工具控件抽成 `controls`：heavy 模式经
  `createPortal` 投进标签条右端动作区（`.tabbar-actions`，App 持有
  该 DOM 节点作为 prop 传入）；纯净模式没有标签条，走自带工具行并
  显示文件名（`showName=false` 分支不变）。
- 控件高度从 28px 压到 22px（`.tabbar-actions` 局部规则），与
  `tab-list-btn` 同档，标签行仍是"一行工具"。
- 时序无忧：App mount 完成后 lazy 的 PreviewHost 才 resolve，首渲时
  `actionsSlot` prop 已是真实节点，不会闪一行空档。
- 铺满按钮（`Maximize2` / `Shrink` 切换）由 App 直接渲染在动作区旁，
  与「已打开文件列表」「关闭预览」并排。

预览区顶部从三行减到两行（省 ≈34px），文件名不再出现两遍。

### 2. 铺满窗口态：状态机加第四态

`layoutState` 增加 `"preview-full"`（heavy 且有活动标签且
`previewFull`）：CSS 让 `.sidebar` / `.explorer` / `.ai-side` 全部
`display:none`——与 preview-ai 态同款机制，组件**不卸载**，目录树
展开状态、AI 对话、驱动器栏都保留，退出即恢复。

- 入口：标签条右端 ⤢ 按钮（toggle）；**Esc 退出**（全局快捷键，
  优先级在右键菜单之后、退搜索之前）。
- 「关闭预览」（`closeAllTabs`）时重置 `previewFull`，铺满态不带到
  下一次预览；回浏览态由状态机自动无害（browse 分支不受
  `previewFull` 影响）。
- 缩放 / 拖动 / 双击 100% 与铺满正交：铺满只是给了图片更大的舞台。

## Alternatives considered

- **图片预览态自动隐藏目录**：切标签闪隐，「看图就丢目录」太激进；
  显式开关 + Esc 是可预期的往返。
- **灯箱全屏浮层**（desktop 的 ImageLightbox 模式）：效果相同但要
  多管一层浮层和网页子窗口让位；铺满态复用布局状态机零新增层级。
- **工具按钮上提进 App**（不用 portal）：按钮状态（editor / pinError /
  officeMode / zoom）全在 PreviewHost，上提要连状态一起搬，改动面大；
  portal 只渡 DOM，状态结构不动。
- **缩放条也并入标签条**：它是渲染器自带的（PDF 分页、图片工具），
  与内容紧贴；并入标签条会让「翻页」离画布隔一层，保留原位。

## Consequences

- 预览区顶部少一行；heavy 模式文件名只出现在标签上。
- 铺满态下目录 / AI / 驱动器栏全部让位，Esc 或再点一次按钮恢复；
  AI 让位是 display:none，对话与滚动位置保留。
- `layoutState` 类型扩为五值；`viewerControl.scrapeState` 的 mode
  判定（pure/heavy）不受影响。
- 依据前作：布局状态机三态见
  [2026-10-24-viewer-preview-priority-layout.md](2026-10-24-viewer-preview-priority-layout.md)。

验证：viewer `tsc --noEmit` 0 错；`vite build` 通过；`impeccable
detect`（App.tsx / PreviewHost.tsx）exit 0；`check-preview-layer` OK；
debug 实例实测——标签条右端动作区正常（编辑 / 钉住 / 铺满 / 列表 /
关闭预览一行）、文件名行消失、铺满后预览独占全窗（900x600 图完整
显示）、Esc 退出目录栏原样恢复。
