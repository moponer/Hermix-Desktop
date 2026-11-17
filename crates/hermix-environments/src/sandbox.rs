//! Windows ACL restricted-token sandbox（对齐 deepseek-harness 成熟方案）。
//!
//! 机制（移植自 `sandbox-windows-acl`，见 `docs/notes/proposed/architecture/2026-09-26-windows-acl-sandbox.md`）：
//! `CreateRestrictedToken` 以 `WRITE_RESTRICTED | DISABLE_MAX_PRIVILEGE |
//! LUA_TOKEN` 派生受限 token——write 类访问做双重判定（正常 SID +
//! restricting SID 都过才放行），read 走正常判定原样保留，零 read 授权
//! 成本。工作区与私有 temp 目录各挂路径派生能力 SID（`S-1-4-x-y`）的
//! 写 ACE（可继承，`SetNamedSecurityInfoW` 落盘时内核传播全树）；同一
//! 调用再落 Low 强制完整性标签 + DENY world `FILE_DELETE_CHILD`，堵住
//! 父目录授权的删除路径。token default DACL 并入 restricting SID 的
//! full-access ACE，否则新建匿名管道（子进程 stdio）EPERM。
//!
//! 策略词汇与 deepseek 逐字对齐：`read-only` / `workspace-write` /
//! `danger-full-access`（env `HERMIX_SANDBOX_MODE`，默认
//! workspace-write）。fail-closed：任何一步失败即报错，绝不回退到
//! 不受限 spawn。enforcement 如实为 `partial`（hard-link 别名、
//! AppContainer 树、读不受限，决策文档已记）。
//!
//! 已知边界（与 deepseek 相同，非 bug）：FAT/exFAT 无 ACL 不承诺；CIM
//! 不可用（Authenticated Users 被剔出 restricting 列表，关死 C:\ 根
//! 树创建逃逸）；受限创建配 `CREATE_NO_WINDOW` 会 STATUS_DLL_INIT_FAILED。
//! 受限 token 进程受桌面 ACL 限制本就无法创建可见窗口，故不再显式设置
//! `SW_HIDE`，避免杀软「隐藏执行」规则误报（见 `spawn_restricted`）。

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use hermix_core::AgentError;
use sha2::{Digest, Sha256};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, LocalFree, SetHandleInformation, GENERIC_READ, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{
        AddMandatoryAce, CopySid, CreateRestrictedToken, CreateWellKnownSid, DuplicateTokenEx,
        EqualSid, GetLengthSid, GetTokenInformation, InitializeAcl, SetTokenInformation, ACL,
        ACL_REVISION_DS, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
        DISABLE_MAX_PRIVILEGE, LABEL_SECURITY_INFORMATION, LUA_TOKEN, OBJECT_INHERIT_ACE,
        SecurityImpersonation, SECURITY_ATTRIBUTES, SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_GROUPS,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenDefaultDacl, TokenGroups, TokenIntegrityLevel,
        TokenPrimary, WinLowLabelSid, WinWorldSid, WRITE_RESTRICTED,
    },
    Security::Authorization::{
        ConvertStringSidToSidW, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS,
        GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::{
        Memory::LocalAlloc,
        Pipes::CreatePipe,
        SystemServices::{
            SE_GROUP_INTEGRITY, SE_GROUP_LOGON_ID, SYSTEM_MANDATORY_LABEL_ACE_TYPE,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
        },
        Threading::{
            CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
            TerminateProcess, WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT,
            PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
        },
    },
};

// ── 访问掩码（winnt.h 稳定值；对齐参考实现 win32-abi.ts）────────
const DELETE: u32 = 0x0001_0000;
const FILE_DELETE_CHILD: u32 = 0x0040;
const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
const STANDARD_RIGHTS_WRITE: u32 = 0x0002_0000;
/// 能力 SID 授予掩码：写 + 删 + 子项删；剔除 WRITE_DAC/WRITE_OWNER
/// ——受限子进程不得重写 DACL 或夺权逃逸。
const GRANT_MASK: u32 =
    (FILE_GENERIC_WRITE | DELETE | FILE_DELETE_CHILD) & !STANDARD_RIGHTS_WRITE;
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
/// LocalAlloc LMEM_FIXED | LMEM_ZEROINIT。
const LPTR: u32 = 0x0040;
/// SECURITY_MAX_SID_SIZE。
const SECURITY_MAX_SID_SIZE: usize = 68;

// ---------------------------------------------------------------------------
// 策略词汇（对齐 deepseek SandboxMode）
// ---------------------------------------------------------------------------

/// 文件效应策略。网络与进程可见性不在此词汇内（与 deepseek 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// 不授予任何可写根；写被拒（PowerShell 会保守进入
    /// ConstrainedLanguage——那是 PS 启动行为，非 ACL 边界）。
    ReadOnly,
    /// 工作区根 + 私有 temp 区内可写。
    WorkspaceWrite,
    /// 完全绕过沙箱（不触碰本模块）。
    DangerFullAccess,
}

