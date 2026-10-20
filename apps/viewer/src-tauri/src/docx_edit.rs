//! Word 文档的段落级编辑（重量模式）。
//!
//! **为什么在 Rust 侧做**：docx 是 ZIP + OOXML，编辑要动的是「哪一段的哪段
//! 文字」这种字节级定位。前端做要引 JSZip + 自己写 DOM 手术；Rust 侧
//! `zip` + `quick-xml` **已在工作区的 Cargo.lock 里**（hermix-tools 的
//! docx-rs / rust_xlsxwriter 传递依赖），加进 viewer 不带来任何新增编译
//! 单元，还更快。
//!
//! **保真策略**：只改用户动过的段落，其余字节原样搬运。改写方式是
//! HermesOffice 那类「段落改写」——把该段落的文本 run 换成**一个**新 run，
//! 新 run 沿用原首个文本 run 的 `<w:rPr>`（粗体/斜体/字号/颜色都保住），
//! 含图片的 run 原样保留。代价：段内的**局部**混排格式（比如一句话中间
//! 加粗）会被抹平成整段的统一格式。跨段落的样式、页眉页脚、图片、编号、
//! 纸张设置全部不丢——这比「转成 HTML 再导回」那种整篇重建的路线保真得多。
//!
//! **原文件永不覆盖**：产物路径过 `safe_output_path`，默认落成
//! `<原名>.edited.docx`。

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::safe_output_path;
use crate::pdf_ops::{save_bytes, SaveResult};

/// 正文部件在 docx 包里的路径。
const DOCUMENT_PART: &str = "word/document.xml";

