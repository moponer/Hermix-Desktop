//! viewer 的 MCP（Model Context Protocol）服务：让**任意外部 AI**（Claude
//! Desktop / Cursor / VS Code …）把查看器当工具用。
//!
//! **协议为什么自己写而不引 `hermix-mcp`**：
//!   1. `hermix-mcp` 的 stdio 走 LSP 式 `Content-Length` 头（`transport.rs`），
//!      **不符合 MCP 规范**（规范要求换行分隔的 JSON-RPC）→ 第三方客户端
//!      连不上。「任意外部 AI」这个目标与它直接冲突。
//!   2. `hermix-mcp` → `hermix-tools` 会拖进 chromiumoxide / rusqlite /
//!      docx-rs / ppt-rs 一整套依赖；viewer 是 8 个依赖的轻量 sidecar。
//!   3. MCP 规范的服务端面很小：`initialize` / `tools/list` / `tools/call`
//!      三个方法，serde_json + std::io 就够（六步阶梯第 5 步「最小实现」）。
//!
//! **暴露哪些能力**：文件管理器的预览 / 搜索 / 编辑（用户点名的三项），
//! 外加一组桌面 GUI 操作（列窗口 / 找控件 / 点击 / 填文本 / 按键），
//! 让 agent 能真的去点别的程序的界面。
//!
//! **不暴露什么**：`viewer_run_executable`。MCP 模式没有原生确认框（没有窗口），
//! 让外部 AI 直接跑任意 exe 是最短的误触路径——那个能力留在 GUI 模式里，
//! 由用户亲手点确认框。

use std::io::{BufRead, Write};

use serde_json::{json, Value};

/// MCP 协议版本：客户端 `initialize` 里带什么就回什么（它决定后续用哪版规范）。
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// 单次工具输出上限：工具结果会被读进模型下一轮的上下文，无上限就是上下文炸弹。
const RESULT_CAP: usize = 8_000;

/// 预览文本的截断上限（比 RESULT_CAP 小，留给外层包装）。
const PREVIEW_CAP: usize = 6_000;

// ── 工具定义 ────────────────────────────────────────────────────────────

/// 一个工具的声明（`tools/list` 的元素）。
struct ToolDef {
    name: &'static str,
    description: &'static str,
    /// JSON Schema 的 properties。
    properties: Value,
    required: &'static [&'static str],
    /// 只读 / 破坏性标记——MCP 客户端（Claude Desktop 等）据此决定是否弹权限确认。
    read_only: bool,
    destructive: bool,
}

fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "viewer_list_dir",
            description: "列出目录内容（不递归）。path 必须是绝对路径。",
            properties: json!({"path": {"type": "string", "description": "目录绝对路径"}}),
            required: &["path"],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "viewer_read_file",
            description: "预览一个文件：文本类返回内容，图片/PDF/二进制返回类型与体积摘要。",
            properties: json!({"path": {"type": "string", "description": "文件绝对路径"}}),
            required: &["path"],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "viewer_search",
            description: "搜索文件。给了 path 就在该目录内搜（文件名子串）；不给 path \
                          就走本地全盘索引（类 Everything，跨所有已索引卷，需要管理员权限的索引服务）。",
            properties: json!({
                "query": {"type": "string", "description": "搜索词"},
                "path": {"type": "string", "description": "限定目录（可选）"},
                "limit": {"type": "integer", "description": "最多返回多少条（默认 100）"},
            }),
            required: &["query"],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "viewer_write_text",
            description: "写一个文本文件。目标已存在时必须显式传 overwrite=true —— \
                          默认拒绝覆盖，避免 AI 顺手抹掉用户的文件。",
            properties: json!({
                "path": {"type": "string", "description": "文件绝对路径"},
                "content": {"type": "string", "description": "要写入的文本内容"},
                "overwrite": {"type": "boolean", "description": "允许覆盖已存在的文件"},
            }),
            required: &["path", "content"],
            read_only: false,
            destructive: true,
        },
        ToolDef {
            name: "desktop_list_windows",
            description: "列出桌面上的顶层窗口（hwnd / 标题 / 类名 / 进程 id）。",
            properties: json!({
                "only_visible": {"type": "boolean", "description": "只列可见窗口（默认 true）"},
            }),
            required: &[],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "desktop_list_controls",
            description: "列出某个窗口下的子控件（hwnd / 类名 / 文本 / 控件 id）。",
            properties: json!({"hwnd": {"type": "string", "description": "窗口句柄，如 0x1234"}}),
            required: &["hwnd"],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "desktop_click",
            description: "点击一个控件（按钮走 BM_CLICK，其他控件走鼠标消息序列）。",
            properties: json!({"hwnd": {"type": "string", "description": "控件句柄"}}),
            required: &["hwnd"],
            read_only: false,
            destructive: true,
        },
        ToolDef {
            name: "desktop_set_text",
            description: "设置控件文本（编辑框、下拉框）。直接写内容，不是逐键敲字。",
            properties: json!({
                "hwnd": {"type": "string", "description": "控件句柄"},
                "text": {"type": "string", "description": "要写入的文本"},
            }),
            required: &["hwnd", "text"],
            read_only: false,
            destructive: true,
        },
        ToolDef {
            name: "desktop_read_text",
            description: "读取控件文本。",
            properties: json!({"hwnd": {"type": "string", "description": "控件句柄"}}),
            required: &["hwnd"],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "desktop_press_key",
            description: "向控件发送一次按键（默认回车 VK_RETURN=13）。",
            properties: json!({
                "hwnd": {"type": "string", "description": "控件句柄"},
                "vk": {"type": "integer", "description": "虚拟键码（默认 13 回车）"},
            }),
            required: &["hwnd"],
            read_only: false,
            destructive: true,
        },
        ToolDef {
            name: "browser_open",
            description: "用系统浏览器（Edge / Chrome）打开网址，供 agent 操作页面。\
                          **每个 session 一个独立浏览器进程 + 独立 profile**\
                          （cookie / 登录态互不干扰，多 agent 可并行）。\
                          headed=true 弹出真实浏览器窗口（用户看得见、也能接管），\
                          false 则无头运行。",
            properties: json!({
                "url": {"type": "string", "description": "要打开的网址（http / https）"},
                "session": {"type": "string", "description": "会话 id（隔离键，缺省 default）"},
                "headed": {"type": "boolean", "description": "是否有头（默认 false = 无头）"},
            }),
            required: &["url"],
            read_only: false,
            destructive: false,
        },
        ToolDef {
            name: "browser_navigate",
            description: "让浏览器会话跳到新网址（复用已开好的会话，不要重复 browser_open）。",
            properties: json!({
                "session": {"type": "string", "description": "会话 id"},
                "url": {"type": "string", "description": "目标网址"},
            }),
            required: &["session", "url"],
            read_only: false,
            destructive: false,
        },
        ToolDef {
            name: "browser_text",
            description: "读取当前页面的可见文本。比截图省 token，读页面内容首选。",
            properties: json!({
                "session": {"type": "string", "description": "会话 id"},
            }),
            required: &["session"],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "browser_eval",
            description: "在当前页面执行一段 JavaScript，返回其返回值（按值返回，Promise 会被等待）。",
            properties: json!({
                "session": {"type": "string", "description": "会话 id"},
                "expression": {"type": "string", "description": "要执行的 JavaScript 表达式"},
            }),
            required: &["session", "expression"],
            read_only: false,
            destructive: false,
        },
        ToolDef {
            name: "browser_click",
            description: "点击当前页面的某个坐标（CSS 像素，相对视口左上角）。",
            properties: json!({
                "session": {"type": "string", "description": "会话 id"},
                "x": {"type": "number", "description": "横坐标"},
                "y": {"type": "number", "description": "纵坐标"},
            }),
            required: &["session", "x", "y"],
            read_only: false,
            destructive: true,
        },
        ToolDef {
            name: "browser_screenshot",
            description: "给当前页面截图：返回缩略图（给模型看）与落盘路径（用户可直接打开）。",
            properties: json!({
                "session": {"type": "string", "description": "会话 id"},
            }),
            required: &["session"],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "browser_sessions",
            description: "列出当前所有浏览器会话（id / 有头无头 / 当前网址）。",
            properties: json!({}),
            required: &[],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "browser_close",
            description: "关闭浏览器会话，并删除它的 profile（cookie / 缓存不留痕）。",
            properties: json!({
                "session": {"type": "string", "description": "会话 id"},
            }),
            required: &["session"],
            read_only: false,
            destructive: true,
        },
        ToolDef {
            name: "web_open",
            description: "在**查看器内嵌的网页视图**里打开网址（不是系统浏览器）。\
                          页面就显示在查看器的预览区里，用户能直接看见、也能自己登录；\
                          之后用 webview_* 系列工具就能驱动这同一个页面。\
                          需要查看器以窗口模式运行（MCP HTTP 端点）。",
            properties: json!({
                "url": {"type": "string", "description": "要打开的网址（http / https）"},
            }),
            required: &["url"],
            read_only: false,
            destructive: false,
        },
        ToolDef {
            name: "web_close",
            description: "关掉查看器内嵌的网页视图。",
            properties: json!({}),
            required: &[],
            read_only: false,
            destructive: false,
        },
        ToolDef {
            name: "webview_state",
            description: "内嵌网页视图当前页面的 URL 与标题。",
            properties: json!({}),
            required: &[],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "webview_text",
            description: "读取内嵌网页视图当前页面的可见文本。比截图省 token，读页面内容首选。",
            properties: json!({}),
            required: &[],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "webview_eval",
            description: "在内嵌网页视图当前页面里执行一段 JavaScript，返回其返回值。\
                          **跨源 iframe 里的元素顶层求值够不到**（超星的视频播放器就在\
                          `/ananas/modules/video/index.html` 这个跨源 iframe 里）——\
                          这时用参数 frame 给一个 URL 片段（如 \"ananas\"），\
                          就会在那个 iframe 的执行上下文里执行。",
            properties: json!({
                "expression": {"type": "string", "description": "要执行的 JavaScript 表达式"},
                "frame": {"type": "string", "description": "目标 iframe 的 URL 片段（可选）"},
            }),
            required: &["expression"],
            read_only: false,
            destructive: false,
        },
        ToolDef {
            name: "webview_click",
            description: "点击内嵌网页视图当前页面的某个坐标（CSS 像素，相对视口左上角）。",
            properties: json!({
                "x": {"type": "number", "description": "横坐标"},
                "y": {"type": "number", "description": "纵坐标"},
            }),
            required: &["x", "y"],
            read_only: false,
            destructive: true,
        },
        ToolDef {
            name: "webview_hover",
            description: "把**鼠标移动**到内嵌网页视图当前页面的某个坐标（悬停）。\
                          有些站点的菜单是悬停才出现的（例如超星的用户名 →\
                          「个人空间 / 进入空间」），这时要用它而不是 click。\
                          坐标先取元素的位置（例如用 webview_eval 求 getBoundingClientRect）。",
            properties: json!({
                "x": {"type": "number", "description": "横坐标"},
                "y": {"type": "number", "description": "纵坐标"},
            }),
            required: &["x", "y"],
            read_only: false,
            destructive: false,
        },
        ToolDef {
            name: "webview_navigate",
            description: "让内嵌网页视图跳到新网址。",
            properties: json!({
                "url": {"type": "string", "description": "目标网址"},
            }),
            required: &["url"],
            read_only: false,
            destructive: false,
        },
        ToolDef {
            name: "webview_screenshot",
            description: "给内嵌网页视图当前页面截图：返回缩略图（给模型看）与落盘路径。\
                          **要读页面上的小字（试题、表格）就用 clip + scale**：\
                          clip 是 {x,y,width,height}（顶层文档 CSS 像素），\
                          scale 是放大倍数（常用 2）。整视口截图会被压到 1024 宽，字会糊。\
                          坐标用 webview_eval 量元素位置换算（注意是文档坐标，不是视口坐标）。",
            properties: json!({
                "clip": {
                    "type": "object",
                    "description": "可选裁剪区域 {x,y,width,height}，顶层文档 CSS 像素",
                    "properties": {
                        "x": {"type": "number", "description": "左上角 x"},
                        "y": {"type": "number", "description": "左上角 y"},
                        "width": {"type": "number", "description": "宽"},
                        "height": {"type": "number", "description": "高"},
                    },
                },
                "scale": {"type": "number", "description": "放大倍数，默认 1，配合 clip 用 2"},
            }),
            required: &[],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "webview_click_text",
            description: "**按可见文字点击**，并**自动核对页面是否真的变了**。\
                          优先用这个而不是 webview_click（坐标）：它会按文字找到\
                          真正可点的元素、点完比对 URL 与正文指纹，如实回报 \
                          changed=true/false。changed=false 就是「没点上」，\
                          要换一种点法（更精确的文字 / 先滚到可见），**别当成点成功了\
                          继续往下干**。",
            properties: json!({
                "text": {"type": "string", "description": "要点击的可见文字（如「下一节」「1.3 系统的概念」）"},
                "frame": {"type": "string", "description": "目标 iframe 的 URL 片段（可选）"},
            }),
            required: &["text"],
            read_only: false,
            destructive: true,
        },
        ToolDef {
            name: "viewer_ui_state",
            description: "读取**查看器自己**的界面状态：当前目录、路径栏、搜索词、\
                          已打开的标签（id / 名称 / 类型）、当前活动标签、打开了哪些\
                          面板、视图模式。这是 agent 看清「用户面前是什么」的正路。",
            properties: json!({}),
            required: &[],
            read_only: true,
            destructive: false,
        },
        ToolDef {
            name: "viewer_action",
            description: "直接执行**查看器**的动作（不用去盲点坐标）。\
                          支持的 action：open_file{path} / open_url{url} / set_path{path} / \
                          search{query} / switch_tab{id} / close_tab{id} / close_all_tabs / \
                          menu{command}（菜单栏命令 id，如 edit / settings / sys-panel / \
                          browser-panel / kernel-embedded / kernel-system）/ \
                          open_panel{panel: settings|sys-panel|browser|about} / \
                          set_mode{mode: pure|heavy} / read_view。\
                          返回执行结果与执行后的界面状态摘要。",
            properties: json!({
                "action": {"type": "string", "description": "动作名"},
                "args": {"type": "object", "description": "动作参数（按动作名而定）"},
            }),
            required: &["action"],
            read_only: false,
            destructive: false,
        },
    ]
}

