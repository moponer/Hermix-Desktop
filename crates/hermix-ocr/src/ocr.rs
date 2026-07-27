//! OCR engine abstraction + `oar-ocr` (PP-OCRv5 mobile) implementation.

use oar_ocr::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single recognized text token with its confidence score.
///
/// Bounding-box is intentionally omitted for v1 — the agent only needs the
/// recognized text + confidence to decide rename / fallback. `oar-ocr`
/// also exposes `TextRegion.bounding_box` if geometric layout is needed later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrWord {
    pub text: String,
    pub confidence: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("OCR engine build failed: {0}")]
    EngineBuild(String),
    #[error("OCR prediction failed: {0}")]
    Predict(String),
    #[error("failed to load image '{path}': {reason}")]
    ImageLoad { path: String, reason: String },
    #[error("OCR engine not initialized (model files missing at {0})")]
    NotInitialized(String),
}

/// Provider-agnostic OCR interface. Lets hermix-tools depend on the trait
/// rather than `oar-ocr` directly, so a different engine could be swapped in.
pub trait OcrEngine {
    /// Recognize all text in a local image file. Returns words with
    /// confidence; empty text regions are dropped by the implementation.
    fn recognize_path(&self, path: &str) -> Result<Vec<OcrWord>, OcrError>;
}

/// Configuration for the OCR engine: model file locations + confidence floor.
///
/// All fields are `serde(default)` so this can be embedded in `AgentConfig`
/// without breaking deserialization of older configs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrConfig {
    /// Text detection ONNX model (pp-ocrv5_mobile_det.onnx).
    #[serde(default = "default_det")]
    pub det_model: PathBuf,
    /// Text recognition ONNX model (pp-ocrv5_mobile_rec.onnx).
    #[serde(default = "default_rec")]
    pub rec_model: PathBuf,
    /// Character dictionary (ppocrv5_dict.txt).
    #[serde(default = "default_dict")]
    pub dict_path: PathBuf,
    /// Confidence threshold below which a word is considered low-confidence
    /// (caller decides whether to fall back to sub-agent read_image).
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
}

impl Default for OcrConfig {
    fn default() -> Self {
        let dir = default_model_dir();
        Self {
            det_model: dir.join("pp-ocrv5_mobile_det.onnx"),
            rec_model: dir.join("pp-ocrv5_mobile_rec.onnx"),
            dict_path: dir.join("ppocrv5_dict.txt"),
            confidence_threshold: default_confidence_threshold(),
        }
    }
}

fn default_confidence_threshold() -> f32 {
    0.7
}

fn default_det() -> PathBuf {
    default_model_dir().join("pp-ocrv5_mobile_det.onnx")
}
fn default_rec() -> PathBuf {
    default_model_dir().join("pp-ocrv5_mobile_rec.onnx")
}
fn default_dict() -> PathBuf {
    default_model_dir().join("ppocrv5_dict.txt")
}

/// Fallback model directory when the caller does not supply explicit paths.
/// Uses the OS data-local dir (e.g. %LOCALAPPDATA%/hermix/ocr on Windows).
fn default_model_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "hermix")
        .map(|d| d.data_local_dir().join("ocr"))
        .unwrap_or_else(|| PathBuf::from("ocr_models"))
}

/// `oar-ocr` PP-OCRv5 mobile engine.
pub struct OarOcrEngine {
    engine: OAROCR,
    confidence_threshold: f32,
}

impl OarOcrEngine {
    /// Build the engine from the given config. Model files must exist on disk.
    pub fn new(config: &OcrConfig) -> Result<Self, OcrError> {
        if !Path::new(&config.det_model).exists()
            || !Path::new(&config.rec_model).exists()
            || !Path::new(&config.dict_path).exists()
        {
            return Err(OcrError::NotInitialized(format!(
                "det={} rec={} dict={}",
                config.det_model.display(),
                config.rec_model.display(),
                config.dict_path.display()
            )));
        }
        let engine = OAROCRBuilder::new(
            config.det_model.clone(),
            config.rec_model.clone(),
            config.dict_path.clone(),
        )
        .build()
        .map_err(|e| OcrError::EngineBuild(format!("{e}")))?;
        Ok(Self {
            engine,
            confidence_threshold: config.confidence_threshold,
        })
    }

    /// Confidence floor used by [`Self::recognize_path`] callers to flag
    /// low-confidence words. Exposed so hermix-tools can reuse one threshold.
    pub fn confidence_threshold(&self) -> f32 {
        self.confidence_threshold
    }
}

impl OcrEngine for OarOcrEngine {
    fn recognize_path(&self, path: &str) -> Result<Vec<OcrWord>, OcrError> {
        let img = load_image(path)
            .map_err(|e| OcrError::ImageLoad {
                path: path.to_string(),
                reason: format!("{e}"),
            })?;
        // `predict` returns one OAROCRResult per input image (batch API).
        let results = self
            .engine
            .predict(vec![img])
            .map_err(|e| OcrError::Predict(format!("{e}")))?;
        let result = results
            .first()
            .ok_or_else(|| OcrError::Predict("empty OCR result".into()))?;

        let mut words = Vec::new();
        for region in &result.text_regions {
            if let Some(text) = &region.text {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let confidence = region.confidence.unwrap_or(0.0);
                words.push(OcrWord {
                    text: trimmed.to_string(),
                    confidence,
                });
            }
        }
        Ok(words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_threshold() {
        let cfg = OcrConfig::default();
        assert!((cfg.confidence_threshold - 0.7).abs() < f32::EPSILON);
        assert!(cfg.det_model.to_string_lossy().contains("pp-ocrv5_mobile_det"));
    }

    #[test]
    fn not_initialized_without_models() {
        // Point at a nonexistent dir so the engine refuses to build.
        let cfg = OcrConfig {
            det_model: PathBuf::from("/nonexistent/det.onnx"),
            rec_model: PathBuf::from("/nonexistent/rec.onnx"),
            dict_path: PathBuf::from("/nonexistent/dict.txt"),
            confidence_threshold: 0.7,
        };
        assert!(OarOcrEngine::new(&cfg).is_err());
    }
}
