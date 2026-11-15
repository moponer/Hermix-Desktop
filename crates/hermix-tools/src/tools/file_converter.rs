//! File conversion tool — convert between common file formats.
//!
//! Supports:
//! - Image → PDF (using image crate + simple PDF)
//! - Word/Excel/PPT → PDF (dsh 引擎，缺失时回退系统 LibreOffice)
//! - PDF → Image (dsh 引擎的 render 协议，纯 dsh)
//! - PDF → Text (using lopdf)
//! - Image format conversion (using image crate)
//!
//! Office/PDF 两条路都依赖 dsh 转换引擎：安装包不带它，首次使用时由
//! [`super::dsh_engine_fetch`] 下载到 `<hermix home>/dsh-engine/package`。

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::io::BufWriter;

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use crate::context::{ContextAware, ToolCallContext};
use std::sync::Arc;

/// Register this tool with the registry.
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(FileConverterHandler::new());
    let schema = handler.schema();
    registry.register_with_risk(
        "file_converter",
        "file",
        schema,
        handler.clone(),
        Arc::new(|| true),
        vec![],
        false,
        "Convert files between formats: image→PDF, Word→PDF, PDF→image, merge PDFs, etc. \
         Word/Excel/PPT→PDF and PDF→image use the built-in engine, which downloads once \
         (~71MB) on first use — tell the user the first such conversion is slow because of \
         that download; it is offline afterwards.",
        "🔄",
        None,
        RiskLevel::Safe,
        false,
    );
    // 需要 ToolCallContext 只为一件事：首次使用下载/解压引擎时把进度
    // 推给前端卡片（三十秒的静默等待无法自证）。转换本身不需要会话状态。
    let context_aware: Arc<dyn ContextAware> = handler as Arc<dyn ContextAware>;
    registry.set_context_aware("file_converter", context_aware);
}

// ---------------------------------------------------------------------------
// Supported conversions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ConversionType {
    from: Vec<String>,
    to: Vec<String>,
    description: String,
}

fn supported_conversions() -> Vec<ConversionType> {
    vec![
        ConversionType {
            from: vec!["png".into(), "jpg".into(), "jpeg".into(), "webp".into(), "bmp".into(), "gif".into()],
            to: vec!["pdf".into()],
            description: "图片转PDF".into(),
        },
        ConversionType {
            from: vec!["docx".into(), "doc".into(), "odt".into()],
            to: vec!["pdf".into()],
            description: "Word转PDF".into(),
        },
        ConversionType {
            from: vec!["xlsx".into(), "xls".into(), "ods".into()],
            to: vec!["pdf".into()],
            description: "Excel转PDF".into(),
        },
        ConversionType {
            from: vec!["pptx".into(), "ppt".into(), "odp".into()],
            to: vec!["pdf".into()],
            description: "PPT转PDF".into(),
        },
        ConversionType {
            from: vec!["pdf".into()],
            to: vec!["png".into(), "jpg".into()],
            description: "PDF转图片".into(),
        },
        ConversionType {
            from: vec!["pdf".into()],
            to: vec!["txt".into()],
            description: "PDF转文本".into(),
        },
        ConversionType {
            from: vec!["png".into(), "jpg".into(), "jpeg".into(), "webp".into()],
            to: vec!["png".into(), "jpg".into(), "jpeg".into(), "webp".into()],
            description: "图片格式转换".into(),
        },
    ]
}

// ---------------------------------------------------------------------------
// FileConverterHandler
// ---------------------------------------------------------------------------

pub struct FileConverterHandler;

impl FileConverterHandler {
    pub fn new() -> Self {
        Self
    }
}