fn tool_schema(def: &ToolDef) -> Value {
    json!({
        "name": def.name,
        "description": def.description,
        "inputSchema": {
            "type": "object",
            "properties": def.properties,
            "required": def.required,
        },
        "annotations": {
            "title": def.name,
            "readOnlyHint": def.read_only,
            "destructiveHint": def.destructive,
        },
    })
}

// ── 工具实现 ────────────────────────────────────────────────────────────

/// 文件预览的文本化：文本类给内容，其余给类型+体积摘要。
///
/// 复用 `commands::viewer_read_file` 的分类表（`classify`）——扩展名→类别的
/// 判定只有一份，前端共享层与后端不许分叉（历史上分叉过 57 个扩展名）。
fn preview_text(path: &str) -> Result<String, String> {
    let content = crate::commands::viewer_read_file(path.to_string())?;
    use crate::commands::FileContent;
    Ok(match content {
        FileContent::Text { name, data_base64, .. } => {
            let bytes = crate::commands::base64_decode(&data_base64).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            let mut out = format!("文件：{name}\n");
            out.push_str(&truncate_chars(&text, PREVIEW_CAP));
            out
        }
        FileContent::Image { name, mime, data_base64 } => {
            let size = crate::commands::base64_decode(&data_base64).map(|b| b.len()).unwrap_or(0);
            format!("图片：{name}（{mime}），{size} 字节。图像内容不转文本。")
        }
        FileContent::Pdf { name, data_base64 } => {
            let size = crate::commands::base64_decode(&data_base64).map(|b| b.len()).unwrap_or(0);
            format!("PDF：{name}，{size} 字节。PDF 正文不转文本。")
        }
        FileContent::Bytes { name, extension, data_base64 } => {
            let size = crate::commands::base64_decode(&data_base64).map(|b| b.len()).unwrap_or(0);
            format!("二进制文件：{name}.{extension}，{size} 字节。不转文本。")
        }
        FileContent::Unsupported { name, extension, reason, .. } => {
            format!("{name}.{extension}：{reason}")
        }
    })
}

