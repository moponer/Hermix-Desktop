//! 图片处理内核（F5 重量模式）。
//!
//! **op 列表模型**：交互预览在前端 Canvas（要 60fps），权威导出在 Rust。
//! 同一个 op 因此有两份实现，用**公式约定**把两边钉死：
//!   - 亮度 = 加法（正数变亮），与 `image::imageops::colorops::brighten` 同义
//!   - 对比度 = 以 127.5 为中心的系数（1.0 = 不变），与 `colorops::contrast` 同义
//!   - 灰度 = Rec.601 亮度（`colorops::grayscale`）
//!   - 饱和度 = **围绕灰度轴插值**（`gray + (c - gray) * factor`）——
//!     `image` 没有饱和度 API（只有 grayscale/invert/contrast/brighten/
//!     huerotate/dither/index_colors），手写；这个式子前后端都能一字不差
//!     地复刻，且 factor=0 退化为灰度、factor=1 不变。
//! Rust 侧用单测锁死这些公式（见本文件末尾）。
//!
//! **原图保护**：任何写盘路径都过 `safe_output_path`——与源文件相同一律
//! 拒绝。默认值是 `<原名>.edited.<扩展名>`，前端传什么都不覆盖原图。
//!
//! **不上共享包**：编辑器只属于 viewer 的重量模式，desktop 不背这份
//! CSS 与 token 契约（`scripts/check-preview-layer.mjs` 的硬约束）。
//! `packages/preview` 在 F5 全程零改动。

use std::path::{Path, PathBuf};

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType as SampleFilter;
use image::{DynamicImage, ExtendedColorType, GenericImageView, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

// ════════════════════════════════════════════════════════════════════
//  op 模型
// ════════════════════════════════════════════════════════════════════

/// 一个图片处理动作。序列化成 JSON 传给前端，前端预览与 Rust 导出
/// 消费同一份 op 列表（前端用 camelCase 字段名）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ImageOp {
    /// 裁剪（原图像素坐标）
    Crop { x: u32, y: u32, w: u32, h: u32 },
    /// 旋转：90 / 180 / 270
    Rotate { degrees: u32 },
    /// 翻转
    Flip { horizontal: bool },
    /// 缩放到指定宽高
    Resize { w: u32, h: u32 },
    /// 灰度（Rec.601）
    Grayscale,
    /// 亮度（加法）
    Brighten { amount: i32 },
    /// 对比度（以 127.5 为中心的系数）
    Contrast { factor: f32 },
    /// 饱和度（围绕灰度轴插值）
    Saturate { factor: f32 },
    /// 高斯模糊
    Blur { sigma: f32 },
    /// 马赛克：block×block 像素块取平均色
    Mosaic { block: u32 },
}

/// 按顺序执行 op 链。几何类 op 之后的坐标类 op（裁剪）语义由前端保证
/// 顺序正确（裁剪永远在最后）。
pub fn apply_ops(mut img: DynamicImage, ops: &[ImageOp]) -> Result<DynamicImage, String> {
    for op in ops {
        img = match op {
            ImageOp::Crop { x, y, w, h } => {
                let (iw, ih) = img.dimensions();
                let x = (*x).min(iw);
                let y = (*y).min(ih);
                let w = (*w).clamp(1, iw - x);
                let h = (*h).clamp(1, ih - y);
                // crop_imm 返回的是借用原图的 SubImage 视图，to_image 物化成
                // 独立缓冲（后续 op 要独占像素）。
                DynamicImage::ImageRgba8(image::imageops::crop_imm(&img, x, y, w, h).to_image())
            }
            ImageOp::Rotate { degrees } => match degrees % 360 {
                90 => img.rotate90(),
                180 => img.rotate180(),
                270 => img.rotate270(),
                _ => img,
            },
            ImageOp::Flip { horizontal } => {
                if *horizontal {
                    img.fliph()
                } else {
                    img.flipv()
                }
            }
            ImageOp::Resize { w, h } => {
                if *w == 0 || *h == 0 {
                    img
                } else {
                    img.resize_exact(*w, *h, SampleFilter::Lanczos3)
                }
            }
            ImageOp::Grayscale => img.grayscale(),
            ImageOp::Brighten { amount } => img.brighten(*amount),
            ImageOp::Contrast { factor } => {
                let buf = image::imageops::colorops::contrast(&img, *factor);
                DynamicImage::ImageRgba8(buf)
            }
            ImageOp::Saturate { factor } => saturate(&img, *factor),
            ImageOp::Blur { sigma } => {
                DynamicImage::ImageRgba8(image::imageops::blur(&img, *sigma))
            }
            ImageOp::Mosaic { block } => mosaic(&img, (*block).max(1)),
        };
    }
    Ok(img)
}

