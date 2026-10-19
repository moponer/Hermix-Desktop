//! PDF / 文本编辑内核（F6）。
//!
//! **原文件保护**：任何写盘都过 `safe_output_path`——产物与源文件相同
//! （Windows 上大小写不敏感，`..`、相对路径都算同一文件）一律拒绝。
//! 默认产物名 `<原名>.edited.<扩展名>`，前端传什么都不覆盖原文件。
//!
//! **依赖**：只用已有的 lopdf / image / serde，不新增 crate。GBK 编码走
//! Win32 `WideCharToMultiByte`（CP 936），本文件自带 extern 声明，因此
//! **不需要**改 Cargo.toml；若想换成类型化 API，则给 windows-sys 加
//! `Win32_Globalization`（windows-sys 0.52 里 `Win32_Globalization = ["Win32"]`，
//! 当前未启用）。
//!
//! **能力边界**：lopdf 0.34 只支持 V1/V2 加密（RC4），AES 加密件返回可读
//! 错误而不是静默失败；文本层替换只认 `Tj` 算子且要求整串精确相等。

use std::path::{Path, PathBuf};

use image::DynamicImage;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use serde::{Deserialize, Serialize};

// ── op 模型 ──────────────────────────────────────────────────────────

/// 一个 PDF 页级动作。序列化成 JSON 传给前端（`op` 标签 + camelCase）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum PdfOp {
    /// 删除指定页（页码从 1 开始）
    Delete { pages: Vec<u32> },
    /// 只保留指定页（其余全删）——「提取页」「拆分」。按传入顺序排列。
    Keep { pages: Vec<u32> },
    /// 重排：给完整的页序（如 [3,1,2,5,4]，必须是全部页的一个排列）
    Reorder { order: Vec<u32> },
    /// 旋转：页码 → 角度（90/180/270）
    Rotate { pages: Vec<u32>, degrees: i64 },
    /// 插入空白页：在第几页之前插（1 = 最前，可取 页数+1 = 末尾）
    InsertBlank { at: u32, width: f64, height: f64 },
}

/// 图片来源（base64 载荷 + 声明的像素尺寸）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSource {
    pub name: String,
    pub data_base64: String,
    pub width: u32,
    pub height: u32,
}

/// 落盘结果。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    /// 实际写盘路径
    pub path: String,
    /// 写盘字节数
    pub bytes: usize,
    /// 实际使用的编码（仅文本另存有值，其余为 null）
    pub encoding_used: Option<String>,
}

// ── 产物路径：原文件保护 ─────────────────────────────────────────────

/// 默认产物名：`<原名>.edited.<扩展名>`（同目录）。
pub fn default_output_path(src: &Path) -> PathBuf {
    let stem = src.file_stem().map(|s| s.to_string_lossy().to_string());
    let ext = src.extension().map(|s| s.to_string_lossy().to_string());
    let dir = src.parent().unwrap_or_else(|| Path::new(""));
    let name = match (stem, ext) {
        (Some(s), Some(e)) => format!("{s}.edited.{e}"),
        (Some(s), None) => format!("{s}.edited"),
        _ => "edited".to_string(),
    };
    dir.join(name)
}

/// 同一个文件吗？Windows 上路径大小写不敏感，`..` / 相对路径都能绕，
/// 所以先比规范化绝对路径，再比 canonical（能拿到真实同一文件的判据）。
fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(p)
        };
        abs.to_string_lossy().to_lowercase()
    };
    if norm(a) == norm(b) {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 产物路径守卫：与源文件相同一律拒绝（哪怕前端传了同一个路径）。
pub fn safe_output_path(src: &Path, requested: Option<&str>) -> Result<PathBuf, String> {
    let candidate = match requested.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => default_output_path(src),
    };
    if same_file(&candidate, src) {
        return Err("产物路径不能与原文件相同：原文件只读，请换一个文件名".into());
    }
    Ok(candidate)
}

/// 落盘公共出口：守卫 → 建目录 → 写盘 → 回报路径与字节数。
fn save_output(
    src: &Path,
    requested_out: Option<&str>,
    bytes: &[u8],
) -> Result<SaveResult, String> {
    let out = safe_output_path(src, requested_out)?;
    save_bytes(&out, bytes, None)
}

