//! `create_presentation` tool — high-level PPT generator.
//!
//! Inspired by oh-my-ppt's JSON→slide approach. Instead of the LLM
//! making 100+ individual `office_cli` calls (one per shape/text),
//! the LLM generates a single JSON spec describing the slides, and
//! this tool maps it to a sequence of office_cli commands.
//!
//! ## Flow
//! 1. LLM calls `create_presentation` with theme + slides JSON
//! 2. Tool parses spec, resolves theme
//! 3. Tool generates office_cli command sequence:
//!    - `create <file>` (blank .pptx)
//!    - For each slide: `add slide` + `add shape` (title) + `add shape` (body)
//!    - Set background, colors, fonts from theme
//! 4. Tool executes each command via terminal backend
//! 5. Returns summary (slide count, file path, any errors)
//!
//! ## Why not batch JSON?
//! office_cli batch mode requires a JSON file on disk. We'd have to
//! write it first, then call batch. Simpler to just run individual
//! commands — officecli has a resident mode that keeps the file in
//! memory after the first access (60s timeout), so sequential calls
//! are fast.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::sync::Arc;

use hermix_core::{
    tool_schema, JsonSchema, TerminalBackend, ToolError, ToolHandler, ToolSchema,
    traits::RiskLevel,
};

use crate::ToolRegistry;
use crate::tools::ppt_themes::{
    self, budget, cm, get_theme, list_themes, PresentationSpec, SlideLayout, SlideSpec,
};

pub struct CreatePresentationHandler {
    backend: Arc<dyn TerminalBackend>,
    workspace: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
}

impl CreatePresentationHandler {
    pub fn new(
        backend: Arc<dyn TerminalBackend>,
        workspace: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
    ) -> Self {
        Self { backend, workspace }
    }

    fn resolve_binary() -> Result<String, ToolError> {
        // Reuse the same resolution logic as office_cli
        let candidates: Vec<std::path::PathBuf> = vec![
            std::path::PathBuf::from("officecli-x86_64-pc-windows-msvc.exe"),
            {
                let exe = std::env::current_exe().unwrap_or_default();
                exe.parent()
                    .map(|p| p.join("binaries").join("officecli-x86_64-pc-windows-msvc.exe"))
                    .unwrap_or_default()
            },
            {
                let exe = std::env::current_exe().unwrap_or_default();
                exe.parent()
                    .map(|p| {
                        p.join("..")
                            .join("src-tauri")
                            .join("binaries")
                            .join("officecli-x86_64-pc-windows-msvc.exe")
                    })
                    .unwrap_or_default()
            },
        ];
        for c in &candidates {
            if c.exists() {
                return Ok(c.to_string_lossy().to_string());
            }
        }
        Err(ToolError::ExecutionFailed(
            "officecli binary not found.".to_string(),
        ))
    }

