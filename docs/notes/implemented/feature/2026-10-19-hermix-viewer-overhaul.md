# Decision Note: hermix-viewer 双模式化与全能力重构（超长任务总纲）

Status: implemented — F0–F9 全部交付。双模式（纯净/重量）、彩色类型图标、启动卡顿根因修复、全屏平铺、图片处理内核、PDF/文本编辑、AI 一键入对话、常见文件类型覆盖矩阵均已落地并各自提交（`106bd5f2` → `08621066`）；F10 交互与界面返修（列表改单击即预览 + 溢出/对齐/胶囊/描边清理）、F11 图片编辑器马赛克改「涂抹画笔」、F12 顶部右侧选项组改带文字标签、F13 应用改名「Hermix 文件查看器」、F14 顶部区域打磨（对齐/分层/不溢出 + 标签关闭按钮按需显示）见「交互与界面返修」与「F11」「F12」「F13」「F14」章节；F15–F18（可执行文件「立即运行」、反向路径同步 + 驱动器真实卷标 + 表头对齐配色、顶部路径栏万能输入 + 全盘索引搜索、开放能力 MCP 服务）各自独立成篇：见 [可执行文件「立即运行」](../feature/2026-10-20-viewer-executable-run-immediately.md)、[路径栏万能输入 + 全盘索引搜索](../feature/2026-10-20-viewer-pathbar-universal-input-and-global-search.md)、[开放能力 MCP 服务](../feature/2026-10-20-viewer-open-capabilities-mcp.md)

关联：[hermix-viewer 资源管理器化 + @hermix/preview 共享包](../feature/2026-10-13-hermix-viewer-resource-manager.md)、[独立文件浏览器（hermix-viewer）+ 可折叠 AI 右栏 + 文件关联接管](../feature/2026-10-09-standalone-file-viewer.md)、[共享预览层的样式归属与 Tailwind 边界](../architecture/2026-10-19-shared-preview-layer-ownership.md)、[前端内容指纹纳入编译指纹 —— 根治「exe 里是旧 UI」](../bug-fix/2026-10-20-frontend-fingerprint-in-build.md)

## Decision

把独立文件浏览器 `apps/viewer`（`hermix-viewer`）从「三栏只读资源管理器」升级为**双模式全能力查看器**：

- **纯净模式（light）** — 被文件关联唤起（`HKCU` 登记的 `hermix-viewer.exe` 带路径启动）时进入。无侧栏、无资源管理器、无 AI 栏，只有一条细工具条（另存 / 打开方式 / 显示方式 / 切到重量模式 / 关闭）。目标是「双击一个文件就看到它」，进程与首屏都轻。
- **重量模式（heavy）** — 独立启动时进入：驱动器侧栏 + 目录资源管理器 + 多标签预览 + AI 协助 + **文件编辑与转换工具集**（图片处理、文档/PDF/文本编辑、格式转换）。产物一律另存，**原文件永不被覆盖**。

同时落地三件已定位根因的修复：

1. **启动卡顿** — `viewer_drives` 原是同步 `#[tauri::command]`，内部对每个盘符做 `std::fs::metadata`（`apps/viewer/src-tauri/src/commands.rs:429-453`）。同步命令默认在主线程执行，而 `GetFileAttributesEx` 打在断开的网络映射盘上会阻塞到 SMB 超时（可达数十秒）。前端还把首屏路径挂在驱动器列表后面（`apps/viewer/src/App.tsx:207` 与 `:212-220` 两次调用 `drives()`，第二次拿到结果才 `setCurrentPath`）。→ 驱动器列表改为「异步 + 不做文件系统 IO + 不阻塞首屏 + 侧栏加载态 + 1.5s 超时降级」。
2. **全屏不平铺** — `apps/viewer/src/styles.css` 的 `.explorer { flex: 1; min-width: 280px; max-width: 460px; }` 把中栏宽度钉死在 460px，`.sidebar` 200px、`.ai-side` 320px 又是固定值；窗口没有任何全屏/最大化状态处理。宽屏下剩余空间全是空底色。→ 三栏按比例铺满 + 可拖拽分隔条 + 全屏/最大化状态自适应。
3. **彩色类型图标** — 现状是 lucide 单色图标。→ 建「扩展名 → 专属图标 + 专属色」的类型系统，文件夹有独立图标；桌面端 `FileAssocTab` 的关联后缀列表同步显示对应类型图标。

**归属**：`packages/preview`（`@hermix/preview`）继续是唯一渲染层，desktop 与 viewer 共用；类型判定、图标映射、渲染器样式归共享包，桌面端 `code-view.css` 只留 `.code-*` 与桌面外壳（见 `scripts/check-preview-layer.mjs`）。编辑/转换的**计算内核**归 `apps/viewer/src-tauri`（Rust），交互面板归 viewer 前端——**编辑器不进共享包**：共享包同时被 desktop 消费，把编辑器塞进去会让 desktop 白背一份 CSS 与 token 契约。

**执行方式**：本篇既是立项记录，也是**执行台账**（见下「## 执行台账」），台账已随每个子任务同步更新。

## Alternatives considered

1. **驱动器标签继续用 `std::fs::metadata`** —— 拒绝：这是启动卡顿的物理根因。改用 `GetDriveTypeW` 判「本地磁盘 / 网络驱动器 / 可移动 / 光驱」不碰文件系统，标签文本随之降级，换来的是断网映射盘不再冻结启动。
2. **首屏等驱动器就绪再渲染** —— 拒绝：盘符枚举是毫秒级的 IO，代价却是要等网络盘超时。改为首屏用兜底路径（上次访问目录 → 用户目录 → 首个可用盘）直接渲染，驱动器列表后到后替换。
3. **纯前端 Canvas 做全部图片处理** —— 备选但未全采纳：Canvas 交互与预览快、零依赖，但格式编解码覆盖面（尤其 ico 多尺寸打包、tiff、heic）与批处理弱。最终形态是「前端 Canvas 负责交互与即时预览，Rust 侧负责编码/封装与落盘」，Rust 侧只用 `image` 一类纯 Rust 库。
4. **引入 monaco / codemirror 做代码编辑** —— 拒绝：体量与编译/打包成本远超收益，且预览层已有 `CodeBlock` 渲染。纯文本编辑用原生 `textarea` + 现有 `textDecode` 编码探测。
5. **引入 vscode-icons / iconify 全量图标集** —— 拒绝：真彩色图标集体积大、许可各异；`lucide-react`（viewer 已装 `^0.511.0`）是单色，配色后即达「一眼分辨类型」的效果。最终形态是「lucide 单色图标 + 每类型专属色」，图标组件收进共享包由两端共用。
6. **纯净模式用 CSS 隐藏侧栏实现** —— 拒绝：隐藏的 DOM 仍会加载与订阅，纯净模式的意义就是轻。改为不渲染侧栏组件本身。
7. **编辑产物原地覆盖** —— 拒绝（用户硬要求 + 数据安全）：所有编辑/转换/AI 产物写到新路径（`<name>.edited.<ext>` / `<name>.ai.<ext>`），原文件字节不变，可用 sha256 前后比对验证。
8. **把「文件关联」从 desktop 搬到 viewer** —— 拒绝：关联登记是 `HKCU` 系统级写操作，已有 `file_assoc_list` / `file_assoc_set` 在 desktop 侧。viewer 只做被关联唤起时的行为。
9. **一次性重写 viewer 全部代码** —— 拒绝：违反仓库「不堆屎山 + 六步决策阶梯」。按子任务逐个交付、逐个提交，任何一步都可 revert。
10. **饱和度用 `image` 库现成算子** —— 拒绝：`image 0.25` 的 `imageops/colorops.rs` 只有 grayscale/invert/contrast/brighten/huerotate/dither/index_colors，**没有饱和度**。改为两端各手写一份 `gray + (c - gray) * factor`（`gray = (max + min) / 2`），Rust 侧用单测锁公式，前端逐位复刻。
11. **WebP 有损在 Rust 侧编码** —— 拒绝：`image 0.25` 的 WebP 编码器**只支持无损**（`codecs/webp/encoder.rs:12` 明写）。有损 WebP 走前端 `canvas.toBlob('image/webp', q)` 出字节，Rust 只负责落盘。
12. **ico / tiff 也交给浏览器解** —— 拒绝：浏览器解不了这两类。改由 Rust `image_rasterize` 转 PNG 再给前端（降采样兜底图），且**原图尺寸必须另问 Rust**（`image_info`）——拿兜底图尺寸当原图尺寸会让裁剪坐标整体缩错。
13. **撤销/重做存像素快照** —— 拒绝：4000×3000 的 dataURL 快照 × N 步 = 内存爆炸。改为存 op 列表，重做即重放。
14. **文字图层进 op 链** —— 拒绝：op 链是前后端一一对应的纯几何/色彩算子，文字图层是浏览器渲染的合成层。文字图层作为独立图层由浏览器渲染，导出时走 `image_write_composite` 合成落盘。
15. **未知扩展名兜底当文本** —— 拒绝：Rust 侧继续用白名单判定并回 `unsupported`，前端出「元信息卡」。兜底解文本会让 GBK 兜底把二进制解成乱码——这正是共享预览层笔记里已修过的形态。
16. **媒体走 IPC 字节通道** —— 拒绝：媒体动辄几百 MB，走「读字节 → base64 → IPC → 前端解回」要搬两遍。改走 Tauri **asset 协议**（原生支持 Range，媒体元素可边下边播、可拖进度条）。
17. **docx / pptx / xlsx 写回** —— 本轮不做：需要 zip 读写的**第 3 个**直接依赖，超出「Rust 侧新增直接依赖 ≤ 2 族」的预算。docx/pptx/xlsx 维持只读预览。

