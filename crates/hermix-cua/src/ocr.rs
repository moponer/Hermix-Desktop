//! OCR via **Windows.Media.Ocr** (WinRT).
//!
//! Pipeline: PNG bytes → `InMemoryRandomAccessStream` → `BitmapDecoder`
//! → `SoftwareBitmap` → `OcrEngine.RecognizeAsync` → text + lines + words.
//!
//! ## Platform requirements
//!
//! Windows 10/11 with at least one OCR language pack. English (US)
//! is pre-installed; Chinese Simplified needs the Chinese language
//! feature-on-demand.

use crate::element::ElementRect;
use crate::error::CuaError;
use crate::CuaResult;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Complete OCR result with lines, words, and bounding rectangles.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OcrResponse {
    /// All recognised text (lines joined with `\n`).
    pub text: String,
    /// Individual lines with word-level detail.
    pub lines: Vec<OcrLine>,
    /// Average confidence, or `None` (Windows.Media.Ocr does not
    /// expose per-word confidence scores).
    pub avg_confidence: Option<f64>,
}

/// A single line of recognised text.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub words: Vec<OcrWord>,
}

/// A single word with its bounding rectangle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OcrWord {
    pub text: String,
    pub rect: ElementRect,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Recognise text from a PNG-encoded screenshot.
/// /// The PNG bytes are loaded into a WinRT `InMemoryRandomAccessStream`,
/// decoded via `BitmapDecoder`, converted to a `SoftwareBitmap`, and
/// fed to `OcrEngine::RecognizeAsync`.
/// /// # Errors
/// /// - `Ocr` — engine initialisation or recognition failure.
/// - `NotFound` — no OCR engine available for the user's language
/// (e.g. no language pack installed).
pub fn recognize_from_png(png_data: &[u8]) -> CuaResult<OcrResponse> {
    #[cfg(windows)]
    {
        unsafe { recognize_inner(png_data) }
    }
    #[cfg(not(windows))]
    {
        Err(CuaError::Ocr("OCR is Windows-only.".into()))
    }
}

/// Recognise text from raw RGBA pixels (width × height × 4).
/// /// Not yet implemented — prefer the PNG path because the DXGI
/// / BitBlt screenshot tools already produce PNG output.
pub fn recognize_from_rgba(
    _rgba: &[u8],
    _width: u32,
    _height: u32,
) -> CuaResult<OcrResponse> {
    Err(CuaError::Ocr(
        "RGBA-based OCR not yet wired. Use recognize_from_png instead.".into(),
    ))
}

/// Recognise text from a **local image file** (PNG / JPEG / GIF / WEBP / ...).
/// /// The file is read from disk and decoded by `BitmapDecoder`, which
/// auto-detects the container format, then fed to the OCR engine. This
/// is the path-based, no-API-key entry point used for images that already
/// exist on disk (e.g. a folder of scanned documents / licence plates).
/// /// # Errors
/// /// - `Ocr` — file unreadable, decode failure, or recognition failure
/// (e.g. no OCR language pack for the user's profile languages).
pub fn recognize_from_path(path: &str) -> CuaResult<OcrResponse> {
    #[cfg(windows)]
    {
        let bytes = std::fs::read(path)
            .map_err(|e| CuaError::Ocr(format!("read image file '{path}': {e}")))?;
        unsafe { recognize_inner(&bytes) }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(CuaError::Ocr("OCR is Windows-only.".into()))
    }
}

// =========================================================================
// Implementation (Windows-only)
// =========================================================================

