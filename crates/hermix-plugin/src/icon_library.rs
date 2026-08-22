//! 内置图标库 + 自动图标生成。
//!
//! 自定义应用创建/打包时，如果没有用户提供图标 (`icon_data`)，就根据
//! 应用的 `id` + `category` **确定性**地生成一个专属图标：
//!
//! - 字形：从内置矢量图标库按分类匹配 (file / image / code / chat / ...)
//! - 颜色：按 `id` 哈希从安全调色板选取 (避开蓝/紫, 符合整体灰白美学)
//!
//! 生成结果为 PNG (给运行时壳 `set_icon` 用) 与 ICO 资源 (给 .exe 内嵌图标
//! 补丁用)。全部纯 Rust 栅格化 (`resvg` + `usvg` + `tiny-skia`)，不依赖任何系统字体。
//!
//! 设计要点 (对抗式审查):
//! - 确定性: 同样的 (id, category) 永远生成同样的图标 → 重新打包不会"变脸"。
//! - 安全退化: 任何一步失败都返回 `Err`, 调用方兜底用默认壳图标,
//!   绝不 panic 或让打包中断。
//! - Layer2 (.exe 补丁) 只改资源节, 不碰代码节; 即便补丁格式出错, .exe 仍可
//!   正常运行 (最坏只是 Explorer 不显示自定义图标), 功能零风险。

use std::path::Path;

// ── 安全调色板 (避开蓝/紫色相 220–290, 保持灰白 + 暖/青系) ──
const PALETTE: &[&str] = &[
    "#E5484D", // red
    "#F76808", // orange
    "#FFB224", // amber
    "#46A758", // green
    "#12A594", // teal
    "#22B8CF", // cyan
    "#E93D82", // rose / pink
    "#F2C94C", // yellow (warm)
];

