# Decision Note: 右键菜单换成应用菜单 —— 桌面端不再端出浏览器的网页菜单

Status: implemented — 主窗与文件搜索窗全局接管 `contextmenu`，工作台 iframe 经 SDK 转发；菜单为共享原语 `ui/ContextMenu`

## Decision

桌面应用的右键菜单是**应用菜单**，不是网页菜单。Chromium 的原生右键菜单带
"重新加载 / 检查 / 后台打开 / 书签"这类网页专属项，在桌面壳里既没意义，
又把界面拉回浏览器观感。

### 1. 菜单本体是共享原语

`apps/desktop-tauri/src/components/ui/ContextMenu.tsx`：浮起的面 + 阴影 + 圆角，
**不描边**（用户纪律：UI 一律不用描边，层次靠底色差 + 阴影 + 间距）。
键盘 ↑/↓ 移动、Enter 执行、Esc 关闭；贴视口边缘自动翻转。

### 2. 全局接管，右键永远不给网页菜单

`components/GlobalContextMenu.tsx` 挂在应用根（主窗 + 文件搜索窗各一次）：

- **捕获阶段** `preventDefault()` —— 原生网页菜单永远不出现；
- 右键目标决定菜单项：输入框 / 文本域 / contenteditable 给
  剪切、复制、粘贴、全选；有选区给复制；`<img>` 给复制图片；
  `<a href>` 给复制链接地址、打开链接；其余给全选兜底。
- 组件自己处理右键时（文件管理器的文件菜单、工作台 tab 的菜单）用
  `markContextMenuHandled(e)` 标记，全局菜单让位 —— 标记是同步的：
  捕获阶段已经 preventDefault，组件的处理器在冒泡阶段才跑，两者之间
  没有等待窗口，只能靠事件上的标记传递"我已经处理了"。

### 3. 工作台 iframe 走 SDK 转发

工作台画布是 `asset://` 另一个源，宿主读不到它的 DOM，浏览器菜单会在
iframe 里照弹。宿主 SDK（`wb-sdk.js`）拦截 `contextmenu`、
`preventDefault()`，把坐标、选区、链接 `postMessage` 给宿主；
宿主用同一套菜单渲染，坐标按 iframe 的位置换算成视口坐标。

剪贴板动作一律在**宿主文档**执行（宿主有用户手势，`navigator.clipboard`
可用）；iframe 里的选区文本随消息带过来。

## Alternatives considered

- **引入 `tauri-plugin-context-menu`（原生 Windows 菜单）** —— 否决。
  本地 cargo registry 无此 crate（离线环境拉不到），且原生菜单与本应用
  的设计语言（纸墨体系、无描边、语义令牌）不统一；应用自绘菜单能直接用
  `--color-bg-elevated` / `--shadow-pop` 等既有令牌。

- **只在需要的地方（输入框、文件列表）加右键菜单** —— 否决。
  用户反馈的是"项目各处界面"的右键菜单是网页菜单，逐处补是打地鼠；
  全局接管 + 按目标算菜单项是一处实现覆盖全界面。

- **把 `contextmenu` 的 preventDefault 交给每个组件** —— 否决。
  漏一个就是一处网页菜单；而且组件自己弹菜单时还得跟全局菜单打架
  （现在用标记协调）。

- **让 iframe 用 `document.execCommand('copy')` 自己复制** —— 否决。
  跨源 iframe 拿不到用户手势，`execCommand` / `navigator.clipboard` 都会
  被拦。改成宿主代劳：文本随消息带过来，宿主写系统剪贴板。

## Consequences

- 主窗内**任何**右键都不再弹浏览器菜单，包括此前没处理过的滚动区、
  空白处、代码块、图表等。想让用户复制网页地址？用"复制链接地址"项。

- 菜单项由右键目标推导，不是固定一套。极冷门的目标（如 SVG 内部）
  落到"全选"兜底。

- "粘贴"依赖 `navigator.clipboard.readText()`，被权限拒绝时 toast 提示
  用户用 Ctrl+V，不静默失败。

- "复制图片"用 canvas 导出 PNG 再写剪贴板；跨源图片（canvas 被污染）
  会失败，toast 报错，不假装成功。

- 浏览器小窗（`#/browser-view`）**不**接管 —— 那里本来就是网页，
  原生菜单（复制地址、另存图片）是正确行为。