/// 写盘并回报结果（没有源文件可比的场合：图片转 PDF / 文本另存）。
fn save_bytes(
    out: &Path,
    bytes: &[u8],
    encoding_used: Option<String>,
) -> Result<SaveResult, String> {
    if out.as_os_str().is_empty() {
        return Err("输出路径为空".into());
    }
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| format!("无法创建输出目录: {e}"))?;
        }
    }
    std::fs::write(out, bytes).map_err(|e| format!("写入失败: {e}"))?;
    Ok(SaveResult {
        path: out.to_string_lossy().to_string(),
        bytes: bytes.len(),
        encoding_used,
    })
}

// ── PDF 页树 ─────────────────────────────────────────────────────────

/// 页树里的一页：id + 从祖先继承来的属性。
///
/// 中间层 Pages 节点可以携带子页共享的 MediaBox / Resources；页树扁平化
/// 后中间节点会被剪掉，这些继承属性必须显式落到页上，否则读者按默认
/// 纸张尺寸渲染。
#[derive(Clone)]
struct LeafPage {
    id: ObjectId,
    media_box: Option<Object>,
    resources: Option<Object>,
}

/// 解引用：Kids 可能是间接引用（最多跟 8 层，防环）。
fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    let mut cur = obj;
    for _ in 0..8 {
        let next = match cur {
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(o) => o,
                Err(_) => break,
            },
            _ => break,
        };
        cur = next;
    }
    cur
}

/// 递归收集页树叶子页（按页序）。
fn collect_pages(
    doc: &Document,
    node: ObjectId,
    inherited_media: Option<&Object>,
    inherited_res: Option<&Object>,
    out: &mut Vec<LeafPage>,
) -> Result<(), String> {
    let dict = doc
        .get_dictionary(node)
        .map_err(|e| format!("PDF 结构损坏: {e}"))?;
    let media = dict.get(b"MediaBox").ok().or(inherited_media);
    let res = dict.get(b"Resources").ok().or(inherited_res);
    // 有 Kids 的是中间层 Pages 节点，没有的就是叶子页
    let kids = dict.get(b"Kids").ok().map(|k| resolve(doc, k));
    match kids {
        Some(kids) => {
            let arr = kids.as_array().map_err(|_| "PDF 页树 Kids 异常".to_string())?;
            for kid in arr {
                let id = kid.as_reference().map_err(|_| "PDF 页树引用异常".to_string())?;
                collect_pages(doc, id, media, res, out)?;
            }
        }
        None => out.push(LeafPage {
            id: node,
            media_box: media.cloned(),
            resources: res.cloned(),
        }),
    }
    Ok(())
}

/// 扁平页表 + 页树根节点。
struct PdfPages {
    root: ObjectId,
    leaves: Vec<LeafPage>,
}

fn read_pages(doc: &Document) -> Result<PdfPages, String> {
    let root = doc
        .catalog()
        .map_err(|e| format!("PDF 结构损坏: {e}"))?
        .get(b"Pages")
        .map_err(|_| "PDF 缺少页树根节点".to_string())?
        .as_reference()
        .map_err(|_| "PDF 页树根节点异常".to_string())?;
    let mut leaves = Vec::new();
    collect_pages(doc, root, None, None, &mut leaves)?;
    if leaves.is_empty() {
        return Err("PDF 没有页面".into());
    }
    Ok(PdfPages { root, leaves })
}

/// 写回页树：Kids 顺序 = 新页序，**Count 必须同步**（否则读者报文件损坏）。
///
/// 同时把每页的 Parent 指回根节点、缺省属性补上继承值——扁平化后中间
/// 节点被剪掉，页上的 Parent 若还指着被剪掉的节点就成了悬空引用。
fn write_page_tree(
    doc: &mut Document,
    root: ObjectId,
    pages: &[LeafPage],
    order: &[usize],
) -> Result<(), String> {
    for &i in order {
        let page = &pages[i];
        let (id, media, res) = (page.id, page.media_box.clone(), page.resources.clone());
        let dict = doc
            .get_dictionary_mut(id)
            .map_err(|e| format!("PDF 结构损坏: {e}"))?;
        dict.set("Parent", root);
        if !dict.has(b"MediaBox") {
            if let Some(mb) = media {
                dict.set("MediaBox", mb);
            }
        }
        if !dict.has(b"Resources") {
            if let Some(r) = res {
                dict.set("Resources", r);
            }
        }
    }
    let kids: Vec<Object> = order.iter().map(|&i| Object::Reference(pages[i].id)).collect();
    let root_dict = doc
        .get_dictionary_mut(root)
        .map_err(|e| format!("PDF 结构损坏: {e}"))?;
    root_dict.set("Kids", Object::Array(kids));
    root_dict.set("Count", Object::Integer(order.len() as i64));
    Ok(())
}

