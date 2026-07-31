use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::errors::AgentError;
use crate::types::{FunctionCall, ToolCall};

// ---------------------------------------------------------------------------
// ToolCallParser trait
// ---------------------------------------------------------------------------

/// Trait for extensible tool call parsers.
pub trait ToolCallParser: Send + Sync {
    /// Parse tool calls from raw LLM text content.
    fn parse(&self, content: &str) -> Result<Vec<ToolCall>, AgentError>;

    /// Clone this parser into a boxed value.
    fn clone_box(&self) -> Box<dyn ToolCallParser>;
}

// ---------------------------------------------------------------------------
// Hermes XML parser implementation
// ---------------------------------------------------------------------------

/// The default Hermes-format tool call parser.
/// Parses `<function_calls>` XML blocks containing `<invoke>` and
/// `<parameter>` elements.
#[derive(Clone)]
pub struct HermesToolCallParser;

static CALL_ID_COUNTER: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

fn next_call_id() -> String {
    let mut guard = CALL_ID_COUNTER.lock().unwrap();
    *guard += 1;
    format!("call_{}", guard)
}

/// Parse MiniMax M3 native tool-call format:
/// `<terminal><command>ls -la</command></terminal>`
/// Uses manual byte-scanning with nesting tracking because regex
/// alone cannot correctly match the outer close tag when the inner
/// content also contains `</word>` tags.
fn parse_m3_tool_tags(content: &str, calls: &mut Vec<ToolCall>) {
    let bytes = content.as_bytes();
    let mut pos = 0usize;

    while pos < bytes.len() {
        // Find '<' that starts a potential tool tag
        let Some(tag_start) = bytes[pos..].iter().position(|&b| b == b'<') else {
            break;
        };
        let abs_tag_start = pos + tag_start;

        // Read tag name (alphanumeric + underscore)
        let name_start = abs_tag_start + 1;
        let mut name_end = name_start;
        while name_end < bytes.len() && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_') {
            name_end += 1;
        }
        if name_end == name_start {
            pos = abs_tag_start + 1;
            continue; // empty tag name, skip
        }
        let name = &content[name_start..name_end];

        // Skip known non-tool tags
        if name.eq_ignore_ascii_case("minimax")
            || name.eq_ignore_ascii_case("invoke")
            || name.eq_ignore_ascii_case("parameter")
            || name.eq_ignore_ascii_case("function_calls")
            || name.eq_ignore_ascii_case("think")
        {
            pos = abs_tag_start + 1;
            continue;
        }

        // Skip whitespace and find '>'
        let mut i = name_end;
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'>' {
            pos = abs_tag_start + 1;
            continue; // malformed, skip
        }
        i += 1; // skip '>'

        // Now scan for matching close tag `</name>`, handling
        // nested tags inside (e.g. `<command>...</command>`).
        let close_pattern = format!("</{}>", name);
        let close_bytes = close_pattern.as_bytes();

        let inner_start = i;
        let mut scan_pos = i;
        let mut found_close = None;

        while scan_pos + close_bytes.len() <= bytes.len() {
            if bytes[scan_pos] == b'<' {
                // Check if this is our close tag
                if &bytes[scan_pos..scan_pos + close_bytes.len().min(bytes.len() - scan_pos)] == close_bytes {
                    found_close = Some(scan_pos);
                    break;
                }
                // It's some other XML tag. Scan past it.
                // Find the matching '>'
                scan_pos += 1;
                // Skip tag name
                while scan_pos < bytes.len() && (bytes[scan_pos].is_ascii_alphanumeric() || bytes[scan_pos] == b'_' || bytes[scan_pos] == b'/' || bytes[scan_pos] == b' ' || bytes[scan_pos] == b'"' || bytes[scan_pos] == b'=') {
                    if bytes[scan_pos] == b'>' {
                        break;
                    }
                    scan_pos += 1;
                }
                if scan_pos < bytes.len() && bytes[scan_pos] == b'>' {
                    scan_pos += 1;
                }
                continue;
            }
            scan_pos += 1;
        }

        let Some(close_pos) = found_close else {
            // No matching close tag found — skip this '<'
            pos = abs_tag_start + 1;
            continue;
        };

        let inner = &content[inner_start..close_pos];

        // Parse inner parameter tags: `<command>ls -la</command>`
        // Use the existing tag_re for extracting param name/value pairs
        let tag_re = Regex::new(r#"(?s)<([a-z_][a-z0-9_]*)(?:\s+[^>]*)?>(.*?)</([a-z_][a-z0-9_]*)>"#).unwrap();
        let mut args = serde_json::Map::new();
        for tag_caps in tag_re.captures_iter(inner) {
            let pname = tag_caps.get(1).unwrap().as_str();
            let pval = tag_caps.get(2).unwrap().as_str().trim().to_string();
            let close = tag_caps.get(3).unwrap().as_str();
            if !pname.eq_ignore_ascii_case(close) { continue; }
            if !pname.is_empty() && !pval.is_empty() {
                let val: serde_json::Value =
                    serde_json::from_str(&pval).unwrap_or(serde_json::Value::String(pval));
                args.insert(pname.to_string(), val);
            }
        }

        if !args.is_empty() {
            calls.push(ToolCall {
                id: next_call_id(),
                function: FunctionCall {
                    name: name.to_string(),
                    arguments: serde_json::Value::Object(args).to_string(),
                },
            });
        }

        // Advance past the close tag
        pos = close_pos + close_bytes.len();
    }
}

