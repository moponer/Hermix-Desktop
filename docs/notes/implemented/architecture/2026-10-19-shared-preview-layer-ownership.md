# Decision Note: 共享预览层的样式归属与 Tailwind 边界

Status: implemented — 渲染器样式收进 `@hermix/preview`；共享组件不再依赖 Tailwind 原子类；viewer 与桌面端共用同一份预览排版

关联：[独立查看器白屏——CSP 缺 'wasm-unsafe-eval' 与共享预览包的 WASM 顶层 await](../bug-fix/2026-10-19-viewer-csp-wasm-blank-screen.md)、[独立查看器预览引擎懒加载 + 首调冷启延迟的完整表征](../bug-fix/2026-10-19-viewer-lazy-preview-engine.md)、[右侧栏文档预览的格式保真重构](../feature/2026-10-13-preview-rendering-fidelity.md)

## Decision

**`@hermix/preview` 是唯一的渲染层，它的外观也必须住在包里。** 桌面端 `apps/desktop-tauri/src/styles/code-view.css` 只保留两类规则：代码视图（`.code-*`，桌面独有的 Ln/Col 光标与换行开关）与桌面专属外壳（`.preview-head` / `.preview-fullscreen` / 状态条）。渲染器本体（图片 / PDF / PPTX / 纯文本 / 状态 / 头部动作按钮）的样式全部搬进 `packages/preview/src/styles/preview-doc.css`。

这条规矩是被一轮完整的自查逼出来的：**同一个共享包，被桌面端与 viewer 同时消费，但它的样式有一半住在桌面端的 CSS 文件里。** 于是 viewer 里的这些类型等于"没有样式"——这不是配色偏差，是整块布局塌掉：

| 现象（viewer 右侧预览栏） | 直接原因 | 层级 |
|---|---|---|
| 文件贴在左上角一小块，不是全栏居中 | `.preview-pane` / `.preview-pane-inner` 的布局只由 Tailwind 原子类（`flex flex-col flex-1 overflow-auto`）表达，viewer 没装 Tailwind → 外壳退化成普通块 | 宿主契约缺失 |
| 同上 | `.preview-image-stage` / `.preview-pdf-stage` 等渲染器规则只在桌面端 `code-view.css` | 样式住错了地方 |
| 同上 | `.preview-body` 是 flex 行容器，`.preview-pane` 没有 `flex: 1 1 0%` → 外壳宽度收缩到内容宽度 | 宿主契约缺失 |
| md 章节跑到最上面，目录压住工具栏 | `.preview-md-layout` 的"固定目录列 + 独立滚动正文列"契约只存在于 `MarkdownView.tsx` 的注释里，CSS 侧从未落地 | 宿主契约缺失 |
| md 正文被裁到右边界外、只能横向滚动 | `.preview-md-zoom { width: max-content }` 让正文恒为 68ch，`max-width: 68ch` 这个"行长上限"被当成了"行长定值" | 排版规则写反 |
| 双击文件名后缀变色 | 文件列表没有 `user-select: none`，双击触发浏览器的文本选区 | 缺一条规则 |
| 载入中 / 出错提示贴在左上角 | 载入态与错误态早退在 `.preview-pane` 外壳之外，落在 flex 行容器里按内容宽度排 | 外壳不常驻 |
| 预览区暗底暗字，正文几乎读不出来 | viewer 把 `--color-bg-code` 硬编码成深色（`#0b1220`）而文字 token 跟随亮色主题；语法高亮也按 `isDark` 选了亮色配色 | 主题 token 断裂 |
| HTML 预览永远报"二进制文件，不是可渲染的网页" | viewer 的 Rust 侧 `classify()` 把 `html/htm` 归到 `bytes`，回 `FileContent::Bytes`；前端的 `readText` 见到 bytes 判 `isBinary`，而共享层的 `htmlLoader` 恰恰走 `readText` | 两端契约不一致 |

落地的具体规则：

