//! 账户沙箱：内置 Administrator（RID 500）机器上的正解。
//!
//! ## 为什么需要它
//!
//! `sandbox.rs` 的受限令牌沙箱依赖 `CreateRestrictedToken` 派生令牌的
//! 「自助指派豁免」（派生自调用方自己的令牌即可免 SeAssignPrimaryTokenPrivilege）
//! —— 该豁免对**普通用户**生效，对 **UAC 过滤后的内置管理员**恒报 1314
//! （2026-11-19 实测：四种 `CreateRestrictedToken` 标志组合全部一样；
//! `AdjustTokenPrivileges` 启用特权也无效，UAC 过滤已把特权从令牌剥掉）。
//! AppContainer 同样不可用（`CreateAppContainerProfile` 对内置管理员返回
//! E_ACCESSDENIED）。
//!
//! ## 机制（对齐 Codex CLI windows-sandbox-rs 的核心模型）
//!
//! 1. **setup（提权一次，`ShellExecuteExW("runas")`）**：`NetUserAdd` 建
//!    `HermixSandboxOffline` / `HermixSandboxOnline` 两个本地账户；WFP
//!    过滤器按 offline 账户的 SID 拦全部出/入站流量（持久）；密码用 DPAPI
//!    **机器域**加密后写 `<hermix_home>/sandbox/secrets.json`；登录界面隐藏。
//! 2. **运行时（非提权）**：`CreateProcessWithLogonW(账户, 密码)` 起进程
//!    —— 它自己完成登录，不需要 SeAssignPrimaryToken（MSDN 明确的非特权
//!    替代；Codex 的 runner_client.rs 同款）。
//! 3. **联网按需**：offline/online 的 SID 不重叠，WFP 只挂 offline —— 需要
//!    网络的命令换 online 账户跑即放行（Codex 的 SandboxNetworkIdentity
//!    同构）。开关 = `HERMIX_SANDBOX_NETWORK=on`（审批层的接缝）。
//! 4. **文件隔离**：独立账户的默认 ACL 天然读不到其他用户的 profile、写不进
//!    任何目录；工作区由我们**追加**账户 SID 的 Modify ACE（用户对自有目录
//!    有 WRITE_DAC，免提权）。
//!
//! 与 Codex 的差异：无 runner 二进制 / 无具名管道 IPC / 无能力 SID 体系
//! —— 桌面应用按「账户即隔离边界」直起命令；进程树收容沿用 JobGuard。

use serde::{Deserialize, Serialize};

use hermix_core::AgentError;

pub const OFFLINE_USERNAME: &str = "HermixSandboxOffline";
pub const ONLINE_USERNAME: &str = "HermixSandboxOnline";
pub const SETUP_VERSION: u32 = 1;
/// sidecar 的隐藏子命令：提权进程入口（payload 为 base64 JSON）。
pub const SETUP_FLAG: &str = "--hermix-sandbox-setup-exec";

/// 默认断网（offline）；`HERMIX_SANDBOX_NETWORK=on` 时 online。
/// 开关的决策方是审批层——「按需授权」的接缝。
fn network_on() -> bool {
    matches!(
        std::env::var("HERMIX_SANDBOX_NETWORK").ok().as_deref().map(str::trim),
        Some("on") | Some("1") | Some("true")
    )
}

// ---------------------------------------------------------------------------
// 凭据与标记
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct SandboxUserRecord {
    username: String,
    sid: String,
    /// DPAPI(机器域) 密文，base64。
    password: String,
}

#[derive(Serialize, Deserialize)]
struct SecretsFile {
    version: u32,
    offline: SandboxUserRecord,
    online: SandboxUserRecord,
}

#[derive(Serialize, Deserialize)]
struct SetupMarker {
    version: u32,
}

#[derive(Serialize, Deserialize)]
struct SetupPayload {
    version: u32,
    offline_username: String,
    online_username: String,
    hermix_home: String,
    real_user: String,
}

#[derive(Serialize, Deserialize)]
struct SetupErrorReport {
    message: String,
}

#[derive(Debug, Clone)]
pub struct SandboxAccount {
    pub username: String,
    pub sid: String,
    pub password: String,
}

fn secrets_dir() -> std::path::PathBuf {
    hermix_config::paths::hermix_home().join("sandbox")
}

fn secrets_path() -> std::path::PathBuf {
    secrets_dir().join("secrets.json")
}

fn marker_path() -> std::path::PathBuf {
    secrets_dir().join("setup_marker.json")
}

fn setup_error_path() -> std::path::PathBuf {
    secrets_dir().join("setup_error.json")
}

/// setup 是否已完成（版本匹配）。
pub fn setup_is_complete() -> bool {
    let Ok(text) = std::fs::read_to_string(marker_path()) else {
        return false;
    };
    serde_json::from_str::<SetupMarker>(&text)
        .map(|m| m.version == SETUP_VERSION)
        .unwrap_or(false)
}

/// 读出两个沙箱账户（DPAPI 解密）。setup 未完成或解密失败 = Err。
pub fn load_accounts() -> Result<(SandboxAccount, SandboxAccount), AgentError> {
    let text = std::fs::read_to_string(secrets_path()).map_err(|e| {
        AgentError::Io(format!("sandbox secrets missing (setup not complete?): {e}"))
    })?;
    let file: SecretsFile =
        serde_json::from_str(&text).map_err(|e| AgentError::Io(format!("secrets json: {e}")))?;
    if file.version != SETUP_VERSION {
        return Err(AgentError::Io(format!(
            "sandbox secrets version {} != {SETUP_VERSION} — re-run setup",
            file.version
        )));
    }
    Ok((decode_record(&file.offline)?, decode_record(&file.online)?))
}