// ════════════════════════════════════════════════════════════════════
//  对外数据结构
// ════════════════════════════════════════════════════════════════════

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DocxParagraph {
    pub index: usize,
    pub text: String,
    /// "heading" | "para" | "list" | "quote"
    pub kind: String,
    /// 标题层级（1..6），非标题为 0。
    pub level: u8,
    /// 该段落是否位于表格单元格内。
    pub in_table: bool,
    /// 该段落是否含图片（编辑时图片 run 原样保留）。
    pub has_image: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DocxDocument {
    pub path: String,
    pub paragraphs: Vec<DocxParagraph>,
    /// 文档里的段落总数（可能大于 paragraphs.len()）。
    pub total: usize,
    /// 段落数超上限被截断。
    pub truncated: bool,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DocxEdit {
    pub index: usize,
    pub text: String,
}

// ════════════════════════════════════════════════════════════════════
//  命令
// ════════════════════════════════════════════════════════════════════

/// 读出文档的段落清单（带序号），供前端做段落级编辑。
#[tauri::command(async)]
pub fn docx_read(path: String) -> Result<DocxDocument, String> {
    let src = PathBuf::from(&path);
    let bytes = std::fs::read(&src).map_err(|e| format!("读取失败: {e}"))?;
    let xml = read_part(&bytes, DOCUMENT_PART)?;
    let spans = scan(&xml);
    let total = spans.len();
    let paragraphs = spans
        .iter()
        .enumerate()
        .map(|(i, s)| DocxParagraph {
            index: i,
            text: s.text.clone(),
            kind: s.kind.to_string(),
            level: s.level,
            in_table: s.in_table,
            has_image: s.has_image,
        })
        .collect();
    Ok(DocxDocument {
        path,
        paragraphs,
        total,
        truncated: false,
    })
}

/// 把段落级修改写回，产出**新**文件。
///
/// `expect_paragraphs` 是前端读文档时看到的段落数：写回时源文件若已被
/// 别的程序改过（段落数对不上），直接拒绝而不是把修改写到错位的段落上。
#[tauri::command(async)]
pub fn docx_write(
    src_path: String,
    out_path: Option<String>,
    edits: Vec<DocxEdit>,
    expect_paragraphs: Option<usize>,
) -> Result<SaveResult, String> {
    let src = PathBuf::from(&src_path);
    let bytes = std::fs::read(&src).map_err(|e| format!("读取失败: {e}"))?;
    let xml = read_part(&bytes, DOCUMENT_PART)?;
    let spans = scan(&xml);
    if let Some(expect) = expect_paragraphs {
        if expect != spans.len() {
            return Err("源文档已变化（段落数对不上），请重新打开后再编辑".into());
        }
    }
    let mut map: BTreeMap<usize, String> = BTreeMap::new();
    for e in edits {
        if e.index < spans.len() {
            map.insert(e.index, e.text);
        }
    }
    if map.is_empty() {
        return Err("没有要保存的修改".into());
    }
    let new_xml = apply_edits(&xml, &spans, &map);
    let out_bytes = write_part(&bytes, DOCUMENT_PART, &new_xml)?;
    let out = safe_output_path(&src, out_path.as_deref(), None)?;
    save_bytes(&out, &out_bytes, None)
}

// ════════════════════════════════════════════════════════════════════
//  XML 扫描
// ════════════════════════════════════════════════════════════════════

/// 扫描出来的一个段落（`<w:p>…</w:p>`）。
#[derive(Clone, Debug)]
struct ParaSpan {
    start: usize,
    open_end: usize,
    close_start: usize,
    end: usize,
    self_closing: bool,
    kind: &'static str,
    level: u8,
    in_table: bool,
    has_image: bool,
    text: String,
}

/// 一个 XML 标签的位置与身份。
struct Tag {
    start: usize,
    /// 标签结束（`>` 之后）的偏移。
    end: usize,
    prefix: String,
    name: String,
    closing: bool,
    self_closing: bool,
}

/// 下一个标签。注释 / 处理指令 / CDATA 整段跳过。
fn next_tag(s: &str, from: usize) -> Option<Tag> {
    let mut i = from;
    while i < s.len() {
        let rel = s[i..].find('<')?;
        let start = i + rel;
        let rest = &s[start..];
        if rest.starts_with("<!--") {
            let e = rest.find("-->")? + start + 3;
            i = e;
            continue;
        }
        if rest.starts_with("<?") {
            let e = rest.find("?>")? + start + 2;
            i = e;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let e = rest.find("]]>")? + start + 3;
            i = e;
            continue;
        }
        let gt = match rest.find('>') {
            Some(g) => start + g,
            None => return None,
        };
        let inner = &s[start + 1..gt];
        let closing = inner.starts_with('/');
        let body = if closing { &inner[1..] } else { inner };
        let trimmed = body.trim();
        let self_closing = trimmed.ends_with('/');
        // 标签名只到第一个空白为止：`<w:t xml:space="preserve">` 的名字是
        // `t`，不是 `t xml:space="preserve"`。自闭合的尾斜杠也要去掉。
        let name_src = if self_closing {
            trimmed.trim_end_matches('/').trim_end()
        } else {
            trimmed
        };
        let name_part = name_src.split_whitespace().next().unwrap_or("");
        let (prefix, name) = match name_part.split_once(':') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (String::new(), name_part.to_string()),
        };
        return Some(Tag {
            start,
            end: gt + 1,
            prefix,
            name,
            closing,
            self_closing,
        });
    }
    None
}

/// 取标签上的 `w:val="…"` 属性值。
fn attr_val(tag_text: &str) -> Option<String> {
    let pat = "w:val=\"";
    let idx = tag_text.find(pat)? + pat.len();
    let end = tag_text[idx..].find('"')? + idx;
    Some(tag_text[idx..end].to_string())
}

/// 扫描 document.xml，取出全部段落及其文本。
fn scan(xml: &str) -> Vec<ParaSpan> {
    let mut paras: Vec<ParaSpan> = Vec::new();
    let mut p_stack: Vec<ParaSpan> = Vec::new();
    let mut t_open: Option<usize> = None;
    let mut tbl_depth: usize = 0;
    let mut i = 0usize;

    while let Some(tag) = next_tag(xml, i) {
        i = tag.end;
        // 只认 wordprocessingml 命名空间：`a:p`（绘图里的文本框段落）
        // 之类别的命名空间不能混进来。
        if tag.prefix != "w" {
            continue;
        }
        match (tag.name.as_str(), tag.closing) {
            ("p", false) => {
                if tag.self_closing {
                    paras.push(ParaSpan {
                        start: tag.start,
                        open_end: tag.end,
                        close_start: tag.end,
                        end: tag.end,
                        self_closing: true,
                        kind: "para",
                        level: 0,
                        in_table: tbl_depth > 0,
                        has_image: false,
                        text: String::new(),
                    });
                } else {
                    p_stack.push(ParaSpan {
                        start: tag.start,
                        open_end: tag.end,
                        close_start: 0,
                        end: 0,
                        self_closing: false,
                        kind: "para",
                        level: 0,
                        in_table: tbl_depth > 0,
                        has_image: false,
                        text: String::new(),
                    });
                }
            }
            ("p", true) => {
                if let Some(mut p) = p_stack.pop() {
                    p.close_start = tag.start;
                    p.end = tag.end;
                    paras.push(p);
                }
            }
            ("t", false) => {
                if !tag.self_closing {
                    t_open = Some(tag.end);
                }
            }
            ("t", true) => {
                let from = t_open.take().unwrap_or(tag.start);
                let raw = &xml[from..tag.start];
                let text = quick_xml::escape::unescape(raw)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| raw.to_string());
                if let Some(p) = p_stack.last_mut() {
                    p.text.push_str(&text);
                }
            }
            ("tab", _) => {
                if let Some(p) = p_stack.last_mut() {
                    p.text.push('\t');
                }
            }
            ("br", _) | ("cr", _) => {
                if let Some(p) = p_stack.last_mut() {
                    p.text.push('\n');
                }
            }
            ("drawing", _) | ("pict", _) => {
                if let Some(p) = p_stack.last_mut() {
                    p.has_image = true;
                }
            }
            ("pStyle", _) => {
                let val = attr_val(&xml[tag.start..tag.end]);
                if let Some(p) = p_stack.last_mut() {
                    if let Some(v) = val {
                        if let Some(level) = heading_level(&v) {
                            p.kind = "heading";
                            p.level = level;
                        } else if v.starts_with("List") {
                            p.kind = "list";
                        } else if v == "Quote" {
                            p.kind = "quote";
                        }
                    }
                }
            }
            ("numPr", _) => {
                if let Some(p) = p_stack.last_mut() {
                    if p.kind == "para" {
                        p.kind = "list";
                    }
                }
            }
            ("tbl", false) => tbl_depth += 1,
            ("tbl", true) => tbl_depth = tbl_depth.saturating_sub(1),
            _ => {}
        }
    }
    paras
}