/// 页码校验：页码从 1 开始、落在 1..=total、不重复。
fn validate_page_list(pages: &[u32], total: u32) -> Result<Vec<u32>, String> {
    if pages.is_empty() {
        return Err("页码列表为空".into());
    }
    let mut seen: Vec<u32> = Vec::with_capacity(pages.len());
    for &n in pages {
        if n == 0 {
            return Err("页码从 1 开始".into());
        }
        if n > total {
            return Err(format!("页码 {n} 超出范围 1..={total}"));
        }
        if seen.contains(&n) {
            return Err(format!("页码 {n} 重复"));
        }
        seen.push(n);
    }
    Ok(pages.to_vec())
}

/// 归一化旋转角：只接受 90/180/270（-90 视作 270）。
fn normalize_rotation(degrees: i64) -> Option<i64> {
    match ((degrees % 360) + 360) % 360 {
        d @ (90 | 180 | 270) => Some(d),
        _ => None,
    }
}

/// 新建一个页对象（空内容流 + 指定纸张尺寸）。
fn new_page(doc: &mut Document, parent: ObjectId, width: f32, height: f32) -> ObjectId {
    let content_id = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"Page".to_vec()));
    d.set("Parent", parent);
    d.set(
        "MediaBox",
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    d.set("Resources", Object::Dictionary(Dictionary::new()));
    d.set("Contents", content_id);
    doc.add_object(d)
}

/// 执行一个页级 op（就地改文档）。
pub fn apply_pdf_op(doc: &mut Document, op: &PdfOp) -> Result<(), String> {
    let p = read_pages(doc)?;
    let total = p.leaves.len() as u32;
    match op {
        PdfOp::Delete { pages } => {
            let nums = validate_page_list(pages, total)?;
            let keep: Vec<usize> = (0..total as usize)
                .filter(|i| !nums.contains(&(*i as u32 + 1)))
                .collect();
            if keep.is_empty() {
                return Err("不能删光所有页面".into());
            }
            write_page_tree(doc, p.root, &p.leaves, &keep)
        }
        PdfOp::Keep { pages } => {
            let nums = validate_page_list(pages, total)?;
            let order: Vec<usize> = nums.iter().map(|n| (*n - 1) as usize).collect();
            write_page_tree(doc, p.root, &p.leaves, &order)
        }
        PdfOp::Reorder { order } => {
            if order.len() as u32 != total {
                return Err(format!("重排必须给出全部 {total} 页"));
            }
            let mut seen = vec![false; total as usize];
            let mut idx = Vec::with_capacity(order.len());
            for &n in order {
                if n == 0 || n > total {
                    return Err(format!("页码 {n} 超出范围 1..={total}"));
                }
                if seen[(n - 1) as usize] {
                    return Err(format!("页码 {n} 重复"));
                }
                seen[(n - 1) as usize] = true;
                idx.push((n - 1) as usize);
            }
            write_page_tree(doc, p.root, &p.leaves, &idx)
        }
        PdfOp::Rotate { pages, degrees } => {
            let nums = validate_page_list(pages, total)?;
            let deg = normalize_rotation(*degrees).ok_or("旋转角度必须是 90/180/270")?;
            for n in nums {
                let id = p.leaves[n as usize - 1].id;
                doc.get_dictionary_mut(id)
                    .map_err(|e| format!("PDF 结构损坏: {e}"))?
                    .set("Rotate", Object::Integer(deg));
            }
            Ok(())
        }
        PdfOp::InsertBlank {
            at,
            width,
            height,
        } => {
            if !width.is_finite() || !height.is_finite() || *width <= 0.0 || *height <= 0.0 {
                return Err("空白页尺寸必须为正数".into());
            }
            if *width > 20_000.0 || *height > 20_000.0 {
                return Err("空白页尺寸过大".into());
            }
            if *at < 1 || *at > total + 1 {
                return Err(format!("插入位置 {at} 超出范围 1..={}", total + 1));
            }
            let page_id = new_page(doc, p.root, *width as f32, *height as f32);
            let mut leaves = p.leaves.clone();
            leaves.insert(
                *at as usize - 1,
                LeafPage {
                    id: page_id,
                    media_box: None,
                    resources: None,
                },
            );
            let order: Vec<usize> = (0..leaves.len()).collect();
            write_page_tree(doc, p.root, &leaves, &order)
        }
    }
}

// ── 文本编码 ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Gbk,
}

impl TextEncoding {
    fn name(self) -> &'static str {
        match self {
            TextEncoding::Utf8 => "utf-8",
            TextEncoding::Utf16Le => "utf-16le",
            TextEncoding::Utf16Be => "utf-16be",
            TextEncoding::Gbk => "gbk",
        }
    }
}

