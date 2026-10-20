//! Hermes Viewer 命令（command 必须放独立模块：lib.rs 根定义会与
//! generate_handler 宏冲突 E0255 —— 同 installer 教训）。
//!
//! 边界（决策笔记 2026-10-09-standalone-file-viewer）：
//!   md/text/code 直接渲染；图片 base64；PDF 走 pdf.js；
//!   xlsx/docx/pptx/csv 走前端引擎，bytes 透传；
//!   html 走前端沙箱 iframe（srcDoc + 桥脚本转系统浏览器）；
//!   老二进制 office 走「用系统程序打开」兜底。
//!
//! 文件读取上限（按类型给）：10MB 文本/图片/PDF，**5MB html**（沙箱里要整页
//! 渲染，体积大的页面没意义），50MB xlsx/xls/docx/csv，
//! 100MB pptx（演示文稿常有大量图片）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// 单实例互斥体名 + 第二实例投递路径的 loopback 端口（与 BBS 3901 错开）。
pub const SINGLETON_MUTEX: &str = "HermixViewerSingleton";
pub const SINGLETON_PORT: u16 = 3907;

/// 文件读取上限：按类型给——10MB 给文本/图片/PDF（看一眼够用），
/// 5MB 给 html，50MB 给 xlsx/xls/docx/csv，100MB 给 pptx。超过一律建议走系统程序。
const TEXT_BYTE_LIMIT: u64 = 10 * 1024 * 1024;
const HTML_BYTE_LIMIT: u64 = 5 * 1024 * 1024;
const XLSX_BYTE_LIMIT: u64 = 50 * 1024 * 1024;
const DOCX_BYTE_LIMIT: u64 = 50 * 1024 * 1024;
const XLS_BYTE_LIMIT: u64 = 50 * 1024 * 1024;
const CSV_BYTE_LIMIT: u64 = 50 * 1024 * 1024;
const PPTX_BYTE_LIMIT: u64 = 100 * 1024 * 1024;