1. **宿主布局契约进共享层**：`.preview-pane .preview-pane-inner { flex: 1 1 0%; min-height: 0; overflow: auto; }`。用后代选择器（0,2,0）压过 Tailwind 的单类原子（0,1,0），两端结果一致、不依赖 `@import` 顺序。`.preview-pane` 自身的外壳样式留在各 app（桌面端用 Tailwind、viewer 用自己的 `styles.css`），因为 `AppDetailModal` 还把 `preview-pane` 当作用域标记挂在代码编辑器上，给它写布局会误伤。
2. **渲染器样式搬进共享包**：`preview-image-*` / `preview-pdf-*` / `preview-pptx-*` / `preview-plain-text` / `preview-state-*` / `head-action` 从 `code-view.css` 移到 `preview-doc.css`。桌面端原来对 `.preview-image-stage` 有两处重复定义（后者用 `overflow: hidden` 覆盖前者的 `overflow: auto`），搬过去时合并成一条，保留桌面端的**实际**生效值（`overflow: hidden`）。
3. **共享组件不再用 Tailwind 原子类**：`CodeBlock` 的 `not-prose my-2.5 overflow-hidden rounded-md` 等 7 处改为 `.preview-code*` 语义类；`DocCanvasView` 的 `text-sm text-text-secondary` 改为 `.preview-doc-meta`；`PreviewSurface` 的 `animate-spin` 改为 `.preview-spin`（自带关键帧）。审计结果：共享组件用到的 64 个类名从「22 个在共享 CSS 里无规则」降到 0（余下 2 个是 `language-${lang}` 高亮标记的假阳性）。
4. **viewer 补齐共享层用到的全部 token**：共享 CSS 现在引用 42 个 token，viewer 原本缺 20 个 —— `var()` 解析失败会让**整条声明被丢弃**，于是圆角、间距、字号、行高、字重、链接色、目录选中底色同时静默失效。全部补齐，桌面端原本就是 0 缺失。
5. **viewer 的 PreviewHost 外壳常驻**：载入态与错误态不再早退到 `.preview-pane` 之外，改成同一套壳里的内容，状态提示因此在整栏正中。
6. **桌面端 `.preview-pane *` 的强制等宽兜底收窄**：`components-tool-card.css` 里那条 `.preview-pane, .preview-pane * { font-family: <mono> !important; font-style: normal !important; font-weight: 400 !important }` 把整个预览面板压成等宽 400 字重——共享层的 Markdown 正文变成代码字体、标题不再加粗、预览头文件名（`font-weight: 600`）被压回 400。它与 `components-prose.css` 里已经写明的意图直接冲突（"不能扩展到整个 `.preview-pane`"）。现在收窄到 `.code-viewport` 系列；同文件里 `h1..hr` 的 `margin/font-family` 兜底去掉 `!important`，让共享层更高特异性的 `.preview-pane .preview-md-body h1` 规则能赢。
7. **文件列表不可选中**：`.entries`（viewer）与 `.computer-list-row` / `.computer-detail-row`（桌面端）加 `user-select: none`，对齐系统资源管理器行为——双击文件名的选区高亮不再出现。
8. **目录列按容器比例取宽**：`flex: 0 0 clamp(140px, 30%, 200px)`，窄栏里目录不再吃掉三分之一宽度；目录列底色与正文列拉开一档（`color-mix` 5% 混入文字色），不画描边。
9. **viewer's 代码面随主题走**：`--color-bg-code` / `-header` / `-border` 从硬编码深色改为亮色主题用浅色、暗色主题用深色（取值对齐桌面端 `tokens.css` / `dark.css`），预览区整体底色改为 `var(--color-bg-code)`（与桌面端 `.preview-body` 同底色）。
10. **viewer 的 `html/htm` 改判为文本**：`apps/viewer/src-tauri/src/commands.rs` 的 `classify()` 里把 `html | htm` 从 `"bytes"` 移到 `"text"`。共享层的 `htmlLoader` 走 `readText` 取源码塞进沙箱 iframe，Rust 必须回文本；此前回 bytes 就被判成二进制，**HTML 预览在 viewer 里从未成功渲染过**。新增 `classify_matches_shared_preview_readers` 回归测试，把"扩展名 → 读取器"的对应关系钉死。

## Alternatives considered

**在 viewer 里也装 Tailwind，共享组件继续用原子类。** 否决。装 Tailwind 只解决"类名不生效"，解决不了"样式住在桌面端的文件里"这个根因——渲染器样式仍然在 `code-view.css`，viewer 照样没样式。而 viewer 是零安装的轻量文件管理器（`vite.config.ts` 明确注释"最小化，无 Tailwind"），为它引入 Tailwind 4 的构建产物与 preflight 全局盒模型重置，代价与收益不成比例。

**把共享渲染层的 CSS 复制一份到 viewer。** 否决。两份必然漂移——这正是本次问题的形态（同一套类名在两个 app 里样式不同），复制等于把 bug 固化成两份。

