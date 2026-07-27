//! Batch OCR rename: scan a directory of images, run LOCAL OCR
//! (PP-OCRv5 mobile) in-process, propose new filenames derived from
//! recognised identity fields (plate number, owner/company name,
//! certificate type), and — gated by an approval card — rename them.
//!
//! Design invariants (see AGENTS.md "Zero-context-bloat"):
//! * Images NEVER enter the LLM context. OCR runs inside the Rust
//!   process via `crate::tools::file::ocr_engine()`; only the compact
//!   OCR JSON (text + confidence) is returned to the LLM.
//! * `dry_run` defaults to `true`: the first call returns a preview
//!   plan and touches nothing on disk. The second call (`dry_run:
//!   false`) performs the renames and is `RiskLevel::Dangerous`, so the
//!   dispatch gate forces an `AwaitApproval` card before any file moves.
//! * Files flagged `low_confidence_count > 0` (blurry / ambiguous text)
//!   should be sent to a sub-agent `read_image` (vision) for correction
//!   before applying — the tool does not guess.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{
    tool_schema, traits::RiskLevel, JsonSchema, ToolError, ToolHandler, ToolSchema,
};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ToolRegistry;
use crate::tools::file::ocr_engine;
use hermix_ocr::{OcrEngine, OcrWord};

const IMG_EXTS: &[&str] = &["jpg", "jpeg", "png", "bmp", "webp", "tif", "tiff"];
const CONF_THRESHOLD: f32 = 0.7;
const NAME_CHAR_LIMIT: usize = 60;

/// Known certificate / document type keywords. The first one found in
/// the OCR text becomes the `type` component of the proposed filename.
const CERT_KEYWORDS: &[&str] = &[
    "道路运输证",
    "营运证",
    "行驶证",
    "驾驶证",
    "身份证",
    "营业执照",
    "登记证书",
    "车辆购置税",
    "发票",
    "合格证",
];

#[derive(Default)]
pub struct BatchOcrRenameHandler;

/// Register `batch_ocr_rename` as a `Dangerous` file tool. The danger
/// level guarantees the dispatch gate (`local_agent_service::
/// decide_pre_tool_dispatch`) fires an `AwaitApproval` card before the
/// (destructive) rename runs.
pub fn register(registry: &ToolRegistry) {
    registry.register_with_risk(
        "batch_ocr_rename",
        "file",
        BatchOcrRenameHandler::default().schema(),
        Arc::new(BatchOcrRenameHandler::default()),
        Arc::new(|| true),
        vec![],
        true,
        "Scan a directory of images, run LOCAL OCR (PP-OCRv5) IN-PROCESS, and propose new filenames derived from recognised identity fields (plate number, owner/company name, certificate type). Images never enter the LLM context — only compact OCR text does. \
         ⚠️ DANGEROUS: renames files on disk. Defaults to `dry_run: true` (returns a preview plan, touches nothing). Review the plan, then call again with `dry_run: false` — that call requires your explicit approval card. \
         For any file flagged `low_confidence_count > 0` (blurry / ambiguous), delegate a sub-agent `read_image` for vision correction BEFORE applying.",
        "🏷️",
        None,
        RiskLevel::Dangerous,
        /* override_existing = */ false,
    );
}

// ---------------------------------------------------------------------------
// Pure helpers (no IO — unit-testable)
// ---------------------------------------------------------------------------

/// Replace filesystem-illegal characters, collapse runs of `_`, and cap
/// length. Always returns a non-empty, safe filename stem.
fn sanitize_filename(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' | '/' | ':' | '：' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if c.is_whitespace() => out.push('_'),
            c => out.push(c),
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    let out: String = out.chars().take(NAME_CHAR_LIMIT).collect();
    if out.is_empty() {
        "file".to_string()
    } else {
        out
    }
}