#[derive(Default)]
pub struct ViewerState {
    pub pending: Mutex<Vec<PathBuf>>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// RFC 3339 字符串（如 "2024-03-15T08:01:43Z"），前端直接用。
    pub mtime: Option<String>,
    pub extension: String,
    pub hidden: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    pub path: String,
    pub exists: bool,
    pub is_dir: bool,
    pub is_file: bool,
    pub size: u64,
    pub mtime: Option<String>,
    pub hidden: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DriveRoot {
    pub letter: String,
    pub path: String,
    pub label: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileContent {
    /// Markdown / 纯文本 / 代码：前端按 variant 选渲染器。
    ///
    /// 载荷是**原始字节**（base64），不是字符串：Windows 上记事本存的
    /// txt 常是 UTF-16 LE，国内老软件导出的 csv 常是 GBK，Rust 侧一刀
    /// 按 UTF-8 解会把内容变成乱码。解码规则统一放在共享前端层
    /// （packages/preview/src/lib/textDecode.ts），那里才拿得到 BOM。
    Text {
        name: String,
        /// md | text | code
        variant: String,
        data_base64: String,
    },
    /// 图片（png/jpg/gif/webp/svg/bmp/ico）：data URL 前端直渲染。
    Image {
        name: String,
        mime: String,
        data_base64: String,
    },
    /// PDF：base64 交给前端 pdf.js（旧的 `<embed>` 弃用，WebView2 行为不一致）。
    Pdf { name: String, data_base64: String },
    /// 任意二进制（xlsx/xlsm/xls/docx/pptx/html/csv/tsv）：bytes 透传给前端引擎。
    /// 单一载荷种类，前端按 `extension` 分派。
    Bytes {
        name: String,
        extension: String,
        data_base64: String,
    },
    /// 二期不渲染的类型：走「用系统程序打开」兜底。
    Unsupported {
        name: String,
        extension: String,
        reason: String,
        /// 这是个可执行文件（exe / msi / …）——不可预览，但能直接跑。
        /// 前端据此显示「立即运行」；判定权在后端（见 EXECUTABLE_EXT），
        /// 前端不自己猜扩展名，两边各一份表必然分叉。
        executable: bool,
    },
}

/// Windows 上双击即执行 / 启动的二进制扩展名。
///
/// 这张表同时是**信任边界**：`viewer_run_executable` 只放行这些扩展名，
/// 不能被用来启动任意文件（.bat / .ps1 需要 shell 语义，另走系统程序打开）。
const EXECUTABLE_EXT: &[&str] = &["exe", "msi", "com", "scr", "pif"];

fn ext_of(p: &str) -> String {
    PathBuf::from(p)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// 扩展名 → 读取类别。
///
/// **这张表必须与共享预览层（`packages/preview`）的分派表对齐**：
/// 前端 `detectLanguage` / `isMarkdownExt` 认得扩展名，后端这里再判
/// unsupported，就会出现「前端已选好渲染器，后端却不给字节」——
/// 用户看到的是「不支持该类型」，但能力其实早就有了。
/// 2026-10-20 实测：两张表分叉导致 57 个扩展名白拿不到内容。
///
/// 类别：text（按文本读）/ code（文本，按代码高亮）/ image / pdf /
/// bytes（前端自己解析）/ legacy_office（老二进制，走系统程序）。
fn classify(ext: &str) -> &'static str {
    match ext {
        // html/htm 是**文本**：共享预览层的 htmlLoader 走 readText 取源码，
        // 塞进沙箱 iframe 渲染。此前把 html 归到 "bytes"，Rust 于是回
        // FileContent::Bytes，前端 readText 判成 isBinary → HTML 预览
        // 永远报"二进制文件，不是可渲染的网页"。
        "md" | "markdown" | "mdx" | "txt" | "log" | "text" | "json" | "json5" | "jsonp"
        | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env" | "properties" | "props"
        | "csv" | "tsv" | "html" | "htm" | "ipynb" | "lock" | "map" | "srt" | "vtt"
        | "m3u" | "m3u8" | "npmrc" | "editorconfig" | "gitignore" => "text",
        "rs" | "py" | "pyw" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "c" | "cpp"
        | "cxx" | "cc" | "h" | "hpp" | "hh" | "ino" | "cs" | "java" | "go" | "sh" | "bash"
        | "zsh" | "fish" | "bat" | "cmd" | "ps1" | "psm1" | "sql" | "css" | "scss" | "less"
        | "vue" | "svelte" | "razor" | "rb" | "php" | "lua" | "swift" | "kt" | "kts"
        | "dart" | "scala" | "sc" | "ex" | "exs" | "hs" | "nim" | "zig" | "jl" | "r" | "sol"
        | "proto" | "xml" | "pug" | "jade" | "hbs" | "graphql" | "gql" | "adoc" | "asc"
        | "rst" | "wiki" | "tex" | "dockerfile" | "makefile" | "make" | "cmake" | "nginx"
        | "vim" | "viml" | "vimrc" | "diff" | "patch" | "http" | "regex" | "mermaid" | "git" => "code",
        // tif/tiff：浏览器解不了，但 Rust 的 image 内核能解（编辑器用得上），
        // 预览侧由 viewer 走 image_rasterize 转 PNG。
        "png" | "jpg" | "jpeg" | "jfif" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif"
        | "tif" | "tiff" => "image",
        "pdf" => "pdf",
        // 字节类：xlsx/xlsm/xls 由前端 ExcelJS/SheetJS 解析，
        // docx/docm/dotx/dotm 由 docx-preview 解析，pptx/pptm/ppsx/ppsm/potx/potm 由 pptx-browser 解析。
        "xlsx" | "xlsm" | "xls" | "docx" | "docm" | "dotx" | "dotm" | "pptx" | "pptm" | "ppsx"
        | "ppsm" | "potx" | "potm" => "bytes",
        // 老二进制 office 仍走系统程序
        "doc" | "ppt" | "wps" | "et" | "dps" | "rtf" | "odt" | "ods" | "odp" => "legacy_office",
        _ => "unsupported",
    }
}

fn size_limit_for_ext(ext: &str) -> u64 {
    match ext {
        "html" | "htm" => HTML_BYTE_LIMIT,
        "xlsx" | "xlsm" => XLSX_BYTE_LIMIT,
        "xls" => XLS_BYTE_LIMIT,
        "docx" | "docm" | "dotx" | "dotm" => DOCX_BYTE_LIMIT,
        "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" => PPTX_BYTE_LIMIT,
        "csv" | "tsv" => CSV_BYTE_LIMIT,
        _ => TEXT_BYTE_LIMIT,
    }
}

/// 零依赖 base64：避免引入 base64 crate（六步阶梯）。
pub(crate) fn base64_encode(bytes: &[u8]) -> String {    use std::fmt::Write;
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let _ = write!(out, "{}", TBL[(n >> 18) as usize & 63] as char);
        let _ = write!(out, "{}", TBL[(n >> 12) as usize & 63] as char);
        let _ = if chunk.len() > 1 {
            write!(out, "{}", TBL[(n >> 6) as usize & 63] as char)
        } else {
            write!(out, "=")
        };
        let _ = if chunk.len() > 2 {
            write!(out, "{}", TBL[n as usize & 63] as char)
        } else {
            write!(out, "=")
        };
    }
    out
}

/// 零依赖 base64 解码：`base64_encode` 的逆。空白字符（换行）跳过，
/// 其它非法字符直接判失败——解码失败必须可见，不能静默产出垃圾字节。
pub(crate) fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for &c in text.as_bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            break;
        }
        let v = val(c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// RFC 3339 (秒精度)。前端用 `new Date(s)` 直接解析。
fn mtime_to_rfc3339(t: std::time::SystemTime) -> Option<String> {
    use std::time::UNIX_EPOCH;
    let dur = t.duration_since(UNIX_EPOCH).ok()?;
    let secs = dur.as_secs() as i64;
    chrono_like(secs)
}

/// unix 毫秒 → RFC 3339（索引 helper 的 `modified` 是毫秒）。
pub(crate) fn ms_to_rfc3339(ms: u64) -> Option<String> {
    chrono_like((ms / 1000) as i64)
}

/// 没有 chrono 依赖下的极简 RFC 3339：把 unix 秒换成本地年/月/日/时/分/秒。
/// 时区偏移用本地时间表达（前端用 new Date 解析时不需要时区，行为一致）。
fn chrono_like(secs: i64) -> Option<String> {
    if secs < 0 {
        return None;
    }
    let days = (secs / 86_400) as i64;
    let secs_in_day = secs.rem_euclid(86_400);
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    let (y, mo, d) = civil_from_days(days)?;
    Some(format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}",
    ))
}

