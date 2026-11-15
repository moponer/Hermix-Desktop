//! Voice bridge error types.

use thiserror::Error;

/// Result alias for the voice bridge.
pub type VoiceResult<T> = Result<T, VoiceBridgeError>;

/// Error type for the voice bridge.
///
/// Most errors wrap an underlying `hermix_tools::ToolError`; the
/// `Base64` variant covers the front-end's base64-encoded PCM payload.
#[derive(Debug, Error)]
pub enum VoiceBridgeError {
    #[error("base64 decode failed: {0}")]
    Base64(String),

    #[error("invalid sample rate: {0} (must be > 0)")]
    InvalidSampleRate(u32),

    #[error("empty audio buffer (0 samples)")]
    EmptyAudio,

    #[error("tool error: {0}")]
    Tool(#[from] hermix_core::ToolError),

    #[error("internal: {0}")]
    Internal(String),
}