/// 编码名 → 编码。未知编码回退 UTF-8（第二个返回值标记「是否回退」）。
fn resolve_encoding(name: &str) -> (TextEncoding, bool) {
    let key = name.trim().to_ascii_lowercase().replace('_', "-");
    match key.as_str() {
        "" | "utf-8" | "utf8" => (TextEncoding::Utf8, false),
        "utf-16le" | "utf-16-le" | "utf16le" | "ucs-2" | "ucs2" => (TextEncoding::Utf16Le, false),
        "utf-16be" | "utf-16-be" | "utf16be" => (TextEncoding::Utf16Be, false),
        "gbk" | "gb2312" | "cp936" | "ms936" | "gb18030" => (TextEncoding::Gbk, false),
        _ => (TextEncoding::Utf8, true),
    }
}

/// 文本 → 字节。utf-16 带 BOM（记事本据此判编码）；gbk 走 Win32。
fn encode_text(text: &str, enc: TextEncoding) -> Result<Vec<u8>, String> {
    match enc {
        TextEncoding::Utf8 => Ok(text.as_bytes().to_vec()),
        TextEncoding::Utf16Le => {
            let mut out = vec![0xFF, 0xFE];
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            Ok(out)
        }
        TextEncoding::Utf16Be => {
            let mut out = vec![0xFE, 0xFF];
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_be_bytes());
            }
            Ok(out)
        }
        TextEncoding::Gbk => encode_gbk(text),
    }
}

/// GBK 编码：Win32 `WideCharToMultiByte`（CP 936）。
#[cfg(windows)]
fn encode_gbk(text: &str) -> Result<Vec<u8>, String> {
    // CP_GBK = 936：windows-sys 0.52 的 Globalization 模块没有这个常量
    // （只有 CP_UTF8 = 65001），故用字面量。
    const CP_GBK: u32 = 936;

    #[link(name = "kernel32")]
    extern "system" {
        fn WideCharToMultiByte(
            codepage: u32,
            dwflags: u32,
            lpwidechar: *const u16,
            cchwidechar: i32,
            lpmultibyte: *mut u8,
            cbmultibyte: i32,
            lpdefaultchar: *const u8,
            lpuseddefaultchar: *mut i32,
        ) -> i32;
    }

    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() {
        return Ok(Vec::new());
    }
    // GBK 最多 2 字节/汉字，留 4 倍余量防截断
    let mut buf = vec![0u8; wide.len() * 4];
    // 替身字符 + 标记位：GBK 表外的字符会被悄悄替换成 '?'，
    // 靠 lpUsedDefaultChar 才能发现——宁可报错，也不产出乱码文件。
    let default_char: u8 = b'?';
    let mut used_default: i32 = 0;
    let n = unsafe {
        WideCharToMultiByte(
            CP_GBK,
            0,
            wide.as_ptr(),
            wide.len() as i32,
            buf.as_mut_ptr(),
            buf.len() as i32,
            &default_char,
            &mut used_default,
        )
    };
    if n == 0 {
        return Err("GBK 编码失败：文本含 GBK 无法表示的字符".into());
    }
    if used_default != 0 {
        return Err("GBK 编码失败：文本含 GBK 无法表示的字符".into());
    }
    buf.truncate(n as usize);
    Ok(buf)
}

#[cfg(not(windows))]
fn encode_gbk(_text: &str) -> Result<Vec<u8>, String> {
    Err("GBK 编码仅支持 Windows（项目边界）".into())
}

// ── 图片 → PDF ───────────────────────────────────────────────────────

/// 解码上限：单边 20000px、分配上限 512MB。防「解压炸弹」把内存打爆。
const MAX_SIDE: u32 = 20_000;
const MAX_ALLOC: u64 = 512 * 1024 * 1024;
/// A4 尺寸（PDF point）。图片超出 A4 时等比缩小。
const A4_W: f32 = 595.0;
const A4_H: f32 = 842.0;

fn decode_image(bytes: &[u8], name: &str) -> Result<DynamicImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("无法识别图片格式 {name}: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|e| format!("解码图片失败 {name}: {e}"))
}

