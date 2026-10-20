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
/// 「报告替换了 N 处，其实一处没改」。
fn count_text_hits(doc: &Document, page: u32, from: &str) -> Result<usize, String> {
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
    let content =
        Content::decode(&data).map_err(|e| format!("解析页内容失败: {e}"))?;
    let mut current: Option<&Encoding> = None;
    let mut hits = 0usize;
    for op in &content.operations {
        match op.operator.as_ref() {
            "Tf" => {
                current = op
                    .operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .and_then(|n| encodings.get(n));
            }
            "Tj" => {
                if let (Some(enc), Some(Object::String(bytes, _))) =
                    (current, op.operands.first())
                {
                    if let Ok(text) = Document::decode_text(enc, bytes) {
                        if text == from {
                            hits += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(hits)
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
    let hits = count_text_hits(&doc, page, &from)?;
    if hits == 0 {
        return Err(format!(
            "第 {page} 页没有找到可整串替换的文本「{from}」\
             （该 PDF 可能没有文本层、文本用 TJ 数组显示，或被拆成多段）"
        ));
    }
    doc.replace_text(page, &from, &to)
        .map_err(|e| format!("文本替换失败: {e}"))?;
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("写入 PDF 失败: {e}"))?;
    let mut r = save_bytes(&out, &buf, None)?;
    r.replaced = Some(hits);
    Ok(r)
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
}
