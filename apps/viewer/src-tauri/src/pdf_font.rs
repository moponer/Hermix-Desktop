//! 页面内改字时"用哪个字体写"的决策。
//!
//! ## 为什么优先复用文档自己的字体
//!
//! 中文 PDF 的字体**本来就在页面的 `/Resources` 里**（WPS / Word 生成的
//! Identity-H 子集字体 + ToUnicode）。直接拿它写新字：
//!
//! - 产物**不涨体积**（不嵌任何新字体）；
//! - 新字与原文**完全同款**（比"嵌一个黑体"保真得多）；
//! - **零新依赖**（不需要字体子集化那一套）。
//!
//! 代价：文档的字体是**子集**，不含的字写不出来。这种情况如实报错 ——
//! 静默写出一串乱码比改不了更糟。
//!
//! ## 关键技术点
//!
//! lopdf 的 `Encoding::string_to_bytes` 对 `UnicodeMapEncoding`
//! （Identity-H 那种）是 `unimplemented!()` —— **调用会 panic**。所以
//! Identity-H 的编码必须自己来：从字体的 `/ToUnicode` CMap 反查
//! `unicode → 码位`。
//!
//! CMap 的两种映射段：
//!
//! ```text
//! N beginbfchar
//! <0003> <0020>      ← 码位 → unicode（单字符）
//! endbfchar
//! N beginbfrange
//! <0025> <0027> <0061>            ← 连续区间，目标递增
//! <0028> <0029> [<0062> <0063>]   ← 连续区间，目标逐个列出
//! endbfrange
//! ```

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Encoding, ObjectId};

/// 写进内容流的一段文字（字体 + 字节）。
pub struct FontChoice {
    /// 内容流里用的字体资源名（如 `F1`）。
    pub name: String,
    /// 已编码的字节（CID 字体是 2 字节码位，普通字体是单字节）。
    pub bytes: Vec<u8>,
    /// CID 字体（Type0）要写成十六进制串 `<…>`，普通字体写字面串 `(…)`。
    pub is_cid: bool,
}

/* ── ToUnicode CMap 解析 ─────────────────────────────────────────── */

/// 取出字体的 `/ToUnicode` 流内容。
fn tounicode_bytes(doc: &Document, font: &Dictionary) -> Option<Vec<u8>> {
    let obj = font.get_deref(b"ToUnicode", doc).ok()?;
    let stream = obj.as_stream().ok()?;
    stream.decompressed_content().ok()
}

/// 把一段 CMap 文本里的 `beginbfchar … endbfchar` 段解析成 `unicode → 码位`。
fn parse_bfchar(segment: &str, map: &mut HashMap<char, u16>) {
    let tokens = hex_tokens(segment);
    for pair in tokens.chunks(2) {
        if pair.len() < 2 {
            continue;
        }
        let (Some(code), Some(unis)) = (hex_u16(&pair[0]), hex_to_string(&pair[1])) else {
            continue;
        };
        if let Some(ch) = unis.chars().next() {
            map.insert(ch, code);
        }
    }
}

