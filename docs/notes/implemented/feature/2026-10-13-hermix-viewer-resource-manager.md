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
- 字节上限在 viewer Rust 侧按扩展名给（10MB 文本/图片/PDF、**5MB html**、50MB xlsx/xls/docx/csv、100MB pptx），超过返错误由渲染层提示"用系统程序打开"；桌面端沿用旧的 50MB 文本上限（与 markitdown 配合，未改）。html 单独压到 5MB 是因为它要整页进沙箱 iframe，体积大的页面在预览里没意义。
- 引擎按需分包：exceljs (940KB) / PreviewSurface (921KB) / PdfView (438KB) / xlsx (429KB) / docx-preview (174KB) / PptxView (155KB) / satteri wasm (3MB) 全部独立 chunk；重量模式的编辑面板也各自独立（ImageEditor 20KB / PreviewHost 12KB / PdfPageEditor 8KB / TextEditor 4KB / imageOps 4KB），开 Markdown / 文本不会为引擎与编辑器付下载成本。
- 隐藏文件用 Windows `FILE_ATTRIBUTE_HIDDEN = 0x2`（cf-windows 用 `MetadataExt::file_attributes()`），非 Windows 平台用 `.` 兜底 —— 项目是 Windows-only 但留好跨平台路径便于单测。
- RFC 3339 字符串化时间走极简实现（`civil_from_days` Howard Hinnant 算法），不引 chrono —— viewer 是 sidecar，多一个 crate 不划算。
- `bases/viewer/capabilities/default.json` 不需要手动注册新命令（Tauri v2 同 crate 自定义命令默认允许）。

## 桌面端合入记录（同日完成）

- **Import 切换**：`PreviewPanel.tsx` 改从 `@hermix/preview` 取 `loadPreview` / `PreviewSurface` / `resolveKind` / `supportsTextView` 等；包的 `LoadRequest` 要求显式 `kind`（旧桌面 `LOADERS[kind]` 表随之废弃）。`resolveKind` 的第二参从 `ArtifactKind` 换成 `{ legacy }` hint，desktop 侧映射 `kind === "office" → { legacy: true }`。
- **依赖倒置注入口**：桌面预览与包的差异点不靠复制解决，包 `PreviewSurface` 增加 `renderCode`（desktop 注入自己的 `CodeView`，保留 Ln/Col 光标与换行开关）与 `onOpenImage`（注入灯箱 `openImageViewer`）两个可选 prop；viewer 不传即走默认 CodeBlock / 无灯箱。原 `PreviewSurfaceWithCursor` 兼容壳无消费方，删除。
- **softBreaks 入包**：桌面 MarkdownView 独有的 `hardenSoftLineBreaks`（软换行强化）包侧此前没有——迁入 `packages/preview/src/lib/softBreaks.ts` 并在包 MarkdownView 应用，viewer 因此同步受益。同日复查时发现桌面副本（48 行，与包逐字一致）仍被 `MarkdownBody.tsx` 直接 import，已把该 import 改为 `@hermix/preview`，桌面副本与 `softBreaks.test.ts` 随之删除；用例移入包（`packages/preview/src/lib/softBreaks.test.ts`），覆盖不丢。`codeHighlight.ts` 因有两个包外消费方而保留，见下条。
- **rightSidebarUtils 收敛为 re-export**：扩展名分流那批纯函数与包 `previewUtils` 逐字一致，本地实现删除、改为 `export ... from "@hermix/preview"`；本文件只剩右栏 UI 关注的事（标签、格式化、面包屑）。
- **按笔记原文执行需修正两处**（原文与现实不符，已按实际执行）：① `components/CodeBlock.tsx` 不存在——CodeBlock 内嵌在 `MarkdownBody.tsx` 里；② `lib/codeHighlight.ts` 有 `MarkdownBody` + `CodeView` 两个包外消费方，**不能**随合入删除，保留（`lib/softBreaks.ts` 当时同理保留，后已收敛，见上条）。MarkdownBody 内嵌 CodeBlock 与包 CodeBlock 的统一留作后续（条件：MarkdownBody 改造窗口）。
- **vitest 接管包用例**：desktop `vitest.config.ts` include 扩到 `node_modules/@hermix/preview/src/**` 并配 `resolve.preserveSymlinks`——包测试**必须经符号链接路径**加载，若按真实路径（`packages/preview/src/…`）收集，包内 `import "xlsx"` 等裸导入从文件位置向上找不到 node_modules（pnpm 邻位依赖在链接目标侧）。exclude 需显式收窄（默认值 `**/node_modules/**` 会把包用例一起排掉）。31 文件 / 297 用例与合入前同数，51 个包用例从桌面副本无缝转为包本体。
- **`file:` 依赖 = 每个 app 一份物理快照，不是工作树的符号链接**（复查发现，当日已根治）：仓库根既无 `pnpm-workspace.yaml` 也无 `package.json`，两个 app 各自是 pnpm 根（各有 lock、各有私有 `.pnpm` store）。`file:../../packages/preview` 会把包**复制**进各自的 store 再软链过去——`node_modules/@hermix/preview` 是符号链接没错，但它指向 `.pnpm/@hermix+preview@file+…/` 里的一份**独立副本**。后果：**改 `packages/preview` 后必须两个 app 各跑一次 `pnpm install`**，漏一个就分叉（实测：desktop 已重装而 viewer 未重装，viewer 少 softBreaks、渲染层落后一整个提交）。包内**新增文件**在任一 app 都不会出现，硬链接也只在原地改写时保持。这与 AGENTS.md 记录的"两次『修了但新装的包里没有』"同源。
  - 为什么当时不用 `link:`：它确实给真符号链接，但 pnpm **不会**为 link 目标安装其依赖，包内 `import "satteri"` 这类裸导入会从真实路径向上找不到 node_modules 而构建失败；要修就得给 `packages/preview` 自己的 `node_modules`（即 workspace 成员待遇）或让每个 app 显式声明全部渲染依赖。
  - 根治落地见下一节。