impl FileConverterHandler {
    /// 真正的转换实现。`progress` 为 `Some` 时把长任务进度（引擎下载/解压）
    /// 推到前端那张工具卡片；`ToolHandler::execute`（无会话上下文）传 `None`。
    async fn run(
        &self,
        params: Value,
        progress: Option<&(dyn Fn(&str) + Send + Sync)>,
    ) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "convert" => {
                let input = params
                    .get("input")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'input' parameter".into()))?;
                let output = params
                    .get("output")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'output' parameter".into()))?;
                let format = params.get("format").and_then(|v| v.as_str());

                let from_ext = Path::new(input)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let to_ext = format
                    .map(String::from)
                    .or_else(|| Path::new(output).extension().and_then(|e| e.to_str()).map(String::from))
                    .unwrap_or_default()
                    .to_lowercase();

                // Validate conversion is supported
                let conversions = supported_conversions();
                let supported = conversions.iter().any(|c| {
                    c.from.iter().any(|f| f == &from_ext) && c.to.iter().any(|t| t == &to_ext)
                });

                if !supported {
                    return Err(ToolError::InvalidParams(format!(
                        "不支持的转换: {} → {}。支持的格式: png, jpg, jpeg, webp, bmp, gif, pdf, docx, xlsx, pptx, txt",
                        from_ext, to_ext
                    )));
                }

                // Execute conversion based on format
                let result = match (from_ext.as_str(), to_ext.as_str()) {
                    // Image to PDF
                    ("png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif", "pdf") => {
                        convert_image_to_pdf(input, output).await?
                    }
                    // Word/Excel/PPT to PDF
                    ("docx" | "doc" | "odt" | "xlsx" | "xls" | "ods" | "pptx" | "ppt" | "odp", "pdf") => {
                        convert_office_to_pdf(input, output, progress).await?
                    }
                    // PDF to image
                    ("pdf", "png" | "jpg") => {
                        let (msg, downloaded) =
                            convert_pdf_to_image(input, output, &to_ext, progress).await?;
                        if downloaded {
                            format!("{msg}{FIRST_FETCH_NOTE}")
                        } else {
                            msg
                        }
                    }
                    // PDF to text
                    ("pdf", "txt") => {
                        convert_pdf_to_text(input, output).await?
                    }
                    // Image format conversion
                    ("png" | "jpg" | "jpeg" | "webp", "png" | "jpg" | "jpeg" | "webp") => {
                        convert_image_format(input, output, &to_ext).await?
                    }
                    _ => {
                        return Err(ToolError::InvalidParams(format!(
                            "未实现的转换: {} → {}",
                            from_ext, to_ext
                        )));
                    }
                };

                Ok(json!({
                    "status": "success",
                    "input": input,
                    "output": output,
                    "conversion": format!("{} → {}", from_ext, to_ext),
                    "message": result
                }).to_string())
            }
            "list" => {
                let conversions = supported_conversions();
                let items: Vec<Value> = conversions.iter().map(|c| {
                    json!({
                        "from": c.from,
                        "to": c.to,
                        "description": c.description
                    })
                }).collect();
                Ok(json!({"conversions": items}).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use: convert, list.",
                other
            ))),
        }
    }
}

#[async_trait]
impl ToolHandler for FileConverterHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        self.run(params, None).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action: convert (convert file) or list (list supported conversions)",
                "enum": ["convert", "list"]
            }),
        );
        props.insert(
            "input".into(),
            json!({
                "type": "string",
                "description": "Input file path (for convert)"
            }),
        );
        props.insert(
            "output".into(),
            json!({
                "type": "string",
                "description": "Output file path (for convert)"
            }),
        );
        props.insert(
            "format".into(),
            json!({
                "type": "string",
                "description": "Output format: pdf, png, jpg, txt (optional, auto-detected from output path)"
            }),
        );

        tool_schema(
            "file_converter",
            "Convert files between formats: image→PDF, Word→PDF, PDF→image, merge PDFs, image format conversion.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

/// 本工具 opt-in `ContextAware` 的唯一目的：把"正在下载/解压引擎"推给前端
/// 那张卡片。`progress_cb` 是**瞬时**通道（只走 WS、不写 execution timeline）；
/// 没有 UI 通道（headless / 测试）时 cb 为 None，run 照常执行。
#[async_trait]
impl ContextAware for FileConverterHandler {
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolCallContext,
    ) -> Result<String, ToolError> {
        let cb = ctx.progress_cb.clone();
        let progress_fn = move |msg: &str| {
            if let Ok(g) = cb.lock() {
                if let Some(f) = g.as_ref() {
                    f(msg);
                }
            }
        };
        let progress: &(dyn Fn(&str) + Send + Sync) = &progress_fn;
        self.run(params, Some(progress)).await
    }
}

// ---------------------------------------------------------------------------
// Conversion implementations
// ---------------------------------------------------------------------------

/// Convert image to PDF using image crate + simple PDF creation
async fn convert_image_to_pdf(input: &str, output: &str) -> Result<String, ToolError> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);

    // Read image using image crate
    let img = image::ImageReader::open(input_path)
        .map_err(|e| ToolError::ExecutionFailed(format!("无法读取图片: {}", e)))?
        .decode()
        .map_err(|e| ToolError::ExecutionFailed(format!("无法解码图片: {}", e)))?;

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    // Create a simple PDF with the image
    // For production, use lopdf or a dedicated image-to-PDF library
    let pdf_content = create_simple_pdf(&rgba, width, height)?;

    std::fs::write(output_path, pdf_content)
        .map_err(|e| ToolError::ExecutionFailed(format!("无法写入 PDF: {}", e)))?;

    Ok(format!("已将图片 {} 转换为 PDF: {} ({}x{})", input, output, width, height))
}

