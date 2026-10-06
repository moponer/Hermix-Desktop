//! relay 帧加密 —— PC 侧（P-256 ECDH + AES-256-GCM）
//!
//! 与 App 侧 `apps/hm/src/core/crypto.ts` 互为镜像。**帧格式、HKDF salt/info、
//! AAD 的字节必须逐帧一致**，改动任何一处都要同步另一处，并由
//! `crypto_conformance` 测试守住。
//!
//! ## ECDH 的固定侧永远是「接收方」
//!
//! 发送方 S（手机）与接收方 R（PC）各持一个长期静态密钥对，配对时交换公钥。
//!
//! ```text
//! seal（S 做）：ECDH(ephemeral_private, R.public)  → 随帧下发 ephemeral_public
//! open（R 做）：ECDH(R.private, ephemeral_public)
//! ```
//!
//! 两步用的是**同一个身份 R** 的两侧。写混了不会报明确的错，只会让 open
//! 一直失败——而一屏 `assert!(result.is_err())` 会因此假通过。
//!
//! ## 选型
//!
//! P-256 而非 X25519：App 侧只有 WebCrypto，没有 Curve25519；P-256 是
//! `subtle.deriveBits` 的原生曲线，两端都不必自研密码学。HKDF-SHA256 与
//! AES-256-GCM 同理（WebCrypto 没有 ChaCha20）。
//!
//! 每帧一个 ephemeral → 单帧密钥泄露不影响其他帧，这就是前向保密的最小
//! 实现。不做 Double Ratchet：单向控制流不是双向持续会话。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

use crate::relay_proto::{Envelope, FrameTag, SealedFrame};

/// 本模块的错误。刻意不套用 `mobile::MobileError` —— 帧加密是线路层的事，
/// 耦合到 IM 模块会让两边互相拖。调用方（`relay_transport`）自己决定怎么映射。
#[derive(Debug)]
pub enum CryptoError {
    /// 对端给的 ephemeral 公钥不是合法 P-256 点。
    BadEphemeral(String),
    /// AES-GCM 认证失败：AAD / salt / 方向 / 密文任一被改动都走这里。
    AuthFailed,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::BadEphemeral(e) => write!(f, "bad ephemeral public key: {e}"),
            CryptoError::AuthFailed => write!(f, "frame authentication failed"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// AES-256-GCM 标准 nonce 长度。
pub const NONCE_LEN: usize = 12;

/// HKDF info 前缀，保证两个方向派生出的 key 不相同。
/// 必须与 `crypto.ts` 的 `INFO` 逐字节相同。
pub const INFO_D2N: &[u8] = b"hermix/frame/v1/d2n";
pub const INFO_N2D: &[u8] = b"hermix/frame/v1/n2d";

/// 一方的长期静态身份。配对时只交换 `public_key`。
pub struct Identity {
    secret: SecretKey,
}

impl Identity {
    pub fn from_secret(secret: SecretKey) -> Self {
        Self { secret }
    }
}

/// 密封后随帧下发的密文材料。`ephemeral_public` 走明文帧头。
#[derive(Clone)]
pub struct SealedBody {
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
    pub ephemeral_public: [u8; 65],
}

/// 帧上下文。`frame_id` 进 HKDF salt（防跨帧重放），`header` 作 AAD
/// （relay 看得见 header，改它即解密失败）。
pub struct FrameContext<'a> {
    pub frame_id: &'a [u8],
    pub header: &'a [u8],
    pub direction: Direction,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// 手机 → PC
    DeviceToNode,
    /// PC → 手机
    NodeToDevice,
}

impl Direction {
    fn info(self) -> &'static [u8] {
        match self {
            Direction::DeviceToNode => INFO_D2N,
            Direction::NodeToDevice => INFO_N2D,
        }
    }
}

impl Identity {
    /// 随机生成一方身份。
    pub fn generate() -> Self {
        Self {
            secret: SecretKey::random(&mut rand::rngs::OsRng),
        }
    }