## 根 pnpm workspace（同日根治）

- **Decision**：建根 `pnpm-workspace.yaml`，成员只列**显式三项**——`apps/desktop-tauri`、`apps/viewer`、`packages/*`；两个 app 的 `@hermix/preview` 从 `file:` 改 `workspace:*`；删除两个 app 级的 `pnpm-workspace.yaml` 与 `pnpm-lock.yaml`，根下生成唯一 lock。效果：`apps/*/node_modules/@hermix/preview` 从此是**指向 `packages/preview` 工作树的真符号链接**，改包免安装即时生效；`packages/preview/node_modules` 存在，包内裸导入从真实路径可解析。
- **为什么不写 `apps/*`**：`apps/` 下实际有 10 个项目（android / bbs / desktop-tauri / hm / hx-packager / hx-shell / installer / playground / relay / viewer），其中 relay 走 npm（`package-lock.json`）、android 根本不是 Node 工程。通配会把它们全拖进 pnpm 依赖图——实测一次就把 relay 的 `better-sqlite3` 连同 1400+ 包拉了进来，那是与本次目的完全无关的大范围迁移。**显式列举，别对 `apps/` 用通配。**
- **合并的根配置**：两份 app 级 `pnpm-workspace.yaml` 的 `allowBuilds.esbuild` 与 desktop 的 `overrides["@types/hast"]`（satteri 依赖链的类型版本必须钉死）。另加 `@parcel/watcher`——它是 sass 的 optionalDependency，pnpm 默认不跑依赖构建脚本，遇到被拦的会以 `ERR_PNPM_IGNORED_BUILDS` **直接失败**（不是警告）。
- **`apps/desktop-tauri/.npmrc` 留在原位**：它被 gitignore（可能含本地凭证），pnpm 在该目录下执行时仍会读到，不迁。
- **顺带暴露并修掉三个此前被掩盖的问题**：
  1. `packages/preview` 此前**没有声明 `@types/react` / `@types/react-dom`**。此前包是 store 里的副本，类型恰好从 app 侧兜住了；变成真链接 + 自己的 `node_modules` 后立刻暴露为满屏 TS7016。已补进包的 devDependencies。
  2. 包把 `react` / `react-dom` 同时写进 `dependencies` 与 `peerDependencies`。已从 `dependencies` 删除只留 peer——否则 workspace 里会出现第二份 React，hooks 直接炸。pnpm 的 `auto-install-peers` 把 peer 解到与两个 app **同一个** `react@19.3.0` store 条目（已实测三处符号链接指向同一真实路径），因此无重复实例。
  3. workspace 统一 TypeScript 到 5.9.3（原先两个 app 各钉 `^5.8.3`），TS 5.7+ 的 DOM 库把 `BlobPart` 收紧为 `ArrayBufferView<ArrayBuffer>`，把 `PreviewPanel` 灯箱那处 `new Blob([bytes])` 顶了出来。已按包内 MarkdownView 同法复制到 ArrayBuffer 支撑的 Uint8Array。**这属"版本统一后暴露的既有欠账"，不是迁移引入的回归。**