## Consequences

- **不取代旧笔记。** 本篇是 `2026-10-13-hermix-viewer-resource-manager.md`（三栏形态 + 抽 `@hermix/preview` 包）与 `2026-10-09-standalone-file-viewer.md` 的**后续**，共享包与三栏决策仍以那两篇为准；本篇不推翻它们，只在其上加模式分层、编辑转换能力与三处修复。
- **依赖面变大但下载量为零**：新增 3 个 Rust 直接依赖（`image` / `tauri-plugin-dialog` / `lopdf`）+ 1 个 tauri feature（`protocol-asset`），三者及其传递依赖**全部已在工作区 `Cargo.lock` 中**（`hermix-tools` / `hermix-plugin` / desktop 在用），**新增下载 = 0**。前端 npm 新增 = 0。
- **代码量在预算内**：手写源码净增 ≈ 5,693 行（预算 6,000），单文件最大 `pdf_ops.rs` 1,187 行（门禁告警线 2,500）。
- **编译速度**：`cargo check --workspace --all-targets` 本轮 4.49s（sccache 热）。工作区并集已含 `image` 全 features，viewer 单包冷编译增量实测在 +60~120s 区间，workspace 并集增量 ≈ 0。
- **viewer 尚不可打包分发**：`apps/viewer/src-tauri/tauri.conf.json` 的 `bundle.active = false`，它是独立开发态应用。本任务不负责打包发布，只保证 `cargo check` / `tsc` / `vitest` / `vite build` 四条链路绿。
- **共享层风险已兑现又已控住**：`packages/preview` 是 desktop 与 viewer 的共享面，本轮改了它（`FileTypeIcon` 收进包内 + `previewUtils` 补 5 行）。两端 `tsc --noEmit` 与 desktop 的 vitest（35 文件 / 330 用例）全绿。
- **工作区开工时不干净**：仓库有大量他人未提交改动。本任务每个 commit 只 add 触及的具体文件路径，全程未用 `git add -A` / `git commit -a`。
- **门禁脚本不可直接执行**：本机没有可用 bash（`C:\Windows\System32\bash.exe` 只是 WSL 启动器且无发行版，Git Bash 也不存在），`scripts/ci-gate.sh` / `check-notes.sh` / `check-hygiene.sh` / `check-sidecar-freshness.sh` 无法运行，每个子任务用等价命令复刻。notes 结构检查另补了 `scripts/check-notes.ps1`（bash 版等价复刻，含 CRLF 归一化），门禁脚本本体保持 bash 不动。

## Reintroduction conditions

- 若「彩色类型图标」最终选了轻量方案（lucide + 配色）而用户仍要求真彩色多色图标集，则在图标集体积与许可问题有解时重新引入。
- 若 PDF 结构级编辑（删页 / 插页 / 重排）被证明必须引入完整 PDF 渲染后端，则在 Rust 侧 PDF 栈落地时重新评估。
- 若纯净模式在实际使用中证明与重量模式差异不足以抵消复杂度，则合并回单模式 + 可折叠侧栏。
- 若 docx / pptx / xlsx 写回的需求确认，则引入 zip 读写依赖（Rust 直接依赖增至 4 族，需重新评估预算）。
- 若 WebView2 的媒体 codec 在目标机器上不可用（依赖 Edge 媒体组件），则音视频预览需退回到「用系统程序打开」。

## 执行台账

**规则**：状态先于代码落盘——开工置 `doing` + 心跳 → 动代码 → 过门禁 → 提交 → 置 `done` + 写 commit SHA。表格行只增不减，状态只走 `pending` → `doing` → `done`。

