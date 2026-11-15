# Decision Note: viewer 自绘右键菜单 + 自绘窗口标题栏

Status: implemented — viewer 的页面任何区域都不再出现浏览器/WebView2 原生右键菜单；换成本项目自绘的一套文件操作菜单（按落点动态生成、分组、禁用态、快捷键提示、随屏幕边界自适应、点空白或 Esc 关闭、浅深两套主题都成立、触摸端长按可用）。同时隐藏系统标题栏，自绘一条与主题一致的标题栏，支持拖拽移动窗口、最小化 / 最大化 / 关闭。

关联：[hermix-viewer 双模式化与全能力重构](../feature/2026-10-19-hermix-viewer-overhaul.md)、[viewer 可执行文件立即运行（360 误报始末）](../feature/2026-10-20-viewer-executable-run-immediately.md)、[viewer 开放能力（MCP）](../feature/2026-10-20-viewer-open-capabilities-mcp.md)

## Decision

**1. 全局拦截原生右键，页面任何区域都不出现默认菜单项。**

拦截点在 `App.tsx` 的 `document` **capture** 阶段：`contextmenu` 事件一律 `preventDefault()`，于是 WebView2 的原生菜单（重新加载 / 查看源代码 / 检查）对**任何**元素都不再出现——文件列表、目录树、预览正文、输入框、HTML 沙箱 iframe 一视同仁。

**2. 菜单内容按落点动态生成，模型与呈现分离。**

- `apps/viewer/src/contextMenuModel.tsx` ——「给这个落点摆什么菜」：`buildMenu(target, ctx)` 返回**分组**的菜单项。
- `apps/viewer/src/ContextMenu.tsx` ——「怎么摆、怎么关、怎么用键盘走」：定位、翻转、遮罩、Esc、方向键。
- 落点靠 DOM 上的 `data-ctx` 系列属性识别（`data-ctx="entry|explorer|tab|preview|pathbar|field"` + `data-ctx-path/name/dir/from/id`），全局拦截用 `closest("[data-ctx]")` 找到落点。新增一类落点 = 加一个 kind，不必给每个组件挂 `onContextMenu`。

落点与菜单：

| 落点 | 菜单 |
|------|------|
| 文件 | 打开 / 用系统默认程序打开 · 在资源管理器中显示 / 复制完整路径 / 复制文件 · 用记事本打开 / 用 VS Code 打开 · 加入 AI 对话 / 添加书签 · 属性 |
| 文件夹 | 打开文件夹 · 在资源管理器中显示 / 复制完整路径 / 复制文件夹 · 添加书签 · 属性 |
| 列表空白 | 刷新 / 上一级 · 按名称 / 按大小 / 按修改时间排序 · 显示隐藏文件 · 粘贴文件 |
| 目录树节点 | 打开 · 在资源管理器中显示 / 复制完整路径 · 刷新此节点 |
| 预览标签 | 关闭标签 / 关闭其他 / 关闭全部 · 复制完整路径 / 在资源管理器中显示 · 用系统默认程序打开 |
| 预览区 | 复制完整路径 / 在资源管理器中显示 · 用系统默认程序打开 / 加入 AI 对话 · 复制选中文本 |
| 路径栏 | 粘贴 / 复制选区 / 全选 · 复制当前路径 |
| 输入框（搜索框、AI 栏） | 粘贴 / 复制选区 / 全选 |
| 其它（工具条、侧栏空白） | 刷新 · 复制当前路径 · 浅色 / 深色 / 跟随系统 · 纯净 / 重量模式 |

**2.1 输入框的落点必须**先于** `data-ctx` 祖先判定。**

第一版用 `e.target.closest("[data-ctx]")` 找落点，但根容器 `.app` 自己带 `data-ctx="app"`——搜索框、AI 输入框没有 `data-ctx`，`closest` 一路向上就撞到 `.app`，于是它们的右键拿到的是「应用菜单」（刷新 / 排序 / 切主题），文本编辑（粘贴 / 复制选区 / 全选）**整组消失**：拦掉原生右键之后，用户在搜索框里连粘贴都做不了。修法：命中元素本身是 `INPUT` / `TEXTAREA` / `contentEditable` 时**直接把它当落点**，不再往上找 `data-ctx`。

