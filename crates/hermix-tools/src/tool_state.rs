//! 世界状态探测（L1）：把一次工具调用之后的**可观测世界状态**提取成签名。
//!
//! 这个签名由执行层写进 `ToolResult::state`，agent 的零推进账本
//! （`hermix_agent::progress_ledger`）只读那个字段 —— 状态是**工具执行的
//! 事实**，不是 agent 从返回文本里猜出来的副产物。
//!
//! ## 为什么按族取信
//!
//! 「世界是什么样」只有两类可信来源：**外部世界的真实元数据**（文件
//! `stat`、进程退出码、页面 URL/标题）与工具自己的返回值。后者不可信 ——
//! 工具经常把「我发出了请求」当成「事情发生了」（2026-11-05：
//! `webview_navigate` 返回请求的 URL，页面其实被重定向到登录页）。
//!
//! | 族 | 签名来源 | 例子 |
//! |---|---|---|
//! | 文件 | 磁盘 `stat`（不是工具返回的字符串） | `fs:D:\a.txt;exists=true;size=1203;mtime=1730…` |
//! | shell | 命令 + 输出指纹（抹掉数字抖动） | `sh:git status\|a3f2…` |
//! | 浏览器 | 落点 URL + 标题 | `web:url=https://…;title=温馨提示` |
//!
//! 未覆盖的工具返回 `None` —— 它们不参与零推进判定。宁可少判，不可误判：
//! 把「模型在正常干活」误判成空转会打断真实工作，比漏一次严重得多。

/// 文件系统族：签名取自磁盘元数据。
const FS_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "patch",
    "apply_patch",
    "move_file",
    "delete_file",
    "list_files",
    "list_dir",
    "search_files",
    "create_directory",
];

/// shell 族：签名 = 命令 + 输出指纹。
const SHELL_TOOLS: &[&str] = &["terminal", "execute_command", "run_command"];

/// 浏览器 / 内嵌视图族：签名 = 落点 URL + 标题（或正文指纹）。
const WEB_TOOLS: &[&str] = &[
    "webview_navigate",
    "webview_state",
    "webview_text",
    "webview_eval",
    "webview_click",
    "webview_click_text",
    "web_open",
    "web_close",
    "browser_open",
    "browser_navigate",
    "browser_text",
    "browser_eval",
];

/// MCP 工具名带命名空间前缀（`文件查看器（Hermix 自带）__webview_navigate`），
/// 只取最后一段比较。
fn bare_name(tool: &str) -> &str {
    tool.rsplit("__").next().unwrap_or(tool)
}

/// 状态归一化：抹掉数字串与空白，保留结构。
///
/// 页面上的 `09:56:12`、`耗时 327ms`、`第 14 轮` 每轮都不同；不归一化的话
/// 「同一个登录页」每次都是新状态，零推进永远检测不出来。
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_digits = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('#');
                in_digits = true;
            }
            continue;
        }
        in_digits = false;
        if ch.is_whitespace() {
            continue;
        }
        out.push(ch);
    }
    out
}

/// 归一化后的短指纹（16 位 hex）。用于没法结构化提取的族。
fn fingerprint(text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    normalize(text).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 探测一次调用的世界状态签名。`None` = 这个族不参与判定。
///
/// `args` 是模型下发的原始参数串（可能不是合法 JSON —— 那就按文本处理）。
pub fn signature(tool: &str, args: &str, content: &str) -> Option<String> {
    let name = bare_name(tool).to_ascii_lowercase();
    if FS_TOOLS.iter().any(|t| name == *t) {
        return fs_signature(args);
    }
    if SHELL_TOOLS.iter().any(|t| name == *t) {
        return Some(shell_signature(args, content));
    }
    if WEB_TOOLS.iter().any(|t| name == *t) {
        return Some(web_signature(content));
    }
    None
}

/// 剥开 MCP 结构化信封：`{"hermix_tool_result": {"content": "...", "state": "..."}}`。
///
/// 返回 `(给模型看的正文, 世界状态)`；不是信封就原样返回。键名由
/// `hermix-mcp` 自己生成，普通工具的 JSON 输出不会撞上 —— 这样「状态」在
/// 整条链路上都是字段，不需要任何一方去解析对方的中文文案。
pub fn unwrap_envelope(output: &str) -> (String, Option<String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return (output.to_string(), None);
    };
    let Some(inner) = value.get("hermix_tool_result") else {
        return (output.to_string(), None);
    };
    match (
        inner.get("content").and_then(|c| c.as_str()),
        inner.get("state").and_then(|s| s.as_str()),
    ) {
        (Some(content), Some(state)) => (content.to_string(), Some(state.to_string())),
        _ => (output.to_string(), None),
    }
}

/// 从参数里取第一个可用的路径（各工具的参数名不一致，按常见次序试）。
fn first_path(args: &str) -> Option<std::path::PathBuf> {
    let value: serde_json::Value = serde_json::from_str(args).ok()?;
    for key in ["path", "file_path", "target", "source", "destination", "dir"] {
        if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return Some(std::path::PathBuf::from(s));
            }
        }
    }
    if let Some(first) = value
        .get("paths")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
    {
        if !first.trim().is_empty() {
            return Some(std::path::PathBuf::from(first));
        }
    }
    None
}

