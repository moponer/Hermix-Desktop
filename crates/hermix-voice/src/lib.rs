//! L3: Voice bridge — HTTP-friendly STT/TTS endpoint layer.
//!
//! The voice pipeline itself lives in `hermix_tools::voice_mode`
//! (VAD, AudioSource trait, Whisper/Deepgram/Custom STT) and
//! `hermix_tools::tts_streaming` (ElevenLabs / MiniMax TTS providers,
//! sentence buffer, sanitizer). This crate adds the thin HTTP-friendly
//! layer on top:
//!
//! - [`transcribe::transcribe_pcm`] takes raw f32 PCM samples (e.g. decoded from
//!   a webm blob on the front-end) and returns the transcribed text.
//!
//! ## Why a separate crate?
//!
//! - Decouples the HTTP transport from the tool registry. hermix-server
//!   is large; the voice bridge is small and grows independently.
//! - Front-ends (web, CLI, gateway) can depend on this without pulling
//!   the whole tool registry.
//! - Keeps the "no external resources" policy clean: this crate has no
//!   `reqwest` calls, no model downloads, no global side effects.
//!
//! ## Design decisions
//!
//! This crate is a thin HTTP layer — it does **not**:
//! - Make external HTTP calls directly (delegates to hermix-tools)
//! - Download ONNX/Whisper models (handled by upstream providers)
//! - Use OS-native TTS APIs (e.g., Windows SAPI COM)
//!
//! All actual network calls (Whisper STT, ElevenLabs TTS, etc.) go
//! through `hermix_tools::voice_mode` and `hermix_tools::tts_streaming`.
//! This keeps the voice bridge small and dependency-free.

// Submodules
mod error;
mod transcribe;

// Re-export error types
pub use error::{VoiceBridgeError, VoiceResult};

// Re-export transcribe types and functions
pub use transcribe::{TranscribeRequest, TranscribeResponse, transcribe_pcm, transcribe_with_pipeline};

// Re-export from hermix-tools
pub use hermix_tools::tools::voice_mode::{
    set_voice_mode_enabled, voice_mode_enabled, AudioSink, AudioSource, SttConfig, SttProviderType,
    VadConfig, VoiceModeHandler, VoicePipeline, VoicePipelineConfig,
};
pub use hermix_tools::tts_streaming::{
    PcmSink, SentenceSink, TextDelta, TtsStreamingPipeline, VecSentenceSink,
};

/// Convenience: build a default pipeline. Used by hermix-server's
/// HTTP handler when no per-session pipeline is registered.
pub fn default_pipeline() -> VoicePipeline {
    VoicePipeline::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_includes_cause() {
        let e = VoiceBridgeError::EmptyAudio;
        assert!(format!("{e}").contains("empty audio"));
        let e = VoiceBridgeError::InvalidSampleRate(0);
        assert!(format!("{e}").contains("0"));
    }

    #[test]
    fn default_pipeline_uses_whisper() {
        let p = default_pipeline();
        let cfg = futures::executor::block_on(p.current_config());
        assert_eq!(cfg.stt.provider, SttProviderType::Whisper);
    }

    #[test]
    fn reexports_are_usable() {
        let _ = voice_mode_enabled();
        let _ = set_voice_mode_enabled(false);
        let _ = VoicePipeline::default();
        let _p: SttProviderType = SttProviderType::Whisper;
    }
}