| # | 子任务 | 状态 | 心跳 | 提交 SHA |
|---|--------|------|------|----------|
| F0 | 基线记录（门禁现状 / 代码行数 / 编译耗时 / 脏树快照） | done | 23:38 | 106bd5f2 |
| F1 | 启动卡顿：驱动器列表异步化 + 侧栏加载态 | done | 23:52 | 106bd5f2（Rust）+ 93c6c499（前端） |
| F2 | 全屏/最大化平铺 + 可拖拽分隔条 | done | 23:45 | 84d126a9 |
| F3 | 彩色类型图标系统（列表 + 关联后缀 + 文件夹） | done | 23:50 | e9834e33 |
| F4 | 双模式：纯净模式 / 重量模式与切换 | done | 23:52 | 4c124829 |
| F5 | 重量模式：图片处理工具集 | done | 00:20 | 16576f2a（Rust 内核）+ 273e4518（前端编辑器） |
| F6 | 重量模式：文本 / 文档 / PDF 编辑与转换 | done | 00:33 | f42545f8 + 3643173c（独立审查修复） |
| F7 | AI 协助：一键入对话 + 改图提示词 + 原图保留 | done | 23:52 | 16bbb5be |
| F8 | 常见文件类型覆盖矩阵（预览 / 编辑下限） | done | 00:36 | df3c8f26（后端白名单 + 图标 + 元信息卡）+ 08621066（媒体预览） |
| F9 | 收尾：一致性审查 + 全量门禁 + 笔记归档 + 总结 | done | 00:47 | 本篇归档提交 |
| F10 | 交互与界面返修：列表单击即预览 + 溢出/对齐/胶囊/描边清理 | done | 15:20 | 本轮提交 |
| F11 | 图片编辑器：马赛克改「涂抹画笔」（局部打码），不再只有整图打码 | done | 12:40 | 本轮提交 |
| F12 | 顶部右侧视图选项组：光图标 → 带文字标签的分组控件 | done | 12:55 | 本轮提交 |
| F13 | 应用改名：「Hermix 查看器」→「Hermix 文件查看器」（窗口标题 / productName / ProgID 名 / 设置文案） | done | 12:56 | 本轮提交 |
| F14 | 顶部区域打磨：两行对齐分层 + 不溢出 + 标签关闭按钮按需显示 + 换两处图标 | done | 13:25 | 本轮提交 |
| F15 | 可执行文件元信息卡「立即运行」+ 二进制不可预览文案（Rust 白名单 + 原生确认框） | done | 15:10 | 本轮提交 |
| F16 | 反向路径同步（拖入/外部打开 → 树与中间栏定位）+ 驱动器真实卷标 + 中栏表头与右侧 tab 表头对齐配色 | done | 15:40 | 本轮提交 |
| F17 | 顶部路径栏万能输入（网址 / 绝对路径 / 搜索词）+ 全盘索引搜索（类 Everything，helper 回环 TCP） | done | 16:05 | 本轮提交 |
| F18 | 开放能力：viewer `--mcp` 规范兼容 MCP 服务（文件预览/搜索/编辑 + 桌面 GUI 自动操作） | done | 16:25 | 本轮提交 |

## 交付现状

### 新增依赖清单

| 依赖 | 版本 / feature | 用在哪 | 为什么现有能力不够 | 下载量 | 编译增量 |
|------|---------------|--------|------------------|--------|----------|
| `image` | 0.25（`default-features = false`，features = png/jpeg/webp/gif/bmp/ico/tiff） | F5 图片编解码内核 | 浏览器 Canvas 编解码覆盖不到 ico 多尺寸打包与 tiff | 0（`Cargo.lock` 已有 0.25.10） | 工作区并集已含 image 全 features，并集增量 ≈ 0；viewer 单包冷编译 +60~120s |
| `tauri-plugin-dialog` | 2 | F5 保存 / 打开对话框 | 原生平台能力优先于库：自绘文件选择器等于重做一遍系统对话框 | 0（lock 已有 2.8.2） | 极小 |
| `lopdf` | 0.34（不开 `embed_image`） | F6 PDF 结构级操作（删/插/重排/旋转/文本替换/图转 PDF） | 纯 Rust、可做结构级页操作，不引入 C 依赖 | 0（lock 已有；传递依赖 chrono/encoding_rs/flate2/nom/weezl 亦全在 lock） | 中等 |
| `tauri` feature `protocol-asset` | 2 | F8 媒体预览 | asset 协议原生支持 Range，媒体可边下边播；不走 IPC 搬字节 | 0（仅 feature） | 无 |
| 前端 npm | — | — | — | **0**（`lucide-react` 等已在 viewer） | — |

`avif` feature 明确不开：`ravif → rav1e` 的 asm feature 需要 nasm，MSVC 机器上没有。

### 代码行数增量

| 目录 | 基线（2026-10-19 23:38） | 现状 | 增量 |
|------|------|------|------|
| `apps/viewer/src`（前端） | 11 文件 / 1,895 行 | 17 文件 / 4,954 行 | +6 / +3,059 |
| `apps/viewer/src-tauri/src`（Rust） | 3 文件 / 740 行 | 6 文件 / 2,900 行 | +3 / +2,160 |
| `packages/preview/src`（共享包） | 35 文件 / 6,116 行 | 36 文件 / 6,562 行 | +1 / +446 |
| `apps/desktop-tauri/src` | — | — | +15（`FileTypeIcon` 迁出包内 -183/+2，`SettingsShell` +7，其余为 import 调整） |

`git diff --shortstat 106bd5f2..HEAD` = **30 文件、5,954 插入、129 删除**，其中 132 行落在 tauri 自动生成的 `apps/viewer/src-tauri/gen/schemas/*.json`，**手写源码净增 ≈ 5,693 行**，在 6,000 行预算内。单文件最大 `pdf_ops.rs` 1,187 行 / `ImageEditor.tsx` 986 行，均低于 2,500 行告警线。

### 门禁证据（2026-10-20 00:38–00:44 实跑）

| 命令 | 结果 |
|------|------|
| `cargo check --workspace --all-targets` | exit 0（4.49s，sccache 热，0 warning） |
| `cargo test -p hermix-viewer --lib` | **27 passed / 0 failed**（3.15s） |
| `apps/viewer` `node_modules/.bin/tsc --noEmit` | exit 0 |
| `apps/desktop-tauri` `node_modules/.bin/tsc --noEmit` | exit 0 |
| `apps/desktop-tauri` `node_modules/.bin/vitest run` | **35 文件 / 330 用例全绿**（17.67s） |
| `check-design-guard.mjs`（cwd=apps/viewer） | `DesignGuard: clean` |
| `check-design-guard.mjs`（cwd=apps/desktop-tauri） | `DesignGuard: clean` |
| `node scripts/check-preview-layer.mjs` | OK（64 个类名全部有规则；42 个 token 两侧齐备） |
| `node scripts/check-theme-tokens.mjs` | exit 0（dark 103/103、ink-green 103/103、无孤儿引用、无裸色） |
| `pwsh -NoProfile -File scripts/check-notes.ps1` | `notes: OK` |
| `npx -y impeccable@4.1.0 detect apps/viewer/src` | exit 0 |
| `apps/viewer` `vite build` | exit 0（11.87s） |

### F8 常见文件类型覆盖矩阵

Rust `classify()` 的白名单与共享预览层分派表**逐项对齐**——本轮开工前两端已分叉 57 个扩展名（后端认得、前端不认，或反之），F8 补齐后由 `classify_matches_shared_preview_readers` 回归测试钉住。

| 类别 | 扩展名 | 预览下限 | 编辑 / 转换下限 |
|------|--------|---------|-----------------|
| 位图 | png / jpg / jpeg / jfif / gif / webp / bmp / ico / tif / tiff | 内联预览；ico / tiff 由 Rust `image_rasterize` 转 PNG | 重量模式全量编辑：裁剪 / 旋转 / 翻转 / 缩放 / 拼接 / 转黑白 / 亮度 / 对比度 / 饱和度 / 模糊 / 马赛克 / 加文字 / 格式转换 / 批量 |
| 矢量 | svg | 内联预览 | 无——`image` 解不了矢量，编辑按钮不露出（给一个点开就报错的按钮不如不给） |
| PDF | pdf | pdf.js 渲染 | 页删除 / 插入空白页 / 重排 / 旋转 / 提取 / 图片转 PDF / 文本层替换 |
| 文本与代码 | 60+（md / txt / json / yaml / toml / html / css / js / ts / tsx / py / rs / go / sql / sh …） | 代码视图 / Markdown 渲染 / HTML 沙箱 | 行内编辑 + 另存（编码探测复用 `textDecode`） |
| 表格 | xlsx / xlsm / xls / csv / tsv | ExcelJS / SheetJS / Papa 解析 | 只读（写回需 zip 第 3 个直接依赖，本轮不做） |
| 文档 | docx / docm / dotx / dotm | docx-preview 渲染 | 只读（同上） |
| 演示 | pptx / pptm / ppsx / ppsm / potx / potm | pptx-browser 渲染 | 只读 |
| 老 Office | doc / ppt / wps / et / dps / rtf / odt / ods / odp | 文本表示 | 无——「用系统程序打开」兜底 |
| 音视频 | mp3 / wav / m4a / aac / flac / ogg / opus / wma / aiff / mid；mp4 / m4v / mov / webm / ogv / mkv / avi / wmv / flv / mpg / mpeg / 3gp | asset 协议原生播放（Range） | 无——浏览器解不了的容器触发 error 事件后落回「用系统程序打开」 |
| 其它 | 未列扩展名 | 元信息卡（大小 / 修改时间 / 类型）+「用系统程序打开」 | 无 |