/// 内置矢量字形库 (24×24 viewBox, 白色描边/填充)。无字体依赖。
/// 每个条目: (分类关键字, SVG 内层标记)。
const GLYPHS: &[(&str, &str)] = &[
    ("document", "<path d=\"M6 2h8l4 4v16H6z\"/><path d=\"M14 2v4h4\"/><path d=\"M9 12h6M9 16h6\"/>"),
    ("image", "<rect x=\"3\" y=\"4\" width=\"18\" height=\"16\" rx=\"2\"/><circle cx=\"8.5\" cy=\"9.5\" r=\"1.5\" fill=\"white\" stroke=\"none\"/><path d=\"M5 18l4-4 3 3 4-5 3 4\"/>"),
    ("music", "<path d=\"M9 18V6l10-2v12\"/><circle cx=\"7\" cy=\"18\" r=\"2.5\" fill=\"white\" stroke=\"none\"/><circle cx=\"17\" cy=\"16\" r=\"2.5\" fill=\"white\" stroke=\"none\"/>"),
    ("video", "<rect x=\"3\" y=\"5\" width=\"18\" height=\"14\" rx=\"2\"/><path d=\"M10 9l5 3-5 3z\" fill=\"white\" stroke=\"none\"/>"),
    ("code", "<path d=\"M9 8l-4 4 4 4M15 8l4 4-4 4\"/>"),
    ("terminal", "<rect x=\"3\" y=\"4\" width=\"18\" height=\"16\" rx=\"2\"/><path d=\"M7 10l3 3-3 3M13 16h4\"/>"),
    ("chat", "<path d=\"M4 5h16v11H9l-5 4z\"/><path d=\"M8 10h8M8 13h5\"/>"),
    ("robot", "<rect x=\"5\" y=\"9\" width=\"14\" height=\"9\" rx=\"2\"/><circle cx=\"9.5\" cy=\"13.5\" r=\"1\" fill=\"white\" stroke=\"none\"/><circle cx=\"14.5\" cy=\"13.5\" r=\"1\" fill=\"white\" stroke=\"none\"/><path d=\"M12 9V6M10 6h4\"/><path d=\"M9 18v2M15 18v2\"/>"),
    ("bolt", "<path d=\"M13 2L4 14h6l-1 8 9-12h-6z\" fill=\"white\" stroke=\"none\"/>"),
    ("globe", "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M3 12h18M12 3c3 3 3 15 0 18M12 3c-3 3-3 15 0 18\"/>"),
    ("calendar", "<rect x=\"3\" y=\"5\" width=\"18\" height=\"16\" rx=\"2\"/><path d=\"M3 9h18M8 3v4M16 3v4\"/>"),
    ("check", "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M8 12l3 3 5-6\" stroke-width=\"2.5\"/>"),
    ("gear", "<circle cx=\"12\" cy=\"12\" r=\"3\"/><path d=\"M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M19 5l-2 2M7 17l-2 2\"/>"),
    ("database", "<ellipse cx=\"12\" cy=\"6\" rx=\"8\" ry=\"3\"/><path d=\"M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3\"/>"),
    ("mail", "<rect x=\"3\" y=\"5\" width=\"18\" height=\"14\" rx=\"2\"/><path d=\"M4 7l8 6 8-6\"/>"),
    ("book", "<path d=\"M5 4h10a3 3 0 013 3v13H8a3 3 0 00-3 3z\"/><path d=\"M5 4v16\"/>"),
    ("calculator", "<rect x=\"5\" y=\"3\" width=\"14\" height=\"18\" rx=\"2\"/><rect x=\"8\" y=\"6\" width=\"8\" height=\"3\" rx=\"1\" fill=\"white\" stroke=\"none\"/><circle cx=\"9\" cy=\"14\" r=\"1\" fill=\"white\" stroke=\"none\"/><circle cx=\"15\" cy=\"14\" r=\"1\" fill=\"white\" stroke=\"none\"/><circle cx=\"9\" cy=\"17\" r=\"1\" fill=\"white\" stroke=\"none\"/><circle cx=\"15\" cy=\"17\" r=\"1\" fill=\"white\" stroke=\"none\"/>"),
    ("translate", "<path d=\"M4 6h6M7 6v13M3 9h8\"/><path d=\"M13 20l4-11 4 11M14.5 16h5\"/>"),
    ("search", "<circle cx=\"11\" cy=\"11\" r=\"6\"/><path d=\"M20 20l-4-4\"/>"),
    ("lock", "<rect x=\"5\" y=\"11\" width=\"14\" height=\"9\" rx=\"2\"/><path d=\"M8 11V8a4 4 0 018 0v3\"/>"),
    ("folder", "<path d=\"M3 7a2 2 0 012-2h3.5l2 2H19a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z\"/>"),
    ("grid", "<rect x=\"4\" y=\"4\" width=\"7\" height=\"7\" rx=\"1.5\" fill=\"white\" stroke=\"none\"/><rect x=\"13\" y=\"4\" width=\"7\" height=\"7\" rx=\"1.5\" fill=\"white\" stroke=\"none\" opacity=\"0.55\"/><rect x=\"4\" y=\"13\" width=\"7\" height=\"7\" rx=\"1.5\" fill=\"white\" stroke=\"none\" opacity=\"0.55\"/><rect x=\"13\" y=\"13\" width=\"7\" height=\"7\" rx=\"1.5\" fill=\"white\" stroke=\"none\"/>"),
];

/// 根据分类选择内置字形。未匹配时回退到通用 `grid`。
fn glyph_for_category(category: &str) -> &'static str {
    let c = category.to_ascii_lowercase();
    for (key, glyph) in GLYPHS {
        if key == &c {
            return glyph;
        }
    }
    // 关键字子串匹配 (容错各种写法)
    let rules: &[(&str, &str)] = &[
        ("file", "document"),
        ("image", "image"),
        ("music", "music"),
        ("audio", "music"),
        ("video", "video"),
        ("media", "video"),
        ("productiv", "check"),
        ("dev", "code"),
        ("code", "code"),
        ("terminal", "terminal"),
        ("chat", "chat"),
        ("social", "chat"),
        ("commun", "chat"),
        ("ai", "robot"),
        ("llm", "robot"),
        ("bot", "robot"),
        ("autom", "bolt"),
        ("web", "globe"),
        ("browser", "globe"),
        ("calendar", "calendar"),
        ("setting", "gear"),
        ("config", "gear"),
        ("data", "database"),
        ("analytic", "database"),
        ("mail", "mail"),
        ("email", "mail"),
        ("book", "book"),
        ("doc", "book"),
        ("calcul", "calculator"),
        ("translat", "translate"),
        ("langu", "translate"),
        ("search", "search"),
        ("secur", "lock"),
        ("lock", "lock"),
        ("priv", "lock"),
        ("folder", "folder"),
        ("file", "folder"),
    ];
    for (sub, key) in rules {
        if c.contains(sub) {
            if let Some((_, g)) = GLYPHS.iter().find(|(k, _)| k == key) {
                return g;
            }
        }
    }
    // 默认: 通用应用网格
    GLYPHS
        .iter()
        .find(|(k, _)| k == &"grid")
        .map(|(_, g)| *g)
        .unwrap_or(GLYPHS[0].1)
}

