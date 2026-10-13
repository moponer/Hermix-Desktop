# Decision Note: hermix-viewer 资源管理器化 + @hermix/preview 共享包

Status: implemented — viewer 已接入共享渲染包并重写为三栏资源管理器；桌面端同日完成单向合入，`components/preview/*` 本地副本已删除，两端共用 `packages/preview` 一份实现

## Decision

把桌面端已完整验证的 4 类文件保真化（Excel/Word/Markdown/HTML）从 `apps/desktop-tauri/src/components/preview/*` 抽到独立包 `packages/preview`（包名 `@hermix/preview`，path 依赖），让 `hermix-viewer`（独立文件浏览器）共享同一份渲染层；`viewer` 的 `App.tsx` 重写为「侧栏盘符 + 中部列表 + 右侧多 tab 预览」三栏 Explorer 形态。

为什么这样切：因为 `hermix-viewer` 已是独立 Tauri 进程，不能直接复用 `desktop-tauri/src/components/preview/*`（那一层依赖 `apps/desktop-tauri/src/utils/artifactReader`、`desktopBridge` 等桌面专属 Tauri 桥），而 viewer 没有任何 Tauri 命令能给那套代码用。两个端对"读文件"的契约本来就不一样 —— desktop 是 `tryReadArtifact*`（带产物根白名单 + 祖先回溯），viewer 是 `viewer_read_file`（直读绝对路径 + 字节上限），所以从一开始就不该让 viewer 复制 desktop 的代码。

`FileSource` 抽象（`packages/preview/src/lib/fileSource.ts`）就是为这个差异而留的口子：
- `readText(path)` / `readBytes(path)` / `readMarkdown(path)` 三个最小的 IO 形状
- `fs?: { listDir, stat, parent, drives, search }` —— desktop 没有需要，viewer 有
- `revealInExplorer?` / `openWithSystem?` —— desktop 留空，viewer 接系统调用

桌面端起初**没有**切换到包（抽包时主人正在并行改 `MarkdownBody.tsx`，避开同批文件）；MarkdownBody 改造落定后已于同日完成单向合入：`PreviewPanel` 改 import 共享包，`components/preview/*` 与 `lib/workbook` / `markdownDoc` / `localAssets` / `preview-doc.css` 副本全部删除。合入细节见下方「桌面端合入记录」。

## Alternatives considered

1. **让 viewer 复制一份 desktop 端 preview 代码**（在 viewer 内部再维护一份）—— 拒绝：上一次主应用就吃了"两套预览管线 + 两份 bug"的亏（2026-09-27 refactor 笔记），现在再复制就是退步。

2. **把 desktop 的 `artifactReader` 提到共享包，viewer 也用它** —— 拒绝：viewer 跟 desktop 的 Tauri 桥不同；强制让 viewer 暴露一个无根的 reader 反而是绕路。

3. **path 依赖 vs pnpm workspace 字段** —— 选 path 依赖：`apps/viewer/package.json` 加 `"@hermix/preview": "file:../../packages/preview"`，**不动根 `pnpm-workspace.yaml`**（AGENTS.md 要求避开主人正在改的部分），pnpm 自动建 symlink。

4. **viewer 自己实现一套 Markdown 渲染**（沿用旧 `md.ts` 的极简版）—— 拒绝：share 一份意味着 viewer 失去 GFM / rawHtml / frontmatter / 相对资源解析，与桌面端"同一份 Markdown 同一种渲染"的承诺相违。

5. **viewer 不做资源管理器，仅保留多 tab** —— 拒绝：主人明确要"真正全能 + 资源管理器"形态；只做多 tab 等于没做事。

6. **三栏布局 vs 单页"预览即主页"** —— 选三栏：与 Windows 资源管理器 / macOS Finder 的肌肉记忆对齐，目录、文件、预览同时可见，路径栏与工具条独立。

## Consequences

- `packages/preview/` 形成共享渲染层，desktop 与 viewer 都从它取；桌面端已无本地副本（合入时删除 `components/preview/*`、`lib/workbook|markdownDoc|localAssets`、`styles/preview-doc.css` 及 fixture，约 3.5k 行重复归一）。
- 桌面端测试无回归：31 文件 / 297 用例 全绿（之前 283 → 297 是把 markdownDoc 的 14 个用例 + resolveKind 的 7 个用例 通过新结构跑出来的）。
- viewer 端有自己的"资源管理器 UX"：路径栏（可编辑 + breadcrumb）、后退/前进/上一级/刷新、隐藏文件开关、当前目录子串搜索、列表（名称/大小/修改时间）、右键菜单（进入/复制路径/系统打开/资源管理器中显示）、多 tab 预览、Markdown/Excel/Word/PPT/HTML/图片/PDF/代码/纯文本全保真。
- 旧 `apps/viewer/src/md.ts`（viewer 自研极简 Markdown 渲染）随重写删除——它正是备选方案 4 拒绝的那条路，留着就是死代码。
- 字节上限在 viewer Rust 侧按扩展名给（10MB 文本/图片/PDF/html、50MB xlsx/xls/docx/csv、100MB pptx），超过返错误由渲染层提示"用系统程序打开"；桌面端沿用旧的 50MB 文本上限（与 markitdown 配合，未改）。
- 引擎按需分包：exceljs (939KB) / PdfView (438KB) / PptxView (155KB) / DocxView (174KB) / xlsx (429KB) / satteri wasm (3MB) 全部独立 chunk —— viewer 构建警告中仅 4 个超过 500KB（pdf.worker / satteri wasm / exceljs / 主包），开 Markdown / 文本不会为引擎付下载成本。
- 隐藏文件用 Windows `FILE_ATTRIBUTE_HIDDEN = 0x2`（cf-windows 用 `MetadataExt::file_attributes()`），非 Windows 平台用 `.` 兜底 —— 项目是 Windows-only 但留好跨平台路径便于单测。
- RFC 3339 字符串化时间走极简实现（`civil_from_days` Howard Hinnant 算法），不引 chrono —— viewer 是 sidecar，多一个 crate 不划算。
- `bases/viewer/capabilities/default.json` 不需要手动注册新命令（Tauri v2 同 crate 自定义命令默认允许）。