/// 样式名 → 标题层级。"Heading3" → 3；非标题 → None。
fn heading_level(style: &str) -> Option<u8> {
    let rest = style.strip_prefix("Heading")?;
    let n: u8 = rest.parse().ok()?;
    if (1..=6).contains(&n) {
        Some(n)
    } else {
        None
    }
}

// ════════════════════════════════════════════════════════════════════
//  段落改写
// ════════════════════════════════════════════════════════════════════

/// 把若干段落的文本替换掉，返回新的 document.xml。
///
/// 顺序推进（不是从后往前改偏移）：每段替换后游标直接跳到该段末尾，
/// 偏移不会失效，一次线性扫描即可。
fn apply_edits(
    xml: &str,
    spans: &[ParaSpan],
    edits: &BTreeMap<usize, String>,
) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut cursor = 0usize;
    for (i, span) in spans.iter().enumerate() {
        let Some(text) = edits.get(&i) else { continue };
        if span.start < cursor {
            continue;
        }
        out.push_str(&xml[cursor..span.start]);
        out.push_str(&rebuild_paragraph(xml, span, text));
        cursor = span.end;
    }
    out.push_str(&xml[cursor..]);
    out
}

/// 重建一个段落：新文本进新 run，其余原样。
fn rebuild_paragraph(xml: &str, p: &ParaSpan, new_text: &str) -> String {
    let open_tag = &xml[p.start..p.open_end];

    // 自闭合空段 `<w:p/>`：直接展开成一个带 run 的段落。
    if p.self_closing {
        let head = open_tag.trim_end();
        let head = head.strip_suffix("/>").unwrap_or(head);
        return format!("{head}><w:r>{}</w:r></w:p>", text_to_run_content(new_text));
    }

    let body = &xml[p.open_end..p.close_start];
    let (new_body, inserted) = rewrite_body_text(body, new_text);
    let new_body = if inserted {
        new_body
    } else {
        // 段落里原本没有文本元素（空段）：新 run 插在 `<w:pPr>` 之后
        // ——段落属性必须留在最前。
        let insert_at = ppr_end(&new_body);
        format!(
            "{}{}{}",
            &new_body[..insert_at],
            format!("<w:r>{}</w:r>", text_to_run_content(new_text)),
            &new_body[insert_at..]
        )
    };

    format!("{}{}{}", open_tag, new_body, &xml[p.close_start..p.end])
}

