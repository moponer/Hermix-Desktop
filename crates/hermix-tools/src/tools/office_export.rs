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
        "Write structured data to a REAL .xlsx spreadsheet (editable in Excel/WPS). \
         Use for: delivering processed/merged/cleaned data as an editable spreadsheet — \
         pairs with the spreadsheet-analysis skill's read-back verification. \
         NOT for: reading spreadsheets (use markitdown), styled reports/pages \
         (use the document-authoring canvas). Currently supports format=\"xlsx\" \
         only; .docx/.pptx are planned and rejected with an explicit error.",
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
        if !format.eq_ignore_ascii_case("xlsx") {
            return Err(ToolError::from_message(format!(
                "office_export: format \"{format}\" is not supported yet — only \"xlsx\". \
                 .docx/.pptx export is planned (see the binary-office-export decision note); \
                 meanwhile use the document-authoring canvas for documents/slides."
            )));
        }
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
        let abs = to_absolute_path(path_str);
        let path = Path::new(&abs);
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

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Output .xlsx path (absolute, or workspace-relative). Overwrites if the file exists (mirrors write_file)."
            }),
        );
        props.insert(
            "format".into(),
            json!({
                "type": "string",
                "enum": ["xlsx"],
                "description": "Output format. Only \"xlsx\" is supported today; .docx/.pptx are planned."
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
            "Write structured data to a REAL .xlsx spreadsheet (editable in Excel/WPS).\n\n\
             ## When to use\n\
             Delivering processed/merged/cleaned data as an editable spreadsheet — the write side of the spreadsheet-analysis skill (read schema → process → export → read back to verify).\n\n\
             ## When NOT to use\n\
             Reading spreadsheets (markitdown); styled documents/slides (document-authoring canvas); CSV dumps (write_file).\n\n\
             ## Discipline\n\
             Cells are string | number | bool | null. Sheet names ≤ 31 chars, unique. Overwrite allowed (mirrors write_file). After export, verify via markitdown read-back and report the absolute path.",
            JsonSchema::object(props, vec!["path".into(), "format".into(), "sheets".into()]),
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
        let path = tmp.path().join("out.docx");
        let err = OfficeExportHandler
            .execute(json!({
                "path": path.to_string_lossy(),
                "format": "docx",
                "sheets": [{"name": "s", "rows": [["a"]]}]
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not supported yet"), "got: {err}");
        assert!(!path.exists(), "nothing should be written on rejection");
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
}