/// 把一段 CMap 文本里的 `beginbfrange … endbfrange` 段解析成 `unicode → 码位`。
///
/// 形式一：`<lo> <hi> <dst>` —— 目标从 dst 起连续递增。
/// 形式二：`<lo> <hi> [<d1> <d2> …]` —— 目标逐个列出。
fn parse_bfrange(segment: &str, map: &mut HashMap<char, u16>) {
    // 逐 token 走：遇到 `[` 说明是"目标列表"形式。
    let mut it = segment.char_indices().peekable();
    let bytes = segment.as_bytes();
    let _ = &mut it;
    // 简化：先把整段切成"尖括号组"与"方括号组"的序列。
    let mut groups: Vec<(bool, Vec<String>)> = Vec::new(); // (是否方括号, 内容)
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => {
                if let Some(end) = segment[i..].find('>') {
                    let inner = &segment[i + 1..i + end];
                    groups.push((false, vec![inner.trim().to_string()]));
                    i += end + 1;
                    continue;
                }
                break;
            }
            b'[' => {
                let mut items = Vec::new();
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b']' {
                    if bytes[j] == b'<' {
                        if let Some(end) = segment[j..].find('>') {
                            items.push(segment[j + 1..j + end].trim().to_string());
                            j += end + 1;
                            continue;
                        }
                    }
                    j += 1;
                }
                groups.push((true, items));
                i = j + 1;
                continue;
            }
            _ => {
                i += 1;
            }
        }
    }

    let mut k = 0usize;
    while k + 2 < groups.len() + 1 {
        if k + 2 >= groups.len() {
            break;
        }
        let (is_list, lo_items) = &groups[k];
        let (_, hi_items) = &groups[k + 1];
        let (dst_is_list, dst_items) = &groups[k + 2];
        let (Some(lo), Some(hi)) = (
            lo_items.first().and_then(|s| hex_u16(s)),
            hi_items.first().and_then(|s| hex_u16(s)),
        ) else {
            k += 3;
            continue;
        };
        if *is_list || *dst_is_list {
            // 形式二：目标逐个列出。
            for (i, dst) in dst_items.iter().enumerate() {
                let code = lo + i as u16;
                if let (Some(unis), Some(dst)) = (hex_to_string(dst), Some(code)) {
                    if let Some(ch) = unis.chars().next() {
                        map.insert(ch, dst);
                    }
                }
            }
        } else {
            // 形式一：目标连续递增。
            if let Some(dst0) = dst_items.first().and_then(|s| hex_u16(s)) {
                for i in 0..=(hi.saturating_sub(lo)) {
                    let code = lo + i;
                    if let Some(unis) = hex_to_string(&format!("{:04X}", dst0 + i)) {
                        if let Some(ch) = unis.chars().next() {
                            map.insert(ch, code);
                        }
                    }
                }
            }
        }
        k += 3;
    }
}

/// 取出 CMap 文本里两个标记之间的内容。
fn between(text: &str, start: &str, end: &str) -> Option<String> {
    let s = text.find(start)? + start.len();
    let e = text[s..].find(end)? + s;
    Some(text[s..e].to_string())
}

/// 取出一段文本里所有 `<...>` 尖括号内容。
fn hex_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(end) = text[i..].find('>') {
                out.push(text[i + 1..i + end].trim().to_string());
                i += end + 1;
                continue;
            }
            break;
        }
        i += 1;
    }
    out
}

/// 十六进制 → u16。
fn hex_u16(s: &str) -> Option<u16> {
    let s = s.trim();
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    u16::from_str_radix(s, 16).ok()
}

/// 十六进制（UTF-16BE）→ 字符串。
fn hex_to_string(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() % 4 != 0 {
        return None;
    }
    let mut units = Vec::new();
    for chunk in s.as_bytes().chunks(4) {
        let hex = std::str::from_utf8(chunk).ok()?;
        units.push(u16::from_str_radix(hex, 16).ok()?);
    }
    String::from_utf16(&units).ok()
}

/// 从字体的 `/ToUnicode` CMap 建 `unicode → 码位` 的反查表。
pub fn unicode_to_code(doc: &Document, font: &Dictionary) -> HashMap<char, u16> {
    let Some(data) = tounicode_bytes(doc, font) else {
        return HashMap::new();
    };
    parse_cmap(&String::from_utf8_lossy(&data))
}

/// 解析一段 ToUnicode CMap 文本 → `unicode → 码位`。
pub fn parse_cmap(text: &str) -> HashMap<char, u16> {
    let mut map = HashMap::new();
    if let Some(seg) = between(text, "beginbfchar", "endbfchar") {
        parse_bfchar(&seg, &mut map);
    }
    if let Some(seg) = between(text, "beginbfrange", "endbfrange") {
        parse_bfrange(&seg, &mut map);
    }
    map
}

/* ── 选字体 ──────────────────────────────────────────────────────── */

