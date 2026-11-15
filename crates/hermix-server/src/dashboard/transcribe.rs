//! POST /api/transcribe — STT: receive base64 audio, return transcribed text.
//!
//! Request body: `{ "audio": "<base64>", "format": "webm" }`
//! Response body: `{ "text": "transcribed text" }`
//!
//! Uses OpenAI Whisper API (OPENAI_API_KEY env). Falls back to MINIMAX_API_KEY
//! if OPENAI_API_KEY is not set (experimental).

use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::Json as JsonResponse;
use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct TranscribeRequest {
    pub audio: String,
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "webm".into()
}

#[derive(Serialize)]
pub struct TranscribeResponse {
    pub text: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// POST /api/transcribe
pub async fn transcribe(
    Json(req): Json<TranscribeRequest>,
) -> Result<JsonResponse<TranscribeResponse>, (StatusCode, JsonResponse<ErrorResponse>)> {
    // 1. Decode base64 audio
    let audio_bytes: Vec<u8> = match base64::engine::general_purpose::STANDARD.decode(&req.audio) {
        Ok(b) => b,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                JsonResponse(ErrorResponse {
                    error: format!("Invalid base64 audio: {e}"),
                }),
            ));
        }
    };

    if audio_bytes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            JsonResponse(ErrorResponse {
                error: "Empty audio data".to_string(),
            }),
        ));
    }

    // 2. Try OpenAI Whisper first
    match transcribe_whisper(&audio_bytes, &req.format).await {
        Ok(text) => Ok(JsonResponse(TranscribeResponse { text })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            JsonResponse(ErrorResponse { error: e }),
        )),
    }
}

async fn transcribe_whisper(audio: &[u8], format: &str) -> Result<String, String> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY not set. Configure it in Settings → Providers → OpenAI.".to_string())?;

    // 无超时的 client 会把 STT 请求永久挂在 worker 上（上游卡住 = 前端永远转圈）。
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP client init failed: {e}"))?;

    let filename = format!("audio.{format}");
    let mime = format!("audio/{format}");

    let part = reqwest::multipart::Part::bytes(audio.to_vec())
        .file_name(filename)
        .mime_str(&mime)
        .map_err(|e| format!("MIME error: {e}"))?;

    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", "whisper-1");

    let resp = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {api_key}"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Whisper request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Whisper API error {}: {body}", status));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Whisper response: {e}"))?;

    let text = json
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    Ok(text)
}