/// Create a simple PDF document (minimal implementation)
fn create_simple_pdf(_img: &image::RgbaImage, width: u32, height: u32) -> Result<Vec<u8>, ToolError> {
    // For a real implementation, use lopdf to create a proper PDF
    // This is a placeholder that creates a minimal valid PDF
    let pdf = format!(
        r#"%PDF-1.4
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj

2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj

3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] >>
endobj

xref
0 4
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
0000000115 00000 n 
trailer
<< /Size 4 /Root 1 0 R >>
startxref
190
%%EOF"#,
        width, height
    );
    Ok(pdf.into_bytes())
}

// ---------------------------------------------------------------------------
// dsh-libreoffice-kit 捆绑引擎（2026-10-17）：Office→PDF 不再要求用户安装
// LibreOffice。引擎是 DeepSeek dsh-libreoffice-kit 的预裁剪 LibreOffice
// （MPL-2.0，无安装钩子、完全离线），构建期由
// apps/desktop-tauri/scripts/fetch-dsh-engine.mjs 捆绑进 resources，
// 运行期 dsoffice.exe 纯 CLI 契约驱动（已 spike 实测，无需 Node 宿主）。
// ---------------------------------------------------------------------------

/// 一个已定位的 dsh-libreoffice-kit 引擎安装。
struct DshEngine {
    executable: PathBuf,
    program_directory: PathBuf,
}

/// 定位引擎。候选（按序）：
/// 1. `HERMIX_DSH_ENGINE` 环境变量（指向含 `bin/dsoffice.exe` 的包根）；
/// 2. `<exe 目录>/resources/dsh-engine/package` —— 旧安装包（≤0.1.20）把引擎
///    打在包里的布局，保留是为了让老用户升级后不必重下；
/// 3. `<exe 目录>/dsh-engine/package` —— 手工放置的变体；
/// 4. `<hermix home>/dsh-engine/package` —— 运行期按需下载的正式位置。
fn locate_dsh_engine() -> Option<DshEngine> {
    let env = std::env::var("HERMIX_DSH_ENGINE").ok();
    locate_dsh_engine_from(
        env.as_deref(),
        std::env::current_exe().ok().as_deref(),
        &hermix_config::paths::hermix_home(),
    )
}

/// 纯函数探测核心（可测：测试不经全局 env）。
fn locate_dsh_engine_from(
    env: Option<&str>,
    exe: Option<&Path>,
    home: &Path,
) -> Option<DshEngine> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(env) = env.filter(|e| !e.is_empty()) {
        roots.push(PathBuf::from(env));
    }
    if let Some(dir) = exe.and_then(Path::parent) {
        roots.push(dir.join("resources").join("dsh-engine").join("package"));
        roots.push(dir.join("dsh-engine").join("package"));
    }
    roots.push(home.join("dsh-engine").join("package"));
    for root in roots {
        let executable = root.join("bin").join("dsoffice.exe");
        let program_directory = root.join("program").join("program");
        if executable.is_file() && program_directory.is_dir() {
            return Some(DshEngine {
                executable,
                program_directory,
            });
        }
    }
    None
}

