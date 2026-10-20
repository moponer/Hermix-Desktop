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
fn truncate_chars(text: &str, cap: usize) -> String {
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

/// 执行一个工具调用，返回给模型的文本。
fn call_tool(name: &str, args: &Value) -> Result<String, String> {
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
            let (text, is_error) = match outcome {
                Ok(text) => (truncate_chars(&text, RESULT_CAP), false),
                Err(message) => (message, true),
            };
            Some(result(
                id,
                json!({
                    "content": [{"type": "text", "text": text}],
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