/// 按字符（不是字节）截断：中文一个字 3 字节，按字节切会切出半个字。
pub(crate) fn truncate_chars(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let mut out: String = text.chars().take(cap).collect();
    out.push_str(&format!("\n…（已截断，共 {} 字符）", text.chars().count()));
    out
}

fn entries_to_text(entries: &[crate::commands::DirEntry]) -> String {
    if entries.is_empty() {
        return "（空）".to_string();
    }
    let mut out = String::new();
    for e in entries {
        out.push_str(&format!(
            "{}\t{}\t{}\t{} 字节\n",
            if e.is_dir { "[目录]" } else { "[文件]" },
            e.name,
            e.path,
            e.size
        ));
    }
    out
}

/// 工具输出：纯文本，或「文本 + 图片」（截图给模型看）。
enum ToolOutput {
    Text(String),
    TextAndImage {
        text: String,
        data_base64: String,
        mime: String,
    },
}

/// 取字符串参数。
fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

/// 取可选的数值参数。
fn arg_num(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| v.as_f64())
}

/// 解析可选的裁剪区域：`{x, y, width, height}`，全为顶层文档 CSS 像素。
fn arg_clip(args: &Value) -> Option<(f64, f64, f64, f64)> {
    let c = args.get("clip")?;
    let get = |k: &str| c.get(k).and_then(|v| v.as_f64());
    let (x, y, w, h) = (get("x")?, get("y")?, get("width")?, get("height")?);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some((x, y, w, h))
}

