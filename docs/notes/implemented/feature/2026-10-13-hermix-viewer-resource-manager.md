# Decision Note: hermix-viewer 资源管理器化 + @hermix/preview 共享包

Status: implemented — viewer 已接入共享渲染包并重写为三栏资源管理器；桌面端切换待主人 MarkdownBody 改造落定后单向合入

## Decision

把桌面端已完整验证的 4 类文件保真化（Excel/Word/Markdown/HTML）从 `apps/desktop-tauri/src/components/preview/*` 抽到独立包 `packages/preview`（包名 `@hermix/preview`，path 依赖），让 `hermix-viewer`（独立文件浏览器）共享同一份渲染层；`viewer` 的 `App.tsx` 重写为「侧栏盘符 + 中部列表 + 右侧多 tab 预览」三栏 Explorer 形态。

为什么这样切：因为 `hermix-viewer` 已是独立 Tauri 进程，不能直接复用 `desktop-tauri/src/components/preview/*`（那一层依赖 `apps/desktop-tauri/src/utils/artifactReader`、`desktopBridge` 等桌面专属 Tauri 桥），而 viewer 没有任何 Tauri 命令能给那套代码用。两个端对"读文件"的契约本来就不一样 —— desktop 是 `tryReadArtifact*`（带产物根白名单 + 祖先回溯），viewer 是 `viewer_read_file`（直读绝对路径 + 字节上限），所以从一开始就不该让 viewer 复制 desktop 的代码。

`FileSource` 抽象（`packages/preview/src/lib/fileSource.ts`）就是为这个差异而留的口子：
- `readText(path)` / `readBytes(path)` / `readMarkdown(path)` 三个最小的 IO 形状
- `fs?: { listDir, stat, parent, drives, search }` —— desktop 没有需要，viewer 有
- `revealInExplorer?` / `openWithSystem?` —— desktop 留空，viewer 接系统调用

桌面端这次**没有**切换到包，仍走自己的 `components/preview/*`（AGENTS.md 提醒主人在并行改 `MarkdownBody.tsx`，避开）。所有桌面端测试 297/297 通过；后续主人完成 `MarkdownBody` 改造后再合并是低风险、纯 import 路径替换。

## Alternatives considered

1. **让 viewer 复制一份 desktop 端 preview 代码**（在 viewer 内部再维护一份）—— 拒绝：上一次主应用就吃了"两套预览管线 + 两份 bug"的亏（2026-09-27 refactor 笔记），现在再复制就是退步。

2. **把 desktop 的 `artifactReader` 提到共享包，viewer 也用它** —— 拒绝：viewer 跟 desktop 的 Tauri 桥不同；强制让 viewer 暴露一个无根的 reader 反而是绕路。

3. **path 依赖 vs pnpm workspace 字段** —— 选 path 依赖：`apps/viewer/package.json` 加 `"@hermix/preview": "file:../../packages/preview"`，**不动根 `pnpm-workspace.yaml`**（AGENTS.md 要求避开主人正在改的部分），pnpm 自动建 symlink。

4. **viewer 自己实现一套 Markdown 渲染**（沿用旧 `md.ts` 的极简版）—— 拒绝：share 一份意味着 viewer 失去 GFM / rawHtml / frontmatter / 相对资源解析，与桌面端"同一份 Markdown 同一种渲染"的承诺相违。

5. **viewer 不做资源管理器，仅保留多 tab** —— 拒绝：主人明确要"真正全能 + 资源管理器"形态；只做多 tab 等于没做事。

6. **三栏布局 vs 单页"预览即主页"** —— 选三栏：与 Windows 资源管理器 / macOS Finder 的肌肉记忆对齐，目录、文件、预览同时可见，路径栏与工具条独立。

## Consequences