/// FNV-1a 哈希 → 调色板索引 (确定性)。
fn color_index(id: &str) -> usize {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash as usize) % PALETTE.len()
}

/// 组装完整 SVG (带圆角背景 + 白色字形)。
fn build_svg(inner: &str, color: &str, size: u32) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\">\
            <rect x=\"0\" y=\"0\" width=\"24\" height=\"24\" rx=\"5\" fill=\"{color}\"/>\
            <g stroke=\"white\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" fill=\"none\">\
                {inner}\
            </g>\
        </svg>"
    )
}

/// 将 SVG 栅格化为 PNG 字节 (正方形, 边长由 SVG 内 width/height 决定)。
pub fn render_svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| format!("SVG 解析失败: {e}"))?;
    let sz = tree.size();
    let w = sz.width().ceil() as u32;
    let h = sz.height().ceil() as u32;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or_else(|| "无法创建 pixmap".to_string())?;
    // resvg 0.42 API: render(tree, transform, &mut PixmapMut) -> ()
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|e| format!("PNG 编码失败: {e}"))
}

/// 将任意尺寸 SVG 栅格化为指定边长 `size` 的方形 PNG。
/// 用于任务栏 ICO 多尺寸生成 (图标库 SVG 是 24×24, 需放大到 256)。
/// 内部: 把 SVG 的 width/height 覆写为 size, 再走 render_svg_to_png。
pub fn render_svg_bytes_to_sized_png(svg_bytes: &[u8], size: u32) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(svg_bytes)
        .map_err(|e| format!("SVG 非 UTF-8: {e}"))?;
    // 用 XML 属性替换 width/height (内置库 SVG 是 <svg ... width="24" height="24" ...>)
    // 保守: 只替换 `width="N"` / `height="N"` 到目标尺寸, 不动 viewBox.
    let re = regex_lite::Regex::new(r#"width="\d+(\.\d+)?""#)
        .expect("width regex");
    let mut out = re.replace(text, format!("width=\"{size}\"")).to_string();
    let re_h = regex_lite::Regex::new(r#"height="\d+(\.\d+)?""#)
        .expect("height regex");
    out = re_h.replace(&out, format!("height=\"{size}\"")).to_string();
    render_svg_to_png(&out)
}

/// 生成单个尺寸的 PNG 图标 (默认 256×256)。
pub fn generate_icon_png(name: &str, id: &str, category: &str) -> Result<Vec<u8>, String> {
    let _ = name; // 保留参数, 未来可叠加首字母
    let color = PALETTE[color_index(id)];
    let inner = glyph_for_category(category);
    let svg = build_svg(inner, color, 256);
    render_svg_to_png(&svg)
}

/// 多尺寸 ICO 的构造参数。
const ICO_SIZES: &[u32] = &[16, 32, 48, 256];

/// 生成 .exe 内嵌图标补丁所需的资源数据:
/// - `group`: RT_GROUP_ICON 资源字节 (GRPICONDIR)
/// - `icons`: 每个 RT_ICON 资源 (id, PNG 字节)
///
/// 同时返回用于写盘的 `.ico` 文件字节, 方便调试/校验。
pub fn generate_icon_resources(
    name: &str,
    id: &str,
    category: &str,
) -> Result<(Vec<u8> /*group*/, Vec<(u16, Vec<u8>)> /*icons*/, Vec<u8> /*ico*/), String> {
    let _ = name;
    let color = PALETTE[color_index(id)];
    let inner = glyph_for_category(category);

    let mut pngs: Vec<(u32, Vec<u8>)> = Vec::new();
    for &s in ICO_SIZES {
        let svg = build_svg(inner, color, s);
        let png = render_svg_to_png(&svg)?;
        pngs.push((s, png));
    }

    Ok(assemble_ico_resources(pngs))
}

/// 由多尺寸 PNG 装配 ICO 文件字节 + RT_GROUP_ICON 资源 (GRPICONDIR)。
/// `pngs`: (size, png_bytes)，顺序与 `ICO_SIZES` 一致。供所有图标生成路径复用，
/// 避免 `generate_icon_resources` / `generate_ico_resources_from_png` /
/// `generate_tiled_icon_resources` 三处重复同一段二进制布局逻辑。
fn assemble_ico_resources(
    pngs: Vec<(u32, Vec<u8>)>,
) -> (Vec<u8> /*group*/, Vec<(u16, Vec<u8>)> /*icons*/, Vec<u8> /*ico*/) {
    // ── 构造 ICO 文件 (PNG 直接嵌入, 现代 Windows 支持) ──
    let mut ico: Vec<u8> = Vec::new();
    ico.extend_from_slice(&[0, 0]); // reserved
    ico.extend_from_slice(&[1, 0]); // type = icon
    ico.extend_from_slice(&[(pngs.len() as u8), 0]); // count (low, high)
    let mut image_offset: u32 = 6 + (pngs.len() as u32) * 16;
    for (size, png) in &pngs {
        let w = if *size >= 256 { 0u8 } else { *size as u8 };
        ico.push(w); // width
        ico.push(w); // height
        ico.push(0); // color count
        ico.push(0); // reserved
        ico.extend_from_slice(&[1, 0]); // planes (low, high)
        ico.extend_from_slice(&[32, 0]); // bit count (low, high)
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes()); // bytes in res
        ico.extend_from_slice(&image_offset.to_le_bytes()); // image offset
        image_offset += png.len() as u32;
    }
    for (_, png) in &pngs {
        ico.extend_from_slice(png);
    }

    // ── 构造 RT_GROUP_ICON 资源 (GRPICONDIR, 14 字节/条目) ──
    let mut group: Vec<u8> = Vec::new();
    group.extend_from_slice(&[0, 0]); // reserved
    group.extend_from_slice(&[1, 0]); // type = icon
    group.extend_from_slice(&[(pngs.len() as u8), 0]); // count
    let mut icon_entries: Vec<(u16, Vec<u8>)> = Vec::new();
    for (i, (size, png)) in pngs.iter().enumerate() {
        let id = (i + 1) as u16;
        let w = if *size >= 256 { 0u8 } else { *size as u8 };
        group.push(w); // width
        group.push(w); // height
        group.push(0); // color count
        group.push(0); // reserved
        group.extend_from_slice(&[1, 0]); // planes
        group.extend_from_slice(&[32, 0]); // bit count
        group.extend_from_slice(&(png.len() as u32).to_le_bytes()); // bytes in res
        group.extend_from_slice(&id.to_le_bytes()); // id (替换 ICO 中的 offset)
        icon_entries.push((id, png.clone()));
    }

    (group, icon_entries, ico)
}

