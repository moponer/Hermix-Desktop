// Windows GUI 子系统：不弹控制台窗口，双击 exe 直接出 WebView 窗口
// 必须是文件第一条 — inner attribute 在 doc comment 之前声明
#![windows_subsystem = "windows"]

//! hx-shell — 极简 WebView2 应用壳（基于 wry）
//!
//! 单文件（打包）模式：HTML 与图标作为 PE 资源 (RT_RCDATA) 嵌入 exe，
//! 取代旧版「exe 末尾拼接 payload」（该结构会触发 Defender 误报）。
//! 旧版 EOF 尾部格式仍作兜底：
//!   [icon?][len:8LE][html][len:8LE][MAGIC:HXAPP1]
//!
//! 测试版（dev）模式：启动时传 `--html <path>` 直接加载外部 html 文件，
//! 不读 exe 内 payload、不落单 exe，便于开发期快速预览。
//! 若 html 文件缺失/不可读，显示友好错误页（含原因与运行/打包提示），而非崩溃。
//!
//! 绑定（页面端）：
//!   window.hx.*  — 文档化公开 API（writeFile/readFile/getInfo/exec/...）
//!   window.invoke(cmd, args) → Promise — 底层 RPC（window.hx.* 内部调用）
//!     内部走 window.chrome.webview.postMessage({id, m:cmd, a:args})
//!     Rust 处理后 evaluate_script 回调 window['__hx_resolve_{id}'](result)
//!
//! 已实现命令（Rust 端）：
//!   writeFile(path, content) / readFile(path) / getInfo()
//!   exec({cmd, args}) → {stdout, stderr, exitCode}
//!   appendFile(path, content) / removeFile(path)
//!   readDir(path) → [entries] / stat(path) → {size, isDir, modified}
//!   createDir(path) / removeDir(path)
//!   shell_temp_file({prefix, suffix}) → 临时文件路径字符串
//!   shell_fs_write({path, content})
//!   shell_exec({cmd, args:[...], timeoutSecs}) → {stdout, stderr, exitCode}
//!   shell_fs_delete({path})
//!
//! hxfile:// 自定义协议：
//!   hxfile://localhost/<path> → 直接读取本地文件返回给 WebView
//!   用于 <audio src="hxfile://localhost/C:/Music/song.mp3">
//!   window.hx.fileUrl(path) 把路径转成 hxfile:// URL

use std::borrow::Cow;
use std::path::PathBuf;

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::{Icon, WindowBuilder};
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::WebViewBuilder;

const MAGIC: &[u8] = b"HXAPP1";

pub const BUILD_PROBE: &str = "BUILD_PROBE_2026_DC_V7";

// 自定义应用 PE 资源 (RT_RCDATA 整数 id)，与 hermix_plugin::icon_library 约定一致。
// 取代旧版「exe 末尾拼接 payload」：尾部附加大块数据会触发 Defender
// Behavior:Win32/DefenseEvasion.A!ml 误报（被识别为 packer/cryptor 重叠数据）。
const RT_RCDATA: windows_sys::core::PCWSTR = 10u16 as *const u16;
const RES_HXAPP: u16 = 1000; // html payload
const RES_HXICON: u16 = 1001; // 图标 PNG/SVG
const RES_HXAPPMULTI: u16 = 1002; // 多文件归档 (Phase 0, spec §1) — app:// 虚拟协议用

// ------------------------------------------------------------------
// 调试日志（写 exe 同目录 hx-shell.log）
// ------------------------------------------------------------------
fn hx_log(msg: &str) {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("hx-shell.log");
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                use std::io::Write;
                let _ = writeln!(f, "{}", msg);
            }
        }
    }
}

// ------------------------------------------------------------------
// 从当前 exe 的 PE 资源 (RT_RCDATA) 读取自定义应用 payload / 图标。
// 取代旧版「exe 末尾拼接 payload」：尾部附加大块数据正是 Defender
// Behavior:Win32/DefenseEvasion.A!ml 的结构型触发点。打包器用
// hermix_plugin::icon_library::embed_app_payload 写入同名资源。
// ------------------------------------------------------------------
#[cfg(windows)]
fn read_pe_resource(id: u16) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{
        FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
    };
    unsafe {
        let module: HMODULE = GetModuleHandleW(std::ptr::null());
        if module == 0 {
            hx_log(&format!("read_pe_resource({id}): GetModuleHandleW=0"));
            return None;
        }
        let res = FindResourceW(module, id as *const u16, RT_RCDATA);
        hx_log(&format!(
            "read_pe_resource({id}): FindResourceW={res}, err={}",
            std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
        ));
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
fn read_pe_resource(_id: u16) -> Option<Vec<u8>> {
    None
}

// ------------------------------------------------------------------
// 自读尾部 payload（单文件模式，PE 资源未命中时的兜底）
// 格式：
//   旧版：[html][html_len:8LE][MAGIC]
//   新版(有图标)：[icon_png|svg][icon_len:8LE][html][html_len:8LE][MAGIC]
// 通过 icon_len 段是否存在判断。
// ------------------------------------------------------------------
fn read_self_payload() -> Option<Vec<u8>> {
    // 优先 PE 资源 (RT_RCDATA 1000)，结构合法，不触发 Defender 尾部探测
    if let Some(v) = read_pe_resource(RES_HXAPP) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    // 兜底：旧版 exe 末尾拼接 payload
    let self_exe = std::env::current_exe().ok()?;
    let bytes = std::fs::read(&self_exe).ok()?;
    if bytes.len() < 14 {
        return None;
    }
    let tail = &bytes[bytes.len() - 6..];
    if tail != MAGIC {
        return None;
    }
    let len_start = bytes.len() - 6 - 8;
    let html_len = u64::from_le_bytes(bytes[len_start..len_start + 8].try_into().ok()?) as usize;
    if html_len == 0 || len_start < html_len {
        return None;
    }
    let html_bytes = &bytes[len_start - html_len..len_start];
    Some(html_bytes.to_vec())
}

/// 读取尾部 icon 段（如有）。返回 RGBA bytes + width + height。
/// 支持两种格式：
/// - PNG 段（`\x89PNG` 开头）→ 内置极简 PNG 解码器
/// - SVG 段（`<svg` / `<?xml` 开头）→ resvg 渲染为 RGBA
fn read_self_icon() -> Option<(Vec<u8>, u32, u32)> {
    // 优先 PE 资源 (RT_RCDATA 1001)，打包器写入的图标 PNG/SVG
    if let Some(icon_bytes) = read_pe_resource(RES_HXICON) {
        if !icon_bytes.is_empty() {
            let looks_svg = icon_bytes
                .iter()
                .skip_while(|b| b.is_ascii_whitespace())
                .take(1)
                .next()
                .copied()
                == Some(b'<');
            return if looks_svg {
                decode_svg_to_rgba(std::str::from_utf8(&icon_bytes).ok()?)
            } else {
                decode_png_to_rgba(&icon_bytes)
            };
        }
    }
    // 兜底：旧版 exe 末尾拼接 icon 段
    let self_exe = std::env::current_exe().ok()?;
    let bytes = std::fs::read(&self_exe).ok()?;
    if bytes.len() < 22 { return None; } // 至少 2*8 + 6 = 22
    let tail = &bytes[bytes.len() - 6..];
    if tail != MAGIC { return None; }
    let html_len_start = bytes.len() - 6 - 8;
    let html_len = u64::from_le_bytes(bytes[html_len_start..html_len_start + 8].try_into().ok()?) as usize;
    if html_len == 0 || html_len_start < html_len { return None; }
    // html 前面是否还有 icon 段？
    if html_len_start < html_len + 8 { return None; }
    let icon_len_start = html_len_start - html_len - 8;
    let icon_len = u64::from_le_bytes(bytes[icon_len_start..icon_len_start + 8].try_into().ok()?) as usize;
    if icon_len == 0 || icon_len > 1024 * 1024 || icon_len_start < icon_len { return None; }
    let icon_bytes = &bytes[icon_len_start - icon_len..icon_len_start];
    // SVG 检测：以 '<' 开头（<svg / <?xml），否则当 PNG 处理
    let looks_svg = icon_bytes
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .take(1)
        .next()
        .copied()
        == Some(b'<');
    if looks_svg {
        decode_svg_to_rgba(std::str::from_utf8(icon_bytes).ok()?)
    } else {
        decode_png_to_rgba(icon_bytes)
    }
}

/// 用 resvg 把 SVG 渲染成 RGBA。输出尺寸按 SVG viewBox 实际大小（图标通常 24×24 太小，
/// 放大到 256×256 作为窗口图标）。失败返回 None。
fn decode_svg_to_rgba(svg: &str) -> Option<(Vec<u8>, u32, u32)> {
    use resvg::tiny_skia::{self, Pixmap};
    use resvg::usvg;
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &opt).ok()?;
    let size = tree.size();
    let (w, h) = (size.width().max(1.0), size.height().max(1.0));
    // Lucide 默认 24×24 → 放大到 256 窗口图标，保持宽高比
    let scale = 256.0 / w.max(h);
    let target_w = (w * scale).round().max(1.0) as u32;
    let target_h = (h * scale).round().max(1.0) as u32;
    let mut pixmap = Pixmap::new(target_w, target_h)?;
    let ts = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, ts, &mut pixmap.as_mut());
    Some((pixmap.data().to_vec(), target_w, target_h))
}

