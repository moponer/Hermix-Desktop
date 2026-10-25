# Decision Note: 配对后键盘顶起界面复发 —— manifest 键名大小写写错 + hideKeyboard 裸竞速

Status: implemented — manifest 键名修正为 `softinputMode`（adjustResize 真正生效）；配对成功后等键盘收到 0 再 reLaunch

## Decision

用户二次上报同症：配对成功跳到对话页后，整页被顶起、顶栏/底栏被裁出屏幕。
排查发现 2026-10-25 早些时候的修复（47b4fb77）**只生效了一半**，两处根因：

### 1. manifest 键名大小写写错，adjustResize 从未生效

`apps/hm/manifest.json` 写的是 `"softInputMode"`（驼峰 I）。uni-app 的合法键是
**全小写的 `softinputMode`**（`app-plus.softinputMode`，取值
`adjustResize`/`adjustPan`）——HBuilderX 解析 manifest 严格按键名，多打的大写 I
让它被**静默忽略**，Android 一直跑在默认 `adjustPan`（整窗上移，键盘收起后
窗口可能不还原）。修正为 `softinputMode` 后，键盘弹出改压缩窗口，跳页时
不存在「上移状态被新页继承」这个形态。

### 2. hideKeyboard 后立即 reLaunch 是裸竞速

`uni.hideKeyboard()` 只是**发起**收起；紧跟着同步调 `uni.reLaunch()`，跳页时
收起动画还在中途，被顶起的窗口状态照样带进对话页。修复：
`reLaunchAfterKeyboardClose(url)`（`pages/pairing/index.vue`）——发起收起后用
`uni.onKeyboardHeightChange` 等高度真正回到 0 再跳，另留 350ms 超时兜底
（键盘本来就没开时高度回调不会来）；平台没有该 API（H5）则直接跳。
顺带把 `busy` 的复位从 `finally` 挪进 `catch`：成功路径等待跳页期间按钮若
复活，双击就是两次配对提交。

## Alternatives considered

- **只修 manifest 键名** —— 不足。adjustResize 生效后竞速大体无害（窗口是
  resize 不是 pan），但真机行为（全屏/沉浸模式、机型差异）无法在本仓库验证，
  同一症状已两次上报，保留等收起的确定性屏障值得这 ~15 行。
- **固定 `setTimeout(reLaunch, 300)`** —— 否决。纯拍时间：慢机型动画更长时
  一样带病跳页；等高度回调是事件驱动，不赌时序。
- **在对话页 onMounted 里「矫正」窗口偏移** —— 否决。adjustPan 的窗口偏移
  不暴露给 WebView JS，页面侧无从还源；治本在源头不让跳页发生在动画中途。

## Consequences

- 配对成功到对话页出现之间多了 ≤350ms（键盘收起动画时长），一次性流程可接受。
- App 端键盘行为全局从 adjustPan 变为 adjustResize：输入时页面压缩而非上移，
  对话页输入栏天然贴在键盘上方（此前靠 `kbHeight` 补偿，该补偿仍保留给 H5）。
- 若未来再动 manifest 键名，对照 uni-app 官方 manifest 文档逐字符核对——
  错一个大小写就是静默 no-op，这正是本次复发的形态。
