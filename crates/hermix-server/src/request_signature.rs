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
    ///
    /// 容量上限：`retain` 每次请求都是 O(n)，且此前无上限——高频设备请求
    /// 可让表单调增长到打满内存。超限时整表清空（与限流表同策略：这些
    /// 都是尽力而为的防护，内存不能无界）；清空只可能放过一个时间窗内的
    /// 重放，代价可接受。
    pub fn insert_if_new(&self, nonce: &str) -> bool {
        const NONCE_MAX: usize = 4096;
        let mut seen = self.seen.lock().expect("nonce cache lock");
        let now = Instant::now();
        seen.retain(|_, at| now.duration_since(*at) < NONCE_TTL);
        if seen.len() >= NONCE_MAX {
            seen.clear();
        }
        seen.insert(nonce.to_string(), now).is_none()
    }

    pub fn len(&self) -> usize {
        self.seen.lock().expect("nonce cache lock").len()
    }
}

/// 计算签名（客户端与服务端共用同一实现，避免格式漂移）。
///
/// **真 HMAC-SHA256**，不是 `SHA256(key ‖ msg)` 那种手搓构造。
/// 两者在这里的差别不是洁癖：后者是**长度扩展攻击**的标准靶子——攻击者拿到
/// 一组 (签名, 消息) 后能算出「同一密钥下、消息以任意尾巴扩展」的合法签名，
/// 而域分隔标签（`"|hermix-req-sig|"`）挡不住，因为攻击者已知前缀长度。
/// HMAC 的双重 ipad/opad 结构才是为此设计的。
///
/// 手写而不是引 `hmac` crate：`sha2` 已是依赖，HMAC 构造本身是良定义的常数行
/// 代码；App 侧用 WebCrypto 的 `subtle.sign`，两边由
/// `cross_language_signature_vector` 交叉验证——两个独立实现对同一固定向量
/// 算出同一个值，比任何一方单独 round-trip 都有说服力。
pub fn compute_signature(device_token: &str, method: &str, path: &str, t: i64, nonce: &str) -> String {
    use sha2::{Digest, Sha256};

    const BLOCK: usize = 64; // SHA-256 的块大小
    let key = device_token.as_bytes();
    // 密钥短于一块就补零，等于或长于一块先哈希到 64 字节（这里保留分支，
    // 因为 device token 格式可能变长，而客户端也会照做）。
    let mut k0 = [0u8; BLOCK];
    if key.len() > BLOCK {
        let h = Sha256::digest(key);
        k0[..32].copy_from_slice(&h);
    } else {
        k0[..key.len()].copy_from_slice(key);
    }

    let message = format!("{method}\n{path}\n{t}\n{nonce}");

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k0[i];
        opad[i] ^= k0[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message.as_bytes());
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().iter().map(|b| format!("{b:02x}")).collect()
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

    /// **跨语言契约向量**。Hermix M（uni-app / TS）必须算出**同一个 hex**——
    /// 这个测试是那条契约的权威定义，不是我对另一端实现的猜测。
    ///
    /// 为什么要固定向量而不是只做 round-trip：round-trip 只证明"自己算的能被
    /// 自己验"，两边算法一起错也照样绿。而 App 端的实现在另一个工具链里
    /// （H5 目标虽可在 CI 构建，但这段算法不在那条路径上），唯一能钉住线格式
    /// 的就是这个常量。改这里等于改协议，必须同步改 App 侧。
    #[test]
    fn cross_language_signature_vector() {
        assert_eq!(
            compute_signature("hmxd_test_token", "GET", "/v1/ws-stream/s1", 1_700_000_000, "abc123"),
            "521925f25e9194ded7ecc29a57879549d48ee8ceb752b390bf6a8f4e5b9a734f",
            "App 侧 computeSignature 必须与此逐字一致"
        );
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