fn decode_record(rec: &SandboxUserRecord) -> Result<SandboxAccount, AgentError> {
    let blob = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &rec.password)
        .map_err(|e| AgentError::Io(format!("secrets base64: {e}")))?;
    let plain = dpapi_unprotect(&blob)?;
    Ok(SandboxAccount {
        username: rec.username.clone(),
        sid: rec.sid.clone(),
        password: String::from_utf8(plain)
            .map_err(|_| AgentError::Io("sandbox password not utf-8".into()))?,
    })
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn guid(v: u128) -> windows_sys::core::GUID {
    windows_sys::core::GUID::from_u128(v)
}

// ---------------------------------------------------------------------------
// 非提权侧：请求 setup（UAC）
// ---------------------------------------------------------------------------

/// 触发一次提权 setup（UAC；ConsentPromptBehaviorAdmin=0 的机器上静默），
/// 等待完成并校验 marker。被拒绝/失败时返回带说明的 Err。
pub fn request_setup() -> Result<(), AgentError> {
    if setup_is_complete() {
        return Ok(());
    }
    let payload = SetupPayload {
        version: SETUP_VERSION,
        offline_username: OFFLINE_USERNAME.into(),
        online_username: ONLINE_USERNAME.into(),
        hermix_home: hermix_config::paths::hermix_home()
            .to_string_lossy()
            .into_owned(),
        real_user: std::env::var("USERNAME").unwrap_or_default(),
    };
    let json = serde_json::to_string(&payload)
        .map_err(|e| AgentError::Io(format!("setup payload: {e}")))?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, json.as_bytes());

    let exe = std::env::current_exe().map_err(|e| AgentError::Io(format!("current_exe: {e}")))?;
    let code = shell_execute_elevated(&exe, &format!("{SETUP_FLAG} {b64}"))?;
    if code != 0 {
        let detail = std::fs::read_to_string(setup_error_path())
            .ok()
            .and_then(|t| serde_json::from_str::<SetupErrorReport>(&t).ok())
            .map(|r| r.message)
            .unwrap_or_else(|| format!("exit code {code}"));
        return Err(AgentError::Io(format!("沙箱账户 setup 失败：{detail}")));
    }
    if !setup_is_complete() {
        return Err(AgentError::Io("sandbox setup 完成但 marker 缺失".into()));
    }
    Ok(())
}

/// `ShellExecuteExW(verb="runas")` 提权启动 exe 并等待退出。UAC 拒绝 = 1223。
#[cfg(windows)]
fn shell_execute_elevated(exe: &std::path::Path, params: &str) -> Result<u32, AgentError> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    let exe_w = to_wide(&exe.to_string_lossy());
    let params_w = to_wide(params);
    let verb_w = to_wide("runas");
    unsafe {
        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        // NOASYNC：Tokio worker 无消息循环，必须同步激活；
        // NOCLOSEPROCESS：要拿 hProcess 等退出码。
        sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        sei.lpVerb = verb_w.as_ptr();
        sei.lpFile = exe_w.as_ptr();
        sei.lpParameters = params_w.as_ptr();
        sei.nShow = 0; // SW_HIDE
        if ShellExecuteExW(&mut sei) == 0 {
            let err = GetLastError();
            return Err(AgentError::Io(format!(
                "提权启动失败（Win32 error {err}）：{}",
                if err == 1223 { "管理员授权被取消" } else { "UAC 启动错误" }
            )));
        }
        let h = sei.hProcess;
        WaitForSingleObject(h, 10 * 60 * 1000);
        let mut code: u32 = 1;
        GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        Ok(code)
    }
}

/// 提权进程入口：sidecar main 检测到 SETUP_FLAG 时调用并按返回值退出。
/// None = 参数不是本 flag（正常启动路径）。
pub fn elevated_entry_from_args() -> Option<i32> {
    let mut args = std::env::args();
    let _exe = args.next()?;
    let flag = args.next()?;
    if flag != SETUP_FLAG {
        return None;
    }
    let payload_b64 = args.next().unwrap_or_default();
    Some(run_elevated_setup(&payload_b64))
}

fn run_elevated_setup(payload_b64: &str) -> i32 {
    let _ = std::fs::create_dir_all(secrets_dir());
    let result = decode_payload(payload_b64).and_then(|p| {
        if p.version != SETUP_VERSION {
            return Err(AgentError::Io(format!("setup version {} mismatch", p.version)));
        }
        provision(&p)
    });
    match result {
        Ok(()) => {
            let marker = serde_json::to_string(&SetupMarker { version: SETUP_VERSION })
                .unwrap_or_default();
            let _ = std::fs::write(marker_path(), marker);
            0
        }
        Err(e) => {
            let report =
                serde_json::to_string(&SetupErrorReport { message: e.to_string() })
                    .unwrap_or_default();
            let _ = std::fs::write(setup_error_path(), report);
            1
        }
    }
}

fn decode_payload(b64: &str) -> Result<SetupPayload, AgentError> {
    let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64.trim())
        .map_err(|e| AgentError::Io(format!("setup payload base64: {e}")))?;
    serde_json::from_slice(&raw).map_err(|e| AgentError::Io(format!("setup payload json: {e}")))
}

// ---------------------------------------------------------------------------
// 提权侧：provision（账户 + 隐藏 + WFP + secrets）
// ---------------------------------------------------------------------------

