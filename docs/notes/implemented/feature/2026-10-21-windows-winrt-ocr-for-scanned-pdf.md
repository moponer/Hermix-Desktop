# Decision Note: 扫描件 OCR —— 用 Windows 自带的 WinRT OCR，零新依赖

Status: implemented — `pdf_ocr.rs`（WinRT OCR + 隐形文字层）与 `pdf_ocr_page` 命令已落地；端到端验证通过：扫描件原本抽不出任何文字，OCR 产物被 pdf.js 抽出 6 个词项（英文 + 中文），位置正确

## 验证证据（2026-10-21）

用**仓库里真实的 `pdf_ocr.rs`**（`#[path]` 引入，不复制逻辑）跑完整链路：

| 步骤 | 结果 |
|---|---|
| 造「扫描件」PDF（整页只有 JPEG/DCTDecode 图像，**无字体资源**） | 页面 900×300pt，pdf.js `getTextContent()` 抽出 **0** 个文字项（对照基线成立） |
| 调 `ocr_page_to_pdf` | 识别出 **9 个词**，语言 `zh-Hans-CN`；产物 `/Contents` = 2 个流（原流**一字未动** + 新流含 `3 Tr` / `Tj`） |
| pdf.js 读回产物 | 抽出 6 个文字项：`Hello`(59,129) / `OCR 12345`(253,130) / `扫描件`(51,246) / `文`(250,246) / `字`(315,246) / `层`(381,246) —— 中文与数字都在，两行基线不同（位置正确） |

实现与原方案的差异（诚实记录）：命令名是 `pdf_ocr_page`（不是原写的 `pdf_add_ocr_text`）；**只做了「OCR 这一页」**，没有做「OCR 全部页」的批量入口；OCR 用的位图由前端用 pdf.js 以 2 倍缩放渲染成 PNG 传入（Rust 侧不渲染）。

## Decision

给 PDF 编辑器加"**让扫描件可搜索 / 可复制**"：把页面渲染成位图 → 调 Windows 自带的
`Windows.Media.Ocr.OcrEngine` 识别 → 把识别结果作为**隐形文字层**写回 PDF。

### 关键事实（都是实测，不是推断）

| 事实 | 证据 |
|---|---|
| 本机 WinRT OCR 可用，且**装了简体中文语言包** | `OcrEngine::AvailableRecognizerLanguages` → `zh-Hans-CN` + `en-US`；`TryCreateFromLanguage(zh-Hans-CN)` 成功 |
| Rust 能调通（windows **0.58**） | 独立工程实测：读 PNG → `RecognizeAsync` → `TEXT = "Hello OCR 12345"`，每个词带包围盒 |
| **零新依赖** | `windows 0.58` 已在 Cargo.lock（hermix-desktop / hermix-server / hermix-tools 在用），本地 registry 已缓存；只需给 viewer 加 feature |
| 文字层**不需要嵌字体** | 文字是**隐形**的（渲染模式 3），只需要"码位→Unicode"的映射能被抽取器读懂 → 我们自写 `/ToUnicode` CMap 即可，**不嵌任何字体程序**（产物 0 字节增长） |

### 落地做法

1. **前端**：用已有的 pdf.js 把目标页渲染成 PNG（canvas → base64）→ 调 Rust 命令。
2. **Rust**（新增 `pdf_ocr.rs`）：
   - `CoInitializeEx(None, COINIT_MULTITHREADED)`（`Media.Ocr` 是 apartment-sensitive 的）；
   - `StorageFile` → `BitmapDecoder` → `SoftwareBitmap` → `OcrEngine::RecognizeAsync`；
   - 词与包围盒（像素）→ 按渲染缩放换算成 **PDF 用户空间**坐标；
   - 往页内容流**追加**隐形文字层：
     `BT 3 Tr /X <size> Tf x y Td <utf16be-hex> Tj ET`（`3 Tr` = 不可见渲染）。
   - 字体：Type0 / Identity-H + 我们自写的 `/ToUnicode`（复用 `pdf_font.rs` 的 CMap 读写），
     **不嵌字体程序** —— 文字看不见，只需要可抽取。
3. **降级**：机器没装 OCR 语言包 → 明确报错"系统没装中文 OCR 语言包"，
   并给出装法（设置 → 时间和语言 → 语言 → 添加中文 → 语言可选功能 → 基本 OCR）。

### 顺带：手写签名（官方引擎有、我们也有，只是形态不同）

- **我们已经有**：签名板（`SignaturePad.tsx`）→ 拖框放置 → 写成 **AcroForm 的 `/Sig` 域**
  （带外观流）→ Acrobat / 福昕 里显示为"签名"、可双击重签。
- **官方引擎多的是两样**：
  1. **空的签名域**（放个框给别人以后签）→ **已补上**（2026-10-21，与手写签名共用
     `/Widget` + `/FT /Sig` 结构，差别是没有位图、没有 `/V`，另加一个只画边框的外观流；
     见 `2026-10-21-pdf-page-annotation-layer.md`）；
  2. **PKI 数字签名**（可验签、带证书与时间戳）→ 需要证书体系 + CAdES/PAdES 封装 + 新依赖
     → **不做**（与"便携、零安装、文件不出本机"的产品前提不匹配）。

## Alternatives considered

- **Tesseract**（OCRmyPDF 的引擎）：要用户自己装 tesseract.exe + 语言包 → 装机率低，
  不能作为"点一下就有"的功能。WinRT 是系统自带，**零安装**。
- **云端 OCR**（各家 API）：文档要出本机，与产品前提冲突；且离线不可用。
- **把 OCR 放进 markitdown**：它不含 OCR（实测其 `--list-plugins` 无 OCR 能力）。
- **用 Rust 侧渲染 PDF 再 OCR**：Rust 侧没有渲染器（lopdf 不渲染）；我们本来就有
  pdf.js 在前端渲染 → 复用现成的，零新增。
- **OCR 文字层嵌一个中文字体**：隐形文字根本不需要字形，嵌字体只会让产物涨几百 KB。
- **数字签名**：见上（不做）。

## Consequences

- 扫描件从此可搜索、可复制、可选中（这是"改不了"的第一号原因）。
- 产物**不涨体积**（隐形文字 + 一个 ToUnicode CMap，几十 KB 以内）。
- OCR 结果的质量取决于系统引擎（印刷体中文效果一般，手写体差）—— 与 Tesseract 同量级，
  但**零安装**是决定性优势。
- 需要 `windows` crate 启用 WinRT feature（`Media_Ocr` / `Graphics_Imaging` /
  `Foundation_Collections` / `Globalization` / `Storage_Streams` / `Win32_System_Com`）→
  该 crate 会带新 feature 重编一次（一次性的编译时间）。
- **COM apartment**：`Media.Ocr` 必须在 MTA 里调 → Rust 侧在专用线程上初始化 apartment，
  不能直接在 tokio 工作线程上裸调。

## 实施顺序

1. Rust：`pdf_ocr.rs`（WinRT 调用 + 坐标换算 + 隐形文字层）+ `pdf_ocr_page` 命令；
2. 前端：PDF 编辑器加「扫描件 → OCR 这一页」，结果给出词数 / 语言 / 产物路径，并可打开产物；
3. 验证：造一张扫描件（位图）→ OCR → 用 pdf.js 的 `getTextContent()` 断言**中文能被抽出来**。

「OCR 全部页」的批量入口**未做**（当前逐页处理，产物逐次写出）。