impl SandboxMode {
    /// `HERMIX_SANDBOX_MODE`：`read-only` / `workspace-write`（默认）/
    /// `danger-full-access`（含 `off`/`0` 别名）。无法识别的值按默认
    /// 处理并告警——配置错误不能把安全面静默放大。
    pub fn from_env() -> Self {
        match std::env::var("HERMIX_SANDBOX_MODE").ok().as_deref().map(str::trim) {
            Some("read-only") => SandboxMode::ReadOnly,
            Some("danger-full-access") | Some("off") | Some("0") => SandboxMode::DangerFullAccess,
            Some(other) if !other.is_empty() => {
                tracing::warn!(value = %other, "unknown HERMIX_SANDBOX_MODE; falling back to workspace-write");
                SandboxMode::WorkspaceWrite
            }
            _ => SandboxMode::WorkspaceWrite,
        }
    }
}

// ---------------------------------------------------------------------------
// 能力 SID 派生（精确对齐 workspace-sid.ts）
// ---------------------------------------------------------------------------

/// sha256 → 子授权对（30-bit）。工作区 SID 确定性派生（跨会话/重启同
/// 目录同 SID → standing ACE 复用）；temp SID 用 `temp\0` 域分离。
fn subauthorities(domain: &[u8], path: &str) -> (u32, u32) {
    let mut h = Sha256::new();
    h.update(domain);
    h.update(path.as_bytes());
    let d = h.finalize();
    let modulus = (1u32 << 30) - 1;
    let first = (u32::from_le_bytes(d[0..4].try_into().unwrap()) % modulus) + 1;
    let second = (u32::from_le_bytes(d[4..8].try_into().unwrap()) % modulus) + 1;
    (first, second)
}

/// 工作区写身份（`S-1-4-x-y`）。输入必须是规范化路径（大小写/别名
/// 拼写收敛），否则同一目录会派生出第二个身份。
pub fn workspace_write_sid(workspace_root: &str) -> String {
    let (a, b) = subauthorities(b"", workspace_root);
    format!("S-1-4-{a}-{b}")
}

/// 私有 temp 目录写身份（`S-1-4-x-y-1`）。固定第三子授权把 temp 身份
/// 与两段式工作区 SID 域分离——共享身份会让兄弟会话互写 temp 树。
pub fn temp_write_sid(temp_dir: &str) -> String {
    let (a, b) = subauthorities(b"temp\0", temp_dir);
    format!("S-1-4-{a}-{b}-1")
}

// ---------------------------------------------------------------------------
// 通用助手
// ---------------------------------------------------------------------------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn sandbox_err(api: &str) -> AgentError {
    // fail-closed：错误带 API 名与精确 Win32 码（决策文档对 POC 的核心修正）。
    let code = unsafe { GetLastError() };
    AgentError::Io(format!("sandbox: {api} failed (Win32 error {code})"))
}

fn sandbox_msg(msg: impl Into<String>) -> AgentError {
    AgentError::Io(format!("sandbox: {}", msg.into()))
}

/// Windows 命令行参数引用（CommandLineToArgvW 语义）。
fn quote_windows_arg(s: &str) -> String {
    if !s.is_empty() && !s.chars().any(|c| c == ' ' || c == '\t' || c == '"' || c == '\n' || c == '\r') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat('\\').take(backslashes * 2 + 1));
                backslashes = 0;
                out.push('"');
            }
            _ => {
                out.extend(std::iter::repeat('\\').take(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat('\\').take(backslashes * 2));
    out.push('"');
    out
}

/// 盘根（`C:\`）不可作为 workspace：standing ACE 会落满全盘树。
fn is_drive_root(p: &Path) -> bool {
    match p.parent() {
        None => true,
        Some(parent) => parent == p,
    }
}

/// 路径重叠检查（输入须已规范化）：任一为另一的前缀（含相等）即冲突。
fn paths_overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// 规范化并剥 `\\?\` 前缀（该形态不能作 GetNamedSecurityInfoW 对象名，
/// 也影响 SID 派生输入的拼写收敛）。
fn canonical_path(p: &Path) -> Result<PathBuf, AgentError> {
    let canon = std::fs::canonicalize(p)
        .map_err(|e| sandbox_msg(format!("canonicalize failed: {e}")))?;
    Ok(hermix_core::path_security::strip_win32_long_path_buf(&canon))
}

// ---------------------------------------------------------------------------
// 受限 token 构造（对齐 token.ts；每个 Win32 调用都检查，fail-closed）
// ---------------------------------------------------------------------------

/// 打开当前进程 token（CreateRestrictedToken 前置：需要 DUPLICATE +
/// ADJUST_DEFAULT + ASSIGN_PRIMARY 权限）。
fn open_current_process_token() -> Result<HANDLE, AgentError> {
    let mut token: HANDLE = 0;
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            (TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY) as u32,
            &mut token,
        )
    };
    if ok == 0 {
        return Err(sandbox_err("OpenProcessToken"));
    }
    Ok(token)
}

