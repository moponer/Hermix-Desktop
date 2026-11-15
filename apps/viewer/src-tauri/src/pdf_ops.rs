//! PDF / 文本编辑内核（F6）。
//!
//! **原文件保护**：任何写盘都过 `paths::safe_output_path`——产物与源文件
//! 相同（Windows 上大小写不敏感，`..`、相对路径、**硬链接**都算同一
//! 文件）一律拒绝。默认产物名 `<原名>.edited.<扩展名>`，前端传什么都
//! 不覆盖原文件。
//!
//! **依赖**：新增唯一直接依赖 `lopdf` 0.34（0.34 已在工作区 Cargo.lock，
//! 传递依赖全在 → 编译成本 ≈ 0）。GBK 编码走 Win32 `WideCharToMultiByte`
//! （CP 936），本文件自带 extern 声明，**不引 encoding_rs**。
//!
//! **能力边界**：lopdf 0.34 支持 V1/V2 加密（RC4）、不支持 AES（V4/V5）；
//! 文本层替换只认 `Tj` 算子且要求整串精确相等（`TJ` 数组显示文本不处理）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::paths::safe_output_path;
use image::DynamicImage;
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Encoding, Object, ObjectId, Stream};
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
    /// 实际替换的处数（仅文本替换有值，其余为 null）
    pub replaced: Option<usize>,
    /// 需要如实告诉用户的话（例如"旧字未能真删除"）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ── 产物路径：原文件保护 ─────────────────────────────────────────────
//
// 守卫实现在 `paths.rs`（图片 / PDF / 文本共用一份）：判据含**文件身份**
// （volume serial + file index），硬链接也会被拦住。

/// 落盘公共出口：守卫 → 建目录 → 写盘 → 回报路径与字节数。
fn save_output(
    src: &Path,
    requested_out: Option<&str>,
    bytes: &[u8],
) -> Result<SaveResult, String> {
    let out = safe_output_path(src, requested_out, None)?;
    save_bytes(&out, bytes, None)
}

/// 写盘并回报结果（没有源文件可比的场合：图片转 PDF / 文本另存）。
pub(crate) fn save_bytes(
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
        replaced: None,
        note: None,
    })
}

// ── PDF 页树 ─────────────────────────────────────────────────────────

/// 页树里的一页：id + 从祖先继承来的属性。
///
/// 中间层 Pages 节点可以携带子页共享的 MediaBox / Resources / CropBox /
/// Rotate；页树扁平化后中间节点会被剪掉，这些继承属性必须显式落到页上，
/// 否则读者按默认纸张尺寸渲染、角度与裁剪被静默改掉。
#[derive(Clone)]
struct LeafPage {
    id: ObjectId,
    media_box: Option<Object>,
    resources: Option<Object>,
    crop_box: Option<Object>,
    rotate: Option<i64>,
}