顺带把「复制选区」的禁用态也接上真实状态：菜单打开瞬间读输入框的 `selectionStart/selectionEnd`，没选区就置灰（与预览区「复制选中文本」同一套规矩）。

**2.2 往受控输入框写值，必须走原型上的原生 setter。**

React 在元素**实例**上劫持了 `value`（用它记「上一次的值」来判断 `onChange` 要不要触发）。`el.value = next` 走的是被劫持的 setter，React 认为「值没变」→ `onChange` 不触发 → 受控组件下一帧就把新值弹回去，看起来就是「粘贴没反应」。正确写法是取原型上的原生 setter 绕过劫持：

```ts
const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value")?.set;
setter?.call(el, next);
el.dispatchEvent(new Event("input", { bubbles: true }));
```


**3. 菜单里不放占位项；禁用态按真实状态给。**

「用 VS Code 打开」由 Rust 侧 `viewer_available_apps` 探测（`code.exe` 在不在 PATH 里），**没装就置灰**而不是给一个点了才报错的假按钮；「复制选中文本」没选中文本时置灰；「关闭其他标签」只有一个标签时置灰。快捷键提示只写**真的能按**的键（全局有对应监听）。

**4. 文件「复制 / 粘贴」走 Win32 剪贴板（CF_HDROP），不走浏览器 Clipboard API。**

浏览器 Clipboard API 写不了「文件列表」；`navigator.clipboard.readText()` 在 WebView2 里还要 clipboard-read 权限，而 WebView2 没有权限弹窗——直接失败。所以新增 `apps/viewer/src-tauri/src/clipboard.rs`：`copy_to_clipboard`（拼 DROPFILES 头 + UTF-16 路径表）、`paste_from_clipboard`（`DragQueryFileW`）、`read_text` / `write_text`（CF_UNICODETEXT，路径复制与路径栏粘贴都走它）。粘贴遇到同名项**跳过不覆盖**。

**5. 所有「viewer 让系统去做点什么」的动作一律走 `ShellExecuteExW`，不用 `Command::new(..).spawn()`。**

新增 `shell_exec(verb, file, params, working_dir)` 统一封装；`viewer_reveal_in_explorer` 顺带从 `Command::new("explorer.exe").spawn()` 改过来（同样理由：360 行为监控会把「自己创建子进程」判成木马，2026-10-20 已有前车之鉴）。「打开方式」支持 `default` / `notepad` / `code` / `properties` / `reveal`。

**6. 隐藏系统标题栏，自绘标题栏。**

`tauri.conf.json` 加 `"decorations": false`。新组件 `apps/viewer/src/TitleBar.tsx`：应用名 + 当前文件/目录 + 最小化 / 最大化（还原）/ 关闭。拖拽移动窗口走 `getCurrentWindow().startDragging()`（在 `pointerdown` 里调），**不用** `data-tauri-drag-region`——后者靠 `closest()` 判定，标题栏里的按钮会被一起拖走，点「关闭」会变成拖窗口。标题栏底色用 `--bg-primary`，与下面工具条的 `--bg-tabbar` 自然分出两段，不画分隔线。

> 2026-10-21 更新：标题栏不再显示当前文件/目录——文件名与网址归纯净模式顶栏的地址栏，文档名改走窗口标题（`setTitle`）。见 [viewer 顶栏改浏览器式地址栏](2026-10-21-viewer-topbar-address-bar.md)。

**6.1 Tauri v2 权限：窗口操作必须显式授权。** `apps/viewer/src-tauri/capabilities/default.json` 原来只有 `core:window:allow-close` / `allow-set-title`，于是 `start_dragging` / `minimize` / `toggle_maximize` / `is_maximized` 全部被 ACL 拒绝（报错文案：`window.start_dragging not allowed. Permissions associated with this command: core:window:allow-start-dragging`）。自绘标题栏要用的这四个权限已补进 capability 文件。

**7. `decorations: false` 会连窗口边缘缩放一起弄丢，必须补回来。**