/// 按 glyph_key 选内置字形；空字符串或未知 key 时按 category 回退（再回退 grid）。
/// glyph_key 大小写不敏感（内部转小写后与 `GLYPHS` 比对）。
fn resolve_glyph(glyph_key: &str, category: &str) -> &'static str {
    let key = glyph_key.to_ascii_lowercase();
    if !key.is_empty() {
        if let Some((_, g)) = GLYPHS.iter().find(|(k, _)| *k == key.as_str()) {
            return g;
        }
    }
    glyph_for_category(category)
}

/// 生成单个尺寸、带 `tile_color` 圆角背景 + 白色字形的 PNG（边长 `size`）。
/// 用于窗口标题栏 PNG（hx-shell `read_self_icon`）。结果与前端 `tile_color`
/// 方块同源：背景 = manifest 指定的纯色，前景 = 白色字形（无渐变）。
pub fn generate_tiled_icon_png(
    tile_color: &str,
    glyph_key: &str,
    category: &str,
    size: u32,
) -> Result<Vec<u8>, String> {
    let inner = resolve_glyph(glyph_key, category);
    let svg = build_svg(inner, tile_color, size);
    render_svg_to_png(&svg)
}

/// 生成带 `tile_color` 圆角背景 + 白色字形的多尺寸任务栏 ICO 资源 (16/32/48/256)。
/// 与 `generate_icon_resources` 同输出格式，直接喂 `patch_exe_icon` 即可。
/// 当用户显式配置 `tile_color` 时调用，覆盖默认 hash 调色板路径。
pub fn generate_tiled_icon_resources(
    tile_color: &str,
    glyph_key: &str,
    category: &str,
) -> Result<(Vec<u8> /*group*/, Vec<(u16, Vec<u8>)> /*icons*/, Vec<u8> /*ico*/), String> {
    let inner = resolve_glyph(glyph_key, category);
    let mut pngs: Vec<(u32, Vec<u8>)> = Vec::new();
    for &s in ICO_SIZES {
        let svg = build_svg(inner, tile_color, s);
        pngs.push((s, render_svg_to_png(&svg)?));
    }
    Ok(assemble_ico_resources(pngs))
}