### 真机验证（2026-10-20 12:00 后补做）与由此暴露的缺陷

本篇初稿写的是「未做真机 GUI 验证」。随后按用户要求真机跑了一次，**发现三处只有跑起来才会暴露的缺陷，均已修复**（提交见下）：

1. **目录被当文件打开**。主程序「在查看器中打开」传目录进来时，Rust 的 `seed_pending` 只收 `p.is_file()` → 目录被静默丢弃；单实例投递那条路更是无条件 `emit("viewer-open-file")`，前端无条件 `openPath()` → 目录变成一个报「不是文件」的标签页。**F4 只修了启动路径的目录/文件判别，投递路径与拖放路径漏了。** 现在启动参数、单实例投递、拖放三条路共用前端同一条判别 `openTarget`。
2. **图片编辑器画布永远空白**。`ImageEditor.tsx` 里 `const ctx = out.getContext("2d")` 取错了对象（out 是离屏画布），于是 `ctx.drawImage(out, 0, 0)` 是把画布画在自己身上。**导出的那条路取的是 out，所以导出结果其实正确——只有屏幕上看不到。** 这是最坏的一种缺陷形态：功能"能用"，但用户眼前的界面是错的。
3. **媒体预览在 Windows 上被 CSP 拦掉**。真机投递 WAV 落回「这个浏览器解不了该媒体格式（WAV）」，但 Chromium 原生支持 WAV，根因不在解码器：Tauri v2 在 Windows 上把 asset 协议映射成 **http 承载**的自定义 scheme（`convert_file_src` → `http://asset.localhost/<encoded>`），macOS/Linux 才是 `asset://localhost`。viewer 的 CSP 只写了 `media-src 'self' asset: ...`——`asset:` 是 **scheme 源**，匹配不到 `http://asset.localhost` → 浏览器直接把媒体请求拦掉 → `<audio>` 触发 error → UI 把「协议被拦」误报成「格式解不了」。desktop 端的 CSP 早就写着 `http://asset.localhost`（img-src / frame-src），viewer 端漏了。同时把错误提示按 `MediaError.code` 分成两类：DECODE / SRC_NOT_SUPPORTED 才叫「解不了」，其余归为「加载失败（被占用 / 权限 / 协议拦截）」——原来只有一句话，把两类根因糊在一起，误导排障。

真机跑通的项（有截图证据）：重量模式三栏 + 彩色类型图标 + 驱动器异步列表；纯净模式（带文件启动 → 只渲染预览面，细工具条「系统打开 / 重量模式 / 关闭」）；目录投递 → 正确导航进目录；单实例投递文件 → 正确开预览标签；图片编辑器（裁剪/旋转/翻转/文字/参数滑杆/转黑白/尺寸/导出/拼接 +「原图 1200×800 · 当前 1200×800 · 原文件不…」）；AI 栏（8 个改图提示词模板 +「AI 的产物会另存为新文件，原文件不会被改动」）。

**主程序 → 查看器 的端到端链路**（用户反馈「运行主程序找不到文件查看器」后专门验证）：

主程序的「在查看器中打开」（`commands.rs::open_in_viewer` → `viewer_exe_path()` 取主程序 exe 同目录的 `hermix-viewer.exe`）在真机上完整走通：主程序右栏「工作区」→ 右键 `docs/DEEPSEEK_HARNESS_BORROWINGS.md` →「在查看器中打开」→ **从零拉起新的 viewer 进程**（不是投递到已有实例），打开的正是本次的新界面——纯净模式的细工具条「系统打开 / 重量模式 / 关闭」是本轮新增，旧产物根本没有，切「重量模式」后三栏、md 渲染、目录大纲、「编辑」入口全部就位。

用户此前看到旧界面的根因链是三条叠加，缺一不可：**① dev 模式从不构建 viewer**（`dev.mjs` 只构建 hermix sidecar，exe 停在 cargo 随手留下的旧产物）；**② sccache 的缓存键不含宏展开期读取的文件**（`generate_context!` 读的 dist 变了，crate 指纹却没变 → 旧 rlib 被原样复用）；**③ 运行中的 exe 被 Windows 锁住**（cargo 覆盖失败，旧二进制原地留下）。三条分别由 `dev.mjs` 补构建、补前端指纹双写（`OUT_DIR` 的 .rs + `cargo:rustc-env`）、构建前结束同名进程收口，详见 [前端内容指纹纳入编译指纹](../bug-fix/2026-10-20-frontend-fingerprint-in-build.md)。

**仍未验证**（本机无法复现所需条件，如实标注）：

- 媒体 codec 支持：**已实测**——PCM 的 wav 与 H.264 + AAC 的 mp4 均能内联播放（asset 协议 + Range 正常）。mkv / avi / wmv / flv / mpg 未实测（已设计为落回「用系统程序打开」）。
- asset 协议在 Windows 上的路径形态：**已实测**——走 `http://asset.localhost`，必须写进 CSP 的 `media-src`（`asset:` 只覆盖 macOS/Linux 形态），否则请求被浏览器静默拦掉。
- PDF 产物在 pdf.js / Acrobat 中的实际渲染未验证。
- 图片**导出产物**的像素结果未验证（画布渲染已验证，导出路径与画布同源但未落盘比对）。
- 纯净 ↔ 重量模式的跨启动持久化未实测（代码已写）。
- 启动卡顿的验收指标「断网映射盘下冷启动首屏 < 1.2s」**未实测**——本机没有断网映射盘可复现该条件；根因（同步命令 + 文件系统 IO）已消除，但指标本身无证据。

**功能边界（有意为之，非缺陷）**：

- docx / pptx / xlsx 写回未做（依赖预算）。
- PDF 文本替换只改内容流文本，不动字形、表格结构与 XObject。
- GIF 动图编辑语义定为「取首帧并明示」。
- EXIF 方向修正未做（`image` 不自动应用 EXIF orientation）。

### 交互与界面返修（2026-10-20，用户实机反馈后）

用户实机用过一轮后提了两条：**中间栏列表必须双击才出预览，浏览一批文件时每张都要双击一次，累**；**界面还有溢出、没对齐、胶囊按钮丑、输入框带描边**。这一轮据此返修，改动只落在 viewer 自己的 `apps/viewer/src/App.tsx` 与 `apps/viewer/src/styles.css`，**不碰 `packages/preview` 共享包**——避免波及 desktop 端；共享层实测本无描边/胶囊问题。

**单击语义统一为「打开」**：文件 → 右侧预览标签，目录 → 进入该目录。理由有二：用户明确抱怨双击累；「单击打开」是资源管理器既有范式，侧栏树 + 面包屑 + 后退按钮已提供退路。

