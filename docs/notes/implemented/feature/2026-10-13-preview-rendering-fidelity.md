# Decision Note: 右侧栏文档预览的格式保真重构

Status: implemented — Markdown / Excel / Word / HTML 四类预览已按源文件版面重做

## Decision

把「右侧栏预览」从**按扩展名凑合渲染**改成**按源文件版面还原**，判定与加载管线
（`resolveKind` → `LOADERS` → `PreviewSurface`）保持不变，渲染层与样式层重做。

四类问题的根因与处置：

| 类型 | 根因 | 处置 |
|---|---|---|
| Excel | SheetJS 社区版读 xlsx 时只能拿回 fill，font/border/alignment 全丢；且第一行被当表头吃掉（每张表少一行数据） | `.xlsx/.xlsm` 改走 ExcelJS，保住字体/填充/边框/对齐/数字格式/列宽/行高/合并/冻结窗格/隐藏行列；第一行回归数据 |
| Word | `docx-preview` 的四个选项被逐项关掉（`ignoreWidth` / `ignoreHeight` / `renderHeaders` / `renderFooters`） | 只声明与默认不同的项，交给库按 sectPr 出纸张；页眉页脚恢复渲染；正文不再被容器 CSS 覆盖字号行高 |
| HTML | `resolveKind` 里根本没有 html 分支，`.html` 被 `detectLanguage` 认成 `markup` 直接进代码视图 | 新增 `html` 载荷与 `HtmlView`：沙箱 iframe 渲染页面，工具条可切视口宽度/源码/系统浏览器 |
| Markdown | `rawHtml` 未开导致内嵌 HTML 被吞；frontmatter 混进正文；相对路径图片裂图；行长写死 780px | 开 `rawHtml`、剥 frontmatter、相对资源经产物读取链转 blob URL、行长按 `ch` 约束 |

同时新增：

- `lib/workbook.ts` —— 表格保真模型（样式翻译 + 主题色/tint + 单位换算 + 行高前缀和）。
- `styles/preview-doc.css` —— 文档排版层，从 `code-view.css` 拆出。
- 缩放控件上收到 `PreviewPanel` 头部，四类流式排版内容共用一处。

## Alternatives considered

**换成 `xlsx-js-style`。** 与 `xlsx@0.18.5` 同一份 SheetJS 内核，只补了写样式。
实测读同一份样本，`cellStyles: true` 拿到的仍然只有 fill —— 加它等于什么都没解决，
却多背一个近 1 MB 的依赖。否决。

**手写 OOXML 解析（JSZip + 自写 styles.xml 解析）。** 能做到零依赖，但等于把
Excel 格式规范重新实现一遍：numFmt 的条件段、tint 的 HSL 插值、边框样式表、主题色索引
重排，四处都是容易做错且难测的规则。属于典型重复造轮子，否决。

**自建数字格式化。** 已有 SheetJS 的 `SSF`（Excel 数字格式的完整实现）而且它本来就是
依赖（`.xls` 走它），两套格式化实现必然在日期口径上打架。改用 `SSF`，且只在工作簿里
真的出现非 General 格式串时才动态加载。采纳。

**HTML 用 `mammoth`/文本化。** mammoth 只处理 docx 且明确丢失版式，对 html 更是对不上。
否决。

**HTML 沙箱开 `allow-same-origin`。** 能让相对路径资源正常解析，但同时给了页面脚本
访问应用源与存储的能力。改为 `allow-scripts` + 不透明源 + 链接走桥脚本交系统浏览器；
相对路径的完整渲染需求由「用默认浏览器打开」兜底。

**放宽全局 CSP 以加载外部 CSS/JS。** 这是全应用的安全边界，超出本次任务范围；沙箱
iframe 已经隔断了脚本执行面，没有理由为一个预览页动全局策略。否决，改为如实说明边界。

**把 HTML 的相对资源也读进来内联。** 需要对 HTML 文本做属性级重写，收益只覆盖"相对
图片"一种情况，而 `<link>` / `@import` / `url()` 仍会漏。与其写半个不完整的重写器，
不如给一个点开就是完整环境的出口。

## Consequences

- **多了一个依赖**：`exceljs@4.4.0`（browser 预打包产物约 925 KB）。按需 `import()`，
  只有打开表格时才下载；换来的能力是唯一能把 OOXML 样式读回来的开源实现。
- **保持按需加载的边界**：`workbook.ts`（拉 ExcelJS）是动态 import，唯一被静态
  import 的是 `colName` 这个纯字符串工具——它住在 `lib/localAssets.ts`，与
  引擎无关。Vite 第一次构建曾误把 ExcelJS 拽进主包（939KB 砸到首屏），
  把 `colName` 拆出去之后 ExcelJS 仍是独立 chunk。
- **SheetJS 保留但降级**：只服务 `.xls`（ExcelJS 不支持 BIFF）与 `SSF` 格式化。
  BIFF 的样式表读不回来，所以 `.xls` / `.csv` 的载荷带 `fidelity: "plain"`，
  工具条显示「仅内容」徽标 —— 不假装已还原。
- **表格解析上限**：单表 20000 行 × 512 列，超出部分不渲染。行做窗口化（行高前缀和 +
  二分），列全量渲染 —— 真正会炸的是行数。
- **HTML 预览继承宿主 CSP**：走 https 的外部样式表/脚本不加载，相对路径图片不解析。
  这是沙箱的代价，已写进 `HtmlView` 的文件头注释与工具条提示。
- **`preview-doc.css` 是新的排版权威源**：Markdown 排版、目录、Word 纸张、表格网格、
  网页舞台的规则都在这里；`code-view.css` 只留代码视图与桌面专属外壳
  （`.preview-head` / `.preview-fullscreen` / 状态条）。图片、PDF、PPTX 画布原本
  还留在 `code-view.css`，2026-10-19 已一并搬进 `preview-doc.css`——共享包
  `@hermix/preview` 被桌面端与 viewer 同时消费，渲染器样式留在桌面端会让
  viewer 的这些类型完全没有样式。改预览排版请改共享包那个文件，不要在
  `code-view.css` 里加第二条同选择器规则。详见
  [共享预览层的样式归属与 Tailwind 边界](../architecture/2026-10-19-shared-preview-layer-ownership.md)。
- **缩放只在 `markdown` / `docx` / `spreadsheet` / `html` 上出现**：PDF 与 PPTX 是
  canvas，缩放由各自引擎的像素倍率管（见 `PdfView` / `PptxView`），再叠一层会双重缩放。