/// 执行一个工具调用。
///
/// 截图类工具要同时给模型**文字**与**图片**（MCP 的 content 数组支持
/// image 块），所以返回类型是 `ToolOutput` 而不是纯字符串。
fn call_tool(name: &str, args: &Value) -> Result<ToolOutput, String> {
    if name == "browser_screenshot" {
        let session = arg_str(args, "session")?;
        let (path, data_base64, mime) = crate::browser::screenshot(&session)?;
        return Ok(ToolOutput::TextAndImage {
            text: format!("截图已保存：{path}"),
            data_base64,
            mime,
        });
    }
    if name == "webview_screenshot" {
        let clip = arg_clip(args);
        let scale = arg_num(args, "scale").unwrap_or(1.0);
        let (path, data_base64, mime) = crate::webview_cdp::screenshot(clip, scale)?;
        let note = match clip {
            Some((x, y, w, h)) => format!("（裁剪 {x},{y} {w}x{h}，{scale} 倍）"),
            None => String::new(),
        };
        return Ok(ToolOutput::TextAndImage {
            text: format!("内嵌网页视图截图已保存：{path}{note}"),
            data_base64,
            mime,
        });
    }
    call_tool_text(name, args).map(ToolOutput::Text)
}

/// 执行一个工具调用，返回给模型的文本。
fn call_tool_text(name: &str, args: &Value) -> Result<String, String> {
    let str_arg = |key: &str| -> Result<String, String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("缺少参数 {key}"))
    };
    match name {
        "viewer_list_dir" => {
            let path = str_arg("path")?;
            let entries = crate::commands::viewer_list_dir(path.clone(), Some(false))?;
            Ok(entries_to_text(&entries))
        }
        "viewer_read_file" => {
            let path = str_arg("path")?;
            preview_text(&path)
        }
        "viewer_search" => {
            let query = str_arg("query")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            match args.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                Some(path) => {
                    let entries = crate::commands::viewer_search_dir(
                        path.to_string(),
                        query.clone(),
                        Some(false),
                        Some(limit),
                    )?;
                    Ok(entries_to_text(&entries))
                }
                None => {
                    let result = crate::filesearch::global_search(&query, limit)?;
                    let mut out = format!(
                        "全盘索引命中 {} 条（返回 {} 条，用时 {}ms）\n",
                        result.total_matches, result.returned, result.elapsed_ms
                    );
                    out.push_str(&entries_to_text(&result.entries));
                    Ok(out)
                }
            }
        }
        "viewer_write_text" => {
            let path = str_arg("path")?;
            let content = str_arg("content")?;
            let overwrite = args.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);
            let target = std::path::PathBuf::from(&path);
            if target.exists() && !overwrite {
                return Err(format!(
                    "文件已存在：{path}。确认要覆盖请显式传 overwrite=true。"
                ));
            }
            if let Some(parent) = target.parent() {
                if !parent.exists() {
                    return Err(format!("目录不存在：{}", parent.display()));
                }
            }
            std::fs::write(&target, content.as_bytes())
                .map_err(|error| format!("写入失败：{error}"))?;
            Ok(format!(
                "已写入 {}（{} 字节）",
                path,
                content.as_bytes().len()
            ))
        }
        "desktop_list_windows" => {
            let only_visible = args
                .get("only_visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let windows = crate::gui::list_windows(only_visible);
            if windows.is_empty() {
                return Ok("（没有匹配的窗口）".into());
            }
            let mut out = String::new();
            for w in windows {
                out.push_str(&format!(
                    "{}\t{}\t{}\tpid={}\n",
                    w.hwnd, w.title, w.class, w.pid
                ));
            }
            Ok(out)
        }
        "desktop_list_controls" => {
            let hwnd = str_arg("hwnd")?;
            let controls = crate::gui::list_controls(&hwnd)?;
            if controls.is_empty() {
                return Ok("（没有子控件）".into());
            }
            let mut out = String::new();
            for c in controls {
                out.push_str(&format!(
                    "{}\t{}\t{}\tid={}\n",
                    c.hwnd, c.class, c.text, c.id
                ));
            }
            Ok(out)
        }
        "desktop_click" => {
            let hwnd = str_arg("hwnd")?;
            crate::gui::click(&hwnd)
        }
        "desktop_set_text" => {
            let hwnd = str_arg("hwnd")?;
            let text = str_arg("text")?;
            crate::gui::set_text(&hwnd, &text)
        }
        "desktop_read_text" => {
            let hwnd = str_arg("hwnd")?;
            crate::gui::read_text(&hwnd)
        }
        "desktop_press_key" => {
            let hwnd = str_arg("hwnd")?;
            let vk = args.get("vk").and_then(|v| v.as_u64()).unwrap_or(13) as u32;
            crate::gui::press_key(&hwnd, vk)
        }
        "browser_open" => {
            let url = str_arg("url")?;
            let session = args
                .get("session")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string();
            // 有头 / 无头：参数优先；没给就用设置里的默认值
            // （设置 → 通用 → "agent 浏览器默认有头"）。
            let headed = args
                .get("headed")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| {
                    crate::settings::ViewerSettings::load().browser.headed
                });
            let info = crate::browser::open(&session, &url, headed)?;
            Ok(format!(
                "会话 {} 已打开（{}）：{}\n会话 id = {}",
                info.id,
                if info.headed { "有头" } else { "无头" },
                info.url,
                info.id
            ))
        }
        "browser_navigate" => {
            let session = str_arg("session")?;
            let url = str_arg("url")?;
            let final_url = crate::browser::navigate(&session, &url)?;
            Ok(format!("已跳转到 {final_url}"))
        }
        "browser_text" => {
            let session = str_arg("session")?;
            crate::browser::page_text(&session)
        }
        "browser_eval" => {
            let session = str_arg("session")?;
            let expression = str_arg("expression")?;
            crate::browser::eval(&session, &expression)
        }
        "browser_click" => {
            let session = str_arg("session")?;
            let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            crate::browser::click(&session, x, y)
        }
        "browser_sessions" => {
            let list = crate::browser::list();
            if list.is_empty() {
                return Ok("（没有浏览器会话）".into());
            }
            let mut out = String::new();
            for s in list {
                out.push_str(&format!(
                    "{}\t{}\t{}\n",
                    s.id,
                    if s.headed { "有头" } else { "无头" },
                    s.url
                ));
            }
            Ok(out)
        }
        "browser_close" => {
            let session = str_arg("session")?;
            crate::browser::close(&session)?;
            Ok(format!("会话 {session} 已关闭"))
        }
        // ── 内嵌网页视图（通道 A 的 agent 操控面）──
        "web_open" => {
            let url = str_arg("url")?;
            // 浏览器自动化任务**默认纯净模式**：预览区最大、没有资源管理器 /
            // AI 栏干扰，用户看的和 agent 操作的是同一块屏幕。切模式失败不
            // 阻断（查看器可能没开窗口）。
            if let Ok(app) = crate::viewer_control::app() {
                let _ = crate::viewer_control::run_action(
                    &app,
                    "set_mode",
                    json!({"mode": "pure"}),
                );
            }
            crate::webview_host::open_at_last_bounds(&url)?;
            Ok(format!("已在查看器的内嵌网页视图里打开 {url}（用户就在那块预览区里看它）"))
        }
        "web_close" => {
            let app = crate::webview_host::app()?;
            crate::webview_host::close_webview(&app);
            Ok("内嵌网页视图已关闭".to_string())
        }
        "webview_state" => {
            let state = crate::webview_cdp::state()?;
            Ok(format!("{}\t{}", state.title, state.url))
        }
        "webview_text" => crate::webview_cdp::text(),
        "webview_eval" => {
            let expression = str_arg("expression")?;
            let frame = args.get("frame").and_then(|v| v.as_str());
            crate::webview_cdp::eval(&expression, frame)
        }
        "webview_click" => {
            let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            crate::webview_cdp::click(x, y)
        }
        "webview_click_text" => {
            let text = str_arg("text")?;
            let frame = args.get("frame").and_then(|v| v.as_str());
            let result = crate::webview_cdp::click_text(&text, frame)?;
            Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| format!("{result:?}")))
        }
        "webview_hover" => {
            let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            crate::webview_cdp::hover(x, y)
        }
        "webview_navigate" => {
            let url = str_arg("url")?;
            crate::webview_cdp::navigate(&url)
        }
        // ── 查看器自控面（AI 直接操作查看器自己的界面）──
        "viewer_ui_state" => {
            let state = crate::viewer_control::ui_state();
            Ok(serde_json::to_string_pretty(&state).unwrap_or_else(|_| state.to_string()))
        }
        "viewer_action" => {
            let action = str_arg("action")?;
            let args = args.get("args").cloned().unwrap_or(json!({}));
            let app = crate::viewer_control::app()?;
            let result = crate::viewer_control::run_action(&app, &action, args)?;
            Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()))
        }
        other => Err(format!("未知工具：{other}")),
    }
}