/// 文件族签名：**磁盘上的**存在性 / 大小 / mtime，与工具返回的文案无关。
///
/// 读同一个未改动的文件两次 → 签名相同 → 零推进（正确）；写文件后再读 →
/// size/mtime 变了 → 新状态（正确）。
fn fs_signature(args: &str) -> Option<String> {
    let path = first_path(args)?;
    let meta = std::fs::metadata(&path).ok();
    let (exists, size, mtime_ms) = match &meta {
        Some(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            (true, m.len(), mtime)
        }
        None => (false, 0, 0),
    };
    Some(format!(
        "fs:{};exists={exists};size={size};mtime={mtime_ms}",
        path.display()
    ))
}

/// shell 族签名：命令本身 + 输出指纹。
///
/// 命令变了（换路了）签名就变，这是**正确的**「新状态」；命令与输出都没变，
/// 说明这条命令再跑一次也不会带来新信息。
fn shell_signature(args: &str, content: &str) -> String {
    let command = serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|v| {
            v.get("command")
                .or_else(|| v.get("cmd"))
                .and_then(|c| c.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| args.to_string());
    format!("sh:{}|{}", normalize(&command), fingerprint(content))
}

/// 浏览器族签名：优先解析常见返回文案里的落点 URL，最后退回正文指纹。
///
/// 内嵌视图那几条工具（`webview_navigate` 等）不依赖这里 —— 它们把落点
/// 通过 MCP `_meta` 结构化交出来，由 [`unwrap_envelope`] 直接取到。这里是
/// 给系统浏览器工具（`browser_open` / `browser_navigate`）兜底。
fn web_signature(content: &str) -> String {
    if let Some((_, rest)) = content.split_once("已跳转到 ") {
        let url = rest.lines().next().unwrap_or("").trim();
        if url.starts_with("http") {
            return format!("web:url={url}");
        }
    }
    if let Some((_, rest)) = content.split_once("已打开（") {
        // `会话 x 已打开（有头）：https://…`
        if let Some((_, url)) = rest.split_once('：') {
            let url = url.lines().next().unwrap_or("").trim();
            if url.starts_with("http") {
                return format!("web:url={url}");
            }
        }
    }
    format!("web:{}", fingerprint(content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_signature_tracks_disk_not_tool_text() {
        let dir = std::env::temp_dir().join(format!("hermix-tool-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let args = format!(
            r#"{{"path":{}}}"#,
            serde_json::to_string(&file.display().to_string()).unwrap()
        );

        let first = signature("read_file", &args, "工具说读到了 hello").expect("signature");
        // 同样的磁盘状态，工具文案不同 —— 签名必须一致。
        let second = signature("read_file", &args, "完全不同的一段返回文案").expect("signature");
        assert_eq!(first, second);
        assert!(first.contains("size=5"));

        std::fs::write(&file, "hello world").unwrap();
        let third = signature("read_file", &args, "hello world").expect("signature");
        assert_ne!(first, third, "磁盘内容变了必须被视为新状态");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_a_stable_state() {
        let args = r#"{"path":"D:\\does-not-exist-hermix\\x.txt"}"#;
        let a = signature("read_file", args, "文件不存在").expect("signature");
        let b = signature("read_file", args, "文件不存在").expect("signature");
        assert_eq!(a, b);
        assert!(a.contains("exists=false"));
    }

    #[test]
    fn shell_signature_ignores_numeric_jitter() {
        let args = r#"{"command":"git status"}"#;
        let a = signature("terminal", args, "took 312ms, 12 files").expect("signature");
        let b = signature("terminal", args, "took 998ms, 47 files").expect("signature");
        assert_eq!(a, b, "纯数字差异不算新状态");
        // 换命令 → 新状态。
        let c = signature("terminal", r#"{"command":"git log"}"#, "took 312ms, 12 files").unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn envelope_carries_state_out_of_band() {
        let raw = r#"{"hermix_tool_result":{"content":"已导航到 https://x/y","state":"url=https://x/y | title=温馨提示"}}"#;
        let (content, state) = unwrap_envelope(raw);
        assert_eq!(content, "已导航到 https://x/y", "模型只看到正文");
        assert_eq!(
            state.as_deref(),
            Some("url=https://x/y | title=温馨提示"),
            "世界状态走字段"
        );

        // 普通输出（包括形状相近的普通 JSON）不得被误剥。
        let plain = r#"{"content":"a","state":"b"}"#;
        assert_eq!(unwrap_envelope(plain), (plain.to_string(), None));
        assert_eq!(unwrap_envelope("hello"), ("hello".to_string(), None));
    }

    #[test]
    fn browser_signature_falls_back_to_the_landing_url() {
        let a = signature("browser_navigate", "{}", "已跳转到 https://a.example/x").unwrap();
        assert_eq!(a, "web:url=https://a.example/x");
    }

    #[test]
    fn unknown_tools_do_not_participate() {
        assert!(signature("memory", "{}", "saved").is_none());
        assert!(signature("todo", "{}", "3 items").is_none());
    }
}
