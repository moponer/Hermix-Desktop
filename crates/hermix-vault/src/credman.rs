//! 主密钥托管：Windows Credential Manager（CredReadW/CredWriteW）。
//!
//! 决策笔记：主密钥**不落 `~/.hermix` 明文文件**；Credential Manager 在极少数
//! 精简系统缺失时如实报错（降级为口令派生需配套口令 UI，属后续独立工作）。

#[cfg(windows)]
use crate::VaultError;

/// CredMan 条目名（TargetName）。
#[cfg(windows)]
pub const MASTER_KEY_TARGET: &str = "HermixVault/MasterKey";

/// 读主密钥。`Ok(None)` = 尚未创建（ERROR_NOT_FOUND，首启正常路径）。
#[cfg(windows)]
pub fn read_master_key() -> Result<Option<Vec<u8>>, VaultError> {
    use windows_sys::Win32::Security::Credentials::{CredFree, CredReadW, CRED_TYPE_GENERIC};

    // str::encode_utf16 是固有方法，无需 OsStrExt trait import。
    let name_w: Vec<u16> = MASTER_KEY_TARGET
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut cred: *mut windows_sys::Win32::Security::Credentials::CREDENTIALW =
        std::ptr::null_mut();
    let ok = unsafe { CredReadW(name_w.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
    if ok == 0 {
        // 立刻取 last_os_error（后续任何调用都会覆盖它）。
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(1168) {
            // ERROR_NOT_FOUND：条目不存在 = 首启。
            return Ok(None);
        }
        return Err(VaultError::CredMan(format!("CredReadW 失败: {err}")));
    }
    // 密钥字节在 CredentialBlob（裸指针 + 长度），拷出后立刻 CredFree。
    let blob = unsafe {
        std::slice::from_raw_parts((*cred).CredentialBlob, (*cred).CredentialBlobSize as usize)
            .to_vec()
    };
    unsafe { CredFree(cred.cast()) };
    Ok(Some(blob))
}

/// 写主密钥（首启生成后调用；重复写为覆盖——更新密钥走 re-encrypt 流程）。
#[cfg(windows)]
pub fn write_master_key(key: &[u8]) -> Result<(), VaultError> {
    use windows_sys::Win32::Security::Credentials::{
        CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    let name_w: Vec<u16> = MASTER_KEY_TARGET
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let user_w: Vec<u16> = "Hermix"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let cred = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: name_w.as_ptr() as *mut u16,
        Comment: std::ptr::null_mut(),
        // 写入时 LastWritten 被系统忽略，置零即可。
        LastWritten: unsafe { std::mem::zeroed() },
        CredentialBlobSize: key.len() as u32,
        CredentialBlob: key.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: user_w.as_ptr() as *mut u16,
    };
    let ok = unsafe { CredWriteW(&cred, 0) };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        return Err(VaultError::CredMan(format!("CredWriteW 失败: {err}")));
    }
    Ok(())
}

/// 非 Windows：项目 Windows-only，编译占位（如实报错，永不静默成功）。
#[cfg(not(windows))]
pub fn read_master_key() -> Result<Option<Vec<u8>>, VaultError> {
    Err(VaultError::CredMan("仅支持 Windows（项目边界）".into()))
}

#[cfg(not(windows))]
pub fn write_master_key(_key: &[u8]) -> Result<(), VaultError> {
    Err(VaultError::CredMan("仅支持 Windows（项目边界）".into()))
}