/// `RT_GROUP_ICON` 在 windows-sys 0.52 未导出 (它是 MAKEINTRESOURCE(14))。
const RT_GROUP_ICON: windows_sys::core::PCWSTR = 14u16 as *const u16;

/// `RT_RCDATA` 在 windows-sys 0.52 未导出 (它是 MAKEINTRESOURCE(10))。
const RT_RCDATA: windows_sys::core::PCWSTR = 10u16 as *const u16;

/// 自定义应用资源名 (RT_RCDATA 整数 id)。
const RES_HXAPP: u16 = 1000; // html payload
const RES_HXICON: u16 = 1001; // 窗口标题栏图标 PNG
const RES_HXAPPMULTI: u16 = 1002; // 多文件归档 blob (app:// 虚拟协议用)

/// 把自定义应用的 HTML 与图标 PNG 作为 PE 资源 (RT_RCDATA) 嵌入 exe。
///
/// 取代旧的「exe 末尾拼接 payload」方式：尾部附加大块数据正是 Defender
/// `Behavior:Win32/DefenseEvasion.A!ml` 的结构型触发点（被识别为 packer/cryptor
/// 重叠数据）。改为正规资源节后结构合法；若再配代码签名，信誉触发也消失。
///
/// 仅改资源节，不动代码/清单。同 id 重复写入会替换旧值。
/// `BeginUpdateResourceW(path, FALSE)` 保留 loader 原有资源（含 RT_GROUP_ICON 任务栏图标）。
#[cfg(windows)]
pub fn embed_app_payload(
    exe_path: &Path,
    html: &[u8],
    icon_png: Option<&[u8]>,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{BOOL, FALSE, HANDLE, TRUE};
    use windows_sys::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
    };

    let path_w: Vec<u16> = exe_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // 保留现有资源 (FALSE)，只增/改 HXAPP/HXICON。
    let h: HANDLE = unsafe { BeginUpdateResourceW(path_w.as_ptr(), FALSE) };
    if h == 0 {
        return Err(format!(
            "BeginUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }

    let write = |h: HANDLE, name: u16, data: &[u8]| -> Result<(), String> {
        let ok: BOOL = unsafe {
            UpdateResourceW(
                h,
                RT_RCDATA,
                name as *const u16,
                0,
                data.as_ptr() as *const core::ffi::c_void,
                data.len() as u32,
            )
        };
        if ok == FALSE {
            let _ = unsafe { EndUpdateResourceW(h, TRUE) };
            Err(format!(
                "UpdateResourceW(name={name}) 失败: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    };

    if let Err(e) = write(h, RES_HXAPP, html) {
        let _ = unsafe { EndUpdateResourceW(h, TRUE) };
        return Err(e);
    }
    if let Some(icon) = icon_png {
        if let Err(e) = write(h, RES_HXICON, icon) {
            let _ = unsafe { EndUpdateResourceW(h, TRUE) };
            return Err(e);
        }
    }

    let ok: BOOL = unsafe { EndUpdateResourceW(h, FALSE) }; // 提交
    if ok == FALSE {
        return Err(format!(
            "EndUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// 嵌入多文件应用归档 blob（Phase 0，spec §1）。
///
/// 与 `embed_app_payload` 区别：本函数只写归档（1002），不写单 HTML（1000）。
/// 当归档存在时壳优先走 `app://` 多文件加载；壳回退到 `RES_HXAPP`（1000）
/// 是单文件旧路径，保留兼容性。
#[cfg(windows)]
pub fn embed_app_payload_multi(exe_path: &Path, archive: &[u8]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{BOOL, FALSE, HANDLE, TRUE};
    use windows_sys::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
    };

    let path_w: Vec<u16> = exe_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let h: HANDLE = unsafe { BeginUpdateResourceW(path_w.as_ptr(), FALSE) };
    if h == 0 {
        return Err(format!(
            "BeginUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ok: BOOL = unsafe {
        UpdateResourceW(
            h,
            RT_RCDATA,
            RES_HXAPPMULTI as *const u16,
            0,
            archive.as_ptr() as *const core::ffi::c_void,
            archive.len() as u32,
        )
    };
    if ok == FALSE {
        let _ = unsafe { EndUpdateResourceW(h, TRUE) };
        return Err(format!(
            "UpdateResourceW(name={RES_HXAPPMULTI}) 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ok: BOOL = unsafe { EndUpdateResourceW(h, FALSE) };
    if ok == FALSE {
        return Err(format!(
            "EndUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// 同时嵌入单 HTML（1000）和多文件归档（1002）—— 当 app 既有单 HTML 又有 vendor/
/// 资源目录时使用。壳启动按优先级：归档存在走 app://，否则回退 RES_HXAPP。
#[cfg(windows)]
pub fn embed_app_payload_both(
    exe_path: &Path,
    html: &[u8],
    icon_png: Option<&[u8]>,
    archive: Option<&[u8]>,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{BOOL, FALSE, HANDLE, TRUE};
    use windows_sys::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
    };

    let path_w: Vec<u16> = exe_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let h: HANDLE = unsafe { BeginUpdateResourceW(path_w.as_ptr(), FALSE) };
    if h == 0 {
        return Err(format!(
            "BeginUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let write = |name: u16, data: &[u8]| -> Result<(), String> {
        let ok: BOOL = unsafe {
            UpdateResourceW(
                h,
                RT_RCDATA,
                name as *const u16,
                0,
                data.as_ptr() as *const core::ffi::c_void,
                data.len() as u32,
            )
        };
        if ok == FALSE {
            Err(format!(
                "UpdateResourceW(name={name}) 失败: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    };
    if let Err(e) = write(RES_HXAPP, html) {
        let _ = unsafe { EndUpdateResourceW(h, TRUE) };
        return Err(e);
    }
    if let Some(icon) = icon_png {
        if let Err(e) = write(RES_HXICON, icon) {
            let _ = unsafe { EndUpdateResourceW(h, TRUE) };
            return Err(e);
        }
    }
    if let Some(arc) = archive {
        if let Err(e) = write(RES_HXAPPMULTI, arc) {
            let _ = unsafe { EndUpdateResourceW(h, TRUE) };
            return Err(e);
        }
    }
    let ok: BOOL = unsafe { EndUpdateResourceW(h, FALSE) };
    if ok == FALSE {
        return Err(format!(
            "EndUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// 常量供壳端使用（避免在 hx-shell/src/main.rs 中硬编码资源 id）
pub const HX_RES_APP_MULTI: u16 = RES_HXAPPMULTI;

/// 从当前 exe 的 PE 资源 (RT_RCDATA) 读取自定义应用 payload / 图标。
/// 找不到（未打包 / 非 Windows）返回 None，调用方回退到 EOF 尾部或默认值。
#[cfg(windows)]
pub fn read_app_resource(id: u16) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{
        FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
    };

    unsafe {
        let module: HMODULE = GetModuleHandleW(std::ptr::null());
        if module == 0 {
            return None;
        }
        let res = FindResourceW(module, id as *const u16, RT_RCDATA);
        if res == 0 {
            return None;
        }
        let hglob = LoadResource(module, res);
        if hglob.is_null() {
            return None;
        }
        let size = SizeofResource(module, res);
        if size == 0 {
            return None;
        }
        let ptr = LockResource(hglob);
        if ptr.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr as *const u8, size as usize).to_vec())
    }
}

#[cfg(not(windows))]
pub fn read_app_resource(_id: u16) -> Option<Vec<u8>> {
    None
}

/// 给已复制出的 `.exe` 打内嵌图标补丁 (仅 Windows)。
///
/// 只改资源节, 不碰代码。即便失败也只返回 Err, 调用方据此告警,
/// `.exe` 仍可正常运行。
#[cfg(windows)]
pub fn patch_exe_icon(
    exe_path: &Path,
    group: &[u8],
    icons: &[(u16, Vec<u8>)],
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{BOOL, FALSE, HANDLE, TRUE};
    use windows_sys::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::RT_ICON;

    let path_w: Vec<u16> = exe_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // BeginUpdateResourceW(path, bDeleteExistingResources=FALSE): 保留清单等其它资源
    let h: HANDLE = unsafe { BeginUpdateResourceW(path_w.as_ptr(), FALSE) };
    if h == 0 {
        return Err(format!(
            "BeginUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }

    // 写入 RT_GROUP_ICON (id = 1)
    let ok: BOOL = unsafe {
        UpdateResourceW(
            h,
            RT_GROUP_ICON,
            1u16 as *const u16,
            0,
            group.as_ptr() as *const core::ffi::c_void,
            group.len() as u32,
        )
    };
    if ok == FALSE {
        let _ = unsafe { EndUpdateResourceW(h, TRUE) }; // 丢弃改动
        return Err(format!(
            "UpdateResourceW(GROUP_ICON) 失败: {}",
            std::io::Error::last_os_error()
        ));
    }

    // 写入每个 RT_ICON
    for (id, data) in icons {
        let ok: BOOL = unsafe {
            UpdateResourceW(
                h,
                RT_ICON,
                *id as *const u16,
                0,
                data.as_ptr() as *const core::ffi::c_void,
                data.len() as u32,
            )
        };
        if ok == FALSE {
            let _ = unsafe { EndUpdateResourceW(h, TRUE) };
            return Err(format!(
                "UpdateResourceW(ICON id={id}) 失败: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    let ok: BOOL = unsafe { EndUpdateResourceW(h, FALSE) }; // FALSE = 提交改动
    if ok == FALSE {
        return Err(format!(
            "EndUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// 从已存在的 PNG 字节 (`manifest.icon_file` 用户上传的任意尺寸) 生成
/// 嵌入 .exe 资源所需的多尺寸 PNG + ICO/GRPICONDIR 资源数据。
///
/// 与 `generate_icon_resources` 的差异:
/// - 不依赖内置矢量图标库, 直接用用户提供的 PNG
/// - 用 `image` crate 解码 + 重采样到 16/32/48/256 四个尺寸
/// - 输出同样的 `(group, icons, ico)` 格式, 调 `patch_exe_icon` 即可
///
/// 解码失败 / 图片太小时降级为仅保留单帧 256(原图), 减少负担。
///
/// 若 manifest.icon_file 存在则用其替换任务栏图标；否则维持 `generate_icon_resources` 的兜底路径。
#[cfg(windows)]
pub fn generate_ico_resources_from_png(
    png_data: &[u8],
) -> Result<(Vec<u8> /*group*/, Vec<(u16, Vec<u8>)> /*icons*/, Vec<u8> /*ico_file*/), String> {
    use image::imageops::FilterType;

    let img = image::load_from_memory(png_data)
        .map_err(|e| format!("PNG 解码失败 (image crate): {e}"))?;

    // 缩放到 ICO_SIZES 各档 (高质量重采样, FilterType::Lanczos3)
    let mut pngs: Vec<(u32, Vec<u8>)> = Vec::new();
    for &target_size in ICO_SIZES {
        let resized = img.resize_exact(target_size, target_size, FilterType::Lanczos3);
        let mut buf = std::io::Cursor::new(Vec::new());
        resized
            .write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| format!("PNG 编码失败: {e}"))?;
        pngs.push((target_size, buf.into_inner()));
    }

    Ok(assemble_ico_resources(pngs))
}

/// 便捷封装: 读 `manifest.icon_file` (PNG) → 打 exe 资源补丁。
/// 任何一步失败返回 Err, 调用方据此告警; .exe 仍可正常运行。
#[cfg(windows)]
pub fn patch_exe_icon_from_png(exe_path: &Path, png_data: &[u8]) -> Result<(), String> {
    let (group, icons, _ico) = generate_ico_resources_from_png(png_data)?;
    patch_exe_icon(exe_path, &group, &icons)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_signature_ok() {
        let png = generate_icon_png("PDF 转换器", "pdf-converter", "file-tools").unwrap();
        assert_eq!(&png[0..4], &[0x89, 0x50, 0x4E, 0x47]); // PNG magic
    }

    #[test]
    fn ico_signature_ok() {
        let (_, _, ico) = generate_icon_resources("PDF 转换器", "pdf-converter", "file-tools").unwrap();
        // ICONDIR: reserved(0,0) type(1,0) count(N,0)
        assert_eq!(&ico[0..2], &[0, 0]);
        assert_eq!(&ico[2..4], &[1, 0]);
        assert_eq!(ico[4] as usize, ICO_SIZES.len());
    }

    #[test]
    fn determinism() {
        let a = generate_icon_png("App", "same-id", "code").unwrap();
        let b = generate_icon_png("App", "same-id", "code").unwrap();
        assert_eq!(a, b);
        // 不同 id → 不同颜色 (同分类)
        let c = generate_icon_png("App", "other-id", "code").unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn group_resource_layout() {
        let (group, icons, _) = generate_icon_resources("X", "x-id", "media").unwrap();
        // GRPICONDIR: 6 头 + 14*N 条目
        assert_eq!(group.len(), 6 + 14 * icons.len());
        assert_eq!(icons.len(), ICO_SIZES.len());
    }

    #[test]
    fn category_fallback_grid() {
        // 完全无法识别的分类 → 兜底 grid, 但仍能生成有效 PNG
        let png = generate_icon_png("App", "zzz-unknown", "totally-unknown-cat").unwrap();
        assert_eq!(&png[0..4], &[0x89, 0x50, 0x4E, 0x47]);
    }

    #[test]
    fn tiled_icon_png_and_ico() {
        // 自定义 tile_color → 单 PNG 有效, 多尺寸 ICO 头合法
        let png = generate_tiled_icon_png("#1ABC9C", "music", "audio", 256).unwrap();
        assert_eq!(&png[0..4], &[0x89, 0x50, 0x4E, 0x47]);
        let (group, icons, ico) =
            generate_tiled_icon_resources("#1ABC9C", "music", "audio").unwrap();
        assert_eq!(&ico[0..2], &[0, 0]);
        assert_eq!(&ico[2..4], &[1, 0]);
        assert_eq!(ico[4] as usize, ICO_SIZES.len());
        assert_eq!(group.len(), 6 + 14 * icons.len());
        assert_eq!(icons.len(), ICO_SIZES.len());
    }

    #[test]
    fn tiled_glyph_fallback_by_category() {
        // 未知 glyph key → 回退到 category 映射, 仍生成合法 ICO
        let (_g, icons, _i) =
            generate_tiled_icon_resources("#FF6B6B", "not-a-glyph", "file-tools").unwrap();
        assert_eq!(icons.len(), ICO_SIZES.len());
    }
}