// ── JSON-RPC 分派 ───────────────────────────────────────────────────────

fn result(id: Value, payload: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": payload})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// 处理一条请求，返回 `Some(响应)` 或 `None`（通知，无需响应）。
pub fn handle_request(request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
    // 通知（无 id）不回复。
    let id = id?;

    match method {
        "initialize" => {
            let version = request
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Some(result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "hermix-viewer", "version": env!("CARGO_PKG_VERSION")},
                }),
            ))
        }
        "ping" => Some(result(id, json!({}))),
        "tools/list" => {
            let tools: Vec<Value> = tool_defs().iter().map(tool_schema).collect();
            Some(result(id, json!({"tools": tools})))
        }
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if name.is_empty() {
                return Some(error(id, -32602, "tools/call 缺少 name"));
            }
            let outcome = call_tool(name, &args);
            let (content, is_error) = match outcome {
                Ok(ToolOutput::Text(text)) => (
                    json!([{"type": "text", "text": truncate_chars(&text, RESULT_CAP)}]),
                    false,
                ),
                Ok(ToolOutput::TextAndImage {
                    text,
                    data_base64,
                    mime,
                }) => (
                    json!([
                        {"type": "text", "text": truncate_chars(&text, RESULT_CAP)},
                        {"type": "image", "data": data_base64, "mimeType": mime}
                    ]),
                    false,
                ),
                Err(message) => (json!([{"type": "text", "text": message}]), true),
            };
            Some(result(
                id,
                json!({
                    "content": content,
                    "isError": is_error,
                }),
            ))
        }
        "resources/list" => Some(result(id, json!({"resources": []}))),
        "prompts/list" => Some(result(id, json!({"prompts": []}))),
        other => Some(error(id, -32601, &format!("未实现的方法：{other}"))),
    }
}

