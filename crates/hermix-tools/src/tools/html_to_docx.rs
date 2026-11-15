//! `html_to_docx` — convert an HTML fragment/document to a .docx file.
//!
//! Fills the gap between `office_export` (structured JSON blocks → OOXML)
//! and the model's natural output (HTML). LLM-generated HTML is clean and
//! regular, so a hand-rolled block parser covers the real vocabulary —
//! h1-h6, p, ul/ol+li, table/tr/td, blockquote, hr, plus inline
//! strong/em/code/a and the common entities — with **zero new
//! dependencies** (the parser is ~200 lines; `docx-rs` is already in the
//! tree via `office_export`).
//!
//! Malformed input degrades gracefully: unknown tags are unwrapped, a
//! document with no recognized blocks produces a single paragraph with
//! the tag-stripped text so the conversion never silently loses content.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermix_core::{tool_schema, traits::RiskLevel, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::ToolRegistry;

pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(HtmlToDocxHandler);
    let schema = handler.schema();
    registry.register_with_risk(
        "html_to_docx",
        "file",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Convert HTML to a Word .docx file. Understands headings h1-h6, paragraphs, \
         ordered/unordered lists, tables, blockquotes, and inline bold/italic/links.",
        "📄",
        None,
        RiskLevel::Caution,
        false,
    );
}

// ---------------------------------------------------------------------------
// Document model
// ---------------------------------------------------------------------------

/// Inline run with formatting flags.
#[derive(Debug, Clone)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
}

#[derive(Debug, Clone)]
enum Block {
    /// Heading level 1-6.
    Heading(u8, Vec<Run>),
    Paragraph(Vec<Run>),
    Bullet(Vec<Run>),
    Numbered(Vec<Run>),
    /// Rows of cells, each cell a list of inline runs.
    Table(Vec<Vec<Vec<Run>>>),
}

// ---------------------------------------------------------------------------
// HTML parsing
// ---------------------------------------------------------------------------

/// Decode the entities LLMs actually emit. Anything else passes through.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let (entity, advance) = if tail.starts_with("&amp;") {
            ("&", 5)
        } else if tail.starts_with("&lt;") {
            ("<", 4)
        } else if tail.starts_with("&gt;") {
            (">", 4)
        } else if tail.starts_with("&quot;") {
            ("\"", 6)
        } else if tail.starts_with("&#39;") {
            ("'", 5)
        } else if tail.starts_with("&apos;") {
            ("'", 6)
        } else if tail.starts_with("&nbsp;") {
            ("\u{00a0}", 6)
        } else if tail.starts_with("&#") {
            // Numeric entity: &#123; (decimal) or &#x1F600; (hex).
            let end = tail.find(';').unwrap_or(0);
            if end > 2 {
                let body = &tail[2..end];
                // Pick the radix from the prefix, then parse ONCE. The old
                // `or_else` retried the un-trimmed digits as hex, so every
                // hexadecimal entity failed and leaked through as raw text.
                let (radix, digits) = match body.strip_prefix('x').or_else(|| body.strip_prefix('X'))
                {
                    Some(hex) => (16, hex),
                    None => (10, body),
                };
                let code = u32::from_str_radix(digits, radix)
                    .ok()
                    .and_then(char::from_u32);
                match code {
                    Some(c) => {
                        out.push(c);
                        rest = &tail[end + 1..];
                        continue;
                    }
                    None => ("", 0),
                }
            } else {
                ("", 0)
            }
        } else {
            ("", 0)
        };
        if advance == 0 {
            // Unknown entity — emit the '&' literally and move on.
            out.push('&');
            rest = &tail[1..];
        } else {
            out.push_str(entity);
            rest = &tail[advance..];
        }
    }
    out.push_str(rest);
    out
}

/// Is this tag a container whose content should be recursed into (the tag
/// itself contributes nothing)?
fn is_transparent(tag: &str) -> bool {
    matches!(
        tag,
        "html" | "body" | "head" | "div" | "span" | "section" | "article" | "main" | "header"
            | "footer" | "font" | "center" | "small" | "sub" | "sup" | "u" | "s" | "mark"
    )
}

fn is_skipped(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "head" | "title" | "meta" | "link")
}

