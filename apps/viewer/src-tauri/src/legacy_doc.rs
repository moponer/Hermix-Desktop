//! 老版 .doc（Word 97-2003 二进制）的尽力文本抽取。
//!
//! 背景（2026-11-11）：markitdown sidecar 只认 OOXML，`.doc` 直接报
//! "unknown file type: doc"，老文档在查看器里完全打不开。本模块用 `cfb`
//! 读 OLE 复合容器，按 FIB 定位正文范围做**尽力**预览——目标是"能看"，
//! 不是无损转换；复杂排版（修订、域、嵌入对象）不保证还原。
//!
//! 解析顺序：
//!   1. FIB 头（WordDocument 流前 64 字节）拿 `fcMin/fcMac`；
//!   2. 若 `fComplex`（分段表）：读 1Table/0Table 的 CLX，按 piece table
//!      的 PCD 逐段取文本（fc 是 4K 压缩页码时按 cp1252，否则 UTF-16LE）；
//!   3. 简单情形直接按 `fExtChar` 位选 UTF-16LE / cp1252 解码；
//!   4. 任何一步失败 → 落回启发式全文扫描（可打印 UTF-16LE / cp1252 游程）。

use std::path::Path;

/// 老版 .doc → 尽力文本（markdown 直出，段落间空行）。
pub fn extract_doc_text(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("无法读取文件：{e}"))?;
    if bytes.len() < 512 || &bytes[0..8] != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return Err("不是 OLE 复合文档（老版 .doc 头部不符）".into());
    }

    let mut doc = cfb::open(path).map_err(|e| format!("OLE 容器打开失败：{e}"))?;
    let word = read_stream(&mut doc, "WordDocument")
        .ok_or_else(|| "OLE 容器里没有 WordDocument 流（可能不是 Word 文档）".to_string())?;
    if word.len() < 64 {
        return Err("WordDocument 流过短，无法解析".into());
    }

    let fc_min = u32_at(&word, 0x18) as usize;
    let fc_mac = u32_at(&word, 0x1C) as usize;
    let flags = u16_at(&word, 0x0A);
    let f_ext_char = flags & 0x1000 != 0; // fExtChar
    let f_complex = flags & 0x0004 != 0; // fComplex

    let text = if f_complex {
        // 分段表在 Table 流（fWhichTblStm 位选 1Table / 0Table）的 fcClx 处。
        let tbl_name = if flags & 0x0200 != 0 { "1Table" } else { "0Table" };
        let table = read_stream(&mut doc, tbl_name);
        let fc_clx = u32_at(&word, 0x01A2) as usize;
        let lcb_clx = u32_at(&word, 0x01A6) as usize;
        match table
            .as_deref()
            .filter(|t| fc_clx + lcb_clx <= t.len() && lcb_clx > 0)
            .and_then(|t| extract_piece_text(t, lcb_clx, &word))
        {
            Some(t) if !t.trim().is_empty() => t,
            _ => heuristic_extract(&bytes),
        }
    } else if fc_mac > fc_min && fc_mac <= word.len() {
        decode_word_range(&word, fc_min, fc_mac, f_ext_char)
    } else {
        heuristic_extract(&bytes)
    };

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err("没能从 .doc 里抽出正文（可能全是嵌入对象或图片）".into());
    }
    Ok(trimmed)
}

/// FIB 分段表（piece table）解析：CLX → pcdt(0x02) → CP 数组 + PCD 数组。
/// 正文本体在 `word`（WordDocument 流）里：fc 位 30 = 压缩页（cp1252，
/// fc/2 为流内偏移），否则 fc 直接是 UTF-16LE 偏移。
fn extract_piece_text(table: &[u8], lcb_clx: usize, word: &[u8]) -> Option<String> {
    // CLX 从 table 的 fcClx 开始（由调用方保证区间）。这里直接扫到 pcdt。
    let start = table.len() - lcb_clx.min(table.len());
    let clx = &table[start..];
    let mut i = 0usize;
    // 跳过 Prc 段（0x01 前缀 + 可变长），直到 pcdt(0x02)。
    while i < clx.len() {
        match clx[i] {
            0x01 => {
                if i + 3 > clx.len() {
                    return None;
                }
                let cb = u16_at(clx, i + 1) as usize;
                i += 3 + cb;
            }
            0x02 => {
                if i + 5 > clx.len() {
                    return None;
                }
                let lcb = u32_at(clx, i + 1) as usize;
                return parse_piece_table(&clx[i + 5..(i + 5 + lcb).min(clx.len())], word);
            }
            _ => return None,
        }
    }
    None
}