/// Howard Hinnant `civil_from_days`：从 1970-01-01 起算的天数反推年月日。
/// 不引 chrono：viewer 是 sidecar，再多一个 crate 不划算。
fn civil_from_days(z: i64) -> Option<(i32, u32, u32)> {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    if y < 1 || y > 9999 {
        return None;
    }
    Some((y as i32, m, d))
}

/// Windows 隐藏属性（FILE_ATTRIBUTE_HIDDEN = 0x2）。非 Windows 平台用
/// 名字首字符 `.` 兜底（项目是 Windows-only，但留好跨平台路径便于单测）。
#[cfg(windows)]
fn is_hidden(meta: &std::fs::Metadata, name: &str) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    if meta.file_attributes() & HIDDEN != 0 {
        return true;
    }
    name.starts_with('.')
}
#[cfg(not(windows))]
fn is_hidden(_meta: &std::fs::Metadata, name: &str) -> bool {
    name.starts_with('.')
}

fn file_name_lossy(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn validate_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("路径为空".into());
    }
    if path.contains('\0') {
        return Err("路径包含非法字符".into());
    }
    Ok(PathBuf::from(path))
}

fn build_dir_entry(p: &Path) -> Option<DirEntry> {
    let meta = std::fs::metadata(p).ok()?;
    let name = file_name_lossy(p);
    if name.is_empty() {
        return None;
    }
    let is_dir = meta.is_dir();
    let size = if is_dir { 0 } else { meta.len() };
    let mtime = meta.modified().ok().and_then(mtime_to_rfc3339);
    let hidden = is_hidden(&meta, &name);
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    Some(DirEntry {
        name,
        path: p.to_string_lossy().to_string(),
        is_dir,
        size,
        mtime,
        extension: ext,
        hidden,
    })
}

fn sort_entries(entries: &mut [DirEntry]) {
    // 目录在前；同类内按名称排序（locale-naive：依赖 Windows 排序规则，
    // Rust std 的 `sort` 走字节序；用户在 Windows 资源管理器里看到的也
    // 是 Unicode 字典序，相差不大）。
    use std::cmp::Ordering;
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
}

// ════════════════════════════════════════════════════════════════════
//  Tauri 命令
// ════════════════════════════════════════════════════════════════════

/// 读文件并按扩展名分发。路径是绝对路径（资源管理器/主程序/命令行传入）。
///
/// `command(async)`：文件 IO 走 async runtime，不占 Tauri 主线程——
/// 同步命令默认在主线程执行，50MB 的 xlsx 读盘会把窗口冻住。
#[tauri::command(async)]
pub fn viewer_read_file(path: String) -> Result<FileContent, String> {
    let p = validate_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|e| format!("无法读取文件信息: {e}"))?;
    if !meta.is_file() {
        return Err(format!("不是文件: {path}"));
    }
    let name = file_name_lossy(&p);
    let ext = ext_of(&path);
    let class = classify(&ext);

    // 可执行文件不可预览：连字节都不读（大 exe 有几百 MB，读一遍再丢掉
    // 是白费 IO 与内存），也不吃大小上限——超大的 exe 同样不可预览，
    // 报「文件过大」只会让用户更困惑。
    if EXECUTABLE_EXT.contains(&ext.as_str()) {
        return Ok(unsupported_content(name, ext));
    }

    let limit = size_limit_for_ext(&ext);
    if meta.len() > limit {
        return Err(format!(
            "文件超过 {}MB（{} MB），请在系统程序中打开",
            limit / 1024 / 1024,
            meta.len() / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(&p).map_err(|e| format!("读取失败: {e}"))?;
    Ok(match class {
        "image" => {
            let mime = match ext.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" | "jfif" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "svg" => "image/svg+xml",
                "bmp" => "image/bmp",
                "ico" => "image/x-icon",
                "avif" => "image/avif",
                "tif" | "tiff" => "image/tiff",
                _ => "application/octet-stream",
            };
            FileContent::Image {
                name,
                mime: mime.into(),
                data_base64: base64_encode(&bytes),
            }
        }
        "pdf" => FileContent::Pdf {
            name,
            data_base64: base64_encode(&bytes),
        },
        "text" | "code" => {
            let variant = if class == "text" { "text" } else { "code" };
            FileContent::Text {
                name,
                variant: variant.into(),
                data_base64: base64_encode(&bytes),
            }
        }
        "bytes" => FileContent::Bytes {
            name,
            extension: ext,
            data_base64: base64_encode(&bytes),
        },
        "legacy_office" => FileContent::Unsupported {
            name,
            extension: ext,
            reason: "老版二进制格式需要 Office，请用系统程序打开".into(),
            executable: false,
        },
        _ => unsupported_content(name, ext),
    })
}

/// 「不渲染」的结果。措辞分两种：可执行文件说清「不可预览但能跑」，
/// 其余类型说清「用系统程序打开」。
fn unsupported_content(name: String, ext: String) -> FileContent {
    let executable = EXECUTABLE_EXT.contains(&ext.as_str());
    FileContent::Unsupported {
        name,
        extension: ext,
        reason: if executable {
            "二进制可执行文件不可预览，点击「立即运行」执行".into()
        } else {
            "暂不支持该类型，请用系统程序打开".into()
        },
        executable,
    }
}

/// 列目录。可选 `show_hidden`：默认 false（与 Windows 资源管理器一致）。
/// 不递归：树形 UI 由前端按需逐级请求，避免一次性把大目录全塞回来。
/// `command(async)`：目录枚举在 async runtime 上跑，主线程不被 IO 占住。
#[tauri::command(async)]
pub fn viewer_list_dir(path: String, show_hidden: Option<bool>) -> Result<Vec<DirEntry>, String> {
    let p = validate_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|e| format!("无法访问: {e}"))?;
    if !meta.is_dir() {
        return Err(format!("不是目录: {path}"));
    }
    let show_hidden = show_hidden.unwrap_or(false);
    let read = std::fs::read_dir(&p).map_err(|e| format!("读取目录失败: {e}"))?;
    let mut out: Vec<DirEntry> = Vec::new();
    for entry in read.flatten() {
        if let Some(item) = build_dir_entry(&entry.path()) {
            if !show_hidden && item.hidden {
                continue;
            }
            out.push(item);
        }
    }
    sort_entries(&mut out);
    Ok(out)
}