struct BlockParser {
    blocks: Vec<Block>,
    /// Inline accumulator for the block currently being read.
    inline: Vec<Run>,
    current: String,
    bold: usize,
    italic: usize,
    /// Ordered-list item counter: `Some(n)` while inside <ol>.
    ol_depth: usize,
}

impl BlockParser {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            inline: Vec::new(),
            current: String::new(),
            bold: 0,
            italic: 0,
            ol_depth: 0,
        }
    }

    fn flush_run(&mut self) {
        let text = std::mem::take(&mut self.current);
        if text.is_empty() {
            return;
        }
        let text = decode_entities(&text);
        // Collapse whitespace runs (HTML collapses them anyway) unless the
        // text is only whitespace, in which case keep one space for word
        // separation between two runs.
        let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            if !self.inline.is_empty() {
                if let Some(last) = self.inline.last_mut() {
                    last.text.push(' ');
                }
            }
            return;
        }
        // Preserve the space that separated this run from its neighbours:
        // `<p>This is <strong>important</strong> text</p>` must not become
        // "This isimportanttext" (the most common LLM HTML shape).
        let mut run_text = String::with_capacity(collapsed.len() + 2);
        if text.starts_with(char::is_whitespace) && !self.inline.is_empty() {
            run_text.push(' ');
        }
        run_text.push_str(&collapsed);
        if text.ends_with(char::is_whitespace) {
            run_text.push(' ');
        }
        self.inline.push(Run {
            text: run_text,
            bold: self.bold > 0,
            italic: self.italic > 0,
        });
    }

    /// Close the pending block, if any inline content was collected.
    fn flush_block(&mut self, block: Block) {
        self.flush_run();
        if self.inline.is_empty() {
            return;
        }
        let runs = std::mem::take(&mut self.inline);
        self.blocks.push(match block {
            Block::Heading(l, _) => Block::Heading(l, runs),
            Block::Paragraph(_) => Block::Paragraph(runs),
            Block::Bullet(_) => Block::Bullet(runs),
            Block::Numbered(_) => Block::Numbered(runs),
            // Tables never route through flush_block (cells accumulate
            // into `row` directly); degrade to a paragraph if one does.
            Block::Table(_) => Block::Paragraph(runs),
        });
    }

    fn flush_plain(&mut self) {
        self.flush_block(Block::Paragraph(Vec::new()));
    }
}

