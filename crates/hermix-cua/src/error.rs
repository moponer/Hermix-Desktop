//! Error types for `hermix-cua`.
//!
//! Intentionally **self-contained** (does not depend on
//! `hermix_core::ToolError`) so the crate stays a leaf. The
//! `hermix-tools` wrapper translates `CuaError` into the
//! canonical `ToolError` at the public `ToolHandler` boundary.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CuaError {
    /// `DXGI` subsystem failure (Phase 2 — currently unused because
    /// Phase 1 captures via BitBlt; surfaced by the DXGI backend
    /// once it lands).
    #[error("DXGI: {0}")]
    Dxgi(String),

    /// `SendInput` failed or returned an unexpected value. The
    /// `String` payload is the OS error string (`HRESULT` formatted
    /// via `Display`).
    #[error("SendInput: {0}")]
    SendInput(String),

    /// Win32 GDI / windowing API failure (BitBlt, GetDC, etc.).
    #[error("Win32 GDI: {0}")]
    Gdi(String),

    /// UIAutomation failure (Phase 2 stub).
    #[error("UIA: {0}")]
    Uia(String),

    /// OCR failure (Phase 2 stub).
    #[error("OCR: {0}")]
    Ocr(String),

    /// CDP failure (Phase 2 stub).
    #[error("CDP: {0}")]
    Cdp(String),

    /// PNG encoding failed.
    #[error("PNG encode: {0}")]
    Png(String),

    /// Safety policy rejected the action (blacklisted chord,
    /// rate-limited, etc.).
    #[error("denied by safety: {0}")]
    Denied(String),

    /// The action exceeded its budgeted wall-clock time.
    #[error("action timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// Caller-supplied parameters were rejected (bad coordinates,
    /// zero-length text, etc.).
    #[error("invalid params: {0}")]
    InvalidParams(String),

    /// Target resource (window, process, monitor) was not found.
    #[error("not found: {0}")]
    NotFound(String),
}

impl CuaError {
    /// Short string identifier (snake_case) suitable for the wire
    /// payload returned by the `hermix-tools` wrappers. Mirrors
    /// `ToolError`'s string surface so the desktop UI can render a
    /// stable error code regardless of which side did the wrapping.
    pub fn kind(&self) -> &'static str {
        match self {
            CuaError::Dxgi(_) => "dxgi",
            CuaError::SendInput(_) => "send_input",
            CuaError::Gdi(_) => "gdi",
            CuaError::Uia(_) => "uia",
            CuaError::Ocr(_) => "ocr",
            CuaError::Cdp(_) => "cdp",
            CuaError::Png(_) => "png",
            CuaError::Denied(_) => "denied",
            CuaError::Timeout(_) => "timeout",
            CuaError::InvalidParams(_) => "invalid_params",
            CuaError::NotFound(_) => "not_found",
        }
    }
}