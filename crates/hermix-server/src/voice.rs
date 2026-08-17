//! Voice transcribe/synthesize endpoints and TickLoop control.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;

use serde::{Deserialize, Serialize};

use crate::errors::HttpError;

use super::*;

pub(crate) async fn voice_transcribe(
    Json(req): Json<hermix_voice::TranscribeRequest>,
) -> Result<Json<hermix_voice::TranscribeResponse>, HttpError> {
    let pipeline = hermix_voice::default_pipeline();
    let resp = hermix_voice::transcribe_pcm(&req, Some(&pipeline))
        .await
        .map_err(voice_bridge_err_to_http)?;
    Ok(Json(resp))
}

/// `POST /v1/voice/synthesize` — accept text + provider/voice/format
/// and return synthesized audio bytes. Currently a structured
/// placeholder; the actual network call to the upstream TTS provider
/// is performed by `tts_streaming` once a `PcmSink` is wired in.
pub(crate) async fn voice_synthesize(
    Json(req): Json<hermix_voice::SynthesizeRequest>,
) -> Result<Json<hermix_voice::SynthesizeResponse>, HttpError> {
    let resp = hermix_voice::synthesize_text(&req, None)
        .await
        .map_err(voice_bridge_err_to_http)?;
    Ok(Json(resp))
}

/// Convert `hermix_voice::VoiceBridgeError` to an `HttpError` with a
/// stable 4xx/5xx mapping.
fn voice_bridge_err_to_http(e: hermix_voice::VoiceBridgeError) -> HttpError {
    use hermix_voice::VoiceBridgeError as V;
    let status = match &e {
        V::Base64(_) | V::InvalidSampleRate(_) | V::EmptyAudio | V::Internal(_) => {
            StatusCode::BAD_REQUEST
        }
        V::EmptySynthesis | V::Tool(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpError {
        status,
        message: e.to_string(),
    }
}

#[cfg(test)]
mod voice_handler_tests {
    use super::*;

    #[test]
    fn base64_err_maps_to_400() {
        let e = hermix_voice::VoiceBridgeError::Base64("bad".into());
        let h = voice_bridge_err_to_http(e);
        assert_eq!(h.status, StatusCode::BAD_REQUEST);
        assert!(h.message.contains("bad"));
    }

    #[test]
    fn invalid_sample_rate_maps_to_400() {
        let e = hermix_voice::VoiceBridgeError::InvalidSampleRate(0);
        let h = voice_bridge_err_to_http(e);
        assert_eq!(h.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn empty_synthesis_maps_to_500() {
        let e = hermix_voice::VoiceBridgeError::EmptySynthesis;
        let h = voice_bridge_err_to_http(e);
        assert_eq!(h.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn empty_audio_maps_to_400() {
        let e = hermix_voice::VoiceBridgeError::EmptyAudio;
        let h = voice_bridge_err_to_http(e);
        assert_eq!(h.status, StatusCode::BAD_REQUEST);
    }
}

//TickLoop API
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct TickLoopStatus {
    running: bool,
    phase: String,
}

/// `GET /v1/tickloop` — return current TickLoop status.
pub(crate) async fn get_tickloop_status(State(state): State<HttpServerState>) -> Json<TickLoopStatus> {
    let running = state.tickloop_manager.is_running().await;
    Json(TickLoopStatus {
        running,
        phase: if running {
            "active".to_string()
        } else {
            "stopped".to_string()
        },
    })
}

#[derive(Debug, Deserialize)]
pub(crate) struct TickLoopToggle {
    enabled: bool,
}

/// `POST /v1/tickloop` — enable or disable the TickLoop at runtime.
pub(crate) async fn toggle_tickloop(
    State(state): State<HttpServerState>,
    Json(body): Json<TickLoopToggle>,
) -> Result<Json<TickLoopStatus>, HttpError> {
    if body.enabled {
        state.tickloop_manager
            .start(Some(Arc::new(state.consciousness_tracker.clone())))
            .await;
    } else {
        state.tickloop_manager.stop().await;
    }
    Ok(Json(TickLoopStatus {
        running: body.enabled,
        phase: if body.enabled { "active".to_string() } else { "stopped".to_string() },
    }))
}