/// 祖先链上继承下来的页属性。
#[derive(Clone, Default)]
struct Inherited {
    media_box: Option<Object>,
    resources: Option<Object>,
    crop_box: Option<Object>,
    rotate: Option<i64>,
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
    inherited: &Inherited,
    out: &mut Vec<LeafPage>,
) -> Result<(), String> {
    let dict = doc
        .get_dictionary(node)
        .map_err(|e| format!("PDF 结构损坏: {e}"))?;
    let media = dict.get(b"MediaBox").ok().or(inherited.media_box.as_ref());
    let res = dict.get(b"Resources").ok().or(inherited.resources.as_ref());
    let crop = dict.get(b"CropBox").ok().or(inherited.crop_box.as_ref());
    // Rotate 是整数（页显示角度，继承自祖先页节点）。
    let rotate = dict
        .get(b"Rotate")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .or(inherited.rotate);
    // 有 Kids 的是中间层 Pages 节点，没有的就是叶子页
    let kids = dict.get(b"Kids").ok().map(|k| resolve(doc, k));
    match kids {
        Some(kids) => {
            let arr = kids.as_array().map_err(|_| "PDF 页树 Kids 异常".to_string())?;
            let child_inherited = Inherited {
                media_box: media.cloned(),
                resources: res.cloned(),
                crop_box: crop.cloned(),
                rotate,
            };
            for kid in arr {
                let id = kid.as_reference().map_err(|_| "PDF 页树引用异常".to_string())?;
                collect_pages(doc, id, &child_inherited, out)?;
            }
        }
        None => out.push(LeafPage {
            id: node,
            media_box: media.cloned(),
            resources: res.cloned(),
            crop_box: crop.cloned(),
            rotate,
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
    collect_pages(doc, root, &Inherited::default(), &mut leaves)?;
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
        let (crop, rotate) = (page.crop_box.clone(), page.rotate);
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
        // CropBox / Rotate 同样可继承：丢了会让页面的裁剪与显示角度被
        // 静默改掉（读者以为是自己转的，其实是被文件编辑弄丢的）。
        if !dict.has(b"CropBox") {
            if let Some(c) = crop {
                dict.set("CropBox", c);
            }
        }
        if !dict.has(b"Rotate") {
            if let Some(rot) = rotate {
                dict.set("Rotate", Object::Integer(rot));
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
                    crop_box: None,
                    rotate: None,
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
        // 刻意不接 gb18030：它是 GBK 超集，用 CP936 编码会丢字符。
        // 宁可报「不支持」也不要写出一个内容不对的文件。
        "gbk" | "gb2312" | "cp936" | "ms936" => (TextEncoding::Gbk, false),
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
    /// 每页当前的显示角度（0/90/180/270，含从祖先继承来的）。
    /// 前端「再转 90°」必须基于它算**绝对**角度——后端写的是绝对值。
    pub rotations: Vec<i64>,
}

/// 加密 PDF 的处置：**准确**的原因。
///
/// lopdf 0.34 支持 V1/V2（RC4，`decrypt` 可用），不支持 V4/V5（AES）。
/// 此前一律报「AES 加密」，会把 RC4 加密件误诊——用户以为自己遇上的是
/// 最常见的那种 AES，实际是能解开的 RC4。
fn reject_encrypted(doc: &Document) -> Result<(), String> {
    if !doc.is_encrypted() {
        return Ok(());
    }
    let version = doc
        .trailer
        .get(b"Encrypt")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .and_then(|id| doc.get_dictionary(id).ok())
        .and_then(|d| d.get(b"V").ok())
        .and_then(|o| o.as_i64().ok());
    Err(match version {
        Some(1) | Some(2) => {
            "该 PDF 使用 RC4 口令加密（V1/V2），当前不支持编辑；请先用阅读器另存为无加密版本".to_string()
        }
        Some(4) | Some(5) => "该 PDF 使用 AES 加密（V4/V5），当前不支持编辑".to_string(),
        _ => "该 PDF 已加密，当前不支持编辑".to_string(),
    })
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
            rotations: Vec::new(),
        });
    }
    let leaves = read_pages(&doc)?.leaves;
    let rotations = leaves
        .iter()
        .map(|p| p.rotate.unwrap_or(0).rem_euclid(360))
        .collect();
    Ok(PdfInfo {
        path: src_path,
        pages: leaves.len() as u32,
        encrypted: false,
        bytes,
        rotations,
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
    safe_output_path(&src, out_path.as_deref(), None)?;
    let mut doc = Document::load(&src).map_err(|e| format!("无法打开 PDF: {e}"))?;
    reject_encrypted(&doc)?;
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

/// 统计第 `page` 页里「用 Tj 算子显示、且文本整串等于 from」的次数。
///
/// 与 lopdf 的 `replace_text` 用**同一套**编码解析（页字体 → 字体编码 →
/// `decode_text`），所以命中数与实际会不会被替换是一致的——不会出现
/// 页内文本替换：认 `Tj` / `'` / `"` 以及 `TJ` 数组里的每个字符串项。
///
/// **为什么自己写而不用 lopdf 的 `replace_text`**：那只处理 `Tj` 算子。而
/// Word / LaTeX / 各类报表工具生成的 PDF 大量使用 `TJ` 数组做字距微调
/// （一句话被拆成十几段），于是旧实现会报"明明看得见那行字，却没找到"。
///
/// 替换时用**同一字体的编码**把新文写回（`Document::encode_text`）——
/// 直接塞 UTF-8 字节是非法的，阅读器会按字体编码解出乱码。
///
/// 返回命中数；0 表示这一页没有可整串替换的文字（调用方据此报错）。
fn replace_text_in_page(
    doc: &mut Document,
    page: u32,
    from: &str,
    to: &str,
) -> Result<usize, String> {
    let page_id = doc
        .page_iter()
        .nth(page.saturating_sub(1) as usize)
        .ok_or_else(|| format!("页码 {page} 超出范围"))?;

    // 这一段借用 doc（编码表与页面内容都是只读），结束后才写回。
    let (new_data, hits) = {
        let encodings: BTreeMap<Vec<u8>, Encoding<'_>> = doc
            .get_page_fonts(page_id)
            .map_err(|e| format!("读取页字体失败: {e}"))?
            .into_iter()
            .map(|(name, font)| font.get_font_encoding(doc).map(|it| (name, it)))
            .collect::<std::result::Result<BTreeMap<Vec<u8>, Encoding<'_>>, lopdf::Error>>()
            .map_err(|e| format!("读取字体编码失败: {e}"))?;
        let data = doc
            .get_page_content(page_id)
            .map_err(|e| format!("读取页内容失败: {e}"))?;
        let mut content =
            Content::decode(&data).map_err(|e| format!("解析页内容失败: {e}"))?;

        let mut current: Option<&Encoding<'_>> = None;
        let mut hits = 0usize;
        for op in content.operations.iter_mut() {
            match op.operator.as_ref() {
                "Tf" => {
                    current = op
                        .operands
                        .first()
                        .and_then(|o| o.as_name().ok())
                        .and_then(|n| encodings.get(n));
                }
                // 直接显示文字的三个算子：Tj / ' / "
                "Tj" | "'" | "\"" => {
                    if let (Some(enc), Some(obj)) = (current, op.operands.first_mut()) {
                        if let Object::String(bytes, _) = obj {
                            if let Ok(text) = Document::decode_text(enc, bytes) {
                                if text == from {
                                    *bytes = Document::encode_text(enc, to);
                                    hits += 1;
                                }
                            }
                        }
                    }
                }
                // TJ 数组：逐项检查（字距微调会把一句话拆成多段）
                "TJ" => {
                    if let (Some(enc), Some(Object::Array(items))) =
                        (current, op.operands.first_mut())
                    {
                        for item in items.iter_mut() {
                            if let Object::String(bytes, _) = item {
                                if let Ok(text) = Document::decode_text(enc, bytes) {
                                    if text == from {
                                        *bytes = Document::encode_text(enc, to);
                                        hits += 1;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let encoded = content
            .encode()
            .map_err(|e| format!("编码页内容失败: {e}"))?;
        (encoded, hits)
    };

    if hits == 0 {
        return Ok(0);
    }
    // 写回页面内容流。多数页只有一个内容流；多个时合并进第一个并只留它。
    let ids = doc.get_page_contents(page_id);
    if let Some(&first) = ids.first() {
        if let Ok(Object::Stream(stream)) = doc.get_object_mut(first) {
            stream.set_content(new_data);
            let _ = stream.compress();
        }
        if ids.len() > 1 {
            if let Ok(dict) = doc.get_dictionary_mut(page_id) {
                dict.set(b"Contents", Object::Reference(first));
            }
        }
    }
    Ok(hits)
}

/// 2D 仿射矩阵 `[a b c d e f]`（PDF 的矩阵记法）。
#[derive(Clone, Copy, Debug)]
struct Mat {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Mat {
    fn identity() -> Self {
        Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 }
    }

    /// `self × m`（PDF 里 `cm` 是把新矩阵乘在左边）。
    fn mul(&self, m: &Mat) -> Mat {
        Mat {
            a: self.a * m.a + self.b * m.c,
            b: self.a * m.b + self.b * m.d,
            c: self.c * m.a + self.d * m.c,
            d: self.c * m.b + self.d * m.d,
            e: self.e * m.a + self.f * m.c + m.e,
            f: self.e * m.b + self.f * m.d + m.f,
        }
    }

    fn translate(tx: f64, ty: f64) -> Mat {
        Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: tx, f: ty }
    }
}

/// 在页面上找出并**删除**显示某段文字的算子（真删除：旧字不再存在于文件里）。
///
/// ## 判据：字符串精确匹配 + 位置消歧
///
/// PDFBox 的做法是逐字形追踪（`showGlyph` 给出每个字形的文本矩阵），靠
/// **位置**判断"这是不是我要删的那段"。但逐字形需要字体的 advance width
/// ——标准 14 号字没有 /Widths、CID 字体的 /W 又是另一套结构，取宽度这件事
/// 在纯 Rust 里代价不小。
///
/// 我们换一个等价判据：前端已经知道用户点了哪句字（**原文**），后端把
/// 每一处显示算子的字符串按**当前字体的编码**解出来，与原文精确比对；
/// 若同一页有多处相同文字，再用**位置**（文本矩阵 × CTM，精确可算）消歧。
/// 位置不可信时（同一 BT 块里的第二段文字且字体宽度未知）宁可不动 ——
/// 删错地方比没删更糟。
///
/// 返回删掉的处数；找不到返回 0。
fn remove_text_ops_at(
    doc: &mut Document,
    page: u32,
    rect: [f64; 4],
    target: &str,
) -> Result<usize, String> {
    let page_id = doc
        .page_iter()
        .nth(page.saturating_sub(1) as usize)
        .ok_or_else(|| format!("页码 {page} 超出范围"))?;

    let encodings: BTreeMap<Vec<u8>, Encoding<'_>> = doc
        .get_page_fonts(page_id)
        .map_err(|e| format!("读取页字体失败: {e}"))?
        .into_iter()
        .map(|(name, font)| font.get_font_encoding(doc).map(|it| (name, it)))
        .collect::<std::result::Result<BTreeMap<Vec<u8>, Encoding<'_>>, lopdf::Error>>()
        .map_err(|e| format!("读取字体编码失败: {e}"))?;

    let data = doc
        .get_page_content(page_id)
        .map_err(|e| format!("读取页内容失败: {e}"))?;
    let mut content =
        Content::decode(&data).map_err(|e| format!("解析页内容失败: {e}"))?;

    // ── 追踪矩阵状态，找出候选 ──
    let mut ctm = Mat::identity();
    let mut stack: Vec<Mat> = Vec::new();
    let mut tm = Mat::identity();
    let mut tlm = Mat::identity();
    let mut leading = 0.0f64;
    let mut current_font: Option<&Encoding<'_>> = None;
    // 同一 BT 块里，上一串显示之后位置是否还可信（宽度未知 → 不可信）
    let mut origin_reliable = true;

    // 候选：(算子下标, TJ 数组里的元素下标 Option, 起点)
    let mut candidates: Vec<(usize, Option<usize>, f64, f64, bool)> = Vec::new();

    for (idx, op) in content.operations.iter().enumerate() {
        match op.operator.as_ref() {
            "q" => stack.push(ctm),
            "Q" => {
                if let Some(m) = stack.pop() {
                    ctm = m;
                }
            }
            "cm" => {
                if let Some(m) = read_matrix(op.operands.as_slice()) {
                    ctm = m.mul(&ctm);
                }
            }
            "BT" => {
                tm = Mat::identity();
                tlm = Mat::identity();
                origin_reliable = true;
            }
            "Tm" => {
                if let Some(m) = read_matrix(op.operands.as_slice()) {
                    tm = m;
                    tlm = m;
                }
            }
            "TL" => {
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok().map(|v| v as f64)) {
                    leading = v;
                }
            }
            "Td" | "TD" => {
                if let (Some(tx), Some(ty)) = (
                    op.operands.first().and_then(|o| o.as_float().ok().map(|v| v as f64)),
                    op.operands.get(1).and_then(|o| o.as_float().ok().map(|v| v as f64)),
                ) {
                    if op.operator == "TD" {
                        leading = -ty;
                    }
                    tlm = Mat::translate(tx, ty).mul(&tlm);
                    tm = tlm;
                    origin_reliable = true;
                }
            }
            "T*" => {
                tlm = Mat::translate(0.0, -leading).mul(&tlm);
                tm = tlm;
                origin_reliable = true;
            }
            "Tf" => {
                current_font = op
                    .operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .and_then(|n| encodings.get(n));
            }
            "Tj" | "'" | "\"" => {
                let t = tm.mul(&ctm);
                if let (Some(enc), Some(Object::String(bytes, _))) =
                    (current_font, op.operands.first())
                {
                    if let Ok(text) = Document::decode_text(enc, bytes) {
                        if text == target {
                            candidates.push((idx, None, t.e, t.f, origin_reliable));
                        }
                    }
                }
                origin_reliable = false;
            }
            "TJ" => {
                let t = tm.mul(&ctm);
                if let (Some(enc), Some(Object::Array(items))) =
                    (current_font, op.operands.first())
                {
                    for (i, item) in items.iter().enumerate() {
                        if let Object::String(bytes, _) = item {
                            if let Ok(text) = Document::decode_text(enc, bytes) {
                                if text == target {
                                    candidates.push((idx, Some(i), t.e, t.f, origin_reliable));
                                }
                            }
                        }
                    }
                }
                origin_reliable = false;
            }
            _ => {}
        }
    }

    if candidates.is_empty() {
        return Ok(0);
    }

    // 多处相同文字 → 用位置消歧（位置不可信就不删，宁可让用户再点一次）
    let chosen: Vec<(usize, Option<usize>)> = if candidates.len() == 1 {
        vec![(candidates[0].0, candidates[0].1)]
    } else {
        let (lx, ly, hx, hy) = (
            rect[0].min(rect[2]),
            rect[1].min(rect[3]),
            rect[0].max(rect[2]),
            rect[1].max(rect[3]),
        );
        let tol = 3.0f64; // 位置容差（pt）
        let inside: Vec<(usize, Option<usize>)> = candidates
            .iter()
            .filter(|c| c.4 && c.2 >= lx - tol && c.2 <= hx + tol && c.3 >= ly - tol && c.3 <= hy + tol)
            .map(|c| (c.0, c.1))
            .collect();
        if inside.len() != 1 {
            return Err(format!(
                "这一页有 {} 处相同的文字，无法确定要改哪一处；请点更完整的一段文字",
                candidates.len()
            ));
        }
        inside
    };

    // ── 真正删掉 ──
    let mut removed = 0usize;
    let mut drop_ops: Vec<usize> = Vec::new();
    for (op_idx, elem) in &chosen {
        let op = &mut content.operations[*op_idx];
        match elem {
            None => drop_ops.push(*op_idx),
            Some(i) => {
                // TJ 数组：把命中的那一项换成空串（保留其余项与字距）
                if let Some(Object::Array(items)) = op.operands.first_mut() {
                    if let Some(slot) = items.get_mut(*i) {
                        *slot = Object::String(Vec::new(), lopdf::StringFormat::Literal);
                        removed += 1;
                    }
                }
            }
        }
    }
    if !drop_ops.is_empty() {
        drop_ops.sort_unstable();
        drop_ops.dedup();
        let mut i = 0usize;
        content.operations.retain(|_| {
            let keep = !drop_ops.contains(&i);
            i += 1;
            keep
        });
        removed += drop_ops.len();
    }

    if removed == 0 {
        return Ok(0);
    }

    // 写回页面内容流（多流时合并进第一个并只留它）。
    let encoded = content
        .encode()
        .map_err(|e| format!("编码页内容失败: {e}"))?;
    let ids = doc.get_page_contents(page_id);
    if let Some(&first) = ids.first() {
        if let Ok(Object::Stream(stream)) = doc.get_object_mut(first) {
            stream.set_content(encoded);
            let _ = stream.compress();
        }
        if ids.len() > 1 {
            if let Ok(dict) = doc.get_dictionary_mut(page_id) {
                dict.set(b"Contents", Object::Reference(first));
            }
        }
    }
    Ok(removed)
}

/// 从算子的操作数里读一个 6 元矩阵（`cm` / `Tm`）。
fn read_matrix(operands: &[Object]) -> Option<Mat> {
    if operands.len() < 6 {
        return None;
    }
    let g = |i: usize| operands[i].as_float().ok().map(|v| v as f64);
    Some(Mat {
        a: g(0)?,
        b: g(1)?,
        c: g(2)?,
        d: g(3)?,
        e: g(4)?,
        f: g(5)?,
    })
}

/// PDF 文本层替换。
///
/// **能力有限**（lopdf 0.34 的实现边界，命令名如实体现）：只处理 `Tj`
/// 算子，要求整串精确相等（不是子串替换），且字体编码解不出来时会跳过
/// 该处。扫描件（纯图片）没有文本层，本命令无效。
///
/// 命中 0 处时**直接报错**而不是静默产出一个内容未变的文件。
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
    let out = safe_output_path(&src, out_path.as_deref(), None)?;
    let mut doc = Document::load(&src).map_err(|e| format!("无法打开 PDF: {e}"))?;
    reject_encrypted(&doc)?;
    let hits = replace_text_in_page(&mut doc, page, &from, &to)?;
    if hits == 0 {
        return Err(format!(
            "第 {page} 页没有找到可整串替换的文本「{from}」\
             （该 PDF 可能没有文本层、文字被拆成多段，或字体编码解不出来）"
        ));
    }
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败: {e}"))?;
    let mut r = save_bytes(&out, &buf, None)?;
    r.replaced = Some(hits);
    Ok(r)
}

/// 在指定页加一条**便签批注**（`/Text` 注释）。
///
/// 坐标是 **PDF 用户空间**（左下为原点，单位 pt）—— 前端用 pdf.js 的
/// `viewport.convertToPdfPoint()` 从屏幕坐标换算过来。
///
/// 为什么用批注而不是把字"烧"进页面内容流：往内容流里插字要自己带字体
/// 资源，标准 14 号字没有中文字形（中文会渲染成空白），而批注把文字原样
/// 存进 `/Contents`，由阅读器用自己的字体渲染 —— 中文、emoji 都不丢。
///
/// 批注是**追加**语义：每次调用产出一份新文件（源文件仍受守卫保护）。
#[tauri::command(async)]
pub fn pdf_add_annotation(
    src_path: String,
    out_path: Option<String>,
    page: u32,
    x: f64,
    y: f64,
    text: String,
) -> Result<SaveResult, String> {
    let src = PathBuf::from(&src_path);
    if !src.is_file() {
        return Err(format!("不是文件: {src_path}"));
    }
    let note = text.trim();
    if note.is_empty() {
        return Err("批注内容为空".into());
    }
    let out = safe_output_path(&src, out_path.as_deref(), None)?;
    let mut doc = Document::load(&src).map_err(|e| format!("无法打开 PDF: {e}"))?;
    reject_encrypted(&doc)?;
    let page_id = doc
        .page_iter()
        .nth(page.saturating_sub(1) as usize)
        .ok_or_else(|| format!("页码 {page} 超出范围"))?;

    let mut annot = Dictionary::new();
    annot.set(b"Type", Object::Name(b"Annot".to_vec()));
    annot.set(b"Subtype", Object::Name(b"Text".to_vec()));
    // 便签图标 18×18 pt，锚在点击处（左下角）。
    annot.set(
        b"Rect",
        Object::Array(vec![
            Object::Real(x as f32),
            Object::Real(y as f32),
            Object::Real((x + 18.0) as f32),
            Object::Real((y + 18.0) as f32),
        ]),
    );
    annot.set(b"Contents", Object::string_literal(note));
    annot.set(b"T", Object::string_literal("Hermix"));
    annot.set(b"Name", Object::Name(b"Note".to_vec()));
    // F = 4：打印时可见。
    annot.set(b"F", Object::Integer(4));
    annot.set(b"Open", Object::Boolean(false));
    let annot_id = doc.add_object(annot);

    // 挂到页面的 /Annots：原来有就直接追加，没有就新建一个数组。
    append_annots(&mut doc, page_id, vec![annot_id]);

    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败: {e}"))?;
    save_bytes(&out, &buf, None)
}

/* ══════════════════════════════════════════════════════════════════
   标注（FreeText / Highlight / Ink）—— 画在页面上、写进 PDF 的注释层
   ══════════════════════════════════════════════════════════════════ */

/// 一条标注（前端画好的 → 这里写成 PDF 注释）。
///
/// 坐标一律是 **PDF 用户空间**（左下为原点，单位 pt）：前端用 pdf.js 的
/// `viewport.convertToPdfPoint()` 换算过来。
///
/// 为什么是标准注释而不是往页面内容流里插字：内容流插字要自带字体资源，
/// 标准 14 号字没有中文字形（中文渲染成空白）；注释把内容存在
/// `/Contents` / `/InkList` 里，由阅读器用自己的字体渲染 —— 中文、emoji
/// 都不丢，而且 Acrobat / 福昕 / Edge / pdf.js 都能正常显示与继续编辑。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PdfAnnotation {
    /// 自由文字：矩形 + 内容 + 字号 + 颜色。
    FreeText {
        rect: [f64; 4],
        contents: String,
        #[serde(default)]
        font_size: Option<f64>,
        #[serde(default)]
        color: Option<[f64; 3]>,
    },
    /// 高亮：矩形 + 四边形组（PDF 的 QuadPoints）。
    Highlight {
        rect: [f64; 4],
        #[serde(default)]
        quad_points: Vec<[f64; 8]>,
        #[serde(default)]
        color: Option<[f64; 3]>,
    },
    /// 墨迹：矩形 + 若干笔画（每条笔画是一串点）。
    Ink {
        rect: [f64; 4],
        #[serde(default)]
        ink_list: Vec<Vec<[f64; 2]>>,
        #[serde(default)]
        color: Option<[f64; 3]>,
        #[serde(default)]
        width: Option<f64>,
    },
    /// 图章：矩形 + 一张本地图片（路径是用户在系统文件对话框里自己选的）。
    Stamp {
        rect: [f64; 4],
        image_path: String,
    },
    /// 手写签名：矩形 + 签名位图（base64 PNG）+ 签名域名字。
    ///
    /// 写成 **AcroForm 的 /Sig 域**（不是普通注释）：Acrobat / 福昕 会把它
    /// 显示成"签名"（可双击重签、可校验），而普通 Stamp 只是"贴了张图"。
    Signature {
        rect: [f64; 4],
        image_base64: String,
        field_name: String,
    },
    /// **空签名域**：只放一个框，**不签名**，留给以后别人去签。
    ///
    /// 和 `Signature` 的区别：没有签名位图、没有内容，只有域本身。Acrobat /
    /// 福昕 会把它显示成可点击的「签名」字段，双击即进入签名流程；没装这类
    /// 阅读器的用户看到的是一个带边框的占位框。用途是「合同留一个签字栏发给
    /// 对方」——自己的手写签名那条路径做不到这个（那是替人签好了）。
    SignatureField {
        rect: [f64; 4],
        field_name: String,
    },
}

/// 把若干注释对象挂到某页的 /Annots（原来有就追加，没有就新建数组）。
fn append_annots(doc: &mut Document, page_id: ObjectId, annots: Vec<ObjectId>) {
    let existing = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|d| d.get(b"Annots").ok().cloned());
    match existing {
        Some(Object::Reference(id)) => {
            if let Ok(Object::Array(items)) = doc.get_object_mut(id) {
                for a in annots {
                    items.push(Object::Reference(a));
                }
            }
        }
        Some(Object::Array(mut items)) => {
            for a in annots {
                items.push(Object::Reference(a));
            }
            if let Ok(dict) = doc.get_dictionary_mut(page_id) {
                dict.set(b"Annots", Object::Array(items));
            }
        }
        _ => {
            let items = annots
                .into_iter()
                .map(Object::Reference)
                .collect::<Vec<_>>();
            let id = doc.add_object(Object::Array(items));
            if let Ok(dict) = doc.get_dictionary_mut(page_id) {
                dict.set(b"Annots", Object::Reference(id));
            }
        }
    }
}

/// 页面资源里补一个标准 Helvetica 字体。
///
/// FreeText 的 `/DA` 要引用一个字体名；页面资源里没有它时，严格的阅读器
/// 可能不画字。补一个 /Helv（非嵌入的标准字体，各阅读器自带的替代字体
/// 即可渲染）是最省事也最兼容的做法。
fn ensure_helvetica(doc: &mut Document, page_id: ObjectId) {
    let resources_id = match doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|d| d.get(b"Resources").ok().cloned())
    {
        Some(Object::Reference(id)) => id,
        _ => {
            let id = doc.add_object(Object::Dictionary(Dictionary::new()));
            if let Ok(dict) = doc.get_dictionary_mut(page_id) {
                dict.set(b"Resources", Object::Reference(id));
            }
            id
        }
    };
    let font_id = match doc
        .get_dictionary(resources_id)
        .ok()
        .and_then(|d| d.get(b"Font").ok().cloned())
    {
        Some(Object::Reference(id)) => id,
        _ => {
            let id = doc.add_object(Object::Dictionary(Dictionary::new()));
            if let Ok(dict) = doc.get_dictionary_mut(resources_id) {
                dict.set(b"Font", Object::Reference(id));
            }
            id
        }
    };
    let has_helv = doc
        .get_dictionary(font_id)
        .ok()
        .and_then(|d| d.get(b"Helv").ok().cloned())
        .is_some();
    if has_helv {
        return;
    }
    let helv_id = make_helvetica(doc);
    if let Ok(dict) = doc.get_dictionary_mut(font_id) {
        dict.set(b"Helv", Object::Reference(helv_id));
    }
}

/// 造一个标准 Helvetica 字体对象（非嵌入，读者自带替代字体即可渲染）。
fn make_helvetica(doc: &mut Document) -> ObjectId {
    let mut font = Dictionary::new();
    font.set(b"Type", Object::Name(b"Font".to_vec()));
    font.set(b"Subtype", Object::Name(b"Type1".to_vec()));
    font.set(b"BaseFont", Object::Name(b"Helvetica".to_vec()));
    font.set(b"Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    doc.add_object(Object::Dictionary(font))
}

fn real_array(values: impl IntoIterator<Item = f64>) -> Object {
    Object::Array(values.into_iter().map(|v| Object::Real(v as f32)).collect())
}

/// 文本 → PDF 字符串。
///
/// PDF 的字符串只有两种合法编码：PDFDocEncoding（ASCII）或 **UTF-16BE + BOM**。
/// 直接把 UTF-8 字节塞进字面串是**非法**的 —— 阅读器会按 PDFDocEncoding 解，
/// 中文变成乱码（pdf.js 直接解不出内容）。
fn pdf_text(s: &str) -> Object {
    if s.is_ascii() {
        return Object::string_literal(s);
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in s.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes, lopdf::StringFormat::Hexadecimal)
}

fn color_array(color: Option<[f64; 3]>) -> Object {
    let c = color.unwrap_or([1.0, 0.92, 0.23]); // 默认高亮黄
    real_array(c)
}

/// 把一条标注变成 PDF 注释对象。
fn build_annot(
    doc: &mut Document,
    ann: &PdfAnnotation,
    page_id: ObjectId,
) -> Result<ObjectId, String> {
    let mut d = Dictionary::new();
    d.set(b"Type", Object::Name(b"Annot".to_vec()));
    d.set(b"F", Object::Integer(4)); // 打印时可见
    match ann {
        PdfAnnotation::FreeText {
            rect,
            contents,
            font_size,
            color,
        } => {
            d.set(b"Subtype", Object::Name(b"FreeText".to_vec()));
            d.set(b"Rect", real_array(rect.to_vec()));
            d.set(b"Contents", pdf_text(contents));
            let size = font_size.unwrap_or(14.0);
            // /DA：默认外观串（字体名 + 字号 + 颜色）
            let da = format!("/Helv {} Tf 0 g", size.round() as i64);
            d.set(b"DA", Object::string_literal(da.as_str()));
            d.set(b"C", color_array(color.map(|c| [c[0], c[1], c[2]])));
        }
        PdfAnnotation::Highlight {
            rect,
            quad_points,
            color,
        } => {
            d.set(b"Subtype", Object::Name(b"Highlight".to_vec()));
            d.set(b"Rect", real_array(rect.to_vec()));
            // /QuadPoints 是**扁平**的 8n 个数（不是数组的数组）—— 写成嵌套
            // 数组时 pdf.js 会直接把这面高亮丢掉（静默失败，最难查的一种）。
            let mut quads: Vec<f64> = Vec::with_capacity(quad_points.len() * 8);
            for q in quad_points {
                quads.extend_from_slice(&q[..]);
            }
            if !quads.is_empty() {
                d.set(b"QuadPoints", real_array(quads));
            }
            d.set(b"C", color_array(*color));
        }
        PdfAnnotation::Ink {
            rect,
            ink_list,
            color,
            width,
        } => {
            d.set(b"Subtype", Object::Name(b"Ink".to_vec()));
            d.set(b"Rect", real_array(rect.to_vec()));
            let strokes: Vec<Object> = ink_list
                .iter()
                .map(|s| {
                    let mut nums: Vec<f64> = Vec::with_capacity(s.len() * 2);
                    for p in s {
                        nums.push(p[0]);
                        nums.push(p[1]);
                    }
                    real_array(nums)
                })
                .collect();
            if !strokes.is_empty() {
                d.set(b"InkList", Object::Array(strokes));
            }
            d.set(b"C", color_array(*color));
            let mut bs = Dictionary::new();
            bs.set(b"W", Object::Real(width.unwrap_or(2.0) as f32));
            d.set(b"BS", Object::Dictionary(bs));
        }
        PdfAnnotation::Stamp { rect, image_path } => {
            d.set(b"Subtype", Object::Name(b"Stamp".to_vec()));
            d.set(b"Rect", real_array(rect.to_vec()));
            d.set(b"Name", Object::Name(b"Stamp".to_vec()));
            // 图章要有**外观流**（/AP）才会被画出来：注释本身只存位置，
            // 真正"长什么样"在 /AP /N 里。
            let ap = build_stamp_appearance(doc, rect, image_path)?;
            let mut apd = Dictionary::new();
            apd.set(b"N", Object::Reference(ap));
            d.set(b"AP", Object::Dictionary(apd));
        }
        PdfAnnotation::Signature {
            rect,
            image_base64,
            field_name,
        } => {
            // 签名域是 AcroForm 的 widget：/Subtype /Widget + /FT /Sig。
            // 这样 Acrobat / 福昕 把它显示成"签名"（可双击重签），而不是
            // 一张贴上去的图。
            d.set(b"Subtype", Object::Name(b"Widget".to_vec()));
            d.set(b"FT", Object::Name(b"Sig".to_vec()));
            d.set(b"T", pdf_text(field_name));
            d.set(b"Rect", real_array(rect.to_vec()));
            d.set(b"F", Object::Integer(4)); // 打印时可见
            d.set(b"P", Object::Reference(page_id));
            let (jpeg, w, h) = decode_signature_png(image_base64)?;
            let ap = build_image_appearance(doc, rect, &jpeg, w, h);
            let mut apd = Dictionary::new();
            apd.set(b"N", Object::Reference(ap));
            d.set(b"AP", Object::Dictionary(apd));
        }
        PdfAnnotation::SignatureField { rect, field_name } => {
            // 空签名域：结构与上面那个手写签名**完全一样**（Widget + /FT /Sig），
            // 区别只是**没有签名位图** —— 域是空的，等别人来签。
            d.set(b"Subtype", Object::Name(b"Widget".to_vec()));
            d.set(b"FT", Object::Name(b"Sig".to_vec()));
            d.set(b"T", pdf_text(field_name));
            // /TU = 悬停提示（Acrobat 里鼠标停上去显示）。
            d.set(b"TU", pdf_text(&format!("{field_name}（双击签名）")));
            d.set(b"Rect", real_array(rect.to_vec()));
            d.set(b"F", Object::Integer(4)); // 打印时可见
            d.set(b"P", Object::Reference(page_id));
            // 外观流：只画一个空框（0.7pt 灰边）。没有 /AP 的话，部分阅读器
            // 会把域画成「什么都没有」，用户根本不知道这里能签。
            let ap = build_empty_signature_appearance(doc, rect);
            let mut apd = Dictionary::new();
            apd.set(b"N", Object::Reference(ap));
            d.set(b"AP", Object::Dictionary(apd));
            // /MK = 域外观（Acrobat「域选项」里看到的就是它）。
            let mut mk = Dictionary::new();
            mk.set(b"BC", real_array(vec![0.45, 0.45, 0.45]));
            d.set(b"MK", Object::Dictionary(mk));
        }
    }
    Ok(doc.add_object(d))
}

/// 空签名域的外观流：一个空心矩形边框。
///
/// 结构与图片外观同源，只是内容流画的是边框而不是位图：
///   /XObject /Subtype /Form，BBox = 注释矩形尺寸
///   内容：`0.7 w 0.45 G 0.5 0.5 (w-1) (h-1) re S`
fn build_empty_signature_appearance(doc: &mut Document, rect: &[f64; 4]) -> ObjectId {
    let (x0, y0, x1, y1) = (rect[0], rect[1], rect[2], rect[3]);
    let w = (x1 - x0).abs().max(2.0);
    let h = (y1 - y0).abs().max(2.0);
    let content = format!("q 0.7 w 0.45 G 0.5 0.5 {iw} {ih} re S Q", iw = w - 1.0, ih = h - 1.0);
    let mut fd = Dictionary::new();
    fd.set(b"Type", Object::Name(b"XObject".to_vec()));
    fd.set(b"Subtype", Object::Name(b"Form".to_vec()));
    fd.set(b"BBox", real_array(vec![0.0, 0.0, w, h]));
    fd.set(b"Resources", Object::Dictionary(Dictionary::new()));
    doc.add_object(Stream::new(fd, content.into_bytes()))
}

/// 图章的外观流：把图片按矩形铺满。
///
/// 结构（PDF 规范）：
///   图片   → /XObject /Subtype /Image（DCTDecode 的 JPEG）
///   外观   → /XObject /Subtype /Form，BBox = 注释矩形，内容里 `cm` 缩放后 `Do`
fn build_stamp_appearance(
    doc: &mut Document,
    rect: &[f64; 4],
    image_path: &str,
) -> Result<ObjectId, String> {
    let path = PathBuf::from(image_path);
    let raw = std::fs::read(&path).map_err(|e| format!("读取图章图片失败：{e}"))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "stamp".into());
    let img = decode_image(&raw, &name)?.to_rgb8();
    let (w, h) = img.dimensions();
    let jpeg = encode_jpeg(&img, "图章图片")?;
    Ok(build_image_appearance(doc, rect, &jpeg, w, h))
}

/// 图片 → JPEG（PDF 里直接用 DCTDecode，少一层解压）。
fn encode_jpeg(img: &image::RgbImage, label: &str) -> Result<Vec<u8>, String> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("{label}尺寸为 0"));
    }
    let mut jpeg: Vec<u8> = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(img)
        .map_err(|e| format!("{label}编码失败：{e}"))?;
    Ok(jpeg)
}

/// 前端手写签名（PNG，base64）→ (JPEG 字节, 宽, 高)。
fn decode_signature_png(image_base64: &str) -> Result<(Vec<u8>, u32, u32), String> {
    let raw = crate::commands::base64_decode(image_base64)
        .ok_or("签名图片不是合法的 base64")?;
    let img = image::load_from_memory(&raw)
        .map_err(|e| format!("签名图片解码失败：{e}"))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    let jpeg = encode_jpeg(&img, "签名图片")?;
    Ok((jpeg, w, h))
}

/// 图片外观流（签名与图章共用）：把一张 JPEG 按矩形铺满。
fn build_image_appearance(
    doc: &mut Document,
    rect: &[f64; 4],
    jpeg: &[u8],
    iw: u32,
    ih: u32,
) -> ObjectId {
    let mut sd = Dictionary::new();
    sd.set(b"Type", Object::Name(b"XObject".to_vec()));
    sd.set(b"Subtype", Object::Name(b"Image".to_vec()));
    sd.set(b"Width", Object::Integer(iw as i64));
    sd.set(b"Height", Object::Integer(ih as i64));
    sd.set(b"ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    sd.set(b"BitsPerComponent", Object::Integer(8));
    sd.set(b"Filter", Object::Name(b"DCTDecode".to_vec()));
    let image_id = doc.add_object(Stream::new(sd, jpeg.to_vec()));

    let (x0, y0, x1, y1) = (rect[0], rect[1], rect[2], rect[3]);
    let rw = (x1 - x0).abs().max(1.0);
    let rh = (y1 - y0).abs().max(1.0);
    // 内容流：把单位正方形拉成矩形大小，再贴到矩形左下角。
    let content = format!(
        "q {rw} 0 0 {rh} {x0} {y0} cm /Im0 Do Q",
        rw = rw,
        rh = rh,
        x0 = x0,
        y0 = y0
    );

    let mut resources = Dictionary::new();
    let mut xobjects = Dictionary::new();
    xobjects.set(b"Im0", Object::Reference(image_id));
    resources.set(b"XObject", Object::Dictionary(xobjects));

    let mut fd = Dictionary::new();
    fd.set(b"Type", Object::Name(b"XObject".to_vec()));
    fd.set(b"Subtype", Object::Name(b"Form".to_vec()));
    fd.set(b"FormType", Object::Integer(1));
    fd.set(
        b"BBox",
        real_array([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]),
    );
    fd.set(b"Resources", Object::Dictionary(resources));
    doc.add_object(Stream::new(fd, content.into_bytes()))
}

/// 按位置替换页面里的一段文字（**遮盖 + 插入**）。
///
/// 与 `pdf_replace_text`（内容流整串匹配）是**两条不同的路**：
///   - `pdf_replace_text`：在内容流里找一模一样的字节串。遇到 TJ 拆段、
///     自定义编码、子集字体就失配 —— PDFBox 2.0 为此删掉了官方示例。
///   - 本命令：**由调用方给位置**（前端用 pdf.js 的文本层算出目标文字的
///     矩形与基线），这里只管"盖住那块 + 用受控字体写新字"。
///     与原文字怎么编码、怎么拆段完全无关。
///
/// **写法**：不动原有内容流 —— 新算子写进一个**新的内容流**，页面的
/// `/Contents` 变成 `[原流…, 新流]`（PDF 规范允许多个流顺序拼接）。
/// 这样原内容一个字节都不动，零风险。
///
/// **已知代价**：旧字仍在文件里（被矩形盖住），复制仍可能带出。要真正
/// 删掉要走"丢弃绘制算子"那条路（见决策笔记的 P1）。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TextReplaceAt {
    /// 页码（1-based）。
    pub page: u32,
    /// 要盖住的矩形（PDF 用户空间）：[x0, y0, x1, y1]。
    pub rect: [f64; 4],
    /// 新文字的基线起点（PDF 用户空间）。
    pub x: f64,
    pub y: f64,
    /// 新文字。
    pub text: String,
    /// 字号（pt）。
    pub font_size: f64,
    /// 盖住原字用的底色（RGB 0..1）。默认白。
    #[serde(default)]
    pub background: Option<[f64; 3]>,
    /// 要**真删除**的原文（前端知道用户点了哪句）。
    ///
    /// 给了就先把内容流里显示这段文字的算子删掉（真删除：复制不出来），
    /// 再盖 + 写新字；不给就只做"遮盖 + 写入"（旧字仍在文件里）。
    #[serde(default)]
    pub remove_text: Option<String>,
}

/// PDF 字面串的转义 + WinAnsi 编码（标准 14 号字只认单字节编码）。
fn latin1_literal(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '(' => out.extend_from_slice(b"\\("),
            ')' => out.extend_from_slice(b"\\)"),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 256 => out.push(c as u8),
            // 编码表里没有的字符用 '?' 代替（与"中文走注释层"的策略一致）。
            _ => out.push(b'?'),
        }
    }
    out
}

/// 按位置替换页面里的一段文字：盖住原字 + 用标准 14 号字写新字。
#[tauri::command(async)]
pub fn pdf_replace_text_at(
    src_path: String,
    out_path: Option<String>,
    req: TextReplaceAt,
) -> Result<SaveResult, String> {
    let src = PathBuf::from(&src_path);
    if !src.is_file() {
        return Err(format!("不是文件: {src_path}"));
    }
    if req.text.is_empty() {
        return Err("新文字为空".into());
    }
    let out = safe_output_path(&src, out_path.as_deref(), None)?;
    let mut doc = Document::load(&src).map_err(|e| format!("无法打开 PDF: {e}"))?;
    reject_encrypted(&doc)?;
    let page_id = doc
        .page_iter()
        .nth(req.page.saturating_sub(1) as usize)
        .ok_or_else(|| format!("页码 {} 超出范围", req.page))?;

    // ① 先真删除旧字的绘制算子（可选）。
    //
    // 尽力而为：页面内容解不出来时（lopdf 的内容流解析器**不支持内联图像
    // BI/ID/EI**，含内联图的页会解析失败）就只做"遮盖 + 写入"，并如实
    // 告诉用户"旧字没能真删除"—— 不能因为删不掉就让整条命令失败。
    let mut removal_note: Option<String> = None;
    let removed = match req.remove_text.as_deref().filter(|s| !s.is_empty()) {
        Some(text) => match remove_text_ops_at(&mut doc, req.page, req.rect, text) {
            Ok(n) => n,
            Err(e) => {
                removal_note = Some(format!("旧字未能真删除（{e}）；已做遮盖 + 写入"));
                0
            }
        },
        None => 0,
    };

    // ② 选字体：优先用**文档自己嵌的字体**（同款、保真最好、产物不涨体积）。
    //
    // 文档里的中文字体是**子集**（Identity-H + ToUnicode），不含的字写不出来 ——
    // 这种情况如实报错，不静默写乱码。拉丁文则可以退回标准 14 号 Helvetica。
    let font_name: String;
    let text_op: Vec<u8>;
    match crate::pdf_font::pick_font_for_text(&doc, page_id, &req.text) {
        Some(choice) => {
            font_name = crate::pdf_font::font_ref(&choice.name);
            text_op = crate::pdf_font::text_operator(&choice);
        }
        None => {
            if !req.text.is_ascii() {
                return Err(
                    "这份 PDF 的字体里写不了这些字（页面里的字体是子集，不含这些字）。\
                     换一个文中出现过的字，或用支持中文嵌入字体的 PDF。"
                        .into(),
                );
            }
            ensure_helvetica(&mut doc, page_id);
            font_name = "/Helv".to_string();
            let mut op = Vec::new();
            op.extend_from_slice(b"(");
            op.extend_from_slice(&latin1_literal(&req.text));
            op.extend_from_slice(b") Tj\n");
            text_op = op;
        }
    }

    let (rx0, ry0, rx1, ry1) = (req.rect[0], req.rect[1], req.rect[2], req.rect[3]);
    let w = (rx1 - rx0).abs().max(1.0);
    let h = (ry1 - ry0).abs().max(1.0);
    let bg = req.background.unwrap_or([1.0, 1.0, 1.0]);
    let size = req.font_size.clamp(1.0, 200.0);

    // ③ 盖住原字：填一个与背景同色的矩形。
    // ④ 写新字：以基线 (x, y) 为起点。
    let mut ops: Vec<u8> = Vec::new();
    ops.extend_from_slice(
        format!(
            "{r} {g} {b} rg\n{x0} {y0} {w} {h} re\nf\n",
            r = bg[0],
            g = bg[1],
            b = bg[2],
            x0 = rx0.min(rx1),
            y0 = ry0.min(ry1),
            w = w,
            h = h
        )
        .as_bytes(),
    );
    ops.extend_from_slice(b"BT\n");
    ops.extend_from_slice(format!("{font_name} {size} Tf\n").as_bytes());
    ops.extend_from_slice(format!("{x} {y} Td\n", x = req.x, y = req.y).as_bytes());
    ops.extend_from_slice(&text_op);
    ops.extend_from_slice(b"ET\n");


    // 新内容流 + 页面的 /Contents 变成 [原流…, 新流]（原内容零改动）。
    let new_stream_id = doc.add_object(Stream::new(Dictionary::new(), ops));
    let mut contents: Vec<Object> = doc
        .get_page_contents(page_id)
        .into_iter()
        .map(Object::Reference)
        .collect();
    contents.push(Object::Reference(new_stream_id));
    if let Ok(dict) = doc.get_dictionary_mut(page_id) {
        dict.set(b"Contents", Object::Array(contents));
    }

    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败: {e}"))?;
    let mut r = save_bytes(&out, &buf, None)?;
    r.replaced = Some(removed);
    r.note = removal_note;
    Ok(r)
}

/// 批量写入标注（自由文字 / 高亮 / 墨迹），产出新文件。
#[tauri::command(async)]
pub fn pdf_add_annotations(
    src_path: String,
    out_path: Option<String>,
    page: u32,
    annotations: Vec<PdfAnnotation>,
) -> Result<SaveResult, String> {
    let src = PathBuf::from(&src_path);
    if !src.is_file() {
        return Err(format!("不是文件: {src_path}"));
    }
    if annotations.is_empty() {
        return Err("没有要写入的标注".into());
    }
    let out = safe_output_path(&src, out_path.as_deref(), None)?;
    let mut doc = Document::load(&src).map_err(|e| format!("无法打开 PDF: {e}"))?;
    reject_encrypted(&doc)?;
    let page_id = doc
        .page_iter()
        .nth(page.saturating_sub(1) as usize)
        .ok_or_else(|| format!("页码 {page} 超出范围"))?;

    let needs_font = annotations
        .iter()
        .any(|a| matches!(a, PdfAnnotation::FreeText { .. }));
    if needs_font {
        ensure_helvetica(&mut doc, page_id);
    }
    let ids = annotations
        .iter()
        .map(|a| build_annot(&mut doc, a, page_id))
        .collect::<std::result::Result<Vec<_>, String>>()?;
    append_annots(&mut doc, page_id, ids.clone());

    // 签名域要额外登记到 **AcroForm**（catalog 的 /AcroForm）：只挂 /Annots
    // 的话，Acrobat 会把它当普通注释，而不是可签名的域。
    let sig_ids = ids
        .iter()
        .zip(annotations.iter())
        .filter(|(_, a)| {
            matches!(
                a,
                PdfAnnotation::Signature { .. } | PdfAnnotation::SignatureField { .. }
            )
        })
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    if !sig_ids.is_empty() {
        register_signature_fields(&mut doc, &sig_ids);
    }

    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败: {e}"))?;
    save_bytes(&out, &buf, None)
}

/// 把签名 widget 登记进文档的 AcroForm。
///
/// 结构（PDF 规范）：
///   catalog /AcroForm ← { /Fields [widget…], /DA, /DR, /SigFlags }
///   /SigFlags = 1：文档里存在签名域。
fn register_signature_fields(doc: &mut Document, sig_ids: &[ObjectId]) {
    // /DR（默认资源）里放一个 /Helv —— 表单的默认外观串要引用它。
    let helv_id = make_helvetica(doc);
    let mut fonts = Dictionary::new();
    fonts.set(b"Helv", Object::Reference(helv_id));
    let mut dr = Dictionary::new();
    dr.set(b"Font", Object::Dictionary(fonts));

    // 复用已有的 /AcroForm（文件里本来就可能有表单）。
    let acro_id = match doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok().cloned())
    {
        Some(Object::Reference(id)) => id,
        _ => {
            let id = doc.add_object(Object::Dictionary(Dictionary::new()));
            if let Ok(cat) = doc.catalog_mut() {
                cat.set(b"AcroForm", Object::Reference(id));
            }
            id
        }
    };

    let mut fields: Vec<Object> = match doc
        .get_dictionary(acro_id)
        .ok()
        .and_then(|d| d.get(b"Fields").ok().cloned())
    {
        Some(Object::Array(items)) => items,
        Some(Object::Reference(id)) => doc
            .get_object(id)
            .ok()
            .and_then(|o| o.as_array().ok().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    for id in sig_ids {
        fields.push(Object::Reference(*id));
    }

    if let Ok(dict) = doc.get_dictionary_mut(acro_id) {
        dict.set(b"Fields", Object::Array(fields));
        dict.set(b"DA", Object::string_literal("/Helv 0 Tf 0 g"));
        dict.set(b"DR", Object::Dictionary(dr));
        dict.set(b"SigFlags", Object::Integer(1));
    }
}

/// 图片转 PDF：每张图一页。
///
/// `src_path` 用于**原文件保护**：产物不能是源文件本身（哪怕源是图片、
/// 产物是 .pdf，路径撞上仍会覆盖）。传 None 表示没有可比对的源文件。
#[tauri::command(async)]
pub fn images_to_pdf(
    sources: Vec<ImageSource>,
    out_path: String,
    src_path: Option<String>,
) -> Result<SaveResult, String> {
    if sources.is_empty() {
        return Err("没有可转换的图片".into());
    }
    let out = match src_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(src) => safe_output_path(Path::new(src), Some(&out_path), None)?,
        None => PathBuf::from(&out_path),
    };
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
    let (enc, fell_back) = resolve_encoding(&encoding);
    if fell_back {
        // 静默回退 UTF-8 会让用户以为写的是 GBK，实际是 UTF-8。
        return Err(format!(
            "未知编码「{encoding}」，已支持：utf-8 / utf-16le / utf-16be / gbk"
        ));
    }
    let bytes = encode_text(&text, enc)?;
    let out = safe_output_path(&src, out_path.as_deref(), None)?;
    save_bytes(&out, &bytes, Some(enc.name().to_string()))
}

// ── 单测 ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_guard_rejects_source_keeps_other() {
        let src = PathBuf::from(r"C:\pics\cat.pdf");
        assert!(safe_output_path(&src, Some(r"C:\pics\cat.pdf"), None)
            .unwrap_err()
            .contains("不能与原文件相同"));
        // 大小写不同的同一路径也要拒（Windows 不敏感）
        assert!(safe_output_path(&src, Some(r"C:\Pics\CAT.PDF"), None)
            .unwrap_err()
            .contains("不能与原文件相同"));
        let out = safe_output_path(&src, Some(r"C:\pics\cat.edited.pdf"), None).unwrap();
        assert_eq!(out.file_name().unwrap().to_string_lossy(), "cat.edited.pdf");
    }

    // 路径守卫的测试住在 `paths.rs`（图片 / PDF / 文本共用一份实现，
    // 硬链接判据的测试也在那里）。

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

    /// 页树根节点的 (Count, Kids 长度)。
    ///
    /// 为什么要单独断言：`Count` 错了 lopdf 的 `page_iter()` **照样绿**
    /// （它只遍历 Kids，从不读 Count）——不单独断言就等于没测。
    fn count_and_kids(path: &Path) -> (i64, usize) {
        let doc = Document::load(path).unwrap();
        let root = doc
            .catalog()
            .unwrap()
            .get(b"Pages")
            .unwrap()
            .as_reference()
            .unwrap();
        let d = doc.get_dictionary(root).unwrap();
        let count = d.get(b"Count").unwrap().as_i64().unwrap();
        let kids = d.get(b"Kids").unwrap().as_array().unwrap().len();
        (count, kids)
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
        // Count 必须与 Kids 一致（否则读者报「文件损坏」）。
        let (count, kids) = count_and_kids(&dir.join("blank.pdf"));
        assert_eq!(count as usize, kids);
        assert_eq!(kids, 5);

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
        let (count, kids) = count_and_kids(&out);
        assert_eq!(count as usize, kids);
        assert_eq!(kids, 3);
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
        let res = images_to_pdf(vec![src.clone(), src], out.to_string_lossy().to_string(), None).unwrap();
        assert!(res.bytes > 0);
        assert_eq!(Document::load(&out).unwrap().get_pages().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 批注要真的挂到**指定那一页**的 /Annots 上，内容原样保留。
    #[test]
    fn annotation_is_appended_to_the_target_page() {
        let dir = tmp_dir("annot");
        let src = dir.join("a.pdf");
        build_sample_pdf(&src, 2);
        let res = pdf_add_annotation(
            src.to_string_lossy().to_string(),
            None,
            2,
            100.0,
            200.0,
            "hello note".into(),
        )
        .unwrap();
        let doc = Document::load(&res.path).unwrap();
        let page = doc.page_iter().nth(1).unwrap(); // 第 2 页
        let annots_id = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Annots")
            .unwrap()
            .as_reference()
            .unwrap();
        let items = doc.get_object(annots_id).unwrap().as_array().unwrap();
        assert_eq!(items.len(), 1);
        let annot = doc
            .get_dictionary(items[0].as_reference().unwrap())
            .unwrap();
        assert_eq!(
            annot.get(b"Subtype").unwrap().as_name().unwrap(),
            b"Text".as_slice()
        );
        match annot.get(b"Contents").unwrap() {
            Object::String(bytes, _) => assert_eq!(bytes, b"hello note"),
            other => panic!("批注内容不是字符串：{other:?}"),
        }
        // 另一页不该被牵连。
        let other = doc.page_iter().nth(0).unwrap();
        assert!(doc.get_dictionary(other).unwrap().get(b"Annots").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 自由文字 / 高亮 / 墨迹三种标注写进 PDF 后，注释层结构要对。
    ///
    /// 这是"别的阅读器能不能看见"的地基：类型名（/FreeText / /Highlight /
    /// /Ink）、矩形、QuadPoints、InkList、/DA 与 /C 都是 PDF 规范里的标准
    /// 字段 —— 写对了，Acrobat / 福昕 / Edge / pdf.js 才会认。
    #[test]
    fn three_annotation_kinds_are_written_as_standard_annots() {
        let dir = tmp_dir("annots");
        let src = dir.join("a.pdf");
        build_sample_pdf(&src, 2);
        let res = pdf_add_annotations(
            src.to_string_lossy().to_string(),
            None,
            1,
            vec![
                PdfAnnotation::FreeText {
                    rect: [100.0, 200.0, 300.0, 240.0],
                    contents: "签字处".into(),
                    font_size: Some(18.0),
                    color: Some([0.1, 0.2, 0.8]),
                },
                PdfAnnotation::Highlight {
                    rect: [50.0, 400.0, 250.0, 420.0],
                    quad_points: vec![[50.0, 420.0, 250.0, 420.0, 50.0, 400.0, 250.0, 400.0]],
                    color: Some([1.0, 0.92, 0.23]),
                },
                PdfAnnotation::Ink {
                    rect: [10.0, 10.0, 60.0, 60.0],
                    ink_list: vec![vec![[10.0, 10.0], [30.0, 40.0], [60.0, 20.0]]],
                    color: Some([0.9, 0.1, 0.1]),
                    width: Some(2.0),
                },
            ],
        )
        .unwrap();
        let out = PathBuf::from(&res.path);
        // 留一份给外部解析器（pdf.js）交叉验证用。
        let probe = std::env::temp_dir().join("hermix-annots-probe.pdf");
        let _ = std::fs::copy(&out, &probe);

        let doc = Document::load(&out).unwrap();
        let page = doc.page_iter().nth(0).unwrap();
        let annots_id = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Annots")
            .unwrap()
            .as_reference()
            .unwrap();
        let items = doc.get_object(annots_id).unwrap().as_array().unwrap();
        assert_eq!(items.len(), 3);

        let subtypes = items
            .iter()
            .map(|it| {
                doc.get_dictionary(it.as_reference().unwrap())
                    .unwrap()
                    .get(b"Subtype")
                    .unwrap()
                    .as_name()
                    .unwrap()
                    .to_vec()
            })
            .collect::<Vec<_>>();
        assert_eq!(subtypes, vec![b"FreeText".to_vec(), b"Highlight".to_vec(), b"Ink".to_vec()]);

        // 自由文字：内容与默认外观串（/DA）要在。
        let ft = doc
            .get_dictionary(items[0].as_reference().unwrap())
            .unwrap();
        match ft.get(b"Contents").unwrap() {
            // 非 ASCII 文本按 PDF 规范必须是 **UTF-16BE + BOM**，不是 UTF-8 原始字节
            // （把 UTF-8 直接塞进字面串，阅读器按 PDFDocEncoding 解 → 中文乱码）。
            Object::String(bytes, _) => {
                let mut expect = vec![0xFE, 0xFF];
                for unit in "签字处".encode_utf16() {
                    expect.extend_from_slice(&unit.to_be_bytes());
                }
                assert_eq!(bytes, expect.as_slice());
            }
            other => panic!("Contents 不是字符串：{other:?}"),
        }
        match ft.get(b"DA").unwrap() {
            Object::String(bytes, _) => {
                assert!(String::from_utf8_lossy(bytes).contains("18 Tf"));
            }
            other => panic!("DA 不是字符串：{other:?}"),
        }

        // 高亮：QuadPoints 必须是**扁平的 8n 个数**（四个数一组）。
        // 写成数组的数组时 pdf.js 会静默丢弃这条高亮 —— 这就是断言要盯的。
        let hl = doc
            .get_dictionary(items[1].as_reference().unwrap())
            .unwrap();
        let quads = hl.get(b"QuadPoints").unwrap().as_array().unwrap();
        assert_eq!(quads.len(), 8, "QuadPoints 必须是扁平的 8 个数");
        assert!(
            quads.iter().all(|q| q.as_float().is_ok()),
            "QuadPoints 的每一项都应是数字（不是嵌套数组）：{quads:?}"
        );

        // 墨迹：InkList 一条笔画六个数。
        let ink = doc
            .get_dictionary(items[2].as_reference().unwrap())
            .unwrap();
        let strokes = ink.get(b"InkList").unwrap().as_array().unwrap();
        assert_eq!(strokes.len(), 1);
        assert_eq!(strokes[0].as_array().unwrap().len(), 6);

        // 页面资源里补了 Helvetica（FreeText 的 /DA 要引用它）。
        let resources = doc.get_dictionary(page).unwrap().get(b"Resources").unwrap();
        let res_dict = doc
            .get_dictionary(resources.as_reference().unwrap())
            .unwrap();
        let fonts = res_dict.get(b"Font").unwrap().as_reference().unwrap();
        assert!(doc
            .get_dictionary(fonts)
            .unwrap()
            .get(b"Helv")
            .is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空签名域：AcroForm 的 /Sig 域，**没有 /V**（= 还没人签）。
    ///
    /// 「替人签好」和「留个框让人签」是两件事：后者必须有 /AcroForm 登记，
    /// 否则 Acrobat 只把它当普通注释，双击不会进入签名流程。
    #[test]
    fn empty_signature_field_is_an_unsigned_acroform_sig_widget() {
        let dir = tmp_dir("sigfield");
        let src = dir.join("a.pdf");
        build_sample_pdf(&src, 1);
        let res = pdf_add_annotations(
            src.to_string_lossy().to_string(),
            None,
            1,
            vec![PdfAnnotation::SignatureField {
                rect: [100.0, 700.0, 300.0, 740.0],
                field_name: "甲方签名".into(),
            }],
        )
        .unwrap();
        let doc = Document::load(&res.path).unwrap();
        let page = doc.page_iter().next().unwrap();
        let annots_id = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Annots")
            .unwrap()
            .as_reference()
            .unwrap();
        let items = doc.get_object(annots_id).unwrap().as_array().unwrap();
        assert_eq!(items.len(), 1);
        let w = doc.get_dictionary(items[0].as_reference().unwrap()).unwrap();
        assert_eq!(w.get(b"Subtype").unwrap().as_name().unwrap(), b"Widget".as_slice());
        assert_eq!(w.get(b"FT").unwrap().as_name().unwrap(), b"Sig".as_slice());
        // 没人签过 → 不能有 /V（有 /V 就是「已经签了」）。
        assert!(w.get(b"V").is_err());
        // 外观流要在（否则阅读器把它画成什么都没有）。
        assert!(w.get(b"AP").is_ok());

        // catalog /AcroForm /Fields 里登记了它，/SigFlags = 1（文档含签名域）。
        let acro_id = doc
            .catalog()
            .unwrap()
            .get(b"AcroForm")
            .unwrap()
            .as_reference()
            .unwrap();
        let acro = doc.get_dictionary(acro_id).unwrap();
        let fields = acro.get(b"Fields").unwrap().as_array().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(acro.get(b"SigFlags").unwrap().as_i64().unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