/// 一张图 → 一页 PDF。
///
/// JPEG 字节以 DCTDecode 存成 XObject。**统一转成 RGB8 再编码为 JPEG**：
/// PDF 的 DCTDecode 就是 JPEG，原样嵌入 PNG 会坏文件，而灰度/CMYK 的 JPEG
/// 又对不上 DeviceRGB 色彩空间。
fn build_image_page(
    doc: &mut Document,
    pages_id: ObjectId,
    source: &ImageSource,
) -> Result<ObjectId, String> {
    let raw = crate::commands::base64_decode(&source.data_base64)
        .ok_or("非法 base64 数据")?;
    let img = decode_image(&raw, &source.name)?.to_rgb8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("图片尺寸非法: {}", source.name));
    }
    let mut jpeg: Vec<u8> = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&img)
        .map_err(|e| format!("JPEG 编码失败 {}: {e}", source.name))?;

    // 页面尺寸 = 图片比例（1px 视作 1pt），超出 A4 则等比缩小
    let (mut pw, mut ph) = (w as f32, h as f32);
    let scale = (A4_W / pw).min(A4_H / ph);
    if scale < 1.0 {
        pw *= scale;
        ph *= scale;
    }
    let page_id = new_page(doc, pages_id, pw, ph);

    let mut sd = Dictionary::new();
    sd.set("Type", Object::Name(b"XObject".to_vec()));
    sd.set("Subtype", Object::Name(b"Image".to_vec()));
    sd.set("Width", Object::Integer(w as i64));
    sd.set("Height", Object::Integer(h as i64));
    sd.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    sd.set("BitsPerComponent", Object::Integer(8));
    sd.set("Filter", Object::Name(b"DCTDecode".to_vec()));
    doc.insert_image(page_id, Stream::new(sd, jpeg), (0.0, 0.0), (pw, ph))
        .map_err(|e| format!("嵌入图片失败 {}: {e}", source.name))?;
    Ok(page_id)
}

// ── Tauri 命令 ───────────────────────────────────────────────────────

/// PDF 基本信息：页数 + 是否加密。前端要开页码编辑器就得先知道几页。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PdfInfo {
    pub path: String,
    /// 页数（加密时为 0）
    pub pages: u32,
    pub encrypted: bool,
    /// 文件字节数
    pub bytes: u64,
}

#[tauri::command(async)]
pub fn pdf_info(src_path: String) -> Result<PdfInfo, String> {
    let src = PathBuf::from(&src_path);
    if !src.is_file() {
        return Err(format!("不是文件: {src_path}"));
    }
    let bytes = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
    let doc = Document::load(&src).map_err(|e| format!("无法打开 PDF: {e}"))?;
    if doc.is_encrypted() {
        return Ok(PdfInfo {
            path: src_path,
            pages: 0,
            encrypted: true,
            bytes,
        });
    }
    let pages = read_pages(&doc)?.leaves.len() as u32;
    Ok(PdfInfo {
        path: src_path,
        pages,
        encrypted: false,
        bytes,
    })
}

/// PDF 页级操作：删页 / 提取页 / 重排 / 旋转 / 插空白页。
///
/// `ops` 是一串**按顺序执行**的动作，最后只落一个文件——前端把用户的
/// 一串操作攒起来一次执行，而不是每点一下就产出一个中间文件。
/// 页号语义是「前一个 op 执行后的页序」。
///
/// 产物路径过守卫：与源文件相同一律拒绝，默认写 `<原名>.edited.pdf`。
#[tauri::command(async)]
pub fn pdf_ops(
    src_path: String,
    out_path: Option<String>,
    ops: Vec<PdfOp>,
) -> Result<SaveResult, String> {
    let src = PathBuf::from(&src_path);
    if !src.is_file() {
        return Err(format!("不是文件: {src_path}"));
    }
    if ops.is_empty() {
        return Err("没有要执行的操作".into());
    }
    // 早失败：路径不合法就别白读一遍 PDF
    safe_output_path(&src, out_path.as_deref())?;
    let mut doc = Document::load(&src).map_err(|e| format!("无法打开 PDF: {e}"))?;
    if doc.is_encrypted() {
        return Err("该 PDF 使用 AES 加密，当前不支持编辑".into());
    }
    for op in &ops {
        apply_pdf_op(&mut doc, op)?;
    }
    // 剪掉被删页残留的对象，再压紧对象号（写回后不留空洞）
    doc.prune_objects();
    doc.renumber_objects();
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败: {e}"))?;
    save_output(&src, out_path.as_deref(), &buf)
}

