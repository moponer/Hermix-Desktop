// payload.rs — 解析"自身 exe 尾部"内嵌的自定义应用 (html + manifest)
//
// 打包布局 (由 inject_app 工具生成):
//   [ 原始 loader.exe 字节 ][ PAYLOAD ][ MAGIC(8) ][ PAYLOAD_LEN(u64 LE) ]
//   PAYLOAD = manifest_len(u64) + manifest_bytes + html_len(u64) + html_bytes

use serde::Deserialize;

pub const MAGIC: &[u8; 8] = b"HERMIXAP";

#[derive(Debug, Deserialize, Default)]
pub struct WindowConfig {
    #[serde(default = "default_width")]
    pub width: f64,
    #[serde(default = "default_height")]
    pub height: f64,
    #[serde(default = "default_true")]
    pub resizable: bool,
}

fn default_width() -> f64 {
    800.0
}
fn default_height() -> f64 {
    600.0
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub window_config: WindowConfig,
}

pub struct Payload {
    pub manifest: Manifest,
    pub html: String,
}

/// 读取自身 exe 并解析尾部 payload。不是自定义应用 exe 时返回 Err。
pub fn read_from_exe() -> Result<Payload, String> {
    let exe = std::env::current_exe().map_err(|e| format!("取 exe 路径失败: {e}"))?;
    let bytes = std::fs::read(&exe).map_err(|e| format!("读 exe 失败: {e}"))?;
    parse_trailer(&bytes)
}

pub fn parse_trailer(bytes: &[u8]) -> Result<Payload, String> {
    let n = bytes.len();
    if n < 16 + 8 {
        return Err("文件过短，不含应用 payload".into());
    }
    let magic_start = n - 16;
    if &bytes[magic_start..magic_start + 8] != MAGIC {
        return Err("未找到 HERMIXAP 魔数 (这不是自定义应用 exe)".into());
    }
    let payload_len =
        u64::from_le_bytes(bytes[n - 8..n].try_into().unwrap()) as usize;
    let payload_start = magic_start - payload_len;
    if payload_start > magic_start {
        return Err("payload 长度损坏".into());
    }
    let payload = &bytes[payload_start..magic_start];

    let mut off = 0;
    let mlen = u64::from_le_bytes(payload[off..off + 8].try_into().unwrap()) as usize;
    off += 8;
    let manifest_str = String::from_utf8_lossy(&payload[off..off + mlen]).to_string();
    off += mlen;
    let hlen = u64::from_le_bytes(payload[off..off + 8].try_into().unwrap()) as usize;
    off += 8;
    let html = String::from_utf8_lossy(&payload[off..off + hlen]).to_string();

    let manifest: Manifest =
        serde_json::from_str(&manifest_str).map_err(|e| format!("manifest 解析失败: {e}"))?;
    Ok(Payload { manifest, html })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用合成 buffer 验证 parse_trailer 与 inject_app 写出的字节布局一致。
    #[test]
    fn roundtrip_trailer() {
        let manifest = r#"{"id":"x","name":"Test","permissions":["fs-read"],"window_config":{"width":300,"height":200,"resizable":false}}"#;
        let html = "<html>hi</html>";
        let mut buf = b"FAKELOADERBINARY".to_vec();
        let mut payload = Vec::new();
        payload.extend_from_slice(&(manifest.len() as u64).to_le_bytes());
        payload.extend_from_slice(manifest.as_bytes());
        payload.extend_from_slice(&(html.len() as u64).to_le_bytes());
        payload.extend_from_slice(html.as_bytes());
        buf.extend_from_slice(&payload);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&(payload.len() as u64).to_le_bytes());

        let p = parse_trailer(&buf).expect("解析失败");
        assert_eq!(p.manifest.name, "Test");
        assert_eq!(p.manifest.permissions, vec!["fs-read".to_string()]);
        assert_eq!(p.manifest.window_config.width, 300.0);
        assert_eq!(p.manifest.window_config.resizable, false);
        assert_eq!(p.html, html);
    }

    #[test]
    fn rejects_plain_exe() {
        let buf = b"MZ\x90\x00not an app\x00\x00\x00".to_vec();
        assert!(parse_trailer(&buf).is_err());
    }
}
