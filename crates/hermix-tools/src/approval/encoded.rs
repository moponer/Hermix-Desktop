//! PowerShell `-EncodedCommand` 载荷解码。
//!
//! 2026-09-26 的裁决是「命令执行（设置环境变量或命令编码的）不用审批，只有删除等
//! 高危才审批」。审批引擎里只落实了前半句：`-EncodedCommand <base64>` 被当作一个
//! 普通参数放行，而 `DENIED_PATTERNS` / `CONFIRM_PATTERNS` 都只看命令行文本，于是
//! `Remove-Item -Recurse -Force C:\` 编码后连「删除要审批」这条都不再命中。
//!
//! 这个模块只服务**加严**方向：把载荷解出明文，让调用方重新跑危险模式匹配。
//! 「编码这个动作」本身仍然不弹卡，保持原裁决的 UX。

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

/// `-EncodedCommand` 的合法参数前缀。PowerShell 接受唯一前缀缩写，实际出现最多
/// 的是全称与 `-enc`；`-ec` / `-en` 一并收下是因为它们同样能唯一命中该参数。
/// 刻意**不收** `-e`：它太泛，会把 `-ErrorAction` 之类的碎片当成载荷。
const ENCODED_FLAGS: [&str; 4] = ["-encodedcommand", "-enc", "-ec", "-en"];

/// 从命令行里解出编码载荷的明文。不是 PowerShell / 没有编码参数 / 解不出来 → `None`。
///
/// 只处理 `-EncodedCommand <b64>`、`-EncodedCommand:<b64>`、`-enc=<b64>` 三种写法
/// （PowerShell 的参数分隔符 `:` 与 `=` 都出现过）。
pub fn decode_powershell_encoded(command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();
    if !(lower.contains("powershell") || lower.contains("pwsh")) {
        return None;
    }

    let tokens: Vec<&str> = command
        .split(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '(' | ')' | ';' | '&' | '|')
        })
        .filter(|t| !t.is_empty())
        .collect();

    for (i, raw) in tokens.iter().enumerate() {
        let (flag, inline) = match raw.split_once([':', '=']) {
            Some((f, v)) => (f.to_ascii_lowercase(), Some(v)),
            None => (raw.to_ascii_lowercase(), None),
        };
        if !ENCODED_FLAGS.contains(&flag.as_str()) {
            continue;
        }
        let payload = inline.or_else(|| tokens.get(i + 1).copied())?;
        return decode_payload(payload);
    }
    None
}

/// base64 → UTF-16LE。PowerShell 侧编码用 `[Text.Encoding]::Unicode`
/// （UTF-16LE）；个别工具直接塞 UTF-8，UTF-16 解出替换字符时回退试一次。
fn decode_payload(payload: &str) -> Option<String> {
    let bytes = STANDARD.decode(payload.trim()).ok()?;
    if bytes.len() < 2 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let decoded = String::from_utf16(&units).ok();
    match decoded {
        Some(text) if !text.contains('\u{FFFD}') && !text.trim().is_empty() => Some(text),
        _ => String::from_utf8(bytes)
            .ok()
            .filter(|text| !text.trim().is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_utf16le(script: &str) -> String {
        let bytes: Vec<u8> = script
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        STANDARD.encode(bytes)
    }

    #[test]
    fn decodes_full_flag_form() {
        let payload = encode_utf16le("Remove-Item -Recurse -Force C:\\");
        let cmd = format!("powershell -NoProfile -EncodedCommand {payload}");
        assert_eq!(
            decode_powershell_encoded(&cmd).as_deref(),
            Some("Remove-Item -Recurse -Force C:\\")
        );
    }

    #[test]
    fn decodes_abbreviated_and_inline_forms() {
        let payload = encode_utf16le("rm -rf /");
        assert!(decode_powershell_encoded(&format!("pwsh -enc {payload}")).is_some());
        assert!(decode_powershell_encoded(&format!("powershell -EncodedCommand:{payload}")).is_some());
        assert!(decode_powershell_encoded(&format!("powershell -enc={payload}")).is_some());
    }

    #[test]
    fn ignores_non_powershell_and_plain_commands() {
        assert!(decode_powershell_encoded("rm -rf /").is_none());
        assert!(decode_powershell_encoded("cargo build -enc foo").is_none());
        assert!(decode_powershell_encoded("powershell -Command \"Get-Date\"").is_none());
    }

    #[test]
    fn malformed_payload_is_none() {
        assert!(decode_powershell_encoded("powershell -enc not-base64!!").is_none());
        assert!(decode_powershell_encoded("powershell -enc").is_none());
    }
}