tao 在无边框时去掉 `WS_CAPTION` 与 `WS_THICKFRAME`（`tao/src/platform_impl/windows/window_state.rs`），窗口就变成固定大小。Rust 侧在 `on_window_event`（`Resized` / `Focused(true)`）里把 `WS_THICKFRAME` 加回去（**不动** `WS_CAPTION`，所以系统仍不画原生标题栏），并 `SetWindowPos(SWP_FRAMECHANGED)` 让系统重算非客户区——否则命中测试仍按旧样式走，看起来就是「加了样式也拖不动」。

实测：拖右边缘宽度 1216→1316、拖左边缘左边界 360→260，边缘缩放正常。

（备选方案：Tauri 2.12 的 JS API 有 `startResizeDragging(direction)`，可以自己画 8 个缩放热区。本轮没走那条路——WS_THICKFRAME 补回后系统原生边缘缩放即可用，不必再叠一层前端热区。留着 `startResizeDragging` 作退路。）

**8. 排序、快捷键与反馈。**

- 排序方式（名称 / 大小 / 修改时间）进右键菜单并持久化；目录永远在前（资源管理器惯例）。
- 全局快捷键：`F5` 刷新、`Ctrl+Shift+C` 复制路径、`Ctrl+W` 关标签、`Ctrl+V` 粘贴文件、`Alt+←/→/↑` 后退/前进/上一级、`Esc` 关菜单或退出搜索。输入框内让位（交给输入框自己处理）。
- 动作结果必须看得见：新增轻提示（`.toast`），复制 / 粘贴 / 打开失败都有回执。

**8.1 菜单的键盘焦点要分清"鼠标开的"与"键盘开的"。**

Chromium 的 `contextmenu` 事件，鼠标右键与键盘菜单键都是 `detail === 0`，分不出来。若一律自动聚焦第一项，鼠标用户每次右键都会看到第一项顶着焦点环（像被选中）。做法是自己记一个"刚刚按过菜单键（`ContextMenu` / `Shift+F10`）"的标记：键盘唤起才自动聚焦第一项，鼠标唤起只把焦点放在菜单容器上（容器不画环）。

**9. 触摸端长按。**

`pointerdown`（`pointerType !== "mouse"`）后 500ms 未移动即开菜单，移动超过 10px 取消；长按后 Chromium 补发的 `contextmenu` 事件在 800ms 内忽略，避免弹两次。菜单行高在 `pointer: coarse` 下放大到手指点得中的尺寸。

## Alternatives considered

1. **保留原生标题栏，只调配色** ——拒绝（用户明确要求）：原生标题栏的配色由系统决定，浅色 / 深色两套主题都对不上；且自绘标题栏是「拖拽移动窗口」这类原生交互的前提。
2. **`data-tauri-drag-region` 做拖拽** ——拒绝：Tauri v2 用 `closest("[data-tauri-drag-region]")` 判定，标题栏里的按钮默认也落在拖拽区，点「最大化」会变成拖窗口。要规避就得给每个按钮加例外标记，比在 `pointerdown` 里自己判一次更绕。
3. **用 `tauri.conf.json` 的 `titleBarStyle: "Overlay"`** ——拒绝：那是 macOS 的能力，Windows 无效。
4. **保留边缘缩放但用 Tauri 的 `startResizing()`** ——不可用：Tauri 2.12 的 `Window` 没有这个 API（grep 全 crate 无 `start_resizing`），只有 `start_dragging`。所以走 Win32 补 `WS_THICKFRAME`。
5. **菜单做成多级子菜单（「打开方式 ▸」）** ——本轮不做：子菜单要额外的悬停延迟、翻转定位与键盘路径，成本远大于收益；平铺 + 分组间距同样能读出层次，且更符合「菜单项不绕」的使用习惯。
6. **菜单组之间画分隔线** ——拒绝：本项目的硬规矩是「一律不用描边，层次靠底色差 + 阴影 + 间距」（唯一保留的是键盘 focus-visible 焦点环）。分组靠留白读出来。
7. **用浏览器 Clipboard API 复制文件 / 读剪贴板** ——拒绝：写不了文件列表（CF_HDROP），读文本要 clipboard-read 权限而 WebView2 无权限弹窗。
8. **右键菜单只挂在文件列表行上（现状）** ——拒绝：用户要的是「页面任何区域」的菜单；而且原来的菜单只有 6 个平铺项、没有分组 / 禁用态 / 快捷键提示。
9. **粘贴时同名项弹窗询问是否覆盖** ——拒绝（本轮）：`window.confirm` 是门禁红线，且弹窗要自己实现一套确认 UI；先按「跳过同名项」处理，语义安全（绝不覆盖用户文件），代价是用户要多清一次同名文件。
10. **给搜索框 / AI 输入框单独挂 `onContextMenu`** ——拒绝：与「落点靠 `data-ctx` 属性识别」的统一约定冲突，且要改每个组件。改成「命中元素本身是输入框就优先当落点」，一处规则覆盖所有输入框。