/// 饱和度：围绕灰度轴插值。
///
/// `gray = (max + min) / 2`（HSL 的 L）；`out = gray + (c - gray) * factor`。
/// factor=0 → 全灰度；factor=1 → 不变；factor>1 → 更鲜艳。
/// 刻意不做完整 HSL 往返：公式短到前后端能逐位复刻，且没有色相漂移。
pub fn saturate(img: &DynamicImage, factor: f32) -> DynamicImage {
    let factor = factor.clamp(0.0, 2.0);
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let mut out = RgbaImage::new(w, h);
    for (x, y, px) in rgba.enumerate_pixels() {
        let (r, g, b) = (px[0] as f32, px[1] as f32, px[2] as f32);
        let gray = (r.max(g).max(b) + r.min(g).min(b)) / 2.0;
        let mix = |c: f32| (gray + (c - gray) * factor).clamp(0.0, 255.0) as u8;
        out.put_pixel(
            x,
            y,
            Rgba([mix(r), mix(g), mix(b), px[3]]),
        );
    }
    DynamicImage::ImageRgba8(out)
}

/// 马赛克：缩到 1/block 再按最近邻放大回去（等效于每块取平均色）。
pub fn mosaic(img: &DynamicImage, block: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    let small_w = w.div_ceil(block).max(1);
    let small_h = h.div_ceil(block).max(1);
    let small = img.resize_exact(small_w, small_h, SampleFilter::Triangle);
    small.resize_exact(w, h, SampleFilter::Nearest)
}

// ════════════════════════════════════════════════════════════════════
//  读写
// ════════════════════════════════════════════════════════════════════

/// 解码上限：单边 20000px、分配上限 512MB。防「解压炸弹」把内存打爆。
const MAX_SIDE: u32 = 20_000;
const MAX_ALLOC: u64 = 512 * 1024 * 1024;

fn reader_for(
    path: &Path,
) -> Result<image::ImageReader<std::io::BufReader<std::fs::File>>, String> {
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| format!("无法打开图片: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("无法识别图片格式: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    Ok(reader)
}

pub fn decode(path: &Path) -> Result<DynamicImage, String> {
    reader_for(path)?.decode().map_err(|e| format!("解码失败: {e}"))
}

/// 只读图片头（不解码像素）：宽高 + 格式。
pub fn info(path: &Path) -> Result<(u32, u32, Option<ImageFormat>), String> {
    let reader = reader_for(path)?;
    let fmt = reader.format();
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("读取图片尺寸失败: {e}"))?;
    Ok((w, h, fmt))
}

/// 编码为指定格式。`quality` 只对 JPEG 生效（JPEG 是唯一有损质量参数的
/// 常用格式；PNG 走压缩等级、WebP 走无损——见笔记 Alternatives）。
pub fn encode(
    img: &DynamicImage,
    format: &str,
    quality: Option<u8>,
    ico_sizes: &[u32],
) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    match format {
        "jpeg" | "jpg" => {
            let q = quality.unwrap_or(90).clamp(1, 100);
            JpegEncoder::new_with_quality(&mut out, q)
                .encode_image(&img.to_rgb8())
                .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        }
        "ico" => {
            // ICO 多尺寸：每档用 PNG 帧（Windows Vista+ 原生支持）。
            let sizes: Vec<u32> = if ico_sizes.is_empty() {
                vec![16, 32, 48, 256]
            } else {
                ico_sizes.to_vec()
            };
            let mut frames = Vec::new();
            for s in sizes {
                let side = s.clamp(1, 256);
                // 保持长宽比：resize 是"装进框内"，不是拉伸。
                let resized = img.resize(side, side, SampleFilter::Lanczos3);
                let rgba = resized.to_rgba8();
                let (w, h) = (rgba.width(), rgba.height());
                let frame = IcoFrame::as_png(rgba.as_raw(), w, h, ExtendedColorType::Rgba8)
                    .map_err(|e| format!("ICO 帧构造失败: {e}"))?;
                frames.push(frame);
            }
            IcoEncoder::new(&mut out)
                .encode_images(&frames)
                .map_err(|e| format!("ICO 编码失败: {e}"))?;
        }
        other => {
            let fmt = match other {
                "png" => ImageFormat::Png,
                "gif" => ImageFormat::Gif,
                "webp" => ImageFormat::WebP,
                "bmp" => ImageFormat::Bmp,
                "tiff" | "tif" => ImageFormat::Tiff,
                _ => return Err(format!("不支持的输出格式: {other}")),
            };
            let mut cur = std::io::Cursor::new(&mut out);
            img.write_to(&mut cur, fmt)
                .map_err(|e| format!("{other} 编码失败: {e}"))?;
        }
    }
    Ok(out)
}