fn provision(payload: &SetupPayload) -> Result<(), AgentError> {
    let offline_pwd = random_password();
    let online_pwd = random_password();
    ensure_local_user(&payload.offline_username, &offline_pwd)?;
    ensure_local_user(&payload.online_username, &online_pwd)?;
    hide_from_welcome_screen(&[&payload.offline_username, &payload.online_username])?;
    install_wfp_filters_for_account(&payload.offline_username)?;

    let users = SecretsFile {
        version: SETUP_VERSION,
        offline: SandboxUserRecord {
            username: payload.offline_username.clone(),
            sid: account_sid_string(&payload.offline_username)?,
            password: dpapi_protect(offline_pwd.as_bytes())?,
        },
        online: SandboxUserRecord {
            username: payload.online_username.clone(),
            sid: account_sid_string(&payload.online_username)?,
            password: dpapi_protect(online_pwd.as_bytes())?,
        },
    };
    std::fs::create_dir_all(secrets_dir())
        .map_err(|e| AgentError::Io(format!("secrets dir: {e}")))?;
    std::fs::write(
        secrets_path(),
        serde_json::to_string(&users).map_err(|e| AgentError::Io(format!("secrets json: {e}")))?,
    )
    .map_err(|e| AgentError::Io(format!("secrets write: {e}")))?;
    Ok(())
}

/// `NetUserAdd`（幂等：已存在时只重置密码——企业策略可能接管了账户标志，
/// Codex 同款 recovery 语义）。
fn ensure_local_user(name: &str, password: &str) -> Result<(), AgentError> {
    use windows_sys::Win32::NetworkManagement::NetManagement::{
        NetUserAdd, NetUserSetInfo, UF_DONT_EXPIRE_PASSWD, UF_SCRIPT, USER_INFO_1,
        USER_INFO_1003, USER_PRIV_USER,
    };
    let name_w = to_wide(name);
    let pwd_w = to_wide(password);
    unsafe {
        let info = USER_INFO_1 {
            usri1_name: name_w.as_ptr() as *mut u16,
            usri1_password: pwd_w.as_ptr() as *mut u16,
            usri1_password_age: 0,
            usri1_priv: USER_PRIV_USER,
            usri1_home_dir: std::ptr::null_mut(),
            usri1_comment: std::ptr::null_mut(),
            usri1_flags: UF_SCRIPT | UF_DONT_EXPIRE_PASSWD,
            usri1_script_path: std::ptr::null_mut(),
        };
        let status =
            NetUserAdd(std::ptr::null(), 1, &info as *const _ as *const u8, std::ptr::null_mut());
        if status != 0 {
            // 已存在：只重置密码（level 1003），不动其他标志。
            let pw = USER_INFO_1003 { usri1003_password: pwd_w.as_ptr() as *mut u16 };
            NetUserSetInfo(
                std::ptr::null(),
                name_w.as_ptr(),
                1003,
                &pw as *const _ as *const u8,
                std::ptr::null_mut(),
            );
        }
    }
    Ok(())
}

/// 从登录界面隐藏（Winlogon UserList DWORD=0；「用户/组」列表无官方隐藏机制）。
fn hide_from_welcome_screen(usernames: &[&str]) -> Result<(), AgentError> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE,
        REG_DWORD, REG_OPTION_NON_VOLATILE,
    };
    const USERLIST: &str =
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\SpecialAccounts\UserList";
    let key_path = to_wide(USERLIST);
    unsafe {
        let mut key: HKEY = 0;
        let rc = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            key_path.as_ptr(),
            0,
            std::ptr::null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        );
        if rc != 0 {
            return Err(AgentError::Io(format!("UserList key: Win32 error {rc}")));
        }
        for u in usernames {
            let name = to_wide(u);
            let zero: u32 = 0;
            RegSetValueExW(key, name.as_ptr(), 0, REG_DWORD, &zero as *const u32 as *const u8, 4);
        }
        RegCloseKey(key);
    }
    Ok(())
}

/// 账户 SID 字符串（LookupAccountNameW → ConvertSidToStringSidW）。
fn account_sid_string(name: &str) -> Result<String, AgentError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::LookupAccountNameW;
    let name_w = to_wide(name);
    unsafe {
        let mut cb_sid = 0u32;
        let mut domain_len = 0u32;
        let mut use_type: i32 = 0;
        // 两次调用：先探长度再取 SID。
        LookupAccountNameW(
            std::ptr::null(),
            name_w.as_ptr(),
            std::ptr::null_mut(),
            &mut cb_sid,
            std::ptr::null_mut(),
            &mut domain_len,
            &mut use_type,
        );
        let mut sid = vec![0u8; cb_sid as usize];
        let mut domain = vec![0u16; domain_len as usize + 1];
        if LookupAccountNameW(
            std::ptr::null(),
            name_w.as_ptr(),
            sid.as_mut_ptr() as *mut std::ffi::c_void,
            &mut cb_sid,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut use_type,
        ) == 0
        {
            return Err(AgentError::Io(format!("LookupAccountNameW({name}) failed")));
        }
        let mut sid_str: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid.as_mut_ptr() as *mut std::ffi::c_void, &mut sid_str) == 0 {
            return Err(AgentError::Io("ConvertSidToStringSidW failed".into()));
        }
        let mut len = 0usize;
        while *sid_str.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
        LocalFree(sid_str as *mut std::ffi::c_void);
        Ok(s)
    }
}

// ---------------------------------------------------------------------------
// WFP：offline 账户断网（4 条持久过滤器，幂等）
// ---------------------------------------------------------------------------

