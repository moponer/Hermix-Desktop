//! Office export tool — write structured JSON data to a real binary
//! Office file. Currently `.xlsx` (batch 1 of the binary-office-export
//! pipeline; see `docs/notes/proposed/feature/
//! 2026-10-17-binary-office-export-pipeline.md` — DOCX/PPTX follow in
//! later batches and are rejected with an explicit error for now).
//!
//! Why this exists: the canvas pipeline (document-authoring) delivers
//! HTML/PDF; `file_converter` converts *out of* Office formats. Neither
//! produces an *editable* .xlsx/.docx. This tool closes the write side
//! with a pure-Rust OOXML writer — zero Python, zero LibreOffice.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{
    tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel,
};

use crate::tools::file::to_absolute_path;
use crate::ToolRegistry;

/// Sanity caps — the LLM already holds the data in context, so anything
/// past these bounds is a bug (e.g. a runaway loop building the JSON),
/// not legitimate output. Errors out instead of hanging the write.
const MAX_TOTAL_ROWS: usize = 100_000;
/// Excel hard column limit.
const MAX_COLS: usize = 16_384;
/// Excel hard sheet-name limit.
const MAX_SHEET_NAME_CHARS: usize = 31;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `office_export` tool. Toolset/risk mirror `write_file`
/// (file toolset, Caution — writes to disk), keeping the file-toolset
/// risk grading uniform.
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(OfficeExportHandler);
    let schema = handler.schema();
    registry.register_with_risk(
        "office_export",
        "file",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Write structured data to REAL Office files (editable in Excel/WPS/Word/PowerPoint): \
         format=\"xlsx\" spreadsheets, format=\"docx\" documents (heading1-3/paragraph/bullet \
         blocks), format=\"pptx\" presentations. \
         Use for: delivering processed/merged/cleaned data or documents as editable files. \
         NOT for: reading files (use markitdown), HTML→Word conversion (use html_to_docx), \
         PDF export (use file_converter).",
        "📊",
        None,
        RiskLevel::Caution,
        false,
    );
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Tool for exporting JSON table data to `.xlsx`.
pub struct OfficeExportHandler;

fn cell_write_error(desc: &str) -> ToolError {
    ToolError::ExecutionFailed(format!(
        "office_export: {desc}. Cells must be string / number / bool / null."
    ))
}

/// Write one JSON cell value at (row, col). `null` leaves the cell blank.
fn write_cell(
    ws: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    v: &Value,
) -> Result<(), ToolError> {
    match v {
        Value::Null => Ok(()),
        Value::Bool(b) => ws
            .write_boolean(row, col, *b)
            .map(|_| ())
            .map_err(|e| cell_write_error(&format!("write_boolean at ({row},{col}): {e}"))),
        Value::Number(n) => {
            let f = n.as_f64().ok_or_else(|| {
                cell_write_error(&format!("unrepresentable number at ({row},{col})"))
            })?;
            ws.write_number(row, col, f)
                .map(|_| ())
                .map_err(|e| cell_write_error(&format!("write_number at ({row},{col}): {e}")))
        }
        Value::String(s) => ws
            .write_string(row, col, s)
            .map(|_| ())
            .map_err(|e| cell_write_error(&format!("write_string at ({row},{col}): {e}"))),
        other => Err(cell_write_error(&format!(
            "unsupported cell type at ({row},{col}): {}",
            other
        ))),
    }
}

/// Sanity caps for document/presentation exports.
const MAX_BLOCKS: usize = 5_000;
const MAX_SLIDES: usize = 200;
const MAX_TEXT_CHARS: usize = 1_000_000;

