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

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &[u8] = br#"{"v":2,"type":"route"}"#;

    /// 手机 → PC：密封用 PC 的静态公钥，PC 用自己的静态私钥解开。
    /// 这条是**基线**——下面那些 `is_err()` 断言只有在它成立时才有意义，
    /// 否则被测代码无条件失败会让它们全部假通过。
    #[test]
    fn d2n_roundtrip() {
        let phone = Identity::from_seed(&[1u8; 32]);
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
        let pc = Identity::from_seed(&[3u8; 32]);
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
        let phone = Identity::from_seed(&[5u8; 32]);
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
        let phone = Identity::from_seed(&[7u8; 32]);
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
        let phone = Identity::from_seed(&[9u8; 32]);
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
        let phone = Identity::from_seed(&[11u8; 32]);
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
        let phone = Identity::from_seed(&[13u8; 32]);
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