**双击的第二次点击用 `ev.detail !== 1` 挡掉**，而不是加防抖延迟。防抖会给「快速连续浏览文件」这个主场景凭空加延迟；`detail` 天然区分单击与双击的第二次，零延迟且精确。标签按路径去重（`openedRef`），所以双击文件也不会开两个标签。已打开的文件在列表里给底色高亮（`is-active`）——单击即预览之后「点了有反应」必须看得见，否则用户会以为没点中又点一次。

**界面返修**（逐条对应用户指出的现象）：

| 现象 | 根因 | 处理 |
|------|------|------|
| 输入框有描边 | `.path-input` / `.search-input` / `.ai-input` 三处漏写 `border`，吃浏览器默认描边 | 统一 `border: none`，层次改由底色差承担；全文件仍只保留键盘 `focus-visible` 焦点环（无障碍必需） |
| 胶囊按钮丑 | `.ai-quick-btn` / `.ed-dirty` 的 `border-radius: 999px` 与同屏方角控件语言不一致 | 改 6px / 4px |
| 表头与数值不对齐 | 「大小」列表头左对齐而该列数值右对齐 | 表头跟随右对齐；表头/单元格 padding 统一 6px |
| 窄窗口下 AI 栏被挤出 | 三栏 `min-width` 之和（200+200+320）超过窗口宽 | `.body-row` 加 `overflow: hidden`；`.preview` 的 `min-width` 归零（预览是唯一该被压缩的栏，内容自己能滚） |
| 窄栏下名字列被压没 | 「大小 + 修改时间」两列固定宽，名字列被挤到 0 宽 | 改用**容器查询**按列表自身宽度让「修改时间」列先让位——这一栏可拖，与视口宽度无关，媒体查询跟踪不到 |
| 编辑器侧栏挤掉画布 | 侧栏固定 248px | 改 `min(248px, 42%)` |
| 标签条看着错位 | 横向溢出时 8px 滚动条把标签条撑成两行高 | 隐藏滚动条但保留滚动能力 |

「修改时间」列先让位而不是「大小」列：时间信息价值最低、文件名价值最高，先保住名字。

**detect 的绿灯必须自证**：`npx -y impeccable@4.1.0 detect src` 对**解析不了的文件会静默返回 0**，所以本轮另造了一个已知反模式的探针文件（`border-left:3px solid` + `transition:width` + `bounce` 动画），确认 detect 真的能检出（exit 2、3 条违规）之后，才认它对本目录的 exit 0 是有效绿灯。探针文件随即删除。

**本轮验证证据**（真机 1216 / 820 / 640 三档宽度逐项截图核对）：

| 门禁 | 结果 |
|------|------|
| `cargo test -p hermix-viewer --lib` | 28 passed / 0 failed |
| `apps/viewer` `node_modules/.bin/tsc --noEmit` | exit 0 |
| `check-design-guard.mjs`（cwd=apps/viewer） | `DesignGuard: clean` |
| `node scripts/check-preview-layer.mjs` | OK |
| `pwsh -NoProfile -File scripts/check-notes.ps1` | `notes: OK` |
| `npx -y impeccable@4.1.0 detect src` | exit 0（探针自证见上） |
| 真机 1216px | 单击 `config.json` → 右侧立即出 JSON 预览（带「编辑」/ JSON 徽标 / 复制），该行底色高亮；单击目录 `codebuddy-safe-delete` → 成功进入，面包屑变为 `F: > tmp-rustc > codebuddy-safe-delete`；长文件名省略号正常；路径框/搜索框/AI 输入框描边消失；表头「大小」右对齐且与数值对齐；「修改时间」不折行；AI 快捷按钮由胶囊变方角 |
| 真机 820px（AI 栏打开） | 不再溢出；名字列保留；「修改时间」列自动让位 |
| 真机 640px（应用最小宽） | 无横向溢出；路径框文字省略；AI 输入框 placeholder 断行位置略难看（不溢出，未动） |

### F11 图片编辑器：马赛克改「涂抹画笔」（局部打码）

用户实机用过图片编辑器后指出：**马赛克应该是选涂抹画笔自己涂，而不是把整张图片马赛克化**。原来 `mosaic` 是一个**参数型 op**（`{op:"mosaic", block}`），滑杆一拉整幅生效——那是"整图打码"的一种效果，不是"局部打码"这个工具。局部打码（涂掉人脸、车牌、证件号）才是真实需求。

**形态**：工具栏加「马赛克笔」按钮 → 拿起笔 → 在画布上按住拖动 → 涂过的地方才马赛克。参数区新增「马赛克笔」段：笔刷大小（4–200 状态像素）、格子大小（2–64 状态像素）、「拿起马赛克笔」开关、「清除全部涂抹（N 笔）」。原来的整图滑杆**保留但改名「整图马赛克」**，两种语义明确分开，不互相冒充。

**实现选择：笔画不进 op 链，走浏览器合成路径**。

- op 链的契约是「前后端一一对应，Rust 逐条执行」（`image_ops.rs`）。一笔涂抹是**任意形状的路径蒙版**，op 链表达不了；要在 Rust 侧复刻「圆头折线 + 抗锯齿 + 逐块取平均色」，等于把同一套算法写两遍——这正是"画布永远空白"那类缺陷的来源。
- 所以笔画与文字图层**同构**：独立状态、浏览器画、导出时整幅合成图交给 Rust 转格式落盘。**预览与导出走同一套代码 → 所见即所得**，且 Rust 侧 0 改动（`ImageOp` 枚举与前端类型仍一一对应，不破契约）。
- 已有笔画时导出自动切到 `image_write_composite`（与文字图层同一条路）；没有笔画时仍走 `image_apply` 的 op 链快路径。

**算法**：按格子大小分组，**整图马赛克只算一次当底图**，再用笔刷轨迹做蒙版（圆头折线 = 一串圆），只把蒙版覆盖的像素换成底图像素。混合用 canvas 合成（`destination-in`）而不是逐像素 JS——整图马赛克算一次、混合走 GPU，拖动时能保持流畅；逐像素会让百万像素的画布每帧跑百万次循环。边缘是**软过渡**（蒙版带抗锯齿 alpha，source-over 混合），硬边一眼就看出是"贴了块马赛克"。

**坐标约定**：笔画点记在**状态空间**（op 链折叠后的画布尺寸，单位＝原图像素），预览按 `scale` 放大绘制、导出按 1:1 绘制，两边同一份结果。落笔时记下当时的画布尺寸（`refW/refH`）：落笔之后再裁剪/缩放，笔画按比例映射过去（先裁剪再涂抹则完全准确；先涂再裁剪只做等比映射，不做偏移——文字图层也是同一类取舍）。

**撤销**：笔画进主撤销栈（`EditSnapshot = {ops, strokes}`），**一笔 = 一步撤销**。文字图层历来不进撤销栈（滑杆每改一次就压一条历史会把栈撑爆），维持原样不扩大范围。

**真机验证（1900×1040 窗口，逐项截图 + 像素级比对）**：