/// 本项目自己的 WFP 命名空间 GUID。**不要**重新生成——会孤儿化旧的持久
/// provider/sublayer（Codex 注释同款纪律）。
const WFP_PROVIDER_KEY: u128 = 0x9c4d_2f6a_1b3e_4d57_8a92_c4e1_f0b6_d375;
const WFP_SUBLAYER_KEY: u128 = 0x6f3a_91d8_2e47_4c0b_b15a_d9e8_f3a2_c614;
/// 4 条过滤器 key（AUTH_CONNECT / RECV_ACCEPT × V4/V6）。
const WFP_FILTER_KEYS: [u128; 4] = [
    0x2b8e_5c7f_9a14_4d63_8f26_e0d3_b5a9_7c81,
    0x71c3_a9e5_4f82_4b1d_9d6a_3e8c_1f5b_2d40,
    0xd4a1_7b3c_6e29_4f85_b3d1_9c7f_2a5e_08b3,
    0x8e2d_4f6a_c7b1_49d3_a5e8_1f6c_3b9d_0a72,
];

/// accctrl.h / fwptypes.h 的 FWP_ACTRL_MATCH_FILTER（windows-sys 未导出）。
const FWP_ACTRL_MATCH_FILTER: u32 = 0x0000_000C;

fn install_wfp_filters_for_account(account: &str) -> Result<(), AgentError> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::{
        FwpmEngineOpen0, FwpmFilterAdd0, FwpmFilterDeleteByKey0, FwpmProviderAdd0,
        FwpmSubLayerAdd0, FwpmTransactionAbort0,
        FwpmTransactionBegin0, FwpmTransactionCommit0, FWPM_FILTER0,
        FWPM_FILTER_CONDITION0, FWPM_LAYER_ALE_AUTH_CONNECT_V4,
        FWPM_LAYER_ALE_AUTH_CONNECT_V6, FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4,
        FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6, FWPM_PROVIDER0, FWPM_PROVIDER_FLAG_PERSISTENT,
        FWPM_SESSION0, FWPM_SUBLAYER0, FWPM_SUBLAYER_FLAG_PERSISTENT, FWP_ACTION_BLOCK,
        FWP_MATCH_EQUAL, FWP_SECURITY_DESCRIPTOR_TYPE, FWPM_CONDITION_ALE_USER_ID,
        FWP_BYTE_BLOB,
    };
    use windows_sys::Win32::Foundation::FWP_E_ALREADY_EXISTS;
    use windows_sys::Win32::Security::Authorization::{
        BuildExplicitAccessWithNameW, BuildSecurityDescriptorW, GRANT_ACCESS,
    };
    const RPC_C_AUTHN_DEFAULT: u32 = 0xFFFF_FFFF;

    unsafe {
        let session_name = to_wide("Hermix sandbox WFP");
        let mut session: FWPM_SESSION0 = std::mem::zeroed();
        session.displayData.name = session_name.as_ptr() as *mut u16;
        session.txnWaitTimeoutInMSec = 60_000;
        let mut engine: HANDLE = 0; // HKEY/HANDLE 在 windows-sys 0.52 是 isize 别名
        let rc = FwpmEngineOpen0(
            std::ptr::null(),
            RPC_C_AUTHN_DEFAULT,
            std::ptr::null_mut(),
            &session,
            &mut engine,
        );
        if rc != 0 {
            return Err(AgentError::Io(format!("FwpmEngineOpen0: 0x{rc:08X}")));
        }
        let result = (|| -> Result<(), AgentError> {
            let rc = FwpmTransactionBegin0(engine, 0);
            if rc != 0 {
                return Err(AgentError::Io(format!("FwpmTransactionBegin0: 0x{rc:08X}")));
            }
            let provider_key = guid(WFP_PROVIDER_KEY);
            let provider_name = to_wide("Hermix sandbox WFP");
            let mut provider: FWPM_PROVIDER0 = std::mem::zeroed();
            provider.providerKey = provider_key;
            provider.displayData.name = provider_name.as_ptr() as *mut u16;
            provider.flags = FWPM_PROVIDER_FLAG_PERSISTENT;
            let rc = FwpmProviderAdd0(engine, &provider, std::ptr::null_mut());
            if rc != 0 && rc as i32 != FWP_E_ALREADY_EXISTS {
                return Err(AgentError::Io(format!("FwpmProviderAdd0: 0x{rc:08X}")));
            }
            let sublayer_key = guid(WFP_SUBLAYER_KEY);
            let sublayer_name = to_wide("Hermix sandbox WFP");
            let mut sublayer: FWPM_SUBLAYER0 = std::mem::zeroed();
            sublayer.subLayerKey = sublayer_key;
            sublayer.displayData.name = sublayer_name.as_ptr() as *mut u16;
            sublayer.flags = FWPM_SUBLAYER_FLAG_PERSISTENT;
            sublayer.providerKey = &provider_key as *const _ as *mut _;
            sublayer.weight = 0x8000;
            let rc = FwpmSubLayerAdd0(engine, &sublayer, std::ptr::null_mut());
            if rc != 0 && rc as i32 != FWP_E_ALREADY_EXISTS {
                return Err(AgentError::Io(format!("FwpmSubLayerAdd0: 0x{rc:08X}")));
            }
            // 用户条件：一个「匹配该账户」的安全描述符（BuildSecurityDescriptorW
            // 产出 FWP_SECURITY_DESCRIPTOR_TYPE 需要的 SD blob）。
            let account_w = to_wide(account);
            let mut ea: windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W =
                std::mem::zeroed();
            BuildExplicitAccessWithNameW(
                &mut ea,
                account_w.as_ptr() as *mut u16,
                FWP_ACTRL_MATCH_FILTER,
                GRANT_ACCESS,
                0,
            );
            let mut sd: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut sd_len = 0u32;
            let rc = BuildSecurityDescriptorW(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                1,
                &ea,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd_len,
                &mut sd,
            );
            if rc != 0 {
                return Err(AgentError::Io(format!("BuildSecurityDescriptorW: {rc}")));
            }
            let blob = windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::FWP_BYTE_BLOB {
                size: sd_len,
                data: sd as *mut u8,
            };
            let layers = [
                FWPM_LAYER_ALE_AUTH_CONNECT_V4,
                FWPM_LAYER_ALE_AUTH_CONNECT_V6,
                FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4,
                FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6,
            ];
            for (i, layer) in layers.iter().enumerate() {
                let key = guid(WFP_FILTER_KEYS[i]);
                // 幂等：先删旧（不存在不算错）。
                let _ = FwpmFilterDeleteByKey0(engine, &key);
                let mut condition: FWPM_FILTER_CONDITION0 = std::mem::zeroed();
                condition.fieldKey = FWPM_CONDITION_ALE_USER_ID;
                condition.matchType = FWP_MATCH_EQUAL;
                condition.conditionValue.r#type = FWP_SECURITY_DESCRIPTOR_TYPE;
                condition.conditionValue.Anonymous.sd = &blob as *const FWP_BYTE_BLOB as *mut FWP_BYTE_BLOB;
                let filter_name = to_wide("Hermix sandbox offline block");
                let mut filter: FWPM_FILTER0 = std::mem::zeroed();
                filter.filterKey = key;
                filter.displayData.name = filter_name.as_ptr() as *mut u16;
                filter.layerKey = *layer;
                filter.subLayerKey = sublayer_key;
                filter.numFilterConditions = 1;
                filter.filterCondition = &mut condition;
                filter.action.r#type = FWP_ACTION_BLOCK;
                let mut filter_id = 0u64;
                let rc = FwpmFilterAdd0(engine, &filter, std::ptr::null_mut(), &mut filter_id);
                if rc != 0 {
                    return Err(AgentError::Io(format!("FwpmFilterAdd0: 0x{rc:08X}")));
                }
            }
            let rc = FwpmTransactionCommit0(engine);
            if rc != 0 {
                return Err(AgentError::Io(format!("FwpmTransactionCommit0: 0x{rc:08X}")));
            }
            Ok(())
        })();
        if let Err(e) = result {
            let _ = FwpmTransactionAbort0(engine);
            return Err(e);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DPAPI（机器域）：elevated 写入 / 非提权读取
// ---------------------------------------------------------------------------

const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
const CRYPTPROTECT_LOCAL_MACHINE: u32 = 0x4;

fn dpapi_protect(data: &[u8]) -> Result<String, AgentError> {
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptProtectData(
            &mut input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
            &mut out,
        );
        if ok == 0 {
            return Err(AgentError::Io("CryptProtectData failed".into()));
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        windows_sys::Win32::Foundation::LocalFree(out.pbData as *mut std::ffi::c_void);
        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            bytes,
        ))
    }
}

