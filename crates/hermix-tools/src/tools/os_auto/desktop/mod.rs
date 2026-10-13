//! Desktop automation tools — the UI Automation half of
//! [`crate::tools::os_auto`].
//!
//! Six tools, one worker thread, one opt-in toolset. The split is
//! deliberate: the two read-only tools that answer "what is on screen"
//! ([`win_list_windows`], `win_snapshot`) are `Safe`, while the three that
//! act on the user's real desktop (press, click, type) are `Caution`,
//! because they cannot be undone by re-reading the screen.

pub mod input;
pub mod uia;
pub mod worker;

use async_trait::async_trait;
use hermix_core::{tool_schema, traits::RiskLevel, JsonSchema, ToolError, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::tools::os_auto::truncate_chars;
use crate::ToolRegistry;
use uia::parse_hwnd;

/// Outermost cap on anything handed back to the LLM. Tool results are read
/// into the next turn's context, so an unbounded result is a context bomb
/// no matter how well the inner limits are tuned.
const RESULT_CAP: usize = 8_000;

/// Agent-supplied limits are clamped: a model asking for 50_000 nodes is a
/// mistake, and obeying it would cost more context than the whole run.
const MAX_DEPTH: u32 = 32;
const MAX_NODES: u32 = 5_000;
const MAX_CHARS: u32 = 20_000;

#[derive(Clone, Copy)]
enum Op {
    ListWindows,
    Snapshot,
    Invoke,
    SetValue,
    SendKeys,
    ReadText,
}

const KEY_GRAMMAR: &str = "Key sequence. Special keys in braces: {ENTER} {TAB} {ESC} {SPACE} {BACKSPACE} {DEL} {INS} {HOME} {END} {PGUP} {PGDN} {UP} {DOWN} {LEFT} {RIGHT} {F1}-{F12}. A modifier holds for the next key only: {CTRL}a is Ctrl+A, {ALT}{F4} is Alt+F4. Every other character is literal text. An unknown {TOKEN} is rejected rather than typed.";

const TOOLS: [(&str, Op, RiskLevel, &str); 6] = [
    (
        "win_list_windows",
        Op::ListWindows,
        RiskLevel::Safe,
        "List visible top-level Windows windows with hwnd, title, class, pid and rect.",
    ),
    (
        "win_snapshot",
        Op::Snapshot,
        RiskLevel::Safe,
        "Dump the UI Automation tree of a window as one line per element, each with a `ref=` handle for later use.",
    ),
    (
        "win_invoke",
        Op::Invoke,
        RiskLevel::Caution,
        "Click an element from a win_snapshot using its Invoke pattern (buttons, menu items, links).",
    ),
    (
        "win_set_value",
        Op::SetValue,
        RiskLevel::Caution,
        "Write text into an element from a win_snapshot using its Value pattern (text boxes, editable fields).",
    ),
    (
        "win_send_keys",
        Op::SendKeys,
        RiskLevel::Caution,
        "Send a key sequence to a window. See the `keys` parameter for the grammar.",
    ),
    (
        "win_read_text",
        Op::ReadText,
        RiskLevel::Safe,
        "Read the text content of a window (defaults to the foreground window). Use for editors, logs and consoles that expose no useful element tree.",
    ),
];

pub fn register(registry: &ToolRegistry) {
    for (name, op, risk, desc) in TOOLS {
        let handler = Arc::new(DesktopHandler { op });
        registry.register_with_risk(
            name,
            "desktop",
            handler.schema(),
            handler,
            // Always available: UI Automation ships with Windows. Whether the
            // operator wants it is the toolset toggle's job, not a check_fn.
            Arc::new(|| true),
            vec![],
            true,
            desc,
            "🪟",
            // `None` on purpose — the registry truncates on a byte boundary,
            // which can split a CJK character; `truncate_chars` below does it
            // right.
            None,
            risk,
            false,
        );
        // All six share one UIA apartment and drive the user's real desktop,
        // so two of them must never run concurrently against it.
        registry.mark_interactive(name);
    }
}

struct DesktopHandler {
    op: Op,
}

fn req_str(params: &Value, key: &str) -> Result<String, ToolError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidParams(format!("缺少必填参数 `{key}`")))
}