/// 从 token groups 里摘 logon SID（S-1-5-5-x-y，SE_GROUP_LOGON_ID 属性）。
/// keep-alive 不变量：restricted 列表缺它 → DLL 初始化 0xC0000142。
fn find_logon_sid(token: HANDLE) -> Result<Vec<u8>, AgentError> {
    let mut needed: u32 = 0;
    unsafe {
        // 第一次调用预期失败（ERROR_INSUFFICIENT_BUFFER）拿到所需大小。
        GetTokenInformation(token, TokenGroups, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(sandbox_err("GetTokenInformation(size query)"));
    }
    let mut buf = vec![0u8; needed as usize];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenGroups,
            buf.as_mut_ptr() as *mut c_void,
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(sandbox_err("GetTokenInformation(TokenGroups)"));
    }
    let groups = unsafe { &*(buf.as_ptr() as *const TOKEN_GROUPS) };
    // Groups 声明为 [SID_AND_ATTRIBUTES; 1]（柔性数组惯用法），按
    // GroupCount 索引会越界——用指针步进。
    for i in 0..groups.GroupCount as usize {
        let entry = unsafe { &*groups.Groups.as_ptr().add(i) };
        if entry.Attributes & SE_GROUP_LOGON_ID as u32 == SE_GROUP_LOGON_ID as u32
            && !entry.Sid.is_null()
        {
            let len = unsafe { GetLengthSid(entry.Sid) };
            let mut copy = vec![0u8; len as usize];
            if unsafe { CopySid(len, copy.as_mut_ptr() as *mut c_void, entry.Sid) } == 0 {
                return Err(sandbox_err("CopySid(logon)"));
            }
            return Ok(copy);
        }
    }
    Err(sandbox_msg(format!(
        "no logon SID found among {} token groups",
        groups.GroupCount
    )))
}

/// SID 字符串 → 二进制 SID（ConvertStringSidToSidW，返回方持有内存，
/// 用完 LocalFree）。
struct OwnedSid {
    ptr: *mut c_void,
}

impl OwnedSid {
    fn parse(sddl: &str) -> Result<Self, AgentError> {
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let w = wide(sddl);
        if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut ptr) } == 0 {
            return Err(sandbox_err("ConvertStringSidToSidW"));
        }
        Ok(Self { ptr })
    }

    fn from_well_known(kind: i32) -> Result<Self, AgentError> {
        let mut buf = [0u8; SECURITY_MAX_SID_SIZE];
        let mut size = buf.len() as u32;
        if unsafe {
            CreateWellKnownSid(
                kind,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut c_void,
                &mut size,
            )
        } == 0
        {
            return Err(sandbox_err("CreateWellKnownSid"));
        }
        let allocated = unsafe { LocalAlloc(LPTR, size as usize) };
        if allocated.is_null() {
            return Err(sandbox_err("LocalAlloc(sid)"));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(buf.as_ptr(), allocated as *mut u8, size as usize);
        }
        Ok(Self { ptr: allocated })
    }

    fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for OwnedSid {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { LocalFree(self.ptr) };
        }
    }
}