fn dpapi_unprotect(blob: &[u8]) -> Result<Vec<u8>, AgentError> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: blob.len() as u32,
            pbData: blob.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
            &mut out,
        );
        if ok == 0 {
            return Err(AgentError::Io("CryptUnprotectData failed".into()));
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        windows_sys::Win32::Foundation::LocalFree(out.pbData as *mut std::ffi::c_void);
        Ok(bytes)
    }
}

// ---------------------------------------------------------------------------
// 密码生成：24 字符，四类各一 + 全表填充（保证过任何密码策略）
// ---------------------------------------------------------------------------

fn random_password() -> String {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    const CLASSES: [&[u8]; 4] = [
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        b"abcdefghijklmnopqrstuvwxyz",
        b"0123456789",
        b"!@#$%^&*()-_=+",
    ];
    let mut raw = [0u8; 24];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            raw.as_mut_ptr(),
            raw.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        // BCrypt 失败极罕见；退回时间熵（setup 在 UAC 提权进程里，panic 不可见）。
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        for (i, b) in raw.iter_mut().enumerate() {
            *b = ((t >> ((i % 4) * 8)) as u8).wrapping_mul(31).wrapping_add(i as u8);
        }
    }
    let alphabet: Vec<u8> = CLASSES.concat();
    let mut out = String::with_capacity(24);
    for (i, class) in CLASSES.iter().enumerate() {
        out.push(class[raw[i] as usize % class.len()] as char);
    }
    for b in &raw[CLASSES.len()..] {
        out.push(alphabet[*b as usize % alphabet.len()] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// 端到端入口：选账户 + 授权工作区 + 起进程
// ---------------------------------------------------------------------------

/// 选网络模式对应的账户。secrets 缺失 = Err（setup 的自动触发在调用方
/// `local.rs`：只有确认当前 exe 能处理 SETUP_FLAG 才发 UAC，避免测试
/// 二进制把自己提权重跑一遍）。
pub fn pick_account() -> Result<SandboxAccount, AgentError> {
    let (offline, online) = load_accounts()?;
    Ok(if network_on() { online } else { offline })
}

/// 当前进程能否安全地自触发提权 setup：只有真正的 sidecar / 桌面 exe
/// 认识 SETUP_FLAG。测试二进制（hermix_environments-…）不行——它会把
/// 自己提权重跑一遍测试。生产路径（hermix-server.exe sidecar）= true。
pub fn can_request_setup() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .map(|stem| {
            stem == "hermix"
                || stem.starts_with("hermix-server")
                || stem.starts_with("hermix-desktop")
        })
        .unwrap_or(false)
}

/// 给沙箱账户追加工作区 Modify ACE。幂等：hermix_home 下的 grant 标记。
/// 用户对自有目录有 WRITE_DAC → 免提权。
pub fn grant_workspace_write(
    workspace: &std::path::Path,
    account: &SandboxAccount,
) -> Result<(), AgentError> {
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW,
        SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT,
        TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
    };
    let mut marker = secrets_dir();
    marker.push("grants");
    let digest = sha2_hex(format!("{}|{}", workspace.display(), account.sid).as_bytes());
    let _ = std::fs::create_dir_all(&marker);
    marker.push(format!("{digest}.grant"));
    if marker.exists() {
        return Ok(());
    }
    let ws_w = to_wide(&workspace.to_string_lossy());
    let sid_w = to_wide(&account.sid);
    unsafe {
        let mut sid_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        if ConvertStringSidToSidW(sid_w.as_ptr(), &mut sid_ptr) == 0 || sid_ptr.is_null() {
            return Err(AgentError::Io("ConvertStringSidToSidW failed".into()));
        }
        let mut old_dacl: *mut windows_sys::Win32::Security::ACL = std::ptr::null_mut();
        let mut sd: *mut std::ffi::c_void = std::ptr::null_mut();
        let rc = GetNamedSecurityInfoW(
            ws_w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_dacl,
            std::ptr::null_mut(),
            &mut sd,
        );
        if rc != 0 {
            return Err(AgentError::Io(format!("GetNamedSecurityInfoW: {rc}")));
        }
        // 安全描述符是一次分配（内部含 DACL），用完必须整体释放。
        // Modify = FILE_GENERIC_READ|WRITE|EXECUTE|DELETE（资源管理器「修改」同款掩码）。
        const MODIFY_MASK: u32 = 0x0013_01FF;
        let mut ea: EXPLICIT_ACCESS_W = std::mem::zeroed();
        ea.grfAccessPermissions = MODIFY_MASK;
        ea.grfAccessMode = GRANT_ACCESS;
        ea.grfInheritance = 0x3; // CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
        ea.Trustee.pMultipleTrustee = std::ptr::null_mut();
        ea.Trustee.MultipleTrusteeOperation = 0;
        ea.Trustee.TrusteeForm = TRUSTEE_IS_SID;
        ea.Trustee.TrusteeType = TRUSTEE_IS_UNKNOWN;
        ea.Trustee.ptstrName = sid_ptr as *mut u16;
        let mut new_dacl: *mut windows_sys::Win32::Security::ACL = std::ptr::null_mut();
        let rc = SetEntriesInAclW(1, &ea, old_dacl, &mut new_dacl);
        if rc != 0 {
            return Err(AgentError::Io(format!("SetEntriesInAclW: {rc}")));
        }
        let rc = SetNamedSecurityInfoW(
            ws_w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null_mut(),
        );
        if rc != 0 {
            windows_sys::Win32::Foundation::LocalFree(sd);
            return Err(AgentError::Io(format!("SetNamedSecurityInfoW: {rc}")));
        }
        windows_sys::Win32::Foundation::LocalFree(sd);
    }
    let _ = std::fs::write(&marker, account.sid.as_bytes());
    Ok(())
}