#[cfg(windows)]
unsafe fn recognize_inner(png_data: &[u8]) -> CuaResult<OcrResponse> {
    use windows::Storage::Streams::{
        DataWriter, InMemoryRandomAccessStream,
    };
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;

    // 1. Initialise COM for this thread (no-op on subsequent calls).
    let _ = windows::Win32::System::Com::CoInitializeEx(
        None,
        windows::Win32::System::Com::COINIT_MULTITHREADED,
    );

    // 2. Write PNG bytes into an in-memory WinRT stream.
    let stream = InMemoryRandomAccessStream::new()
        .map_err(|e| CuaError::Ocr(format!("create stream: {e}")))?;
    let output_stream = stream
        .GetOutputStreamAt(0)
        .map_err(|e| CuaError::Ocr(format!("get output stream: {e}")))?;
    let writer = DataWriter::CreateDataWriter(&output_stream)
        .map_err(|e| CuaError::Ocr(format!("create writer: {e}")))?;
    writer
        .WriteBytes(png_data)
        .map_err(|e| CuaError::Ocr(format!("write bytes: {e}")))?;
    writer
        .StoreAsync()
        .map_err(|e| CuaError::Ocr(format!("store async: {e}")))? // <-- typo fixed
        .get()
        .map_err(|e| CuaError::Ocr(format!("store wait: {e}")))?;
    stream
        .Seek(0)
        .map_err(|e| CuaError::Ocr(format!("seek: {e}")))?;

    // 3. Decode PNG → SoftwareBitmap.
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .map_err(|e| CuaError::Ocr(format!("create decoder: {e}")))?
        .get()
        .map_err(|e| CuaError::Ocr(format!("decode wait: {e}")))?;
    let software_bitmap = decoder
        .GetSoftwareBitmapAsync()
        .map_err(|e| CuaError::Ocr(format!("get bitmap: {e}")))?
        .get()
        .map_err(|e| CuaError::Ocr(format!("bitmap wait: {e}")))?;

    // 4. Create OCR engine using the user's profile languages.
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| CuaError::Ocr(format!("create engine: {e}")))?;

    // 5. Recognise.
    let result = engine
        .RecognizeAsync(&software_bitmap)
        .map_err(|e| CuaError::Ocr(format!("recognise: {e}")))?
        .get()
        .map_err(|e| CuaError::Ocr(format!("recognise wait: {e}")))?;

    // 6. Extract full text.
    let full_text =
        format!("{}", result.Text().map_err(|e| CuaError::Ocr(format!("get text: {e}")))?);

    // 7. Extract lines and words.
    let lines_vec = result
        .Lines()
        .map_err(|e| CuaError::Ocr(format!("get lines: {e}")))?;
    let line_count = lines_vec
        .Size()
        .map_err(|e| CuaError::Ocr(format!("lines size: {e}")))?;

    let mut lines_out = Vec::with_capacity(line_count as usize);
    for i in 0..line_count {
        let line = lines_vec
            .GetAt(i)
            .map_err(|e| CuaError::Ocr(format!("line[{i}]: {e}")))?;
        let line_text =
            format!("{}", line.Text().map_err(|e| CuaError::Ocr(format!("line[{i}] text: {e}")))?);

        let words_vec = line
            .Words()
            .map_err(|e| CuaError::Ocr(format!("line[{i}] words: {e}")))?;
        let word_count = words_vec
            .Size()
            .map_err(|e| CuaError::Ocr(format!("words[{i}] size: {e}")))?;

        let mut words_out = Vec::with_capacity(word_count as usize);
        for j in 0..word_count {
            let word = words_vec
                .GetAt(j)
                .map_err(|e| CuaError::Ocr(format!("word[{i},{j}]: {e}")))?;
            let word_text = format!(
                "{}",
                word.Text()
                    .map_err(|e| CuaError::Ocr(format!("word[{i},{j}] text: {e}")))?
            );

            // Windows.Media.Ocr returns a Windows.Foundation.Rect
            // with X, Y, Width, Height in physical pixels.
            let ocr_rect = word
                .BoundingRect()
                .map_err(|e| CuaError::Ocr(format!("word[{i},{j}] rect: {e}")))?;

            words_out.push(OcrWord {
                text: word_text,
                rect: ElementRect {
                    x: ocr_rect.X as i32,
                    y: ocr_rect.Y as i32,
                    width: ocr_rect.Width as i32,
                    height: ocr_rect.Height as i32,
                },
            });
        }

        lines_out.push(OcrLine {
            text: line_text,
            words: words_out,
        });
    }

    Ok(OcrResponse {
        text: full_text,
        lines: lines_out,
        avg_confidence: None,
    })
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_types_are_serializable() {
        let word = OcrWord {
            text: "Hello".into(),
            rect: ElementRect::default(),
        };
        let line = OcrLine {
            text: "Hello World".into(),
            words: vec![word],
        };
        let resp = OcrResponse {
            text: "Hello World".into(),
            lines: vec![line],
            avg_confidence: Some(0.95),
        };
        let json = serde_json::to_string(&resp).expect("OcrResponse serializable");
        assert!(json.contains("\"text\":\"Hello World\""));
        assert!(json.contains("\"avg_confidence\":0.95"));
    }

    #[test]
    fn stub_returns_error_not_panic() {
        let err = recognize_from_png(&[]).unwrap_err();
        // On Windows the inner function is called and may produce
        // a different error (e.g. "create stream: ..."); on CI
        // non-Windows targets it always returns "OCR is Windows-only".
        // In both cases it should be an Ocr variant, not a panic.
        assert!(
            format!("{:?}", err).contains("Ocr"),
            "expected Ocr error, got {err:?}"
        );
    }
}