#[async_trait]
impl ToolHandler for OfficeExportHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path_str = params
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ToolError::from_message("office_export: missing `path`"))?;
        let format = params
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("xlsx");
        let abs = to_absolute_path(path_str);
        match format.to_ascii_lowercase().as_str() {
            "xlsx" => self.export_xlsx(&params, &abs),
            "docx" => self.export_docx(&params, &abs),
            "pptx" => self.export_pptx(&params, &abs),
            other => Err(ToolError::from_message(format!(
                "office_export: format \"{other}\" is not supported — supported: xlsx, docx, pptx."
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        self.office_schema()
    }
}

impl OfficeExportHandler {
    /// XLSX export: structured table data → real workbook.
    fn export_xlsx(&self, params: &Value, abs: &str) -> Result<String, ToolError> {
        let sheets = params
            .get("sheets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::from_message("office_export: missing `sheets` array"))?;
        if sheets.is_empty() {
            return Err(ToolError::from_message(
                "office_export: `sheets` must contain at least one sheet",
            ));
        }

        // --- Validate sheets up front (fail before touching disk) ---
        let mut seen_names = std::collections::HashSet::new();
        let mut total_rows = 0usize;
        for (i, s) in sheets.iter().enumerate() {
            let name = s
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|n| !n.trim().is_empty())
                .ok_or_else(|| {
                    ToolError::from_message(format!("office_export: sheets[{i}] missing `name`"))
                })?;
            if name.chars().count() > MAX_SHEET_NAME_CHARS {
                return Err(ToolError::from_message(format!(
                    "office_export: sheets[{i}] name exceeds {MAX_SHEET_NAME_CHARS} chars (Excel limit): \"{name}\""
                )));
            }
            if !seen_names.insert(name.to_string()) {
                return Err(ToolError::from_message(format!(
                    "office_export: duplicate sheet name \"{name}\""
                )));
            }
            let rows = s
                .get("rows")
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    ToolError::from_message(format!("office_export: sheets[{i}] missing `rows` array"))
                })?;
            total_rows += rows.len();
            if rows.len() > MAX_COLS {
                return Err(ToolError::from_message(format!(
                    "office_export: sheets[{i}] has {} rows — did you swap rows/columns? (max per sheet: {MAX_COLS})",
                    rows.len()
                )));
            }
            for (r, row) in rows.iter().enumerate() {
                let cells = row.as_array().ok_or_else(|| {
                    ToolError::from_message(format!(
                        "office_export: sheets[{i}].rows[{r}] is not an array"
                    ))
                })?;
                if cells.len() > MAX_COLS {
                    return Err(ToolError::from_message(format!(
                        "office_export: sheets[{i}].rows[{r}] has {} cells — exceeds Excel column limit",
                        cells.len()
                    )));
                }
            }
        }
        if total_rows > MAX_TOTAL_ROWS {
            return Err(ToolError::from_message(format!(
                "office_export: {total_rows} total rows exceeds the {MAX_TOTAL_ROWS} sanity cap — \
                 split the export or pass a pre-filtered table"
            )));
        }

        // --- Build the workbook ---
        let mut workbook = rust_xlsxwriter::Workbook::new();
        for s in sheets {
            let name = s.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            let ws = workbook.add_worksheet();
            ws.set_name(name).map_err(|e| {
                ToolError::from_message(format!("office_export: set_name \"{name}\": {e}"))
            })?;
            for (r, row) in s
                .get("rows")
                .and_then(|v| v.as_array())
                .unwrap_or(&Vec::new())
                .iter()
                .enumerate()
            {
                for (c, cell) in row
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .enumerate()
                {
                    // r/c fits u32/u16: validated against MAX_TOTAL_ROWS/MAX_COLS above.
                    write_cell(ws, r as u32, c as u16, cell)?;
                }
            }
        }

        // --- Save ---
        let path = Path::new(abs);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ToolError::from_message(format!(
                        "office_export: create_dir_all {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }
        workbook.save(path).map_err(|e| {
            ToolError::from_message(format!("office_export: save {}: {e}", path.display()))
        })?;
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        Ok(json!({
            "ok": true,
            "path": abs,
            "format": "xlsx",
            "sheets": sheets.len(),
            "total_rows": total_rows,
            "size_bytes": size,
            "note": "Verify before delivering: re-read via markitdown and check row counts (spreadsheet-analysis skill)."
        })
        .to_string())
    }

    /// Shared save helper: create parent dirs, write bytes, return size.
    fn save_bytes(path: &Path, bytes: &[u8]) -> Result<u64, ToolError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ToolError::from_message(format!(
                        "office_export: create_dir_all {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }
        std::fs::write(path, bytes).map_err(|e| {
            ToolError::from_message(format!("office_export: write {}: {e}", path.display()))
        })?;
        Ok(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
    }

    /// DOCX export: heading/paragraph/bullet blocks → real Word document.
    fn export_docx(&self, params: &Value, abs: &str) -> Result<String, ToolError> {
        use docx_rs::{AbstractNumbering, Docx, Level, LevelJc, LevelText, NumberFormat, Numbering, NumberingId, IndentLevel, Paragraph, Run, Start, Style};

        let blocks = params
            .get("blocks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::from_message("office_export: missing `blocks` array"))?;
        if blocks.is_empty() {
            return Err(ToolError::from_message(
                "office_export: `blocks` must contain at least one block",
            ));
        }
        if blocks.len() > MAX_BLOCKS {
            return Err(ToolError::from_message(format!(
                "office_export: {len} blocks exceeds the {MAX_BLOCKS} sanity cap",
                len = blocks.len()
            )));
        }
        let mut total_chars = 0usize;
        for (i, b) in blocks.iter().enumerate() {
            let text = b.get("text").and_then(|v| v.as_str()).unwrap_or_default();
            if text.trim().is_empty() {
                return Err(ToolError::from_message(format!(
                    "office_export: blocks[{i}].text is empty"
                )));
            }
            total_chars += text.chars().count();
            if total_chars > MAX_TEXT_CHARS {
                return Err(ToolError::from_message(format!(
                    "office_export: total text exceeds the {MAX_TEXT_CHARS} char sanity cap"
                )));
            }
        }

        // One bullet numbering definition shared by all bullet blocks.
        //
        // Heading styles MUST be declared explicitly. `Paragraph::style("Heading1")`
        // only *references* a style id; `docx-rs` does not ship the definition.
        // Without the `add_style` calls below, `styles.xml` contains nothing but
        // `Normal`, every heading renders as body text, and Word's navigation
        // pane / table of contents see a flat document. Verified by reading the
        // generated file back with python-docx: `len(doc.styles) == 1` before
        // this fix.
        let mut docx = Docx::new()
            .add_abstract_numbering(
                AbstractNumbering::new(1).add_level(Level::new(
                    0,
                    Start::new(1),
                    NumberFormat::new("bullet"),
                    LevelText::new("•"),
                    LevelJc::new("left"),
                )),
            )
            .add_numbering(Numbering::new(1, 1))
            .add_style(
                Style::new("Heading1", docx_rs::StyleType::Paragraph)
                    .name("Heading 1")
                    .outline_lvl(0),
            )
            .add_style(
                Style::new("Heading2", docx_rs::StyleType::Paragraph)
                    .name("Heading 2")
                    .outline_lvl(1),
            )
            .add_style(
                Style::new("Heading3", docx_rs::StyleType::Paragraph)
                    .name("Heading 3")
                    .outline_lvl(2),
            );

        for b in blocks {
            let text = b.get("text").and_then(|v| v.as_str()).unwrap_or_default();
            let kind = b.get("type").and_then(|v| v.as_str()).unwrap_or("paragraph");
            let run = Run::new().add_text(text);
            let paragraph = match kind {
                "heading1" => Paragraph::new().style("Heading1").add_run(run),
                "heading2" => Paragraph::new().style("Heading2").add_run(run),
                "heading3" => Paragraph::new().style("Heading3").add_run(run),
                "bullet" => Paragraph::new()
                    .numbering(NumberingId::new(1), IndentLevel::new(0))
                    .add_run(run),
                "paragraph" => Paragraph::new().add_run(run),
                other => {
                    return Err(ToolError::from_message(format!(
                        "office_export: unknown block type \"{other}\" — supported: \
                         heading1, heading2, heading3, paragraph, bullet"
                    )));
                }
            };
            docx = docx.add_paragraph(paragraph);
        }

        let path = Path::new(abs);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ToolError::from_message(format!(
                        "office_export: create_dir_all {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }
        let file = std::fs::File::create(path).map_err(|e| {
            ToolError::from_message(format!(
                "office_export: create {}: {e}",
                path.display()
            ))
        })?;
        docx.pack(file).map_err(|e| {
            ToolError::from_message(format!("office_export: pack {}: {e}", path.display()))
        })?;
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        Ok(json!({
            "ok": true,
            "path": abs,
            "format": "docx",
            "blocks": blocks.len(),
            "total_chars": total_chars,
            "size_bytes": size,
            "note": "Verify before delivering: re-read via markitdown and check the heading structure."
        })
        .to_string())
    }

    /// PPTX export: title + bullets per slide → real presentation.
    fn export_pptx(&self, params: &Value, abs: &str) -> Result<String, ToolError> {
        use ppt_rs::{create_pptx_with_content, SlideContent};

        let presentation_name = params
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Presentation");
        let slides = params
            .get("slides")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::from_message("office_export: missing `slides` array"))?;
        if slides.is_empty() {
            return Err(ToolError::from_message(
                "office_export: `slides` must contain at least one slide",
            ));
        }
        if slides.len() > MAX_SLIDES {
            return Err(ToolError::from_message(format!(
                "office_export: {} slides exceeds the {MAX_SLIDES} sanity cap",
                slides.len()
            )));
        }
        let mut total_chars = 0usize;
        for (i, s) in slides.iter().enumerate() {
            let title = s
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| {
                    ToolError::from_message(format!(
                        "office_export: slides[{i}] missing `title`"
                    ))
                })?;
            total_chars += title.chars().count();
            let bullets = s.get("bullets").and_then(|v| v.as_array());
            for (j, b) in bullets.unwrap_or(&Vec::new()).iter().enumerate() {
                let text = b.as_str().unwrap_or_default();
                if text.trim().is_empty() {
                    return Err(ToolError::from_message(format!(
                        "office_export: slides[{i}].bullets[{j}] is empty or not a string"
                    )));
                }
                total_chars += text.chars().count();
            }
        }
        if total_chars > MAX_TEXT_CHARS {
            return Err(ToolError::from_message(format!(
                "office_export: total text exceeds the {MAX_TEXT_CHARS} char sanity cap"
            )));
        }

        let mut built: Vec<SlideContent> = Vec::with_capacity(slides.len());
        for s in slides {
            let title = s.get("title").and_then(|v| v.as_str()).unwrap_or_default();
            let mut slide = SlideContent::new(title);
            if let Some(bullets) = s.get("bullets").and_then(|v| v.as_array()) {
                for b in bullets {
                    // unwrap_or_default unreachable: validated above.
                    slide = slide.add_bullet(b.as_str().unwrap_or_default());
                }
            }
            built.push(slide);
        }

        let bytes = create_pptx_with_content(presentation_name, built)
            .map_err(|e| ToolError::from_message(format!("office_export: pptx build: {e}")))?;
        let path = Path::new(abs);
        let size = Self::save_bytes(path, &bytes)?;

        Ok(json!({
            "ok": true,
            "path": abs,
            "format": "pptx",
            "slides": slides.len(),
            "total_chars": total_chars,
            "size_bytes": size,
            "note": "Verify before delivering: re-read via markitdown and check slide titles."
        })
        .to_string())
    }

    fn office_schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Output file path (absolute, or workspace-relative), with the matching extension. Overwrites if the file exists (mirrors write_file)."
            }),
        );
        props.insert(
            "format".into(),
            json!({
                "type": "string",
                "enum": ["xlsx", "docx", "pptx"],
                "description": "Output format. xlsx → requires `sheets`; docx → requires `blocks`; pptx → requires `slides`."
            }),
        );
        props.insert(
            "sheets".into(),
            json!({
                "type": "array",
                "description": "One or more sheets, in order.",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Sheet name (1-31 chars, unique per workbook)." },
                        "rows": {
                            "type": "array",
                            "description": "Rows of cells. Each cell: string | number | bool | null (blank).",
                            "items": { "type": "array", "items": {} }
                        }
                    },
                    "required": ["name", "rows"]
                }
            }),
        );
        tool_schema(
            "office_export",
            "Write structured data to a REAL binary Office file — .xlsx workbook, .docx Word document, or .pptx presentation (all editable in Office/WPS).\n\n\
             ## When to use\n\
             Delivering data as an editable spreadsheet (pairs with spreadsheet-analysis skill); delivering a Word/PPT deliverable the canvas pipeline cannot produce. Format branches: xlsx → `sheets` [{name, rows[[cell]]}], cells are string|number|bool|null; docx → `blocks` [{type: heading1|heading2|heading3|paragraph|bullet, text}]; pptx → `title` + `slides` [{title, bullets: [string]}].\n\n\
             ## When NOT to use\n\
             Reading any Office file (markitdown); styled/flowing documents and HTML slides (document-authoring canvas — this tool produces plain structured exports, not designed layouts); CSV dumps (write_file).\n\n\
             ## Discipline\n\
             Overwrite allowed (mirrors write_file). After export, verify via markitdown read-back and report the absolute path in your reply.",
            JsonSchema::object(props, vec!["path".into(), "format".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_name_and_required_params() {
        let s = OfficeExportHandler.schema();
        assert_eq!(s.name, "office_export");
        let desc = s.description.as_str();
        assert!(desc.contains("When NOT to use"), "three-element desc: {desc}");
        assert!(desc.contains("xlsx"));
    }

    #[tokio::test]
    async fn happy_path_writes_multi_sheet_xlsx() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("out").join("report.xlsx");
        let handler = OfficeExportHandler;
        let result = handler
            .execute(json!({
                "path": path.to_string_lossy(),
                "format": "xlsx",
                "sheets": [
                    {"name": "汇总", "rows": [
                        ["地区", "销售额", "环比", "备注"],
                        ["华东", 1200.5, -0.03, null],
                        ["华南", 980, true, "新渠道"]
                    ]},
                    {"name": "明细", "rows": [["id"], [1], [2]]}
                ]
            }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["sheets"], 2);
        assert_eq!(parsed["total_rows"], 6);
        // File exists and is a non-empty zip (xlsx = zip container)
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 100, "xlsx should be a non-trivial zip");
        assert_eq!(&bytes[0..2], b"PK", "xlsx is a zip container");
    }

    #[tokio::test]
    async fn rejects_unsupported_format_explicitly() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("out.odp");
        let err = OfficeExportHandler
            .execute(json!({
                "path": path.to_string_lossy(),
                "format": "odp",
                "slides": [{"title": "t"}]
            }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not supported"),
            "got: {err}"
        );
        assert!(!path.exists(), "nothing should be written on rejection");
    }

    #[tokio::test]
    async fn docx_export_writes_heading_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("out").join("报告.docx");
        let result = OfficeExportHandler
            .execute(json!({
                "path": path.to_string_lossy(),
                "format": "docx",
                "blocks": [
                    {"type": "heading1", "text": "季度报告"},
                    {"type": "paragraph", "text": "本季度营收同比增长 12%。"},
                    {"type": "heading2", "text": "风险"},
                    {"type": "bullet", "text": "汇率波动"},
                    {"type": "bullet", "text": "供应链延迟"}
                ]
            }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["blocks"], 5);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 100);
        assert_eq!(&bytes[0..2], b"PK", "docx is a zip container");
    }

    #[tokio::test]
    async fn docx_rejects_unknown_block_type_and_empty_text() {
        let handler = OfficeExportHandler;
        let base = tempfile::tempdir().unwrap();
        let p = base.path().join("a.docx").to_string_lossy().to_string();

        let err = handler
            .execute(json!({
                "path": p, "format": "docx",
                "blocks": [{"type": "table", "text": "x"}]
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown block type"), "got: {err}");

        let err = handler
            .execute(json!({
                "path": p, "format": "docx",
                "blocks": [{"type": "paragraph", "text": "  "}]
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("text is empty"), "got: {err}");
    }

    #[tokio::test]
    async fn pptx_export_writes_slides() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("out").join("deck.pptx");
        let result = OfficeExportHandler
            .execute(json!({
                "path": path.to_string_lossy(),
                "format": "pptx",
                "title": "季度汇报",
                "slides": [
                    {"title": "概览", "bullets": ["营收 +12%", "成本 -3%"]},
                    {"title": "风险"}
                ]
            }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["slides"], 2);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 100);
        assert_eq!(&bytes[0..2], b"PK", "pptx is a zip container");
    }

    #[tokio::test]
    async fn pptx_rejects_missing_title_and_empty_bullet() {
        let handler = OfficeExportHandler;
        let base = tempfile::tempdir().unwrap();
        let p = base.path().join("a.pptx").to_string_lossy().to_string();

        let err = handler
            .execute(json!({
                "path": p, "format": "pptx",
                "slides": [{"bullets": ["x"]}]
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing `title`"), "got: {err}");

        let err = handler
            .execute(json!({
                "path": p, "format": "pptx",
                "slides": [{"title": "t", "bullets": ["ok", "   "]}]
            }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("empty or not a string"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_duplicate_sheet_names_and_bad_cells() {
        let handler = OfficeExportHandler;
        let base = tempfile::tempdir().unwrap();
        let p = base.path().join("a.xlsx").to_string_lossy().to_string();

        let err = handler
            .execute(json!({
                "path": p, "format": "xlsx",
                "sheets": [{"name": "s", "rows": [["a"]]}, {"name": "s", "rows": [["b"]]}]
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("duplicate sheet name"));

        let err = handler
            .execute(json!({
                "path": p, "format": "xlsx",
                "sheets": [{"name": "s", "rows": [[{"nested": 1}]]}]
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Cells must be"), "got: {err}");
    }

    #[tokio::test]
    async fn overwrite_is_allowed_and_reports_size() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.xlsx").to_string_lossy().to_string();
        let handler = OfficeExportHandler;
        for rows in [vec![["a"]], vec![["a"], ["b"], ["c"]]] {
            handler
                .execute(json!({
                    "path": p, "format": "xlsx",
                    "sheets": [{"name": "s", "rows": rows}]
                }))
                .await
                .unwrap();
        }
        let meta = std::fs::metadata(&p).unwrap();
        assert!(meta.len() > 0);
    }

    /// End-to-end: produce all three Office formats and assert the bytes are
    /// real OOXML (ZIP with the expected parts), not a renamed text file.
    ///
    /// The stronger cross-check lives in the shell script that reads these
    /// files back with Python openpyxl / python-docx / python-pptx — an
    /// *independent* implementation. A writer that only agrees with its own
    /// reader can be confidently wrong; this test only guarantees structure.
    #[tokio::test]
    async fn e2e_writes_all_three_formats() {
        let tmp = tempfile::tempdir().unwrap();
        let handler = OfficeExportHandler;

        // ---- xlsx: two sheets, mixed cell types ----
        let xlsx = tmp.path().join("report.xlsx");
        let out = handler
            .execute(json!({
                "path": xlsx.to_string_lossy(),
                "format": "xlsx",
                "sheets": [
                    {
                        "name": "Sales",
                        "rows": [
                            ["Region", "Q1", "Q2", "Active"],
                            ["North", 1200, 1450.5, true],
                            ["South", 980, 1100, false],
                            ["East", 0, 0, null]
                        ]
                    },
                    {
                        "name": "汇总",
                        "rows": [
                            ["指标", "数值"],
                            ["总销售额", 4730.5],
                            ["区域数", 3]
                        ]
                    }
                ]
            }))
            .await
            .expect("xlsx export should succeed");
        eprintln!("xlsx: {out}");
        assert!(xlsx.exists());
        assert_ooxml(&xlsx, "xl/workbook.xml");

        // ---- docx: headings, paragraphs, bullets ----
        let docx = tmp.path().join("report.docx");
        let out = handler
            .execute(json!({
                "path": docx.to_string_lossy(),
                "format": "docx",
                "blocks": [
                    {"type": "heading1", "text": "季度报告"},
                    {"type": "heading2", "text": "销售概况"},
                    {"type": "paragraph", "text": "本季度总销售额 4730.5，环比增长 12%。"},
                    {"type": "bullet", "text": "北区领先，贡献 1450.5"},
                    {"type": "bullet", "text": "南区稳定，贡献 1100"},
                    {"type": "heading3", "text": "风险"},
                    {"type": "paragraph", "text": "东区两个季度均为零，需要排查。"}
                ]
            }))
            .await
            .expect("docx export should succeed");
        eprintln!("docx: {out}");
        assert!(docx.exists());
        assert_ooxml(&docx, "word/document.xml");

        // ---- pptx: title slide + content slides ----
        let pptx = tmp.path().join("deck.pptx");
        let out = handler
            .execute(json!({
                "path": pptx.to_string_lossy(),
                "format": "pptx",
                "title": "季度回顾",
                "slides": [
                    {"title": "销售概况", "bullets": ["总销售额 4730.5", "环比 +12%"]},
                    {"title": "风险", "bullets": ["东区连续两季为零", "需要排查渠道"]}
                ]
            }))
            .await
            .expect("pptx export should succeed");
        eprintln!("pptx: {out}");
        assert!(pptx.exists());
        assert_ooxml(&pptx, "ppt/presentation.xml");
    }

    /// Assert `path` looks like real OOXML: a ZIP archive (PK\x03\x04 magic)
    /// whose central directory mentions `member`.
    ///
    /// Deliberately does not pull in the `zip` crate: the point of this
    /// assertion is to catch "a text file renamed to .xlsx", and the magic
    /// plus a substring scan over the central directory does that. The
    /// *content* correctness check is done by an independent reader
    /// (openpyxl / python-docx / python-pptx) — a writer that only agrees with
    /// its own reader can be confidently wrong.
    fn assert_ooxml(path: &std::path::Path, member: &str) {
        let bytes = std::fs::read(path).expect("file must exist");
        assert!(
            bytes.starts_with(b"PK\x03\x04"),
            "{} must be a ZIP archive (OOXML), first bytes: {:?}",
            path.display(),
            &bytes[..bytes.len().min(8)]
        );
        // ZIP stores entry names in the central directory as plain UTF-8.
        let haystack = String::from_utf8_lossy(&bytes);
        assert!(
            haystack.contains(member),
            "{} must contain the OOXML part {member}",
            path.display()
        );
        // A real document has more than a stub; 1 KB is far below any valid
        // workbook/document/presentation.
        assert!(
            bytes.len() > 1024,
            "{} is suspiciously small ({} bytes)",
            path.display(),
            bytes.len()
        );
    }

    /// Emit the three sample files to `$OFFICE_VERIFY_DIR` for out-of-process
    /// verification with an independent reader (openpyxl / python-docx /
    /// python-pptx). Ignored by default: it is a fixture generator, not an
    /// assertion.
    ///
    ///   OFFICE_VERIFY_DIR=<refs>/office cargo test -p hermix-tools \
    ///     --lib office_verify_emit -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "fixture generator for independent verification"]
    async fn office_verify_emit() {
        let dir = std::env::var("OFFICE_VERIFY_DIR")
            .expect("set OFFICE_VERIFY_DIR to an existing directory");
        let dir = std::path::PathBuf::from(dir);
        let handler = OfficeExportHandler;

        handler.execute(json!({
            "path": dir.join("report.xlsx").to_string_lossy(),
            "format": "xlsx",
            "sheets": [
                {"name": "Sales", "rows": [
                    ["Region", "Q1", "Q2", "Active"],
                    ["North", 1200, 1450.5, true],
                    ["South", 980, 1100, false],
                    ["East", 0, 0, null]
                ]},
                {"name": "汇总", "rows": [
                    ["指标", "数值"],
                    ["总销售额", 4730.5],
                    ["区域数", 3]
                ]}
            ]
        })).await.expect("xlsx");

        handler.execute(json!({
            "path": dir.join("report.docx").to_string_lossy(),
            "format": "docx",
            "blocks": [
                {"type": "heading1", "text": "季度报告"},
                {"type": "heading2", "text": "销售概况"},
                {"type": "paragraph", "text": "本季度总销售额 4730.5，环比增长 12%。"},
                {"type": "bullet", "text": "北区领先，贡献 1450.5"},
                {"type": "bullet", "text": "南区稳定，贡献 1100"},
                {"type": "heading3", "text": "风险"},
                {"type": "paragraph", "text": "东区两个季度均为零，需要排查。"}
            ]
        })).await.expect("docx");

        handler.execute(json!({
            "path": dir.join("deck.pptx").to_string_lossy(),
            "format": "pptx",
            "title": "季度回顾",
            "slides": [
                {"title": "销售概况", "bullets": ["总销售额 4730.5", "环比 +12%"]},
                {"title": "风险", "bullets": ["东区连续两季为零", "需要排查渠道"]}
            ]
        })).await.expect("pptx");

        eprintln!("emitted to {}", dir.display());
    }

    /// Regression guard for the bug found on 2026-11-18: `Paragraph::style("Heading1")`
    /// only *references* a style id, and `docx-rs` ships no heading definitions.
    /// The generated `styles.xml` therefore contained nothing but `Normal`, so
    /// every heading rendered as body text and Word's navigation pane / TOC
    /// saw a flat document. Verified externally with python-docx, which
    /// reported `len(doc.styles) == 1` before the fix and 4 after.
    ///
    /// This asserts on the raw `styles.xml` bytes so it stays in-process: the
    /// heading style ids must appear in the document's style table.
    #[tokio::test]
    async fn docx_declares_heading_styles() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("h.docx");
        OfficeExportHandler
            .execute(json!({
                "path": path.to_string_lossy(),
                "format": "docx",
                "blocks": [
                    {"type": "heading1", "text": "One"},
                    {"type": "heading2", "text": "Two"},
                    {"type": "heading3", "text": "Three"},
                    {"type": "paragraph", "text": "body"}
                ]
            }))
            .await
            .expect("docx export");

        // ZIP entry names are stored uncompressed in the central directory,
        // so a raw byte scan finds `word/styles.xml`. Pull the local header
        // offset and inflate that entry by hand — small and dependency-free.
        let bytes = std::fs::read(&path).unwrap();
        let styles = extract_zip_entry(&bytes, "word/styles.xml")
            .expect("docx must contain word/styles.xml");
        let xml = String::from_utf8_lossy(&styles);
        for id in ["Heading1", "Heading2", "Heading3"] {
            assert!(
                xml.contains(id),
                "styles.xml must define {id}; got: {} chars of styles",
                xml.len()
            );
        }
        // A style id alone is not enough — the paragraph must also point at it.
        let doc = extract_zip_entry(&bytes, "word/document.xml")
            .expect("docx must contain word/document.xml");
        let doc_xml = String::from_utf8_lossy(&doc);
        assert!(doc_xml.contains("Heading1"), "a paragraph must reference Heading1");
    }

    /// Minimal ZIP entry reader for tests only.
    ///
    /// Avoids adding the `zip` crate as a dev-dependency just to read back
    /// what we wrote. `docx-rs` stores entries uncompressed (method 0), so the
    /// stored branch is the one that matters; deflate is handled by returning
    /// `None` rather than silently producing garbage.
    fn extract_zip_entry(bytes: &[u8], name: &str) -> Option<Vec<u8>> {
        let mut i = 0usize;
        while i + 30 <= bytes.len() {
            if &bytes[i..i + 4] != b"PK\x03\x04" {
                i += 1;
                continue;
            }
            let method = u16::from_le_bytes([bytes[i + 8], bytes[i + 9]]);
            let comp_size = u32::from_le_bytes([
                bytes[i + 18], bytes[i + 19], bytes[i + 20], bytes[i + 21],
            ]) as usize;
            let name_len = u16::from_le_bytes([bytes[i + 26], bytes[i + 27]]) as usize;
            let extra_len = u16::from_le_bytes([bytes[i + 28], bytes[i + 29]]) as usize;
            let name_start = i + 30;
            if name_start + name_len > bytes.len() {
                return None;
            }
            let entry_name = String::from_utf8_lossy(&bytes[name_start..name_start + name_len]);
            let data_start = name_start + name_len + extra_len;
            if entry_name == name {
                let end = data_start.checked_add(comp_size)?;
                if end > bytes.len() {
                    return None;
                }
                let raw = &bytes[data_start..end];
                return match method {
                    0 => Some(raw.to_vec()),
                    // docx-rs writes stored entries; a deflated one would mean
                    // the writer changed, which is worth noticing rather than
                    // silently decoding.
                    _ => None,
                };
            }
            i = data_start.saturating_add(comp_size.max(1));
        }
        None
    }
}
