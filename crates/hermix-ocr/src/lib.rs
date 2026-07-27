//! hermix-ocr: local OCR engine wrapper around `oar-ocr` (PP-OCRv5 mobile).
//!
//! Exposes a provider-agnostic [`OcrEngine`] trait plus the
//! [`OarOcrEngine`] implementation backed by `oar-ocr`. The agent layer
//! (hermix-tools) wires this into the `read_image` / `read_pdf` tools and
//! falls back to a sub-agent's `read_image` (LLM multimodal) when a key
//! field's confidence is below the configured threshold.
//!
//! Design note (2026-07-26): this crate deliberately does NOT depend on
//! hermix-core / hermix-config. Model paths are passed in via [`OcrConfig`]
//! by the caller (which resolves `hermix_home()` / config). The
//! [`OcrConfig::default`] model dir is only a fallback for standalone use.

pub mod ocr;

pub use ocr::{OarOcrEngine, OcrConfig, OcrEngine, OcrError, OcrWord};
