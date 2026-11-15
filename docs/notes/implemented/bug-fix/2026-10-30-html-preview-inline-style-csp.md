# Decision Note: 打包版 HTML 预览丢样式——Tauri 的 CSP nonce 注入把父页 'unsafe-inline' 顶掉

Status: implemented — 2026-10-30 定位并修复「LLM 生成的 .html 在预览器里只剩浏览器默认样式（h1 变大、表格无边框、无配色）」，只在**打包版**复现；`apps/desktop-tauri/src-tauri/tauri.conf.json` 增加 `dangerousDisableAssetCspModification: ["style-src", "script-src"]`

关联：[独立查看器白屏——CSP 缺 'wasm-unsafe-eval'](2026-10-19-viewer-csp-wasm-blank-screen.md)（同一类问题：CSP 配置与共享渲染栈的实际需要脱节，症状都不带错误提示）

## Decision

`apps/desktop-tauri/src-tauri/tauri.conf.json` 的 `app.security` 增加一行：

```json
"dangerousDisableAssetCspModification": ["style-src", "script-src"],
```

HTML 预览（`packages/preview/src/viewers/HtmlView.tsx`）的实现本身**一个字都不用改**：它一直是把文件内容原样塞进 `<iframe srcdoc sandbox="allow-scripts">`，全仓没有任何清洗、转义或剥 `<style>` 的代码。样式是**被浏览器按 CSP 拦掉的**。

### 根因链条（每一环都有实证）

1. `apps/desktop-tauri/index.html` 里有启动遮罩（splash）的**内联 `<style>`**（第 64–220 行）和两段**非空内联 `<script>`**（第 11–63 行）。
2. **编译期**：`tauri-codegen` 解析所有嵌入的 `.html`，给内联 `<style>` 注入 nonce 占位符、把内联 `<script>` 的 sha256 记进 CSP hash 表（`tauri-codegen-2.7.4/src/context.rs:47-88`）。触发条件是"存在内联内容"，与用户是否想要 nonce 无关。
3. **运行期**：`tauri` 把占位符换成随机 nonce，并把 `'nonce-…'` 追加进 `style-src`、把 `'sha256-…'` 追加进 `script-src`（`tauri-2.12.1/src/manager/mod.rs:53-153`）。它**只追加、不删**配置里的 `'unsafe-inline'`。
4. **CSP3 语义**：指令里一旦出现 nonce-source 或 hash-source，`'unsafe-inline'` 对行内内容**立即失效**——"两个都写着"不等于"两个都放行"。
5. **`srcdoc` 继承父文档的 policy container**：预览 iframe 里的 `<style>` 是 LLM 写的，不可能带父文档那个运行时随机 nonce（nonce 也不暴露给 JS），于是整块样式表被丢弃。

即：**Tauri 出于加固目的做的注入，反向废掉了配置里明写的 `'unsafe-inline'`**，受害面不止预览——父页面自己除 splash 之外的所有行内样式、以及 `HtmlView` 注入的导航桥脚本（`BRIDGE_SCRIPT`）在打包版里同样是哑的（后者是"点预览里的链接不跳转"的真正原因，此前被当成沙箱限制）。

### 实证（不靠推理，两条独立证据）

**证据一：同一份 HTML、两种 CSP 的对照实验**（无头 Edge，`srcdoc` iframe 内 `getComputedStyle(h1).color` / `document.styleSheets.length` 由页内脚本 postMessage 回报）：

| 父页 `style-src` | iframe 内 h1 计算色 | styleSheets.length |
|---|---|---|
| `'self' 'unsafe-inline' 'nonce-abc123'` | `rgb(0, 0, 0)`（默认墨色） | `0` ← 样式表**整块丢弃** |
| `'self' 'unsafe-inline'` | `rgb(255, 0, 0)` | `1` ← 正常 |

**证据二：为什么开发时不复现。** dev 走 `devUrl`（`http://127.0.0.1:1420`），前端资源不经 `tauri-codegen` 嵌入，也就没有 nonce/hash 注入；只有 `frontendDist`（`release/frontend`）的打包版才注入。与 2026-10-19 那次白屏同构：**同一套渲染代码，两份 CSP，dev 掩盖打包版**。