    /// 供测试用：从 32 字节种子派生（生产路径不要用）。
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            secret: SecretKey::from_slice(seed).expect("32 bytes is a valid P-256 scalar"),
        }
    }

    /// 未压缩 SPKI，65 字节。进二维码的就是它。
    pub fn public_key(&self) -> [u8; 65] {
        self.secret
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .try_into()
            .expect("uncompressed P-256 point is always 65 bytes")
    }

    /// 私钥标量的 32 字节。**只给持久化层用**（`relay_identity` 拿去 DPAPI
    /// 保护）——任何密码学操作都该走 `seal_body` / `open_body`。
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes().into()
    }
}

/// shared secret → content key。
fn content_key(shared: &[u8], ctx: &FrameContext<'_>) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(ctx.frame_id), shared);
    let mut key = [0u8; 32];
    // 32 字节对 HKDF-SHA256 总是够的（hash 输出长度），不会失败。
    hk.expand(ctx.direction.info(), &mut key)
        .expect("32 bytes fits HKDF-SHA256 output");
    key
}

/// 加密一帧的 body。`receiver_public` 必须是**接收方**的静态公钥。
pub fn seal_body(
    plaintext: &[u8],
    receiver_public: &PublicKey,
    ctx: &FrameContext<'_>,
) -> SealedBody {
    let ephemeral = SecretKey::random(&mut rand::rngs::OsRng);
    let shared = diffie_hellman(ephemeral.to_nonzero_scalar(), receiver_public.as_affine());
    let key = content_key(shared.raw_secret_bytes().as_slice(), ctx);

    let mut nonce = [0u8; NONCE_LEN];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("32 bytes is a valid AES-256 key");
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: ctx.header,
            },
        )
        .expect("AES-GCM encrypt only fails on absurdly long input");

    SealedBody {
        nonce,
        ciphertext,
        ephemeral_public: ephemeral
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .try_into()
            .expect("uncompressed P-256 point is always 65 bytes"),
    }
}

/// 解密一帧的 body。`receiver_private` 必须是**同一个接收方**的静态私钥。
pub fn open_body(
    sealed: &SealedBody,
    receiver_private: &SecretKey,
    ctx: &FrameContext<'_>,
) -> Result<Vec<u8>, CryptoError> {
    let ephemeral = PublicKey::from_sec1_bytes(&sealed.ephemeral_public)
        .map_err(|e| CryptoError::BadEphemeral(e.to_string()))?;
    let shared = diffie_hellman(receiver_private.to_nonzero_scalar(), ephemeral.as_affine());
    let key = content_key(shared.raw_secret_bytes().as_slice(), ctx);

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| CryptoError::BadEphemeral(e.to_string()))?;
    cipher
        .decrypt(
            Nonce::from_slice(&sealed.nonce),
            Payload {
                msg: &sealed.ciphertext,
                aad: ctx.header,
            },
        )
        .map_err(|_| CryptoError::AuthFailed)
}

/// 把一帧的明文内层帧封进 `env.payload`。
///
/// AAD 取 `env` 上的路由字段（`Envelope::aad()`），所以**必须先把 Envelope
/// 的路由字段填好再调用**——`payload` 不参与 AAD，用空串占位即可。
///
/// # peer 与 `open_envelope` 的 `own` 必须同源
///
/// `peer` 是**接收方**的静态公钥。谁拿 `own` 去解，ECDH 就必须对齐到同一个
/// 身份：`ECDH(eph_priv, peer_pub)` 与 `ECDH(peer_priv, eph_pub)` 导出同一个
/// shared secret。**写混成"seal 面向 A、却拿 B 的私钥解"永远解不开**，而且失败
/// 表现是 `AuthFailed`，看不出是配对错了还是密文被改——这个坑本模块已经踩过
/// 两次，所以在这里写死。
///
/// `frame_id` 两端必须一致（它进 HKDF salt），由调用方保证——实践中取随机
/// 16 字节，对方从 `eid` 字段拿到。
pub fn seal_envelope(
    env: &mut Envelope,
    inner_plaintext: &str,
    peer: &PublicKey,
    frame_id: &[u8],
) -> Result<(), CryptoError> {
    use base64::Engine;
    let ctx = FrameContext {
        frame_id,
        header: &env.aad(),
        direction: Direction::NodeToDevice,
    };
    let sealed = seal_body(inner_plaintext.as_bytes(), peer, &ctx);
    let b64 = base64::engine::general_purpose::STANDARD;
    env.payload = Some(
        serde_json::to_string(&SealedFrame {
            ct: b64.encode(&sealed.ciphertext),
            nonce: b64.encode(sealed.nonce),
            ep: b64.encode(sealed.ephemeral_public),
            eid: b64.encode(frame_id),
        })
        .map_err(|e| CryptoError::BadEphemeral(e.to_string()))?,
    );
    Ok(())
}