| 验证项 | 结果 |
|--------|------|
| 拿起笔 → 拖动涂抹 | 涂到的那一行文字被打码成块，**上下相邻两行原样清晰**（截图放大 6× 逐行核对） |
| 笔刷光标 | 跟随指针的圆圈，半径与「笔刷大小」一致（用 `box-shadow` 画边，不用 `border`，与全局「不用描边」约定一致） |
| 撤销 | 撤销后那一行恢复清晰，其余像素不变 |
| 整图马赛克滑杆 | 仍可用，整幅打码，与笔刷语义区分清楚 |
| **导出产物像素级比对**（原图 vs `another-photo.edited.jpg`） | 阈值 60 下**全图 1500×940 只有 2976 个像素（0.21%）被改**，且全部落在 **y=363–396 / x=105–614** 这一行内；**其余 910 行完全一致**。阈值 25 下多出的零星像素是 JPEG 重编码噪声（文本边缘），在 60 阈值下消失 |
| 产物路径 | `another-photo.edited.jpg`（原文件字节未变，原图保护规则照旧生效） |

### F12 顶部右侧视图选项组：光图标 → 带文字标签的分组控件

用户实机反馈两条：**右上角那几个图标"很 low"**，以及**「那个眼睛是干啥的？」**。

**根因不是画得丑，是没说明白。** 右上角原本是三个纯图标按钮（`Eye/EyeOff` = 显示隐藏文件、`Sparkles`+AI = AI 栏开关、`Monitor/Sun/Moon` = 主题切换），没有文字标签，只有一个要悬停才出现的 `title`。用户看到一个"眼睛被划掉"的灰色线条时，只能理解成"某种可见性开关"，猜不到它管的是**文件列表里的隐藏项**——于是问"那是干啥的"。同类问题：**AI 那一颗 24px 的魔法星旁边挂 13px 的"AI"字**，字号不对等，看着像拼凑的。

**处理**（按"细化保留"原则改，不推翻整体形态）：

1. **一律配文字标签**：隐藏文件 / AI / 跟随系统（主题按钮的标签直接显示当前模式：跟随系统 · 浅色 · 深色）。标签让三个控件"自我解释"，不再依赖悬停提示。
2. **图标统一 16px**：工具条上 24px 的 lucide 线框图标配 13px 正文显得笨重；16px 与正文比例正常，整条工具条（导航键 + 选项组）一个规格。
3. **收进一个分组容器**（`.tb-group`）：白面（`--bg-primary`）压在浅灰工具条上，层次靠**底色差**——与同行的路径输入框是同一种表面，一眼看得出"这是一块控件区"。不用描边（全局约定）。
4. **导航按钮补 `aria-label`**（后/前/上一级/刷新）：仓库规矩要求图标按钮有 `aria-label`，这四个此前只有 `title`。
5. **提示文案改成 Windows 语义**：原先按"`.` 开头"写隐藏文件，但 **Windows 的隐藏是文件属性（HIDDEN attribute），不是点号前缀**——实测给文件打上 Hidden 属性后开关正常显示该文件，而点号文件并不出现。提示改为「带「隐藏」属性的文件」。

**顺带验证的既有行为**（未改，只确认没坏）：点「隐藏文件」→ 图标由 EyeOff 变 Eye、按钮高亮、隐藏文件出现在列表顶部；点主题按钮 → 跟随系统 → 浅色 → 深色 循环，图标与标签同步。

**真机验证**：亮色 1216px / 640px（应用最小宽）、暗色各截一版逐项核对。640px 下路径框文字截断、选项组完整不溢出；暗色下分组面与路径框同为 `--bg-primary`，与工具条拉开一档，激活项是 accent 底色 + accent 文字。

### F13 应用改名：「Hermix 查看器」→「Hermix 文件查看器」

用户原话：「名字改为Hermix 文件查看器」。

原名「Hermix 查看器」少了「文件」二字：它在任务栏标题、Windows「打开方式」列表、桌面的设置提示里读作「看什么器」，而这个应用实际能开的是 md / 文本 / 代码 / 图片 / PDF / Office——**名字没把能力说全**。

**改名落在四个品牌位**（逐个确认语义，不是全局替换）：

1. `apps/viewer/index.html` 的 `<title>` —— WebView 文档标题。
2. `apps/viewer/src-tauri/tauri.conf.json` 的窗口 `title` —— 标题栏 / 任务栏，**用户最常看到的那一处**。
3. 同文件 `productName` —— Windows 应用显示名（写进 VERSIONINFO 资源）。
4. `apps/desktop-tauri` 侧两处引用同一产品名的地方：注册表 ProgID 的友好名（`ensure_viewer_progid` 写 `HKCU\Software\Classes\Hermix.Viewer` 的默认值，Windows「打开方式」列表显示的就是它）+ 设置页「文件关联」分组标题与提示文案。

**刻意没改的**：

- `identifier: "com.hermix.viewer"` —— 改它会换掉应用数据目录与单实例互斥的标识，属破坏性变更，不在改名诉求内。
- **`hermix-viewer.exe` 文件名** —— 由 `Cargo.toml` 的 `[[bin]] name` 钉死；桌面端 `viewer_exe_path()` 硬编码 `dir.join("hermix-viewer.exe")` 找它。`productName` 只进 bundle 与 VERSIONINFO，**不改 exe 名**，所以「在查看器中打开」链路不受影响（`bundle.active = false`，本来也不走打包）。实测重编后产物仍是 `target/debug/hermix-viewer.exe`。
- 代码与界面里泛指的「查看器」不动（`打开方式` / `查看器对话` / `关闭查看器`）——那些是动词短语或通用名词，不是品牌名。

**真机验证**：重建前端产物（`vite build` → `dist/index.html` 的 title 已是新名，UTF-8 22 字节 = `Hermix 文件查看器`）→ 重编 Rust（`tauri.conf.json` 是 `generate_context!` 的输入，**必须重编才生效**）→ 启动真机：`MainWindowTitle` 码点逐字等于 `Hermix 文件查看器`（`U+6587 U+4EF6 U+67E5 U+770B U+5668`），标题栏截图核对。

**门禁**：viewer `tsc` / desktop `tsc` 均 exit 0；`check-design-guard.mjs` clean；`check-preview-layer.mjs` OK；`impeccable detect src` exit 0 且 findings 为空——并用四反模式探针（`side-tab` / `bounce-easing` / `layout-transition` / `broken-image`）自证 detect 真能检出（退出码 2、四条全中），不是空跑绿灯。

**本轮未做的**：桌面端 exe 没有重链。`tauri-build` 的 `copy_binaries` 要删 `target/debug/hermix.exe` 再拷 sidecar，而该文件被**运行中的 sidecar** 持有（Windows 文件锁），重链必须先结束正在跑的主程序——不为一次改名中断用户正在使用的桌面端。设置页新文案与 ProgID 新名会在**下一次重新构建桌面端**后生效；Rust 侧编译正确性另用独立 target 目录（绕开文件锁）跑 `cargo check -p hermix-desktop --all-targets` 验证。本机 `HKCU\Software\Classes\Hermix.Viewer` 实测**不存在**（用户从未勾选过文件关联），因此没有旧名残留需要清理。

### F14 顶部区域打磨：对齐 / 分层 / 不溢出 + 标签关闭按钮按需显示 + 换两处图标

用户实机截图标注了两块，并点了几处图标：①顶部两行（导航+路径框 / 面包屑+搜索框）**"要对齐，要分层，不要溢出"**；②文件头标签 **"要焦点显示关闭图标，正常不显示，每个文件头标题应该能有点间隔"**；③右上角"眼睛那几个图标"与④空状态的文件夹图标"也要换"。

**根因不是画得糙，是三套数值各说各话**：