## 桌面端合入记录（同日完成）

- **Import 切换**：`PreviewPanel.tsx` 改从 `@hermix/preview` 取 `loadPreview` / `PreviewSurface` / `resolveKind` / `supportsTextView` 等；包的 `LoadRequest` 要求显式 `kind`（旧桌面 `LOADERS[kind]` 表随之废弃）。`resolveKind` 的第二参从 `ArtifactKind` 换成 `{ legacy }` hint，desktop 侧映射 `kind === "office" → { legacy: true }`。
- **依赖倒置注入口**：桌面预览与包的差异点不靠复制解决，包 `PreviewSurface` 增加 `renderCode`（desktop 注入自己的 `CodeView`，保留 Ln/Col 光标与换行开关）与 `onOpenImage`（注入灯箱 `openImageViewer`）两个可选 prop；viewer 不传即走默认 CodeBlock / 无灯箱。原 `PreviewSurfaceWithCursor` 兼容壳无消费方，删除。
- **softBreaks 入包**：桌面 MarkdownView 独有的 `hardenSoftLineBreaks`（软换行强化）包侧此前没有——迁入 `packages/preview/src/lib/softBreaks.ts` 并在包 MarkdownView 应用，viewer 因此同步受益。
- **rightSidebarUtils 收敛为 re-export**：扩展名分流那批纯函数与包 `previewUtils` 逐字一致，本地实现删除、改为 `export ... from "@hermix/preview"`；本文件只剩右栏 UI 关注的事（标签、格式化、面包屑）。
- **按笔记原文执行需修正两处**（原文与现实不符，已按实际执行）：① `components/CodeBlock.tsx` 不存在——CodeBlock 内嵌在 `MarkdownBody.tsx` 里；② `lib/codeHighlight.ts` 有 `MarkdownBody` + `CodeView` 两个包外消费方，**不能**随合入删除，保留（`lib/softBreaks.ts` 同理保留，MarkdownBody 在用）。MarkdownBody 内嵌 CodeBlock 与包 CodeBlock 的统一留作后续（条件：MarkdownBody 改造窗口）。
- **vitest 接管包用例**：desktop `vitest.config.ts` include 扩到 `node_modules/@hermix/preview/src/**` 并配 `resolve.preserveSymlinks`——包测试**必须经符号链接路径**加载，若按真实路径（`packages/preview/src/…`）收集，包内 `import "xlsx"` 等裸导入从文件位置向上找不到 node_modules（pnpm 邻位依赖在链接目标侧）。exclude 需显式收窄（默认值 `**/node_modules/**` 会把包用例一起排掉）。31 文件 / 297 用例与合入前同数，51 个包用例从桌面副本无缝转为包本体。

## Reintroduction conditions

- 恢复桌面本地副本的条件：共享包 path 依赖被证明不可行（如要发布独立仓库而 symlink 工具链不可用）。届时从 git 历史恢复 `components/preview/*`，并同步恢复 vitest include。
- **统一 MarkdownBody 内嵌 CodeBlock 与包 CodeBlock**：待 MarkdownBody 改造窗口。届时包需导出 `codeHighlight`（或 MarkdownBody 改用包 CodeBlock 的形状），然后删桌面 `lib/codeHighlight.ts`；`lib/softBreaks.ts` 已入包，桌面副本届时可一并删（改为从包 import）。

## 验证

抽包当时（viewer 侧）：

- `cargo check -p hermix-viewer` exit 0（4.74s，0 warning）
- `apps/viewer tsc --noEmit` exit 0
- `apps/viewer vite build` exit 0（48.72s；exceljs / PdfView / xlsx / PptxView / docx-preview / satteri 六个引擎各自独立 chunk，超 500KB 的 4 个是 pdf.worker / satteri wasm / exceljs / 主包）
- `apps/desktop-tauri vitest run` 31 文件 / 297 用例 全绿（79.49s）
- `apps/desktop-tauri tsc --noEmit` exit 0
- `apps/desktop-tauri vite build`：IDE shell 内两次均失败于清空 outDir——WorkBuddy 经 `NODE_OPTIONS` 注入的 node shim 把 vite 的 `rmSync`（`emptyOutDir`）劫持到 genie-trash 回收站二进制，该二进制 ETIMEDOUT（15219 个模块 transform 全过，死在渲染 chunks 之前）。环境问题，非代码回归。IDE 内跑 `vite build` / `npm run build` 的处方：**`CODEBUDDY_SAFE_DELETE_ENABLED=0 <命令>`**（shim 自带的退出开关，只关删除劫持、保留语言 shim，实测 exit 0）；`env -u NODE_OPTIONS` 为全剥方案，亦可。

桌面端单向合入后：

- `apps/desktop-tauri tsc --noEmit` exit 0；`apps/viewer tsc --noEmit` exit 0
- `apps/desktop-tauri vitest run` 31 文件 / 297 用例 全绿（含包内 51 用例）
- `apps/desktop-tauri vite build` exit 0（4m12s，`CODEBUDDY_SAFE_DELETE_ENABLED=0`；超 2600KB chunk 警告为既有已知项）
- `apps/viewer vite build` exit 0（38.18s，包侧改动后回归）
- `npx -y impeccable@4.1.0 detect`（PreviewPanel / 包 PreviewSurface / 包 MarkdownView / fileSourceAdapter）exit 0