/// 在页面的字体里找一个**能无损写出**这段文字的。
///
/// 找不到返回 `None` —— 调用方据此决定退回标准 14 号字（拉丁文）或报错
/// （中文：字体是子集，不含那个字就是写不出来）。
pub fn pick_font_for_text(doc: &Document, page_id: ObjectId, text: &str) -> Option<FontChoice> {
    if text.is_empty() {
        return None;
    }
    let fonts = doc.get_page_fonts(page_id).ok()?;
    for (name, font) in fonts {
        let Ok(enc) = font.get_font_encoding(doc) else {
            continue;
        };
        let is_identity_h = font
            .get(b"Encoding")
            .ok()
            .and_then(|o| o.as_name_str().ok())
            .map(|s| s == "Identity-H")
            .unwrap_or(false);
        let bytes: Vec<u8> = match &enc {
            // Identity-H 那种：自己从 ToUnicode 反查（lopdf 的 string_to_bytes
            // 在这一支是 unimplemented!()，调用会 panic）。
            Encoding::UnicodeMapEncoding(_) => {
                if !is_identity_h {
                    continue;
                }
                let map = unicode_to_code(doc, font);
                let mut out = Vec::new();
                let mut ok = true;
                for ch in text.chars() {
                    match map.get(&ch) {
                        Some(&code) => out.extend_from_slice(&code.to_be_bytes()),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    continue;
                }
                out
            }
            _ => enc.string_to_bytes(text),
        };
        if bytes.is_empty() {
            continue;
        }
        // 回读校验：编码必须无损（否则写出去的是乱码）。
        match Document::decode_text(&enc, &bytes) {
            Ok(decoded) if decoded == text => {}
            _ => continue,
        }
        // CID 字体只有 **Identity-H** 才能这么写：那时码位 = CID = GID。
        // 别的 CID 编码（GBK-EUC-H 之类）是多字节字符编码，码位 ≠ CID，
        // 按 ToUnicode 的码位写回去会写出错字 —— 这种字体直接跳过。
        let is_cid = font
            .get(b"Encoding")
            .ok()
            .and_then(|o| o.as_name_str().ok())
            .map(|s| s == "Identity-H")
            .unwrap_or(false);
        return Some(FontChoice {
            name: String::from_utf8_lossy(&name).to_string(),
            bytes,
            is_cid,
        });
    }
    None
}

/// 字节 → 内容流里的字符串写法（CID 走十六进制串，普通字体走字面串）。
pub fn text_operator(choice: &FontChoice) -> Vec<u8> {
    let mut out = Vec::new();
    if choice.is_cid {
        out.push(b'<');
        for b in &choice.bytes {
            out.extend_from_slice(format!("{b:02X}").as_bytes());
        }
        out.extend_from_slice(b"> Tj\n");
    } else {
        out.push(b'(');
        for b in &choice.bytes {
            match b {
                b'(' => out.extend_from_slice(b"\\("),
                b')' => out.extend_from_slice(b"\\)"),
                b'\\' => out.extend_from_slice(b"\\\\"),
                _ => out.push(*b),
            }
        }
        out.extend_from_slice(b") Tj\n");
    }
    out
}

/// 字体资源名 → 内容流里的引用（`/F1`）。
pub fn font_ref(name: &str) -> String {
    format!("/{}", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMAP: &str = "\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
4 beginbfchar
<0001> <4F60>
<0002> <597D>
<0003> <4E16>
<0004> <754C>
endbfchar
1 beginbfrange
<0010> <0012> <0041>
endbfrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end";

    #[test]
    fn bfchar_gives_unicode_to_code() {
        let map = parse_cmap(CMAP);
        assert_eq!(map.get(&'你'), Some(&1));
        assert_eq!(map.get(&'好'), Some(&2));
        assert_eq!(map.get(&'世'), Some(&3));
        assert_eq!(map.get(&'界'), Some(&4));
    }

    #[test]
    fn bfrange_expands_to_increasing_codes() {
        let map = parse_cmap(CMAP);
        // <0010> <0012> <0041> → A/B/C 分别对应码位 0x10 / 0x11 / 0x12
        assert_eq!(map.get(&'A'), Some(&0x10));
        assert_eq!(map.get(&'B'), Some(&0x11));
        assert_eq!(map.get(&'C'), Some(&0x12));
    }

    #[test]
    fn cid_text_is_written_as_hex_string() {
        let choice = FontChoice {
            name: "F1".into(),
            bytes: vec![0x00, 0x01, 0x00, 0x02],
            is_cid: true,
        };
        assert_eq!(text_operator(&choice), b"<00010002> Tj\n".to_vec());
        assert_eq!(font_ref("F1"), "/F1");
    }

    #[test]
    fn simple_font_text_escapes_parentheses() {
        let choice = FontChoice {
            name: "F2".into(),
            bytes: b"a(b)c".to_vec(),
            is_cid: false,
        };
        assert_eq!(text_operator(&choice), b"(a\\(b\\)c) Tj\n".to_vec());
    }
}