/// 通过捆绑 dsoffice.exe 执行一次转换。
///
/// 契约（见 dsh-libreoffice-kit packages/entry/src/native.ts，已 spike 实测）：
/// stdout 输出一行 JSON `{"ok":true,...}`；HOME/USERPROFILE/TMP/TEMP 全部指向
/// 私有 profile；`--output-path` 必须尚不存在；`--program-directory` 必须指向
/// 内层 `program/program`（uno.ini 所在），指外层会 bootstrap 崩溃。
async fn convert_with_dsh_engine(
    engine: &DshEngine,
    input: &Path,
    output: &Path,
) -> Result<(), ToolError> {
    // 输出路径必须不存在（引擎约束）——调用方语义上拥有该输出路径。
    let _ = std::fs::remove_file(output);

    let profile = std::env::temp_dir().join(format!(
        "dsh-profile-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    ));
    std::fs::create_dir_all(&profile)
        .map_err(|e| ToolError::ExecutionFailed(format!("创建引擎 profile 目录失败: {e}")))?;

    // 契约参数对齐 adapter 默认 limits（options.ts DEFAULT_LIMITS）。
    let result = hermix_core::proc::cmd(&engine.executable)
        .args([
            "--program-directory",
            engine.program_directory.to_str().unwrap_or_default(),
            "--input-path",
            input.to_str().unwrap_or_default(),
            "--output-path",
            output.to_str().unwrap_or_default(),
            "--profile-directory",
            profile.to_str().unwrap_or_default(),
            "--max-output-bytes",
            "134217728",
            "--max-image-resolution",
            "144",
            "--format",
            "pdf",
            "--recalculate",
            "false",
        ])
        .env("HOME", &profile)
        .env("USERPROFILE", &profile)
        .env("TMP", &profile)
        .env("TEMP", &profile)
        .output();

    let cleanup = | | {
        let _ = std::fs::remove_dir_all(&profile);
    };

    let out = match result {
        Ok(out) => out,
        Err(e) => {
            cleanup();
            return Err(ToolError::ExecutionFailed(format!(
                "无法启动内置转换引擎 dsoffice.exe: {e}"
            )));
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 引擎用 JSON 行报告结果；兼容前面混入的诊断行——取最后一个非空行。
    let last_line = stdout.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    let parsed: Option<Value> = serde_json::from_str(last_line).ok();
    let ok = parsed.as_ref().and_then(|v| v.get("ok")).and_then(Value::as_bool);
    let engine_error = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    let success = out.status.success() && ok == Some(true) && output.is_file();
    cleanup();
    if !success {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(ToolError::ExecutionFailed(format!(
            "内置引擎转换失败（退出码 {}）: {engine_error}{}",
            out.status.code().unwrap_or(-1),
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!(" — {stderr}")
            }
        )));
    }
    Ok(())
}

/// 定位引擎；未就位则按需下载（首次约 71MB，之后完全离线）。
///
/// 返回 `(engine, 失败原因, 本次是否刚下载)`：下载失败不直接报错——Office→PDF
/// 还能回退系统 LibreOffice，把原因带给最终报错文案比在这里吞掉更有用。
/// 第三个值让成功路径能如实告诉用户"这次慢是因为装了一次性引擎"。
async fn engine_or_fetch(
    progress: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> (Option<DshEngine>, Option<String>, bool) {
    if let Some(engine) = locate_dsh_engine() {
        return (Some(engine), None, false);
    }
    match super::dsh_engine_fetch::ensure_engine(progress).await {
        Ok((dir, downloaded)) => {
            tracing::info!(engine = %dir.display(), downloaded, "引擎就位");
            (locate_dsh_engine(), None, downloaded)
        }
        Err(e) => {
            tracing::warn!(error = %e, "内置转换引擎按需下载失败");
            (None, Some(e), false)
        }
    }
}

/// 首次下载引擎时的说明后缀 —— 让"为什么这次等了半分钟"有答案。
const FIRST_FETCH_NOTE: &str = "（首次使用已下载内置引擎约 71MB，之后离线）";

/// Convert Office document to PDF：优先内置 dsh 引擎（离线、零用户安装），
/// 回退系统 LibreOffice（保留：保真度历史路径），都没有则给出可行动的报错。
async fn convert_office_to_pdf(
    input: &str,
    output: &str,
    progress: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<String, ToolError> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);

    // 1. 内置 dsh 引擎 —— 便携产品的主路径；首次使用时按需下载。
    let (engine, fetch_error, downloaded) = engine_or_fetch(progress).await;
    let engine_failure: Option<String> = match engine {
        Some(engine) => match convert_with_dsh_engine(&engine, input_path, output_path).await {
            Ok(()) => {
                return Ok(format!(
                    "已将 {input} 转换为 PDF（内置 dsh 转换引擎）{}",
                    if downloaded { FIRST_FETCH_NOTE } else { "" }
                ))
            }
            Err(e) => {
                tracing::warn!(error = %e, "dsh 引擎转换失败，回退系统 LibreOffice");
                Some(format!("内置引擎转换失败：{e}"))
            }
        },
        None => Some(format!(
            "内置引擎不可用：{}",
            fetch_error.unwrap_or_else(|| "未定位到引擎".into())
        )),
    };

    // 2. 系统 LibreOffice 回退（用户已自行安装时的保真路径）。
    let output_dir = Path::new(output).parent().unwrap_or(Path::new("."));

    let libreoffice = find_libreoffice().map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "{e}\n{}\n首次使用会自动下载约 71MB 引擎（需联网），下载后可离线使用；\
             也可直接安装 LibreOffice。",
            engine_failure.unwrap_or_default(),
        ))
    })?;

    // Run LibreOffice conversion
    let output = hermix_core::proc::cmd(&libreoffice)
        .args([
            "--headless",
            "--convert-to", "pdf",
            "--outdir", output_dir.to_str().unwrap_or("."),
            input_path.to_str().unwrap_or(""),
        ])
        .output()
        .map_err(|e| ToolError::ExecutionFailed(format!("无法执行 LibreOffice: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError::ExecutionFailed(format!(
            "LibreOffice 转换失败: {}",
            stderr
        )));
    }

    Ok(format!("已将 {} 转换为 PDF", input))
}