impl ToolCallParser for HermesToolCallParser {
    fn parse(&self, content: &str) -> Result<Vec<ToolCall>, AgentError> {
        let mut calls = Vec::new();

        // Format 1 (MiniMax-M2 / MiniMax / older Hermes fine-tunes):
        // the well-formed `<minimax:tool_call>…<invoke>…</invoke>…</minimax:tool_call>`
        // wrapper. We try this first because it is the *narrowest*
        // match (specific tag name) and the wrappers never nest, so
        // getting it first avoids accidentally trying to parse the
        // same invoke twice via the looser fallback regexes below.
        // The LLM in the wild occasionally drops the closing
        // `</minimax:tool_call>` (the model runs out of tokens mid
        // block) or omits the leading `<` (text-only continuation
        // after a tool-call). Both shapes are still matched by the
        // invoke extractor further down.
        // Crucially: if the model emits `<minimax:tool_call>` or even
        // just `<invoke name="X">` but the stream cuts mid-block
        // (no closing tags), the block_re and the nested `<parameter>`
        // extractors below produce **zero** matches — there's no
        // `</invoke>`, no `</minimax:tool_call>`, and the parameter
        // content is empty. We need a *best-effort* fallback: when the
        // structured extraction fails, look for a bare `<invoke
        // name="X">` tag (even with empty body) and record it so the
        // agent loop knows a tool call was attempted, rather than
        // silently skipping to a next-turn re-pick attempt.
        {
            let block_re = Regex::new(r"(?is)<\s*minimax:tool_call\s*>(.*?)</\s*minimax:tool_call\s*>").unwrap();
            let invoke_re = Regex::new(r#"(?is)<\s*invoke\s+name\s*=\s*"([^"]+)"\s*>(.*?)</\s*invoke\s*>"#).unwrap();
            let param_re = Regex::new(r#"(?is)<\s*parameter\s+name\s*=\s*"([^"]+)"\s*>(.*?)</\s*parameter\s*>"#).unwrap();
            // Same backreference workaround as the `tag_re` below.
            let tag_re = Regex::new(r#"(?is)<([A-Za-z_][\w-]*)[^>]*?>(.*?)</([A-Za-z_][\w-]*)\s*>"#).unwrap();

            for block_caps in block_re.captures_iter(content) {
                let block = block_caps.get(1).unwrap().as_str();
                for inv_caps in invoke_re.captures_iter(block) {
                    let name = inv_caps.get(1).unwrap().as_str().to_string();
                    let params_block = inv_caps.get(2).unwrap().as_str();

                    let mut args = serde_json::Map::new();
                    for param_caps in param_re.captures_iter(params_block) {
                        let pname = param_caps.get(1).unwrap().as_str().to_string();
                        let pval = param_caps.get(2).unwrap().as_str().trim().to_string();
                        let val: serde_json::Value =
                            serde_json::from_str(&pval).unwrap_or(serde_json::Value::String(pval));
                        args.insert(pname, val);
                    }
                    // Fallback for the parameter-as-named-tag form
                    // (`<command>ls</command>`) in case the model used
                    // that style inside the wrapper.
                    if args.is_empty() {
                        for tag_caps in tag_re.captures_iter(params_block) {
                            let pname = tag_caps.get(1).unwrap().as_str();
                            let pval = tag_caps.get(2).unwrap().as_str().trim().to_string();
                            let close_name = tag_caps.get(3).unwrap().as_str();
                            if pname != close_name { continue; }
                            if !pname.is_empty() && !pval.is_empty() {
                                let val: serde_json::Value =
                                    serde_json::from_str(&pval).unwrap_or(serde_json::Value::String(pval));
                                args.insert(pname.to_string(), val);
                            }
                        }
                    }

                    calls.push(ToolCall {
                        id: next_call_id(),
                        function: FunctionCall {
                            name,
                            arguments: serde_json::Value::Object(args).to_string(),
                        },
                    });
                }
            }
        }

        // Format 2 (Hermes standard): the classic
        // `<function_calls><invoke name="X"><parameter name="Y">V</parameter></invoke></function_calls>`.
        let func_calls_re = Regex::new(r"(?s)<function_calls>(.*?)</function_calls>").unwrap();
        let invoke_re = Regex::new(r#"(?s)<invoke\s+name="([^"]+)">(.*?)</invoke>"#).unwrap();
        let param_re = Regex::new(r#"<parameter\s+name="([^"]+)">(.*?)</parameter>"#).unwrap();

        for fc_caps in func_calls_re.captures_iter(content) {
            let block = fc_caps.get(1).unwrap().as_str();
            for inv_caps in invoke_re.captures_iter(block) {
                let name = inv_caps.get(1).unwrap().as_str().to_string();
                let params_block = inv_caps.get(2).unwrap().as_str();

                let mut args = serde_json::Map::new();
                for param_caps in param_re.captures_iter(params_block) {
                    let pname = param_caps.get(1).unwrap().as_str().to_string();
                    let pval = param_caps.get(2).unwrap().as_str().trim().to_string();
                    let val: serde_json::Value =
                        serde_json::from_str(&pval).unwrap_or(serde_json::Value::String(pval));
                    args.insert(pname, val);
                }

                calls.push(ToolCall {
                    id: next_call_id(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::Value::Object(args).to_string(),
                    },
                });
            }
        }

        // Also try ```tool_call blocks if no XML calls were found
        if calls.is_empty() {
            let tool_call_re = Regex::new(r"(?s)```tool_call\s*\n(.*?)\n```").unwrap();
            for caps in tool_call_re.captures_iter(content) {
                let raw = caps.get(1).unwrap().as_str().trim();
                let parsed: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
                    AgentError::InvalidToolCall(format!("Invalid JSON in tool_call block: {}", e))
                })?;

                let name = parsed
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentError::InvalidToolCall(
                            "Missing 'name' field in tool_call block".to_string(),
                        )
                    })?
                    .to_string();

                let arguments = parsed
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
                    .to_string();

                calls.push(ToolCall {
                    id: next_call_id(),
                    function: FunctionCall { name, arguments },
                });
            }
        }

        // Also try minimax-style: <invoke name="tool"><param_name>value</param_name></invoke>
        // where param names are used as tag names directly (e.g. <command>ls</command>)
        // Also handles stray quotes in tag names like <command">ls</command>
        if calls.is_empty() {
            let invoke_re = Regex::new(r#"(?s)<invoke\s+name="([^"]+)">(.*?)</invoke>"#).unwrap();
            // Tag-name match: capture both the opening and closing tag
            // names and verify in code that they match. We can't use a
            // backreference (`</\1>`) here because Rust's `regex` crate
            // does not support them — see
            // https://docs.rs/regex/latest/regex/#grouping-and-flags.
            // The same workaround is used in the `<minimax:tool_call>`
            // branch above.
            let tag_re = Regex::new(r#"(?s)<([A-Za-z_][\w-]*)[^>]*?>(.*?)</([A-Za-z_][\w-]*)\s*>"#).unwrap();

            for inv_caps in invoke_re.captures_iter(content) {
                let name = inv_caps.get(1).unwrap().as_str().to_string();
                let params_block = inv_caps.get(2).unwrap().as_str();

                let mut args = serde_json::Map::new();
                for tag_caps in tag_re.captures_iter(params_block) {
                    let pname = tag_caps.get(1).unwrap().as_str();
                    let pval = tag_caps.get(2).unwrap().as_str().trim().to_string();
                    let close_name = tag_caps.get(3).unwrap().as_str();
                    // The Rust `regex` crate doesn't support
                    // backreferences, so enforce open/close tag
                    // equality in code.
                    if pname != close_name { continue; }
                    if !pname.is_empty() && !pval.is_empty() {
                        let val: serde_json::Value =
                            serde_json::from_str(&pval).unwrap_or(serde_json::Value::String(pval));
                        args.insert(pname.to_string(), val);
                    }
                }

                if !args.is_empty() {
                    calls.push(ToolCall {
                        id: next_call_id(),
                        function: FunctionCall {
                            name,
                            arguments: serde_json::Value::Object(args).to_string(),
                        },
                    });
                }
            }
        }

        // Format 5 (MiniMax M3 native): tool name as outer XML tag,
        // parameters as child tags. Uses manual byte-scanning with
        // nesting tracking to correctly match open/close tag pairs.
        if calls.is_empty() {
            parse_m3_tool_tags(content, &mut calls);
        }

        //Format 6 — *best-effort* unclosed-block extractor .
        // When MiniMax-M3 (or M2.x / older Hermes fine-tunes) hits
        // finish_reason="stop" while still inside a tool-call XML
        // block, the structured extractors above return nothing:
        // - `</invoke>` is missing → Format 1 block_re fails
        // - `</function_calls>` is missing → Format 2 fails
        // - `<invoke>...</invoke>` is missing → Format 4 fails
        // In this case we do a *single-pass* scan for any `<invoke
        // name="X">` (even with empty body) and record a skeletal
        // ToolCall so the agent loop knows **something was attempted**.
        // The agent loop's `decide_pre_tool_dispatch` will then handle
        // the incomplete tool call through the normal approval path
        // (which will likely deny it — "no args for terminal") rather
        // than silently skipping to the next turn and wasting tokens.
        if calls.is_empty() {
            let bare_invoke_re = Regex::new(r#"(?s)<\s*invoke\s+name\s*=\s*"([^"]+)"\s*>"#).unwrap();
            for caps in bare_invoke_re.captures_iter(content) {
                let name = caps.get(1).unwrap().as_str().to_string();
                calls.push(ToolCall {
                    id: next_call_id(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::Value::Object(serde_json::Map::new()).to_string(),
                    },
                });
            }

            // Also catch unclosed MiniMax M3 native tags like
            // `<terminal>` at the very end of content (no `</terminal>`).
            if calls.is_empty() {
                parse_m3_tool_tags(content, &mut calls);
            }

            // If still nothing, look for a bare M3 tag name at the
            // very end of the content with a `>` that hasn't closed.
            // e.g. `<terminal>` (complete opener, no close tag) or
            // `<terminal` (stream cut mid-tag).
            if calls.is_empty() {
                // Match `<tagname` or `<tagname>` at end of content.
                let unclosed_m3_re =
                    Regex::new(r"<\s*([A-Za-z_][\w-]*)\s*>?\s*$").unwrap();
                if let Some(caps) = unclosed_m3_re.captures(content) {
                    let tag_name = caps.get(1).unwrap().as_str().to_string();
                    // Only record known tool-like tag names.
                    let known_tools = [
                        "terminal", "read_file", "write_file", "browser",
                        "computer_use", "execute_code", "list_files",
                        "process_registry", "web_extract", "web_search",
                        "delegate_task", "list_tools",
                    ];
                    if known_tools.iter().any(|t| tag_name.eq_ignore_ascii_case(t)) {
                        calls.push(ToolCall {
                            id: next_call_id(),
                            function: FunctionCall {
                                name: tag_name,
                                arguments: serde_json::Value::Object(
                                    serde_json::Map::new(),
                                ).to_string(),
                            },
                        });
                    }
                }
            }
        }

        Ok(calls)
    }

    fn clone_box(&self) -> Box<dyn ToolCallParser> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Public parsing functions
// ---------------------------------------------------------------------------

/// Parse all tool calls from LLM text content using the default Hermes parser.
pub fn parse_tool_calls(content: &str) -> Result<Vec<ToolCall>, AgentError> {
    let parser = HermesToolCallParser;
    parser.parse(content)
}

/// Strip MiniMax M3 native tool-call tags from text content.
/// Removes blocks like `<terminal><command>ls</command></terminal>`
/// and `<read_file><path>/tmp</path></read_file>` where the outer
/// tag name is the tool name and inner tags are parameters.
fn remove_m3_tool_tags(text: &mut String) {
    // Use manual scanning to correctly match open/close tag pairs.
    let mut result = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut pos = 0usize;

    while pos < bytes.len() {
        // Find next '<'
        let Some(tag_start) = bytes[pos..].iter().position(|&b| b == b'<') else {
            // No more '<' — copy remaining text
            result.push_str(&text[pos..]);
            break;
        };
        let abs_tag_start = pos + tag_start;

        // Copy text before '<'
        if abs_tag_start > pos {
            result.push_str(&text[pos..abs_tag_start]);
        }

        // Read tag name
        let name_start = abs_tag_start + 1;
        let mut name_end = name_start;
        while name_end < bytes.len()
            && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_')
        {
            name_end += 1;
        }
        if name_end == name_start {
            // Empty tag, copy '<' and continue
            result.push('<');
            pos = abs_tag_start + 1;
            continue;
        }
        let name = &text[name_start..name_end];

        // Skip known non-tool / HTML-like tags
        if name.eq_ignore_ascii_case("minimax")
            || name.eq_ignore_ascii_case("invoke")
            || name.eq_ignore_ascii_case("parameter")
            || name.eq_ignore_ascii_case("function_calls")
            || name.eq_ignore_ascii_case("think")
            || name.eq_ignore_ascii_case("b")
            || name.eq_ignore_ascii_case("i")
            || name.eq_ignore_ascii_case("u")
            || name.eq_ignore_ascii_case("a")
            || name.eq_ignore_ascii_case("p")
            || name.eq_ignore_ascii_case("div")
            || name.eq_ignore_ascii_case("span")
            || name.eq_ignore_ascii_case("br")
            || name.eq_ignore_ascii_case("hr")
        {
            // Not a tool tag — copy '<' and continue
            result.push('<');
            pos = abs_tag_start + 1;
            continue;
        }

        // Skip '>' — need to handle optional whitespace before '>'
        let mut i = name_end;
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'>' {
            // Malformed — copy what we have and continue
            result.push_str(&text[abs_tag_start..name_end]);
            pos = name_end;
            continue;
        }
        let after_open = i + 1; // position after '>'

        // Check if the content after the opening tag contains nested XML.
        // If yes, try to find the matching close tag; otherwise it's prose.
        // Quick check: is there a '<' within the next N bytes?
        let lookahead_limit = (after_open + 256).min(bytes.len());
        let has_nested = bytes[after_open..lookahead_limit].iter().any(|&b| b == b'<');

        if !has_nested {
            // No nested XML — this is prose like "<terminal> is the window"
            result.push_str(&text[abs_tag_start..after_open]);
            pos = after_open;
            continue;
        }

        // Find matching close tag `</name>`
        let close_pattern = format!("</{}>", name);
        let close_bytes = close_pattern.as_bytes();
        let mut found_close = None;
        let mut scan_pos = after_open;
        while scan_pos + close_bytes.len() <= bytes.len() {
            if bytes[scan_pos] == b'<' {
                if scan_pos + close_bytes.len() <= bytes.len()
                    && &bytes[scan_pos..scan_pos + close_bytes.len()] == close_bytes
                {
                    found_close = Some(scan_pos);
                    break;
                }
                // Skip past this tag
                scan_pos += 1;
                while scan_pos < bytes.len()
                    && bytes[scan_pos] != b'>'
                {
                    scan_pos += 1;
                }
                if scan_pos < bytes.len() {
                    scan_pos += 1; // skip '>'
                }
                continue;
            }
            scan_pos += 1;
        }

        if let Some(close_pos) = found_close {
            // Strip the whole block — don't add anything to result
            pos = close_pos + close_bytes.len();
        } else {
            // No close tag found — it might be prose, copy '<' and continue
            result.push('<');
            pos = abs_tag_start + 1;
        }
    }

    *text = result;
}

/// Separate plain text from tool calls in LLM output content.
/// Returns a tuple of (plain_text, tool_calls).
pub fn separate_text_and_calls(content: &str) -> (String, Vec<ToolCall>) {
    let calls = parse_tool_calls(content).unwrap_or_default();
    let mut result = content.to_string();

    // Remove <minimax:tool_call>...</minimax:tool_call> blocks
    // (MiniMax-M2 / MiniMax native format).
    let minimax_re = Regex::new(r"(?is)<\s*minimax:tool_call\s*>.*?</\s*minimax:tool_call\s*>").unwrap();
    result = minimax_re.replace_all(&result, "").into_owned();

    // Remove <function_calls>...</function_calls> blocks
    let func_calls_re = Regex::new(r"(?s)<function_calls>.*?</function_calls>").unwrap();
    result = func_calls_re.replace_all(&result, "").to_string();

    // Remove ```tool_call ... ``` blocks
    let tool_call_re = Regex::new(r"(?s)```tool_call\s*\n.*?\n```").unwrap();
    result = tool_call_re.replace_all(&result, "").to_string();

    // Trim excessive whitespace left behind
    // Also strip MiniMax M3 native format: <terminal><command>ls</command></terminal>
    remove_m3_tool_tags(&mut result);
    let result = result.trim().to_string();

    (result, calls)
}

// ---------------------------------------------------------------------------
// Extensible parser registry
// ---------------------------------------------------------------------------

static PARSER_REGISTRY: LazyLock<Mutex<HashMap<String, Box<dyn ToolCallParser>>>> =
    LazyLock::new(|| {
        let mut map: HashMap<String, Box<dyn ToolCallParser>> = HashMap::new();
        map.insert("hermes".to_string(), Box::new(HermesToolCallParser));
        Mutex::new(map)
    });

/// Register a custom parser by name.
pub fn register_parser(name: impl Into<String>, parser: Box<dyn ToolCallParser>) {
    let mut registry = PARSER_REGISTRY.lock().unwrap();
    registry.insert(name.into(), parser);
}

/// Retrieve a parser by name from the global registry.
pub fn get_parser(name: &str) -> Option<Box<dyn ToolCallParser>> {
    let registry = PARSER_REGISTRY.lock().unwrap();
    registry.get(name).map(|p| p.clone_box())
}

// ---------------------------------------------------------------------------
// Round-trip formatting
// ---------------------------------------------------------------------------

/// Format a list of tool calls back into the Hermes `<function_calls>` XML format.
/// This enables round-trip fidelity: parsing then formatting should be
/// semantically equivalent to the original input.
pub fn format_tool_calls(calls: &[ToolCall]) -> String {
    if calls.is_empty() {
        return String::new();
    }

    let mut buf = String::from("<function_calls>\n");
    for tc in calls {
        buf.push_str(&format!("<invoke name=\"{}\">\n", tc.function.name));
        let args: serde_json::Value =
            serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(map) = &args {
            for (key, val) in map {
                let val_str = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                buf.push_str(&format!(
                    "<parameter name=\"{}\">{}</parameter>\n",
                    key, val_str
                ));
            }
        }
        buf.push_str("</invoke>\n");
    }
    buf.push_str("</function_calls>");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hermes_xml_format() {
        let content = r#"
I'll look that up for you.

<function_calls>
<invoke name="search">
<parameter name="query">rust async traits</parameter>
</invoke>
</function_calls>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "search");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["query"], "rust async traits");
    }

    #[test]
    fn test_parse_tool_call_json_format() {
        let content = r#"
Let me run that.
```tool_call
{"name": "calculator", "arguments": {"expr": "2+2"}}
```
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "calculator");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["expr"], "2+2");
    }

    #[test]
    fn test_invalid_json_in_tool_call_tags() {
        let content = r#"
```tool_call
{bad json!!!}
```
"#;
        let result = parse_tool_calls(content);
        assert!(result.is_err());
        match result.unwrap_err() {
            AgentError::InvalidToolCall(msg) => {
                assert!(msg.contains("Invalid JSON"));
            }
            other => panic!("expected InvalidToolCall, got {:?}", other),
        }
    }

    #[test]
    fn test_separate_text_and_calls() {
        let content = r#"
Hello! Let me search for that.

<function_calls>
<invoke name="search">
<parameter name="query">test</parameter>
</invoke>
</function_calls>
"#;
        let (text, calls) = separate_text_and_calls(content);
        assert!(!calls.is_empty());
        assert!(!text.contains("function_calls"));
        assert!(text.contains("Hello"));
    }

    #[test]
    fn test_no_tool_calls() {
        let content = "Just a plain message.";
        let calls = parse_tool_calls(content).unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn test_round_trip_format() {
        let calls = vec![ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "read_file".to_string(),
                arguments: r#"{"path":"/tmp/test.txt"}"#.to_string(),
            },
        }];
        let formatted = format_tool_calls(&calls);
        assert!(formatted.contains("<function_calls>"));
        assert!(formatted.contains("<invoke name=\"read_file\">"));
        assert!(formatted.contains("<parameter name=\"path\">"));
    }