/// 在账户沙箱里跑一条 PowerShell 命令的完整路径：账户缺失时触发 setup，
/// 授权工作区，然后 `CreateProcessWithLogonW` 起进程。任何**基础设施**
/// 失败都返回 Err（调用方据此降级）；命令自身的失败由 SandboxChild 收集。
pub fn run_command_in_account_sandbox(
    command: &str,
    workspace: &std::path::Path,
) -> Result<crate::sandbox::SandboxChild, AgentError> {
    let account = pick_account()?;
    grant_workspace_write(workspace, &account)?;
    let temp_dir = workspace.join(".hermix").join("tmp");
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| AgentError::Io(format!("sandbox temp dir: {e}")))?;
    let ps = std::env::var("SystemRoot")
        .map(|root| {
            std::path::PathBuf::from(root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("powershell.exe"));
    spawn_as_account(
        &account,
        &ps.to_string_lossy(),
        &[
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            command.to_string(),
        ],
        workspace,
        &temp_dir,
    )
}

/// `CreateProcessWithLogonW` + 管道（stdin = NUL 设备，stdout/stderr = 可继承
/// 写端）。不需要 SeAssignPrimaryToken——它自己做登录。
pub(crate) fn spawn_as_account(
    account: &SandboxAccount,
    exe: &str,
    args: &[String],
    cwd: &std::path::Path,
    temp_dir: &std::path::Path,
) -> Result<crate::sandbox::SandboxChild, AgentError> {
    spawn_as_account_ex(account, exe, args, cwd, temp_dir, true, true, true)
}

/// 诊断变体：no_window / with_env / with_nul_stdin 逐项开关。
pub(crate) fn spawn_as_account_ex(
    account: &SandboxAccount,
    exe: &str,
    args: &[String],
    cwd: &std::path::Path,
    temp_dir: &std::path::Path,
    no_window: bool,
    with_env: bool,
    _with_nul_stdin: bool,
) -> Result<crate::sandbox::SandboxChild, AgentError> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Threading::{
        CreateProcessWithLogonW, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
        PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
    };

    let mut cmdline = crate::sandbox::quote_windows_arg_pub(exe);
    for a in args {
        cmdline.push(' ');
        cmdline.push_str(&crate::sandbox::quote_windows_arg_pub(a));
    }
    let mut cmdline_w: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
    let cwd_w = to_wide(&cwd.to_string_lossy());
    let env_block = build_account_env_block(temp_dir, account);

    // stdin：NUL 设备（非交互；与受限路径同款）。
    let nul = to_wide(r"\\.\NUL");
    let sa_inherit = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    unsafe {
        let stdin = CreateFileW(
            nul.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &sa_inherit,
            OPEN_EXISTING,
            0,
            0, // hTemplateFile: HANDLE(isize)
        );
        if stdin == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(AgentError::Io("NUL open failed".into()));
        }
        let (out_r, out_w) = make_inheritable_pipe()?;
        let (err_r, err_w) = make_inheritable_pipe()?;
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESTDHANDLES;
        // stdin 不给句柄：NUL 设备句柄以调用者身份打开，CreateProcessWithLogonW
        // 对新用户的继承句柄做访问重检时拒绝（error 5）。非交互命令不读 stdin。
        si.hStdInput = 0; // HANDLE(isize)：不给 stdin（NUL 句柄对沙箱账户不可继承）
        si.hStdOutput = out_w;
        si.hStdError = err_w;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let user_w = to_wide(&account.username);
        let pwd_w = to_wide(&account.password);
        let exe_w = to_wide(exe);
        // domain "." = 本机账户。
        let ok = CreateProcessWithLogonW(
            user_w.as_ptr(),
            to_wide(".").as_ptr(),
            pwd_w.as_ptr(),
            0, // 不 LOGON_WITH_PROFILE：首次登录后 profile 已存在，免载入开销
            // **必须**显式传 appName：NULL 时路径解析发生在新用户的环境里，
            // 其 PATH 为空 → 找不到可执行文件（实测 err 3/5）。
            exe_w.as_ptr(),
            cmdline_w.as_mut_ptr(),
            if no_window { CREATE_NO_WINDOW } else { 0 } | CREATE_UNICODE_ENVIRONMENT,
            if with_env { env_block.as_ptr() as *const std::ffi::c_void } else { std::ptr::null() },
            cwd_w.as_ptr(),
            &si,
            &mut pi,
        );
        // 子端句柄无论成败都要收掉。
        CloseHandle(out_w);
        CloseHandle(err_w);
        CloseHandle(stdin);
        if ok == 0 {
            let err = windows_sys::Win32::Foundation::GetLastError();
            CloseHandle(out_r);
            CloseHandle(err_r);
            return Err(AgentError::Io(format!(
                "CreateProcessWithLogonW failed (Win32 error {err})"
            )));
        }
        CloseHandle(pi.hThread);
        use std::os::windows::io::FromRawHandle;
        Ok(crate::sandbox::SandboxChild::from_raw(
            pi.hProcess,
            pi.dwProcessId,
            std::fs::File::from_raw_handle(out_r as _),
            std::fs::File::from_raw_handle(err_r as _),
        ))
    }
}