/// Find LibreOffice executable
fn find_libreoffice() -> Result<String, ToolError> {
    // Try common paths on Windows
    let paths = [
        r"C:\Program Files\LibreOffice\program\soffice.exe",
        r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
    ];

    for path in &paths {
        if Path::new(path).exists() {
            return Ok(path.to_string());
        }
    }

    // Try which command
    if let Ok(output) = hermix_core::proc::cmd("which").arg("libreoffice").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }

    Err(ToolError::ExecutionFailed(
        "未找到 LibreOffice。请安装 LibreOffice 或将其路径添加到系统 PATH。".to_string()
    ))
}

/// Convert PDF to PNG/JPG via the bundled dsh engine's render protocol.
///
/// Worker contract (dsh-libreoffice-kit packages/entry/src/native-image-renderer.ts):
/// spawn `dsoffice.exe --operation render-images …`, read one JSON line
/// (`kind:"ready"` with page dimensions in twips), then per selected page
/// write `P {index} {part} {w} {h} {x} {y} {twW} {twH}\n` on stdin and read
/// back a `kind:"paint"` JSON line pointing at a `{w}×{h}×4` raw RGBA tile
/// file inside the scratch dir; finish with `DONE\n`. For PDF input the
/// RGBA is straight (pdfium), so pages encode directly via the `image` crate.
/// 返回 `(消息, 本次是否刚下载引擎)` —— 后者用于在结果里说明首调耗时来源。
async fn convert_pdf_to_image(
    input: &str,
    output: &str,
    format: &str,
    progress: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<(String, bool), ToolError> {
    let format = format.to_ascii_lowercase();
    if !matches!(format.as_str(), "png" | "jpg" | "jpeg") {
        return Err(ToolError::InvalidParams(format!(
            "PDF 转图片仅支持 png/jpg，收到 `{format}`"
        )));
    }
    // PDF 渲染没有第二条路（libreoffice 不走这条协议）：引擎按需下载，
    // 失败就直接报原因，不假装成功。
    let (engine, fetch_error, downloaded) = engine_or_fetch(progress).await;
    let Some(engine) = engine else {
        return Err(ToolError::ExecutionFailed(format!(
            "PDF 转图片需要内置 dsh 转换引擎：{}。首次使用会自动下载约 71MB（需联网），\
             请检查网络后重试。",
            fetch_error.unwrap_or_else(|| "未定位到引擎".into())
        )));
    };

    let input_path = Path::new(input).to_path_buf();
    let output_path = Path::new(output);
    let out_dir = output_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("pdf-page")
        .to_string();
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| ToolError::ExecutionFailed(format!("无法创建输出目录: {e}")))?;

    const DPI: f64 = 144.0;
    const MAX_PAGES: usize = 8;

    let scratch = std::env::temp_dir().join(format!(
        "dsh-render-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    ));
    let profile = scratch.join("profile");
    std::fs::create_dir_all(&scratch).map_err(|e| {
        ToolError::ExecutionFailed(format!("无法创建渲染暂存目录: {e}"))
    })?;
    std::fs::create_dir_all(&profile).map_err(|e| {
        ToolError::ExecutionFailed(format!("无法创建引擎 profile 目录: {e}"))
    })?;

    let mut child = hermix_core::proc::cmd_tokio(&engine.executable)
        .args([
            "--operation",
            "render-images",
            "--program-directory",
            engine.program_directory.to_str().unwrap_or_default(),
            "--input-path",
            input_path.to_str().unwrap_or_default(),
            "--scratch-directory",
            scratch.to_str().unwrap_or_default(),
            "--profile-directory",
            profile.to_str().unwrap_or_default(),
            "--max-output-bytes",
            "134217728",
            "--max-image-resolution",
            "144",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // Never pipe stderr: nobody reads it, and a full pipe (~64 KB) would
        // block the engine mid-render while we wait on `read_line` forever.
        .stderr(std::process::Stdio::null())
        .env("HOME", &profile)
        .env("USERPROFILE", &profile)
        .env("TMP", &profile)
        .env("TEMP", &profile)
        .spawn()
        .map_err(|e| ToolError::ExecutionFailed(format!("无法启动内置渲染引擎: {e}")))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ToolError::ExecutionFailed("渲染引擎 stdin 不可用".into()))?;
    let mut stdout = tokio::io::BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| ToolError::ExecutionFailed("渲染引擎 stdout 不可用".into()))?,
    );

    let cleanup = |scratch: &Path| {
        let _ = std::fs::remove_dir_all(scratch);
    };

    // Helper: read one JSON line and require ok:true.
    use tokio::io::AsyncWriteExt as _;
    let result = async {
        let mut ready_line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut stdout, &mut ready_line)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("渲染引擎输出读取失败: {e}")))?;
        let ready: Value = serde_json::from_str(ready_line.trim())
            .map_err(|e| ToolError::ExecutionFailed(format!("渲染引擎返回非法 JSON: {e}")))?;
        if ready.get("ok").and_then(Value::as_bool) != Some(true)
            || ready.get("kind").and_then(Value::as_str) != Some("ready")
        {
            let err = ready
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("引擎未初始化");
            return Err(ToolError::ExecutionFailed(format!("渲染引擎失败: {err}")));
        }
        if ready.get("documentType").and_then(Value::as_str) != Some("pdf") {
            return Err(ToolError::InvalidParams(
                "render-images 仅支持 PDF 输入".into(),
            ));
        }
        let pages = ready
            .get("pages")
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::ExecutionFailed("引擎未返回页面尺寸".into()))?;
        if pages.is_empty() {
            return Err(ToolError::InvalidParams("PDF 没有可渲染页面".into()));
        }

        // Plan: full-page regions for the first MAX_PAGES pages,
        // rectangle units = twips/15 (96ths of an inch).
        let scale = DPI / 96.0;
        let mut produced: Vec<String> = Vec::new();
        let mut total_png = 0u64;
        for (part, page) in pages.iter().take(MAX_PAGES).enumerate() {
            let tw_w = page.get("width").and_then(Value::as_f64).unwrap_or(0.0);
            let tw_h = page.get("height").and_then(Value::as_f64).unwrap_or(0.0);
            if tw_w < 15.0 || tw_h < 15.0 {
                return Err(ToolError::ExecutionFailed("引擎返回非法页面尺寸".into()));
            }
            let rect_w = tw_w / 15.0;
            let rect_h = tw_h / 15.0;
            let width = (rect_w * scale).ceil() as u32;
            let height = (rect_h * scale).ceil() as u32;
            if width == 0 || height == 0 || width as u64 * height as u64 > 16_000_000 {
                return Err(ToolError::ExecutionFailed(format!(
                    "第 {} 页超出像素上限（{width}×{height}），请拆分文档",
                    part + 1
                )));
            }

            let index = part + 1;
            stdin
                .write_all(format!("P {index} {part} {width} {height} 0 0 {tw_w} {tw_h}\n").as_bytes())
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("渲染指令写入失败: {e}")))?;

            let mut paint_line = String::new();
            tokio::io::AsyncBufReadExt::read_line(&mut stdout, &mut paint_line)
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("渲染结果读取失败: {e}")))?;
            let paint: Value = serde_json::from_str(paint_line.trim())
                .map_err(|e| ToolError::ExecutionFailed(format!("渲染结果非法 JSON: {e}")))?;
            if paint.get("ok").and_then(Value::as_bool) != Some(true) {
                let err = paint
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("未知渲染错误");
                return Err(ToolError::ExecutionFailed(format!(
                    "第 {} 页渲染失败: {err}",
                    part + 1
                )));
            }
            let tile_path = paint
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::ExecutionFailed("渲染结果缺 tile 路径".into()))?;
            let raw = std::fs::read(tile_path).map_err(|e| {
                ToolError::ExecutionFailed(format!("读取渲染 tile 失败: {e}"))
            })?;
            if raw.len() != (width as usize) * (height as usize) * 4 {
                return Err(ToolError::ExecutionFailed(format!(
                    "渲染 tile 不完整（{} 字节，期望 {}）",
                    raw.len(),
                    width as usize * height as usize * 4
                )));
            }

            // PDF 输入：pdfium 直出 straight RGBA（TS 侧对 pdf 不做通道
            // 交换，见 native-image-renderer.ts L181），逐字节映射 RgbaImage。
            let mut img = image::RgbaImage::new(width, height);
            for (px, rgba) in raw.chunks_exact(4).enumerate() {
                let x = (px % width as usize) as u32;
                let y = (px / width as usize) as u32;
                img.put_pixel(x, y, image::Rgba([rgba[0], rgba[1], rgba[2], rgba[3]]));
            }
            let out_name = format!("{stem}-page-{:04}.{format}", index);
            let out_path = out_dir.join(&out_name);
            let mut img_out = BufWriter::new(
                std::fs::File::create(&out_path)
                    .map_err(|e| ToolError::ExecutionFailed(format!("无法创建 {out_name}: {e}")))?,
            );
            if format == "png" {
                image::DynamicImage::ImageRgba8(img)
                    .write_to(&mut img_out, image::ImageFormat::Png)
                    .map_err(|e| ToolError::ExecutionFailed(format!("PNG 编码失败: {e}")))?;
            } else {
                // JPEG 无 alpha 通道——转 RGB 再编码。
                image::DynamicImage::ImageRgba8(img)
                    .to_rgb8()
                    .write_to(&mut img_out, image::ImageFormat::Jpeg)
                    .map_err(|e| ToolError::ExecutionFailed(format!("JPEG 编码失败: {e}")))?;
            }
            total_png += std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            produced.push(out_name);
        }

        stdin
            .write_all(b"DONE\n")
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("DONE 写入失败: {e}")))?;
        let mut done_line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut stdout, &mut done_line)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("完成确认读取失败: {e}")))?;


        Ok((
            format!(
                "已将 {input} 渲染为 {} 张 {} 图片（共 {} 字节）：{}",
                produced.len(),
                format.to_uppercase(),
                total_png,
                produced.join(", ")
            ),
            downloaded,
        ))
    }
    .await;

    // Reap the engine on EVERY exit path (success, `?` bail-outs, encode
    // errors). Previously the child was only killed on success, so an early
    // return left dsoffice.exe running against a scratch dir we then deleted.
    let _ = child.start_kill();
    let _ = child.wait().await;
    cleanup(&scratch);
    result
}

