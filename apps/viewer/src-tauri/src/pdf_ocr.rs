//! 扫描件 OCR —— 用 **Windows 自带的 WinRT OCR** 给 PDF 加一层隐形文字。
//!
//! ## 为什么是 WinRT 而不是 Tesseract
//!
//! - **零安装**：Windows 10/11 自带 `Windows.Media.Ocr`，装了中文语言包就能用；
//!   Tesseract 要用户自己装 exe + 语言包 → 装机率低，做不成"点一下就有"。
//! - **零新依赖**：`windows` crate 0.58 已经在依赖树里（hermix-desktop /
//!   hermix-server / hermix-tools 在用），本地 registry 也已缓存。
//!
//! ## 产物长什么样
//!
//! 识别出的文字以**渲染模式 3（不可见）**的文字层写进页面内容流：
//!
//! ```text
//! BT 3 Tr /X <size> Tf <x> <y> Td <utf16be-hex> Tj ET
//! ```
//!
//! 文字看不见，但**能被选中和搜索**（这正是扫描件最需要的能力）。
//!
//! 字体：Type0 / Identity-H（码位 = UTF-16 码元）+ 我们自写的 `/ToUnicode`。
//! **不嵌字体程序** —— 文字本来就是隐形的，只需要抽取器读得懂码位到 Unicode
//! 的映射，所以产物**不涨体积**。
//!
//! ## 线程模型（踩过的坑）
//!
//! COM apartment 是**每线程**的：`Media.Ocr` 必须在 MTA 里调，而 Tauri 的
//! 异步命令跑在 tokio 工作线程上 —— 在那儿初始化 apartment 等于白干。
//! 所以整段 OCR 放进 `spawn_blocking`（固定一个线程），在闭包内部初始化
//! apartment。

use std::path::PathBuf;

use serde::Serialize;
use windows::core::HSTRING;
use windows::Foundation::IAsyncOperation;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapDecoder, SoftwareBitmap};
use windows::Media::Ocr::{OcrEngine, OcrResult};
use windows::Storage::StorageFile;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// OCR 识别出来的一段词（PDF 用户空间坐标）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OcrWord {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub size: f64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OcrPageResult {
    /// 产物路径。
    pub path: String,
    /// 识别出的词数。
    pub words: usize,
    /// 识别出的全文。
    pub text: String,
    /// 实际用的 OCR 语言。
    pub language: String,
}

