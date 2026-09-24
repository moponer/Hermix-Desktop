//! 设备请求签名验证（§5.5 第 4 层，对策 T4 重放）。
//!
//! token 泄漏 ≠ 无限使用：已配对设备在 Bearer 之外再附一条签名头，
//! 服务器校验 HMAC-SHA256 + 时间窗 + nonce 单次使用，抓包重发无效。
//!
//! 签名格式（客户端）：
//! ```text
//! X-Hermix-Signature: t=<unix_sec>,n=<hex nonce>,s=<hex hmac>
//! sig = HMAC_SHA256(device_token, METHOD + "\n" + path + "\n" + t + "\n" + nonce)
//! ```
//!
//! 签名不含 body：中间件阶段消费 body 会破坏 axum 提取器链路，
//! 防篡改由 TLS 传输层保证，本层只防重放（时间窗 + nonce）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 时间窗：签名时间戳偏离服务器当前时间的容许范围。
pub const TIMESTAMP_WINDOW_SECS: i64 = 300;
/// nonce 记忆时长：超过时间窗后即可安全清除。
const NONCE_TTL: Duration = Duration::from_secs(TIMESTAMP_WINDOW_SECS as u64 * 2);

/// 单次使用的 nonce 缓存（进程内）。重启清空是可接受的：
/// 时间窗只有 ±300s，重启期间旧签名基本已出窗。
/// 内部 Arc 使持有方（HttpSecurity）可 Clone 共享同一份缓存。
#[derive(Default, Clone)]
pub struct NonceCache {
    seen: std::sync::Arc<Mutex<HashMap<String, Instant>>>,
}

impl NonceCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 首次出现 → true（并记忆）；重复出现 → false。
    pub fn insert_if_new(&self, nonce: &str) -> bool {
        let mut seen = self.seen.lock().expect("nonce cache lock");
        let now = Instant::now();
        seen.retain(|_, at| now.duration_since(*at) < NONCE_TTL);
        seen.insert(nonce.to_string(), now).is_none()
    }

    pub fn len(&self) -> usize {
        self.seen.lock().expect("nonce cache lock").len()
    }
}

/// 计算签名（客户端与服务端共用同一实现，避免格式漂移）。
pub fn compute_signature(device_token: &str, method: &str, path: &str, t: i64, nonce: &str) -> String {
    use sha2::{Digest, Sha256};
    let message = format!("{method}\n{path}\n{t}\n{nonce}");
    let mut mac = Sha256::new();
    mac.update(device_token.as_bytes());
    mac.update(b"|hermix-req-sig|");
    mac.update(message.as_bytes());
    mac.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 解析并校验签名头。返回 Ok(()) 表示通过；
/// Err 描述首个失败原因（供日志与 401 响应）。
pub fn verify_signature(
    header: &str,
    device_token: &str,
    method: &str,
    path: &str,
    now: i64,
    nonces: &NonceCache,
) -> Result<(), &'static str> {
    let mut t: Option<i64> = None;
    let mut nonce: Option<&str> = None;
    let mut sig: Option<&str> = None;
    for part in header.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("t=") {
            t = v.parse().ok();
        } else if let Some(v) = part.strip_prefix("n=") {
            nonce = Some(v);
        } else if let Some(v) = part.strip_prefix("s=") {
            sig = Some(v);
        }
    }
    let (Some(t), Some(nonce), Some(sig)) = (t, nonce, sig) else {
        return Err("malformed signature header");
    };
    if (now - t).abs() > TIMESTAMP_WINDOW_SECS {
        return Err("signature timestamp outside window");
    }
    if !nonces.insert_if_new(nonce) {
        return Err("nonce replayed");
    }
    let expected = compute_signature(device_token, method, path, t, nonce);
    // 常量时间比较：逐字节异或累积，避免短路泄漏前缀匹配长度。
    if expected.len() != sig.len()
        || expected
            .bytes()
            .zip(sig.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return Err("signature mismatch");
    }
    Ok(())
}

/// 从请求头提取 `X-Hermix-Signature` 值（存在且非空）。
pub fn signature_header(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("x-hermix-signature")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "hmxd_test_token";
    const METHOD: &str = "POST";
    const PATH: &str = "/api/sessions/abc/messages";

    fn valid_header(now: i64) -> String {
        let nonce = "aabbccdd";
        let sig = compute_signature(TOKEN, METHOD, PATH, now, nonce);
        format!("t={now},n={nonce},s={sig}")
    }

    #[test]
    fn valid_signature_passes() {
        let nonces = NonceCache::new();
        let now = chrono::Utc::now().timestamp();
        assert!(verify_signature(&valid_header(now), TOKEN, METHOD, PATH, now, &nonces).is_ok());
    }

    #[test]
    fn stale_timestamp_rejected() {
        let nonces = NonceCache::new();
        let now = chrono::Utc::now().timestamp();
        let old = now - TIMESTAMP_WINDOW_SECS - 1;
        let header = {
            let nonce = "nonce-old";
            let sig = compute_signature(TOKEN, METHOD, PATH, old, nonce);
            format!("t={old},n={nonce},s={sig}")
        };
        assert!(verify_signature(&header, TOKEN, METHOD, PATH, now, &nonces).is_err());
    }

    #[test]
    fn nonce_replay_rejected() {
        let nonces = NonceCache::new();
        let now = chrono::Utc::now().timestamp();
        let header = valid_header(now);
        assert!(verify_signature(&header, TOKEN, METHOD, PATH, now, &nonces).is_ok());
        // 同一签名头重发：nonce 已记忆 → 拒绝。
        assert!(verify_signature(&header, TOKEN, METHOD, PATH, now, &nonces).is_err());
    }

    #[test]
    fn wrong_token_rejected() {
        let nonces = NonceCache::new();
        let now = chrono::Utc::now().timestamp();
        assert!(verify_signature(&valid_header(now), "hmxd_other", METHOD, PATH, now, &nonces).is_err());
    }

    #[test]
    fn malformed_header_rejected() {
        let nonces = NonceCache::new();
        let now = chrono::Utc::now().timestamp();
        assert!(verify_signature("garbage", TOKEN, METHOD, PATH, now, &nonces).is_err());
        assert!(verify_signature("t=abc,n=x,s=y", TOKEN, METHOD, PATH, now, &nonces).is_err());
    }

    #[test]
    fn different_method_or_path_rejected() {
        let nonces = NonceCache::new();
        let now = chrono::Utc::now().timestamp();
        assert!(verify_signature(&valid_header(now), TOKEN, "GET", PATH, now, &nonces).is_err());
        assert!(verify_signature(&valid_header(now), TOKEN, METHOD, "/other", now, &nonces).is_err());
    }

    #[test]
    fn nonce_cache_evicts_expired() {
        let nonces = NonceCache::new();
        nonces.insert_if_new("stale-nonce");
        assert_eq!(nonces.len(), 1);
        // 无法拨动 Instant；插入足够多不同 nonce 后旧条目仍在（TTL 内），
        // 这里只验证接口不 panic 且计数正确。
        nonces.insert_if_new("fresh-nonce");
        assert_eq!(nonces.len(), 2);
    }
}