/// Take the first meaningful token following a key (e.g. `车牌号码：`),
/// stripping the colon/space separator and sanitizing it. OCR often
/// inserts an English label (`PlateNo.`, `Vehicle`) between the key and
/// the real value, so we skip pure-English label fragments and require
/// the token to contain at least one digit or CJK character.
fn grab_after(text: &str, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(idx) = text.find(k) {
            let rest = &text[idx + k.len()..];
            let rest = rest.trim_start_matches(|c| {
                matches!(c, '：' | ':' | ' ' | '\u{a0}' | '　')
            });
            for tok in rest.split_whitespace() {
                let val = sanitize_filename(tok);
                if !val.is_empty() && has_digit_or_cjk(&val) {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// True if `s` contains at least one ASCII digit or CJK character —
/// used to reject pure-English label tokens (e.g. `PlateNo.`,
/// `Vehicle`) when grabbing field values.
fn has_digit_or_cjk(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_ascii_digit() || ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// Heuristic: does this token look like a Chinese license plate?
/// Province char (CJK) + digits (+ optional letter), length >= 6.
fn looks_like_plate(tok: &str) -> bool {
    let has_cjk = tok.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    let has_digit = tok.chars().any(|c| c.is_ascii_digit());
    tok.chars().count() >= 6 && has_cjk && has_digit
}

/// Extract (plate, owner_name, cert_type) from joined OCR text.
fn extract_fields(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let plate = grab_after(text, &["车牌号码", "号牌号码", "车牌号", "号牌"]).or_else(|| {
        text.split_whitespace()
            .find(|t| looks_like_plate(t))
            .map(|s| sanitize_filename(s))
    });
    let name = grab_after(
        text,
        &["所有人", "业户名称", "车辆所有人", "业户", "姓名", "名称"],
    );
    let cert = CERT_KEYWORDS
        .iter()
        .find(|k| text.contains(**k))
        .map(|s| s.to_string());
    (plate, name, cert)
}

// ---------------------------------------------------------------------------
// Filesystem helpers (mirror MoveFileHandler's safe rename)
// ---------------------------------------------------------------------------

/// Walk `dir` (optionally recursive) collecting image files.
fn collect_images(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if recursive {
                    collect_images(&p, recursive, out);
                }
            } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                if IMG_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
                    out.push(p);
                }
            }
        }
    }
}

/// Move `src` → `dst` with overwrite protection and cross-device
/// (EXDEV) fallback to copy + delete. Mirrors `MoveFileHandler::move_entry`.
fn safe_rename(src: &Path, dst: &Path, overwrite: bool) -> Result<(), String> {
    if dst.exists() {
        if !overwrite {
            return Err(format!(
                "目标已存在: '{}'（设 overwrite:true 覆盖）",
                dst.display()
            ));
        }
        if dst.is_dir() {
            std::fs::remove_dir_all(dst)
                .map_err(|e| format!("覆盖目标目录失败 '{}': {}", dst.display(), e))?;
        } else {
            std::fs::remove_file(dst)
                .map_err(|e| format!("覆盖目标文件失败 '{}': {}", dst.display(), e))?;
        }
    }
    if let Some(parent) = dst.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建目标父目录失败 '{}': {}", parent.display(), e))?;
        }
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(17) /* EXDEV */ => {
            std::fs::copy(src, dst)
                .map_err(|e| format!("跨盘复制失败 '{} → {}': {}", src.display(), dst.display(), e))?;
            std::fs::remove_file(src)
                .map_err(|e| format!("跨盘移动后删除源失败 '{}': {}", src.display(), e))?;
            Ok(())
        }
        Err(e) => Err(format!(
            "移动失败 '{} → {}': {}",
            src.display(),
            dst.display(),
            e
        )),
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

