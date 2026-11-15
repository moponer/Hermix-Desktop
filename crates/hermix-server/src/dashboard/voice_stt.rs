//! 手机端语音识别配置（桌面端配置 → 手机继承，主人拍板 2026-10-14）。
//!
//! 数据流：桌面设置页 `PUT /api/voice/stt` 写 `$HERMIX_HOME/voice_stt.json`
//! → 每条 WS 会话建立时（`ws::handle_ws_stream`）经密封帧
//! `voice.stt_config` 下发 → 手机端据此构建 `CloudAsr` 直连该 provider。
//! 手机把音频直接发给 provider（不经 relay、不经 PC），识别出的文字
//! 走既有加密 WS 通道——relay 全程只见密封帧。
//!
//! **未配置 = 不推帧**，手机端长按说话显示「桌面端未配置，仅支持文字发送」，
//! 与拍板语义一致。api_key 属敏感凭据：只经 E2EE 密封通道与本地 dashboard
//! API 存取，永不进日志（本模块不打任何含 key 的日志）。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::errors::HttpError;
use crate::state::HttpServerState;
use axum::http::StatusCode;

/// 一条云 STT provider 配置。provider 无关：只要 endpoint 收 POST 音频
/// （multipart 或裸字节）返回 `{text}` 即可（MiniMax / OpenAI 兼容 /
/// SiliconFlow 等都满足），具体形状由手机端 `CloudAsr.headers` 适配。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceSttConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 识别端点完整 URL（含 path）。
    #[serde(default)]
    pub endpoint: String,
    /// provider API key。敏感：仅存本文件 + 经 E2EE 帧下发。
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: Option<String>,
    /// 语言代码，如 "zh"。
    #[serde(default)]
    pub language: Option<String>,
    /// 音频 MIME（如 audio/mpeg），手机端录音格式与它配套。
    #[serde(default = "default_audio_mime")]
    pub audio_mime: String,
    /// 附加请求头（部分 provider 要求自定义鉴权头形状）。
    #[serde(default)]
    pub extra_headers: std::collections::BTreeMap<String, String>,
}

fn default_audio_mime() -> String {
    "audio/mpeg".into()
}

impl VoiceSttConfig {
    fn path(state: &HttpServerState) -> PathBuf {
        state.hermix_home.join("voice_stt.json")
    }

    /// 读配置。文件缺失 / 解析失败 → None（= 未配置）。
    pub fn load(state: &HttpServerState) -> Option<Self> {
        let raw = std::fs::read_to_string(Self::path(state)).ok()?;
        match serde_json::from_str(&raw) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::warn!("voice_stt.json 解析失败（按未配置处理）: {e}");
                None
            }
        }
    }

    fn save(&self, state: &HttpServerState) -> Result<(), String> {
        let path = Self::path(state);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}

/// GET /api/voice/stt — 桌面设置页读当前配置。未配置返回 enabled:false 空壳。
pub async fn get_voice_stt(State(state): State<HttpServerState>) -> Json<VoiceSttConfig> {
    Json(VoiceSttConfig::load(&state).unwrap_or_default())
}

/// PUT /api/voice/stt — 桌面设置页保存。保存即生效：下一次 WS 会话建立
/// 就会把新配置推给手机（已有会话在重连后更新）。
pub async fn put_voice_stt(
    State(state): State<HttpServerState>,
    Json(body): Json<VoiceSttConfig>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // 边界校验：开启时 endpoint 必须是 https（手机在公网环境直连它）
    if body.enabled {
        if body.endpoint.is_empty() {
            return Err(HttpError {
                status: StatusCode::BAD_REQUEST,
                message: "开启语音识别必须填写 endpoint".into(),
            });
        }
        if !body.endpoint.starts_with("https://") {
            return Err(HttpError {
                status: StatusCode::BAD_REQUEST,
                message: "endpoint 必须是 https（音频要从公网直连）".into(),
            });
        }
        if body.api_key.is_empty() {
            return Err(HttpError {
                status: StatusCode::BAD_REQUEST,
                message: "开启语音识别必须填写 API key".into(),
            });
        }
    }
    body.save(&state).map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("voice_stt.json 写入失败: {e}"),
    })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
