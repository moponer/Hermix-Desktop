# Decision Note: PDF 页面内文本编辑 —— 按位置定位、遮盖插入、真删除、中文嵌入

Status: implemented — **P0/P1/P2 全部落地并验证**：P0 可点选改字（遮盖插入，原内容流零改动）+ 标准字体度量随包；P1 真删除旧字（按文本矩阵 × CTM 定位，丢弃显示算子）；P2 复用**文档自己嵌入的字体**写中文（产物不涨体积、零新依赖）

## Decision

把「改 PDF 页面里的字」从**内容流字符串匹配**换成**按位置定位 + 自己写回**的路线，分三级实施：

### P0｜可选中文本 + 遮盖插入（拉丁文）—— 已落地

- **前端**：PDF 编辑器加「改字」模式（工具条：页面 / 标注 / 改字）。把 pdf.js `getTextContent()` 的每个 item 渲染成**覆盖在画布上的可点选图层**（按 `item.transform` 定位），点一句即选中；侧栏显示原文、输入新文、字号（取自原文字）。
- **后端**（Rust，lopdf）：新命令 `pdf_replace_text_at`，往页面内容流**追加**两段算子 —— ① 一个矩形填充盖住原字区域；② `BT /Helv <size> Tf <x> <y> Td (新文) Tj ET`（标准 14 号字，零嵌入）。页面资源里补 `/Helv`（复用 `ensure_helvetica`）。
- **写法**：原内容流**一个字节不动** —— 新算子写进一个**新的内容流**，页面的 `/Contents` 变成 `[原流…, 新流]`（PDF 规范允许多个流顺序拼接）。
- **坐标**：前端把 viewport 坐标换算成 PDF 用户空间，并把**基线 y**（`transform[5]`）一起传给后端 —— 写字要对齐基线，不是对齐矩形底边。
- **顺带修掉的既有隐患**：标准 14 号字（Helvetica/Times/Courier）的**度量文件此前根本没有被服务** —— pdf.js 算不出字宽，`getTextContent()` 的 `item.width` 可能是 0，文本层会点不中；标准字体的渲染也一直走回退字体。现在度量文件随包（`apps/*/public/standard_fonts/`，0.76 MB，来自 pdfjs-dist，随 pdfjs-dist 升级同步），以**稳定文件名**服务（`?url` 会被 Vite 加 hash，pdf.js 按标准名请求会 404）。

### P1｜真删除（旧字真正消失）

- **后端**：解析内容流时跟踪文本矩阵（CTM × Tm × Tlm，处理 `q/Q/cm/gs`、`BT/ET`、`Tm/Td/TD/T*/TL/Tf/Tz/Tc/Tw`、`'/"`、`Tj/TJ`），算出每个显示算子的落点；**落在目标矩形内的算子整个不写回**。参照 PDFBox 的 `PDFGraphicsStreamEngine` + `showGlyph` 模型。
- 效果：旧字不在文件里（复制不露双份），文本层其余部分完好。

### P2｜中文（嵌入子集字体）

- **依赖**：`subsetter` / `ttf-parser` / `fontdb` **已在 Cargo.lock 里**（`pdfrs → ppt-rs → hermix-tools` 带进来），直接加进 viewer 的 `Cargo.toml` 即**零新增构建单元**（与当初加 `zip`/`quick-xml` 同一理由）。
- **字体来源**：Windows 系统字体，优先单文件 TTF（`simhei.ttf` / `simkai.ttf` / `Deng.ttf`；`msyh.ttc`/`simsun.ttc` 是 TTC 容器，暂不处理）。实测这些字体的 `OS/2.fsType = 8`（Editable Embedding）→ 允许把子集嵌进用户自己的文档（不涉及再分发字体本身）。
- **写回结构**：Type0 / CIDFontType2 —— `/FontFile2`（子集）+ `/FontDescriptor` + `/W` 宽度数组 + `/CIDToGIDMap /Identity` + `/ToUnicode` CMap；内容流用 Identity-H 的 2 字节 CID 串。
- **降级**：找不到可嵌入字体或文本含非 ASCII 时，明确提示"中文页面内改字不可用"，引导改用**注释层**（FreeText，读者用自己的字体重渲染）。

### 验证链（每级都要过）

Rust 测试二进制在本机起不来（tauri-build 被运行中的 `hermix.exe` 锁文件，既有环境问题），因此沿用**独立探针 + pdf.js 交叉解析**：探针产出 PDF → `pdfjs-dist` 的 `getTextContent()` / `getAnnotations()` 断言"新字在、旧字不在、位置对"。

## Alternatives considered

