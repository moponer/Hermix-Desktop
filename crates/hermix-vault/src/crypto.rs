//! 加解密：AES-256-GCM（密码字段落盘加密）+ 主密钥生成。
//!
//! 密文布局：`nonce(12B) || ciphertext+tag`——nonce 随密文同存，无需额外状态。
//! 密钥来源见 `credman`（Windows Credential Manager 托管，不落盘明文）。

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use zeroize::Zeroizing;

use crate::VaultError;

/// 主密钥：Drop 时内存清零（zeroize），杜绝残留 core dump / 页面文件。
pub type MasterKey = Zeroizing<[u8; 32]>;

const NONCE_LEN: usize = 12;

/// 生成 32 字节主密钥（OS 熵源；getrandom 0.2 与 aes-gcm 0.10 同代，锁内已有）。
pub fn generate_master_key() -> Result<MasterKey, VaultError> {
    let mut key = Zeroizing::new([0u8; 32]);
    getrandom::getrandom(key.as_mut_slice()).map_err(|e| VaultError::Crypto(format!("熵源失败: {e}")))?;
    Ok(key)
}

/// 加密：返回 `nonce(12B) || ciphertext`。
pub fn encrypt(key: &MasterKey, plaintext: &str) -> Result<Vec<u8>, VaultError> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key.as_slice()));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes).map_err(|e| VaultError::Crypto(format!("熵源失败: {e}")))?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|_| VaultError::Crypto("AES-GCM 加密失败".into()))?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

/// 解密 `nonce(12B) || ciphertext`。
pub fn decrypt(key: &MasterKey, blob: &[u8]) -> Result<String, VaultError> {
    if blob.len() <= NONCE_LEN {
        return Err(VaultError::Crypto("密文过短（nonce 缺失）".into()));
    }
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key.as_slice()));
    let pt = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ct)
        .map_err(|_| VaultError::Crypto("AES-GCM 解密失败（密文或密钥不符）".into()))?;
    String::from_utf8(pt).map_err(|_| VaultError::Crypto("解密结果非 UTF-8".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tamper() {
        let key = generate_master_key().unwrap();
        let plain = "p@ssw0rd-中文-🔐"; // 含非 ASCII：验证 UTF-8 全链路
        let blob = encrypt(&key, plain).unwrap();
        assert_ne!(blob, plain.as_bytes());
        assert_eq!(decrypt(&key, &blob).unwrap(), plain);

        // 篡改任一密文字节必须失败（GCM tag 校验）——信任边界。
        let mut tampered = blob.clone();
        tampered[13] ^= 0xff;
        assert!(decrypt(&key, &tampered).is_err());

        // 错误密钥必须失败。
        let other = generate_master_key().unwrap();
        assert!(decrypt(&other, &blob).is_err());
    }
}