/// 段落正文里的文本元素改写：
///  - 第一个 `<w:t>` → 换成新内容（`<w:t>` / `<w:br/>` / `<w:tab/>` 序列），
///    新文本因此沿用该 run 的 `<w:rPr>`（粗体/字号/颜色都保住）
///  - 其余 `<w:t>` → **内容清空**（保留元素与 run 结构，run 里的图片不丢）
///
/// 返回 (新正文, 是否已写入新文本)。
fn rewrite_body_text(body: &str, new_text: &str) -> (String, bool) {
    let content = text_to_run_content(new_text);
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;
    let mut done = false;
    while let Some(tag) = next_tag(body, i) {
        let is_text = tag.prefix == "w" && tag.name == "t" && !tag.closing;
        if !is_text {
            out.push_str(&body[i..tag.end]);
            i = tag.end;
            continue;
        }
        out.push_str(&body[i..tag.start]);
        if tag.self_closing {
            if !done {
                out.push_str(&content);
                done = true;
            }
            i = tag.end;
            continue;
        }
        let Some(close) = next_tag(body, tag.end) else {
            out.push_str(&body[tag.start..]);
            return (out, done);
        };
        if !(close.closing && close.prefix == "w" && close.name == "t") {
            // 不是预期的闭合标签（异常文档）：原样保留，不做手术。
            out.push_str(&body[tag.start..close.start]);
            i = close.start;
            continue;
        }
        if !done {
            out.push_str(&content);
            done = true;
        } else {
            out.push_str(&body[tag.start..tag.end]);
            out.push_str("</w:t>");
        }
        i = close.end;
    }
    out.push_str(&body[i..]);
    (out, done)
}

/// `<w:pPr>…</w:pPr>` 的结束偏移（没有则 0）——新 run 必须排在它后面。
fn ppr_end(body: &str) -> usize {
    let head = body.trim_start();
    let lead = body.len() - head.len();
    if !head.starts_with("<w:pPr") {
        return 0;
    }
    let gt = match head.find('>') {
        Some(g) => g,
        None => return 0,
    };
    if head[..gt].trim_end().ends_with('/') {
        return lead + gt + 1;
    }
    match head.find("</w:pPr>") {
        Some(rel) => lead + rel + "</w:pPr>".len(),
        None => 0,
    }
}