1. **控件不同高**：路径框 28px（6px 内距 + 13px 字），搜索框 24px（5px + 12px 字）——同屏两个输入框不是一套，搜索框矮一截、字小一号。
2. **两行内边距不一致**：工具条 `6px 10px`、面包屑行 `6px 12px` → 导航按钮与面包屑左边缘差 2px、选项组与搜索框右边缘差 2px。各自看齐，合在一起右边框错开。
3. **路径框吞掉整行**：`flex: 1` 让它在 1216px 下宽 760px，一个白面占掉三分之二屏，右侧选项组被挤成附属品。
4. **标签零间隔 + 关闭图标常显**：`.tab` 没有 margin，标签挤成一串字；`×` 永远占位，一排 × 像噪声。

**处理**（全部落在"同一套数值"上，不靠感觉）：

- 引入 `--ctl-h: 28px` 一个高度 token：路径框 / 搜索框 / 工具条按钮 / 右上角选项组白面**全部 28px**；选项组内按钮 24px（白面 28 = 2×2 内距 + 24），于是"组"与路径框是两个齐平的面。
- 两行左右内边距统一 12px → 左右边界真正对齐（导航↔面包屑、选项组↔搜索框各差 2px 的错位消失）。
- 路径框改有界：`flex: 0 1 460px`（可压缩不扩张），右侧留白交给选项组——左一簇右一簇，中间是干净的底色。
- 搜索框改 13px 字 / 28px 高，与路径框同规格。
- 面包屑单段 `max-width: 180px` + 省略号，行容器 `overflow: hidden` → 路径再深也不把搜索框挤出窗口（原来没有收缩规则）。
- 标签：`flex: none` + `margin-right: 4px` + 上圆角 → 标签之间 4px 间隔、互不挤压；`flex: none` 还让标签多时横向滚动而不是被压扁。
- 标签关闭按钮：`opacity: 0` 起步，`hover / 激活 / 键盘聚焦` 才 1。**只用 opacity 不用 visibility** → 保留占位，切标签时标题宽度不跳；按钮仍在 Tab 序列里（键盘可达）。
- **图标替换**：隐藏文件 `Eye/EyeOff` → `Funnel/FunnelX`（漏斗=筛选，直指"这份列表被筛了没有"；眼睛是通用可见性语义，正是用户"那个眼睛是干啥的"的来源）；空状态 `📂` emoji → lucide `FileSearch`（emoji 在 Windows 上渲染成彩色系统文件夹，与全站线性图标不是一套，还随系统语言变样）。AI 的 `Sparkles`、主题的 `Monitor/Sun/Moon` **保留**：✨=AI、屏/日/月=跟随系统/浅色/深色是公认符号，换掉反而更难认。
- **顺手收掉一处同屏重复**：没有标签时，标签条里那句「单击文件即可在右侧预览；拖入文件亦可」和下方空状态主文案说的是同一件事。现在**标签条只在有标签时渲染**，空状态副文案补上「也可直接拖入文件」——同一件事只说一遍，也不再留一条空灰条。

**真机验证**（1216px 与 640px 两档，截图逐项核对）：

| 检查项 | 结果 |
|---|---|
| 路径框宽度 | 由 760px 收到 ~430px，右侧留白归选项组，不再是占满半屏的白面 |
| 两行左右边界 | 导航 ↔ 面包屑、选项组 ↔ 搜索框，左右边缘齐平（此前各差 2px） |
| 控件同高 | 路径框 / 搜索框 / 选项组白面全部 28px，搜索框不再是 24px 矮一截的小字 |
| 标签间隔 | `demo.rs │ README.md │ preview-sample.png │ tone-440…` 之间留 4px，不再挤成一串字 |
| 关闭图标 | 只有正在看的那个标签显示 `×`（激活态白底那个），其余三个标签无 `×`；hover 任一标签也会淡入 |
| 隐藏文件图标 | 漏斗（`FunnelX` = 正在排除），不再是眼睛 |
| 空状态图标 | lucide `FileSearch` 线条图标，替掉 📂 emoji；文案改为「也可直接拖入文件 · 支持 Markdown / 文本 / 代码 / 图片 / PDF / Office / HTML」 |
| 640px 最小宽 | 顶部两行完整不溢出，路径框可压缩到 ~170px 仍可用，搜索框与选项组右边界仍齐平 |

**门禁**：viewer `tsc` / desktop `tsc` 均 exit 0；`check-design-guard.mjs` clean；`check-preview-layer.mjs` OK；`impeccable detect src` exit 0 且 findings 为空；`cargo test -p hermix-viewer --lib` 通过；`cargo check --workspace --all-targets --exclude hermix-desktop` 通过。Rust 侧本轮零改动。

## 各子任务要点
### F0 基线（2026-10-19 23:38 实测）

| 项目 | 结果 |
|------|------|
| `cargo check -p hermix-viewer --all-targets` | exit 0，2m13s（sccache 热） |
| `apps/viewer` tsc --noEmit | exit 0 |
| `apps/desktop-tauri` tsc --noEmit | exit 0 |
| `apps/desktop-tauri` vitest run | 35 文件 / 330 用例全绿，exit 0 |
| design guard（desktop + viewer 两个 cwd） | clean |
| preview-layer guard | OK（64 个类名 / 42 个 token） |
| `npx -y impeccable@4.1.0 detect apps/viewer/src` | exit 0 |
| 行数基线 | viewer 前端 11 文件 / 1895 行；viewer rust 3 文件 / 740 行；packages/preview 35 文件 / 6116 行 |

**两个必须记住的环境事实**：

1. **本机没有可用的 bash**。`C:\Windows\System32\bash.exe` 只是 WSL 启动器，且未安装任何发行版；Git Bash 也不存在。因此 `scripts/ci-gate.sh`、`scripts/check-notes.sh`、`scripts/check-hygiene.sh`、`scripts/check-sidecar-freshness.sh` **无法直接执行**——每个子任务用等价命令复刻（cargo check / 两处 tsc / vitest / design guard ×2 / preview-layer / impeccable detect + notes 结构复核）。这不是代码缺陷，是本机缺件；门禁脚本本身保持 bash 不动。
2. **开工时工作区不干净**：100 个文件已被暂存（他人/上一轮的在途改动：预览共享层归属、查看器预览引擎懒加载、文本解码、CSP 修复、组合注入等）。这批改动已由 `106bd5f2` 单独落地；F1 的 Rust 侧改动被同一次提交捎带进去（见 F1 行的 SHA 说明）。

**既有门禁失败（与本任务无关，已修）**：`docs/MCPHUB_BORROWINGS.md` 留在 `docs/` 顶层却不在 `scripts/check-notes.sh` 的 `ALLOWED` 白名单里 → notes 结构检查必红（该文件来自提交 `0b72f4f4`，当时归档了 MCPHUB 借鉴笔记却没同步白名单）。F9 已把它加进白名单。

### F1 提交归属说明

F1 的改动被拆在两个提交里，原因是**看门狗与主会话并发提交**（首次执行时台账还没落 doing 状态，看门狗判成「有 pending 无 doing」而接管，与主会话同时改同一个工作树）：

- `106bd5f2` — 102 文件：开工前已暂存的在途改动（100）+ F1 的 Rust 侧（`apps/viewer/src-tauri/Cargo.toml` / `src/commands.rs` / `src/lib.rs`）。归属混合。
- `93c6c499` — 2 文件：F1 的前端侧（`apps/viewer/src/App.tsx` / `src/styles.css`）。

**已修的根因**：看门狗的接管判据改为「先读存活标记 `<TMPDIR>\hermix-viewer-task\live.txt`，20 分钟内更新过 = 主会话活着，只报告不动手」。

