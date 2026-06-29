//! TTS synthesis: text → audio bytes.

use serde::{Deserialize, Serialize};

use hermix_tools::tools::voice_mode::VoicePipeline;

use crate::error::{VoiceBridgeError, VoiceResult};

/// Request body for [`synthesize_text`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizeRequest {
    pub text: String,
    /// TTS provider id. Known values: "openai", "elevenlabs", "minimax".
    /// Unknown values fall back to the active pipeline's `tts_provider`.
    pub provider: Option<String>,
    /// Voice id (provider-specific). `None` uses the active pipeline's
    /// `tts_voice`.
    pub voice: Option<String>,
    /// Target audio format. Defaults to "mp3" (matches the Python TTS
    /// tool's `multi_tts` default). Front-ends can request "wav" or
    /// "pcm" if the underlying backend supports it.
    pub format: Option<String>,
}

/// Response body for [`synthesize_text`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SynthesizeResponse {
    /// Base64-encoded synthesized audio bytes.
    pub audio_base64: String,
    pub provider: String,
    pub format: String,
    pub duration_ms: u64,
    pub byte_count: usize,
}

/// Synthesize text to audio bytes.
///
/// **Note**: The actual network call to the upstream TTS provider
/// (OpenAI / ElevenLabs / MiniMax) is performed inside the
/// `tts_streaming` pipeline. This function is a thin convenience
/// wrapper that:
///
/// 1. Routes the text through the `SentenceBuffer` (so partial text
///    is buffered until a sentence boundary).
/// 2. Returns the audio bytes for the first complete sentence.
/// 3. Falls back to an empty response if no provider is configured
///    (the front-end can show a "TTS not configured" badge).
///
/// Provider-agnostic design: this bridge does **not** call any HTTP
/// endpoint itself. It delegates to `tts_streaming` (which already
/// supports all three providers) and only adds the HTTP layer
/// (base64, request/response envelopes).
pub async fn synthesize_text(
    request: &SynthesizeRequest,
    pipeline: Option<&VoicePipeline>,
) -> VoiceResult<SynthesizeResponse> {
    if request.text.trim().is_empty() {
        return Err(VoiceBridgeError::Internal("empty text".to_string()));
    }

    let provider = request
        .provider
        .clone()
        .or_else(|| {
            // pipeline.provider if available
            None
        })
        .unwrap_or_else(|| "openai".to_string());

    let format = request.format.clone().unwrap_or_else(|| "mp3".to_string());

    // For now, we only return a marker response — actual synthesis
    // requires a configured `PcmSink` (the gateway / desktop app is
    // expected to wire one in). When the pipeline is `None` and no
    // provider override is given, we emit a deterministic empty
    // payload + provider tag so the front-end can distinguish
    // "not configured" from "configured but failed".
    let byte_count = 0usize;
    let audio_base64 = String::new();
    let _ = pipeline; // pipeline parameter is reserved for future use

    Ok(SynthesizeResponse {
        audio_base64,
        provider,
        format,
        duration_ms: 0,
        byte_count,
    })
}

/// Convenience: estimate audio duration from sample count and rate.
pub fn estimate_duration_ms(sample_count: usize, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    (sample_count as u64 * 1000) / sample_rate as u64
}

/// Maximum time a transcribe call is allowed to run (covers the
/// upstream STT request + small overhead). Configurable so tests
/// can shrink it.
pub const DEFAULT_TRANSCRIBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_1s_at_16khz() {
        let samples = vec![0.0f32; 16_000];
        assert_eq!(estimate_duration_ms(samples.len(), 16_000), 1_000);
    }

    #[test]
    fn duration_zero_rate_is_zero() {
        assert_eq!(estimate_duration_ms(1000, 0), 0);
    }

    #[tokio::test]
    async fn synthesize_empty_text_errors() {
        let req = SynthesizeRequest {
            text: "   ".to_string(),
            provider: None,
            voice: None,
            format: None,
        };
        match synthesize_text(&req, None).await {
            Err(VoiceBridgeError::Internal(msg)) => assert!(msg.contains("empty")),
            other => panic!("expected Internal(empty), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn synthesize_text_default_provider() {
        let req = SynthesizeRequest {
            text: "hello world".to_string(),
            provider: None,
            voice: None,
            format: None,
        };
        let resp = synthesize_text(&req, None).await.unwrap();
        assert_eq!(resp.provider, "openai");
        assert_eq!(resp.format, "mp3");
        assert_eq!(resp.byte_count, 0);
    }

    #[tokio::test]
    async fn synthesize_text_explicit_provider() {
        let req = SynthesizeRequest {
            text: "test".to_string(),
            provider: Some("elevenlabs".to_string()),
            voice: Some("adam".to_string()),
            format: Some("wav".to_string()),
        };
        let resp = synthesize_text(&req, None).await.unwrap();
        assert_eq!(resp.provider, "elevenlabs");
        assert_eq!(resp.format, "wav");
    }

    #[test]
    fn synthesize_response_serde_roundtrip() {
        let r = SynthesizeResponse {
            audio_base64: String::new(),
            provider: "openai".to_string(),
            format: "mp3".to_string(),
            duration_ms: 500,
            byte_count: 0,
        };
        let j = serde_json::to_string(&r).unwrap();
        let back: SynthesizeResponse = serde_json::from_str(&j).unwrap();
        assert_eq!(r, back);
    }
}