// ════════════════════════════════════════════════════════════════════
//  产物路径：原图保护
// ════════════════════════════════════════════════════════════════════

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

/// 同一个文件吗？Windows 上路径大小写不敏感，且 `..` / 相对路径都能绕，
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

// ════════════════════════════════════════════════════════════════════
//  Tauri 命令
// ════════════════════════════════════════════════════════════════════

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// 小写扩展名：png / jpg / …（前端据此选工具集）
    pub extension: String,
}

/// 图片元信息：只读文件头，不解码像素。
#[tauri::command(async)]
pub fn image_info(path: String) -> Result<ImageInfo, String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err(format!("不是文件: {path}"));
    }
    let (w, h, _) = info(&p)?;
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    Ok(ImageInfo {
        path,
        width: w,
        height: h,
        extension: ext,
    })
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImageRaster {
    /// PNG 的 base64（前端 <img src="data:image/png;base64,…">）
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
}

/// 把浏览器解不了的格式（ico / tiff）转成 PNG 给前端显示。
/// `max_side` 给缩略图用（0 / 缺省 = 原尺寸）。
#[tauri::command(async)]
pub fn image_rasterize(path: String, max_side: Option<u32>) -> Result<ImageRaster, String> {
    let p = PathBuf::from(&path);
    let mut img = decode(&p)?;
    if let Some(side) = max_side.filter(|s| *s > 0) {
        let (w, h) = img.dimensions();
        if w.max(h) > side {
            img = img.resize(side, side, SampleFilter::Lanczos3);
        }
    }
    let (w, h) = img.dimensions();
    let mut png: Vec<u8> = Vec::new();
    let mut cur = std::io::Cursor::new(&mut png);
    img.write_to(&mut cur, ImageFormat::Png)
        .map_err(|e| format!("PNG 编码失败: {e}"))?;
    Ok(ImageRaster {
        png_base64: crate::commands::base64_encode(&png),
        width: w,
        height: h,
    })
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImageOutput {
    /// 实际写盘路径
    pub path: String,
    pub bytes: usize,
    pub width: u32,
    pub height: u32,
}

/// 执行 op 链 → 编码 → 落盘。
///
/// **原图保护在两层**：Rust 侧拒绝「产物 == 源文件」，且默认产物名是
/// `<原名>.edited.<扩展名>`。前端传什么都不覆盖原图。
#[tauri::command(async)]
pub fn image_apply(
    path: String,
    ops: Option<Vec<ImageOp>>,
    out_path: Option<String>,
    format: Option<String>,
    quality: Option<u8>,
    ico_sizes: Option<Vec<u32>>,
) -> Result<ImageOutput, String> {
    let src = PathBuf::from(&path);
    if !src.is_file() {
        return Err(format!("不是文件: {path}"));
    }
    let img = decode(&src)?;
    let edited = apply_ops(img, &ops.unwrap_or_default())?;
    let out = safe_output_path(&src, out_path.as_deref())?;
    let fmt = format.unwrap_or_else(|| {
        src.extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_else(|| "png".into())
    });
    let bytes = encode(&edited, &fmt, quality, &ico_sizes.unwrap_or_default())?;
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("无法创建输出目录: {e}"))?;
        }
    }
    std::fs::write(&out, &bytes).map_err(|e| format!("写入失败: {e}"))?;
    let (w, h) = edited.dimensions();
    Ok(ImageOutput {
        path: out.to_string_lossy().to_string(),
        bytes: bytes.len(),
        width: w,
        height: h,
    })
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BatchItem {
    pub src: String,
    pub out: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// 批量处理：纯 Rust 循环，不经 WebView（20+ 张图不该把像素搬来搬去）。
/// 产物一律写到 `out_dir`，文件名 `<原名>.edited.<扩展名>`。
#[tauri::command(async)]
pub fn image_batch(
    paths: Vec<String>,
    ops: Option<Vec<ImageOp>>,
    out_dir: String,
    format: Option<String>,
    quality: Option<u8>,
) -> Result<Vec<BatchItem>, String> {
    let dir = PathBuf::from(&out_dir);
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建输出目录: {e}"))?;
    }
    let ops = ops.unwrap_or_default();
    let fmt = format.unwrap_or_else(|| "png".into());
    let mut out = Vec::new();
    for p in paths {
        let src = PathBuf::from(&p);
        out.push(match batch_one(&src, &ops, &dir, &fmt, quality) {
            Ok(dest) => BatchItem {
                src: p.clone(),
                out: dest.to_string_lossy().to_string(),
                ok: true,
                error: None,
            },
            Err(e) => BatchItem {
                src: p.clone(),
                out: String::new(),
                ok: false,
                error: Some(e),
            },
        });
    }
    Ok(out)
}