/// 单个文件 / 目录的元信息。前端用它判断能否进入 / 能否预览。
/// `command(async)`：stat 也要 IO（打网络盘会等 SMB 超时），挪出主线程。
#[tauri::command(async)]
pub fn viewer_stat(path: String) -> Result<FileStat, String> {
    let p = validate_path(&path)?;
    match std::fs::metadata(&p) {
        Ok(meta) => {
            let name = file_name_lossy(&p);
            let mtime = meta.modified().ok().and_then(mtime_to_rfc3339);
            let hidden = is_hidden(&meta, &name);
            Ok(FileStat {
                path: p.to_string_lossy().to_string(),
                exists: true,
                is_dir: meta.is_dir(),
                is_file: meta.is_file(),
                size: meta.len(),
                mtime,
                hidden,
            })
        }
        Err(_) => Ok(FileStat {
            path: p.to_string_lossy().to_string(),
            exists: false,
            is_dir: false,
            is_file: false,
            size: 0,
            mtime: None,
            hidden: false,
        }),
    }
}

/// 父目录（用于「上一级」按钮）。已是盘根时返回 None。
#[tauri::command]
pub fn viewer_parent(path: String) -> Option<String> {
    let p = PathBuf::from(&path);
    let parent = p.parent()?.to_string_lossy().to_string();
    if parent.is_empty() {
        return None;
    }
    Some(parent)
}