## Consequences

- **前端新增文件 3 个**（`ContextMenu.tsx` / `contextMenuModel.tsx` / `TitleBar.tsx`），**Rust 新增模块 1 个**（`clipboard.rs`）；新增 npm 依赖：0；新增 Rust 依赖：0（`windows-sys` 只加 3 个 feature：`Win32_System_DataExchange` / `Win32_System_Memory` / `Win32_System_Ole`）。
- **窗口不再有原生标题栏**：双击标题栏 = 最大化/还原（已实现），系统菜单（Alt+Space）仍在，但右击标题栏不再弹系统菜单——这是「全局拦截原生右键」的必然结果。
- **`decorations: false` 的窗口边缘缩放靠 Win32 补样式**：这是 tao 之外的私有约定，升级 tao / tauri 时要重新验证（`keep_resize_border` 里有注释指向 tao 的实现位置）。
- **「打开方式」清单是启动时探测一次的快照**：装完 VS Code 后要重启 viewer 才不会出现置灰的「用 VS Code 打开」。
- **剪贴板文件复制是 Explorer 语义**：复制出来的文件可以在别的资源管理器窗口粘贴，也可以在 Word / VS Code 里收到。
- **快捷键提示的诚实性由全局监听保证**：`F5` / `Ctrl+Shift+C` / `Ctrl+W` / `Ctrl+V` / `Alt+方向键` 都有真实处理；如果将来某个键被 WebView2 抢走（WebView2 自带 F5 / Ctrl+R 等浏览器键），菜单提示会与实际不符——真机验证时确认过这批键没被抢。
- **Rust 侧新增 2 条单测**（DROPFILES 字节布局：路径表偏移必须等于头长、路径必须是 UTF-16 + 双 NUL 结尾），锁住「拼错偏移 → Explorer 收到乱码路径」这个最易错点。
- **拦掉原生右键 = 自己补齐浏览器原本给的能力**：输入框的「粘贴 / 复制选区 / 全选」是原生菜单提供的，拦掉之后必须逐项补回来，否则「拦得更彻底」反而是功能倒退（第一版就漏了搜索框这一处，是靠真机截图才发现的）。
- **真机验证的两条纪律**：① 截图前先确认窗口里没有用户的真实业务数据（本轮误拍过一次 G: 盘的工资表，截图已当场销毁，后续一律用裁剪后的局部图）；② 验证时**用户可能同时在操作这台机器**，脚本点击会与真人抢鼠标，脚本要把「点击 → 截图」压进同一个进程、并对结果逐张核对。

## Reintroduction conditions

- 若用户反馈「右键菜单项太多 / 找不到常用项」：优先做**用户自定义菜单项**（配置里排序列 + 显隐），而不是继续加项。
- 若需要多级子菜单（如「打开方式 ▸」「排序 ▸」）：以 `ContextMenu.tsx` 的定位逻辑为基础加一层 hover 展开，键盘路径要同步补（方向键进入 / Esc 逐级退出）。
- 若要支持「剪切 / 删除 / 重命名」：删除属破坏性操作，需要先补确认弹窗（原生命令框已在 `viewer_run_executable` 里用过，`tauri-plugin-dialog` 已在依赖里），并把删除目标限制在用户当前目录内。
- 若 WebView2 版本变化导致 F5 / Ctrl+W 被浏览器键抢走：把刷新与关标签的提示键换成不会被抢的组合（如 `Ctrl+Shift+R` / `Ctrl+Shift+W`），保持「菜单提示 = 真实可按」这一条不破。