/// 识别一张位图（PNG 字节）→ 词 + 像素包围盒。
///
/// `scale` = PDF 点 / 图像像素，用来把像素坐标换算成 PDF 用户空间。
fn ocr_image(png: &[u8], scale: f64, page_h: f64) -> Result<(Vec<OcrWord>, String, String), String> {
    // COM apartment：本线程专用。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    // 落临时文件：BitmapDecoder 要一个 StorageFile（WinRT 没有"从内存字节开"的
    // 便捷路径）。文件名带 pid + 计数器，避免并发 OCR 撞车。
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("hermix-ocr-{}-{}.png", std::process::id(), n));
    std::fs::write(&tmp, png).map_err(|e| format!("写临时图片失败：{e}"))?;

    let result = (|| -> Result<(Vec<OcrWord>, String, String), String> {
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(tmp.to_string_lossy().as_ref()))
            .map_err(|e| format!("打开图片失败：{e}"))?
            .get()
            .map_err(|e| format!("取图片失败：{e}"))?;
        let stream = file
            .OpenAsync(windows::Storage::FileAccessMode::Read)
            .map_err(|e| format!("开图片流失败：{e}"))?
            .get()
            .map_err(|e| format!("取图片流失败：{e}"))?;
        let decoder = BitmapDecoder::CreateAsync(&stream)
            .map_err(|e| format!("图片解码器失败：{e}"))?
            .get()
            .map_err(|e| format!("取解码器失败：{e}"))?;
        let bitmap: SoftwareBitmap = decoder
            .GetSoftwareBitmapAsync()
            .map_err(|e| format!("取位图失败：{e}"))?
            .get()
            .map_err(|e| format!("取位图失败：{e}"))?;

        // 引擎：优先中文，其次用户语言。
        let engine = match Language::CreateLanguage(&HSTRING::from("zh-Hans-CN"))
            .ok()
            .and_then(|lang| OcrEngine::TryCreateFromLanguage(&lang).ok())
        {
            Some(e) => e,
            None => OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| {
                format!(
                    "系统没有可用的 OCR 引擎（{}）。中文 OCR 需要系统语言包：\
                     设置 → 时间和语言 → 语言 → 添加「中文」→ 语言可选功能 → 勾「基本 OCR」",
                    e.message()
                )
            })?,
        };
        let language = engine
            .RecognizerLanguage()
            .map(|l| l.LanguageTag().map(|s| s.to_string()).unwrap_or_default())
            .unwrap_or_default();

        let op: IAsyncOperation<OcrResult> = engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| format!("启动识别失败：{e}"))?;
        let result: OcrResult = op.get().map_err(|e| format!("识别失败：{e}"))?;
        let full_text = result.Text().map(|s| s.to_string()).unwrap_or_default();

        // 收集词与包围盒（图像像素，左上原点）。
        let mut words: Vec<OcrWord> = Vec::new();
        let lines = result.Lines().map_err(|e| format!("取文本行失败：{e}"))?;
        let line_count = lines.Size().map_err(|e| format!("取行数失败：{e}"))? as u32;
        for i in 0..line_count {
            let line = lines.GetAt(i).map_err(|e| format!("取行失败：{e}"))?;
            let ws = line.Words().map_err(|e| format!("取词失败：{e}"))?;
            let word_count = ws.Size().map_err(|e| format!("取词数失败：{e}"))? as u32;
            for j in 0..word_count {
                let word = ws.GetAt(j).map_err(|e| format!("取词失败：{e}"))?;
                let r = word
                    .BoundingRect()
                    .map_err(|e| format!("取词位置失败：{e}"))?;
                let text = word.Text().map(|s| s.to_string()).unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }
                // 像素 → PDF 用户空间：x 右为正、y **上**为正。
                words.push(OcrWord {
                    text,
                    x: r.X as f64 * scale,
                    y: page_h - (r.Y + r.Height) as f64 * scale,
                    size: (r.Height as f64 * scale).max(1.0),
                });
            }
        }
        Ok((words, full_text, language))
    })();

    let _ = std::fs::remove_file(&tmp);
    result
}

/// 生成 `/ToUnicode` CMap（码位 → Unicode）。
///
/// 码位就是字符的 UTF-16 码元（Identity-H 语义），所以映射是"自己映射自己"。
fn to_unicode_cmap(pairs: &[(u16, u16)]) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    out.push_str(&format!("{} beginbfchar\n", pairs.len()));
    for (code, uni) in pairs {
        out.push_str(&format!("<{code:04X}> <{uni:04X}>\n"));
    }
    out.push_str("endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend");
    out
}