    #[test]
    fn test_multiple_tool_calls_xml() {
        let content = r#"
<function_calls>
<invoke name="search">
<parameter name="query">rust</parameter>
</invoke>
<invoke name="read_file">
<parameter name="path">/tmp/a.txt</parameter>
</invoke>
</function_calls>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "search");
        assert_eq!(calls[1].function.name, "read_file");
    }

    #[test]
    fn test_get_parser_registry() {
        let parser = get_parser("hermes");
        assert!(parser.is_some());

        let parser = get_parser("nonexistent");
        assert!(parser.is_none());
    }

    #[test]
    fn test_parameter_json_value_parsing() {
        let content = r#"
<function_calls>
<invoke name="configure">
<parameter name="count">42</parameter>
<parameter name="enabled">true</parameter>
<parameter name="label">hello world</parameter>
</invoke>
</function_calls>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        // "42" is not valid JSON object value, so it should be a string
        assert_eq!(args["count"], 42);
        assert_eq!(args["enabled"], true);
        assert_eq!(args["label"], "hello world");
    }

    /// Regression for the user-reported leak:
    /// `minimax:MiniMax-M2` model emits tool calls wrapped in
    /// `<minimax:tool_call>…</minimax:tool_call>` instead of the
    /// standard Hermes `<function_calls>` block. The default parser
    /// must recognise this wrapper.
    #[test]
    fn test_parse_minimax_wrapper_format() {
        let content = r#"
<minimax:tool_call>
<invoke name="terminal">
<parameter name="command">ls -la</parameter>
</invoke>
</minimax:tool_call>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1, "expected one tool call from <minimax:tool_call>");
        assert_eq!(calls[0].function.name, "terminal");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["command"], "ls -la");
    }

    /// MiniMax-M2 sometimes packs multiple invokes inside the same
    /// wrapper (e.g. `cd … && pwd`). Make sure the parser returns
    /// both, in order.
    #[test]
    fn test_parse_minimax_wrapper_with_multiple_invoke() {
        let content = r#"
<minimax:tool_call>
<invoke name="terminal">
<parameter name="command">cd /tmp</parameter>
</invoke>
<invoke name="terminal">
<parameter name="command">pwd</parameter>
</invoke>
</minimax:tool_call>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "terminal");
        let a0: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        let a1: serde_json::Value = serde_json::from_str(&calls[1].function.arguments).unwrap();
        assert_eq!(a0["command"], "cd /tmp");
        assert_eq!(a1["command"], "pwd");
    }

    /// `separate_text_and_calls` must also strip the `<minimax:tool_call>`
    /// wrapper from the visible text portion, otherwise the wrapper
    /// itself would still leak to the user even after the inner
    /// invoke is recognised.
    #[test]
    fn test_separate_strips_minimax_wrapper() {
        let content = "好的,正在列出目录。\n<minimax:tool_call>\n<invoke name=\"terminal\">\n<parameter name=\"command\">ls</parameter>\n</invoke>\n</minimax:tool_call>\n结果如上。";
        let (text, calls) = separate_text_and_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "terminal");
        assert!(!text.contains("minimax:tool_call"), "wrapper leaked into visible text: {text}");
        assert!(!text.contains("<invoke"), "invoke leaked: {text}");
        assert!(text.contains("好的,正在列出目录。"));
        assert!(text.contains("结果如上。"));
    }

    //MiniMax M3 native format ----
    // Tool name as outer XML tag, parameters as child tags.
    // <terminal> <command>ls</command> </terminal>

    #[test]
    fn test_parse_m3_terminal_format() {
        let content = "<terminal> <command>ls -la</command> </terminal>";
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1, "expected one tool call from <terminal>");
        assert_eq!(calls[0].function.name, "terminal");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["command"], "ls -la");
    }

    #[test]
    fn test_parse_m3_terminal_with_piped_command() {
        // Real-world repro from user screenshot
        let content = "<terminal> <command>ls -la /workspace 2>&1 | head -100</command> </terminal>";
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "terminal");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["command"], "ls -la /workspace 2>&1 | head -100");
    }

    #[test]
    fn test_parse_m3_terminal_with_prose_prefix() {
        let content = "好的，正在查看目录。\n<terminal> <command>ls</command> </terminal>";
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "terminal");
    }

    #[test]
    fn test_parse_m3_terminal_no_whitespace() {
        // Model may output without spaces between tags
        let content = "<terminal><command>pwd</command></terminal>";
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "terminal");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["command"], "pwd");
    }

    #[test]
    fn test_parse_m3_read_file_format() {
        let content = "<read_file> <path>/tmp/test.txt</path> </read_file>";
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "/tmp/test.txt");
    }

    #[test]
    fn test_does_not_false_positive_on_prose_tags() {
        // Regular prose with angle brackets should not be parsed as tool calls
        let content = "The <b>bold</b> text is here.";
        let calls = parse_tool_calls(content).unwrap();
        assert!(calls.is_empty(), "prose <b> should not be parsed as tool call");
    }

    #[test]
    fn test_separate_strips_m3_terminal_format() {
        let content = "现在呢\n<terminal> <command>ls -la /workspace 2>&1 | head -100</command> </terminal>";
        let (text, calls) = separate_text_and_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "terminal");
        assert!(!text.contains("<terminal"), "terminal tag leaked: {text}");
        assert!(!text.contains("<command"), "command tag leaked: {text}");
        assert!(text.contains("现在呢"));
    }

    //Unclosed-block extraction -----------------------
    // When MiniMax-M3 hits finish_reason="stop" while still inside a
    // tool-call XML block, the structured extractors return nothing.
    // We need a best-effort fallback that records a skeletal ToolCall
    // so the agent loop can handle it through the normal pipeline
    // rather than silently skipping the turn.

    #[test]
    fn test_unclosed_invoke_is_captured() {
        // The model emitted `<invoke name="terminal">` but the
        // stream cut before the closing tag or any parameters.
        let content = "好的，我来查看目录。\n<invoke name=\"terminal\">";
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1, "unclosed <invoke> should be captured");
        assert_eq!(calls[0].function.name, "terminal");
        // Arguments are empty because there was no body — that's OK.
        // The agent loop will pass this to the tool with empty args.
    }

    #[test]
    fn test_unclosed_m3_tag_is_captured() {
        // MiniMax-M3 native format: `<terminal` at end of content.
        let content = "让我看看目录。\n<terminal";
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1, "unclosed M3 tag should be captured");
        assert_eq!(calls[0].function.name, "terminal");
    }

    #[test]
    fn test_unclosed_function_calls_tag_is_captured() {
        // LLM outputs `<function_calls>` but the stream cuts.
        // There should be no `</function_calls>` and the call
        // should still be detected.
        let content = "我准备用终端查看。\n<function_calls>\n<invoke name=\"list_files\">";
        let calls = parse_tool_calls(content).unwrap();
        assert!(calls.len() >= 1, "bare <invoke> inside unclosed <function_calls> should be captured");
        assert_eq!(calls[0].function.name, "list_files");
    }

    #[test]
    fn test_unclosed_tool_with_partial_args() {
        // The model started writing parameters but the stream cut.
        // We should still capture the tool name and partial args.
        let content = "正在处理...\n<invoke name=\"execute_code\"><parameter name=\"code\">x = ";
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "execute_code");
        // args may be empty or partial — either is acceptable.
    }

    #[test]
    fn test_unclosed_m3_tag_does_not_match_unknown_tag() {
        // Unknown tags like `<foo>` should NOT be captured as tool calls.
        let content = "some prose\n<foo>";
        let calls = parse_tool_calls(content).unwrap();
        assert!(calls.is_empty(), "unknown tag <foo> should not be captured");
    }

    #[test]
    fn test_unclosed_m3_terminal_is_known_tool() {
        // Known tool names at end of content should be captured.
        for tool_name in &["terminal", "read_file", "write_file", "browser", "computer_use", "execute_code"] {
            let content = format!("start\n<{}>", tool_name);
            let calls = parse_tool_calls(&content).unwrap();
            assert!(!calls.is_empty(), "known tool <{}> should be captured", tool_name);
            assert_eq!(calls[0].function.name, *tool_name);
        }
    }
}