/// 后台命令：账户沙箱内 detach，stdout/stderr 重定向到工作区日志文件。
/// 不走管道（无人读会卡死子进程）；文件在工作区授权范围内，继承句柄
/// 的访问重检能通过。返回 PID。
pub fn spawn_background(
    account: &SandboxAccount,
    exe: &str,
    args: &[String],
    cwd: &std::path::Path,
    log_path: &std::path::Path,
) -> Result<u32, AgentError> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessWithLogonW, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
        PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
    };

    let mut cmdline = crate::sandbox::quote_windows_arg_pub(exe);
    for a in args {
        cmdline.push(' ');
        cmdline.push_str(&crate::sandbox::quote_windows_arg_pub(a));
    }
    let mut cmdline_w: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
    let cwd_w = to_wide(&cwd.to_string_lossy());
    let env_block = build_account_env_block(&temp_dir_of(cwd, account), account);

    let log_w = to_wide(&log_path.to_string_lossy());
    let sa_inherit = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    unsafe {
        // 日志文件以调用者身份创建（授权目录内），子进程继承写句柄。
        let log_handle = CreateFileW(
            log_w.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &sa_inherit,
            OPEN_ALWAYS,
            0,
            0,
        );
        if log_handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(AgentError::Io("background log file open failed".into()));
        }
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdInput = 0;
        si.hStdOutput = log_handle as HANDLE;
        si.hStdError = log_handle as HANDLE;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let user_w = to_wide(&account.username);
        let pwd_w = to_wide(&account.password);
        let exe_w = to_wide(exe);
        let ok = CreateProcessWithLogonW(
            user_w.as_ptr(),
            to_wide(".").as_ptr(),
            pwd_w.as_ptr(),
            0,
            exe_w.as_ptr(),
            cmdline_w.as_mut_ptr(),
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            env_block.as_ptr() as *const std::ffi::c_void,
            cwd_w.as_ptr(),
            &si,
            &mut pi,
        );
        CloseHandle(log_handle as HANDLE);
        if ok == 0 {
            let err = windows_sys::Win32::Foundation::GetLastError();
            return Err(AgentError::Io(format!(
                "CreateProcessWithLogonW failed (Win32 error {err})"
            )));
        }
        CloseHandle(pi.hThread);
        Ok(pi.dwProcessId)
    }
}

/// 后台日志路径：<cwd>/.hermix/tmp/bg-<pid 时戳>.log
fn background_log_path(cwd: &std::path::Path) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    cwd.join(".hermix").join("tmp").join(format!("bg-{stamp}.log"))
}