/// 往页面追加隐形文字层，产出新文件。
pub fn ocr_page_to_pdf(
    src: &PathBuf,
    out: &PathBuf,
    page_no: u32,
    png: &[u8],
) -> Result<OcrPageResult, String> {
    use lopdf::{Dictionary, Document, Object, Stream};

    let mut doc = Document::load(src).map_err(|e| format!("无法打开 PDF：{e}"))?;
    let page_id = doc
        .page_iter()
        .nth(page_no.saturating_sub(1) as usize)
        .ok_or_else(|| format!("页码 {page_no} 超出范围"))?;

    // 页面尺寸（pt）。
    let media = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|d| d.get(b"MediaBox").ok().cloned());
    let (pw, ph) = match media {
        Some(Object::Array(items)) => {
            let num = |i: usize| -> f64 {
                items.get(i).and_then(|o| o.as_float().ok()).unwrap_or(0.0) as f64
            };
            let (x0, y0, x1, y1) = (num(0), num(1), num(2), num(3));
            ((x1 - x0).abs().max(1.0), (y1 - y0).abs().max(1.0))
        }
        _ => (595.0, 842.0),
    };

    // 图像尺寸 → 缩放：PDF 点 / 图像像素。
    let img_w = png_image_width(png).unwrap_or(0);
    let scale = if img_w > 0 { pw / img_w as f64 } else { 1.0 };

    let (words, full_text, language) = ocr_image(png, scale, ph)?;
    if words.is_empty() {
        return Err("这一页没有识别出文字（可能不是扫描件，或图像太模糊）".into());
    }

    // 字体：Type0 / Identity-H + 自写 ToUnicode（不嵌字体程序）。
    let mut pairs: Vec<(u16, u16)> = Vec::new();
    for w in &words {
        for unit in w.text.encode_utf16() {
            if !pairs.iter().any(|(c, _)| *c == unit) {
                pairs.push((unit, unit));
            }
        }
    }
    let cmap = to_unicode_cmap(&pairs);
    let cmap_id = doc.add_object(Stream::new(Dictionary::new(), cmap.into_bytes()));

    let mut desc = Dictionary::new();
    desc.set(b"Type", Object::Name(b"Font".to_vec()));
    desc.set(b"Subtype", Object::Name(b"CIDFontType0".to_vec()));
    desc.set(b"BaseFont", Object::Name(b"HermixOCR".to_vec()));
    let mut csi = Dictionary::new();
    csi.set(b"Registry", Object::string_literal("Adobe"));
    csi.set(b"Ordering", Object::string_literal("Identity"));
    csi.set(b"Supplement", Object::Integer(0));
    desc.set(b"CIDSystemInfo", Object::Dictionary(csi));
    desc.set(b"DW", Object::Integer(1000));
    let desc_id = doc.add_object(Object::Dictionary(desc));

    let mut type0 = Dictionary::new();
    type0.set(b"Type", Object::Name(b"Font".to_vec()));
    type0.set(b"Subtype", Object::Name(b"Type0".to_vec()));
    type0.set(b"BaseFont", Object::Name(b"HermixOCR".to_vec()));
    type0.set(b"Encoding", Object::Name(b"Identity-H".to_vec()));
    type0.set(b"DescendantFonts", Object::Array(vec![Object::Reference(desc_id)]));
    type0.set(b"ToUnicode", Object::Reference(cmap_id));
    let font_id = doc.add_object(Object::Dictionary(type0));

    // 页面资源里挂上这个字体。
    let resources_id = match doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|d| d.get(b"Resources").ok().cloned())
    {
        Some(Object::Reference(id)) => id,
        _ => {
            let id = doc.add_object(Object::Dictionary(Dictionary::new()));
            if let Ok(d) = doc.get_dictionary_mut(page_id) {
                d.set(b"Resources", Object::Reference(id));
            }
            id
        }
    };
    let fonts_id = match doc
        .get_dictionary(resources_id)
        .ok()
        .and_then(|d| d.get(b"Font").ok().cloned())
    {
        Some(Object::Reference(id)) => id,
        _ => {
            let id = doc.add_object(Object::Dictionary(Dictionary::new()));
            if let Ok(d) = doc.get_dictionary_mut(resources_id) {
                d.set(b"Font", Object::Reference(id));
            }
            id
        }
    };
    if let Ok(d) = doc.get_dictionary_mut(fonts_id) {
        d.set(b"HermixOCR", Object::Reference(font_id));
    }

    // 隐形文字层：3 Tr = 不可见渲染。
    let mut ops: Vec<u8> = Vec::new();
    ops.extend_from_slice(b"q\n3 Tr\n");
    for w in &words {
        let mut hex = String::new();
        for unit in w.text.encode_utf16() {
            hex.push_str(&format!("{unit:04X}"));
        }
        ops.extend_from_slice(
            format!(
                "BT\n/HermixOCR {size} Tf\n{x} {y} Td\n<{hex}> Tj\nET\n",
                size = w.size,
                x = w.x,
                y = w.y,
                hex = hex
            )
            .as_bytes(),
        );
    }
    ops.extend_from_slice(b"Q\n");

    // 追加为新的内容流（原内容一个字节不动）。
    let new_stream_id = doc.add_object(Stream::new(Dictionary::new(), ops));
    let mut contents: Vec<Object> = doc
        .get_page_contents(page_id)
        .into_iter()
        .map(Object::Reference)
        .collect();
    contents.push(Object::Reference(new_stream_id));
    if let Ok(d) = doc.get_dictionary_mut(page_id) {
        d.set(b"Contents", Object::Array(contents));
    }

    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败：{e}"))?;
    std::fs::write(out, &buf).map_err(|e| format!("写产物失败：{e}"))?;

    Ok(OcrPageResult {
        path: out.to_string_lossy().to_string(),
        words: words.len(),
        text: full_text,
        language,
    })
}

/// 从 PNG 头里读宽度（IHDR）。
fn png_image_width(png: &[u8]) -> Option<u32> {
    if png.len() < 24 || &png[1..4] != b"PNG" {
        return None;
    }
    Some(u32::from_be_bytes([png[16], png[17], png[18], png[19]]))
}
