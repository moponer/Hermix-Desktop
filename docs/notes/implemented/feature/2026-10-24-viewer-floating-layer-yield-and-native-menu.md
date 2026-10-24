# Decision Note: 查看器浮层让位派生化 + 菜单统一 Win11 原生规格

Status: implemented — 任何浮层展开时网页子 webview 让位（`web_set_visible`）由状态派生布尔驱动；标题栏下拉与右键菜单共用一套"Win11 原生风"规格与行组件（`MenuRow.tsx`）

## Decision

两件事同一天拍板，因为它们是同一个用户报告的两半：「内置浏览器执行自动化
任务时，点开其他菜单界面被挡在浏览器后面」「窗口菜单显示效果过于网页」。

### 1. 浮层让位：命令计数器 → 状态派生布尔

网页是**独立的子 webview（独立 HWND）**，永远画在父 webview 的 DOM 之上
——z-index 管不到，不是同一层。浮层展开时把网页 `hide()`（不是 `close()`，
关掉会丢页面状态），全部收起后 `show()` 回来。这条机制 2026-10-21 随
应用内浏览器落地（见
[2026-10-21-viewer-agent-driven-in-app-browser.md](2026-10-21-viewer-agent-driven-in-app-browser.md)，
其中"菜单让位"描述的计数器实现已被本文取代），本次改的是
**驱动方式与覆盖面**：

- **旧**：`menuDepthRef` 计数器，只有标题栏菜单和右键菜单两处接线；右键
  菜单经**点选 / Esc / 遮罩点击**关闭的路径从不减一 → 计数器泄漏，网页
  会一直藏到下次滚动才恢复。设置 / 系统面板 / 关于 / 浏览器面板 / 标签
  列表弹层**完全没接线**，网页 tab 激活时全被盖住（标签列表还物理落在
  预览列正上方）。
- **新**：`App.tsx` 里 `floatingChromeOpen = menu !== null || menubarOpen ||
  sysOpen || aboutOpen || settingsOpen || browserOpen || tabListOpen` 一个
  派生布尔 + 一个 effect 驱动 `web_set_visible`。所有浮层的开合都是
  React 状态，「忘了减」在结构上不可能——状态回 false，网页就恢复。
  Agent 侧 `open_panel{...}` 走同一批 setter，自动覆盖。

**不参与让位**：toast（短暂且非交互，藏网页反而闪）；拖放遮罩（拖放事件
被子 HWND 吃掉是另一个维度的问题，见 Consequences）。

### 2. 菜单统一 Win11 原生规格，行渲染收口一处

「过于网页」的来源是两套自绘菜单各写一套且都偏网页下拉：圆角 9/10、
内缩胶囊高亮、大而柔的漂浮投影、滑入位移、图标 13/15、字号 12.5/13。
统一为桌面菜单的样子（新 token，`styles.css` 菜单区是唯一权威）：

| 项 | 统一后 |
| --- | --- |
| 表面 | `--radius-menu: 8px`、`--shadow-menu`（收紧变硬）、4px 内边距 |
| 行 | **整行高亮**（高亮占满行宽，不是内缩胶囊）、`--radius-menu-item: 4px`、padding 5×10 |
| 图标 | `--icon-size-menu: 14px`，槽位 CSS 压住 lucide 的 size 属性（两处 13/15 归一） |
| 字号 | 12.5px（≈Win 原生 12px） |
| 分隔线 | 全宽 hairline（右键菜单组间留白改为分隔线，标题栏分隔线收窄为 4px 边距） |
| 入场 | 80ms 纯 opacity 淡入，无位移（位移是网页下拉的特征） |

- **行组件 `src/MenuRow.tsx`**：勾选 / 行首槽 / 文字 / 快捷键一处渲染，
  两个菜单共用。行首两种形态对应两种信息结构：标题栏菜单**勾选列常驻**
  （视图菜单的项可同时有勾选和图标）；右键菜单勾选与图标共用一个槽
  （Windows 习惯，二选一）。外层 `<button>`（角色 / 禁用语义 / 事件）
  仍由各菜单自定。
- **样式收口**：`.menubar-pop` 的表面 / 行样式从 `system-panel.css` 迁入
  `styles.css` 菜单区（两套菜单一份规格，分居两文件正是此前漂移的根源）；
  `system-panel.css` 只留菜单栏本体（`.menubar` / `.menubar-btn`）。
- **z-index 阶梯成文**：`styles.css` 菜单区注释是全窗口唯一权威表
  （60 小浮层 / 99-100 右键 / 120 toast / 130 菜单栏 / 140 次级模态 /
  200 顶级模态）。`.bpanel-overlay` 60→140：模态不得压不过右键菜单。
  `.tab-list-pop` 表面换用菜单 token。
- 顺带修掉一个既有 bug：右键菜单的勾选号 `<Check />` 没传 size（lucide
  默认 24px，溢出 15px 容器）比图标大一圈——槽位 CSS 现在统一压住。

## Alternatives considered

- **让浮层真正画在网页之上**（子 webview 下沉 / 浮层开独立置顶窗口）：
  子 HWND 的叠放由 Windows 窗口管理器管，webview 嵌在主窗口里没有独立
  z 轴可调；浮层各开窗口则失去 DOM 菜单的统一交互（键盘导航、焦点管理
  全要跨窗口重做）。放弃。
- **保留计数器，补齐关闭路径**：每加一个浮层都要记得在**所有**关闭路径
  减一（点选 / Esc / 遮罩 / 滚动 / 焦点丢失），漏一处就是这次的 bug。
  派生布尔把这条纪律从「靠人记」变成「结构保证」。放弃补丁。
- **右键菜单组间保留纯留白**（原注释称"硬规矩是不描边"）：该规矩针对
  浮面描边（边框），hairline 分隔线是菜单内容的一部分（标题栏菜单一直
  有 `.menu-sep`）；长菜单里留白读不出组边界，原生菜单用线。改为分隔线。
- **引入 Radix / headless 菜单库**：两处菜单的定位 / 翻转 / 键盘导航已经
  各自可用且行为不同（槽位锚定 vs 视口点锚定），为统一视觉引库不值
  （六步阶梯第 4/5 步：现有依赖与最小实现优先）。放弃。

## Consequences

- 网页 tab 激活时打开任何浮层，页面短暂消失（露出底下的空占位）——这是
  HWND 让位的固有时序，菜单合上即恢复；对话框类浮层本就有全屏遮罩，
  视觉上无感。
- `web_set_visible` 在浮层开合瞬间可能连续调用，Rust 侧是幂等的
  show/hide，无副作用；没有网页时是安全空操作。
- Agent 正在自动化网页时若用户开浮层，页面被 `hide()` 但 CDP 会话不受
  影响（自动化照常跑，`captureScreenshot` 强制出帧）。
- 拖放遮罩与 toast 仍可能被网页盖住：前者要等「拖放事件被子 HWND 吞掉」
  的问题单独解决（Windows 拖放到子 webview 需要原生侧 AllowDrop 转发），
  后者接受（2.6s 自动消失，不值得为它闪整个网页）。
- viewer 前端没有测试基建（无 vitest），本次行为面（派生布尔）与视觉面
  （CSS token）都不在 ci-gate 覆盖内；`tsc --noEmit` 与
  `impeccable detect` 需手动跑（impeccable 对 viewer 报的 5 处
  `transition: width` 是系统面板进度条的既有项，与本次无关）。