/// 批量里的单张：读 → op → 编码 → 写到 out_dir（原文件永不覆盖）。
fn batch_one(
    src: &Path,
    ops: &[ImageOp],
    dir: &Path,
    fmt: &str,
    quality: Option<u8>,
) -> Result<PathBuf, String> {
    let img = decode(src)?;
    let edited = apply_ops(img, ops)?;
    let target = safe_output_path(src, None)?;
    let name = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "edited.png".into());
    let dest = dir.join(name);
    if same_file(&dest, src) {
        return Err("产物与原文件相同，已拒绝".into());
    }
    let bytes = encode(&edited, fmt, quality, &[])?;
    std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// 「另存为」系统对话框：走 Rust 侧插件，前端不装 npm 包（增量保持 0）。
/// 阻塞等待用户选择，因此必须离开主线程。
#[tauri::command(async)]
pub fn image_save_dialog(
    app: tauri::AppHandle,
    default_name: String,
    filters: Option<Vec<(String, Vec<String>)>>,
) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dlg = app.dialog().file().set_file_name(default_name);
    for (name, exts) in filters.unwrap_or_default() {
        let refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
        dlg = dlg.add_filter(name, &refs);
    }
    dlg.blocking_save_file()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().to_string())
}

// ════════════════════════════════════════════════════════════════════
//  单测：把公式钉死（前端按同一约定复刻）
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(r: u8, g: u8, b: u8) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([r, g, b, 255])))
    }

    #[test]
    fn saturate_factor_one_is_identity() {
        let img = solid(200, 100, 50);
        let out = saturate(&img, 1.0).to_rgba8();
        assert_eq!(out.get_pixel(0, 0).0, [200, 100, 50, 255]);
    }

    #[test]
    fn saturate_zero_is_gray() {
        let img = solid(200, 100, 50);
        let out = saturate(&img, 0.0).to_rgba8();
        let px = out.get_pixel(0, 0).0;
        assert_eq!(px[0], px[1]);
        assert_eq!(px[1], px[2]);
        // gray = (max+min)/2 = (200+50)/2 = 125
        assert_eq!(px[0], 125);
    }

    #[test]
    fn saturate_keeps_alpha() {
        let mut img = solid(200, 100, 50);
        if let DynamicImage::ImageRgba8(mut buf) = img {
            buf.put_pixel(0, 0, Rgba([200, 100, 50, 77]));
            img = DynamicImage::ImageRgba8(buf);
        }
        let out = saturate(&img, 1.5).to_rgba8();
        assert_eq!(out.get_pixel(0, 0).0[3], 77);
    }

    #[test]
    fn default_output_path_inserts_suffix() {
        let p = default_output_path(Path::new(r"C:\pics\cat.png"));
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "cat.edited.png");
    }

    #[test]
    fn safe_output_path_rejects_source() {
        let src = PathBuf::from(r"C:\pics\cat.png");
        let err = safe_output_path(&src, Some(r"C:\pics\cat.png")).unwrap_err();
        assert!(err.contains("不能与原文件相同"));
    }

    #[test]
    fn safe_output_path_rejects_same_file_different_case() {
        let src = PathBuf::from(r"C:\pics\cat.png");
        let err = safe_output_path(&src, Some(r"C:\Pics\CAT.PNG")).unwrap_err();
        assert!(err.contains("不能与原文件相同"));
    }

    #[test]
    fn crop_clamps_to_image_bounds() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(10, 8, Rgba([1, 2, 3, 255])));
        let out = apply_ops(
            img,
            &[ImageOp::Crop {
                x: 5,
                y: 5,
                w: 100,
                h: 100,
            }],
        )
        .unwrap();
        assert_eq!(out.dimensions(), (5, 3));
    }

    #[test]
    fn rotate_90_swaps_dimensions() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(10, 8, Rgba([1, 2, 3, 255])));
        let out = apply_ops(img, &[ImageOp::Rotate { degrees: 90 }]).unwrap();
        assert_eq!(out.dimensions(), (8, 10));
    }

    #[test]
    fn ico_encode_produces_header() {
        let img = solid(10, 20, 30);
        let bytes = encode(&img, "ico", None, &[16, 32]).unwrap();
        // ICO 头：reserved(2) type(2)=1 count(2)
        assert_eq!(&bytes[0..2], &[0, 0]);
        assert_eq!(&bytes[2..4], &[1, 0]);
        assert_eq!(&bytes[4..6], &[2, 0]);
    }

    #[test]
    fn jpeg_quality_changes_size() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 64, Rgba([120, 130, 140, 255])));
        let hi = encode(&img, "jpg", Some(95), &[]).unwrap();
        let lo = encode(&img, "jpg", Some(20), &[]).unwrap();
        assert!(lo.len() < hi.len(), "低质量应更小: {} vs {}", lo.len(), hi.len());
    }
}