/// Parse an HTML fragment into document blocks. Returns `None` when the
/// input contains no text at all.
fn parse_blocks(html: &str) -> Option<Vec<Block>> {
    let mut p = BlockParser::new();
    let bytes = html.as_bytes();
    let mut i = 0;
    // Structural nesting we track outside the generic scan.
    let mut list_stack: Vec<bool> = Vec::new(); // true = ordered
    // Cell accumulation for tables.
    let mut table: Option<Vec<Vec<Vec<Run>>>> = None;
    let mut row: Option<Vec<Vec<Run>>> = None;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            let Some(close) = html[i..].find('>') else { break };
            let raw_tag = &html[i + 1..i + close];
            i += close + 1;
            let (closing, tag) = match raw_tag.strip_prefix('/') {
                Some(t) => (true, t),
                None => (false, raw_tag),
            };
            let tag = tag.split_whitespace().next().unwrap_or(tag).to_ascii_lowercase();
            // Self-closing marker is irrelevant for our tags.
            if is_skipped(&tag) {
                if !closing && matches!(tag.as_str(), "script" | "style" | "title") {
                    // Skip the element's entire content.
                    let close_tag = format!("</{}>", tag);
                    if let Some(end) = html[i..].to_ascii_lowercase().find(&close_tag) {
                        i += end + close_tag.len();
                    }
                }
                continue;
            }
            match tag.as_str() {
                "br" if !closing => {
                    // Line break: close the current paragraph, keep going.
                    p.flush_plain();
                }
                "hr" if !closing => {
                    p.flush_plain();
                    p.blocks.push(Block::Paragraph(vec![Run {
                        text: "———".into(),
                        bold: false,
                        italic: false,
                    }]));
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" if !closing => {
                    p.flush_plain();
                    let level: u8 = tag[1..].parse().unwrap_or(6);
                    // Read until the matching close tag; inline formatting
                    // inside headings is common (e.g. <h2><strong>).
                    let close_tag = format!("</{}>", tag);
                    let end = html[i..]
                        .to_ascii_lowercase()
                        .find(&close_tag)
                        .map(|e| i + e)
                        .unwrap_or(bytes.len());
                    let inner = &html[i..end];
                    let mut hp = BlockParser::new();
                    parse_inline_into(&mut hp, inner);
                    hp.flush_run();
                    p.blocks.push(Block::Heading(
                        level.min(6),
                        if hp.inline.is_empty() {
                            vec![Run { text: "(空标题)".into(), bold: true, italic: false }]
                        } else {
                            hp.inline
                        },
                    ));
                    p.inline.clear();
                    p.current.clear();
                    i = (end + close_tag.len()).min(bytes.len());
                }
                "table" if !closing => {
                    p.flush_plain();
                    table = Some(Vec::new());
                }
                "table" if closing => {
                    if let Some(mut t) = table.take() {
                        if let Some(r) = row.take() {
                            t.push(r);
                        }
                        if !t.is_empty() {
                            p.blocks.push(Block::Table(t));
                        }
                    }
                }
                "tr" if !closing => {
                    row = Some(Vec::new());
                }
                "tr" if closing => {
                    if let (Some(t), Some(r)) = (table.as_mut(), row.take()) {
                        t.push(r);
                    }
                }
                "td" | "th" if !closing => {
                    p.flush_run();
                }
                "td" | "th" if closing => {
                    p.flush_run();
                    let cell = std::mem::take(&mut p.inline);
                    if let Some(r) = row.as_mut() {
                        r.push(cell);
                    }
                }
                "ul" if !closing => {
                    p.flush_plain();
                    list_stack.push(false);
                }
                "ol" if !closing => {
                    p.flush_plain();
                    list_stack.push(true);
                }
                "ul" | "ol" if closing => {
                    list_stack.pop();
                }
                "li" if !closing => {
                    p.flush_plain();
                }
                "li" if closing => {
                    let ordered = list_stack.last().copied().unwrap_or(false);
                    p.flush_block(if ordered {
                        Block::Numbered(Vec::new())
                    } else {
                        Block::Bullet(Vec::new())
                    });
                }
                "blockquote" if !closing => p.flush_plain(),
                "p" if closing => p.flush_plain(),
                "strong" | "b" if !closing => {
                    p.flush_run();
                    p.bold += 1;
                }
                "strong" | "b" if closing => {
                    p.flush_run();
                    p.bold = p.bold.saturating_sub(1);
                }
                "em" | "i" if !closing => {
                    p.flush_run();
                    p.italic += 1;
                }
                "em" | "i" if closing => {
                    p.flush_run();
                    p.italic = p.italic.saturating_sub(1);
                }
                "a" if !closing => {
                    // Links render as text plus the URL in parentheses —
                    // a paper document has no click target.
                    let href = extract_attr(raw_tag, "href");
                    if let Some(href) = href {
                        p.current.push_str(&format!(" [{href}] "));
                    }
                }
                _ if is_transparent(&tag) => {}
                _ if closing => {
                    // Unknown closing tag — flush nothing, just continue.
                }
                _ => {
                    // Unknown opening tag (pre, code, img, …): transparent
                    // for blocks; `pre`/`code` inline content flows into the
                    // current run as plain text.
                }
            }
        } else {
            // Text until the next tag.
            let end = html[i..].find('<').map(|e| i + e).unwrap_or(bytes.len());
            p.current.push_str(&html[i..end]);
            i = end;
        }
    }
    p.flush_plain();
    if p.blocks.is_empty() {
        None
    } else {
        Some(p.blocks)
    }
}

/// Parse inline-only content (heading interiors) into the parser's run list.
fn parse_inline_into(p: &mut BlockParser, html: &str) {
    // Reuse the main scanner by feeding it and collecting only inline runs.
    // Headings rarely contain block tags; anything block-ish is treated as
    // text by the inline accumulator here.
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let Some(close) = html[i..].find('>') else { break };
            let raw_tag = &html[i + 1..i + close];
            i += close + 1;
            let (closing, tag) = match raw_tag.strip_prefix('/') {
                Some(t) => (true, t),
                None => (false, raw_tag),
            };
            let tag = tag.split_whitespace().next().unwrap_or(tag).to_ascii_lowercase();
            match tag.as_str() {
                "strong" | "b" if !closing => {
                    p.flush_run();
                    p.bold += 1;
                }
                "strong" | "b" if closing => {
                    p.flush_run();
                    p.bold = p.bold.saturating_sub(1);
                }
                "em" | "i" if !closing => {
                    p.flush_run();
                    p.italic += 1;
                }
                "em" | "i" if closing => {
                    p.flush_run();
                    p.italic = p.italic.saturating_sub(1);
                }
                "br" => p.current.push(' '),
                _ => {}
            }
        } else {
            let end = html[i..].find('<').map(|e| i + e).unwrap_or(bytes.len());
            p.current.push_str(&html[i..end]);
            i = end;
        }
    }
}