/// 列出本机盘符（Windows）。Linux/macOS 留空数组。
///
/// **为什么不用 `std::fs::metadata` 判盘符类型**：那是对文件系统的真实
/// IO，打在断开的网络映射盘（`net use` 留下的 Z:/Y:）上会阻塞到 SMB
/// 超时（可达数十秒）。这个命令在启动路径上，而同步命令默认在 Tauri
/// 卷标（用户给这块盘起的名字，如 "Windows" / "工作盘"）。
///
/// `GetVolumeInformationW` 是**真 IO**，只对本地盘调用：网络盘的卷标查询
/// 要走网络，断开的映射盘会卡到 SMB 超时（几十秒）——那正是本命令
/// 当初被改成 `GetDriveTypeW` 的原因。网络盘保留类型标签。
#[cfg(windows)]
fn volume_label(root: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let mut wide: Vec<u16> = root.encode_utf16().collect();
    wide.push(0);
    let mut buf = vec![0u16; 260];
    // 只需要卷标：文件系统名缓冲区传 NULL + 长度 0（Windows 允许）。
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// 主线程执行——一次阻塞就是整个窗口白屏。`GetDriveTypeW` 只问驱动类型，
/// 不碰文件系统，恒为内核态快操作。
///
/// 卷标（"Windows (C:)" 这类用户自己起的名字）另外用
/// `GetVolumeInformationW` 取，只对**本地盘**取——网络盘的卷标查询要走
/// 网络，断开的映射盘会卡到 SMB 超时（可达数十秒），那会让整个驱动器
/// 列表迟迟不出现。
#[tauri::command(async)]
pub fn viewer_drives() -> Vec<DriveRoot> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
        use windows_sys::Win32::System::WindowsProgramming::{
            DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
        };
        let mask = unsafe { GetLogicalDrives() };
        (0..26)
            .filter_map(|i| {
                if mask & (1 << i) == 0 {
                    return None;
                }
                let letter = ((b'A' + i) as char).to_string();
                let path = format!("{letter}:\\");
                let mut wide: Vec<u16> = path.encode_utf16().collect();
                wide.push(0);
                let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
                // 有卷标就用卷标（用户真正认得的名字），没有才退回类型名。
                let volume = match drive_type {
                    DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_RAMDISK => volume_label(&path),
                    _ => None,
                };
                let label = match volume {
                    Some(v) if !v.trim().is_empty() => format!("{} ({letter}:)", v.trim()),
                    _ => match drive_type {
                        DRIVE_FIXED => format!("本地磁盘 ({letter}:)"),
                        DRIVE_REMOVABLE => format!("可移动磁盘 ({letter}:)"),
                        DRIVE_REMOTE => format!("网络驱动器 ({letter}:)"),
                        DRIVE_CDROM => format!("光驱 ({letter}:)"),
                        DRIVE_RAMDISK => format!("内存盘 ({letter}:)"),
                        _ => format!("磁盘 ({letter}:)"),
                    },
                };
                Some(DriveRoot { letter, path, label })
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// 用户主目录：首屏的兜底路径（上次访问目录之后）。
/// 纯路径解析，不做文件系统 IO。
#[tauri::command(async)]
pub fn viewer_home_dir(app: tauri::AppHandle) -> Option<String> {
    use tauri::Manager;
    app.path()
        .home_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// 在 Windows 资源管理器里高亮选中一个文件（`explorer /select,"path"`）。
///
/// 走 ShellExecute 而不是 `Command::new("explorer.exe").spawn()`：后者是
/// 「程序自己创建子进程」，正是 360 行为监控的误报触发点（见 shell_exec
/// 的说明）。这里让系统 shell 去开 explorer，语义与资源管理器双击同构。
#[tauri::command]
pub fn viewer_reveal_in_explorer(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        let p = validate_path(&path)?;
        if !p.exists() {
            return Err(format!("文件不存在: {path}"));
        }
        reveal_in_explorer(&p)
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// shell32 的 PIDL 接口（"在资源管理器中打开并选中"用）。
///
/// 为什么自己声明：windows-sys 0.52 没有 `Win32_UI_Shell_Common` 特性，
/// 而 `ILCreateFromPathW` / `SHOpenFolderAndSelectItems` 这些函数的参数
/// 类型正好挂在那个缺失的模块下（`Common::ITEMIDLIST`），0.52 里拿不到。
/// 升级 windows-sys 会波及全仓 0.52 约定的其它调用点（HWND / HANDLE 在
/// 0.59 之后从 isize 变回指针），为 6 个函数动整个工作区不划算。
///
/// ITEMIDLIST 的布局是稳定的：`cb`（字节数）+ 变长记录。
#[cfg(windows)]
mod pidl {
    #[repr(C)]
    pub struct ItemIdList {
        pub cb: u16,
        pub ab_record: [u8; 1],
    }

    extern "system" {
        pub fn ILCreateFromPathW(pszpath: *const u16) -> *mut ItemIdList;
        pub fn ILClone(pidl: *const ItemIdList) -> *mut ItemIdList;
        pub fn ILRemoveLastID(pidl: *mut ItemIdList) -> i32;
        pub fn ILFindLastID(pidl: *const ItemIdList) -> *mut ItemIdList;
        pub fn ILFree(pidl: *mut ItemIdList);
        pub fn SHOpenFolderAndSelectItems(
            pidlfolder: *const ItemIdList,
            cidl: u32,
            apidl: *const *const ItemIdList,
            dwflags: u32,
        ) -> i32;
    }
}

/// 在资源管理器里定位并选中一个已存在的路径。
///
/// 用 `SHOpenFolderAndSelectItems`（shell32 的官方 API），**不用**
/// `explorer.exe /select,...`：后者要么自己创建子进程（360 行为监控的
/// 误报触发点），要么经 ShellExecute 时参数被 shell 吞掉（实测
/// ShellExecuteEx 返回成功但窗口没开）。
/// SHOpenFolderAndSelectItems 直接跟已在运行的 explorer.exe 说话，
/// 既不创建进程，又能真正「打开并选中」。
#[cfg(windows)]
fn reveal_in_explorer(p: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use pidl::{
        ILClone, ILCreateFromPathW, ILFindLastID, ILFree, ILRemoveLastID,
        SHOpenFolderAndSelectItems,
    };

    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        // 整条路径的 PIDL
        let full = ILCreateFromPathW(wide.as_ptr());
        if full.is_null() {
            return Err("无法解析该路径（Shell 拒绝）".into());
        }
        // 最后一段 = 要选中的文件；去掉最后一段 = 它所在的文件夹
        let item = ILFindLastID(full);
        let folder = ILClone(full);
        ILRemoveLastID(folder);
        let item_ptr: *const pidl::ItemIdList = item;
        let apidl: *const *const pidl::ItemIdList = &item_ptr;
        let hr = SHOpenFolderAndSelectItems(folder, 1, apidl, 0);
        ILFree(folder);
        ILFree(full);
        if hr < 0 {
            return Err(format!("打开资源管理器失败（HRESULT {hr:#x}）"));
        }
    }
    Ok(())
}

/// 在目录下做文件名子串搜索（深度 1，不递归到子目录——避免大目录爆栈）。
/// `max` 限制返回条数（默认 200），避免一次性传太多。
#[tauri::command(async)]
pub fn viewer_search_dir(
    path: String,
    query: String,
    show_hidden: Option<bool>,
    max: Option<usize>,
) -> Result<Vec<DirEntry>, String> {
    let p = validate_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|e| format!("无法访问: {e}"))?;
    if !meta.is_dir() {
        return Err(format!("不是目录: {path}"));
    }
    let show_hidden = show_hidden.unwrap_or(false);
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(Vec::new()); // 空查询：不返回任何结果
    }
    let max = max.unwrap_or(200);
    let read = std::fs::read_dir(&p).map_err(|e| format!("读取目录失败: {e}"))?;
    let mut out: Vec<DirEntry> = Vec::new();
    for entry in read.flatten() {
        let Some(item) = build_dir_entry(&entry.path()) else {
            continue;
        };
        if !show_hidden && item.hidden {
            continue;
        }
        if !item.name.to_lowercase().contains(&q) {
            continue;
        }
        out.push(item);
        if out.len() >= max {
            break;
        }
    }
    sort_entries(&mut out);
    Ok(out)
}

/// 用系统 shell「打开」一个文件或网址（等价于在资源管理器里双击）。
///
/// **为什么不用 `Command::new(path).spawn()`**：那是「程序自己创建子进程」，
/// 正是 Windows 行为监控（360 的「恶意行为监控」等）的经典误报触发点。
/// 2026-10-20 实测：viewer 用 CreateProcess 启动 notepad.exe，被 360 判成
/// 「ADVtrojan/GenInjector.E（木马病毒）」并**直接杀掉 viewer 进程**——
/// 程序本身干净，但用户看到的是「我的软件报毒了」。
///
/// 走 ShellExecute 则是「请系统 shell 打开这个文件」：子进程由 shell
/// （explorer.exe）创建，不由 viewer 创建。这与用户在资源管理器里双击
/// 完全同构，行为监控看到的是一次普通的「打开文件」。
///
/// `working_dir` 对应 Explorer 双击的语义（工作目录 = 文件所在目录），
/// 免得程序读不到同目录的配置文件。
#[cfg(windows)]
fn shell_open(target: &Path, working_dir: Option<&Path>) -> Result<(), String> {
    shell_exec(Some("open"), target, None, working_dir)
}

/// 通用 ShellExecuteEx 包装：动词（open / properties / …）+ 目标 + 参数。
///
/// 所有「viewer 让系统去做点什么」的动作都走这里——**绝不能**改回
/// `Command::new(..).spawn()`（360 误报，2026-10-20 教训）。
#[cfg(windows)]
fn shell_exec(
    verb: Option<&str>,
    file: &Path,
    params: Option<&str>,
    working_dir: Option<&Path>,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let target_w: Vec<u16> = file.as_os_str().encode_wide().chain(Some(0)).collect();
    let verb_w: Vec<u16> = verb
        .map(|v| v.encode_utf16().chain(Some(0)).collect())
        .unwrap_or_default();
    let params_w: Vec<u16> = params
        .map(|v| v.encode_utf16().chain(Some(0)).collect())
        .unwrap_or_default();
    let dir_w: Vec<u16> = working_dir
        .map(|d| d.as_os_str().encode_wide().chain(Some(0)).collect())
        .unwrap_or_default();

    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.lpVerb = if verb_w.is_empty() {
        std::ptr::null()
    } else {
        verb_w.as_ptr()
    };
    sei.lpFile = target_w.as_ptr();
    sei.lpParameters = if params_w.is_empty() {
        std::ptr::null()
    } else {
        params_w.as_ptr()
    };
    sei.lpDirectory = if dir_w.is_empty() {
        std::ptr::null()
    } else {
        dir_w.as_ptr()
    };
    sei.nShow = SW_SHOWNORMAL;
    // ShellExecuteEx 成功返回 > 32；<= 32 是 SE_ERR_* 错误码。
    let ok = unsafe { ShellExecuteExW(&mut sei) };
    if ok <= 32 {
        return Err(format!("系统无法打开该目标（ShellExecute 返回 {ok}）"));
    }
    Ok(())
}

/// `%SystemRoot%\System32` 下的系统可执行文件（notepad / explorer）。
#[cfg(windows)]
fn system_exe(name: &str) -> Option<PathBuf> {
    let root = std::env::var_os("SystemRoot")?;
    let p = PathBuf::from(root).join("System32").join(name);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// 在 PATH 里找可执行文件（用于「用 VS Code 打开」）。
#[cfg(windows)]
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// 用指定方式打开目标。`app` 取值：
///   - `default`    系统默认程序（等价双击）
///   - `notepad`    记事本
///   - `code`       VS Code（code.exe 不在 PATH 时直接报错，不静默失败）
///   - `properties` Windows 属性对话框
///   - `reveal`     在资源管理器中定位
#[tauri::command]
pub fn viewer_open_with(path: String, app: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        let p = validate_path(&path)?;
        match app.as_str() {
            "default" => shell_open(&p, p.parent()),
            "notepad" => {
                let np = system_exe("notepad.exe").ok_or("找不到系统记事本（notepad.exe）")?;
                let params = format!("\"{}\"", p.display());
                shell_exec(Some("open"), &np, Some(&params), p.parent())
            }
            "code" => {
                let exe = find_in_path("code.exe").ok_or("未找到 VS Code（code.exe 不在 PATH 中）")?;
                let params = format!("\"{}\"", p.display());
                shell_exec(Some("open"), &exe, Some(&params), p.parent())
            }
            "properties" => shell_exec(Some("properties"), &p, None, None),
            "reveal" => reveal_in_explorer(&p),
            other => Err(format!("不支持的打开方式: {other}")),
        }
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 探测本机可用的「打开方式」——右键菜单据此把没装的置灰，
/// 而不是给一个点了才报错的假按钮。
#[tauri::command]
pub fn viewer_available_apps() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut apps: Vec<String> = Vec::new();
        if system_exe("notepad.exe").is_some() {
            apps.push("notepad".into());
        }
        if find_in_path("code.exe").is_some() {
            apps.push("code".into());
        }
        apps
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// 写文本到剪贴板（路径复制 / 复制选中文本）。
#[tauri::command]
pub fn viewer_copy_text(text: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::clipboard::write_text(&text)
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 读剪贴板文本（路径栏「粘贴」）。
#[tauri::command]
pub fn viewer_paste_text() -> Result<String, String> {
    #[cfg(windows)]
    {
        crate::clipboard::read_text()
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 复制文件/文件夹到剪贴板（CF_HDROP），返回条目数。
///
/// 与浏览器 Clipboard API 无关：那个 API 写不了「文件列表」。
#[tauri::command]
pub fn viewer_copy_files(paths: Vec<String>) -> Result<usize, String> {
    #[cfg(windows)]
    {
        crate::clipboard::copy_to_clipboard(&paths)
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 从剪贴板粘贴文件到目标目录。已存在的同名项**跳过**（不覆盖）。
#[tauri::command]
pub fn viewer_paste_files(dir: String) -> Result<PasteResult, String> {
    #[cfg(windows)]
    {
        let target = validate_path(&dir)?;
        if !target.is_dir() {
            return Err(format!("不是目录: {dir}"));
        }
        let items = crate::clipboard::paste_from_clipboard()?;
        if items.is_empty() {
            return Err("剪贴板里没有文件".into());
        }
        let mut copied: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for src in items {
            let name = src
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let dst = target.join(&name);
            if dst.exists() {
                skipped.push(name);
                continue;
            }
            crate::clipboard::copy_recursive(&src, &dst)?;
            copied.push(name);
        }
        Ok(PasteResult { copied, skipped })
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 粘贴结果：复制成功的与因同名被跳过的。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PasteResult {
    pub copied: Vec<String>,
    pub skipped: Vec<String>,
}

/// 用系统默认程序打开。
#[tauri::command]
pub fn viewer_open_with_system(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        shell_open(Path::new(&path), None)
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 运行一个可执行文件（exe / msi / com / scr / pif）。
///
/// 三道信任边界：
///   1. 扩展名必须在 `EXECUTABLE_EXT` 白名单里——这个命令不能被用来启动
///      任意文件（.bat / .ps1 需要 shell 语义，另走「用系统程序打开」）；
///   2. 路径必须存在且是文件；
///   3. **执行前弹原生确认框**：预览一个陌生 exe 就把它跑起来是最短的
///      误触路径，所以确认必须由用户亲手点，不能静默执行。
///
/// 返回 true = 已启动；false = 用户在确认框里取消了（不是错误）。
#[tauri::command(async)]
pub fn viewer_run_executable(app: tauri::AppHandle, path: String) -> Result<bool, String> {
    #[cfg(windows)]
    {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
        let p = validate_path(&path)?;
        let ext = ext_of(&path);
        if !EXECUTABLE_EXT.contains(&ext.as_str()) {
            return Err(format!("不支持直接运行该类型（{ext}）"));
        }
        if !p.is_file() {
            return Err(format!("文件不存在或不是文件: {path}"));
        }
        let name = file_name_lossy(&p);
        let confirmed = app
            .dialog()
            .message(format!(
                "即将运行：{name}\n\n路径：{}\n\n运行来源不明的程序可能影响系统，确认可信后再继续。",
                p.display()
            ))
            .title("运行可执行文件")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "运行".into(),
                "取消".into(),
            ))
            .blocking_show();
        if !confirmed {
            return Ok(false);
        }
        // 交给系统 shell「打开」——工作目录设为文件所在目录：程序若读同目录
        // 的配置文件，从 viewer 的工作目录启动会找不到（双击 exe 时 cwd
        // 就是它所在目录）。
        //
        // 刻意**不**用 Command::new(path).spawn()：自己创建子进程是行为
        // 监控的经典误报触发点（360 会判成木马并杀掉 viewer）。
        let cwd = p.parent().unwrap_or_else(|| std::path::Path::new("."));
        shell_open(&p, Some(cwd))?;
        Ok(true)
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 前端就绪后取走启动参数/单实例投递的待开文件。
#[tauri::command]
pub fn viewer_take_pending(state: tauri::State<ViewerState>) -> Vec<String> {
    let mut pending = state.pending.lock().unwrap();
    pending.drain(..).map(|p| p.to_string_lossy().to_string()).collect()
}

/// ai_send 的归一化返回：reply 从 server 响应中取出，sessionId 回传给
/// 前端，后续消息带上即可复用同一会话（多轮上下文由 server 侧保存）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiReply {
    pub reply: String,
    pub session_id: String,
}

// ---------------------------------------------------------------------------
// AI 右栏 — 直连本地 server 的 /v1/ REST（非流式，一期预览级）。
// viewer 是独立进程，不能用主程序前端的 stdio 桥（那根管道归主 app）；
// 本地 HTTP 面无鉴权且仅 127.0.0.1 可达（server 默认 bind 127.0.0.1:3000）。
// ---------------------------------------------------------------------------

/// API 基址：默认内置端口，env `HERMIX_VIEWER_API_BASE` 可覆写（与
/// server 的 bind env 解耦——这里要的是完整 base，不是端口）。
fn api_base() -> String {
    std::env::var("HERMIX_VIEWER_API_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:3000".into())
}

/// 非流式问一轮：无 session_id 时建会话（标题带文件名），有则复用；
/// 然后往同一会话 send_message。文件上下文拼进 prompt（服务器零改动；
/// selection 为空只带路径）。
#[tauri::command]
pub async fn ai_send(
    text: String,
    file_path: Option<String>,
    selection: Option<String>,
    session_id: Option<String>,
) -> Result<AiReply, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("HTTP client 构建失败: {e}"))?;
    let base = api_base().trim_end_matches('/').to_string();

    let sid = match session_id.filter(|s| !s.trim().is_empty()) {
        Some(sid) => sid,
        None => {
            let title = file_path
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_name())
                .map(|n| format!("查看器: {}", n.to_string_lossy()))
                .unwrap_or_else(|| "查看器对话".into());
            let created: serde_json::Value = client
                .post(format!("{base}/v1/sessions"))
                .json(&serde_json::json!({ "title": title }))
                .send()
                .await
                .map_err(|e| format!("创建会话失败: {e}"))?
                .json()
                .await
                .map_err(|e| format!("解析会话响应失败: {e}"))?;
            created
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("会话响应缺 id: {created}"))?
                .to_string()
        }
    };

    let mut prompt = String::new();
    if let Some(p) = &file_path {
        prompt.push_str(&format!("[正在查看的文件] {p}\n\n"));
    }
    if let Some(sel) = &selection {
        let sel = sel.trim();
        if !sel.is_empty() {
            prompt.push_str("[选中的片段]\n");
            // 选区可能是大段文本：截断保护，避免一次拖爆请求体。
            prompt.push_str(&sel.chars().take(4000).collect::<String>());
            prompt.push_str("\n\n");
        }
    }
    prompt.push_str(&text);

    let resp = client
        .post(format!("{base}/v1/sessions/{sid}/messages"))
        .json(&serde_json::json!({ "text": prompt }))
        .send()
        .await
        .map_err(|e| format!("发送失败: {e}"))?;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    if status != 200 {
        return Err(format!(
            "HTTP {status}: {}",
            body.get("error").and_then(|v| v.as_str()).unwrap_or("(无详情)")
        ));
    }
    let reply = body
        .get("reply")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("消息响应缺 reply: {body}"))?
        .to_string();
    Ok(AiReply { reply, session_id: sid })
}

#[cfg(test)]
mod tests {
    use super::{base64_decode, base64_encode, classify, EXECUTABLE_EXT};

    /// base64 编解码必须是互逆的——图片合成的 PNG 走解码路径，
    /// 差一个 bit 就会让整张合成图报废。
    #[test]
    fn base64_roundtrip() {
        for bytes in [
            vec![],
            vec![0u8],
            vec![1, 2, 3],
            (0u8..=255).collect::<Vec<u8>>(),
        ] {
            let enc = base64_encode(&bytes);
            assert_eq!(base64_decode(&enc).as_deref(), Some(bytes.as_slice()));
        }
    }

    /// 非法字符必须判失败，而不是静默产出垃圾字节。
    #[test]
    fn base64_decode_rejects_garbage() {
        assert!(base64_decode("!!!!").is_none());
    }

    /// viewer 侧的扩展名分类必须与共享预览层（@hermix/preview）的读取器
    /// 预期一致：共享层的 htmlLoader / markdownLoader / textLoader 走
    /// readText，表格与 Office 走 readBytes。分类错一档，viewer 侧就
    /// 永远看不到对应类型的预览（曾因此把 .html 判成二进制）。
    #[test]
    fn classify_matches_shared_preview_readers() {
        // readText 家族：源码 / 正文类
        for ext in ["md", "markdown", "txt", "json", "csv", "tsv", "html", "htm"] {
            assert_eq!(classify(ext), "text", "{ext} 应走 readText");
        }
        for ext in ["rs", "ts", "tsx", "py", "vue"] {
            assert_eq!(classify(ext), "code", "{ext} 应走 readText");
        }
        // readBytes 家族：前端引擎解析
        for ext in ["xlsx", "xlsm", "xls", "docx", "pptx"] {
            assert_eq!(classify(ext), "bytes", "{ext} 应走 readBytes");
        }
        for ext in ["png", "jpg", "webp", "svg", "gif"] {
            assert_eq!(classify(ext), "image", "{ext} 应走图片分支");
        }
        assert_eq!(classify("pdf"), "pdf");
        assert_eq!(classify("doc"), "legacy_office");
        assert_eq!(classify("unknownext"), "unsupported");
        assert_eq!(classify("exe"), "unsupported");
    }

    /// EXECUTABLE_EXT 是「立即运行」的信任边界：多一个扩展名，就多一类
    /// 能被直接启动的文件；少一个，用户点了「立即运行」会被拒。
    ///
    /// 脚本类（bat / cmd / ps1）刻意不在表内——它们需要 shell 语义
    /// （cmd.exe / powershell 的解析与策略），走「用系统程序打开」，
    /// 不由 viewer 直接启动。dll / sys 是库不是程序，双击也不会执行。
    #[test]
    fn executable_whitelist_is_exact() {
        for ext in ["exe", "msi", "com", "scr", "pif"] {
            assert!(EXECUTABLE_EXT.contains(&ext), "{ext} 应可直接运行");
        }
        for ext in ["bat", "cmd", "ps1", "vbs", "js", "txt", "dll", "sys"] {
            assert!(!EXECUTABLE_EXT.contains(&ext), "{ext} 不该被直接启动");
        }
    }
}