/// 文本 → run 内元素序列。
///
/// `\n` 变 `<w:br/>`、`\t` 变 `<w:tab/>`：这两者是 run 的**同级**元素，
/// 不能塞进 `<w:t>` 里（塞进去 Word 不认）。
fn text_to_run_content(text: &str) -> String {
    let mut out = String::new();
    let mut first_line = true;
    for line in text.split('\n') {
        if !first_line {
            out.push_str("<w:br/>");
        }
        first_line = false;
        let mut first_seg = true;
        for seg in line.split('\t') {
            if !first_seg {
                out.push_str("<w:tab/>");
            }
            first_seg = false;
            if seg.is_empty() {
                continue;
            }
            out.push_str("<w:t xml:space=\"preserve\">");
            let esc = quick_xml::escape::escape(seg);
            out.push_str(&esc);
            out.push_str("</w:t>");
        }
    }
    out
}

// ════════════════════════════════════════════════════════════════════
//  ZIP 容器
// ════════════════════════════════════════════════════════════════════

fn read_part(bytes: &[u8], name: &str) -> Result<String, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("不是有效的 Word 文档（ZIP 打开失败）：{e}"))?;
    let mut file = archive
        .by_name(name)
        .map_err(|_| "文档里找不到 word/document.xml——这可能不是 .docx 文件".to_string())?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| format!("读取正文失败: {e}"))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 复制整个包，只替换指定部件的内容。条目顺序原样保留（`[Content_Types].xml`
/// 仍在首位，某些阅读器对此敏感）。
fn write_part(src: &[u8], name: &str, content: &str) -> Result<Vec<u8>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(src))
        .map_err(|e| format!("打开 Word 文档失败: {e}"))?;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("读取压缩包条目失败: {e}"))?;
        let entry = file.name().to_string();
        if file.is_dir() {
            writer
                .add_directory(entry, options)
                .map_err(|e| format!("写入失败: {e}"))?;
            continue;
        }
        writer
            .start_file(&entry, options)
            .map_err(|e| format!("写入失败: {e}"))?;
        if entry == name {
            writer
                .write_all(content.as_bytes())
                .map_err(|e| format!("写入失败: {e}"))?;
        } else {
            std::io::copy(&mut file, &mut writer).map_err(|e| format!("写入失败: {e}"))?;
        }
    }
    let cursor = writer.finish().map_err(|e| format!("打包失败: {e}"))?;
    Ok(cursor.into_inner())
}