### 为什么禁用修改是"回到配置原文"而不是降低安全性

`tauri.conf.json` 里 `style-src`/`script-src` 本来就写着 `'unsafe-inline'`——作者的本意就是放行行内样式与脚本。Tauri 的注入并没有收紧它（`'unsafe-inline'` 一直留在字符串里），只是让它在 nonce 出现后**静默失效**。禁掉注入后，实际生效的 CSP 与配置文件逐字一致，可预测、可审查；而"字符串里写着放行、运行时却被顶掉"是不可预测的那一类。真正收紧行内资源需要的是删掉 `'unsafe-inline'` 并给自家资源配套 nonce，那是另一件事（见 Consequences）。

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 把 splash 的内联 `<style>` 移到外部 CSS，消除 nonce 注入的触发点 | 只解决 style 一半：index.html 的两段内联 `<script>` 仍会让 `script-src` 拿到 sha256，`'unsafe-inline'` 对脚本同样失效 → 预览页里的交互脚本与导航桥脚本还是哑的。而且主题 bootstrap 脚本必须在 React 之前**同步**执行，外置成经典脚本要进 `public/`、脱离 TS/打包链，维护成本换来的是同一个结果 |
| 给预览 iframe 里的内容补 nonce | nonce 只在父文档 CSP 字符串里，JS 读不到（`document.currentScript.nonce` 仅对当次执行的脚本可见），也无法为外来 HTML 逐条打标记 |
| 用 blob: / data: URL 替代 `srcdoc` | 这三种都属于"本地 scheme 文档"，policy container 一律继承创建者，CSP 一样生效 |
| 预览改走 `asset:` 协议（独立文档、不继承父 CSP） | `assetProtocol.scope` 是 `$HOME/.hermix/**`，而预览对象可以是任意路径的文件；为预览把 scope 放宽到 `**` 是拿安全边界换样式 |
| 拆掉 `HtmlView` 的 `sandbox="allow-scripts"` | 不解决样式（`style-src` 单独生效），还砍掉"预览交互式页面"的能力 |
| 全局删掉 `tauri.conf.json` 里的 `'unsafe-inline'`，全面改 nonce | 需要给 splash、主题 bootstrap、导航桥脚本逐个配套 nonce，而桥脚本是运行时拼进外来 HTML 的（注入点没有 nonce 通道）→ 结构上做不到，属于独立的安全改造议题 |

## Consequences

- **得到**：打包版 HTML 预览恢复 LLM 页面自带的全部样式（内联 `<style>`、`style=""` 属性）；预览页里 `<script>` 恢复执行（`sandbox="allow-scripts"` 的设计意图此前在打包版里是失效的），链接转系统浏览器的桥脚本也随之复活。
- **代价**：放弃 Tauri 对 `style-src` / `script-src` 两条指令的自动 nonce/hash 加固。实际生效的 CSP 与 `tauri.conf.json` 原文一致——安全面**不比配置承诺的更宽**，但也不再享受"字符串写 unsafe-inline、运行期自动收紧"这层保护。若将来要真正收紧，前提是 `HtmlView` 换一条不继承父 CSP 的加载通道（见上表 `asset:` 方案），而不是继续靠 CSP 注入。
- **仍然遗留**：
  - `apps/viewer/src-tauri/tauri.conf.json` 未改：它的 `index.html` 没有内联 `<style>`/`<script>`，不触发注入，**样式本来就正常**；但它的 `script-src` 只有 `'self' 'wasm-unsafe-eval'`（无 `'unsafe-inline'`），所以独立查看器里预览页的脚本仍是哑的。要不要给 viewer 也开行内脚本是独立的安全决策，本次不动。
  - `srcdoc` 继承 CSP 的其余部分不变：`default-src 'self'` 仍挡住 https 外链样式表/脚本，相对路径图片仍不解析——这两条是设计取舍，工具条「浏览器打开」是出口。
  - **门禁缺口**：目前没有任何检查能发现"某个 app 的 index.html 有内联资源、配置却没关 CSP 修改"。复发条件很具体（往 index.html 加内联 `<style>`/`<script>` 的 app 就会被 silently 加 nonce），但检查它需要读 `tauri.conf.json` + 扫 index.html，本次未做。