/// `payload` 是否是**形状正确**的密封信封（四个字段齐全且非空）。
///
/// 与 `apps/relay/src/proto.js::isSealed` 同一口径——relay 靠它判"这帧
/// 受不受保护"，hermix 侧用它判"收到的帧是不是被封装过"。两端判定必须
/// 一致，否则要么 relay 拒掉合法密文，要么 hermix 去解明文。
pub fn is_sealed_payload(payload: &str) -> bool {
    let Ok(frame) = serde_json::from_str::<SealedFrame>(payload) else {
        return false;
    };
    !frame.ct.is_empty()
        && !frame.nonce.is_empty()
        && !frame.ep.is_empty()
        && !frame.eid.is_empty()
}

/// 拆开 `env.payload`，还原明文内层帧。
///
/// `own` 必须是**发送方**的静态私钥——与 seal 时的 `peer` 同源。`frame_id`
/// 从密文信封的 `eid` 取，不另找来源。
///
/// `direction` 是**发送方向**。PC 侧解手机来的帧恒为 [`Direction::DeviceToNode`]；
/// 测试里要模拟"手机解开 PC 封的帧"就得传 [`Direction::NodeToDevice`]。方向进
/// HKDF info，所以传错必然解不开——这正是防串向的机制本身。
pub fn open_envelope(
    env: &Envelope,
    own: &SecretKey,
    direction: Direction,
) -> Result<String, CryptoError> {
    use base64::Engine;
    let raw = env
        .payload
        .as_deref()
        .ok_or_else(|| CryptoError::AuthFailed)?;
    let frame: SealedFrame =
        serde_json::from_str(raw).map_err(|e| CryptoError::BadEphemeral(e.to_string()))?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let decode = |s: &str| -> Result<Vec<u8>, CryptoError> {
        b64.decode(s)
            .map_err(|e| CryptoError::BadEphemeral(format!("base64: {e}")))
    };
    let nonce_b = decode(&frame.nonce)?;
    if nonce_b.len() != NONCE_LEN {
        return Err(CryptoError::BadEphemeral(format!(
            "nonce is {} bytes, expected {NONCE_LEN}",
            nonce_b.len()
        )));
    }
    let sealed = SealedBody {
        nonce: nonce_b
            .try_into()
            .map_err(|_| CryptoError::BadEphemeral("nonce length".into()))?,
        ciphertext: decode(&frame.ct)?,
        ephemeral_public: decode(&frame.ep)?
            .try_into()
            .map_err(|_| CryptoError::BadEphemeral("ephemeral must be 65 bytes".into()))?,
    };
    let ctx = FrameContext {
        frame_id: &decode(&frame.eid)?,
        header: &env.aad(),
        direction,
    };
    let plain = open_body(&sealed, own, &ctx)?;
    String::from_utf8(plain).map_err(|e| CryptoError::BadEphemeral(format!("inner frame utf8: {e}")))
}

// ── 配对期密钥交换（TLS 式） ─────────────────────────────────────

/// 手机在配对后发来的密钥交换帧的 `type`。
///
/// **为什么公钥交换本身也要加密**：手机的静态公钥只有一条传输路径——
/// 经 relay。而 relay 手上有手机公钥就能解开 PC 发给手机的全部帧，等于
/// E2EE 形同虚设。解法与 TLS 交换证书同构：用二维码里已有的 `pk_pc` 作为
/// 信任锚，把 `[设备信息, pk_ph]` 整体封一层再发。relay 看得到信封但解不开
/// （没有 `sk_pc`），于是学不到 `pk_ph`。
pub const KEY_EXCHANGE_TYPE: &str = "key.exchange";