/// 极简 PNG 解码（仅支持非隔行 8-bit RGBA/RGB 真彩图）。
/// 委托给 `image` crate 太重；自实现：解析 IHDR → 找 IDAT → zlib 解压 → 还原 filter → 合并通道。
/// 若失败返回 None，壳降级为无图标。
fn decode_png_to_rgba(png: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    // PNG signature
    if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" { return None; }
    let mut pos = 8;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut bit_depth = 0u8;
    let mut color_type = 0u8;
    let mut idat = Vec::new();
    while pos + 8 <= png.len() {
        let len = u32::from_be_bytes(png[pos..pos + 4].try_into().ok()?) as usize;
        let kind = &png[pos + 4..pos + 8];
        let data = &png[pos + 8..pos + 8 + len];
        match kind {
            b"IHDR" if data.len() >= 13 => {
                width = u32::from_be_bytes(data[0..4].try_into().ok()?);
                height = u32::from_be_bytes(data[4..8].try_into().ok()?);
                bit_depth = data[8];
                color_type = data[9];
                if bit_depth != 8 { return None; }
                // 2=RGB, 6=RGBA
                if color_type != 2 && color_type != 6 { return None; }
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos += 8 + len + 4; // len + type + data + crc
    }
    if width == 0 || height == 0 || idat.is_empty() { return None; }

    // zlib decompress（PNG IDAT 是 zlib stream）
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut raw = Vec::with_capacity(idat.len() * 2);
    if ZlibDecoder::new(&idat[..]).read_to_end(&mut raw).is_err() { return None; }

    let channels = if color_type == 6 { 4 } else { 3 };
    let stride = width as usize * channels;
    let mut pixels = vec![0u8; height as usize * stride];
    let mut prev_row = vec![0u8; stride];

    for y in 0..height as usize {
        let row_start = y * (stride + 1); // +1 for filter byte
        if row_start + stride > raw.len() { return None; }
        let filter = raw[row_start];
        let row = &raw[row_start + 1..row_start + 1 + stride];
        let out = &mut pixels[y * stride..(y + 1) * stride];
        match filter {
            0 => out.copy_from_slice(row),
            1 => { // Sub
                for x in 0..stride {
                    let left = if x >= channels { out[x - channels] } else { 0 };
                    out[x] = row[x].wrapping_add(left);
                }
            }
            2 => { // Up
                for x in 0..stride {
                    out[x] = row[x].wrapping_add(prev_row[x]);
                }
            }
            3 => { // Average
                for x in 0..stride {
                    let left = if x >= channels { out[x - channels] } else { 0 };
                    out[x] = row[x].wrapping_add(((left as u16 + prev_row[x] as u16) / 2) as u8);
                }
            }
            4 => { // Paeth
                for x in 0..stride {
                    let a = if x >= channels { out[x - channels] } else { 0 };
                    let b = prev_row[x];
                    let c = if x >= channels { prev_row[x - channels] } else { 0 };
                    let p = a as i16 + b as i16 - c as i16;
                    let pa = (p - a as i16).abs();
                    let pb = (p - b as i16).abs();
                    let pc = (p - c as i16).abs();
                    let pr = if pa <= pb && pa <= pc { a }
                             else if pb <= pc { b }
                             else { c };
                    out[x] = row[x].wrapping_add(pr);
                }
            }
            _ => return None,
        }
        prev_row.copy_from_slice(out);
    }

    // RGB → RGBA
    let rgba = if channels == 4 {
        pixels
    } else {
        let mut out = Vec::with_capacity(pixels.len() * 4 / 3);
        for chunk in pixels.chunks(3) {
            out.extend_from_slice(chunk);
            out.push(255);
        }
        out
    };
    Some((rgba, width, height))
}

// ------------------------------------------------------------------
// 内置绑定（JSON-RPC over WebView2 postMessage）
// ------------------------------------------------------------------

// 应用基准目录：writeFile/readFile 等相对路径文件操作解析到此目录，而非
// 进程 cwd。由主程序 spawn 时通过 --app-dir 显式传入应用目录，兜底为
// --html 父目录 / 当前 exe 父目录。目的：彻底隔离自定义应用对主项目
// 源码目录的写入——否则状态文件会落到 src-tauri/，被 Tauri 文件监听捕获
// 后重建主窗口，导致在途 IPC 回调失联（"Couldn't find callback id"）。
static APP_BASE_DIR: std::sync::OnceLock<Option<std::path::PathBuf>> =
    std::sync::OnceLock::new();

fn app_base_dir() -> Option<&'static std::path::Path> {
    APP_BASE_DIR.get().and_then(|o| o.as_deref())
}

/// 应用层路径 → 绝对路径：绝对路径原样返回；相对路径解析到应用基准目录
/// （APP_BASE_DIR），无法获取基准目录时退回 cwd（保证旧打包产物不崩）。
fn resolve_app_path(path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Some(base) = app_base_dir() {
        return base.join(p);
    }
    p.to_path_buf()
}

/// writeFile(path, content) — 数组形式参数
fn hx_write_file(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    let content = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::write(resolve_app_path(path), content) {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// readFile(path) — 数组形式参数（文本 only）
fn hx_read_file(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::read_to_string(resolve_app_path(path)) {
        Ok(content) => serde_json::json!({"ok": true, "data": content}),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// readFileBytes(path) — 二进制读取，返回 base64 字符串（页面端 atob 解码）
fn hx_read_file_bytes(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::read(resolve_app_path(path)) {
        Ok(bytes) => serde_json::json!({"ok": true, "data": base64_encode(&bytes)}),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// writeFileBytes(path, base64) — 二进制写入
fn hx_write_file_bytes(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    let b64 = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match base64_decode(b64) {
        Some(bytes) => match std::fs::write(resolve_app_path(path), &bytes) {
            Ok(()) => serde_json::json!({"ok": true}),
            Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
        },
        None => serde_json::json!({"ok": false, "err": "invalid base64"}),
    }
}

/// getInfo() — 壳信息 + 完整 API 清单
/// LLM 调用此方法发现可用 API，不依赖可能过时的 SKILL.md 文档
fn hx_get_info() -> serde_json::Value {
    serde_json::json!({"ok": true, "data": {
        "shell": "hx-shell",
        "engine": "wry/WebView2",
        "single_file": read_self_payload().is_some(),
        "version": BUILD_PROBE,
        // ── API 清单：LLM 必须只使用这里列出的方法 ──
        "apis": {
            "writeFile":     {"params": ["path: string", "content: string"], "returns": "{ok, err?}"},
            "readFile":      {"params": ["path: string"], "returns": "{ok, data: string, err?}", "note": "文本文件 only"},
            "readFileBytes": {"params": ["path: string"], "returns": "{ok, data: base64, err?}", "note": "二进制文件 → base64"},
            "writeFileBytes":{"params": ["path: string", "base64: string"], "returns": "{ok, err?}", "note": "二进制写入"},
            "appendFile":     {"params": ["path: string", "content: string"], "returns": "{ok, err?}"},
            "removeFile":     {"params": ["path: string"], "returns": "{ok, err?}"},
            "readDir":        {"params": ["path: string"], "returns": "{ok, data: string[], err?}", "note": "纯名字数组"},
            "readDirDetailed":{"params": ["path: string"], "returns": "{ok, data: [{name, isDir, isFile, size, modified}], err?}", "note": "带元信息"},
            "stat":           {"params": ["path: string"], "returns": "{ok, data: {size, isDir, isFile, modified}, err?}"},
            "createDir":      {"params": ["path: string"], "returns": "{ok, err?}"},
            "removeDir":      {"params": ["path: string"], "returns": "{ok, err?}"},
            "exec":           {"params": ["{cmd: string, args: string[]}"], "returns": "{ok, data: {stdout, stderr, exitCode}, err?}"},
            "openDialog":     {"params": ["{title?, directory?, filters?}"], "returns": "{ok, data: string | null, err?}", "note": "原生文件选择"},
            "saveDialog":     {"params": ["{title?, defaultName?, filters?}"], "returns": "{ok, data: string | null, err?}", "note": "原生保存对话框"},
            "getInfo":        {"params": [], "returns": "{ok, data: this_object}"},
            "fileUrl":        {"params": ["path: string"], "returns": "string", "note": "path → hxfile:// URL for <audio>/<img>/<video>"},
            "showMessage":    {"params": ["msg: string"], "returns": "{ok, data}"},
        },
        // ── 窗口控制（无装饰窗口标题栏）──
        "win": {
            "note": "无装饰窗口 — 必须在 HTML 中自行实现标题栏。调用 window.hx.win.* 控制窗口。",
            "apis": {
                "minimize":       {"params": [], "returns": "{ok}"},
                "toggleMaximize": {"params": [], "returns": "{ok, data: {maximized: bool}}"},
                "close":          {"params": [], "returns": "never (exits process)"},
                "setTitle":       {"params": ["title: string"], "returns": "{ok}"},
                "startDrag":      {"params": [], "returns": "{ok, err?}", "note": "鼠标按下标题栏时调用，启动窗口拖动"},
                "isMaximized":    {"params": [], "returns": "{ok, data: bool}"},
            },
        },
        // ── 自定义协议 ──
        "protocols": {
            "hxfile": "hxfile://localhost/<path> — WebView 直接读取本地文件（音频/图片/视频），支持 seek"
        },
        // ── 明确不支持的 API（别猜，别试）──
        "notSupported": ["clipboardGet", "clipboardSet", "getEnv", "fetch"],
    }})
}

/// appendFile(path, content) — 追加写入
fn hx_append_file(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    let content = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    use std::io::Write;
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => match f.write_all(content.as_bytes()) {
            Ok(()) => serde_json::json!({"ok": true}),
            Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
        },
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// removeFile(path) — 删除文件
fn hx_remove_file(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::remove_file(path) {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// readDir(path) — 列出目录内容（纯名字 string[]）
fn hx_read_dir(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::read_dir(path) {
        Ok(entries) => {
            let names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            serde_json::json!({"ok": true, "data": names})
        }
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// readDirDetailed(path) — 列出目录内容，带每个条目的元信息
fn hx_read_dir_detailed(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::read_dir(path) {
        Ok(entries) => {
            let items: Vec<serde_json::Value> = entries
                .filter_map(|e| e.ok())
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let md = e.metadata().ok();
                    let (size, is_dir, is_file, modified) = md
                        .map(|m| {
                            (
                                m.len(),
                                m.is_dir(),
                                m.is_file(),
                                m.modified()
                                    .ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                            )
                        })
                        .unwrap_or((0, false, false, 0));
                    serde_json::json!({
                        "name": name,
                        "size": size,
                        "isDir": is_dir,
                        "isFile": is_file,
                        "modified": modified,
                    })
                })
                .collect();
            serde_json::json!({"ok": true, "data": items})
        }
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// stat(path) — 文件元信息
fn hx_stat(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::metadata(path) {
        Ok(md) => {
            let modified = md.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            serde_json::json!({"ok": true, "data": {
                "size": md.len(),
                "isDir": md.is_dir(),
                "isFile": md.is_file(),
                "modified": modified,
            }})
        }
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// createDir(path) — 创建目录
fn hx_create_dir(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::create_dir_all(path) {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// removeDir(path) — 删除目录（递归）
fn hx_remove_dir(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

// ------------------------------------------------------------------
// exec — 执行外部命令（对象形式参数 {cmd, args}）
// 含 program-not-found fallback：常见 Windows 程序 (powershell, cmd)
// 在 PATH 不全或被剥离的子进程中找不到时，自动回退到 System32 全路径
// ------------------------------------------------------------------
fn hx_exec(args: &serde_json::Value) -> serde_json::Value {
    // 兼容两种形式：对象 {cmd, args} 或 数组 ["cmd", "arg1", ...]
    let (cmd, argv) = if args.is_array() {
        let arr = args.as_array().unwrap();
        let cmd = arr.get(0).and_then(|v| v.as_str()).unwrap_or("");
        let argv: Vec<String> = arr.iter().skip(1)
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        (cmd, argv)
    } else {
        let cmd = args.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        let argv: Vec<String> = args.get("args")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        (cmd, argv)
    };

    if cmd.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty cmd"});
    }

    // Program-not-found fallback（Windows 常见路径）
    let resolved_cmd = resolve_program_path(cmd);
    let resolved_cmd_ref = resolved_cmd.as_deref().unwrap_or(cmd);

    let output = std::process::Command::new(resolved_cmd_ref).args(&argv).output();
    match output {
        Ok(o) => serde_json::json!({
            "ok": true,
            "data": {
                "stdout": String::from_utf8_lossy(&o.stdout).to_string(),
                "stderr": String::from_utf8_lossy(&o.stderr).to_string(),
                "exitCode": o.status.code().unwrap_or(-1),
            }
        }),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

/// 在 PATH 找不到时，自动尝试 Windows System32 常见程序路径
/// 返回 None 表示无 fallback（继续用原 cmd 让 Command 自身报错）
fn resolve_program_path(cmd: &str) -> Option<String> {
    // 已经是绝对路径直接返回 None（让 Command 自己处理）
    if cmd.contains(':') || cmd.starts_with('\\') || cmd.starts_with('/') {
        return None;
    }
    // 只对短命令名 fallback（避免误判带参数的长字符串）
    let name = cmd.split_whitespace().next().unwrap_or(cmd);
    if name.contains('\\') || name.contains('/') || name.contains('.') {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let candidates: &[&str] = match lower.as_str() {
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => &[
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        ],
        "cmd" | "cmd.exe" => &[
            r"C:\Windows\System32\cmd.exe",
        ],
        "where" | "where.exe" => &[
            r"C:\Windows\System32\where.exe",
        ],
        _ => return None,
    };
    for path in candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }
    None
}

// ------------------------------------------------------------------
// openDialog / saveDialog — 原生文件/目录选择（rfd）
// ------------------------------------------------------------------

/// openDialog({title?, directory?, multi?, filters?}) — 文件/目录选择
/// - directory: true → 选目录；否则选文件
/// - multi: true → 多选（返回 string[]）
/// - filters: [{"name": "图片", "extensions": ["png","jpg"]}]
fn hx_open_dialog(args: &serde_json::Value) -> serde_json::Value {
    let mut dialog = rfd::FileDialog::new();
    if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
        dialog = dialog.set_title(title);
    }
    if let Some(dir) = args.get("directory").and_then(|v| v.as_str()) {
        dialog = dialog.set_directory(dir);
    }
    if let Some(filters) = args.get("filters").and_then(|v| v.as_array()) {
        for f in filters {
            if let (Some(name), Some(exts)) = (
                f.get("name").and_then(|v| v.as_str()),
                f.get("extensions").and_then(|v| v.as_array()),
            ) {
                let exts: Vec<&str> = exts.iter().filter_map(|v| v.as_str()).collect();
                if !exts.is_empty() {
                    dialog = dialog.add_filter(name, &exts);
                }
            }
        }
    }
    let is_dir = args.get("directory").and_then(|v| v.as_bool()).unwrap_or(false);
    let multi = args.get("multi").and_then(|v| v.as_bool()).unwrap_or(false);
    if is_dir {
        match dialog.pick_folder() {
            Some(p) => serde_json::json!({"ok": true, "data": p.to_string_lossy().to_string()}),
            None => serde_json::json!({"ok": true, "data": null}),
        }
    } else if multi {
        match dialog.pick_files() {
            Some(files) => {
                let paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().to_string()).collect();
                serde_json::json!({"ok": true, "data": paths})
            }
            None => serde_json::json!({"ok": true, "data": null}),
        }
    } else {
        match dialog.pick_file() {
            Some(p) => serde_json::json!({"ok": true, "data": p.to_string_lossy().to_string()}),
            None => serde_json::json!({"ok": true, "data": null}),
        }
    }
}

/// saveDialog({title?, defaultName?, filters?}) — 保存对话框
fn hx_save_dialog(args: &serde_json::Value) -> serde_json::Value {
    let mut dialog = rfd::FileDialog::new();
    if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
        dialog = dialog.set_title(title);
    }
    if let Some(name) = args.get("defaultName").and_then(|v| v.as_str()) {
        dialog = dialog.set_file_name(name);
    }
    if let Some(filters) = args.get("filters").and_then(|v| v.as_array()) {
        for f in filters {
            if let (Some(name), Some(exts)) = (
                f.get("name").and_then(|v| v.as_str()),
                f.get("extensions").and_then(|v| v.as_array()),
            ) {
                let exts: Vec<&str> = exts.iter().filter_map(|v| v.as_str()).collect();
                if !exts.is_empty() {
                    dialog = dialog.add_filter(name, &exts);
                }
            }
        }
    }
    match dialog.save_file() {
        Some(p) => serde_json::json!({"ok": true, "data": p.to_string_lossy().to_string()}),
        None => serde_json::json!({"ok": true, "data": null}),
    }
}

// ------------------------------------------------------------------
// 窗口控制 — win_* 命令（无装饰窗口的标题栏 API）
// ------------------------------------------------------------------

/// 窗口控制处理器：minimize / maximize / toggleMaximize / close / setTitle / startDrag / isMaximized
/// 所有方法接收 Arc<Window> 引用，操作同一个窗口实例。
fn hx_win_control(
    cmd: &str,
    args: &serde_json::Value,
    window: &std::sync::Arc<tao::window::Window>,
) -> serde_json::Value {
    match cmd {
        "minimize" => {
            window.set_minimized(true);
            serde_json::json!({"ok": true})
        }
        "maximize" => {
            window.set_maximized(true);
            serde_json::json!({"ok": true})
        }
        "unmaximize" => {
            window.set_maximized(false);
            serde_json::json!({"ok": true})
        }
        "toggleMaximize" => {
            let maximized = window.is_maximized();
            window.set_maximized(!maximized);
            serde_json::json!({"ok": true, "data": {"maximized": !maximized}})
        }
        "close" => {
            // 单窗口 GUI 应用：直接退出进程
            std::process::exit(0);
        }
        "setTitle" => {
            let title = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            window.set_title(title);
            serde_json::json!({"ok": true})
        }
        "startDrag" => {
            match window.drag_window() {
                Ok(()) => serde_json::json!({"ok": true}),
                Err(e) => serde_json::json!({"ok": false, "err": format!("{e:?}")}),
            }
        }
        "isMaximized" => {
            serde_json::json!({"ok": true, "data": window.is_maximized()})
        }
        _ => serde_json::json!({"ok": false, "err": format!("unknown win command: {cmd}")}),
    }
}

// ------------------------------------------------------------------
// 磁盘清理器所需的 shell_* 命令（对象形式参数，保留向后兼容）
// ------------------------------------------------------------------

fn hx_shell_temp_file(args: &serde_json::Value) -> serde_json::Value {
    let prefix = args.get("prefix").and_then(|v| v.as_str()).unwrap_or("hx_");
    let suffix = args.get("suffix").and_then(|v| v.as_str()).unwrap_or("");
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("{}{}{}", prefix, n, suffix));
    serde_json::json!({"ok": true, "data": path.to_string_lossy().to_string()})
}

fn hx_shell_fs_write(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    match std::fs::write(resolve_app_path(path), content) {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

fn hx_shell_fs_delete(args: &serde_json::Value) -> serde_json::Value {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return serde_json::json!({"ok": false, "err": "empty path"});
    }
    let r = std::fs::remove_file(resolve_app_path(path))
        .or_else(|_| std::fs::remove_dir_all(resolve_app_path(path)));
    match r {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(e) => serde_json::json!({"ok": false, "err": e.to_string()}),
    }
}

fn hx_shell_exec(args: &serde_json::Value) -> serde_json::Value {
    hx_exec(args)
}

// ------------------------------------------------------------------
// base64 辅助（无外部依赖，手写最小实现）
// ------------------------------------------------------------------
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (b0 as u32) << 16 | (b1 as u32) << 8 | b2 as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for c in s.chars() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            '=' => break,
            _ if c.is_whitespace() => continue,
            _ => return None,
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

// ------------------------------------------------------------------
// hxfile:// 自定义协议 — 让 WebView 直接访问本地文件
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// 多文件应用归档（Phase 0 / spec §1）：从 PE 资源 1002 读归档 blob，
// 然后由 `app://` 协议按 relpath 解包给 WebView。
// 格式与 hermix-plugin/src/app_archive.rs 镜像定义（壳不引 hermix-plugin
// 以避免循环依赖 — 格式稳定、自包含，用本地复制解析器）。
// ------------------------------------------------------------------

/// 归档 blob 解析结果。blob 保留全字节，entries 是相对路径→(offset, length) 表。
struct ParsedArchive {
    blob: Vec<u8>,
    entries: Vec<(String, usize, usize)>, // (relpath, offset, length)
}

fn parse_app_archive(blob: &[u8]) -> Option<ParsedArchive> {
    if blob.len() < 4 + 1 + 4 + 32 {
        return None;
    }
    if &blob[0..4] != b"HXAM" {
        return None;
    }
    if blob[4] != 0x01 {
        return None;
    }
    let count = u32::from_le_bytes(blob[5..9].try_into().ok()?) as usize;
    let mut pos = 9usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 2 > blob.len() {
            return None;
        }
        let rp_len = u16::from_le_bytes(blob[pos..pos + 2].try_into().ok()?) as usize;
        pos += 2;
        if pos + rp_len > blob.len() {
            return None;
        }
        let relpath = std::str::from_utf8(&blob[pos..pos + rp_len])
            .ok()?
            .to_string();
        pos += rp_len;
        if pos + 4 > blob.len() {
            return None;
        }
        let data_len = u32::from_le_bytes(blob[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4;
        if pos + data_len > blob.len() {
            return None;
        }
        entries.push((relpath, pos, data_len));
        pos += data_len;
    }
    Some(ParsedArchive {
        blob: blob.to_vec(),
        entries,
    })
}

/// 自读归档 blob（PE 资源 1002）。
fn read_self_archive() -> Option<Vec<u8>> {
    let v = read_pe_resource(RES_HXAPPMULTI);
    hx_log(&format!("read_self_archive(1002): {:?}", v.as_ref().map(|b| b.len())));
    let v = v?;
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 进程内静态归档缓存：startup 时一次性 parse，所有 `app://` 请求查表。
/// 无锁（启动期单线程，运行时只读）。
static APP_ARCHIVE: std::sync::OnceLock<Option<ParsedArchive>> = std::sync::OnceLock::new();

/// Phase 6 dev 模式：startup 时若传 --app-dir，写入此 PathBuf，
/// `app://` 请求查表时优先 PE 归档 fallback 磁盘，相对外联全活。
static APP_DIR_FOR_DEV: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

fn get_app_archive() -> Option<&'static ParsedArchive> {
    APP_ARCHIVE.get_or_init(|| {
        let bytes = read_self_archive()?;
        match parse_app_archive(&bytes) {
            Some(a) => {
                hx_log(&format!("app archive parsed: {} entries", a.entries.len()));
                Some(a)
            }
            None => {
                hx_log("app archive parse failed (bad magic / truncated)");
                None
            }
        }
    }).as_ref()
}

fn get_app_dir_for_dev() -> Option<&'static std::path::PathBuf> {
    APP_DIR_FOR_DEV.get_or_init(|| None).as_ref()
}

/// 简单 MIME 类型推断（基于文件扩展名）
fn mime_type(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") { "text/html" }
    else if lower.ends_with(".css") { "text/css" }
    else if lower.ends_with(".js") { "text/javascript" }
    else if lower.ends_with(".json") { "application/json" }
    else if lower.ends_with(".txt") { "text/plain" }
    // 音频
    else if lower.ends_with(".mp3") { "audio/mpeg" }
    else if lower.ends_with(".flac") { "audio/flac" }
    else if lower.ends_with(".wav") { "audio/wav" }
    else if lower.ends_with(".ogg") { "audio/ogg" }
    else if lower.ends_with(".m4a") { "audio/mp4" }
    else if lower.ends_with(".aac") { "audio/aac" }
    else if lower.ends_with(".wma") { "audio/x-ms-wma" }
    // 图片
    else if lower.ends_with(".png") { "image/png" }
    else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") { "image/jpeg" }
    else if lower.ends_with(".gif") { "image/gif" }
    else if lower.ends_with(".svg") { "image/svg+xml" }
    else if lower.ends_with(".webp") { "image/webp" }
    else if lower.ends_with(".bmp") { "image/bmp" }
    // 视频
    else if lower.ends_with(".mp4") { "video/mp4" }
    else if lower.ends_with(".webm") { "video/webm" }
    else if lower.ends_with(".avi") { "video/x-msvideo" }
    else if lower.ends_with(".mkv") { "video/x-matroska" }
    else { "application/octet-stream" }
}

/// URL percent-decode（处理 %20 等编码字符）
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i+1] as char).to_digit(16);
            let lo = (bytes[i+2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// app:// 协议处理器（Phase 1 / spec §1）：从归档里按 relpath 解包返回给 WebView。
/// Phase 6 dev 模式：先查 PE 归档，miss 再查磁盘 `--app-dir`（dev runner 直接 host app_dir，
///                跳过 .dev.html 生成、与 prod 共用 app:// handler）。
/// URL 形式：app://localhost/index.html、app://localhost/vendor/pdf.js 等。
/// 找不到则 404（WebView 自动 fallback 到 about:blank，但浏览器/前端可拦截此错误）。
fn app_protocol_handler(_webview_id: &str, request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let uri_path = request.uri().path();
    hx_log(&format!("app:// {}", uri_path));
    // uri_path = "/index.html" 或 "/vendor/foo.js"
    let relpath = if let Some(stripped) = uri_path.strip_prefix('/') {
        stripped
    } else {
        uri_path
    };
    // 1) prod 模式：PE 归档优先
    if let Some(archive) = get_app_archive() {
        for (rp, offset, length) in &archive.entries {
            if rp == relpath {
                let start = *offset;
                let end = start + *length;
                let body = archive.blob.get(start..end).unwrap_or(&[]);
                let mime = mime_type(relpath);
                return http::Response::builder()
                    .header(CONTENT_TYPE, mime)
                    .header("Access-Control-Allow-Origin", "*")
                    .body(Cow::Owned(body.to_vec()))
                    .unwrap();
            }
        }
    }
    // 2) dev 模式：磁盘 host（spec §8.7 Phase 6）
    if let Some(app_dir) = get_app_dir_for_dev() {
        let fs_path = app_dir.join(relpath);
        // 防越界：relpath 含 ".." 拒绝（防 ../etc/passwd）
        if relpath.contains("..") {
            return http::Response::builder()
                .status(403)
                .header(CONTENT_TYPE, "text/plain")
                .body(Cow::Owned(b"forbidden: relative path with ..".to_vec()))
                .unwrap();
        }
        if let Ok(bytes) = std::fs::read(&fs_path) {
            let mime = mime_type(relpath);
            return http::Response::builder()
                .header(CONTENT_TYPE, mime)
                .header("Access-Control-Allow-Origin", "*")
                .body(Cow::Owned(bytes))
                .unwrap();
        }
    }
    hx_log(&format!("app:// 404: {relpath}"));
    http::Response::builder()
        .status(404)
        .header(CONTENT_TYPE, "text/plain")
        .body(Cow::Owned(format!("not found: {relpath}").into_bytes()))
        .unwrap()
}

/// hxfile:// 协议处理器：从 URL 路径读取本地文件
fn hxfile_handler(_webview_id: &str, request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let uri_path = request.uri().path();
    hx_log(&format!("hxfile: {}", uri_path));

    // URL 格式: hxfile://localhost/C:/Music/song.mp3
    // uri_path = /C:/Music/song.mp3 → 去掉前导 / 得到文件路径
    let file_path = if let Some(stripped) = uri_path.strip_prefix('/') {
        percent_decode(stripped)
    } else {
        percent_decode(uri_path)
    };

    match std::fs::read(&file_path) {
        Ok(content) => {
            let mime = mime_type(&file_path);
            http::Response::builder()
                .header(CONTENT_TYPE, mime)
                .header("Access-Control-Allow-Origin", "*")
                .body(Cow::Owned(content))
                .unwrap()
        }
        Err(e) => {
            hx_log(&format!("hxfile error: {} → {}", file_path, e));
            http::Response::builder()
                .header(CONTENT_TYPE, "text/plain")
                .status(404)
                .body(Cow::Owned(format!("File not found: {file_path}").into_bytes()))
                .unwrap()
        }
    }
}

// ------------------------------------------------------------------
// 入口
// ------------------------------------------------------------------
fn main() -> wry::Result<()> {
    // shell 预置前端能力层 (spec §7) — IIFE 已自我执行, 直接 include_str 嵌入 init script
    // DEV_APP_URL_HELPER_v2 — 改 runtime.js 时同步改此 token 让 cargo 必然重 include_str!
    const HERMIX_RUNTIME_JS: &str = include_str!("../resources/hermix-runtime.js");
    let _rt_dev_marker: &str = "DEV_APP_URL_HELPER_v2";

    // 原生 JS init script (raw 字符串, 不参与 format! 以避免 brace 冲突)
    const INIT_SCRIPT_BASE: &str = r#"
            (function(){
              try {
                window.__hx_id = 0;

                // ── 底层 RPC：window.invoke(cmd, args) → Promise ──
                window.invoke = function(cmd, args) {
                  return new Promise(function(resolve) {
                    var id = 'hx' + (++window.__hx_id);
                    window['__hx_resolve_' + id] = function(r){
                      delete window['__hx_resolve_' + id];
                      resolve(r);
                    };
                    try {
                      window.chrome.webview.postMessage(JSON.stringify({ id: id, m: cmd, a: args || {} }));
                    } catch (e) { resolve({ok: false, err: String(e)}); }
                  });
                };

                // ── 文档化公开 API：window.hx.* ──
                window.hx = {
                  writeFile: function(path, content) { return window.invoke('writeFile', [path, content]); },
                  readFile: function(path) { return window.invoke('readFile', [path]); },
                  readFileBytes: function(path) { return window.invoke('readFileBytes', [path]); },
                  writeFileBytes: function(path, b64) { return window.invoke('writeFileBytes', [path, b64]); },
                  appendFile: function(path, content) { return window.invoke('appendFile', [path, content]); },
                  removeFile: function(path) { return window.invoke('removeFile', [path]); },
                  readDir: function(path) { return window.invoke('readDir', [path]); },
                  readDirDetailed: function(path) { return window.invoke('readDirDetailed', [path]); },
                  stat: function(path) { return window.invoke('stat', [path]); },
                  createDir: function(path) { return window.invoke('createDir', [path]); },
                  removeDir: function(path) { return window.invoke('removeDir', [path]); },
                  openDialog: function(opts) { return window.invoke('openDialog', opts || {}); },
                  saveDialog: function(opts) { return window.invoke('saveDialog', opts || {}); },
                  exec: function(cmd, args) { return window.invoke('exec', {cmd: cmd, args: args || []}); },
                  getInfo: function() { return window.invoke('getInfo', {}); },
                  fileUrl: function(path) {
                    if (!path) return '';
                    var p = String(path).replace(/\\\\/g, '/');
                    if (p.charAt(0) === '/') p = p.slice(1);
                    p = encodeURIComponent(p).replace(/%2F/gi, '/');
                    return 'hxfile://localhost/' + p;
                  },
                  showMessage: function(msg) { return Promise.resolve({ok: true, data: String(msg || '')}); },
                  win: {
                    minimize: function() { return window.invoke('win_minimize', {}); },
                    maximize: function() { return window.invoke('win_maximize', {}); },
                    toggleMaximize: function() { return window.invoke('win_toggleMaximize', {}); },
                    close: function() { return window.invoke('win_close', {}); },
                    setTitle: function(title) { return window.invoke('win_setTitle', [title || '']); },
                    startDrag: function() { return window.invoke('win_startDrag', {}); },
                    isMaximized: function() { return window.invoke('win_isMaximized', {}); },
                  },
                };
              } catch(e) {
                if (typeof console !== 'undefined') console.error('hx init error:', e);
              }
            })();
        "#;

    // --html <path>：直接加载外部 html 文件（测试版 / dev 模式），不读 exe 内 payload、
    let args: Vec<String> = std::env::args().collect();
    let html_arg: Option<String> = args
        .iter()
        .position(|a| a == "--html")
        .and_then(|i| args.get(i + 1).cloned());
    // --html-url <url>：dev mode 加载远程 HTML（用 std::net::TcpStream 拉 HTTP/1.0，避开 ureq/reqwest 依赖）。
    //   关键场景：让 WebView2 在 http(s) origin 下加载，能用 localStorage / fetch(app://) / 异步资源。
    //   --html 用 file:// 或 with_html 都落到 about:blank，无 origin，WebView2 拒绝 localStorage + fetch(app://)。
    let html_url_arg: Option<String> = args
        .iter()
        .position(|a| a == "--html-url")
        .and_then(|i| args.get(i + 1).cloned());

    // 应用基准目录：优先 --app-dir（主程序 spawn 时显式传入应用目录），
    // 兜底 --html 父目录 / 当前 exe 父目录。相对路径文件操作解析到此目录，
    // 不再依赖进程 cwd（tauri dev 时父 cwd 是 src-tauri/）。
    let app_dir_arg: Option<String> = args
        .iter()
        .position(|a| a == "--app-dir")
        .and_then(|i| args.get(i + 1).cloned());
    // ── app 唯一 id ── Hermix.storage/Hermix.sqlite 的命名空间隔离凭据
    // 由主程序 spawn 时传 --app-id <id>；缺省（dev 模式）走 "anonymous"。
    let app_id_arg: Option<String> = args
        .iter()
        .position(|a| a == "--app-id")
        .and_then(|i| args.get(i + 1).cloned());
    let app_dir_path: Option<std::path::PathBuf> = app_dir_arg.clone().map(std::path::PathBuf::from);
    let base_dir: Option<std::path::PathBuf> = app_dir_path
        .clone()
        .or_else(|| {
            html_arg
                .as_ref()
                .and_then(|h| std::path::Path::new(h).parent().map(|p| p.to_path_buf()))
        })
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        })
        .filter(|p| !p.as_os_str().is_empty());
    let _ = APP_BASE_DIR.set(base_dir.clone());
    // Phase 6 dev 模式：把 --app-dir 显式值写入 APP_DIR_FOR_DEV，
    // app:// 协议处理器会把磁盘文件按 relpath 喂 WebView（与 prod 共用 handler）。
    let _ = APP_DIR_FOR_DEV.set(app_dir_path.clone());
    if let Some(ref d) = app_dir_path {
        hx_log(&format!("dev mode: app_dir_for_dev = {}", d.display()));
    }

    // Panic hook：GUI 子系统下没有 stderr，用 MessageBox 显示崩溃信息
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("hx-shell 崩溃\n\n{}", info);
        hx_log(&format!("PANIC: {msg}"));
        #[cfg(windows)]
        unsafe {
            use std::ffi::CString;
            if let Ok(c_msg) = CString::new(msg.clone()) {
                use std::os::raw::{c_int, c_uint};
                extern "system" {
                    fn MessageBoxA(
                        hwnd: *mut std::os::raw::c_void,
                        lp_text: *const std::os::raw::c_char,
                        lp_caption: *const std::os::raw::c_char,
                        u_type: c_uint,
                    ) -> c_int;
                }
                let title = CString::new("hx-shell Error").unwrap();
                MessageBoxA(
                    std::ptr::null_mut(),
                    c_msg.as_ptr(),
                    title.as_ptr(),
                    0x10, // MB_ICONERROR
                );
            }
        }
    }));

    hx_log(&format!("=== hx-shell start {} ===", BUILD_PROBE));
    hx_log(&format!("exe: {:?}", std::env::current_exe().ok()));
    let event_loop = EventLoop::new();
    hx_log("event_loop created");
    // 解析尾部 icon 段（若有），用作窗口标题栏 + 任务栏图标
    let window_icon = read_self_icon().and_then(|(rgba, w, h)| {
        match Icon::from_rgba(rgba, w, h) {
            Ok(ic) => { hx_log(&format!("window icon loaded: {}x{}", w, h)); Some(ic) }
            Err(e) => { hx_log(&format!("Icon::from_rgba failed: {e:?}")); None }
        }
    });
    // 自定义应用窗口启动在屏幕左侧/左上角, 不居中。
    // 根因: tao WindowBuilder 未设置位置, Windows 默认 (0,0)。
    // 解法: 计算主显示器中心, 创建窗口时直接居中 (tao 0.35 API):
    //   - MonitorHandle: size() -> PhysicalSize<u32>, position() -> PhysicalPosition<i32>,
    //     scale_factor() -> f64 (无 work_area 方法 — 用整屏中心, 任务栏占位可接受)
    //   - WindowBuilder::with_position(Position) (0.35 没有 with_outer_position)
    // 取不到 monitor (理论不会发生, Windows 必有主显示器) → 退回默认位置, 不 panic。
    const WIN_W_LOGICAL: f64 = 900.0;
    const WIN_H_LOGICAL: f64 = 640.0;
    let start_position: Option<tao::dpi::Position> = event_loop
        .primary_monitor()
        .map(|m| {
            let scale = m.scale_factor();
            let origin_x = m.position().x as f64 / scale; // 逻辑
            let origin_y = m.position().y as f64 / scale;
            let area_w = m.size().width as f64 / scale;
            let area_h = m.size().height as f64 / scale;
            let x = origin_x + (area_w - WIN_W_LOGICAL) / 2.0;
            let y = origin_y + (area_h - WIN_H_LOGICAL) / 2.0;
            hx_log(&format!(
                "centering window at logical ({x:.1}, {y:.1}), monitor={area_w:.0}x{area_h:.0} scale={scale}"
            ));
            tao::dpi::Position::Logical(tao::dpi::LogicalPosition::new(x, y))
        });
    let mut win_builder = WindowBuilder::new()
        .with_title("Hermix App")
        .with_inner_size(tao::dpi::LogicalSize::new(WIN_W_LOGICAL, WIN_H_LOGICAL))
        .with_window_icon(window_icon)
        .with_decorations(false); // 无原生标题栏 — 应用自行实现标题栏 + 窗口控制按钮
    if let Some(pos) = start_position {
        win_builder = win_builder.with_position(pos);
    }
    let window = win_builder
        .build(&event_loop)
        .unwrap_or_else(|e| {
            let msg = format!("窗口创建失败: {e}");
            hx_log(&msg);
            panic!("{msg}");
        });
    hx_log("window created (decorations=false, centered)");

    // Window 包 Arc 共享给 IPC handler — 无装饰窗口需要 IPC 控制窗口
    let window = std::sync::Arc::new(window);
    let win_ipc = window.clone();
    hx_log("window wrapped in Arc for IPC");

    let payload = read_self_payload();
    hx_log(&format!("payload: {} bytes", payload.as_ref().map(|p| p.len()).unwrap_or(0)));
    let res_dir = exe_dir().join("resources");
    hx_log(&format!("res_dir: {}", res_dir.display()));

    let webview_ref: std::sync::Arc<std::sync::Mutex<Option<wry::WebView>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let wv_ref = webview_ref.clone();

    // ── Phase 3: 准备 init script（基础 + appId + 嵌入 hermix-runtime.js）──
    // 必须在 builder 之前算完；用 String::from + `+` 拼接而非 format!，
    // 因为 INIT_SCRIPT_BASE 与 HERMIX_RUNTIME_JS 内部有大量 `{`/`}` JS 语法，
    // format!() 会扫 brace 把 JS 代码当作 placeholder 报错。
    let init_app_id_prefix = format!(
        "\n;window.Hermix = window.Hermix || {{}};\nwindow.Hermix.__appId = {app_id_json};\n",
        app_id_json = serde_json::to_string(
            &app_id_arg.clone().unwrap_or_else(|| "anonymous".into())
        )
        .unwrap_or_else(|_| "\"anonymous\"".into()),
    );
    let init_full: String = String::from(INIT_SCRIPT_BASE)
        + &init_app_id_prefix
        + HERMIX_RUNTIME_JS;
    hx_log(&format!(
        "init script: {} bytes (runtime {} bytes)",
        init_full.len(),
        HERMIX_RUNTIME_JS.len()
    ));

    let builder = WebViewBuilder::new()
        // hxfile:// 自定义协议 — 让 <audio>/<img>/<video> 能直接访问本地文件
        .with_custom_protocol("hxfile".into(), hxfile_handler)
        // app:// 自定义协议 — Phase 1：从多文件归档按 relpath 解包喂给 WebView
        .with_custom_protocol("app".into(), app_protocol_handler)
        .with_ipc_handler(move |req| {
            let body = req.body().to_string();
            hx_log(&format!("IPC: {}", body));
            let v: serde_json::Value = serde_json::from_str(&body)
                .unwrap_or_else(|_| serde_json::json!({"id": null, "m": "_noop", "a": []}));
            let id = v["id"].as_str().unwrap_or("").to_string();
            let m = v["m"].as_str().unwrap_or("");
            let a = v["a"].clone();
            let result = match m {
                // 文档化公开 API
                "writeFile" => hx_write_file(&a),
                "readFile" => hx_read_file(&a),
                "readFileBytes" => hx_read_file_bytes(&a),
                "writeFileBytes" => hx_write_file_bytes(&a),
                "getInfo" => hx_get_info(),
                "exec" => hx_exec(&a),
                "appendFile" => hx_append_file(&a),
                "removeFile" => hx_remove_file(&a),
                "readDir" => hx_read_dir(&a),
                "readDirDetailed" => hx_read_dir_detailed(&a),
                "stat" => hx_stat(&a),
                "createDir" => hx_create_dir(&a),
                "removeDir" => hx_remove_dir(&a),
                "openDialog" => hx_open_dialog(&a),
                "saveDialog" => hx_save_dialog(&a),
                // 窗口控制（无装饰窗口）
                "win_minimize" => hx_win_control("minimize", &a, &win_ipc),
                "win_maximize" => hx_win_control("maximize", &a, &win_ipc),
                "win_unmaximize" => hx_win_control("unmaximize", &a, &win_ipc),
                "win_toggleMaximize" => hx_win_control("toggleMaximize", &a, &win_ipc),
                "win_close" => hx_win_control("close", &a, &win_ipc),
                "win_setTitle" => hx_win_control("setTitle", &a, &win_ipc),
                "win_startDrag" => hx_win_control("startDrag", &a, &win_ipc),
                "win_isMaximized" => hx_win_control("isMaximized", &a, &win_ipc),
                // 磁盘清理器专用（向后兼容）
                "shell_temp_file" => hx_shell_temp_file(&a),
                "shell_fs_write" => hx_shell_fs_write(&a),
                "shell_exec" => hx_shell_exec(&a),
                "shell_fs_delete" => hx_shell_fs_delete(&a),
                _ => serde_json::json!({"ok": false, "err": format!("unknown: {m}")}),
            };
            // 回调 JS 端 resolver，传入完整 result 对象
            let js = format!("window['__hx_resolve_{}']({})", id, result);
            if let Ok(w) = wv_ref.lock() {
                if let Some(wv) = w.as_ref() {
                    let _ = wv.evaluate_script(&js);
                }
            }
        })
        // init_script — Phase 3 拼好的完整 init 串（含 runtime.js）
        .with_initialization_script(&init_full);

    // --html <path>：直接加载外部 html 文件（测试版 / dev 模式），不读 exe 内 payload、

    // 页面 HTML：单文件模式用尾部 payload，开发模式读外部 html 文件
    // --html-url <http(s) URL> 时改用 with_url(url) 让 WebView2 在真实 origin 下加载，
    // 启用 localStorage / fetch(app://) / 跨源等。html_arg / payload / fallback 都用 with_html (about:blank)。
    let html_source: String = if let Some(p) = &html_arg {
        // 测试版 / dev 模式：直接加载外部 html 文件，不落单 exe、不拼 payload
        hx_log(&format!("dev mode (--html): loading {}", p));
        match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) => {
                hx_log(&format!("--html load failed: {e}"));
                dev_error_html(&format!("--html 加载失败: {e}"), p)
            }
        }
    } else if let Some(p) = payload {
        String::from_utf8_lossy(&p).to_string()
    } else {
        let fp = res_dir.join("index.html");
        hx_log(&format!("dev mode: loading {}", fp.display()));
        match std::fs::read_to_string(&fp) {
            Ok(s) => s,
            Err(e) => {
                hx_log(&format!("index.html load failed: {e}"));
                dev_error_html(&format!("index.html load failed: {e}"), &fp.display().to_string())
            }
        }
    };
    hx_log(&format!("html source: {} bytes", html_source.len()));

    // Phase 1：若多文件归档存在，优先走 app:// 加载（多文件）
    // --html-url 提供远程 origin 时也走 with_url（带 origin，能 fetch app:// 协议）。
    let use_app_protocol = read_self_archive().is_some() && html_arg.is_none() && html_url_arg.is_none();
    let webview = if use_app_protocol {
        // 用 app:// 协议加载，index.html 由归档提供，其它资源按相对解析
        hx_log("loading via app:// (multi-file archive present)");
        builder
            .with_url("app://localhost/index.html")
            .build(&*window)?
    } else if let Some(url) = &html_url_arg {
        // dev 模式远程 origin：让 WebView2 直接加载 http(s) URL，能用 localStorage / fetch(app://)。
        hx_log(&format!("loading via with_url (--html-url): {}", url));
        builder
            .with_url(url.clone())
            .build(&*window)?
    } else {
        hx_log(&format!(
            "loading via with_html ({})",
            if html_arg.is_some() { "dev mode" } else { "single-file fallback" }
        ));
        builder
            .with_html(html_source.clone())
            .build(&*window)?
    };
    hx_log("webview build OK");
    webview_ref.lock().unwrap().replace(webview);
    hx_log("enter event_loop");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(500));
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit
        }
    });
}

// HTML 转义（防 XSS / 路径特殊字符破坏页面结构）
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// dev 测试版加载失败时显示的友好错误页。
/// 灰系配色、无蓝紫，符合 Hermix 设计铁律。reason/hint_path 经 html_escape 防注入。
/// 用结构化 format! 占位符生成，绝不把 Rust 控制流嵌进 HTML 字符串（那是上次损坏的根因）。
fn dev_error_html(reason: &str, hint_path: &str) -> String {
    let reason_esc = html_escape(reason);
    let path_esc = html_escape(hint_path);
    format!(
        r#"<!DOCTYPE html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>加载失败 · Hermix App</title>
<style>
  * {{ box-sizing: border-box; }}
  html,body {{ margin:0; min-height:100%; background:#0f0f12; color:#d0d0dc;
    font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }}
  .wrap {{ min-height:100vh; display:flex; align-items:center; justify-content:center; padding:32px; }}
  .card {{ max-width:560px; width:100%; background:#1c1c20; border:1px solid #2e2e34;
    border-radius:14px; padding:28px 32px; }}
  .dot {{ width:10px; height:10px; border-radius:50%; background:#e5534b; display:inline-block; vertical-align:middle; margin-right:10px; }}
  h1 {{ font-size:18px; margin:0 0 16px; font-weight:600; }}
  .reason {{ background:#232328; border:1px solid #2e2e34; border-radius:8px;
    padding:12px 14px; font-size:13px; color:#e0e0e8; word-break:break-all; white-space:pre-wrap; margin:0 0 14px; }}
  .path {{ font-size:12px; color:#9a9aa6; margin:0 0 18px; word-break:break-all; }}
  .tip {{ font-size:13px; line-height:1.75; color:#b8b8c4; margin:0; }}
  .tip code {{ background:#232328; padding:2px 6px; border-radius:4px; color:#d0d0dc; font-size:12px; }}
  strong {{ color:#fff; font-weight:600; }}
</style></head>
<body><div class="wrap"><div class="card">
  <h1><span class="dot"></span>应用加载失败</h1>
  <p class="reason">{}</p>
  <p class="path">目标文件：{}</p>
  <p class="tip">
    这是 <strong>dev 测试版</strong>。请确认：<br>
    1. 文件路径是否正确、文件是否存在且可读；<br>
    2. 若用 <code>hx-shell.exe --html &lt;path&gt;</code> 运行，<code>&lt;path&gt;</code> 应指向生成的 <code>dist/&lt;id&gt;.dev.html</code>；<br>
    3. 需要分发时，请用 <code>custom_app_manager.package</code> 打包成单文件 exe。
  </p>
</div></div></body></html>"#,
        reason_esc, path_esc
    )
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}