/// 构造受限派生 token：DuplicateTokenEx 出 primary 副本 →
/// CreateRestrictedToken（WRITE_RESTRICTED | DISABLE_MAX_PRIVILEGE |
/// LUA_TOKEN）。restricting 列表 = [logon SID, Everyone, 能力 SID…]；
/// 剔除 Authenticated Users / INTERACTIVE / LOCAL（CIM 不可用 + Public
/// 树写拒——deepseek 决策文档的逃逸封堵）。
fn build_restricted_token(
    capability_sids: &[&OwnedSid],
) -> Result<HANDLE, AgentError> {
    let base = open_current_process_token()?;
    let logon = find_logon_sid(base)?;
    // base token 只用于读取；primary 副本承载后续调整。
    let mut primary: HANDLE = 0;
    let ok = unsafe {
        DuplicateTokenEx(
            base,
            (TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY) as u32,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
    };
    unsafe { CloseHandle(base) };
    if ok == 0 {
        return Err(sandbox_err("DuplicateTokenEx"));
    }

    // logon SID + Everyone 是 keep-alive 不变量（缺一即 0xC0000142 /
    // 0xE0434352）；能力 SID 由调用方按模式给。
    let everyone = OwnedSid::from_well_known(WinWorldSid)?;
    let sid_bufs: Vec<Vec<u8>> = vec![logon];
    let mut sids: Vec<SID_AND_ATTRIBUTES> = Vec::new();
    {
        let logon_ptr = unsafe { LocalAlloc(0x0040, sid_bufs[0].len()) };
        if logon_ptr.is_null() {
            unsafe { CloseHandle(primary) };
            return Err(sandbox_err("LocalAlloc(logon sid)"));
        }
        unsafe { std::ptr::copy_nonoverlapping(sid_bufs[0].as_ptr(), logon_ptr as *mut u8, sid_bufs[0].len()) };
        // logon 缓冲所有权移交 sid_bufs 尾部之外的堆（Drop 段统一释放）。
        sids.push(SID_AND_ATTRIBUTES {
            Sid: logon_ptr,
            Attributes: 0,
        });
        sids.push(SID_AND_ATTRIBUTES {
            Sid: everyone.as_ptr(),
            Attributes: 0,
        });
    }
    for cap in capability_sids {
        sids.push(SID_AND_ATTRIBUTES {
            Sid: cap.as_ptr(),
            Attributes: 0,
        });
    }

    let mut restricted: HANDLE = 0;
    let ok = unsafe {
        CreateRestrictedToken(
            primary,
            WRITE_RESTRICTED | DISABLE_MAX_PRIVILEGE | LUA_TOKEN,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            sids.len() as u32,
            sids.as_ptr(),
            &mut restricted,
        )
    };
    // logon SID 是堆拷贝，其余指向 OwnedSid/everyone 的内存（后者由
    // 所有者释放）；这里只需释放 logon 拷贝与句柄。
    unsafe { LocalFree(sids[0].Sid) };
    unsafe { CloseHandle(primary) };
    if ok == 0 {
        return Err(sandbox_err("CreateRestrictedToken"));
    }
    Ok(restricted)
}

/// 把一个 restricting SID 的 full-access ACE 并进 token default DACL。
/// 不并 → 新建匿名管道（子进程 stdio）在创建时 EPERM，一切带管道的
/// 孙进程 spawn 全挂（token.ts 记录的故障）。
fn grant_token_default_dacl(token: HANDLE, sid: *mut c_void) -> Result<(), AgentError> {
    let mut needed: u32 = 0;
    unsafe {
        GetTokenInformation(token, TokenDefaultDacl, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(sandbox_err("GetTokenInformation(default dacl size)"));
    }
    let mut buf = vec![0u8; needed as usize];
    if unsafe { GetTokenInformation(token, TokenDefaultDacl, buf.as_mut_ptr() as *mut c_void, needed, &mut needed) } == 0 {
        return Err(sandbox_err("GetTokenInformation(TokenDefaultDacl)"));
    }
    let current = unsafe { &*(buf.as_ptr() as *const TOKEN_DEFAULT_DACL) };
    if current.DefaultDacl.is_null() {
        return Err(sandbox_msg("token carries no default DACL"));
    }
    let ea = [build_explicit_access(sid, GRANT_ACCESS, FILE_ALL_ACCESS, OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE)];
    let mut new_acl: *mut ACL = std::ptr::null_mut();
    let rc = unsafe { SetEntriesInAclW(1, ea.as_ptr(), current.DefaultDacl, &mut new_acl) };
    if rc != 0 {
        return Err(sandbox_err("SetEntriesInAclW(default dacl)"));
    }
    let info = TOKEN_DEFAULT_DACL { DefaultDacl: new_acl };
    if unsafe { SetTokenInformation(token, TokenDefaultDacl, &info as *const _ as *const c_void, std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32) } == 0 {
        unsafe { LocalFree(new_acl as *mut c_void) };
        return Err(sandbox_err("SetTokenInformation(default dacl)"));
    }
    unsafe { LocalFree(new_acl as *mut c_void) };
    Ok(())
}

/// token 完整性降到 Low（SetTokenInformation TokenIntegrityLevel）。
/// SID 内联在 label 结构之后，Length 覆盖两者。
fn set_token_low_integrity(token: HANDLE, low_sid: *mut c_void) -> Result<(), AgentError> {
    let sid_len = unsafe { GetLengthSid(low_sid) } as usize;
    let struct_len = std::mem::size_of::<TOKEN_MANDATORY_LABEL>();
    let mut buf = vec![0u8; struct_len + sid_len];
    let label = buf.as_mut_ptr() as *mut TOKEN_MANDATORY_LABEL;
    unsafe {
        (*label).Label.Sid = low_sid;
        (*label).Label.Attributes = SE_GROUP_INTEGRITY as u32;
    }
    if unsafe {
        SetTokenInformation(
            token,
            TokenIntegrityLevel,
            buf.as_mut_ptr() as *const c_void,
            buf.len() as u32,
        )
    } == 0
    {
        return Err(sandbox_err("SetTokenInformation(integrity)"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 目录 ACL 授权（精确对齐 acl.ts：一次 SetNamedSecurityInfoW 落三编辑）
// ---------------------------------------------------------------------------

/// ACE 类型（winnt.h 稳定值）。
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
const ACCESS_DENIED_ACE_TYPE: u8 = 0x1;

/// 一个 EXPLICIT_ACCESS：能力/拒止/撤销由 mode 决定。继承默认
/// OI|CI（子对象与子容器都传播）。
fn build_explicit_access(sid: *mut c_void, mode: i32, perms: u32, inheritance: u32) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: perms,
        grfAccessMode: mode,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid as *mut u16,
        },
    }
}

/// 读目录当前显式 DACL + 强制标签。返回的 ACL 指针都落在 descriptor
/// 分配内部——只允许 LocalFree descriptor，且必须在 SetEntriesInAclW
/// 消费完 ACL 之后（acl.ts 的堆损坏教训）。
struct CurrentSecurity {
    old_dacl: *mut ACL,
    label_acl: *mut ACL,
    descriptor: *mut c_void,
}

impl Drop for CurrentSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe { LocalFree(self.descriptor) };
        }
    }
}

fn read_current_security(path: &str) -> Result<CurrentSecurity, AgentError> {
    let mut owner: *mut c_void = std::ptr::null_mut();
    let mut group: *mut c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wide(path).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            &mut owner,
            &mut group,
            &mut dacl,
            &mut sacl,
            &mut descriptor,
        )
    };
    if rc != 0 {
        return Err(sandbox_msg(format!(
            "GetNamedSecurityInfoW failed (Win32 error {rc}) on {path}"
        )));
    }
    Ok(CurrentSecurity {
        old_dacl: dacl,
        label_acl: sacl,
        descriptor,
    })
}