fn extract_attr(raw_tag: &str, name: &str) -> Option<String> {
    let lower = raw_tag.to_ascii_lowercase();
    let pat = format!("{name}=");
    let pos = lower.find(&pat)? + pat.len();
    let rest = raw_tag[pos..].trim_start();
    if let Some(q) = rest.chars().next() {
        if q == '"' || q == '\'' {
            let end = rest[1..].find(q)? + 1;
            return Some(rest[1..end].to_string());
        }
        let end = rest.find_whitespace();
        return Some(rest[..end].to_string());
    }
    None
}

trait FindWhitespace {
    fn find_whitespace(&self) -> usize;
}

impl FindWhitespace for str {
    fn find_whitespace(&self) -> usize {
        self.find(|c: char| c.is_whitespace()).unwrap_or(self.len())
    }
}

// ---------------------------------------------------------------------------
// docx emission
// ---------------------------------------------------------------------------

fn emit_docx(blocks: &[Block], path: &Path) -> Result<u64, ToolError> {
    use docx_rs::{AbstractNumbering, Docx, Level, LevelJc, LevelText, NumberFormat, Numbering, NumberingId, IndentLevel, Paragraph, Start};

    let mut docx = Docx::new()
        .add_abstract_numbering(
            AbstractNumbering::new(1).add_level(
                Level::new(0, Start::new(1), NumberFormat::new("bullet"), LevelText::new("•"), LevelJc::new("left")),
            ),
        )
        .add_abstract_numbering(
            AbstractNumbering::new(2).add_level(
                Level::new(0, Start::new(1), NumberFormat::new("decimal"), LevelText::new("%1."), LevelJc::new("left")),
            ),
        )
        .add_numbering(Numbering::new(1, 1))
        .add_numbering(Numbering::new(2, 2));

    for block in blocks {
        let paragraph = match block {
            Block::Heading(level, runs) => {
                para_with_runs(Paragraph::new().style(&format!("Heading{level}")), runs)
            }
            Block::Paragraph(runs) => para_with_runs(Paragraph::new(), runs),
            Block::Bullet(runs) => para_with_runs(
                Paragraph::new().numbering(NumberingId::new(1), IndentLevel::new(0)),
                runs,
            ),
            Block::Numbered(runs) => para_with_runs(
                Paragraph::new().numbering(NumberingId::new(2), IndentLevel::new(0)),
                runs,
            ),
            Block::Table(rows) => {
                use docx_rs::{Table, TableCell, TableRow};
                let trs: Vec<TableRow> = rows
                    .iter()
                    .map(|cells| {
                        TableRow::new(
                            cells
                                .iter()
                                .map(|cell| {
                                    TableCell::new()
                                        .add_paragraph(para_with_runs(Paragraph::new(), cell))
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                docx = docx.add_table(Table::new(trs));
                continue;
            }
        };
        docx = docx.add_paragraph(paragraph);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ToolError::from_message(format!("html_to_docx: 创建输出目录 {}: {e}", parent.display()))
        })?;
    }
    let file = std::fs::File::create(path)
        .map_err(|e| ToolError::from_message(format!("html_to_docx: 创建 {}: {e}", path.display())))?;
    docx.pack(BufWriter::new(file)).map_err(|e| {
        ToolError::from_message(format!("html_to_docx: pack {}: {e}", path.display()))
    })?;
    Ok(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
}

fn runs_to_docx(runs: &[Run]) -> Vec<docx_rs::Run> {
    runs.iter()
        .map(|r| {
            let mut run = docx_rs::Run::new().add_text(r.text.clone());
            if r.bold {
                run = run.bold();
            }
            if r.italic {
                run = run.italic();
            }
            run
        })
        .collect()
}

fn para_with_runs(mut p: docx_rs::Paragraph, runs: &[Run]) -> docx_rs::Paragraph {
    for r in runs_to_docx(runs) {
        p = p.add_run(r);
    }
    p
}

// ---------------------------------------------------------------------------
// Tool handler
// ---------------------------------------------------------------------------

struct HtmlToDocxHandler;

fn default_output_path() -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    hermix_config::paths::hermix_home()
        .join("documents")
        .join(format!("html-to-docx-{stamp}.docx"))
}

#[async_trait]
impl ToolHandler for HtmlToDocxHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let html = params
            .get("html")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidParams("缺少必填参数 `html`".into()))?;
        if html.trim().is_empty() {
            return Err(ToolError::InvalidParams("`html` 为空".into()));
        }
        let path: PathBuf = params
            .get("path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(default_output_path);

        let blocks = parse_blocks(html).ok_or_else(|| {
            ToolError::InvalidParams("HTML 中没有可识别的文本内容".into())
        })?;
        let size = emit_docx(&blocks, &path)?;
        Ok(format!(
            "已生成 {}（{} 块内容，{size} 字节）。包含标题 {} 个、列表项 {} 个、表格 {} 个。",
            path.display(),
            blocks.len(),
            blocks.iter().filter(|b| matches!(b, Block::Heading(..))).count(),
            blocks.iter().filter(|b| matches!(b, Block::Bullet(..) | Block::Numbered(..))).count(),
            blocks.iter().filter(|b| matches!(b, Block::Table(..))).count(),
        ))
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "html_to_docx",
            "Convert HTML to a Word .docx file. Supports h1-h6, p, ul/ol+li, tables, \
             blockquote, hr, and inline bold/italic/links. Entities are decoded. \
             `path` is optional — omit it to write under the hermix home documents dir.",
            JsonSchema::object(
                IndexMap::from([
                    (
                        "html".to_string(),
                        json!({"type": "string", "description": "The HTML content to convert. May be a full document or a fragment."}),
                    ),
                    (
                        "path".to_string(),
                        json!({"type": "string", "description": "Output .docx path. Defaults to <hermix home>/documents/<timestamp>.docx."}),
                    ),
                ]),
                vec!["html".into()],
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_decode() {
        assert_eq!(decode_entities("a &amp; b &lt;x&gt; &#39;q&#39;"), "a & b <x> 'q'");
        assert_eq!(decode_entities("&#20013;&#25991;"), "中文");
        assert_eq!(decode_entities("plain&text"), "plain&text");
    }

    #[test]
    fn parses_headings_lists_and_tables() {
        let html = "<h1>标题</h1><p>普通 <strong>加粗</strong> 与 <em>斜体</em>。</p>\
                    <ol><li>第一</li><li>第二</li></ol>\
                    <table><tr><th>列A</th><th>列B</th></tr><tr><td>1</td><td>2</td></tr></table>";
        let blocks = parse_blocks(html).expect("blocks");
        assert!(matches!(blocks[0], Block::Heading(1, _)));
        assert!(matches!(blocks[1], Block::Paragraph(_)));
        assert!(matches!(blocks[2], Block::Numbered(_)));
        assert!(matches!(blocks[3], Block::Numbered(_)));
        assert!(matches!(blocks[4], Block::Table(_)));
        let runs = match &blocks[1] {
            Block::Paragraph(r) => r,
            _ => panic!(),
        };
        // "普通 <strong>加粗</strong> 与 <em>斜体</em>。" splits at every
        // formatting boundary: 普通 / 加粗(bold) / 与 / 斜体(italic) / 。
        assert_eq!(runs.len(), 5);
        assert!(!runs[0].bold);
        assert!(runs[1].bold);
        assert!(runs[3].italic);
    }

    #[test]
    fn script_content_is_skipped() {
        let blocks = parse_blocks("<p>可见</p><script>alert('&lt;hidden&gt;')</script>").expect("blocks");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn unknown_tags_degrade_to_text() {
        let blocks = parse_blocks("<div><span>纯文本</span></div>").expect("blocks");
        assert!(matches!(&blocks[0], Block::Paragraph(r) if r[0].text.contains("纯文本")));
    }

    #[test]
    fn whitespace_only_html_yields_none() {
        assert!(parse_blocks("<div>   </div>").is_none());
    }
}