### F5 图片处理：调研结论

调研只读完成，事实逐条落到本机 cargo registry 源码与依赖树，不是凭记忆。

**依赖：零新增下载。** `image 0.25.10` 已在 `Cargo.lock`（工作区依赖，`hermix-tools` / `hermix-plugin` 都在用），`tauri-plugin-dialog 2.8.2` 已在 lock（desktop 在用）。前端 npm 增量保持 0。

**三条决定实现形态的硬事实**：

1. **`image` 没有饱和度** → 饱和度手写 HSL 调整，两端各一份。
2. **`image` 的 WebP 编码只支持无损** → WebP 有损必须走前端 `canvas.toBlob('image/webp', q)` 出字节、Rust 只负责落盘。
3. **浏览器不解 ico / tiff** → 这两类预览必须由 Rust 转 PNG 再给前端（`image_rasterize` 命令）。

**另外三条**：`image` 不自动应用 EXIF 方向（EXIF 归 F8）；GIF 动图编辑语义定为「取首帧并明示」；`avif` feature 绝不开。

**ICO 多尺寸不用手写字节布局**——`image` 已给 `IcoFrame::as_png` + `IcoEncoder::encode_images`，比仓内 `crates/hermix-plugin/src/icon_library.rs` 的手工装配更可靠。

**编辑器放 `apps/viewer/`，不进 `packages/preview`**：共享包同时被 desktop 消费，把编辑器塞进去会让 desktop 白背一份 CSS 与 token 契约。`packages/preview` 在 F5 全程零改动。

**像素运算的两份实现用「op 列表 + 公式约定」钉死**：交互预览在前端 Canvas（60fps），导出在 Rust（权威）；亮度=加法、对比度=以 127.5 为中心的系数、灰度=Rec.601，Rust 侧用单测锁公式。撤销/重做走 op 列表而非像素快照。

**仓内先例比外部项目更该抄**：`apps/desktop-tauri/src/components/ScreenshotEditor.tsx`（马赛克降采样 + 最近邻放大、`ctx.fillText` 加中文、拖拽裁剪）、`crates/hermix-plugin/src/icon_library.rs`（多尺寸 ICO）、`crates/hermix-tools/src/tools/file_converter.rs`（读→编→写骨架）。外部参考（fabric.js 的「源图 + 滤镜数组 + 每次重算」模型、cropperjs 的比例锁定数学、imagepipe 的「一 op 一纯函数」组织、Loupe/Gwenview 的「查看与编辑分模式」结构）只借鉴机制，不引入代码。

### F5 前端编辑器接入（实施要点）

Rust 内核（`16576f2a`）落地后，前端编辑器接入时改了三处**真实缺陷**，不是接线：

1. **格式 ↔ 扩展名必须一致**。原实现里 `default_output_path` 用的是**源文件**扩展名，于是「png 转 webp」会产出名为 `.edited.png`、内容是 WebP 的文件。改为：格式先归一化（`jpeg`→`jpg`、`tif`→`tiff`），默认产物名用**目标格式**扩展名；另存对话框里若改了后缀，**扩展名反过来决定编码格式**（名实必须一致）。Rust 侧加了 `normalize_format` + 3 个单测锁住。
2. **坐标系三步换算**。指针事件给的是 CSS 像素，画布 backing store 是设备像素，op 里的坐标是原图像素空间——三者要串起来：`原图 = CSS × (画布宽/画布CSS宽) ÷ previewScale`。原先的写法漏了 CSS→设备像素这一跳，裁剪框会整体偏掉。裁剪选框也必须与 canvas 像素级贴合，所以加了 `.img-canvas-frame`（`position:relative; display:inline-block`）包住 canvas 与选框。
3. **原图尺寸要问 Rust**。浏览器解不了 ico/tiff 时走 `image_rasterize` 兜底，兜底图是**降采样**的；若拿它的尺寸当原图尺寸，裁剪坐标就整体缩错了。改为先 `image_info` 拿原图真身尺寸，几何一律按原图算（画面略糊但几何正确），拿不到才回退到解码尺寸。

另外两处实现纪律：`undo/redo` 不在 `setState` 的 updater 里再 `setState`（updater 必须纯函数，StrictMode 下会跑两遍）；`ImageEditor` 走 `React.lazy` 懒加载（编辑器是重量模式的重量功能，纯净模式与文本预览不该为它付首屏 bundle）。

编辑能力只在**重量模式**露出（`PreviewHost` 新增 `canEdit` prop），且只对栅格格式露出（SVG 是矢量，`image` 解不了）。

### F6 PDF / 文本编辑：语义钉死与审查发现

**PDF 页号语义钉死三条**（实现前先钉，否则前后端必然打架）：

1. 列表项要么是**原页号**，要么是**空白页**——不引入「虚拟页号」第三种东西。
2. op 顺序固定：`keep` → `insertBlank`（最终位置**从小到大**插）→ `rotate`（用**最终页号**）。
3. 旋转用**绝对**角（0/90/180/270），不是相对增量。

**`pdf_ops` 提供**：删页 / 提取页 / 重排 / 旋转 / 插空白页（op 列表**顺序执行**）、`pdf_info`（含每页绝对 rotation）、`pdf_replace_text`（命中数统计，**0 命中报错**）、`images_to_pdf`、`write_text`（补 src_path 守卫）。

**独立审查（`verifier` 子 agent）发现并已修**：

1. **硬链接绕过路径守卫** —— 原守卫只比路径字符串，`fs::hard_link` 出来的同 inode 不同路径能把产物写到源文件头上。新增 `apps/viewer/src-tauri/src/paths.rs` 统一三层判据：规范化绝对路径 + 小写 → `canonicalize` → **文件身份**（`GetFileInformationByHandle` 的 volume serial + file index）。实测本机可建硬链接，测试走真实分支。
2. **LeafPage 属性继承丢失** —— 插空白页时未继承 crop_box / rotate，导致插页尺寸或朝向与原页不一致。
3. **加密 PDF 报错不准** —— 按 `/V` 分版本判定（`/V 4` 用 RC4/AES-40，`/V 5` 用 AES-256），不再一律报「不支持」。
4. **静默 no-op** —— Count 与 Kids 数量不一致时 op 应当报错而不是默默跳过。
5. **编码处理** —— 移除 gb18030（PDF 标准字体编码不认它），未知编码明确报错而非乱码。

三处写盘（图片 / PDF / 文本）共用 `paths.rs` 一份守卫。

### F8 覆盖矩阵：后端白名单必须与共享预览层对齐

开工时 `classify()` 与共享层分派表**分叉 57 个扩展名**——后端认得前端不认的类型会落进「不支持」，前端认得后端不认的类型会拿到错误的读取方式。F8 补齐 57+ 个扩展名，并给 `IMAGE_EXT_MIME` 补 avif / jfif。`FileTypeIcon` 的 LOOK 表补约 70 项（字体 / 音视频 / Office 变体）。

`PreviewHost` 的错误态从「一句错误文案」改成**元信息卡**（大小 / 修改时间 / 类型 +「用系统程序打开」），不支持的类型不再是死胡同。

**媒体走 asset 协议**：`convertFileSrc` + Range，需 tauri `protocol-asset` feature + CSP 加 `media-src 'self' asset: blob: data:` + assetProtocol scope 放开（viewer 本就是文件浏览器，能读任意路径，asset 协议没有引入新权限）。