- **Alternatives rejected**：`link:` 协议（不给 link 目标装依赖，裸导入解析失败）；保留 app 级 lock 只加根 workspace（workspace 的定义就是"单 lock 多成员"，两套并存没有意义）。
- **副作用**：安装耗时显著变长（首次 1400+ 包），但之后改包不再需要 `pnpm install`。
- **迁移后验证（全绿）**：`apps/viewer tsc --noEmit` exit 0；`apps/desktop-tauri tsc --noEmit` exit 0；`apps/desktop-tauri vitest run` 31 文件 / 297 用例全绿；`apps/viewer vite build` exit 0（1m15s，六引擎仍各自独立 chunk：exceljs 939.91KB / PdfView 437.89KB / xlsx 429.48KB / docx-preview 173.89KB / PptxView 154.56KB / workbook 9.56KB）；`apps/desktop-tauri vite build` exit 0（7m56s，`CODEBUDDY_SAFE_DELETE_ENABLED=0`；主包 2067KB 的既有大包警告未变）；`cargo check -p hermix-viewer` exit 0（9m09s，0 warning）。
- **决定性验证**：在 `packages/preview` 写入一行探针注释，两个 app 的 `node_modules/@hermix/preview/src/lib/localAssets.ts` **无需 `pnpm install`** 即同时可见该行，撤销后同样同步——这正是旧 `file:` 布局做不到的。

## Reintroduction conditions

- 恢复桌面本地副本的条件：共享包 path 依赖被证明不可行（如要发布独立仓库而 symlink 工具链不可用）。届时从 git 历史恢复 `components/preview/*`，并同步恢复 vitest include。
- **统一 MarkdownBody 内嵌 CodeBlock 与包 CodeBlock**：待 MarkdownBody 改造窗口。届时包需导出 `codeHighlight`（或 MarkdownBody 改用包 CodeBlock 的形状），然后删桌面 `lib/codeHighlight.ts`（350 行，与包逐字一致，`MarkdownBody` + `CodeView` 两个消费方）。`lib/softBreaks.ts` 已收敛，不再属此项。

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
- `apps/viewer vite build` exit 0（38.18s，包侧改动后回归）← **这条当时不成立**：viewer 的包副本当时停在改动前（见上文 `file:` 快照陷阱），它验的不是同一份代码
- `npx -y impeccable@4.1.0 detect`（PreviewPanel / 包 PreviewSurface / 包 MarkdownView / fileSourceAdapter）exit 0

## 复查与修复（同日，只读审查后）

对三个提交做了一轮只读审查（不编译），修掉下列问题：