// ════════════════════════════════════════════════════════════════════
//  单测
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">标题 </w:t></w:r><w:r><w:t>第二段</w:t></w:r></w:p>
<w:p><w:r><w:t>甲</w:t></w:r><w:r><w:t>乙</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>表内文字</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:drawing/></w:r><w:t>带图段落</w:t></w:r></w:p>
<w:p/>
</w:body></w:document>"#;

    #[test]
    fn scan_extracts_paragraphs_with_kinds() {
        let spans = scan(SAMPLE);
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0].text, "标题 第二段");
        assert_eq!(spans[0].kind, "heading");
        assert_eq!(spans[0].level, 1);
        assert_eq!(spans[1].text, "甲乙");
        assert_eq!(spans[2].text, "表内文字");
        assert!(spans[2].in_table);
        assert!(spans[3].has_image);
        assert_eq!(spans[4].text, "");
    }

    #[test]
    fn edit_replaces_text_and_keeps_rpr() {
        let spans = scan(SAMPLE);
        let mut edits = BTreeMap::new();
        edits.insert(0usize, "新标题".to_string());
        let out = apply_edits(SAMPLE, &spans, &edits);
        assert!(out.contains("<w:b/>"), "首段的加粗属性应保留");
        assert!(out.contains("新标题"));
        assert!(!out.contains("第二段"), "同段的其它文本 run 应被清掉");
        assert!(out.contains("表内文字"), "未改动的段落必须原样保留");
        assert!(out.contains("带图段落"));
    }

    #[test]
    fn edit_turns_newline_into_break() {
        let spans = scan(SAMPLE);
        let mut edits = BTreeMap::new();
        edits.insert(1usize, "第一行\n第二行".to_string());
        let out = apply_edits(SAMPLE, &spans, &edits);
        assert!(out.contains("<w:br/>"), "换行应落成 <w:br/>");
        assert!(out.contains("第一行"));
        assert!(out.contains("第二行"));
    }

    #[test]
    fn edit_on_empty_paragraph_inserts_run() {
        let spans = scan(SAMPLE);
        let mut edits = BTreeMap::new();
        edits.insert(4usize, "补上".to_string());
        let out = apply_edits(SAMPLE, &spans, &edits);
        assert!(out.contains("补上"));
        assert!(out.contains("<w:p><w:r><w:t xml:space=\"preserve\">补上</w:t></w:r></w:p>"));
    }

    #[test]
    fn escapes_xml_specials() {
        let spans = scan(SAMPLE);
        let mut edits = BTreeMap::new();
        edits.insert(1usize, "a<b>&c".to_string());
        let out = apply_edits(SAMPLE, &spans, &edits);
        assert!(out.contains("a&lt;b&gt;&amp;c"));
    }

    /// 最小可用的 docx 包：只要 `word/document.xml` 在，就能被本模块读写。
    fn build_mini_docx(document_xml: &str) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("[Content_Types].xml", opts).unwrap();
            w.write_all(
                br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            )
            .unwrap();
            w.start_file("_rels/.rels", opts).unwrap();
            w.write_all(
                br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            )
            .unwrap();
            w.start_file("word/document.xml", opts).unwrap();
            w.write_all(document_xml.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn round_trip_edits_a_paragraph_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("hermix-viewer-docx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.docx");
        std::fs::write(&src, build_mini_docx(SAMPLE)).unwrap();

        let doc = docx_read(src.to_string_lossy().to_string()).unwrap();
        assert_eq!(doc.total, 5);
        let idx = doc
            .paragraphs
            .iter()
            .position(|p| p.text.contains("第二段"))
            .expect("应找到含「第二段」的段落");

        let out = dir.join("a.edited.docx");
        let res = docx_write(
            src.to_string_lossy().to_string(),
            Some(out.to_string_lossy().to_string()),
            vec![DocxEdit {
                index: idx,
                text: "改过的标题".into(),
            }],
            Some(doc.total),
        )
        .unwrap();
        assert!(std::path::Path::new(&res.path).exists());
        assert_ne!(res.path, src.to_string_lossy().to_string(), "产物不能是原文件");

        let doc2 = docx_read(res.path.clone()).unwrap();
        assert_eq!(doc2.total, 5, "段落数不应变化");
        assert!(
            doc2.paragraphs[idx].text.contains("改过的标题"),
            "改动应落到目标段落，实际：{:?}",
            doc2.paragraphs[idx].text
        );
        assert!(
            doc2.paragraphs.iter().any(|p| p.text.contains("表内文字")),
            "未改动的段落必须保留"
        );
        assert!(doc2.paragraphs.iter().any(|p| p.has_image));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_rejects_source_path_and_stale_paragraph_count() {
        let dir = std::env::temp_dir().join(format!("hermix-viewer-docx2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("b.docx");
        std::fs::write(&src, build_mini_docx(SAMPLE)).unwrap();
        let src_str = src.to_string_lossy().to_string();

        // 源文件本身当产物 → 必须被守卫拒绝。
        let err = docx_write(
            src_str.clone(),
            Some(src_str.clone()),
            vec![DocxEdit {
                index: 0,
                text: "x".into(),
            }],
            Some(5),
        )
        .unwrap_err();
        assert!(err.contains("不能与原文件相同"));

        // 段落数对不上（源文件已被改过）→ 必须拒绝，不能写到错位的段落。
        let err = docx_write(
            src_str.clone(),
            None,
            vec![DocxEdit {
                index: 0,
                text: "x".into(),
            }],
            Some(999),
        )
        .unwrap_err();
        assert!(err.contains("已变化"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