/// DACL 里是否已存在完全一致的 ACE（类型/继承/掩码/SID 全等）——
/// 幂等 skip 的判定核心。畸形头按"无"处理，回落 merge 路径。
fn has_exact_entry(acl: *const ACL, ace_type: u8, inheritance: u32, mask: u32, sid: *const c_void) -> bool {
    if acl.is_null() {
        return false;
    }
    let (acl_size, ace_count) = unsafe { ((*acl).AclSize as u32, (*acl).AceCount) };
    if acl_size < 8 || acl_size > 1_048_576 {
        return false;
    }
    let base = acl as *const u8;
    let mut offset = 8usize; // ACL 头 8 字节，其后是第一个 ACE
    for _ in 0..ace_count {
        if offset + 8 > acl_size as usize {
            return false;
        }
        let ace_size = unsafe { *(base.add(offset + 2) as *const u16) } as usize;
        if ace_size < 8 || offset + ace_size > acl_size as usize {
            return false;
        }
        let ace_type_at = unsafe { *base.add(offset) };
        let ace_flags_at = unsafe { *base.add(offset + 1) };
        let mask_at = unsafe { *(base.add(offset + 4) as *const u32) };
        if ace_type_at == ace_type && ace_flags_at as u32 == inheritance && mask_at == mask {
            // 允许/拒止 ACE 的 SID 内联在 Mask 之后（偏移 8）。
            let inline_sid = unsafe { base.add(offset + 8) } as *const c_void;
            if unsafe { EqualSid(inline_sid as _, sid as _) } != 0 {
                return true;
            }
        }
        offset += ace_size;
    }
    false
}

/// Low 强制标签 ACL：单条 SYSTEM_MANDATORY_LABEL_ACE（OI|CI 继承 +
/// no-write-up），调用方 LocalFree。
fn build_low_label_acl(low_sid: *mut c_void) -> Result<*mut ACL, AgentError> {
    let sid_len = unsafe { GetLengthSid(low_sid) } as usize;
    // ACL 头 8 + mandatory ACE（头 4 + mask 4）+ SID。缓冲区差 4 字节
    // 即 ERROR_ALLOTTED_SPACE_EXCEEDED (1344)。
    let acl_len = 8 + 8 + sid_len;
    let acl = unsafe { LocalAlloc(LPTR, acl_len) } as *mut ACL;
    if acl.is_null() {
        return Err(sandbox_err("LocalAlloc(label acl)"));
    }
    if unsafe { InitializeAcl(acl, acl_len as u32, ACL_REVISION_DS) } == 0 {
        let err = sandbox_err("InitializeAcl");
        unsafe { LocalFree(acl as *mut c_void) };
        return Err(err);
    }
    if unsafe {
        AddMandatoryAce(
            acl,
            ACL_REVISION_DS,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            low_sid,
        )
    } == 0
    {
        let err = sandbox_err("AddMandatoryAce");
        unsafe { LocalFree(acl as *mut c_void) };
        return Err(err);
    }
    Ok(acl)
}