/// temp_dir 参数兼容辅助（spawn_background 内用 cwd 推导）。
fn temp_dir_of(cwd: &std::path::Path, _account: &SandboxAccount) -> std::path::PathBuf {
    cwd.join(".hermix").join("tmp")
}

/// 后台命令的完整路径：选账户 → 授权工作区 → detach spawn。
/// 返回给模型的说明文本（含日志路径）。
pub fn run_background_in_account_sandbox(
    command: &str,
    workspace: &std::path::Path,
) -> Result<String, AgentError> {
    let account = pick_account()?;
    grant_workspace_write(workspace, &account)?;
    let temp_dir = workspace.join(".hermix").join("tmp");
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| AgentError::Io(format!("sandbox temp dir: {e}")))?;
    let log_path = background_log_path(workspace);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("sandbox log dir: {e}")))?;
    }
    let ps = std::env::var("SystemRoot")
        .map(|r| {
            std::path::PathBuf::from(r)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("powershell.exe"));
    let pid = spawn_background(
        &account,
        &ps.to_string_lossy(),
        &[
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            command.to_string(),
        ],
        workspace,
        &log_path,
    )?;
    Ok(format!(
        "Process started in background (pid: {pid}); output is being written to {}",
        log_path.display()
    ))
}

/// 账户环境的 env block：父 env + TEMP/TMP 指向工作区私有 temp +
/// USERPROFILE/APPDATA 等改写为沙箱账户自己的 profile（父的指向真实
/// 用户的 profile，账户读不到，留着只会让工具输出误导性错误）。
fn build_account_env_block(temp_dir: &std::path::Path, account: &SandboxAccount) -> Vec<u16> {
    let profile = to_wide(&format!(r"C:\Users\{}", account.username));
    let _ = profile;
    let mut vars: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let k = k.to_string_lossy().into_owned();
            let v = v.to_string_lossy().into_owned();
            let upper = k.to_uppercase();
            if k.contains('=') || matches!(upper.as_str(), "TMP" | "TEMP" | "USERPROFILE" | "HOMEDRIVE" | "HOMEPATH" | "APPDATA" | "LOCALAPPDATA") {
                None
            } else {
                Some((k, v))
            }
        })
        .collect();
    let t = temp_dir.to_string_lossy().into_owned();
    vars.push(("TMP".into(), t.clone()));
    vars.push(("TEMP".into(), t));
    vars.push(("USERPROFILE".into(), format!(r"C:\Users\{}", account.username)));
    vars.push(("APPDATA".into(), format!(r"C:\Users\{}\AppData\Roaming", account.username)));
    vars.push(("LOCALAPPDATA".into(), format!(r"C:\Users\{}\AppData\Local", account.username)));
    vars.sort();
    let mut block = Vec::new();
    for (k, v) in &vars {
        block.extend(k.encode_utf16());
        block.push('=' as u16);
        block.extend(v.encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

fn make_inheritable_pipe() -> Result<(windows_sys::Win32::Foundation::HANDLE, windows_sys::Win32::Foundation::HANDLE), AgentError> {
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let (mut r, mut w): (windows_sys::Win32::Foundation::HANDLE, windows_sys::Win32::Foundation::HANDLE) = (0, 0);
    unsafe {
        if CreatePipe(&mut r, &mut w, &sa, 0) == 0 {
            return Err(AgentError::Io("CreatePipe failed".into()));
        }
    }
    // 读端（父侧）不可继承。
    unsafe {
        windows_sys::Win32::Foundation::SetHandleInformation(
            r,
            windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT,
            0,
        );
    }
    Ok((r, w))
}

fn sha2_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod account_tests {
    use super::*;

    #[test]
    fn password_meets_all_classes() {
        for _ in 0..8 {
            let pwd = random_password();
            assert_eq!(pwd.len(), 24);
            assert!(pwd.chars().any(|c| c.is_ascii_uppercase()), "no upper: {pwd}");
            assert!(pwd.chars().any(|c| c.is_ascii_lowercase()), "no lower: {pwd}");
            assert!(pwd.chars().any(|c| c.is_ascii_digit()), "no digit: {pwd}");
            assert!(
                pwd.chars().any(|c| "!@#$%^&*()-_=+".contains(c)),
                "no symbol: {pwd}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip_machine_scope() {
        let secret = "hermix-sandbox-password-中文".as_bytes();
        let blob_b64 = dpapi_protect(secret).expect("protect");
        let blob = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &blob_b64)
            .expect("decode");
        let plain = dpapi_unprotect(&blob).expect("unprotect");
        assert_eq!(plain, secret);
    }

    #[test]
    fn secrets_file_roundtrip_shape() {
        // 序列化形状验证（不碰 DPAPI——跨进程上下文）
        let users = SecretsFile {
            version: SETUP_VERSION,
            offline: SandboxUserRecord {
                username: OFFLINE_USERNAME.into(),
                sid: "S-1-5-21-1-2-3-1001".into(),
                password: "blob".into(),
            },
            online: SandboxUserRecord {
                username: ONLINE_USERNAME.into(),
                sid: "S-1-5-21-1-2-3-1002".into(),
                password: "blob".into(),
            },
        };
        let json = serde_json::to_string(&users).unwrap();
        let back: SecretsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.offline.username, OFFLINE_USERNAME);
        assert_eq!(back.online.username, ONLINE_USERNAME);
        assert_eq!(back.version, SETUP_VERSION);
    }
}