/// MCP stdio 主循环：stdin 逐行 JSON → stdout 逐行 JSON。
///
/// **stdout 是协议通道**：任何 `println!` 都会污染帧。所以这里只写 stdout，
/// 日志一律走 stderr。
pub fn serve_stdio() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    eprintln!("[viewer-mcp] Hermix 文件查看器 MCP 服务已启动（stdio）");
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            let resp = error(Value::Null, -32700, "解析失败：不是合法 JSON");
            let _ = writeln!(stdout, "{resp}");
            let _ = stdout.flush();
            continue;
        };
        if let Some(resp) = handle_request(&request) {
            let _ = writeln!(stdout, "{resp}");
            let _ = stdout.flush();
        }
    }
    eprintln!("[viewer-mcp] stdin 关闭，服务退出");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    }

    #[test]
    fn initialize_returns_protocol_version_and_tools_capability() {
        let resp = handle_request(&call("initialize", json!({"protocolVersion": "2025-03-26"})))
            .expect("response");
        assert_eq!(resp["result"]["protocolVersion"], "2025-03-26");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_exposes_file_and_gui_tools() {
        let resp = handle_request(&call("tools/list", json!({}))).expect("response");
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"viewer_read_file"));
        assert!(names.contains(&"viewer_search"));
        assert!(names.contains(&"viewer_write_text"));
        assert!(names.contains(&"desktop_click"));
    }

    #[test]
    fn write_tool_requires_explicit_overwrite_for_existing_file() {
        let dir = std::env::temp_dir().join(format!("hermix-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("a.txt");
        std::fs::write(&path, "old").expect("write");

        let resp = handle_request(&call(
            "tools/call",
            json!({
                "name": "viewer_write_text",
                "arguments": {"path": path.to_string_lossy(), "content": "new"},
            }),
        ))
        .expect("response");
        assert_eq!(resp["result"]["isError"], true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");

        let resp = handle_request(&call(
            "tools/call",
            json!({
                "name": "viewer_write_text",
                "arguments": {
                    "path": path.to_string_lossy(),
                    "content": "new",
                    "overwrite": true,
                },
            }),
        ))
        .expect("response");
        assert_eq!(resp["result"]["isError"], false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let resp = handle_request(&call("nope", json!({}))).expect("response");
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn notification_without_id_gets_no_response() {
        let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(handle_request(&notification).is_none());
    }
}