/// PDF 文本层替换。
///
/// **能力有限**（lopdf 0.34 的实现边界，命令名如实体现）：只处理 `Tj`
/// 算子，要求整串精确相等（不是子串替换），且字体编码解不出来时会跳过
/// 该处并告警。扫描件（纯图片）没有文本层，本命令无效。
#[tauri::command(async)]
pub fn pdf_replace_text(
    src_path: String,
    out_path: Option<String>,
    page: u32,
    from: String,
    to: String,
) -> Result<SaveResult, String> {
    let src = PathBuf::from(&src_path);
    if !src.is_file() {
        return Err(format!("不是文件: {src_path}"));
    }
    let out = safe_output_path(&src, out_path.as_deref())?;
    let mut doc = Document::load(&src).map_err(|e| format!("无法打开 PDF: {e}"))?;
    if doc.is_encrypted() {
        return Err("该 PDF 使用 AES 加密，当前不支持编辑".into());
    }
    doc.replace_text(page, &from, &to)
        .map_err(|e| format!("文本替换失败: {e}"))?;
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败: {e}"))?;
    save_bytes(&out, &buf, None)
}

/// 图片转 PDF：每张图一页。
#[tauri::command(async)]
pub fn images_to_pdf(sources: Vec<ImageSource>, out_path: String) -> Result<SaveResult, String> {
    if sources.is_empty() {
        return Err("没有可转换的图片".into());
    }
    let out = PathBuf::from(&out_path);
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut page_ids = Vec::with_capacity(sources.len());
    for src in &sources {
        page_ids.push(build_image_page(&mut doc, pages_id, src)?);
    }
    let mut pd = Dictionary::new();
    pd.set("Type", Object::Name(b"Pages".to_vec()));
    pd.set(
        "Kids",
        Object::Array(
            page_ids
                .iter()
                .map(|id| Object::Reference(*id))
                .collect::<Vec<Object>>(),
        ),
    );
    pd.set("Count", Object::Integer(page_ids.len() as i64));
    doc.set_object(pages_id, Object::Dictionary(pd));
    let mut cat = Dictionary::new();
    cat.set("Type", Object::Name(b"Catalog".to_vec()));
    cat.set("Pages", pages_id);
    let cat_id = doc.add_object(Object::Dictionary(cat));
    doc.trailer.set("Root", cat_id);
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败: {e}"))?;
    save_bytes(&out, &buf, None)
}

/// 文本另存（编码保真）。未知编码回退 UTF-8，实际用的编码在返回值里。
///
/// 带 `src_path` 是为了**原文件保护**：用户在另存对话框里选中源文件
/// 本身时必须被拒绝（没有源路径可比就等于没有守卫）。
#[tauri::command(async)]
pub fn write_text(
    src_path: String,
    out_path: Option<String>,
    text: String,
    encoding: String,
) -> Result<SaveResult, String> {
    let src = PathBuf::from(&src_path);
    let (enc, _fell_back) = resolve_encoding(&encoding);
    let bytes = encode_text(&text, enc)?;
    let out = safe_output_path(&src, out_path.as_deref())?;
    save_bytes(&out, &bytes, Some(enc.name().to_string()))
}