**把共享组件的类名全部换成内联 style，彻底摆脱样式依赖。** 否决。内联 style 表达不了 `:hover` / `:focus-visible` / 媒体查询，而且方向是反的——目标是"样式归共享层"，不是"样式归组件内联"。本次保留语义类，定义放在共享层的 CSS 文件里。

**把桌面端 `code-view.css` 整个搬进共享包。** 否决。该文件里 `.code-*`（桌面独有）与 `.preview-head`（桌面外壳）不属于共享渲染层，搬过去等于用共享包的名义固化桌面端结构，后续 desktop 改外壳要动共享包。

**目录列保持固定 200px。** 讨论后改为按比例取宽。右侧预览栏在 viewer 里约 540px、在桌面端更窄，固定 200px 的目录会占掉三分之一宽度，正文只剩十几字一行；`clamp(140px, 30%, 200px)` 在宽栏仍是 200px，在窄栏收到 140px 起。

**给 viewer 单独写一套"预览层"样式。** 否决——那是把同一个 bug 复制两份，正是本次要消灭的形态。

**在 viewer 的 `readText` 里把 bytes 解成文本（前端解 base64）。** 否决。HTML 明明是文本，Rust 侧改判一档即可；让前端把整份文件的 base64 解回字符串，是把"分类错了一档"的问题转成"每次读都多一次解码"。判据放在产出方（Rust 侧 `classify`）更靠近事实来源。

## Consequences

- **得到**：共享渲染层在两个 app 里表现一致；viewer's 右侧预览栏从"贴在左上角"变成占满整栏居中，md 目录变成左侧固定列且正文独立滚动；md 正文恢复正文字体、标题 600 字重（桌面端此前是被 `!important` 压掉的）；viewer 的 HTML 预览从"从未渲染成功"变成可用；预览区在亮色主题下不再是暗底暗字。
- **代价**：
  - 桌面端 `code-view.css` 的渲染器段落整体删除（图片 / PDF / PPTX / 状态 / 纯文本 / 头部按钮），`components-tool-card.css` 少一条 `!important` 兜底。
  - 共享代码块的圆角从 Tailwind `rounded-md`（6px）变成 `var(--radius-md)`（10px）——统一到设计令牌的容器圆角，与桌面端聊天区的代码块（仍是 Tailwind 6px）有 4px 差异。
  - md 缩放语义从"放大后每行字数不变"变成浏览器式（`Ctrl +/-`：字号变大、行内字数变少），因为正文宽度改为跟随列宽而非 `max-content`。
  - viewer 预览区底色由固定深色改为随主题，亮色主题下预览区是浅灰而不是深色——这是修正，但与此前"深色预览区"的主观印象不同。
- **仍然遗留**（不在本次范围）：
  - viewer's `icon-sm` / `icon-md` 尺寸档位（14 / 18px）与桌面端的 Tailwind `@utility`（12 / 14px）不同，共享组件里的图标在两端大小不一致。
  - 桌面端 CSP 的 `connect-src` 仍缺 `http://ipc.localhost`（见 CSP 那篇）。
  - viewer 首调 IPC 挂起约 20s 的冷启仍未定位（见懒加载那篇）。
- **门禁**：上述缺口已收口为 `scripts/check-preview-layer.mjs`（ci-gate 的 `[4d/14]` 硬门禁）——共享组件类名必须在共享 CSS 里有规则、共享 CSS 的 token 两侧齐备，AGENTS.md 的"前端 UI 纪律"一节同步登记。用临时脚本注入一个 Tailwind 原子类验证过门禁会红。
- **仍未收口**：没有任何检查能发现"两个 app 的 `tauri.conf.json` CSP 漏了共享栈需要的指令"（见 CSP 那篇）；也没有检查能发现"viewer 的 Rust 侧扩展名分类与共享层读取器预期不一致"——后者本次靠新增的 `classify_matches_shared_preview_readers` 单元测试钉住单点，但没有跨 app 的交叉校验。
- **该缺口后续已补**：2026-10-20 的 [hermix-viewer 双模式化与全能力重构](../feature/2026-10-19-hermix-viewer-overhaul.md) 开工时实测到 `classify()` 与共享层分派表**分叉 57 个扩展名**（后端认得前端不认，或反之），已补齐并扩展该回归测试。**分叉的根因是两侧各有一份扩展名表**，所以后续新增类型时必须同时改两侧，否则同类分叉会复发。