#[async_trait]
impl ToolHandler for BatchOcrRenameHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let dir = params
            .get("dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'dir' parameter".into()))?;
        let dir_path = Path::new(dir);
        if !dir_path.is_dir() {
            return Err(ToolError::InvalidParams(format!(
                "'dir' is not a directory or does not exist: '{}'",
                dir
            )));
        }
        let pattern = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let recursive = params
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let dry_run = params
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let overwrite = params
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // 1. Collect candidate image files.
        let mut files = Vec::new();
        collect_images(dir_path, recursive, &mut files);
        if let Some(pat) = &pattern {
            // Simple substring filter on the file name.
            files.retain(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n.contains(pat))
                    .unwrap_or(false)
            });
        }
        files.sort();

        if files.is_empty() {
            return Ok(json!({
                "dry_run": dry_run,
                "scanned": 0,
                "plan": [],
                "summary": "No image files found in the given directory."
            })
            .to_string());
        }

        // 2. OCR each file in-process (blocking work off the async runtime).
        let mut plan: Vec<Value> = Vec::with_capacity(files.len());
        let mut ocr_available = true;

        for path in &files {
            let path_str = path.to_string_lossy().to_string();
            let (words, ocr_err): (Vec<OcrWord>, Option<String>) =
                tokio::task::spawn_blocking(move || match ocr_engine() {
                    Ok(engine) => match engine.recognize_path(&path_str) {
                        Ok(w) => (w, None),
                        Err(e) => (Vec::new(), Some(format!("{e}"))),
                    },
                    Err(e) => (
                        Vec::new(),
                        Some(format!(
                            "OCR engine unavailable: {e}. Ensure models exist at \
                             <hermix_home>/ocr/, or delegate a sub-agent read_image \
                             for vision fallback."
                        )),
                    ),
                })
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("OCR task failed: {e}")))?;

            if ocr_err.is_some() {
                ocr_available = false;
            }

            let full_text = words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let confs: Vec<f32> = words.iter().map(|w| w.confidence).collect();
            let avg = if confs.is_empty() {
                0.0
            } else {
                confs.iter().sum::<f32>() / confs.len() as f32
            };
            let low_conf = words
                .iter()
                .filter(|w| w.confidence < CONF_THRESHOLD)
                .count();

            let original_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Extract identity fields once (reused for both the proposed
            // name and the JSON report).
            let (plate, name, cert) = if ocr_err.is_none() {
                extract_fields(&full_text)
            } else {
                (None, None, None)
            };

            let (proposed_stem, ocr_failed) = if let Some(err) = &ocr_err {
                // OCR unavailable — cannot propose; mark failed.
                (original_stem.clone(), Some(err.clone()))
            } else {
                let mut parts: Vec<String> = Vec::new();
                if let Some(c) = &cert {
                    parts.push(c.clone());
                }
                let ident = plate.clone().or(name.clone());
                if let Some(i) = ident {
                    parts.push(i);
                }
                let mut slug = parts.join("_");
                if slug.is_empty() {
                    // Fallback: top-3 highest-confidence tokens.
                    let mut ws: Vec<&OcrWord> = words.iter().collect();
                    ws.sort_by(|a, b| {
                        b.confidence
                            .partial_cmp(&a.confidence)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let toks: Vec<String> = ws
                        .iter()
                        .take(3)
                        .map(|w| sanitize_filename(&w.text))
                        .filter(|s| !s.is_empty())
                        .collect();
                    slug = toks.join("_");
                }
                if slug.is_empty() {
                    slug = original_stem.clone();
                }
                (sanitize_filename(&slug), None)
            };

            let new_name = if ext.is_empty() {
                proposed_stem.clone()
            } else {
                format!("{proposed_stem}.{ext}")
            };
            let new_path = dir_path.join(&new_name);
            let unchanged = proposed_stem == original_stem;

            plan.push(json!({
                "old_path": path.display().to_string(),
                "proposed_name": new_name,
                "new_path": new_path.display().to_string(),
                "cert_type": cert,
                "identifier": {
                    "plate": plate,
                    "name": name,
                },
                "confidence_avg": avg,
                "low_confidence_count": low_conf,
                "ocr_text_excerpt": full_text.chars().take(200).collect::<String>(),
                "ocr_failed": ocr_failed.is_some(),
                "unchanged": unchanged,
            }));
        }

        // 3a. Dry-run: return the preview plan, touch nothing.
        if dry_run {
            let ready = plan
                .iter()
                .filter(|e| !e["ocr_failed"].as_bool().unwrap_or(false) && !e["unchanged"].as_bool().unwrap_or(false))
                .count();
            let failed = plan
                .iter()
                .filter(|e| e["ocr_failed"].as_bool().unwrap_or(false))
                .count();
            let low = plan
                .iter()
                .filter(|e| e["low_confidence_count"].as_u64().unwrap_or(0) > 0)
                .count();
            return Ok(json!({
                "dry_run": true,
                "scanned": files.len(),
                "ocr_available": ocr_available,
                "proposed_count": ready,
                "ocr_failed_count": failed,
                "low_confidence_files": low,
                "plan": plan,
                "summary": format!(
                    "Scanned {} image(s). {} rename proposal(s) ready, {} OCR-failed, {} need vision review (low_confidence_count>0). \
                     Review the plan; call again with dry_run:false to apply (requires approval).",
                    files.len(), ready, failed, low
                ),
            })
            .to_string());
        }

        // 3b. Apply: perform the renames, deduplicating proposed stems.
        let mut used: HashSet<String> = HashSet::new();
        let mut applied: Vec<Value> = Vec::with_capacity(plan.len());
        let mut renamed = 0;
        let mut skipped = 0;

        for entry in &plan {
            let old = entry["old_path"].as_str().unwrap_or("").to_string();
            let new = entry["new_path"].as_str().unwrap_or("").to_string();
            let ocr_failed = entry["ocr_failed"].as_bool().unwrap_or(false);
            let unchanged = entry["unchanged"].as_bool().unwrap_or(false);
            let old_p = Path::new(&old);
            let new_p = Path::new(&new);

            if ocr_failed {
                applied.push(json!({
                    "old_path": old, "new_path": new,
                    "status": "skipped_ocr_failed",
                    "error": "OCR unavailable for this file; delegate read_image for vision fallback.",
                }));
                skipped += 1;
                continue;
            }
            if unchanged {
                applied.push(json!({
                    "old_path": old, "new_path": new,
                    "status": "unchanged",
                }));
                skipped += 1;
                continue;
            }

            // Deduplicate the destination stem within this batch.
            let ext = new_p
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let stem = new_p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            let mut final_stem = stem.clone();
            let mut counter = 2;
            while used.contains(&final_stem) {
                final_stem = format!("{stem}_{counter}");
                counter += 1;
            }
            used.insert(final_stem.clone());
            let final_new = if ext.is_empty() {
                dir_path.join(&final_stem)
            } else {
                dir_path.join(format!("{final_stem}.{ext}"))
            };

            match safe_rename(old_p, &final_new, overwrite) {
                Ok(()) => {
                    applied.push(json!({
                        "old_path": old,
                        "new_path": final_new.display().to_string(),
                        "status": "renamed",
                    }));
                    renamed += 1;
                }
                Err(e) => {
                    applied.push(json!({
                        "old_path": old,
                        "new_path": final_new.display().to_string(),
                        "status": "error",
                        "error": e,
                    }));
                    skipped += 1;
                }
            }
        }

        Ok(json!({
            "dry_run": false,
            "scanned": files.len(),
            "renamed": renamed,
            "skipped": skipped,
            "applied": applied,
            "summary": format!(
                "Applied. {renamed} file(s) renamed, {skipped} skipped (unchanged / OCR-failed / error). \
                 Review `applied` for per-file results. Re-running is idempotent."
            ),
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "dir".into(),
            json!({
                "type": "string",
                "description": "Directory containing the images to scan and (optionally) rename."
            }),
        );
        props.insert(
            "pattern".into(),
            json!({
                "type": "string",
                "description": "Optional substring filter on file names (e.g. '行驶证' to only process those). Default: all images."
            }),
        );
        props.insert(
            "recursive".into(),
            json!({
                "type": "boolean",
                "default": false,
                "description": "If true, recurse into subdirectories. Default: false (current directory only)."
            }),
        );
        props.insert(
            "dry_run".into(),
            json!({
                "type": "boolean",
                "default": true,
                "description": "If true (default), ONLY return the rename plan and touch nothing. Set false to actually rename — that call requires explicit approval (Dangerous tool)."
            }),
        );
        props.insert(
            "overwrite".into(),
            json!({
                "type": "boolean",
                "default": false,
                "description": "If true, overwrite a destination that already exists. Default false (skip with an error)."
            }),
        );
        tool_schema(
            "batch_ocr_rename",
            "Scan images in a directory, run LOCAL OCR (PP-OCRv5) in-process, and propose / apply new filenames from recognised identity fields (plate, owner, cert type). \
             Images never enter the LLM context. DANGEROUS: actual rename (dry_run:false) requires approval. \
             For files with low_confidence_count>0, delegate a sub-agent read_image for vision correction first.",
            JsonSchema::object(props, vec!["dir".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_illegal_chars_and_caps_length() {
        assert_eq!(sanitize_filename("a/b:c*?"), "a_b_c");
        assert_eq!(sanitize_filename("   "), "file");
        let long = "x".repeat(80);
        assert!(sanitize_filename(&long).chars().count() <= NAME_CHAR_LIMIT);
    }

    #[test]
    fn extract_fields_finds_plate_and_cert() {
        let text = "中华人民共和国道路运输证 车牌号码：沪F-T8697 业户名称：上海安抵运输有限公司";
        let (plate, name, cert) = extract_fields(text);
        assert_eq!(plate.as_deref(), Some("沪F-T8697"));
        assert_eq!(name.as_deref(), Some("上海安抵运输有限公司"));
        assert_eq!(cert.as_deref(), Some("道路运输证"));
    }

    #[test]
    fn looks_like_plate_requires_cjk_and_digit() {
        assert!(looks_like_plate("沪F-T8697"));
        assert!(!looks_like_plate("LGAG4DY39K9010529")); // VIN: no CJK
        assert!(!looks_like_plate("abc")); // too short, no CJK
    }

    #[test]
    fn grab_after_skips_english_label_before_value() {
        // 黑DA5889 行驶证 OCR inserts "PlateNo." between 号牌号码 and the
        // real plate. The grabber must skip the label and return 黑DA5889.
        let text = "号牌号码 PlateNo. 黑DA5889 车辆类型 重型半挂牵引车";
        let plate = grab_after(text, &["车牌号码", "号牌号码", "车牌号", "号牌"]);
        assert_eq!(plate.as_deref(), Some("黑DA5889"));
    }
}
