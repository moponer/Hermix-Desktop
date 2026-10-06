//! PC 侧 E2EE 静态身份：生成、DPAPI 保护、落盘、加载。
//!
//! 与 `apps/hm/src/core/crypto.ts`（App 侧）互为镜像。App 侧把静态私钥留在
//! WebCrypto 的 CryptoKey 里不落盘；PC 侧必须跨进程重启存活，所以要落盘。
//!
//! ## 为什么用 DPAPI 而不是明文
//!
//! `~/.hermix/` 下 `auth.json` / `approval_allowlist.json` / `.env` 全是明文，
//! 照抄最省事。但量级不同：token 泄露撤销即可；**静态私钥泄露则对方能冒充
//! 这台 PC 接收后续所有加密帧**——每帧 ephemeral 把历史范围限住了，但未来
//! 流量全归他，撤销不掉。
//!
//! DPAPI 是系统 API（`crypt32.dll`），零第三方密码学库；hermix 是
//! Windows-only（PR 禁区明确写了），不引入跨平台负担。DPAPI 把数据绑定到
//! **当前 Windows 用户账户**——别的账户 / 别的机器都解不开，这正是我们要的。
//!
//! ## 绝不静默回退
//!
//! DPAPI 失败（账户漫游配置异常、profile 损坏等）时**报错**，不降级成明文存。
//! 静默降级的后果是：用户以为存了密文，实际是裸私钥躺在磁盘上。

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::relay_crypto::Identity;

/// DPAPI 的可选熵。绑定协议标识，防止同一用户下别的应用拿 DPAPI 加密的
/// 数据被误当作本模块的密钥。
const DPAPI_ENTROPY: &[u8] = b"hermix/relay/e2ee/v1";

const IDENTITY_FILE: &str = "identity.json";

/// 落盘格式。`public_key` 明文（本来就是公开的，且配对要展示）；
/// `protected_key` 是 DPAPI 密文。
#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    version: u32,
    public_key: String,
    protected_key: String,
}

#[derive(Debug)]
pub enum IdentityError {
    Io(String),
    Crypto(String),
    Corrupt(String),
    /// DPAPI 拒绝保护/解保护。**不回退明文**——见模块文档。
    Dpapi(String),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityError::Io(e) => write!(f, "identity file io: {e}"),
            IdentityError::Crypto(e) => write!(f, "p256: {e}"),
            IdentityError::Corrupt(e) => write!(f, "identity file corrupt: {e}"),
            IdentityError::Dpapi(e) => write!(f, "DPAPI: {e}"),
        }
    }
}

impl std::error::Error for IdentityError {}

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// 加载本机身份；不存在则生成并落盘。
pub fn load_or_create() -> Result<Identity, IdentityError> {
    let path = identity_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => load_from(&raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Identity::generate();
            store(&id)?;
            Ok(id)
        }
        Err(e) => Err(IdentityError::Io(e.to_string())),
    }
}

/// 已落盘的身份。**不存在返回 `None`**——调用方据此决定是"首次配对"还是
/// "已配对过"。
pub fn load_existing() -> Result<Option<Identity>, IdentityError> {
    let path = identity_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => load_from(&raw).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(IdentityError::Io(e.to_string())),
    }
}

fn load_from(raw: &str) -> Result<Identity, IdentityError> {
    let stored: StoredIdentity = serde_json::from_str(raw).map_err(|e| IdentityError::Corrupt(e.to_string()))?;
    if stored.version != 1 {
        return Err(IdentityError::Corrupt(format!("unsupported version {}", stored.version)));
    }
    let protected = b64()
        .decode(stored.protected_key.as_bytes())
        .map_err(|e| IdentityError::Corrupt(format!("protected_key base64: {e}")))?;
    let plain = dpapi_unprotect(&protected)?;
    if plain.len() != 32 {
        return Err(IdentityError::Corrupt(format!(
            "private key is {} bytes, expected 32",
            plain.len()
        )));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&plain);
    let id = Identity::from_seed(&seed);
    // 明文公钥必须与解出来的私钥自洽——文件被手改过就当场发现，
    // 而不是等到配对时推导出错误的密钥对。
    if b64().encode(id.public_key()) != stored.public_key {
        return Err(IdentityError::Corrupt("public_key 与 private_key 不匹配".into()));
    }
    Ok(id)
}

fn store(id: &Identity) -> Result<(), IdentityError> {
    // P-256 私钥标量就是 32 字节标量。取它需要访问 SecretKey 的字节表示，
    // 走 `to_bytes()` 而不是内存扫描。
    let scalar = id.secret_bytes();
    let protected = dpapi_protect(&scalar)?;
    let stored = StoredIdentity {
        version: 1,
        public_key: b64().encode(id.public_key()),
        protected_key: b64().encode(&protected),
    };
    let path = identity_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| IdentityError::Io(e.to_string()))?;
    }
    let body = serde_json::to_string_pretty(&stored).map_err(|e| IdentityError::Io(e.to_string()))?;
    std::fs::write(&path, body).map_err(|e| IdentityError::Io(e.to_string()))
}