/// 明文密钥交换载荷。**这个结构本身是明文 JSON**——机密性来自外层 seal，
/// 不来自字段本身。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct KeyExchangePayload {
    /// `key.exchange`
    #[serde(rename = "type")]
    pub kind: String,
    /// 手机自己的静态公钥，65 字节未压缩 SPKI，base64。
    pub pk: String,
    /// 配对码的 device_id，PC 侧据此写 `peers.json`。
    pub device_id: String,
}

/// 造一帧密钥交换密文：手机侧用 `pc_public` 封，`device_id` + `pk_phone` 在里。
pub fn build_key_exchange_envelope(
    env: &mut Envelope,
    pc_public: &PublicKey,
    device_id: &str,
    pk_phone: &[u8; 65],
    frame_id: &[u8],
) -> Result<(), CryptoError> {
    use base64::Engine;
    let payload = KeyExchangePayload {
        kind: KEY_EXCHANGE_TYPE.to_string(),
        pk: base64::engine::general_purpose::STANDARD.encode(pk_phone),
        device_id: device_id.to_string(),
    };
    let plain = serde_json::to_string(&payload)
        .map_err(|e| CryptoError::BadEphemeral(format!("key.exchange json: {e}")))?;
    // 手机 → PC，方向是 d2n。
    let ctx = FrameContext {
        frame_id,
        header: &env.aad(),
        direction: Direction::DeviceToNode,
    };
    let sealed = seal_body(plain.as_bytes(), pc_public, &ctx);
    let b64 = base64::engine::general_purpose::STANDARD;
    env.payload = Some(
        serde_json::to_string(&SealedFrame {
            ct: b64.encode(&sealed.ciphertext),
            nonce: b64.encode(sealed.nonce),
            ep: b64.encode(sealed.ephemeral_public),
            eid: b64.encode(frame_id),
        })
        .map_err(|e| CryptoError::BadEphemeral(e.to_string()))?,
    );
    Ok(())
}

/// 判断一段**已解密的**内层帧是不是密钥交换。
pub fn is_key_exchange(plain: &str) -> bool {
    serde_json::from_str::<KeyExchangePayload>(plain)
        .map(|p| p.kind == KEY_EXCHANGE_TYPE)
        .unwrap_or(false)
}

