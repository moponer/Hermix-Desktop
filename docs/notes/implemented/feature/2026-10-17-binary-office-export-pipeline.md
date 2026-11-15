# Decision Note: 二进制 Office 导出管线（纯 Rust 三件套）

Status: implemented — 2026-10-17 全部三批落地（同日完成）

## 背景

`document-authoring` 画布管线只能交付画布 HTML 与打印 PDF；用户要"可编辑的
.pptx/.docx"时模型被明令禁止硬造（`document-authoring.md:77-79`）。历史上
用户曾要求集成 Rust Office 生成能力，经查全仓库无任何落地痕迹（无 docx-rs /
umya / xlsxwriter / calamine 引用）。读取链路已完备（`markitdown` 读 Office
→ Markdown；`file_converter` Office → PDF），唯一缺口是**生成**。

## Decision（提案）

在 `hermix-tools` 新增 Office 生成工具（或扩展现有 file_converter），采用
纯 Rust 三件套，覆盖三类产物的**可编辑二进制导出**：

| 能力 | crate | 理由 |
|---|---|---|
| DOCX 生成 | `docx-rs` | 社区标准选择，纯 Rust，成熟 |
| XLSX 生成 | `rust_xlsxwriter` | 维护最活跃（作者即 Python xlsxwriter 原作者）；读改用 `umya-spreadsheet` |
| PPTX 生成 | `ppt-rs` | 生态唯一活跃选项（2026-09 仍在发版，Rust 2024 edition），支持富格式/主题/模板 |

选型原则：全走 OOXML（zip + XML），零外部二进制、零 Python、零 LibreOffice，
契合便携零安装与"不感知外部资源"禁区。

### 集成方式

- 新工具 `office_export`（或并入 file_converter 的正向转换）：输入 =
  结构化 JSON（大纲/页数组/表格数据）或已落盘的画布 HTML，输出 = 磁盘上的
  .docx/.pptx/.xlsx 路径。
- 与画布管线衔接：`document-authoring` 落盘 HTML 后，同一回复内可调
  `office_export` 产出可编辑版本；画布仍是预览层，二进制是交付层。
- 风险分级 Safe（写工作区文件，无 shell、无网络）。

### 分批

1. **XLSX（已落地 2026-10-17）**：`rust_xlsxwriter` 0.99.1 进
   `hermix-tools`；新工具 `office_export`（toolset "file"，RiskLevel
   **Caution**——与 write_file 保持 file 工具集风险分级一致，此为对提案
   "Safe" 的事实修正）。参数 = path + format("xlsx") + sheets[{name,rows}]，
   单元格 string/number/bool/null；前置校验（sheet 名 ≤31 字符且唯一、
   列 ≤16384、总行 ≤10 万 sanity 帽）失败不落盘；写后回执绝对路径 +
   行数 + 字节数，回执内提示 markitdown 读回校验。docx/pptx 传参显式
   报错（含去向说明）。5 个单测（多表写入、格式拒绝、重名/坏单元格、
   覆盖写、schema 三要素）。
2. **DOCX（已落地 2026-10-17）**：`docx-rs` 0.4.22。`office_export` format
   分派（xlsx/docx/pptx 三分支）；docx 参数 = `blocks`
   [{type: heading1|heading2|heading3|paragraph|bullet, text}]，heading 走
   内置 Heading1/2/3 样式，bullet 共享一个 AbstractNumbering 定义。注意：
   docs.rs 页面文档覆盖率仅 5.5%，`Level::new` 实为 5 参（含 `LevelJc`）、
   `Paragraph` 用 `.style("Heading1")` 而非 `add_style`——以 crate 源码为准。
3. **PPTX（已落地 2026-10-17）**：`ppt-rs` 0.2.36。参数 = `title` +
   `slides` [{title, bullets[string]}]，`create_pptx_with_content` →
   `std::fs::write`。9 个单测覆盖三格式（写读回 PK 头、显式拒绝、坏参数、
   覆盖写）。`hermix-tools` 748 passed 全绿。

## Alternatives considered

- **python-docx / python-pptx / openpyxl（Python 侧车）**：能力最全但引入
  Python 运行时与包管理，破坏便携零安装；Hermix 的 `code_execution` 是可选
  环境，不能作为文档交付的依赖。否决。
- **LibreOffice CLI**：`file_converter` 已在用它做转出，但生成方向要先把
  HTML 转 ODF 再转 OOXML，版面不可控且携带 500MB+ 外部安装。否决。
- **WordCraft / GridCraft / DeckCraft 套件**（2026-10 新出，Rust 重写
  Office、MCP 驱动）：API 未稳定、单人项目、引入三个重型运行时。列为观察项，
  每 6 个月复评。
- **维持现状（只出 HTML/PDF）**：榜单证据（anthropics 官方 pptx/xlsx 技能
  霸榜、awesome-claude-skills 办公类第一是 xlsx）表明可编辑 Office 是高频
  真需求；现状等于把这个场景让给"打不开全歪的假文件"或竞品。否决。

## Consequences

- **编译时间与体积**：三个 OOXML crate 纯 Rust（zip + quick-xml 系），预计
  新增编译时间 < 30s、二进制 < 3MB，可接受。
- **ppt-rs 成熟度风险**：发行历史短，复杂模板（母版/动画）覆盖有限；第一批
  只承诺"结构正确、样式基础"的 PPTX，母版级还原不承诺。
- **双交付形态并存的心智成本**：画布 HTML（预览/打印）与二进制（可编辑）
  并存，`document-authoring` 需同步改写"交付形态"段——该改写归入本笔记
  落地 PR，不另开提示词批次。
- **放弃的东西**：不做 Office 文件的复杂版面 1:1 还原（母版、SmartArt、
  嵌入对象）；画布是真相源，二进制导出是"结构等价"而非"像素等价"。