fn opt_str(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn opt_u32(params: &Value, key: &str, default: u32, cap: u32) -> u32 {
    params
        .get(key)
        .and_then(Value::as_u64)
        // Clamped both ways: a model asking for 50_000 nodes is a mistake,
        // and 0 would return nothing at all while looking like a real
        // answer.
        .map(|v| v.clamp(1, cap as u64) as u32)
        .unwrap_or(default)
}

fn opt_hwnd(
    params: &Value,
    key: &str,
) -> Result<Option<windows::Win32::Foundation::HWND>, ToolError> {
    opt_str(params, key).map(|s| parse_hwnd(&s)).transpose()
}

/// `HWND` wraps a raw pointer and is therefore not `Send`, but the job closure
/// has to cross to the worker thread. The handle travels as its integer value
/// and is rebuilt on the thread that owns the apartment.
fn hwnd_bits(hwnd: Option<windows::Win32::Foundation::HWND>) -> Option<usize> {
    hwnd.map(|h| h.0 as usize)
}

fn hwnd_from(bits: Option<usize>) -> Option<windows::Win32::Foundation::HWND> {
    bits.map(|v| windows::Win32::Foundation::HWND(v as *mut core::ffi::c_void))
}

#[async_trait]
impl ToolHandler for DesktopHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        use worker::DEFAULT_TIMEOUT;
        let out = match self.op {
            Op::ListWindows => {
                let filter = opt_str(&params, "filter");
                worker::worker()?
                    .call(DEFAULT_TIMEOUT, move |s| s.list_windows(filter.as_deref()))
                    .await?
            }
            Op::Snapshot => {
                let window = hwnd_bits(opt_hwnd(&params, "window")?);
                let max_depth = opt_u32(&params, "max_depth", 12, MAX_DEPTH);
                let max_nodes = opt_u32(&params, "max_nodes", 800, MAX_NODES);
                worker::worker()?
                    .call(DEFAULT_TIMEOUT, move |s| {
                        s.snapshot(hwnd_from(window), max_depth, max_nodes)
                    })
                    .await?
            }
            Op::Invoke => {
                let target = req_str(&params, "ref")?;
                worker::worker()?
                    .call(DEFAULT_TIMEOUT, move |s| s.invoke(&target))
                    .await?
            }
            Op::SetValue => {
                let (target, value) = (req_str(&params, "ref")?, req_str(&params, "value")?);
                worker::worker()?
                    .call(DEFAULT_TIMEOUT, move |s| s.set_value(&target, &value))
                    .await?
            }
            Op::SendKeys => {
                let target = req_str(&params, "keys")?;
                let window = hwnd_bits(opt_hwnd(&params, "window")?);
                let strokes = input::parse_keys(&target)?;
                worker::worker()?
                    .call(DEFAULT_TIMEOUT, move |_s| {
                        input::press_keys(&strokes, hwnd_from(window))?;
                        Ok(format!(
                            "已向 {} 发送 {} 个按键序列",
                            window.map_or("当前前台窗口".to_string(), |h| {
                                format!("hwnd=0x{h:x}")
                            }),
                            strokes.len()
                        ))
                    })
                    .await?
            }
            Op::ReadText => {
                let window = hwnd_bits(opt_hwnd(&params, "window")?);
                let max_chars = opt_u32(&params, "max_chars", 8_000, MAX_CHARS);
                worker::worker()?
                    .call(DEFAULT_TIMEOUT, move |s| s.read_text(hwnd_from(window), max_chars))
                    .await?
            }
        };
        Ok(truncate_chars(&out, RESULT_CAP))
    }

    fn schema(&self) -> ToolSchema {
        let window_prop = json!({
            "type": "string",
            "description": "Target window as the `hwnd=0x...` token from win_list_windows (decimal also accepted). Omit for the foreground window."
        });
        let (name, desc, props, required): (&str, &str, IndexMap<String, Value>, Vec<String>) =
            match self.op {
                Op::ListWindows => (
                    "win_list_windows",
                    TOOLS[0].3,
                    IndexMap::from([(
                        "filter".to_string(),
                        json!({"type": "string", "description": "Case-insensitive substring of the window title."}),
                    )]),
                    vec![],
                ),
                Op::Snapshot => (
                    "win_snapshot",
                    TOOLS[1].3,
                    IndexMap::from([
                        ("window".to_string(), window_prop.clone()),
                        ("max_depth".to_string(), json!({"type": "integer", "minimum": 1, "maximum": MAX_DEPTH, "default": 12})),
                        ("max_nodes".to_string(), json!({"type": "integer", "minimum": 1, "maximum": MAX_NODES, "default": 800})),
                    ]),
                    vec![],
                ),
                Op::Invoke => (
                    "win_invoke",
                    TOOLS[2].3,
                    IndexMap::from([(
                        "ref".to_string(),
                        json!({"type": "string", "description": "`ref=` handle from the most recent win_snapshot."}),
                    )]),
                    vec!["ref".into()],
                ),
                Op::SetValue => (
                    "win_set_value",
                    TOOLS[3].3,
                    IndexMap::from([
                        ("ref".to_string(), json!({"type": "string", "description": "`ref=` handle from the most recent win_snapshot."})),
                        ("value".to_string(), json!({"type": "string", "description": "Text to write into the field."})),
                    ]),
                    vec!["ref".into(), "value".into()],
                ),
                Op::SendKeys => (
                    "win_send_keys",
                    TOOLS[4].3,
                    IndexMap::from([
                        ("keys".to_string(), json!({"type": "string", "description": KEY_GRAMMAR})),
                        ("window".to_string(), window_prop.clone()),
                    ]),
                    vec!["keys".into()],
                ),
                Op::ReadText => (
                    "win_read_text",
                    TOOLS[5].3,
                    IndexMap::from([
                        ("window".to_string(), window_prop),
                        ("max_chars".to_string(), json!({"type": "integer", "minimum": 100, "maximum": MAX_CHARS, "default": 8000})),
                    ]),
                    vec![],
                ),
            };
        tool_schema(name, desc, JsonSchema::object(props, required))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(op: Op) -> DesktopHandler {
        DesktopHandler { op }
    }

    #[test]
    fn every_op_registers_under_the_same_toolset_name() {
        let names: Vec<&str> = TOOLS.iter().map(|(n, ..)| *n).collect();
        assert_eq!(
            names,
            vec![
                "win_list_windows",
                "win_snapshot",
                "win_invoke",
                "win_set_value",
                "win_send_keys",
                "win_read_text"
            ]
        );
    }

    #[test]
    fn acting_tools_are_caution_and_reading_tools_are_safe() {
        for (name, _, risk, _) in TOOLS {
            let expected = if name.starts_with("win_list")
                || name == "win_snapshot"
                || name == "win_read_text"
            {
                RiskLevel::Safe
            } else {
                RiskLevel::Caution
            };
            assert_eq!(risk, expected, "{name}");        }
    }

    #[test]
    fn schemas_name_the_tool_and_mark_its_required_params() {
        for (name, op, _, _) in TOOLS {
            let schema = handler(op).schema();
            assert_eq!(schema.name, name);
            let required = schema.parameters.required.as_ref();
            if matches!(op, Op::Invoke | Op::SetValue | Op::SendKeys) {
                assert!(required.is_some_and(|r| !r.is_empty()), "{name}");
            }
        }
    }

    #[test]
    fn agent_supplied_limits_are_clamped_both_ways() {
        let p = json!({"max_nodes": 999_999, "max_depth": 0});
        assert_eq!(opt_u32(&p, "max_nodes", 800, MAX_NODES), MAX_NODES);
        // 0 nodes would look like an empty window rather than a bad request.
        assert_eq!(opt_u32(&p, "max_depth", 12, MAX_DEPTH), 1);
        assert_eq!(opt_u32(&json!({}), "max_nodes", 800, MAX_NODES), 800);
    }

    #[test]
    fn missing_required_param_is_a_clear_error() {
        let err = req_str(&json!({}), "ref").unwrap_err().to_string();
        assert!(err.contains("ref"), "{err}");
        assert!(req_str(&json!({"ref": "e1"}), "ref").is_ok());
    }

    #[test]
    fn result_cap_never_splits_a_multibyte_char() {
        let s = "字".repeat(9_000);
        let capped = truncate_chars(&s, RESULT_CAP);
        assert_eq!(capped.chars().count(), RESULT_CAP);
    }
}