/// 解析密钥交换载荷里的手机公钥。
pub fn parse_key_exchange(plain: &str) -> Result<(String, [u8; 65]), CryptoError> {
    use base64::Engine;
    let p: KeyExchangePayload = serde_json::from_str(plain)
        .map_err(|e| CryptoError::BadEphemeral(format!("key.exchange parse: {e}")))?;
    if p.kind != KEY_EXCHANGE_TYPE {
        return Err(CryptoError::BadEphemeral("not a key.exchange frame".into()));
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(p.pk.as_bytes())
        .map_err(|e| CryptoError::BadEphemeral(format!("pk base64: {e}")))?;
    let pk: [u8; 65] = raw
        .try_into()
        .map_err(|_| CryptoError::BadEphemeral("pk is not 65 bytes".into()))?;
    // 必须是合法的 P-256 点，否则后面每次 seal 都会失败。
    PublicKey::from_sec1_bytes(&pk)
        .map_err(|e| CryptoError::BadEphemeral(format!("pk is not a valid P-256 point: {e}")))?;
    Ok((p.device_id, pk))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &[u8] = br#"{"v":2,"type":"route"}"#;

    /// 手机 → PC：密封用 PC 的静态公钥，PC 用自己的静态私钥解开。
    /// 这条是**基线**——下面那些 `is_err()` 断言只有在它成立时才有意义，
    /// 否则被测代码无条件失败会让它们全部假通过。
    #[test]
    fn d2n_roundtrip() {
        let pc = Identity::from_seed(&[2u8; 32]);
        let ctx = FrameContext {
            frame_id: b"frame-0001",
            header: HEADER,
            direction: Direction::DeviceToNode,
        };
        let sealed = seal_body("把 C 盘清一下".as_bytes(), &pc.secret.public_key(), &ctx);
        let plain = open_body(&sealed, &pc.secret, &ctx).expect("baseline must open");
        assert_eq!(plain, "把 C 盘清一下".as_bytes());
    }

    #[test]
    fn n2d_roundtrip() {
        let phone = Identity::from_seed(&[4u8; 32]);
        let ctx = FrameContext {
            frame_id: b"frame-0002",
            header: HEADER,
            direction: Direction::NodeToDevice,
        };
        let sealed = seal_body("已完成".as_bytes(), &phone.secret.public_key(), &ctx);
        let plain = open_body(&sealed, &phone.secret, &ctx).expect("reverse direction must open");
        assert_eq!(plain, "已完成".as_bytes());
    }

    #[test]
    fn header_tamper_fails_aad() {
        let pc = Identity::from_seed(&[6u8; 32]);
        let ctx = FrameContext {
            frame_id: b"f4",
            header: HEADER,
            direction: Direction::DeviceToNode,
        };
        let sealed = seal_body(b"x", &pc.secret.public_key(), &ctx);
        let tampered = FrameContext {
            header: br#"{"v":2,"type":"route","seq":999}"#,
            ..ctx
        };
        assert!(open_body(&sealed, &pc.secret, &tampered).is_err());
    }

    #[test]
    fn frame_id_change_fails() {
        let pc = Identity::from_seed(&[8u8; 32]);
        let ctx = FrameContext {
            frame_id: b"frame-A",
            header: HEADER,
            direction: Direction::DeviceToNode,
        };
        let sealed = seal_body(b"secret", &pc.secret.public_key(), &ctx);
        let other = FrameContext {
            frame_id: b"frame-B",
            ..ctx
        };
        assert!(open_body(&sealed, &pc.secret, &other).is_err());
    }

    #[test]
    fn direction_flip_fails() {
        let pc = Identity::from_seed(&[10u8; 32]);
        let ctx = FrameContext {
            frame_id: b"f6",
            header: HEADER,
            direction: Direction::DeviceToNode,
        };
        let sealed = seal_body(b"secret", &pc.secret.public_key(), &ctx);
        let flipped = FrameContext {
            direction: Direction::NodeToDevice,
            ..ctx
        };
        assert!(open_body(&sealed, &pc.secret, &flipped).is_err());
    }

    #[test]
    fn ciphertext_tamper_fails() {
        let pc = Identity::from_seed(&[12u8; 32]);
        let ctx = FrameContext {
            frame_id: b"f7",
            header: HEADER,
            direction: Direction::DeviceToNode,
        };
        let sealed = seal_body(b"secret", &pc.secret.public_key(), &ctx);
        let mut bad = sealed.clone();
        bad.ciphertext[0] ^= 0xff;
        assert!(open_body(&bad, &pc.secret, &ctx).is_err());
    }

    #[test]
    fn wrong_receiver_fails() {
        // 用 A 的公钥密封，却拿 B 的私钥解 —— shared secret 不同。
        let pc = Identity::from_seed(&[14u8; 32]);
        let impostor = Identity::from_seed(&[15u8; 32]);
        let ctx = FrameContext {
            frame_id: b"f8",
            header: HEADER,
            direction: Direction::DeviceToNode,
        };
        let sealed = seal_body(b"secret", &pc.secret.public_key(), &ctx);
        assert!(open_body(&sealed, &impostor.secret, &ctx).is_err());
    }

    #[test]
    fn each_frame_uses_a_fresh_ephemeral() {
        let pc = Identity::from_seed(&[16u8; 32]);
        let ctx = FrameContext {
            frame_id: b"same",
            header: HEADER,
            direction: Direction::DeviceToNode,
        };
        let a = seal_body(b"same plaintext", &pc.secret.public_key(), &ctx);
        let b = seal_body(b"same plaintext", &pc.secret.public_key(), &ctx);
        assert_ne!(a.ephemeral_public, b.ephemeral_public);
        assert_ne!(a.ciphertext, b.ciphertext);
    }


    /// 端到端：PC 把明文内层帧封进 Envelope，**手机侧**（这里用 PC 的私钥
    /// 扮演对端）解回原文。这是**基线**——下面两条性质都建立在它之上。
    ///
    /// 注意方向：PC seal 走 `NodeToDevice`，所以解它的也必须是
    /// `NodeToDevice`。PC 自己解手机来的帧才是 `DeviceToNode`。
    #[test]
    fn envelope_seal_open_roundtrip() {
        let pc = Identity::from_seed(&[20u8; 32]);

        let mut env = Envelope::new(FrameTag::Route);
        env.session_id = Some("im:device:dev-aaa".into());
        let inner = r#"{"type":"assistant_message","content":"hi"}"#;
        seal_envelope(&mut env, inner, &pc.secret.public_key(), b"frame-x").expect("seal");

        let payload = env.payload.as_deref().expect("payload must be set");
        assert!(is_sealed_payload(payload), "payload 必须是密封信封形状");
        assert!(!payload.contains("assistant_message"), "明文不得留在信封里");

        let back = open_envelope(&env, &pc.secret, Direction::NodeToDevice).expect("open");
        assert_eq!(back, inner);
    }

    /// 方向隔离：PC 用 `DeviceToNode` 去解自己 `NodeToDevice` 封的帧必须失败。
    /// 方向进 HKDF info，串向就解不开——这是机制本身，不是配置疏忽。
    #[test]
    fn envelope_direction_mismatch_fails() {
        let pc = Identity::from_seed(&[26u8; 32]);
        let mut env = Envelope::new(FrameTag::Route);
        env.session_id = Some("s1".into());
        seal_envelope(&mut env, "secret", &pc.secret.public_key(), b"f1").expect("seal");
        assert!(
            open_envelope(&env, &pc.secret, Direction::DeviceToNode).is_err(),
            "方向反了必须解不开"
        );
    }

    /// AAD 绑路由字段 → 密文搬到别的 session_id 解不开。
    #[test]
    fn ciphertext_cannot_be_moved_to_another_session() {
        let pc = Identity::from_seed(&[22u8; 32]);

        let mut env = Envelope::new(FrameTag::Route);
        env.session_id = Some("im:device:dev-aaa".into());
        seal_envelope(&mut env, "secret", &pc.secret.public_key(), b"f1").expect("seal");

        // 攻击者把同一段密文搬到 B 会话
        let mut moved = env.clone();
        moved.session_id = Some("im:device:dev-bbb".into());
        assert!(
            open_envelope(&moved, &pc.secret, Direction::DeviceToNode).is_err(),
            "密文必须不能跨会话搬运"
        );
    }

    /// 明文 payload（未封装）直接拒——relay 侧也会拒，这里是 hermix 侧的第二道。
    #[test]
    fn plaintext_envelope_is_rejected() {
        let pc = Identity::from_seed(&[24u8; 32]);
        let mut env = Envelope::new(FrameTag::Route);
        env.payload = Some(r#"{"type":"chat","text":"hi"}"#.into());
        assert!(open_envelope(&env, &pc.secret, Direction::DeviceToNode).is_err());
    }

    /// seal_envelope 后 envelope 自身仍是合法 JSON 序列化的。
    #[test]
    fn sealed_envelope_serializes() {
        let phone = Identity::from_seed(&[25u8; 32]);
        let mut env = Envelope::new(FrameTag::Route);
        env.session_id = Some("s1".into());
        seal_envelope(&mut env, "x", &phone.secret.public_key(), b"f").expect("seal");
        let s = serde_json::to_string(&env).expect("serialize");
        let back: Envelope = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back.session_id.as_deref(), Some("s1"));
        assert!(is_sealed_payload(back.payload.as_deref().unwrap()));
    }
    /// 密钥交换往返：手机用 pk_pc 封，PC 用 sk_pc 开。
    #[test]
    fn key_exchange_roundtrip() {
        let pc = Identity::from_seed(&[30u8; 32]);
        let phone = Identity::from_seed(&[31u8; 32]);
        let mut env = Envelope::new(FrameTag::Route);
        env.session_id = Some("im:device:dev-1".into());
        build_key_exchange_envelope(&mut env, &pc.secret.public_key(), "dev-1", &phone.public_key(), b"kx-1")
            .expect("build key exchange");
        // PC 侧用自己的私钥开（发送方是手机）。
        let plain = open_envelope(&env, &pc.secret, Direction::DeviceToNode).expect("open");
        assert!(is_key_exchange(&plain));
        let (device_id, pk) = parse_key_exchange(&plain).expect("parse");
        assert_eq!(device_id, "dev-1");
        assert_eq!(pk, phone.public_key());
    }

    /// **核心边界**：relay 看得到密钥交换的信封，但没 sk_pc 就解不开——于是
    /// 学不到 pk_ph。这条守住了，PC→手机的帧才安全。
    #[test]
    fn relay_cannot_read_the_key_exchange() {
        let pc = Identity::from_seed(&[32u8; 32]);
        let phone = Identity::from_seed(&[33u8; 32]);
        let mut env = Envelope::new(FrameTag::Route);
        env.session_id = Some("im:device:dev-2".into());
        build_key_exchange_envelope(&mut env, &pc.secret.public_key(), "dev-2", &phone.public_key(), b"kx-2")
            .expect("build");
        let payload = env.payload.as_deref().unwrap();
        assert!(is_sealed_payload(payload));
        assert!(!payload.contains("dev-2"), "设备 id 不该泄漏到密文外");
        // 拿到手机公钥（明文交换老方案的漏洞）但没 pk_pc 的一方，解不开。
        let impostor = Identity::from_seed(&[34u8; 32]);
        assert!(
            open_envelope(&env, &impostor.secret, Direction::DeviceToNode).is_err(),
            "没有 pk_pc 对应私钥的一方必须解不开"
        );
    }

    /// PC→手机方向的机密性**完全**依赖 pk_ph 没经明文传输。测试把这条边界
    /// 写死，提醒后来者别把密钥交换改回明文。
    #[test]
    fn phone_direction_needs_the_real_phone_key() {
        let phone = Identity::from_seed(&[36u8; 32]);
        let attacker = Identity::from_seed(&[37u8; 32]);
        let mut env = Envelope::new(FrameTag::Route);
        env.session_id = Some("s".into());
        seal_envelope(&mut env, "机密", &phone.secret.public_key(), b"f").expect("seal");
        // 持有手机私钥的一方能解。
        assert!(open_envelope(&env, &phone.secret, Direction::NodeToDevice).is_ok());
        // 没有 sk_ph 的攻击者解不开。
        assert!(open_envelope(&env, &attacker.secret, Direction::NodeToDevice).is_err());
    }

    /// 非 P-256 的 65 字节垃圾不能被当公钥存下来。
    #[test]
    fn key_exchange_rejects_invalid_point() {
        let pc = Identity::from_seed(&[38u8; 32]);
        let mut env = Envelope::new(FrameTag::Route);
        env.session_id = Some("s".into());
        build_key_exchange_envelope(&mut env, &pc.secret.public_key(), "d", &[0u8; 65], b"k")
            .expect("build");
        let plain = open_envelope(&env, &pc.secret, Direction::DeviceToNode).expect("open");
        assert!(parse_key_exchange(&plain).is_err(), "全零不是合法的 P-256 点");
    }

    /// 密钥交换帧不能被普通业务帧冒充。
    #[test]
    fn key_exchange_detection_is_strict() {
        assert!(!is_key_exchange(r#"{"type":"chat","text":"hi"}"#));
        assert!(!is_key_exchange("not json"));
        assert!(!is_key_exchange(r#"{"kind":"chat"}"#));
    }

    #[test]
    fn public_key_is_65_byte_uncompressed_spki() {
        let id = Identity::from_seed(&[17u8; 32]);
        assert_eq!(id.public_key().len(), 65);
        assert_eq!(id.public_key()[0], 0x04, "uncompressed point tag");
    }

    // ── 跨端一致性 ────────────────────────────────────────────────
    //
    // 下面这组常量由 `apps/hm/dump-vector.ts` 生成（App 用 WebCrypto 跑一遍
    // 真实路径后打印），`PC_PUB` 则是 `Identity::from_seed(&[2u8; 32]).public_key()`
    // 的输出，由 `conformance_fixture_is_self_consistent` 守着。
    // 改协议要重新出基线时：先跑 App 侧 dump 换掉 APP_* 三个常量，
    // Rust 侧这三个不用动（由 seed 推出）。
    //
    // **为什么需要它**：WebCrypto 不能导入 ECDH 私钥，所以「拿固定向量让 App
    // 解密」这条路根本不存在。能验的只有让 App 暴露两个公开量——它算出的
    // shared secret 与它的 ephemeral 公钥——然后：
    //   ① Rust 用固定私钥 + ① 的 ephemeral 公钥复算，比对 shared
    //      → 证明两端 ECDH 的 **x 坐标提取**一致（若一方返回 y 坐标或带前缀，
    //        这里立刻不等）
    //   ② 同一组 nonce/密文再走一遍 `open_body`
    //      → 证明 HKDF 的 salt/info、key 长度、AES-256-GCM 参数、AAD 处理全部一致
    //
    // 两个方向缺一不可：只验 ① 会漏掉 HKDF 侧偏差，只验 ② 会漏掉 ECDH 侧偏差。
    const V_PC_PUB: &str = "BFUPRxAD89+Xw99QaseX9nIfsaH7e49vg9IkSYplyI4kE2CT1wEuUJpzcVy9CwCjzA/0tcAbP/oZarH7MnA2uOY=";
    const V_EPH: &str = "BDJcM631zQKMrNluxNrAL8HTgpmDQx5Y9zgRmiSqW8gzW1DuBhvY34uhl8aAC1thfiieT9k2rX4BIIDMIzGGoCE=";
    const V_SHARED: &str = "aieIUCPrBa4fbDq2tORKT5FOEfVtSbK2pw9uh5D4f0A=";
    const V_NONCE: &str = "HAJ/fiHtZ1C1JQYP";
    const V_CT: &str = "KXZRoLqY5odzwUuVzGG/WUPYBKyEYHfgT9VjzAlW3CW5xw==";
    const V_FRAME_ID: &str = "ZnJhbWUtMDAwMQ==";
    const V_HEADER: &str = "eyJ2IjoyLCJ0eXBlIjoicm91dGUifQ==";
    const V_PLAIN: &str = "5oqKIEMg55uY5riF5LiA5LiL";

    fn b64d(s: &str) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(s).expect("valid base64")
    }

    fn b64d_fixed<const N: usize>(s: &str) -> [u8; N] {
        b64d(s).try_into().expect("base64 decodes to the expected length")
    }

    /// 基线先成立：Rust 从固定种子派生的公钥必须与 App 侧 dump 时导入的那个
    /// 是同一个。不成立说明 seed 或编码约定变了，后面的比对全部无意义。
    #[test]
    fn conformance_fixture_is_self_consistent() {
        let pc = Identity::from_seed(&[2u8; 32]);
        assert_eq!(pc.public_key().to_vec(), b64d(V_PC_PUB));
        assert_eq!(b64d(V_PC_PUB).len(), 65);
    }

    /// ① ECDH 跨端一致：Rust 的 shared secret 必须逐字节等于 WebCrypto 的。
    #[test]
    fn ecdh_shared_secret_matches_webcrypto() {
        let pc = Identity::from_seed(&[2u8; 32]);
        let ephemeral = PublicKey::from_sec1_bytes(&b64d_fixed::<65>(V_EPH)).expect("valid P-256 point");
        let shared = diffie_hellman(pc.secret.to_nonzero_scalar(), ephemeral.as_affine());
        assert_eq!(
            shared.raw_secret_bytes().to_vec(),
            b64d(V_SHARED),
            "ECDH shared secret differs from WebCrypto — check x-coordinate extraction"
        );
    }

    /// ② HKDF + AES-256-GCM + AAD 跨端一致：App 密封的密文，Rust 要能原样解开。
    #[test]
    fn app_ciphertext_opens_in_rust() {
        let pc = Identity::from_seed(&[2u8; 32]);
        let sealed = SealedBody {
            nonce: b64d_fixed::<NONCE_LEN>(V_NONCE),
            ciphertext: b64d(V_CT),
            ephemeral_public: b64d_fixed::<65>(V_EPH),
        };
        let ctx = FrameContext {
            frame_id: &b64d(V_FRAME_ID),
            header: &b64d(V_HEADER),
            direction: Direction::DeviceToNode,
        };
        let plain = open_body(&sealed, &pc.secret, &ctx)
            .expect("Rust must open what WebCrypto sealed — HKDF/AES/AAD diverged");
        assert_eq!(plain, b64d(V_PLAIN));
    }
}