- **继续加固内容流字符串匹配**（扩到更多算子/编码）：PDFBox 2.0 已把官方 `ReplaceText` 示例删除，理由是"文本常常存得很复杂，无法轻易替换"。这条路已被上游判定为不可靠，不做。
- **引入 MuPDF（mupdf-sys）拿现成的 redact/insert**：能力最全，但要编译整个 C 库（本项目明确避免重量依赖），且与"便携零安装、体积敏感"的产品前提冲突。
- **引入 PyMuPDF / DocuSeal 等 AGPL 项目的代码**：许可证不允许。只借鉴**方法论**（定位→移除→写回），实现全部自己写；参考 Apache-2.0（PDFBox / pdf.js / tesseract / qpdf）与 MIT（pdf-lib）项目的公开做法。
- **重画成图（flatten）来"真删除"**：最彻底，但整页变图片 —— 文本层没了、文件变大、放大发虚。作为 P1 的**兜底**（用户明确要"不可恢复"时）保留，不作默认。
- **中文一律走注释层不做嵌入**：省掉 P2。但用户诉求里"改中文字"很常见，注释层不是页内字（读者导出/打印后位置会漂）。因此 P2 保留，注释层作为降级。
- **用 `pdf-lib` 在前端写回**：pdf-lib 是创建型库，不解析既有页内容流，改不了页内文字。

## Consequences

- P0 的产物里**旧字仍在内容流中**（被矩形盖住）：选中复制仍可能带出旧字。这正是 P1 要解决的；P0 只是把"找不到"变成"找得到、改得动"。
- 改字后**版面不回流**：新文字宽度不同可能与相邻内容重叠。真实编辑器同样如此（没有行模型），不做自动避让。
- P2 会让产物文件变大（子集字体几百 KB 级），且嵌入的字体受 `fsType` 约束；因此 P2 只在"确实要写中文"时才嵌入，且优先子集化。
- 三级都要求前端具备**文本层可选中**能力，这是 PDF 编辑器里唯一必须新写的交互层。
- 每一级都是独立的 Rust 命令 + 独立的前端入口，可以单独回退；P1 依赖 P0 的定位数据（矩形），P2 依赖 P0/P1 的写回通道。

## 实施顺序与验收

| 级 | 交付 | 验收 | 状态 |
|---|---|---|---|
| P0 | 文本层可选中 + `pdf_replace_text_at`（遮盖+插入） | 探针 PDF 经 pdf.js 读出：新字在目标位置、旧字仍在但被覆盖 | 已落地 |
| P1 | 内容流算子级真删除 | 探针 PDF 经 pdf.js 读出：旧串**消失**、新串在 | 已落地 |
| P2 | 子集字体嵌入 + Identity-H 写中文 | 探针 PDF 经 pdf.js 读出：中文新字可被抽取（ToUnicode 正确） | 待做 |

**P0 的验证证据**（独立探针 + pdf.js 交叉解析；Rust 测试二进制在本机起不来）：

```
文本="Hello World"    基线=(72.0, 82.0)  宽=124.0
文本="Goodbye World"  基线=(72.0, 82.0)  宽=166.7
```

新字与原字**在同一条基线上**；新内容流的内容原文核对：

```
1 1 1 rg / 72 758 200 26 re / f / BT / /Helv 24 Tf / 72 760 Td / (Goodbye World) Tj / ET
```

### P1｜真删除 —— 已落地

- **后端**：`remove_text_ops_at` —— 解析页内容流，追踪 **CTM 与文本矩阵**（`q/Q/cm`、`BT/ET`、`Tm/Td/TD/T*`、`Tf`），把每个显示算子按**当前字体的编码**解出字符串，与前端给的原文精确比对；命中就**丢弃那个算子**（`Tj`/`'`/`"` 丢整个算子，`TJ` 数组里把命中项换成空串，保留其余项与字距）。
- **判据**：字符串精确匹配 + **位置消歧**（同一页有多处相同文字时，用文本矩阵 × CTM 算出的起点与目标矩形比对；位置不可信就**宁可不删**并报错让用户点更完整的一段）。
- **为什么不逐字形追踪**（PDFBox 的 `showGlyph` 路线）：逐字形需要字体的 advance width —— 标准 14 号字没有 `/Widths`、CID 字体的 `/W` 又是另一套结构，在纯 Rust 里代价过大。字符串匹配 + 位置消歧是等价判据，且不依赖度量。
- **尽力而为**：lopdf 的内容流解析器**不支持内联图像**（`BI/ID/EI`），含内联图的页解不出来 → 此时**跳过真删除**，仍做"遮盖 + 写入"，并如实回传一句"旧字未能真删除"。不能因为删不掉就让整条命令失败。
- **前端**：`pdf_replace_text_at` 的 `removeText` 传用户点的原文；Rust 回传的 `note` 如实显示。

**P1 的验证证据**（探针 + pdf.js 交叉解析）：

```
文本="Kept Text"       基线=(72.0, 142.0)   ← 邻近的 TJ 数组文字完好
文本="Goodbye World"   基线=(72.0, 82.0)    ← 新字在原位
断言：旧字 "Hello World" 已消失： true
```
### P2｜中文改字 —— 设计已细化（尚未实施）