1. **viewer 包副本落后一个提交**（阻塞级）—— 见 Consequences 的快照陷阱。两个 app 重跑 `pnpm install`，现在两端副本与工作树逐字一致（`loaders.ts` 20:48:38/9651 一致，`softBreaks.test.ts` 两端都在）。
2. **AI 会话串台**：`App.tsx` 的 `<AiSidebar>` 没有 `key`，`sessionIdRef` 挂在面板生命周期上。不关 AI 栏换文件，下一句会接着上一个文件的会话问，只是把新文件路径拼进 prompt。加 `key={active?.path ?? "__none__"}` 重建面板即重置会话。
3. **`text` kind 恒返回 `code` 载荷**：`loaders.ts` 里 `text` 与 `code` 共用一个 loader，而它只发 `code`，于是 `types.ts` 的 `{kind:"text"}` 变体与 `PreviewSurface` 的 `case "text"` 都是死代码，`.log` 这类未知扩展名文件在桌面端还会误显 Ln/Col。改为按 `req.kind` 分发（`emit` 显式标注 `PreviewPayload`，否则 TS 只能看到宽泛的 `PreviewKind` 而赋不进 union）。
4. **html 字节上限文档失准**：笔记与 `commands.rs` 两处注释都写 10MB，代码是 5MB。三处统一为 5MB 并补上"沙箱整页渲染所以压得比文本低"的理由。
5. **`softBreaks` 桌面重复副本**（48 行，与包逐字一致）：`MarkdownBody.tsx` 改从 `@hermix/preview` import，桌面副本与其测试删除，用例移入包。`codeHighlight.ts`（350 行）因有两个包外消费方保留，仍是已知未收项。
6. **viewer `fileSource` 两处小疵**：`readText` 为拿 size 把整段 base64 全解一遍（50MB PDF 白做一次全量解码）→ 改为按字符数算字节数；`readMarkdown` 用 `this.readText`，解构方法后会静默炸 → 提为模块级 `readFile`，三条 read 共用同一个 invoke 入口。

修复后验证：

- `apps/viewer tsc --noEmit` exit 0（这条同时覆盖包的类型，因为 viewer 经 node_modules 引包）
- `apps/desktop-tauri vitest run` 31 文件 / 297 用例 全绿（33.56s；包内 `resolveKind` 7 + `preview-md-list` 4 + `softBreaks` 5 + workbook 26 均从 `node_modules/@hermix/preview/src/` 路径跑到）
- `cargo check -p hermix-viewer` / 两个 `vite build` / `desktop tsc` **本轮未跑**（按要求不编译；Rust 侧改动只有注释，无行为变化）

## 后续变更（2026-10-20）

本篇确立的三栏资源管理器形态与共享包决策**未被推翻**，[hermix-viewer 双模式化与全能力重构](../feature/2026-10-19-hermix-viewer-overhaul.md) 在其上叠加了模式分层与编辑转换能力。以下是**事实层面的变化**，细节与理由在那篇：

- **三栏布局不再钉死宽度**。原 `.explorer { max-width: 460px }` 导致全屏下大片留白，现改为按比例铺满 + 可拖拽分隔条 + 全屏/最大化自适应。
- **viewer 分成两种模式**：纯净模式（被文件关联唤起，只渲染预览面，**不产生侧栏 DOM**）与重量模式（资源管理器 + AI + 编辑工具集）。本篇描述的「资源管理器 UX」属重量模式。
- **类型图标收进共享包**：`apps/desktop-tauri/src/components/FileTypeIcon.tsx` 迁到 `packages/preview/src/FileTypeIcon.tsx`，走独立子路径出口（不进 barrel）以保住懒加载分块，桌面端与 viewer 共用一份彩色版本。
- **编辑器不进共享包**：`apps/viewer/src/image/`、`apps/viewer/src/document/` 的编辑面板留在 viewer 侧——共享包同时被 desktop 消费，把编辑器塞进去会让 desktop 白背一份 CSS 与 token 契约。
- **Rust 侧 `classify()` 与共享层分派表对齐**：开工时两者分叉 57 个扩展名，已补齐（见 [共享预览层的样式归属与 Tailwind 边界](../architecture/2026-10-19-shared-preview-layer-ownership.md) 里的同款记录）。**两侧各有一份扩展名表是分叉的根因，新增类型时必须同改两侧。**