fn identity_path() -> std::path::PathBuf {
    hermix_config::paths::hermix_home().join(IDENTITY_FILE)
}

// ── 对端公钥库 ────────────────────────────────────────────────────

/// 已配对设备的静态公钥，按 device_id 索引。
///
/// 存在 `~/.hermix/peers.json` 而不是 `devices.db`：后者是 SQLite 且正被
/// 审计线改动，JSON 简单、gitignore 友好、也不跟并行开发抢同一张表。
#[derive(Serialize, Deserialize, Default)]
struct StoredPeers {
    #[serde(default)]
    peers: std::collections::BTreeMap<String, String>,
}

/// 对端公钥的读写。
pub struct PeerStore {
    path: std::path::PathBuf,
}

impl PeerStore {
    pub fn at_default() -> Self {
        Self {
            path: hermix_config::paths::hermix_home().join("peers.json"),
        }
    }

    /// 测试用：指定路径。
    pub fn at(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    pub fn get(&self, device_id: &str) -> Result<Option<[u8; 65]>, IdentityError> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Ok(None);
        };
        let stored: StoredPeers =
            serde_json::from_str(&raw).map_err(|e| IdentityError::Corrupt(e.to_string()))?;
        match stored.peers.get(device_id) {
            None => Ok(None),
            Some(encoded) => {
                let raw = b64()
                    .decode(encoded.as_bytes())
                    .map_err(|e| IdentityError::Corrupt(format!("peer key base64: {e}")))?;
                raw.try_into()
                    .map(Some)
                    .map_err(|_| IdentityError::Corrupt("peer key 不是 65 字节".into()))
            }
        }
    }

    /// 写入 / 覆盖一个对端公钥。
    pub fn put(&self, device_id: &str, public_key: &[u8; 65]) -> Result<(), IdentityError> {
        let mut stored: StoredPeers = match std::fs::read_to_string(&self.path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| IdentityError::Corrupt(e.to_string()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoredPeers::default(),
            Err(e) => return Err(IdentityError::Io(e.to_string())),
        };
        stored
            .peers
            .insert(device_id.to_string(), b64().encode(public_key));
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| IdentityError::Io(e.to_string()))?;
        }
        let body = serde_json::to_string_pretty(&stored).map_err(|e| IdentityError::Io(e.to_string()))?;
        std::fs::write(&self.path, body).map_err(|e| IdentityError::Io(e.to_string()))
    }

    /// 换绑 / 解绑：旧手机凭据立即失效，它的公钥也必须一起消失。
    pub fn remove(&self, device_id: &str) -> Result<(), IdentityError> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Ok(());
        };
        let mut stored: StoredPeers =
            serde_json::from_str(&raw).map_err(|e| IdentityError::Corrupt(e.to_string()))?;
        if stored.peers.remove(device_id).is_none() {
            return Ok(());
        }
        let body = serde_json::to_string_pretty(&stored).map_err(|e| IdentityError::Io(e.to_string()))?;
        std::fs::write(&self.path, body).map_err(|e| IdentityError::Io(e.to_string()))
    }
}

// ── DPAPI ────────────────────────────────────────────────────────

#[cfg(windows)]
fn dpapi_protect_with(plain: &[u8], entropy: &[u8]) -> Result<Vec<u8>, IdentityError> {
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    // SAFETY: 两个 blob 都指向本函数内活着的 Vec；pbData 在调用期间非空。
    unsafe {
        let input = blob(plain);
        let ent = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(
            &input,
            None,
            Some(&ent),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| IdentityError::Dpapi(format!("CryptProtectData: {e}")))?;
        // DPAPI 用 LocalAlloc 分配输出，必须 LocalFree 释放，否则每次调用泄一块。
        let copied = take_blob(&out);
        let _ = LocalFree(windows::Win32::Foundation::HLOCAL(out.pbData.cast()));
        Ok(copied)
    }
}

#[cfg(windows)]
fn dpapi_unprotect_with(cipher: &[u8], entropy: &[u8]) -> Result<Vec<u8>, IdentityError> {
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    unsafe {
        let input = blob(cipher);
        let ent = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(
            &input,
            None,
            Some(&ent),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| IdentityError::Dpapi(format!("CryptUnprotectData: {e}")))?;
        let copied = take_blob(&out);
        let _ = LocalFree(windows::Win32::Foundation::HLOCAL(out.pbData.cast()));
        Ok(copied)
    }
}