fn parse_piece_table(plcf: &[u8], word: &[u8]) -> Option<String> {
    if plcf.len() < 12 {
        return None;
    }
    // n+1 个 CP（4 字节）后跟 n 个 PCD（8 字节）：n = (len - 4) / 12。
    let n = (plcf.len().saturating_sub(4) / 12).max(1);
    let mut out = String::new();
    for p in 0..n {
        let cp_start = u32_at(plcf, p * 4) as usize;
        let cp_end = u32_at(plcf, (p + 1) * 4) as usize;
        let pcd = 4 * (n + 1) + p * 8;
        if pcd + 8 > plcf.len() {
            break;
        }
        // PCD: 2 字节标志 + 4 字节 fc + 2 字节 prm。fc 位 30 = 压缩（cp1252 页）。
        let fc_raw = u32_at(plcf, pcd + 2);
        let compressed = fc_raw & 0x4000_0000 != 0;
        let fc = (fc_raw & 0x3FFF_FFFF) as usize;
        let char_count = cp_end.saturating_sub(cp_start);
        if compressed {
            let start = fc / 2;
            let end = (start + char_count).min(word.len());
            if start < end {
                out.push_str(&decode_bytes_lossy_cp1252(&word[start..end]));
            }
        } else {
            let start = fc;
            let end = (start + char_count * 2).min(word.len());
            if start < end {
                let units: Vec<u16> = word[start..end]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                out.push_str(&String::from_utf16_lossy(&units));
            }
        }
    }
    (!out.trim().is_empty()).then_some(out)
}

/// 简单正文解码：fExtChar 决定 UTF-16LE / cp1252。
fn decode_word_range(buf: &[u8], from: usize, to: usize, ext_char: bool) -> String {
    let from = from.min(buf.len());
    let to = to.min(buf.len());
    let raw = &buf[from..to];
    if ext_char {
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        decode_bytes_lossy_cp1252(raw)
    }
}

/// cp1252 的高位区按 Windows-1252 表解码，控制符折叠为空白/换行。
fn decode_bytes_lossy_cp1252(raw: &[u8]) -> String {
    raw.iter()
        .map(|&b| match b {
            0x0D | 0x07 => '\n', // 段落标记 / 单元格结束
            0x0A | 0x09 | 0x0B | 0x0C => char::from(b),
            0x00..=0x1F => ' ',
            0x20..=0x7E => b as char,
            0x80 => '€',
            0x82 => '‚',
            0x83 => 'ƒ',
            0x84 => '„',
            0x85 => '…',
            0x86 => '†',
            0x87 => '‡',
            0x88 => 'ˆ',
            0x89 => '‰',
            0x8A => 'Š',
            0x8B => '‹',
            0x8C => 'Œ',
            0x91 => '‘',
            0x92 => '’',
            0x93 => '“',
            0x94 => '”',
            0x95 => '•',
            0x96 => '–',
            0x97 => '—',
            0x98 => '˜',
            0x99 => '™',
            0x9A => 'š',
            0x9B => '›',
            0x9C => 'œ',
            0x9E => 'ž',
            0x9F => 'Ÿ',
            _ => b as char, // Latin-1 区按原字节映射
        })
        .collect()
}

/// 启发式兜底：从整个文件里抽可打印 UTF-16LE / cp1252 游程。
fn heuristic_extract(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    let mut run = String::new();
    while i + 1 < bytes.len() {
        let unit = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        // CJK / ASCII / 常见标点才收，控制符断游程。
        let ch = char::from_u32(unit as u32).unwrap_or('\u{FFFD}');
        if unit == 0x0D || unit == 0x0A {
            if run.len() > 8 {
                out.push_str(&run);
                out.push('\n');
            }
            run.clear();
            i += 2;
            continue;
        }
        if (0x20..0xFFFD).contains(&unit) && ch != '\u{FFFD}' {
            run.push(ch);
        } else if run.len() > 8 {
            out.push_str(&run);
            out.push('\n');
            run.clear();
        } else {
            run.clear();
        }
        i += 2;
    }
    if run.len() > 8 {
        out.push_str(&run);
    }
    out
}

fn read_stream<F: std::io::Read + std::io::Seek>(
    doc: &mut cfb::CompoundFile<F>,
    name: &str,
) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut s = doc.open_stream(name).ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn u16_at(buf: &[u8], off: usize) -> u16 {
    buf.get(off..off + 2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .unwrap_or(0)
}

fn u32_at(buf: &[u8], off: usize) -> u32 {
    buf.get(off..off + 4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_ole_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.doc");
        std::fs::write(&p, b"not an ole file at all........").unwrap();
        assert!(extract_doc_text(&p).is_err());
    }
}