/// 对 `path` 落一次写授权（merge 三编辑，单次 SetNamedSecurityInfoW）：
/// ① 能力 SID 的 GRANT_MASK allow ACE（OI|CI）；
/// ② world SID 的 FILE_DELETE_CHILD deny ACE（仅 CI——该位落到文件的
/// 继承掩码里会挡掉 FILE_ALL_ACCESS 的打开）；
/// ③ Low 强制标签。
/// 幂等：三者精确全等时跳过 apply（否则 eager 继承会全树重传播，
/// 大工作区分钟级）。已有显式 ACE 经 read-merge-write 保留。
fn grant_write(
    path: &str,
    cap_sid: *mut c_void,
    low_sid: *mut c_void,
    world_sid: *mut c_void,
) -> Result<(), AgentError> {
    let current = read_current_security(path)?;
    if has_exact_entry(current.old_dacl, ACCESS_ALLOWED_ACE_TYPE, OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE, GRANT_MASK, cap_sid)
        && has_exact_entry(current.old_dacl, ACCESS_DENIED_ACE_TYPE, CONTAINER_INHERIT_ACE, FILE_DELETE_CHILD, world_sid)
        && has_exact_entry(current.label_acl, SYSTEM_MANDATORY_LABEL_ACE_TYPE as u8, OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE, SYSTEM_MANDATORY_LABEL_NO_WRITE_UP, low_sid)
    {
        return Ok(());
    }
    let entries = [
        build_explicit_access(cap_sid, GRANT_ACCESS, GRANT_MASK, OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE),
        build_explicit_access(world_sid, DENY_ACCESS, FILE_DELETE_CHILD, CONTAINER_INHERIT_ACE),
    ];
    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let rc = unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), current.old_dacl, &mut new_dacl) };
    if rc != 0 {
        return Err(sandbox_msg(format!(
            "SetEntriesInAclW failed (Win32 error {rc}) granting {path}"
        )));
    }
    let label = build_low_label_acl(low_sid)?;
    let apply_rc = unsafe {
        SetNamedSecurityInfoW(
            wide(path).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            label,
        )
    };
    unsafe { LocalFree(new_dacl as *mut c_void) };
    unsafe { LocalFree(label as *mut c_void) };
    if apply_rc != 0 {
        return Err(sandbox_msg(format!(
            "SetNamedSecurityInfoW failed (Win32 error {apply_rc}) granting {path}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 受限进程创建（对齐 runner.ts 的进程内形态 + 2026-09-16 控制台可见性
// 决策：受限创建禁用 CREATE_NO_WINDOW——STATUS_DLL_INIT_FAILED 直接杀
// 死子进程；且 STARTUPINFO 不再置 STARTF_USESHOWWINDOW/SW_HIDE，避免
// 杀软「隐藏执行」误报（受限 token 本就无法附着交互桌面，实践中仍不可见）。
// ---------------------------------------------------------------------------

/// 手工 env block（UTF-16 双 NUL 结尾）：父 env + TMP/TEMP 覆盖指向
/// 私有 temp（子进程里 cargo/rustc 重度依赖 %TEMP%）。
fn build_env_block(temp_dir: &str) -> Vec<u16> {
    let mut vars: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let k = k.to_string_lossy().into_owned();
            let v = v.to_string_lossy().into_owned();
            let upper = k.to_uppercase();
            if k.contains('=') || upper == "TMP" || upper == "TEMP" {
                None
            } else {
                Some((k, v))
            }
        })
        .collect();
    vars.push(("TMP".into(), temp_dir.into()));
    vars.push(("TEMP".into(), temp_dir.into()));
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

/// 可继承匿名管道（受限 token 的 default DACL 已并入 restricting SID
/// 的 full-access ACE，子端创建成功——token.ts 记录的管道 EPERM 故障）。
fn make_inheritable_pipe() -> Result<(HANDLE, HANDLE), AgentError> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let (mut r, mut w): (HANDLE, HANDLE) = (0, 0);
    if unsafe { CreatePipe(&mut r, &mut w, &sa, 0) } == 0 {
        return Err(sandbox_err("CreatePipe"));
    }
    Ok((r, w))
}

/// `wait_timeout` 的结果：正常退出（exit code 原样镜像）或到达 deadline。
pub enum WaitOutcome {
    Exited(u32),
    TimedOut,
}

/// 受限子进程句柄：stdout/stderr 包装为 std 类型（FromRawHandle），
/// wait 走 WaitForSingleObject + GetExitCodeProcess（exit code 原样
/// 镜像，不折叠）。
pub struct SandboxChild {
    process: HANDLE,
    pub pid: u32,
    /// 子进程 stdout 读端（File 包装，Read 可用）。
    pub stdout: std::fs::File,
    /// 子进程 stderr 读端。
    pub stderr: std::fs::File,
}

impl SandboxChild {
    /// 有界等待：每 200ms 醒一次检查 deadline，到期返回 `TimedOut`。
    ///
    /// 旧实现是 `WaitForSingleObject(INFINITE)` + 独立看门狗线程睡满 timeout
    /// 再对**裸 HANDLE** 做 `TerminateProcess`：命令正常结束会让 `Drop` 先
    /// `CloseHandle`，句柄值被系统复用后看门狗可能杀掉无关进程；而且每条
    /// 命令固定泄漏一个 OS 线程最长 timeout 秒。轮询等待没有这两个问题；
    /// 到期后的进程树清理交给调用方（job / taskkill）。
    pub fn wait_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<WaitOutcome, AgentError> {
        const SLICE: std::time::Duration = std::time::Duration::from_millis(200);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(WaitOutcome::TimedOut);
            }
            let wait_ms = remaining.min(SLICE).as_millis().max(1) as u32;
            match unsafe { WaitForSingleObject(self.process, wait_ms) } {
                WAIT_OBJECT_0 => {
                    let mut code: u32 = 0;
                    if unsafe { GetExitCodeProcess(self.process, &mut code) } == 0 {
                        return Err(sandbox_err("GetExitCodeProcess"));
                    }
                    return Ok(WaitOutcome::Exited(code));
                }
                WAIT_TIMEOUT => continue,
                _ => return Err(sandbox_err("WaitForSingleObject")),
            }
        }
    }

    /// 超时/取消时的强杀（只杀直接子进程；子孙由调用方用 job / taskkill 收）。
    pub fn terminate(&self) {
        unsafe { TerminateProcess(self.process, 1) };
    }

    /// 复制 stdout/stderr 读端句柄（DuplicateHandle 语义），供并行读取
    /// 线程使用——先 wait 再读会因管道写满死锁，必须边跑边读。
    pub fn try_clone_pipes(&self) -> Result<(std::fs::File, std::fs::File), AgentError> {
        let so = self.stdout.try_clone().map_err(|e| sandbox_msg(format!("clone stdout pipe: {e}")))?;
        let se = self.stderr.try_clone().map_err(|e| sandbox_msg(format!("clone stderr pipe: {e}")))?;
        Ok((so, se))
    }
}

impl Drop for SandboxChild {
    fn drop(&mut self) {
        if self.process != 0 {
            unsafe { CloseHandle(self.process) };
        }
    }
}