    fn workdir(&self) -> Option<String> {
        self.workspace
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|p| p.to_string_lossy().to_string()))
    }

    async fn run_cmd(&self, cmd: &str, timeout_ms: u64) -> Result<String, ToolError> {
        let workdir = self.workdir();
        match self
            .backend
            .execute_command(cmd, Some(timeout_ms), workdir.as_deref(), false, false)
            .await
        {
            Ok(output) => {
                if output.exit_code != 0 && !output.stdout.is_empty() {
                    // officecli may print warnings to stderr but still succeed
                    tracing::warn!(exit = output.exit_code, "officecli non-zero exit");
                }
                Ok(format!(
                    "{}{}",
                    output.stdout,
                    if output.stderr.is_empty() {
                        String::new()
                    } else {
                        format!("\n[stderr] {}", output.stderr)
                    }
                ))
            }
            Err(e) => Err(ToolError::ExecutionFailed(e.to_string())),
        }
    }

    /// Escape a string for use in office_cli --prop value.
    /// The command is passed through shell; we need to handle special chars.
    fn escape_prop(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "")
    }

    /// Build the office_cli command string for an add operation.
    fn build_add_cmd(
        binary: &str,
        file: &str,
        parent: &str,
        element_type: &str,
        props: &[String],
    ) -> String {
        let mut args = vec![
            "add".to_string(),
            Self::quote(file),
            Self::quote(parent),
            "--type".to_string(),
            element_type.to_string(),
        ];
        for p in props {
            args.push("--prop".to_string());
            args.push(Self::quote(p));
        }
        format!("{} {}", binary, args.join(" "))
    }

    fn quote(s: &str) -> String {
        if s.contains(' ') || s.contains('"') || s.contains('\\') {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        } else {
            s.to_string()
        }
    }

    /// Generate office_cli commands for a single slide.
    fn gen_slide_cmds(
        binary: &str,
        file: &str,
        slide_idx: usize,
        spec: &SlideSpec,
        theme: &ppt_themes::PptTheme,
    ) -> Vec<String> {
        let mut cmds = Vec::new();
        let slide_path = format!("/slide[{}]", slide_idx);

        // 1. Add slide with background
        let accent = if spec.accent.is_empty() {
            theme.accents[(slide_idx - 1) % theme.accents.len()]
        } else {
            spec.accent.as_str()
        };

        let slide_props = vec![
            format!("background={}", theme.background),
            format!("layout=blank"),
        ];
        cmds.push(Self::build_add_cmd(
            binary,
            file,
            "/",
            "slide",
            &slide_props,
        ));

        // 2. Layout-specific content
        match spec.layout {
            SlideLayout::Title => {
                // Large centered title + subtitle
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("text={}", Self::escape_prop(&spec.title)),
                        format!("x={}", cm(2.0)),
                        format!("y={}", cm(4.0)),
                        format!("w={}", cm(21.4)),
                        format!("h={}", cm(3.0)),
                        format!("font={}", theme.title_font),
                        format!("size=44"),
                        format!("color={}", theme.title_color),
                        format!("align=center"),
                        format!("bold=true"),
                        format!("fill=none"),
                        format!("line=none"),
                    ],
                ));
                if !spec.subtitle.is_empty() {
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text={}", Self::escape_prop(&spec.subtitle)),
                            format!("x={}", cm(4.0)),
                            format!("y={}", cm(7.5)),
                            format!("w={}", cm(17.4)),
                            format!("h={}", cm(1.5)),
                            format!("font={}", theme.body_font),
                            format!("size=20"),
                            format!("color={}", theme.muted_color),
                            format!("align=center"),
                            format!("fill=none"),
                            format!("line=none"),
                        ],
                    ));
                }
                // Decorative accent bar
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("x={}", cm(10.7)),
                        format!("y={}", cm(3.5)),
                        format!("w={}", cm(4.0)),
                        format!("h={}", cm(0.15)),
                        format!("fill={}", accent),
                        format!("line=none"),
                    ],
                ));
            }

            SlideLayout::Section => {
                // Accent bar on left + section name
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("x={}", cm(1.5)),
                        format!("y={}", cm(5.5)),
                        format!("w={}", cm(0.3)),
                        format!("h={}", cm(3.0)),
                        format!("fill={}", accent),
                        format!("line=none"),
                    ],
                ));
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("text={}", Self::escape_prop(&spec.title)),
                        format!("x={}", cm(2.5)),
                        format!("y={}", cm(5.5)),
                        format!("w={}", cm(20.0)),
                        format!("h={}", cm(2.5)),
                        format!("font={}", theme.title_font),
                        format!("size=36"),
                        format!("color={}", theme.title_color),
                        format!("align=left"),
                        format!("bold=true"),
                        format!("fill=none"),
                        format!("line=none"),
                    ],
                ));
                if !spec.subtitle.is_empty() {
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text={}", Self::escape_prop(&spec.subtitle)),
                            format!("x={}", cm(2.5)),
                            format!("y={}", cm(8.0)),
                            format!("w={}", cm(20.0)),
                            format!("h={}", cm(1.5)),
                            format!("font={}", theme.body_font),
                            format!("size=18"),
                            format!("color={}", theme.muted_color),
                            format!("align=left"),
                            format!("fill=none"),
                            format!("line=none"),
                        ],
                    ));
                }
            }

            SlideLayout::Content | SlideLayout::Bullets => {
                // Title at top
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("text={}", Self::escape_prop(&spec.title)),
                        format!("x={}", cm(1.5)),
                        format!("y={}", cm(1.0)),
                        format!("w={}", cm(22.4)),
                        format!("h={}", cm(1.5)),
                        format!("font={}", theme.title_font),
                        format!("size=28"),
                        format!("color={}", theme.title_color),
                        format!("align=left"),
                        format!("bold=true"),
                        format!("fill=none"),
                        format!("line=none"),
                    ],
                ));
                // Accent underline
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("x={}", cm(1.5)),
                        format!("y={}", cm(2.6)),
                        format!("w={}", cm(3.0)),
                        format!("h={}", cm(0.1)),
                        format!("fill={}", accent),
                        format!("line=none"),
                    ],
                ));

                if spec.layout == SlideLayout::Bullets {
                    // Bullet items
                    let items: Vec<&String> = spec.body.iter().take(budget::MAX_BULLETS).collect();
                    let start_y = 3.5;
                    let item_h = 1.5;
                    for (i, item) in items.iter().enumerate() {
                        let y = start_y + (i as f64) * item_h;
                        let item_accent = theme.accents[i % theme.accents.len()];
                        // Bullet dot
                        cmds.push(Self::build_add_cmd(
                            binary, file, &slide_path, "shape",
                            &[
                                format!("x={}", cm(1.5)),
                                format!("y={}", cm(y + 0.25)),
                                format!("w={}", cm(0.3)),
                                format!("h={}", cm(0.3)),
                                format!("fill={}", item_accent),
                                format!("line=none"),
                            ],
                        ));
                        // Bullet text
                        let text = if item.len() > budget::BULLET_MAX_CHARS {
                            &item[..budget::BULLET_MAX_CHARS]
                        } else {
                            item.as_str()
                        };
                        cmds.push(Self::build_add_cmd(
                            binary, file, &slide_path, "shape",
                            &[
                                format!("text={}", Self::escape_prop(text)),
                                format!("x={}", cm(2.2)),
                                format!("y={}", cm(y)),
                                format!("w={}", cm(20.5)),
                                format!("h={}", cm(item_h - 0.2)),
                                format!("font={}", theme.body_font),
                                format!("size=18"),
                                format!("color={}", theme.body_color),
                                format!("align=left"),
                                format!("fill=none"),
                                format!("line=none"),
                            ],
                        ));
                    }
                } else {
                    // Content: body text in a single block
                    let body_text = spec.body.join("\\n");
                    let truncated = if body_text.len() > budget::BODY_MAX_CHARS {
                        &body_text[..budget::BODY_MAX_CHARS]
                    } else {
                        &body_text
                    };
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text={}", Self::escape_prop(truncated)),
                            format!("x={}", cm(1.5)),
                            format!("y={}", cm(3.5)),
                            format!("w={}", cm(22.4)),
                            format!("h={}", cm(8.0)),
                            format!("font={}", theme.body_font),
                            format!("size=18"),
                            format!("color={}", theme.body_color),
                            format!("align=left"),
                            format!("fill=none"),
                            format!("line=none"),
                        ],
                    ));
                }
            }

            SlideLayout::TwoColumn => {
                // Title
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("text={}", Self::escape_prop(&spec.title)),
                        format!("x={}", cm(1.5)),
                        format!("y={}", cm(1.0)),
                        format!("w={}", cm(22.4)),
                        format!("h={}", cm(1.5)),
                        format!("font={}", theme.title_font),
                        format!("size=28"),
                        format!("color={}", theme.title_color),
                        format!("align=left"),
                        format!("bold=true"),
                        format!("fill=none"),
                        format!("line=none"),
                    ],
                ));
                // Left column
                if !spec.body.is_empty() {
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text={}", Self::escape_prop(&spec.body[0])),
                            format!("x={}", cm(1.5)),
                            format!("y={}", cm(3.5)),
                            format!("w={}", cm(10.5)),
                            format!("h={}", cm(8.0)),
                            format!("font={}", theme.body_font),
                            format!("size=16"),
                            format!("color={}", theme.body_color),
                            format!("align=left"),
                            format!("fill=none"),
                            format!("line=none"),
                        ],
                    ));
                }
                // Right column
                if spec.body.len() > 1 {
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text={}", Self::escape_prop(&spec.body[1])),
                            format!("x={}", cm(13.0)),
                            format!("y={}", cm(3.5)),
                            format!("w={}", cm(10.5)),
                            format!("h={}", cm(8.0)),
                            format!("font={}", theme.body_font),
                            format!("size=16"),
                            format!("color={}", theme.body_color),
                            format!("align=left"),
                            format!("fill=none"),
                            format!("line=none"),
                        ],
                    ));
                }
            }

            SlideLayout::Quote => {
                // Large centered quote + attribution
                let quote = if spec.quote.len() > budget::QUOTE_MAX_CHARS {
                    &spec.quote[..budget::QUOTE_MAX_CHARS]
                } else {
                    &spec.quote
                };
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("text={}", Self::escape_prop(quote)),
                        format!("x={}", cm(3.0)),
                        format!("y={}", cm(3.5)),
                        format!("w={}", cm(19.4)),
                        format!("h={}", cm(5.0)),
                        format!("font={}", theme.title_font),
                        format!("size=28"),
                        format!("color={}", theme.title_color),
                        format!("align=center"),
                        format!("italic=true"),
                        format!("fill=none"),
                        format!("line=none"),
                    ],
                ));
                if !spec.attribution.is_empty() {
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text=— {}", Self::escape_prop(&spec.attribution)),
                            format!("x={}", cm(3.0)),
                            format!("y={}", cm(9.0)),
                            format!("w={}", cm(19.4)),
                            format!("h={}", cm(1.5)),
                            format!("font={}", theme.body_font),
                            format!("size=18"),
                            format!("color={}", theme.muted_color),
                            format!("align=center"),
                            format!("fill=none"),
                            format!("line=none"),
                        ],
                    ));
                }
            }

            SlideLayout::Data => {
                // Title + placeholder for chart/data
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("text={}", Self::escape_prop(&spec.title)),
                        format!("x={}", cm(1.5)),
                        format!("y={}", cm(1.0)),
                        format!("w={}", cm(22.4)),
                        format!("h={}", cm(1.5)),
                        format!("font={}", theme.title_font),
                        format!("size=28"),
                        format!("color={}", theme.title_color),
                        format!("align=left"),
                        format!("bold=true"),
                        format!("fill=none"),
                        format!("line=none"),
                    ],
                ));
                // Data area placeholder
                if !spec.body.is_empty() {
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text={}", Self::escape_prop(&spec.body.join("\\n"))),
                            format!("x={}", cm(1.5)),
                            format!("y={}", cm(3.5)),
                            format!("w={}", cm(22.4)),
                            format!("h={}", cm(8.0)),
                            format!("font={}", theme.body_font),
                            format!("size=16"),
                            format!("color={}", theme.body_color),
                            format!("align=left"),
                            format!("fill={}", accent),
                            format!("fill.alpha=10"),
                            format!("line=none"),
                        ],
                    ));
                }
            }

            SlideLayout::ImageText => {
                // Title at top
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("text={}", Self::escape_prop(&spec.title)),
                        format!("x={}", cm(1.5)),
                        format!("y={}", cm(1.0)),
                        format!("w={}", cm(22.4)),
                        format!("h={}", cm(1.5)),
                        format!("font={}", theme.title_font),
                        format!("size=28"),
                        format!("color={}", theme.title_color),
                        format!("align=left"),
                        format!("bold=true"),
                        format!("fill=none"),
                        format!("line=none"),
                    ],
                ));
                // Image placeholder (if path provided, use picture; else placeholder shape)
                if let Some(img_path) = &spec.image {
                    if !img_path.is_empty() {
                        cmds.push(Self::build_add_cmd(
                            binary, file, &slide_path, "picture",
                            &[format!("src={}", img_path)],
                        ));
                    }
                }
                // Text on right
                if !spec.body.is_empty() {
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text={}", Self::escape_prop(&spec.body.join("\\n"))),
                            format!("x={}", cm(13.0)),
                            format!("y={}", cm(3.5)),
                            format!("w={}", cm(10.5)),
                            format!("h={}", cm(8.0)),
                            format!("font={}", theme.body_font),
                            format!("size=16"),
                            format!("color={}", theme.body_color),
                            format!("align=left"),
                            format!("fill=none"),
                            format!("line=none"),
                        ],
                    ));
                }
            }

            SlideLayout::Closing => {
                // Centered "Thank You" + contact info
                cmds.push(Self::build_add_cmd(
                    binary, file, &slide_path, "shape",
                    &[
                        format!("text={}", if spec.title.is_empty() { "Thank You".to_string() } else { spec.title.clone() }),
                        format!("x={}", cm(2.0)),
                        format!("y={}", cm(4.5)),
                        format!("w={}", cm(21.4)),
                        format!("h={}", cm(3.0)),
                        format!("font={}", theme.title_font),
                        format!("size=44"),
                        format!("color={}", theme.title_color),
                        format!("align=center"),
                        format!("bold=true"),
                        format!("fill=none"),
                        format!("line=none"),
                    ],
                ));
                if !spec.subtitle.is_empty() {
                    cmds.push(Self::build_add_cmd(
                        binary, file, &slide_path, "shape",
                        &[
                            format!("text={}", Self::escape_prop(&spec.subtitle)),
                            format!("x={}", cm(4.0)),
                            format!("y={}", cm(8.0)),
                            format!("w={}", cm(17.4)),
                            format!("h={}", cm(1.5)),
                            format!("font={}", theme.body_font),
                            format!("size=20"),
                            format!("color={}", theme.muted_color),
                            format!("align=center"),
                            format!("fill=none"),
                            format!("line=none"),
                        ],
                    ));
                }
            }
        }

        // 3. Transition (smooth fade for all slides)
        cmds.push(format!(
            "{} set {} {} --prop transition=fade",
            binary, file, Self::quote(&slide_path)
        ));

        // 4. Speaker notes (if provided)
        if !spec.notes.is_empty() {
            cmds.push(Self::build_add_cmd(
                binary, file, &slide_path, "notes",
                &[format!("text={}", Self::escape_prop(&spec.notes))],
            ));
        }

        cmds
    }
}