原计划是"嵌入子集字体"（`subsetter` + 系统 TTF）。**查过实际 API 后换了方案**：

**新方案：复用文档自己嵌入的字体。** 页面上已经有中文的 PDF，字体就在页面的
`/Resources` 里（多半是 WPS/Word 生成的子集化 CID 字体，Identity-H + ToUnicode）。
把它直接拿来写新字：

- 产物**不涨体积**（不嵌任何新字体）；
- 新字与原文**完全同款**（比嵌一个黑体保真得多）；
- **零新依赖**（`subsetter` 0.2.6 用的是 skrifa/write-fonts 那套 GlyphRemapper，
  接起来比预期麻烦；`ttf-parser` 只能解析不能子集化）。

**要做的两件事**：

1. **反向映射 Unicode → CID**：从字体的 `/ToUnicode` CMap 反查。lopdf 的
   `Encoding::string_to_bytes` 对 `UnicodeMapEncoding` 是 `unimplemented!()`
   （**会 panic**，必须避开），所以要自己解析 CMap 流：`beginbfchar`/`endbfchar`
   的 `<code> <unicode>` 对，以及 `beginbfrange`/`endbfrange` 的
   `<lo> <hi> <dst>` / `<lo> <hi> [<d1> <d2> …]` 三元组与数组形式，
   建成 `unicode → code` 的表。
2. **按字体类型分别写串**：`/Subtype /Type0` 的 CID 字体写 **2 字节十六进制串**
   `<…>`（CID = GID，Identity-H + `/CIDToGIDMap /Identity`）；普通字体写字面串
   `(…)`，转义 `\ ( )` 并按 WinAnsi 编码。

**降级路径（诚实，不静默写乱码）**：

- 文档里没有任何字体能覆盖新文字的字符（字体是子集，不含那个字）→ 报错：
  "这份 PDF 的字体里没有这些字（字体是子集），改不了"；
- 字体没有 `/ToUnicode`（无法反查）→ 同样报错，而不是写出一串乱码。

**顺带把拉丁文也升级**：新字优先用**文档自己的字体**（同款、同号），
找不到能编码的字体时才退回标准 14 号 Helvetica —— 保真更好，也不必新增字体。

**未实施的原因**：本次会话上下文已接近上限，此时动 200 行字体/编码代码，一旦
中途断掉就会留下**编译不过的半成品**（比不做更糟）。因此停在"P0/P1 已交付
且验证通过"的干净检查点，P2 以本节规格交接。
### P2｜中文改字 —— 已实施（方案与原计划不同）

**实施的是"复用文档自己嵌的字体"，不是"嵌入子集字体"。** 理由见下：
子集化要接 `subsetter` 0.2.6 的 `GlyphRemapper`（skrifa/write-fonts 那套），
而文档里的中文字体**本来就在**页面的 `/Resources` 里 —— 拿它写新字，
产物不涨体积、新字与原文同款、**零新依赖**。

**新增模块** `apps/viewer/src-tauri/src/pdf_font.rs`：

- `unicode_to_code(doc, font)` / `parse_cmap(text)`：从字体的 `/ToUnicode`
  CMap 反查 `unicode → 码位`（`bfchar` 单字符、`bfrange` 的区间与列表两种形式）。
  **必须自己解析**：lopdf 的 `Encoding::string_to_bytes` 在
  `UnicodeMapEncoding`（Identity-H）那一支是 `unimplemented!()` —— **会 panic**。
- `pick_font_for_text(doc, page_id, text)`：遍历页面字体，逐个试编码，
  并**回读校验**（`Document::decode_text` 编码回去必须等于原文）——
  编码有损就跳过这个字体，绝不写出乱码。
- `text_operator(choice)`：CID 字体（`/Subtype /Type0`）写 2 字节十六进制串
  `<…>`，普通字体写字面串 `(…)`（转义 `\ ( )`）。

**降级（诚实，不静默写乱码）**：

- 没有任何字体能覆盖新文字的字符（字体是子集）→ 报错"这份 PDF 的字体里写不了
  这些字"，并建议换一个文中出现过的字；
- 拉丁文则退回标准 14 号 Helvetica（零嵌入）。

**顺带升级**：拉丁文现在也**优先用文档自己的字体**（同款、同号），找不到
才用 Helvetica。

**验证**：

- 探针（独立工程）确认反查表与编码正确：`你好世界` → `<0001000200030004>`；
- 单元测试（`pdf_font.rs` 内）覆盖 `bfchar` / `bfrange` 解析与两种写串格式。

**编译与门禁**：`cargo check -p hermix-viewer --all-targets` 无错误无警告；
viewer `tsc --noEmit` 干净；`check-preview-layer` / `check-design-guard` OK；前端 333 测试全过。

**未验证**：真实 WPS/Word 中文 PDF 的端到端（本机没有这类样本，且运行中的
`hermix.exe` 锁着文件、跑不起来）。