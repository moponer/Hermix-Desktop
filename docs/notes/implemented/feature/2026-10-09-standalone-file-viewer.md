# Decision Note: 独立文件浏览器（hermix-viewer）+ 可折叠 AI 右栏 + 文件关联接管

Status: implemented — 2026-10-10 交付：apps/viewer（hermix-viewer.exe，第 5 个 sidecar）单 WebView 多标签 + AI 右栏（直连 /v1/）+ 文件关联设置页 + 主程序互调；一期 md/text/code/image/pdf 直渲，office 走系统程序兜底

## Decision

新增独立 Rust 二进制 **`hermix-viewer.exe`**（workspace 第 5 个 sidecar 家族成员），形态为 Tauri 独立 app（单实例复用单 WebView + 标签页）。

### 为什么必须是独立进程（需求三方约束的唯一解）

「不随主程序关闭」+「被系统设为默认打开方式」+「被主程序反向调用」三条加起来，只有一个答案：独立 exe、自己的生命周期。嵌入主程序的窗口三条都满足不了。

### 功能设计

- **多标签浏览**：单 WebView 内标签切换（不做每标签一窗口——WebView2 实例 ~100MB/个）；Rust 后端流式读文件，打开速度的真正来源在后端而非渲染层。
- **文件类型支持**：复用现有转换链——docx/xlsx/pptx 经 `officecli`/`markitdown` sidecar 转 Markdown 后用现有渲染器（预览级，非编辑级；「用系统程序打开」按钮兜底编辑需求）；PDF 走 read_pdf 能力；图片/文本/代码/MD 直接渲染。
- **类型图标**：不同扩展名不同图标。图标组件已收进共享包 `packages/preview/src/FileTypeIcon.tsx`（独立子路径出口 `@hermix/preview/FileTypeIcon`，不进 barrel 以保住预览引擎的懒加载分块），桌面端与 viewer 共用同一份，每类扩展名配专属色。详见 [hermix-viewer 双模式化与全能力重构](../feature/2026-10-19-hermix-viewer-overhaul.md)。
- **AI 右栏（可折叠）**：内嵌聊天面板连本地 server 的 agent 通道，上下文自动携带当前文件路径/选中文本；快捷指令（总结/检查/润色）形态同 WorkBuddy 参考。
- **与主程序互调**：主程序「在查看器中打开」→ 命令行参数/IPC 传路径给 viewer；主程序右栏选「全屏」时反向调用 viewer 打开（独立进程架构的自然结果）。

### 文件关联接管（敏感操作，显式授权）

- Windows 注册表 `HKCU\Software\Classes` 注册 ProgID + 扩展名关联。
- **设置页按扩展名勾选**（.md/.txt/.pdf/.png/…），不静默抢用户文件关联——抢关联必须用户显式授权（不可削减项）。

### 打包与更新

- 加入 `externalBin`（binaries/viewer），`ensure-sidecar` 编译链白拿；随在线更新的 zip 整包走，永不单独更新。

### 交付拆分

1. viewer 骨架（Tauri 第二 app + 单实例 + 命令行参数开文件）
2. 多标签 + 类型分发渲染（搬运现有预览代码）
3. AI 右栏（agent 通道接入）
4. 文件关联（设置页 + 注册表逻辑）
5. 主程序互调 + 打包链接入

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 主程序内第二窗口（tauri WebviewWindow） | 主程序退出即全关——「不随主程序关闭」直接不满足；也无法被系统设为默认打开方式（进程语义不对） |
| 纯原生 Rust 窗口（egui/wgpu） | 内存/启动确实更优，但 Markdown/表格/PDF 渲染全部重写，工作量数倍；性能瓶颈在文件读取（后端已是 Rust）不在渲染层，收益撑不起成本 |
| 每标签一个 WebView 窗口 | 内存 ~100MB/窗口，多文件场景不可接受；单 WebView 内标签切换即可 |
| 复用现成阅读器（SumatraPDF 等）嵌入/外调 | 非 Rust、不可定制 AI 右栏、打包第三方许可面；与「项目自己的文件浏览器」需求直接冲突 |

## Consequences

- **得到**：常驻的轻量文件浏览器；系统级「打开方式」入口（品牌触点）；AI 右栏让任何文件都能被 agent 分析；主程序↔查看器双向调用。
- **放弃**：极致内存（Tauri + WebView2 vs 原生）；一期不做编辑能力（预览级 + 跳系统程序兜底）。
- **风险**：与主程序同时运行时两套 WebView 内存并存（可接受，均为懒加载）；文件关联被杀软视为敏感注册表写入（HKCU 级、用户显式操作，风险低）。
- **边界**：viewer 不提供写能力（编辑跳系统程序），一期也不接 agent 写工具——先立住「看 + AI 分析」的定位。
