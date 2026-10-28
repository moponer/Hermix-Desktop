# Decision Note: 截图「文本」工具点了画布什么都不发生 —— mousedown 默认聚焦把刚挂载的输入框关掉

Status: implemented — 2026-10-28 交付：文本工具下的画布 mousedown 阻止默认聚焦，
文本框能留下、能输入、Enter / 失焦都能落字；补一条回归测试钉住根因

## Decision

现象：选「文本」工具后点画布，没有任何反应 —— 没有输入框、没有报错，文本工具
100% 不可用（不是"偶尔"）。

### 根因（实测，不是推测）

用真实组件在 vite dev 里跑：mousedown 确实命中 `CANVAS.se-canvas.tool-text`，
但 `.se-text-input` **从未进入 DOM**（探针记录 `input: null`，工具态
`activeTool: "文本"`）。

时序如下：

1. `onMouseDown` 里 `setTextBox(...)` → React 提交 → `<input autoFocus>` 挂载并**拿到焦点**；
2. 事件处理结束，浏览器执行 mousedown 的**默认动作**：canvas 不可聚焦，于是把焦点
   交给 `body` —— 刚拿到焦点的输入框立刻 `blur`；
3. `onBlur={commitTextBox}` 读到 `textBoxRef.current.value === ""` → `setTextBox(null)`
   → 输入框在出现的同一帧被移除。

于是"点画布 → 输入框出现 → 立刻消失"，肉眼就是什么都没发生。

### 修法

`onMouseDown` 的文本分支加 `e.preventDefault()`：阻止 mousedown 的默认聚焦行为，
焦点留在输入框。点击别处时仍走原来那条显式路径（`onMouseDown` 先
`commitTextBox()` 提交上一段，再开新框），`onBlur` 继续兜"点工具栏 / 点空白"，
两条路径都验证过：Enter 提交后画布出现红字（红像素 0 → 643，落点 41,38 ≈ 点击点
40,40），点别处先落字再开新框（红像素 643 不变、新框聚焦且为空），点空白走 blur
兜底落字（643 → 1049）。

顺带修掉 `toImg` 的退化：画布首帧 `getBoundingClientRect()` 是 0×0，除法给出 NaN
坐标（jsdom 与真实环境都能触发）。

回归测试：`ScreenshotEditor.test.tsx` 断言文本工具下画布 mousedown 的
`defaultPrevented === true` 且 `.se-text-input` 存活 —— jsdom 不实现浏览器的默认
聚焦行为，所以只能钉根因；真实浏览器行为靠本轮 playwright 探针验证。

## Alternatives considered

- **去掉 `onBlur` 提交**：能让输入框活下来，但"点别处自动落字"这条顺手行为没了，
  空框还会一直挂在画布上。
- **延迟一帧再挂载输入框**（`setTimeout(0)` / `requestIdleCallback`）：靠时序去
  猜浏览器默认动作的先后，换个 WebView2 版本就可能回退，属于用运气换修复。
- **输入框常驻、靠 CSS 隐藏/显示**：焦点同样会被 mousedown 夺走，只是被夺的时候
  看不见；下一次点画布仍要重新抢焦点。
- **改用 `contentEditable` 叠在画布上**：行为更贴近"直接在图上打字"，但要重写
  坐标、光标、提交三套逻辑，与本次"让工具能用"不成比例。

## Consequences

- 文本工具下画布 mousedown 不再有默认聚焦副作用；canvas 本来也不可聚焦，没有其他
  行为被牵连。
- **已知未修**：输入时按 ESC 会取消整次截图（前端 window 监听 + Rust 侧
  `GetAsyncKeyState` 兜底监听，后者不关心谁有焦点）。用 ESC 取消输入法候选词的人
  会误伤截图。要修得在 Rust 监听里加入"输入框聚焦中"这条信息（前端 → 控制器的新
  通道），不在本次范围内。
- 文本在提交前只是 DOM 输入框（画布上没有实时预览），提交后才进像素层；与框选 /
  箭头 / 画笔的"拖动即预览"不同。要实时预览得在 redraw 里再画一层草稿文字。
- 本轮修好的是"输入框根本不出现"。随后暴露的"字只有 4.4px、看不见"是另一条
  独立的缺陷（标注尺寸按物理像素定义），见
  [2026-10-28-annotation-size-wysiwyg.md](2026-10-28-annotation-size-wysiwyg.md)。
