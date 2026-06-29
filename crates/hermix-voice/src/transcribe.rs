//! STT transcription: PCM audio → text.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};

use hermix_tools::tools::voice_mode::{
    SttProviderType, VoicePipeline, VoicePipelineConfig,
};

use crate::error::{VoiceBridgeError, VoiceResult};

/// Request body for [`transcribe_pcm`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeRequest {
    /// Base64-encoded 16-bit signed little-endian PCM samples,
    /// mono. We accept s16 (browser MediaRecorder default) instead
    /// of f32 to keep the wire format compact.
    pub pcm_base64: String,
    /// Sample rate in Hz (e.g. 16000, 48000).
    pub sample_rate: u32,
    /// Optional STT provider override. `None` uses the active
    /// `VoicePipeline`'s configured provider.
    pub stt_provider: Option<SttProviderType>,
    /// Optional ISO 639-1 language code (e.g. "en", "zh", "ja").
    pub language: Option<String>,
}

impl TranscribeRequest {
    /// Decode the base64 PCM buffer into f32 samples in `[-1.0, 1.0]`.
    pub fn decode_pcm_f32(&self) -> VoiceResult<Vec<f32>> {
        if self.sample_rate == 0 {
            return Err(VoiceBridgeError::InvalidSampleRate(self.sample_rate));
        }
        let bytes = BASE64
            .decode(self.pcm_base64.as_bytes())
            .map_err(|e| VoiceBridgeError::Base64(e.to_string()))?;
        if bytes.is_empty() {
            return Err(VoiceBridgeError::EmptyAudio);
        }
        if bytes.len() % 2 != 0 {
            return Err(VoiceBridgeError::Base64(format!(
                "odd byte count: {} (s16 PCM must be pairs)",
                bytes.len()
            )));
        }
        let samples: Vec<f32> = bytes
            .chunks_exact(2)
            .map(|c| {
                let s = i16::from_le_bytes([c[0], c[1]]);
                (s as f32) / (i16::MAX as f32)
            })
            .collect();
        if samples.is_empty() {
            return Err(VoiceBridgeError::EmptyAudio);
        }
        Ok(samples)
    }
}

/// Response body for [`transcribe_pcm`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscribeResponse {
    pub text: String,
    pub provider: String,
    pub sample_count: usize,
    pub duration_ms: u64,
}

/// Transcribe raw PCM samples using the active `VoicePipeline` config.
///
/// This is the synchronous variant — it constructs a one-shot
/// `VoicePipeline` if `pipeline` is `None`. For long-lived
/// pipelines (e.g. desktop's always-on voice mode), prefer
/// [`transcribe_with_pipeline`] so the configured provider is reused.
pub async fn transcribe_pcm(
    request: &TranscribeRequest,
    pipeline: Option<&VoicePipeline>,
) -> VoiceResult<TranscribeResponse> {
    let samples = request.decode_pcm_f32()?;
    let sample_count = samples.len();
    let duration_ms = (sample_count as u64 * 1000) / request.sample_rate.max(1) as u64;

    let cfg: VoicePipelineConfig = if let Some(p) = pipeline {
        p.current_config().await
    } else {
        let mut cfg = VoicePipelineConfig::default();
        if let Some(provider) = &request.stt_provider {
            cfg.stt.provider = provider.clone();
        }
        if let Some(lang) = &request.language {
            cfg.stt.language = Some(lang.clone());
        }
        cfg
    };

    let stt_cfg = cfg.stt.clone();
    let text = hermix_tools::tools::voice_mode::transcribe_audio(&stt_cfg, &samples, request.sample_rate)
        .await?;

    let provider = match &stt_cfg.provider {
        SttProviderType::Whisper => "whisper".to_string(),
        SttProviderType::Deepgram => "deepgram".to_string(),
        SttProviderType::Custom(_) => "custom".to_string(),
    };

    Ok(TranscribeResponse {
        text,
        provider,
        sample_count,
        duration_ms,
    })
}

/// Transcribe using a specific pipeline (preserves the user's STT
/// provider / language / VAD config from `voice_mode` tool calls).
pub async fn transcribe_with_pipeline(
    request: &TranscribeRequest,
    pipeline: &VoicePipeline,
) -> VoiceResult<TranscribeResponse> {
    transcribe_pcm(request, Some(pipeline)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_pcm_f32_handles_silence() {
        let req = TranscribeRequest {
            pcm_base64: "AAAAAAAAAAA=".to_string(),
            sample_rate: 16000,
            stt_provider: None,
            language: None,
        };
        let samples = req.decode_pcm_f32().unwrap();
        assert_eq!(samples.len(), 4);
        for s in samples {
            assert_eq!(s, 0.0);
        }
    }

    #[test]
    fn decode_pcm_f32_handles_max_amplitude() {
        let req = TranscribeRequest {
            pcm_base64: "/38=".to_string(),
            sample_rate: 16000,
            stt_provider: None,
            language: None,
        };
        let samples = req.decode_pcm_f32().unwrap();
        assert_eq!(samples.len(), 1);
        assert!((samples[0] - 1.0).abs() < 0.001);
    }

    #[test]
    fn decode_pcm_f32_rejects_zero_sample_rate() {
        let req = TranscribeRequest {
            pcm_base64: "AAAA".to_string(),
            sample_rate: 0,
            stt_provider: None,
            language: None,
        };
        match req.decode_pcm_f32() {
            Err(VoiceBridgeError::InvalidSampleRate(0)) => {}
            other => panic!("expected InvalidSampleRate(0), got {other:?}"),
        }
    }

    #[test]
    fn decode_pcm_f32_rejects_odd_byte_count() {
        let req = TranscribeRequest {
            pcm_base64: "AAAA".to_string(),
            sample_rate: 16000,
            stt_provider: None,
            language: None,
        };
        match req.decode_pcm_f32() {
            Err(VoiceBridgeError::Base64(msg)) => {
                assert!(msg.contains("odd"), "msg = {msg}");
            }
            other => panic!("expected Base64(odd), got {other:?}"),
        }
    }

    #[test]
    fn decode_pcm_f32_rejects_invalid_base64() {
        let req = TranscribeRequest {
            pcm_base64: "!!!not-base64!!!".to_string(),
            sample_rate: 16000,
            stt_provider: None,
            language: None,
        };
        match req.decode_pcm_f32() {
            Err(VoiceBridgeError::Base64(_)) => {}
            other => panic!("expected Base64, got {other:?}"),
        }
    }

    #[test]
    fn transcribe_response_serde_roundtrip() {
        let r = TranscribeResponse {
            text: "hi".to_string(),
            provider: "whisper".to_string(),
            sample_count: 16000,
            duration_ms: 1000,
        };
        let j = serde_json::to_string(&r).unwrap();
        let back: TranscribeResponse = serde_json::from_str(&j).unwrap();
        assert_eq!(r, back);
    }
}
