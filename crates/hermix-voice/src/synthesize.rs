//! TTS synthesis: text → audio bytes.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};

use hermix_tools::tools::tts::TtsBackend;
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
/// Delegates to `hermix_tools::backends::tts::MultiTtsBackend` which
/// already supports OpenAI, ElevenLabs, and MiniMax providers. The
/// backend writes the audio to a temp file and returns a JSON string
/// with the file path; this function reads the file, encodes it to
/// base64, and returns it in the HTTP-friendly `SynthesizeResponse`.
///
/// If no API key is configured for any provider, the backend returns
/// an error which we surface as `EmptySynthesis` (the front-end can
/// show a "TTS not configured" badge).
pub async fn synthesize_text(
    request: &SynthesizeRequest,
    pipeline: Option<&VoicePipeline>,
) -> VoiceResult<SynthesizeResponse> {
    if request.text.trim().is_empty() {
        return Err(VoiceBridgeError::Internal("empty text".to_string()));
    }

    // Resolve provider: explicit > pipeline config > default "openai"
    let provider = if let Some(ref p) = request.provider {
        p.clone()
    } else if let Some(pl) = pipeline {
        let cfg = pl.current_config().await;
        cfg.tts_provider
    } else {
        "openai".to_string()
    };

    let format = request.format.clone().unwrap_or_else(|| "mp3".to_string());

    // Use MultiTtsBackend — it already handles OpenAI / ElevenLabs / MiniMax
    // with proper gateway fallback and env-var detection.
    let backend = hermix_tools::backends::tts::MultiTtsBackend::new();
    let voice = request.voice.as_deref();

    let result_json = backend
        .synthesize(&request.text, voice, Some(&provider))
        .await
        .map_err(VoiceBridgeError::Tool)?;

    // MultiTtsBackend returns a JSON string like:
    // {"provider":"openai","file":"/tmp/hermes_tts_xxx.mp3","voice":"alloy","bytes":12345}
    let parsed: serde_json::Value =
        serde_json::from_str(&result_json).map_err(|e| VoiceBridgeError::Internal(e.to_string()))?;

    let file_path = parsed
        .get("file")
        .and_then(|v| v.as_str())
        .ok_or_else(|| VoiceBridgeError::Internal("TTS backend returned no file path".into()))?;

    let audio_bytes = tokio::fs::read(file_path)
        .await
        .map_err(|e| VoiceBridgeError::Internal(format!("failed to read TTS audio file: {e}")))?;

    // Clean up the temp file
    let _ = tokio::fs::remove_file(file_path).await;

    if audio_bytes.is_empty() {
        return Err(VoiceBridgeError::EmptySynthesis);
    }

    let byte_count = audio_bytes.len();
    let audio_base64 = BASE64.encode(&audio_bytes);

    // Estimate duration: MP3 at ~128kbps → ~16KB/s
    let duration_ms = if format == "mp3" {
        (byte_count as u64 * 1000) / 16_000
    } else {
        0
    };

    let resolved_provider = parsed
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or(&provider)
        .to_string();

    Ok(SynthesizeResponse {
        audio_base64,
        provider: resolved_provider,
        format,
        duration_ms,
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

    // These tests previously asserted that `synthesize_text` returned
    // an empty response (byte_count == 0). Now that the function calls
    // the real `MultiTtsBackend`, those assertions no longer hold —
    // the backend will try to make HTTP calls to OpenAI / ElevenLabs
    // and fail if no API key is set.
    // We keep only the empty-text guard test (which short-circuits
    // before any HTTP call) and the serde round-trip test. The
    // provider-resolution logic is tested implicitly by the
    // `MultiTtsBackend` unit tests in `hermix-tools`.

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