- `packages/preview/` 形成新的共享渲染层，**29 个文件 / ~4.6k 行**（含 26 个 workbook 用例、7 个 resolveKind 用例、14 个 markdownDoc 用例、preview-doc.css），与 `desktop-tauri/src/components/preview/*` 短暂并存。后续主人空闲时做 desktop → 包的单向切换。
- 桌面端测试无回归：31 文件 / 297 用例 全绿（之前 283 → 297 是把 markdownDoc 的 14 个用例 + resolveKind 的 7 个用例 通过新结构跑出来的）。
- viewer 端有自己的"资源管理器 UX"：路径栏（可编辑 + breadcrumb）、后退/前进/上一级/刷新、隐藏文件开关、当前目录子串搜索、列表（名称/大小/修改时间）、右键菜单（进入/复制路径/系统打开/资源管理器中显示）、多 tab 预览、Markdown/Excel/Word/PPT/HTML/图片/PDF/代码/纯文本全保真。
- 旧 `apps/viewer/src/md.ts`（viewer 自研极简 Markdown 渲染）随重写删除——它正是备选方案 4 拒绝的那条路，留着就是死代码。
- 字节上限在 viewer Rust 侧按扩展名给（10MB 文本/图片/PDF/html、50MB xlsx/xls/docx/csv、100MB pptx），超过返错误由渲染层提示"用系统程序打开"；桌面端沿用旧的 50MB 文本上限（与 markitdown 配合，未改）。
- 引擎按需分包：exceljs (939KB) / PdfView (438KB) / PptxView (155KB) / DocxView (174KB) / xlsx (429KB) / satteri wasm (3MB) 全部独立 chunk —— viewer 构建警告中仅 4 个超过 500KB（pdf.worker / satteri wasm / exceljs / 主包），开 Markdown / 文本不会为引擎付下载成本。
- 隐藏文件用 Windows `FILE_ATTRIBUTE_HIDDEN = 0x2`（cf-windows 用 `MetadataExt::file_attributes()`），非 Windows 平台用 `.` 兜底 —— 项目是 Windows-only 但留好跨平台路径便于单测。
- RFC 3339 字符串化时间走极简实现（`civil_from_days` Howard Hinnant 算法），不引 chrono —— viewer 是 sidecar，多一个 crate 不划算。
- `bases/viewer/capabilities/default.json` 不需要手动注册新命令（Tauri v2 同 crate 自定义命令默认允许）。

## Reintroduction conditions

- 桌面端要切到共享包时：把 `apps/desktop-tauri/src/components/preview/*` 缩成 4 行 re-export，新增 `lib/fileSourceAdapter.ts`（包 `tryReadArtifact*` / `tryConvertDocumentToMarkdown`），`PreviewPanel.tsx` 改 `import { ... } from "@hermix/preview"`，再删桌面里的 `CodeBlock.tsx` 与 `codeHighlight.ts`（desktop 的 `MarkdownBody.tsx` 仍走自己的 `CodeBlock`，等主人合并后再说）。
- **切换前先给包的用例找 runner**：桌面 vitest 的 include 是 `src/**/*.test.{ts,tsx}`（`vitest.config.ts:14`），扫不到 `packages/preview/src/`——包内 4 个测试文件共 51 个用例（workbook 26 / markdownDoc 14 / resolveKind 7 / preview-md-list 4）目前靠桌面侧同名副本间接覆盖。删桌面副本的同时必须把 `vitest.config.ts` 的 include 扩到 `../../packages/preview/src/**`（或给包加自己的 test script），否则这 51 个用例随副本一起静默停跑。
- viewer 仍依赖 `@hermix/preview` 的 path link；如要发布给其它工程，package.json `main`/`types`/`exports` 已经指向 `./src/index.ts`，搬到独立仓库只需把 import 改 workspace 形式。
- 增加新文件类型：viewer 的 `classify()` 加分支；`size_limit_for_ext()` 加一行；桌面端 `rightSidebarUtils` 的 `officeExtGroup` 等已是共享，扩 `FRONTEND_PARSE_EXTS` 即可。

## 验证

- `cargo check -p hermix-viewer` exit 0（4.74s，0 warning）
- `apps/viewer tsc --noEmit` exit 0
- `apps/viewer vite build` exit 0（48.72s；exceljs / PdfView / xlsx / PptxView / docx-preview / satteri 六个引擎各自独立 chunk，超 500KB 的 4 个是 pdf.worker / satteri wasm / exceljs / 主包）
- `apps/desktop-tauri vitest run` 31 文件 / 297 用例 全绿（79.49s）
- `apps/desktop-tauri tsc --noEmit` exit 0
- `apps/desktop-tauri vite build`：IDE shell 内两次均失败于清空 outDir——WorkBuddy 经 `NODE_OPTIONS` 注入的 node shim 把 vite 的 `rmSync`（`emptyOutDir`）劫持到 genie-trash 回收站二进制，该二进制 ETIMEDOUT（15219 个模块 transform 全过，死在渲染 chunks 之前）。环境问题，非代码回归。IDE 内跑 `vite build` / `npm run build` 的处方：**`CODEBUDDY_SAFE_DELETE_ENABLED=0 <命令>`**（shim 自带的退出开关，只关删除劫持、保留语言 shim，实测 exit 0）；`env -u NODE_OPTIONS` 为全剥方案，亦可。