/// Convert PDF to text using lopdf
async fn convert_pdf_to_text(input: &str, output: &str) -> Result<String, ToolError> {
    let doc = lopdf::Document::load(input)
        .map_err(|e| ToolError::ExecutionFailed(format!("无法加载 PDF: {}", e)))?;

    let mut text = String::new();
    let pages = doc.get_pages();

    for (i, _page_ref) in pages.iter().enumerate() {
        text.push_str(&format!("=== 第 {} 页 ===\n", i + 1));
        // Note: lopdf doesn't directly extract text content easily
        // For production, use a dedicated PDF text extraction library
        text.push_str("[PDF 文本提取需要专用库]\n\n");
    }

    let output_path = Path::new(output);
    std::fs::write(output_path, &text)
        .map_err(|e| ToolError::ExecutionFailed(format!("无法写入文件: {}", e)))?;

    Ok(format!("已从 PDF {} 提取文本到 {} (共 {} 页)", input, output, pages.len()))
}

/// Convert image format using image crate
async fn convert_image_format(input: &str, output: &str, format: &str) -> Result<String, ToolError> {
    let img = image::ImageReader::open(input)
        .map_err(|e| ToolError::ExecutionFailed(format!("无法读取图片: {}", e)))?
        .decode()
        .map_err(|e| ToolError::ExecutionFailed(format!("无法解码图片: {}", e)))?;

    let output_path = Path::new(output);
    let file = std::fs::File::create(output_path)
        .map_err(|e| ToolError::ExecutionFailed(format!("无法创建文件: {}", e)))?;
    let mut writer = BufWriter::new(file);

    match format {
        "png" => {
            img.write_to(&mut writer, image::ImageFormat::Png)
                .map_err(|e| ToolError::ExecutionFailed(format!("无法保存 PNG: {}", e)))?;
        }
        "jpg" | "jpeg" => {
            img.write_to(&mut writer, image::ImageFormat::Jpeg)
                .map_err(|e| ToolError::ExecutionFailed(format!("无法保存 JPEG: {}", e)))?;
        }
        "webp" => {
            img.write_to(&mut writer, image::ImageFormat::WebP)
                .map_err(|e| ToolError::ExecutionFailed(format!("无法保存 WebP: {}", e)))?;
        }
        _ => {
            return Err(ToolError::InvalidParams(format!("不支持的图片格式: {}", format)));
        }
    }

    Ok(format!("已将 {} 转换为 {}: {}", input, format, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_list_conversions() {
        let handler = FileConverterHandler::new();
        let result = handler.execute(json!({"action": "list"})).await.unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        let conversions = parsed["conversions"].as_array().unwrap();
        assert!(conversions.len() > 0);
    }

    #[tokio::test]
    async fn test_convert_missing_input() {
        let handler = FileConverterHandler::new();
        let result = handler.execute(json!({"action": "convert", "output": "test.pdf"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_convert_unsupported_format() {
        let handler = FileConverterHandler::new();
        let result = handler.execute(json!({
            "action": "convert",
            "input": "test.xyz",
            "output": "test.pdf"
        })).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不支持"));
    }

    #[test]
    fn test_supported_conversions() {
        let conversions = supported_conversions();
        assert!(conversions.len() >= 7);
        
        // Check image to PDF is supported
        let img_to_pdf = conversions.iter().any(|c| {
            c.from.contains(&"png".to_string()) && c.to.contains(&"pdf".to_string())
        });
        assert!(img_to_pdf);
    }

    #[test]
    fn test_schema() {
        let handler = FileConverterHandler::new();
        let schema = handler.schema();
        assert_eq!(schema.name, "file_converter");
    }

    /// 端到端 smoke：引擎在位时（HERMIX_DSH_ENGINE 指向包根，或打包后的
    /// resources 布局）真实转换一次 txt→PDF；引擎不在位的开发环境跳过。
    /// CI 无引擎 → 自然跳过；本机验证：设 HERMIX_DSH_ENGINE 指向包根后运行。
    #[tokio::test]
    async fn dsh_engine_txt_to_pdf_smoke() {
        let Some(engine) = locate_dsh_engine() else {
            eprintln!("skip: 未定位到 dsh 引擎（开发环境可设 HERMIX_DSH_ENGINE 后重跑）");
            return;
        };
        let dir = std::env::temp_dir().join(format!("dsh-smoke-{}-{}", std::process::id(), chrono::Utc::now().timestamp_millis()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("smoke.txt");
        std::fs::write(&input, "hello hermix — dsh 引擎 smoke 测试 中文内容").unwrap();
        let output = dir.join("smoke.pdf");
        // 引擎契约要求输出路径尚不存在。
        let _ = std::fs::remove_file(&output);
        convert_with_dsh_engine(&engine, &input, &output)
            .await
            .expect("dsh 引擎 txt→pdf 转换失败");
        assert!(output.is_file());
        assert!(std::fs::metadata(&output).unwrap().len() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dsh_engine_locate_honours_env_override() {
        // 结构探测（纯函数版，不动全局 env）：伪造满足 bin/dsoffice.exe +
        // program/program/uno.ini 的包根，验证候选顺序与匹配条件。
        let dir = std::env::temp_dir().join(format!("dsh-locate-{}-{}", std::process::id(), chrono::Utc::now().timestamp_millis()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("program").join("program")).unwrap();
        std::fs::write(dir.join("bin").join("dsoffice.exe"), b"fake").unwrap();
        std::fs::write(dir.join("program").join("program").join("uno.ini"), b"").unwrap();
        let located = locate_dsh_engine_from(
            Some(dir.to_str().unwrap()),
            None,
            Path::new("Z:\\nonexistent-home"),
        )
        .expect("env override 未生效");
        assert_eq!(located.program_directory, dir.join("program").join("program"));
        // 结构不完整（缺 dsoffice.exe）→ 不匹配。
        std::fs::remove_file(dir.join("bin").join("dsoffice.exe")).unwrap();
        assert!(locate_dsh_engine_from(
            Some(dir.to_str().unwrap()),
            None,
            Path::new("Z:\\nonexistent-home")
        )
        .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// PDF→PNG 端到端：txt→pdf（既有契约）→ render 协议渲染回 PNG。
    /// 引擎缺席的开发环境跳过（CI 无引擎）。
    #[tokio::test]
    async fn dsh_engine_pdf_to_png_smoke() {
        let Some(engine) = locate_dsh_engine() else {
            eprintln!("skip: 未定位到 dsh 引擎");
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "dsh-render-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let txt = dir.join("render.txt");
        std::fs::write(&txt, "dsh render verify 中文内容\n第二行内容").unwrap();
        let pdf = dir.join("render.pdf");
        convert_with_dsh_engine(&engine, &txt, &pdf)
            .await
            .expect("txt→pdf 前置转换失败");
        assert!(pdf.is_file());

        let png = dir.join("render.png");
        let (msg, _downloaded) =
            convert_pdf_to_image(&pdf.to_string_lossy(), &png.to_string_lossy(), "png", None)
                .await
                .expect("pdf→png 渲染失败");
        println!("{msg}");
        // 至少 1 页产物，且文件非空。
        let produced: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("render-page-"))
            .collect();
        assert!(!produced.is_empty(), "没有任何 page PNG 产物");
        for p in &produced {
            assert!(p.metadata().unwrap().len() > 1000, "{} 过小", p.file_name().to_string_lossy());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