fn dpapi_protect(plain: &[u8]) -> Result<Vec<u8>, IdentityError> {
    dpapi_protect_with(plain, DPAPI_ENTROPY)
}

fn dpapi_unprotect(cipher: &[u8]) -> Result<Vec<u8>, IdentityError> {
    dpapi_unprotect_with(cipher, DPAPI_ENTROPY)
}

/// 非 Windows 编译不过——hermix 是 Windows-only 项目（PR 禁区），
/// 这里给的是清晰的编译错误而不是"运行时报不支持"。
#[cfg(not(windows))]
fn dpapi_protect(_plain: &[u8]) -> Result<Vec<u8>, IdentityError> {
    Err(IdentityError::Dpapi("DPAPI requires Windows".into()))
}

#[cfg(not(windows))]
fn dpapi_unprotect(_cipher: &[u8]) -> Result<Vec<u8>, IdentityError> {
    Err(IdentityError::Dpapi("DPAPI requires Windows".into()))
}

/// 构造一个指向 `data` 的 `CRYPT_INTEGER_BLOB`。
///
/// # Safety
/// 返回的 blob 持有 `data` 的裸指针，**必须与 `data` 同生命周期**。
#[cfg(windows)]
unsafe fn blob(data: &[u8]) -> windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
    windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
    }
}

/// 拷出 DPAPI 输出并清零原缓冲。DPAPI 的明文落在 LocalAlloc 的内存里，
/// 释放前先擦掉，避免留在被复用的堆页上。
#[cfg(windows)]
unsafe fn take_blob(out: &windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() || out.cbData == 0 {
        return Vec::new();
    }
    let copied = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
    // 在 DPAPI 的**原始**缓冲上按索引清零。不能用 `&u8 -> *mut u8` 的写法——
    // Rust 1.92 的 `invalid_reference_casting` 默认 deny，那是从共享引用
    // 造可变指针，属未定义行为。清零是为了不让私钥明文留在将被复用的堆页上。
    for i in 0..out.cbData as usize {
        std::ptr::write_volatile(out.pbData.add(i), 0);
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DPAPI 往返。这是**基线**：下面那些"必须失败"的断言只有在它成立时
    /// 才有意义，否则被测代码无条件报错会让它们假通过。
    #[test]
    fn dpapi_roundtrip() {
        let secret = [7u8; 32];
        let cipher = dpapi_protect(&secret).expect("DPAPI protect must succeed on Windows");
        assert_ne!(cipher, secret.to_vec(), "ciphertext must differ from plaintext");
        assert!(
            !cipher.windows(8).any(|w| w == &secret[..8]),
            "plaintext must not appear in ciphertext"
        );
        let back = dpapi_unprotect(&cipher).expect("DPAPI unprotect must succeed");
        assert_eq!(back, secret.to_vec());
    }

    #[test]
    fn wrong_entropy_fails() {
        // 熵不对 → 解不开。防止同一用户下别的应用拿 DPAPI 密文冒充本模块密钥。
        let cipher = dpapi_protect(&[9u8; 32]).expect("protect");
        let mut bad = DPAPI_ENTROPY.to_vec();
        bad[0] ^= 0xff;
        assert!(
            dpapi_unprotect_with(&cipher, &bad).is_err(),
            "entropy mismatch must not decrypt"
        );
        // 正确熵仍要能解开——证明上面失败不是因为密文本身坏了。
        assert!(dpapi_unprotect_with(&cipher, DPAPI_ENTROPY).is_ok());
    }

    #[test]
    fn corrupt_ciphertext_fails() {
        let mut cipher = dpapi_protect(&[3u8; 32]).expect("protect");
        let n = cipher.len();
        cipher[n / 2] ^= 0xff;
        assert!(dpapi_unprotect(&cipher).is_err());
    }

    #[test]
    fn load_rejects_tampered_public_key() {
        // 公钥与私钥不匹配 = 文件被手改过，必须当场报错而不是推导出错误密钥对。
        let raw = serde_json::to_string(&StoredIdentity {
            version: 1,
            public_key: b64().encode([0u8; 65]),
            protected_key: b64().encode(dpapi_protect(&[5u8; 32]).unwrap()),
        })
        .unwrap();
        assert!(matches!(load_from(&raw), Err(IdentityError::Corrupt(_))));
    }

    #[test]
    fn load_rejects_unknown_version() {
        let raw = serde_json::to_string(&StoredIdentity {
            version: 99,
            public_key: String::new(),
            protected_key: String::new(),
        })
        .unwrap();
        assert!(matches!(load_from(&raw), Err(IdentityError::Corrupt(_))));
    }
}