/// 用受限 token 直接 CreateProcessAsUserW（进程内 runner，无外部
/// 可执行件）。stdio：自建管道（子端可继承）+ NUL 设备作 stdin；
/// 父端去继承位。cwd 直传（argv 语义里工作目录不参与解析）。
fn spawn_restricted(
    token: HANDLE,
    exe: &str,
    args: &[String],
    cwd: &str,
    temp_dir: &str,
) -> Result<SandboxChild, AgentError> {
    let mut cmdline = quote_windows_arg(exe);
    for a in args {
        cmdline.push(' ');
        cmdline.push_str(&quote_windows_arg(a));
    }
    let mut cmdline_w: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
    let env_block = build_env_block(temp_dir);
    let cwd_w = wide(cwd);

    let (stdout_r, stdout_w) = make_inheritable_pipe()?;
    let (stderr_r, stderr_w) = make_inheritable_pipe()?;
    // stdin = NUL 设备（可继承）。
    let nul = wide(r"\\.\NUL");
    let stdin = unsafe {
        CreateFileW(
            nul.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };
    if stdin == INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(stdout_r) };
        unsafe { CloseHandle(stdout_w) };
        unsafe { CloseHandle(stderr_r) };
        unsafe { CloseHandle(stderr_w) };
        return Err(sandbox_err("CreateFileW(NUL)"));
    }
    // 父端去掉继承位（子端保持可继承）。
    unsafe { SetHandleInformation(stdout_r, HANDLE_FLAG_INHERIT, 0) };
    unsafe { SetHandleInformation(stderr_r, HANDLE_FLAG_INHERIT, 0) };
    unsafe { SetHandleInformation(stdin, HANDLE_FLAG_INHERIT, 0) };

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // 不使用 STARTF_USESHOWWINDOW / wShowWindow=SW_HIDE：受限 token（Low
    // 完整性）进程受桌面 ACL 限制本就无法创建可见窗口，但显式 SW_HIDE 会被
    // 杀软「隐藏执行」规则命中（父进程是 Hermix 而非系统程序 + 隐藏窗口 +
    // powershell）。去掉隐藏标志后进程在实践中仍不可见（无交互桌面可附着），
    // 但不再触发误报。安全隔离（受限 token / Low IL）完整保留。
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdInput = stdin;
    si.hStdOutput = stdout_w;
    si.hStdError = stderr_w;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let ok = unsafe {
        CreateProcessAsUserW(
            token,
            std::ptr::null(),
            cmdline_w.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // bInheritHandles
            CREATE_UNICODE_ENVIRONMENT,
            env_block.as_ptr() as *const c_void,
            cwd_w.as_ptr(),
            &si,
            &mut pi,
        )
    };
    // 子端/stdin/线程句柄无论成败都要收掉。
    unsafe { CloseHandle(stdout_w) };
    unsafe { CloseHandle(stderr_w) };
    unsafe { CloseHandle(stdin) };
    if ok == 0 {
        let err = sandbox_err("CreateProcessAsUserW");
        unsafe { CloseHandle(stdout_r) };
        unsafe { CloseHandle(stderr_r) };
        return Err(err);
    }
    if pi.hThread != 0 {
        unsafe { CloseHandle(pi.hThread) };
    }

    use std::os::windows::io::FromRawHandle;
    Ok(SandboxChild {
        process: pi.hProcess,
        pid: pi.dwProcessId,
        stdout: unsafe { std::fs::File::from_raw_handle(stdout_r as _) },
        stderr: unsafe { std::fs::File::from_raw_handle(stderr_r as _) },
    })
}

// ---------------------------------------------------------------------------
// 编排入口（对齐 sandbox 包的 provision/run 契约，进程内形态）
// ---------------------------------------------------------------------------

/// 一次受限执行的会话：standing workspace grant + 每命令私有 temp +
/// 受限 token。Drop 收 token + 删 temp（ACE 随目录销毁，免 revoke FFI；
/// 崩溃残留 = 惰性垃圾，文档化）。
pub struct SandboxSession {
    token: HANDLE,
    pub workspace: PathBuf,
    pub temp_dir: PathBuf,
}

impl SandboxSession {
    /// 配置会话：工作区校验 → 私有 temp 创建 → ACL 授权（temp 恒授，
    /// workspace 仅 workspace-write）→ 受限 token（logon + Everyone +
    /// 能力 SID restricting 列表，default DACL 并入 + Low 完整性）。
    /// fail-closed：任何一步失败即错，不产出半配置会话。
    pub fn provision(mode: SandboxMode, workspace: &Path) -> Result<Self, AgentError> {
        if mode == SandboxMode::DangerFullAccess {
            return Err(sandbox_msg("danger-full-access must bypass the sandbox at the call site"));
        }
        let ws = canonical_path(workspace)?;
        if is_drive_root(&ws) {
            return Err(sandbox_msg("workspace must not be a drive root"));
        }
        // temp 根候选：系统 TMP 在前（现状不变），`~/.hermix/tmp` 兜底。
        // 2026-11-10：用户自设 TMP（<TMPDIR>）的 ACL 拒绝 WRITE_DAC，
        // grant_write 的 SetNamedSecurityInfoW 报 Win32 error 5，终端整体
        // 不可用——回退到应用确定可写的主目录，而不是把整个终端废掉。
        let mut temp_roots: Vec<PathBuf> = vec![std::env::temp_dir()];
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let fallback = PathBuf::from(home).join(".hermix").join("tmp");
            if !temp_roots.contains(&fallback) {
                temp_roots.push(fallback);
            }
        }