// ── 单测 ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_guard_rejects_source_keeps_other() {
        let src = PathBuf::from(r"C:\pics\cat.pdf");
        assert!(safe_output_path(&src, Some(r"C:\pics\cat.pdf"))
            .unwrap_err()
            .contains("不能与原文件相同"));
        // 大小写不同的同一路径也要拒（Windows 不敏感）
        assert!(safe_output_path(&src, Some(r"C:\Pics\CAT.PDF"))
            .unwrap_err()
            .contains("不能与原文件相同"));
        let out = safe_output_path(&src, Some(r"C:\pics\cat.edited.pdf")).unwrap();
        assert_eq!(out.file_name().unwrap().to_string_lossy(), "cat.edited.pdf");
    }

    #[test]
    fn default_output_name_inserts_edited() {
        let p = default_output_path(Path::new(r"C:\pics\cat.pdf"));
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "cat.edited.pdf");
    }

    #[test]
    fn pdf_op_serde_round_trip() {
        let json = serde_json::to_string(&PdfOp::Rotate {
            pages: vec![1, 2],
            degrees: 90,
        })
        .unwrap();
        assert_eq!(json, r#"{"op":"rotate","pages":[1,2],"degrees":90}"#);
        let json = serde_json::to_string(&PdfOp::InsertBlank {
            at: 2,
            width: 595.0,
            height: 842.0,
        })
        .unwrap();
        assert!(json.starts_with(r#"{"op":"insertBlank""#), "{json}");
        assert!(json.contains(r#""width":595.0"#), "{json}");
        assert!(serde_json::from_str::<PdfOp>(r#"{"op":"nope"}"#).is_err());
        match serde_json::from_str::<PdfOp>(r#"{"op":"delete","pages":[3]}"#).unwrap() {
            PdfOp::Delete { pages } => assert_eq!(pages, vec![3]),
            _ => panic!("反序列化成了别的 op"),
        }
    }

    #[test]
    fn validate_page_list_rejects_bad_input() {
        assert!(validate_page_list(&[0], 3).unwrap_err().contains("从 1 开始"));
        assert!(validate_page_list(&[4], 3).unwrap_err().contains("超出范围"));
        assert!(validate_page_list(&[2, 2], 3).unwrap_err().contains("重复"));
        assert!(validate_page_list(&[], 3).is_err());
        assert_eq!(validate_page_list(&[1, 3], 3).unwrap(), vec![1, 3]);
    }

    #[test]
    fn normalize_rotation_accepts_only_quarter_turns() {
        assert_eq!(normalize_rotation(90), Some(90));
        assert_eq!(normalize_rotation(-90), Some(270));
        assert_eq!(normalize_rotation(450), Some(90));
        assert_eq!(normalize_rotation(0), None);
        assert_eq!(normalize_rotation(45), None);
    }

    #[test]
    fn encoding_selection_and_bytes() {
        assert_eq!(resolve_encoding("utf-8").0, TextEncoding::Utf8);
        assert_eq!(resolve_encoding("UTF-8").0, TextEncoding::Utf8);
        assert_eq!(resolve_encoding("utf-16le").0, TextEncoding::Utf16Le);
        assert_eq!(resolve_encoding("utf-16be").0, TextEncoding::Utf16Be);
        assert_eq!(resolve_encoding("GBK").0, TextEncoding::Gbk);
        let (enc, fell_back) = resolve_encoding("latin1");
        assert_eq!(enc, TextEncoding::Utf8, "未知编码回退 UTF-8");
        assert!(fell_back);
        assert_eq!(
            encode_text("A", TextEncoding::Utf16Le).unwrap(),
            vec![0xFF, 0xFE, 0x41, 0x00]
        );
        assert_eq!(
            encode_text("A", TextEncoding::Utf16Be).unwrap(),
            vec![0xFE, 0xFF, 0x00, 0x41]
        );
        assert!(encode_text("", TextEncoding::Utf8).unwrap().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn gbk_encodes_chinese_and_rejects_unrepresentable() {
        // 「中文」的 GBK 码 = D6D0 CEC4
        assert_eq!(
            encode_text("中文", TextEncoding::Gbk).unwrap(),
            vec![0xD6, 0xD0, 0xCE, 0xC4]
        );
        // emoji 不在 GBK 里 → 报错而不是悄悄写成 '?'
        assert!(encode_text("😀", TextEncoding::Gbk).is_err());
    }

    #[test]
    fn base64_decode_round_trips() {
        let raw = b"hello";
        let encoded = crate::commands::base64_encode(raw);
        assert_eq!(crate::commands::base64_decode(&encoded).unwrap(), raw);
        assert!(crate::commands::base64_decode("!!!!").is_none());
    }

    // ── PDF 端到端：用 lopdf 现场造文档（不往仓库塞二进制 fixture）──
    // 真机验收（真实 PDF 的渲染效果）由主会话做；这里只锁住页集合与页序。

    /// 造一个 n 页 PDF，每页字典带 "HermixMark" = 页序，便于断言重排结果。
    fn build_sample_pdf(path: &Path, n: u32) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for i in 1..=n {
            let mut d = Dictionary::new();
            d.set("Type", Object::Name(b"Page".to_vec()));
            d.set("Parent", pages_id);
            d.set(
                "MediaBox",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(595),
                    Object::Integer(842),
                ]),
            );
            d.set("Resources", Object::Dictionary(Dictionary::new()));
            d.set("HermixMark", Object::Integer(i as i64));
            kids.push(Object::Reference(doc.add_object(d)));
        }
        let mut pd = Dictionary::new();
        pd.set("Type", Object::Name(b"Pages".to_vec()));
        pd.set("Kids", Object::Array(kids));
        pd.set("Count", Object::Integer(n as i64));
        doc.set_object(pages_id, Object::Dictionary(pd));
        let mut cat = Dictionary::new();
        cat.set("Type", Object::Name(b"Catalog".to_vec()));
        cat.set("Pages", pages_id);
        let cat_id = doc.add_object(Object::Dictionary(cat));
        doc.trailer.set("Root", cat_id);
        doc.save(path).unwrap();
    }

    /// 读回每页的 "HermixMark"（空白页没有该键 → 0）。
    fn marks(path: &Path) -> Vec<i64> {
        let doc = Document::load(path).unwrap();
        doc.page_iter()
            .map(|id| {
                doc.get_dictionary(id)
                    .ok()
                    .and_then(|d| d.get(b"HermixMark").ok())
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(0)
            })
            .collect()
    }

    /// 跑一个 op 并写回，返回产物路径。
    fn run_op(src: &Path, out: &Path, op: PdfOp) {
        let mut doc = Document::load(src).unwrap();
        apply_pdf_op(&mut doc, &op).unwrap();
        doc.prune_objects();
        doc.renumber_objects();
        doc.save(out).unwrap();
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hermix-viewer-pdf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pdf_page_ops_change_page_set_and_order() {
        let dir = tmp_dir("ops");
        let src = dir.join("a.pdf");
        build_sample_pdf(&src, 4);

        run_op(
            &src,
            &dir.join("del.pdf"),
            PdfOp::Delete { pages: vec![2] },
        );
        assert_eq!(marks(&dir.join("del.pdf")), vec![1, 3, 4]);

        run_op(
            &src,
            &dir.join("keep.pdf"),
            PdfOp::Keep {
                pages: vec![3, 1],
            },
        );
        assert_eq!(marks(&dir.join("keep.pdf")), vec![3, 1]);

        run_op(
            &src,
            &dir.join("reorder.pdf"),
            PdfOp::Reorder {
                order: vec![4, 3, 2, 1],
            },
        );
        assert_eq!(marks(&dir.join("reorder.pdf")), vec![4, 3, 2, 1]);

        run_op(
            &src,
            &dir.join("blank.pdf"),
            PdfOp::InsertBlank {
                at: 2,
                width: 595.0,
                height: 842.0,
            },
        );
        assert_eq!(marks(&dir.join("blank.pdf")), vec![1, 0, 2, 3, 4]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 多 op 顺序执行（前端一次导出会攒一串动作）：
    /// keep → insertBlank → rotate 的组合语义必须与前端算的页号一致。
    #[test]
    fn pdf_ops_sequence_keeps_page_numbers_consistent() {
        let dir = tmp_dir("seq");
        let src = dir.join("a.pdf");
        build_sample_pdf(&src, 4);
        let mut doc = Document::load(&src).unwrap();
        // 1) 只保留 4、2（重排）；2) 在第 2 页前插空白；3) 旋转第 3 页。
        for op in [
            PdfOp::Keep {
                pages: vec![4, 2],
            },
            PdfOp::InsertBlank {
                at: 2,
                width: 595.0,
                height: 842.0,
            },
            PdfOp::Rotate {
                pages: vec![3],
                degrees: 90,
            },
        ] {
            apply_pdf_op(&mut doc, &op).unwrap();
        }
        doc.prune_objects();
        doc.renumber_objects();
        let out = dir.join("seq.pdf");
        doc.save(&out).unwrap();
        // keep 后是 [4,2]，插空白后是 [4,空白,2]（空白页没有标记 → 0）。
        assert_eq!(marks(&out), vec![4, 0, 2]);
        // 旋转作用在插页之后的第 3 页 = 原第 2 页。
        let page = Document::load(&out).unwrap().page_iter().nth(2).unwrap();
        let rot = Document::load(&out)
            .unwrap()
            .get_dictionary(page)
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap();
        assert_eq!(rot, 90);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pdf_rotate_sets_rotate_key() {
        let dir = tmp_dir("rotate");
        let src = dir.join("a.pdf");
        build_sample_pdf(&src, 2);
        let mut doc = Document::load(&src).unwrap();
        apply_pdf_op(
            &mut doc,
            &PdfOp::Rotate {
                pages: vec![2],
                degrees: 270,
            },
        )
        .unwrap();
        let page = doc.page_iter().nth(1).unwrap();
        let rot = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap();
        assert_eq!(rot, 270);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn images_to_pdf_makes_one_page_per_image() {
        let dir = tmp_dir("imgpdf");
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            8,
            8,
            image::Rgb([10, 20, 30]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageFormat::Png,
        )
        .unwrap();
        let src = ImageSource {
            name: "a.png".into(),
            data_base64: crate::commands::base64_encode(&png),
            width: 8,
            height: 8,
        };
        let out = dir.join("out.pdf");
        let res = images_to_pdf(vec![src.clone(), src], out.to_string_lossy().to_string()).unwrap();
        assert!(res.bytes > 0);
        assert_eq!(Document::load(&out).unwrap().get_pages().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
