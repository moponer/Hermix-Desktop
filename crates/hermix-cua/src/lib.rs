//! # hermix-cua
//!
//! Self-hosted **Computer-Use Automation** primitives for Hermix Agent.
//! Targets Windows 10/11 x64. Zero cloud / zero third-party runtime
//! dependency (no `browser-use`, no `OpenAI CUA`, no Playwright).
//!
//! ## Scope (Phase 1 / PoC)
//!
//! - **Screen capture** via `BitBlt` against the desktop window DC
//!   (Phase 2 will swap this for `IDXGIOutputDuplication` once the
//!   DXGI swap chain path is validated; the public API is stable).
//! - **Mouse + keyboard** via `SendInput` (`INPUT` / `MOUSEINPUT` /
//!   `KEYBDINPUT`).
//! - **Safety layer** — blacklisted key chords, rate limiting, and
//!   an approval-mode flag surfaced via [`safety::ApprovalMode`].
//! - **Trace recorder** — JSONL audit log every action lands in.
//!
//! ## Out of scope (Phase 2+)
//!
//! - **Element recognition** — UIAutomation (`IUIAutomation`). Stub
//!   in [`element`]; real implementation lands in Phase 2 alongside
//!   the browser CDP client.
//! - **OCR** — `Windows.Media.Ocr` (WinRT). Stub in [`ocr`].
//! - **Browser automation** — self-hosted minimal CDP client
//!   (`tokio_tungstenite` + `serde_json`). Stub in [`browser`].
//!
//! ## Calling convention
//!
//! Every public function is **synchronous and blocking**. Callers
//! inside the agent loop must wrap with
//! `tokio::task::spawn_blocking` (or a thread+channel timeout as
//! `computer_use.rs` does today) — none of these primitives await
//! the OS input pipeline, and the async wrappers in
//! `hermix-tools/src/tools/cua.rs` handle that.

#![cfg_attr(
    not(windows),
    doc = "**This crate currently builds only on `windows` targets.**"
)]

pub mod error;
pub mod safety;
pub mod trace;

#[cfg(windows)]
pub mod action;
#[cfg(windows)]
pub mod capture;

// Phase 2+ stubs — kept non-`cfg(windows)` so consumer code can
// `use hermix_cua::element::...` from any platform without a
// missing-import failure on macOS / Linux CI.
pub mod element;
pub mod ocr;
pub mod browser;

// Phase 2-C: UIAutomation COM client (IUIAutomation element tree).
// This adds another ~170 KB of generated windows crate code but
// provides the full UIA element tree (buttons, textboxes, etc.)
// beyond what Win32 EnumWindows can give.
#[cfg(windows)]
pub mod uia;

pub use error::CuaError;
pub use safety::{ApprovalMode, RiskLevel, SafetyGuard};
pub use trace::{TraceEvent, TraceKind, TraceRecorder};

/// Result alias used across the crate.
pub type CuaResult<T> = std::result::Result<T, CuaError>;