        let mut last_err: Option<AgentError> = None;
        for root in &temp_roots {
            match Self::provision_with_temp_root(mode, &ws, root) {
                Ok(session) => return Ok(session),
                Err(e) => {
                    tracing::warn!(
                        root = %root.display(),
                        error = %e,
                        "sandbox temp root unusable — falling back to the next candidate"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| sandbox_msg("no usable sandbox temp root")))
    }

    /// 在指定 temp 根下建私有会话目录并完成全部授权（`provision` 的
    /// 逐候选实现体；`ws` 已过盘根与规范路径校验）。
    fn provision_with_temp_root(
        mode: SandboxMode,
        ws: &Path,
        temp_root: &Path,
    ) -> Result<Self, AgentError> {
        let temp_dir =
            temp_root.join(format!("hermix-sbx-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| sandbox_msg(format!("temp dir create failed: {e}")))?;
        let temp_canon = canonical_path(&temp_dir)?;
        if paths_overlap(ws, &temp_canon) {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(sandbox_msg("workspace overlaps the sandbox temp root"));
        }

        let ws_str = ws.to_string_lossy().into_owned();
        let temp_str = temp_canon.to_string_lossy().into_owned();
        let ws_sid = OwnedSid::parse(&workspace_write_sid(&ws_str))?;
        let temp_sid = OwnedSid::parse(&temp_write_sid(&temp_str))?;
        let low = OwnedSid::from_well_known(WinLowLabelSid)?;
        let world = OwnedSid::from_well_known(WinWorldSid)?;

        // temp 目录恒授（read-only 模式下 PowerShell 探针要写私有 temp
        // 才保得住 FullLanguage；这是 per-invocation 私有授权，不是对
        // 任意 temp 的放行）。
        grant_write(&temp_str, temp_sid.as_ptr(), low.as_ptr(), world.as_ptr())?;
        if mode == SandboxMode::WorkspaceWrite {
            grant_write(&ws_str, ws_sid.as_ptr(), low.as_ptr(), world.as_ptr())?;
        }

        let caps: Vec<&OwnedSid> = match mode {
            SandboxMode::ReadOnly => vec![&temp_sid],
            SandboxMode::WorkspaceWrite => vec![&ws_sid, &temp_sid],
            SandboxMode::DangerFullAccess => unreachable!("checked above"),
        };
        let token = build_restricted_token(&caps)?;
        if let Err(e) = grant_token_default_dacl(token, temp_sid.as_ptr()) {
            unsafe { CloseHandle(token) };
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(e);
        }
        if let Err(e) = set_token_low_integrity(token, low.as_ptr()) {
            unsafe { CloseHandle(token) };
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(e);
        }

        Ok(Self {
            token,
            workspace: ws.to_path_buf(),
            temp_dir: temp_canon,
        })
    }

    /// 在受限 token 下启动进程。stdout/stderr 为可读句柄，exit code
    /// 原样镜像。
    pub fn spawn(
        &self,
        exe: &str,
        args: &[String],
        cwd: &Path,
    ) -> Result<SandboxChild, AgentError> {
        spawn_restricted(
            self.token,
            exe,
            args,
            &cwd.to_string_lossy(),
            &self.temp_dir.to_string_lossy(),
        )
    }
}

impl Drop for SandboxSession {
    fn drop(&mut self) {
        if self.token != 0 {
            unsafe { CloseHandle(self.token) };
        }
        // best-effort：ACE 随目录销毁；失败仅留惰性垃圾。
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

// ---------------------------------------------------------------------------
// 测试（无 FFI 依赖的纯逻辑部分；FFI 行为由真机冒烟钉死，见 #4）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_derivation_is_deterministic() {
        let a = workspace_write_sid(r"C:\<workspace>\hermix-desktop");
        let b = workspace_write_sid(r"C:\<workspace>\hermix-desktop");
        assert_eq!(a, b);
        assert!(a.starts_with("S-1-4-"));
        let parts: Vec<&str> = a.split('-').collect();
        assert_eq!(parts.len(), 5, "S-1-4-x-y has 5 components: {a}");
    }

    #[test]
    fn sid_derivation_separates_domains() {
        // 工作区与 temp 域分离（temp 三段式尾标 -1）；不同路径不同 SID。
        let ws = workspace_write_sid(r"C:\ws");
        let temp = temp_write_sid(r"C:\ws");
        assert_ne!(ws, temp);
        assert!(temp.ends_with("-1"));
        let t1 = temp_write_sid(r"C:\t1");
        let t2 = temp_write_sid(r"C:\t2");
        assert_ne!(t1, t2, "sibling temp dirs must not share an identity");
    }

    #[test]
    fn sid_subauthorities_stay_in_30bit_range() {
        for p in ["", "a", "C:\\x\\y\\z", "路径/测试"] {
            let s = workspace_write_sid(p);
            let x: u32 = s.split('-').nth(3).unwrap().parse().unwrap();
            let y: u32 = s.split('-').nth(4).unwrap().parse().unwrap();
            assert!(x >= 1 && x < (1 << 30) && y >= 1 && y < (1 << 30));
        }
    }

    #[test]
    fn quote_windows_arg_roundtrips() {
        assert_eq!(quote_windows_arg("plain"), "plain");
        // 无空格/引号：原样返回安全（无引号包裹时尾反斜杠无转义歧义）。
        assert_eq!(quote_windows_arg("tail\\"), "tail\\");
        assert_eq!(quote_windows_arg("with space"), "\"with space\"");
        assert_eq!(quote_windows_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        // 引号包裹时尾反斜杠必须翻倍（转义歧义）。
        assert_eq!(quote_windows_arg("a b\\"), "\"a b\\\\\"");
        assert_eq!(quote_windows_arg(""), "\"\"");
    }

    #[test]
    fn overlap_and_root_checks() {
        assert!(paths_overlap(Path::new(r"C:\a\b"), Path::new(r"C:\a")));
        assert!(paths_overlap(Path::new(r"C:\a"), Path::new(r"C:\a")));
        assert!(!paths_overlap(Path::new(r"C:\a"), Path::new(r"C:\ab")));
        assert!(is_drive_root(Path::new(r"C:\")));
        assert!(!is_drive_root(Path::new(r"C:\ws")));
    }

    #[test]
    fn env_block_overrides_tmp_and_temp() {
        let block = build_env_block(r"C:\t\sbx");
        let text: String = String::from_utf16_lossy(&block);
        assert!(text.contains("TMP=C:\\t\\sbx"));
        assert!(text.contains("TEMP=C:\\t\\sbx"));
    }
}