#[async_trait]
impl ToolHandler for CreatePresentationHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        // Parse the spec from either inline JSON or a file path
        let spec: PresentationSpec = if let Some(spec_str) = params.get("spec").and_then(|v| v.as_str()) {
            serde_json::from_str(spec_str).map_err(|e| {
                ToolError::InvalidParams(format!("Failed to parse 'spec' JSON: {}", e))
            })?
        } else if let Some(spec_obj) = params.get("spec").and_then(|v| v.as_object()) {
            let json_str = serde_json::to_string(spec_obj).map_err(|e| {
                ToolError::InvalidParams(format!("Failed to re-serialize spec: {}", e))
            })?;
            serde_json::from_str(&json_str).map_err(|e| {
                ToolError::InvalidParams(format!("Failed to parse spec object: {}", e))
            })?
        } else {
            return Err(ToolError::InvalidParams(
                "Missing 'spec' parameter (expected JSON object or string)".into(),
            ));
        };

        if spec.slides.is_empty() {
            return Err(ToolError::InvalidParams(
                "spec.slides must not be empty".into(),
            ));
        }

        let theme = get_theme(&spec.theme);
        let binary = Self::resolve_binary()?;
        let file = &spec.file;

        let mut all_cmds = Vec::new();

        // 1. Create the .pptx file
        all_cmds.push(format!("{} create {}", binary, Self::quote(file)));

        // 2. Open in resident mode for faster sequential writes
        all_cmds.push(format!("{} open {}", binary, Self::quote(file)));

        // 3. Generate commands for each slide
        for (i, slide) in spec.slides.iter().enumerate() {
            let slide_idx = i + 1; // 1-based
            let cmds = Self::gen_slide_cmds(&binary, file, slide_idx, slide, theme);
            all_cmds.extend(cmds);
        }

        // 4. Close and save
        all_cmds.push(format!("{} close {}", binary, Self::quote(file)));

        // Execute all commands
        let total = all_cmds.len();
        let mut errors = Vec::new();

        for (i, cmd) in all_cmds.iter().enumerate() {
            tracing::debug!(cmd_index = i, total, cmd = %cmd, "executing office_cli command");
            match self.run_cmd(cmd, 60000).await {
                Ok(_output) => {}
                Err(e) => {
                    let err_msg = format!("Command {}/{} failed: {} | cmd: {}", i + 1, total, e, cmd);
                    tracing::warn!(%err_msg);
                    errors.push(err_msg);
                    // Continue with remaining commands — partial result is better than nothing
                }
            }
        }

        // Build summary
        let summary = if errors.is_empty() {
            format!(
                "✅ Created {} with {} slides (theme: {}). File: {}",
                file,
                spec.slides.len(),
                theme.display_name,
                file,
            )
        } else {
            format!(
                "⚠️ Created {} with {} slides (theme: {}), but {} of {} commands had errors.\nErrors:\n{}",
                file,
                spec.slides.len(),
                theme.display_name,
                errors.len(),
                total,
                errors.join("\n"),
            )
        };

        Ok(summary)
    }

    fn schema(&self) -> ToolSchema {
        let theme_list: Vec<Value> = list_themes()
            .iter()
            .map(|(name, display, desc)| {
                json!({
                    "name": name,
                    "display_name": display,
                    "description": desc,
                })
            })
            .collect();

        let _layout_desc = "Slide layout type. Each layout has predefined element positions:\n\
            - `title`: Large centered title + subtitle (cover slide)\n\
            - `section`: Accent bar + section name (divider)\n\
            - `content`: Title at top + body text below\n\
            - `two_column`: Title + two columns of text\n\
            - `bullets`: Title + 3-6 bullet points with colored dots\n\
            - `image_text`: Title + image + text side by side\n\
            - `data`: Title + data/chart area\n\
            - `quote`: Large centered italic quote + attribution\n\
            - `closing`: Centered 'Thank You' + contact info";

        let spec_desc = format!(
            "Presentation specification JSON. Structure:\n\
            ```json\n\
            {{\n  \"theme\": \"midnight\",\n  \"file\": \"output.pptx\",\n  \"title\": \"My Presentation\",\n  \
            \"slides\": [\n    {{\n      \"layout\": \"title\",\n      \"title\": \"Presentation Title\",\n      \
            \"subtitle\": \"Subtitle text\",\n      \"body\": [\"bullet 1\", \"bullet 2\"],\n      \
            \"accent\": \"\",\n      \"notes\": \"speaker notes\"\n    }}\n  ]\n}}\n```\n\n\
            ## Available themes\n{}\n\n\
            ## Content budget (max chars per element)\n\
            - title: {} chars\n- subtitle: {} chars\n- bullet item: {} chars (max {} items)\n\
            - body: {} chars\n- quote: {} chars\n\n\
            ## Tips\n\
            - Use `title` layout for the first slide, `section` for dividers, `closing` for the last\n\
            - Cycle accents by leaving `accent` empty — the theme cycles through 5 colors\n\
            - Keep bullet items under {} chars for clean display\n\
            - Total slides: 8-15 is optimal for a 15-min presentation",
            serde_json::to_string_pretty(&theme_list).unwrap_or_default(),
            budget::TITLE_MAX_CHARS,
            budget::SUBTITLE_MAX_CHARS,
            budget::BULLET_MAX_CHARS,
            budget::MAX_BULLETS,
            budget::BODY_MAX_CHARS,
            budget::QUOTE_MAX_CHARS,
            budget::BULLET_MAX_CHARS,
        );

        let mut props = IndexMap::new();
        props.insert(
            "spec".into(),
            json!({
                "type": "object",
                "description": spec_desc,
            }),
        );

        tool_schema(
            "create_presentation",
            "Create a beautiful PPT presentation from a JSON spec. \
             Picks a theme (colors, fonts, layouts) and generates slides with \
             consistent visual design. No manual positioning needed — the tool \
             handles all coordinates, colors, and typography based on the theme.\n\n\
             ## Workflow\n\
             1. Plan your presentation outline (title, sections, content)\n\
             2. Pick a theme that matches the tone (midnight=tech, clean=academic, etc.)\n\
             3. Generate the spec JSON and call this tool\n\
             4. Use `office_cli(action=\"view\", file=\"...\", mode=\"screenshot\")` to preview\n\
             5. If adjustments needed, use `office_cli` to edit individual elements",
            JsonSchema::object(props, vec!["spec".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub fn register(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    terminal_workspace: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
) {
    let handler = Arc::new(CreatePresentationHandler::new(terminal_backend, terminal_workspace));
    let schema = handler.schema();
    registry.register_with_risk(
        "create_presentation",
        "office",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Create a beautifully designed PPT from JSON spec with theme templates.",
        "🎨",
        None,
        RiskLevel::Caution,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ppt_themes::SlideLayout;

    #[test]
    fn test_escape_prop() {
        assert_eq!(CreatePresentationHandler::escape_prop("hello"), "hello");
        assert_eq!(CreatePresentationHandler::escape_prop("a\"b"), "a\\\"b");
        assert_eq!(CreatePresentationHandler::escape_prop("a\\b"), "a\\\\b");
        assert_eq!(CreatePresentationHandler::escape_prop("line1\nline2"), "line1\\nline2");
    }

    #[test]
    fn test_quote() {
        assert_eq!(CreatePresentationHandler::quote("hello"), "hello");
        assert_eq!(CreatePresentationHandler::quote("hello world"), "\"hello world\"");
    }

    #[test]
    fn test_gen_slide_cmds_title_layout() {
        let theme = get_theme("midnight");
        let spec = SlideSpec {
            layout: SlideLayout::Title,
            title: "Test Title".to_string(),
            subtitle: "Test Subtitle".to_string(),
            body: vec![],
            image: None,
            quote: String::new(),
            attribution: String::new(),
            accent: String::new(),
            notes: String::new(),
        };
        let cmds = CreatePresentationHandler::gen_slide_cmds("officecli", "test.pptx", 1, &spec, theme);
        // Should have: add slide, add title shape, add subtitle shape, add accent bar, set transition
        assert!(cmds.len() >= 4);
        assert!(cmds[0].contains("add"));
        assert!(cmds[0].contains("slide"));
    }

    #[test]
    fn test_gen_slide_cmds_bullets_layout() {
        let theme = get_theme("clean");
        let spec = SlideSpec {
            layout: SlideLayout::Bullets,
            title: "Key Points".to_string(),
            subtitle: String::new(),
            body: vec!["First point".to_string(), "Second point".to_string(), "Third point".to_string()],
            image: None,
            quote: String::new(),
            attribution: String::new(),
            accent: String::new(),
            notes: String::new(),
        };
        let cmds = CreatePresentationHandler::gen_slide_cmds("officecli", "test.pptx", 2, &spec, theme);
        // Should have: add slide, add title, add underline, 3×(dot+text), transition
        assert!(cmds.len() >= 8);
    }

    #[test]
    fn test_gen_slide_cmds_closing_layout() {
        let theme = get_theme("carbon");
        let spec = SlideSpec {
            layout: SlideLayout::Closing,
            title: "Thank You".to_string(),
            subtitle: "contact@example.com".to_string(),
            body: vec![],
            image: None,
            quote: String::new(),
            attribution: String::new(),
            accent: String::new(),
            notes: String::new(),
        };
        let cmds = CreatePresentationHandler::gen_slide_cmds("officecli", "test.pptx", 3, &spec, theme);
        assert!(cmds.len() >= 3);
    }
}
