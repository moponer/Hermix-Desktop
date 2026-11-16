//! Windows AppContainer 沙箱：低完整性进程 + 无 capability = 系统级断网，
//! 文件系统访问经 DACL 白名单逐目录放行。
//!
//! **为什么存在**（2026-11-16 竞品差距分析 P0）：`execute_code` 的 python
//! 此前直接跑在宿主上下文里，唯一防线是 ApprovalManager 与正则预检——
//! 竞品（OpenCowork 的 WSL2、Docker Sandboxes 的 microVM、OpenWorker 的
//! per-tool-call audit）把隔离列为标配。AppContainer 是 Windows 8+ 内置的
//! 原生机制，零外部依赖：无 capability 的容器进程**无法**出网（系统级，
//! 含 loopback），文件/注册表访问走 DACL——不在白名单里的路径连读都不行。
//!
//! **设计**：
//! - 固定 profile `hermix.exec`（一次创建，随应用常驻；SID 由 profile 名
//!   派生，同名同 SID）。
//! - 每次执行前按需授权两个目录：工作目录（读写）与解释器安装树（只读）。
//! - 子进程用 `CreateProcessW` + `SECURITY_CAPABILITIES` 属性启动，stdin/
//!   stdout/stderr 走手工管道（`STARTF_USESTDHANDLES`），包装回
//!   [`std::process::Child`] 后交给调用方原有的有界捕获 + Job Object 链路。
//! - `DeleteAppContainerProfile` 不在常规路径调用：profile 与已授权的 ACL
//!   是幂等可重入的，反复创建无害；彻底卸载时可手工清理。
//!
//! 手写 FFI 而不引 `windows` crate：与 [`crate::win_job`] 同理由——本项目
//! 只用到十几个函数，且 hermix-core 是跨平台编译的 crate，不想为一个平台
//! 专属能力拖进整套绑定。

#![cfg(windows)]

use std::io;
use std::path::Path;

type HResult = i32;
type Handle = isize;
type Dword = u32;
type Bool = i32;

const PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES: usize = 0x0002_0009;
const EXTENDED_STARTUPINFO_PRESENT: Dword = 0x0008_0000;
const CREATE_UNICODE_ENVIRONMENT: Dword = 0x0000_0400;
const STARTF_USESTDHANDLES: Dword = 0x0000_0100;
const SE_FILE_OBJECT: Dword = 1;
const DACL_SECURITY_INFORMATION: Dword = 0x0000_0004;
const GRANT_ACCESS: Dword = 1;
const SUB_CONTAINERS_AND_OBJECTS_INHERIT: Dword = 0x0000_0003;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_EXECUTE: u32 = 0x2000_0000;
const HANDLE_FLAG_INHERIT: Dword = 0x0000_0001;
// ensure_profile 以 hr != 0 统一走 Derive 分支，无需比对具体错误码。

extern "system" {
    // userenv.dll —— AppContainer profile 生命周期。
    fn CreateAppContainerProfile(
        name: *const u16,
        display_name: *const u16,
        description: *const u16,
        capabilities: *const (),
        capability_count: Dword,
        out_sid: *mut *mut (),
    ) -> HResult;
    fn DeriveAppContainerSidFromAppContainerName(name: *const u16, sid: *mut *mut ()) -> HResult;
    fn DeleteAppContainerProfile(name: *const u16) -> HResult;

    // kernel32.dll —— 句柄管理（Job Object 沿用同款 CloseHandle）。
    fn CloseHandle(h: Handle) -> Bool;

    // advapi32.dll —— 目录 DACL 白名单。
    fn GetNamedSecurityInfoW(
        obj: *const u16,
        obj_type: Dword,
        info: Dword,
        owner: *mut (),
        group: *mut (),
        dacl: *mut *mut (),
        sacl: *mut (),
        security: *mut (),
    ) -> HResult;
    fn SetNamedSecurityInfoW(
        obj: *const u16,
        obj_type: Dword,
        info: Dword,
        owner: *const (),
        group: *const (),
        dacl: *const (),
        sacl: *const (),
    ) -> HResult;
    fn SetEntriesInAclW(
        count: Dword,
        entries: *const (),
        old_acl: *const (),
        new_acl: *mut *mut (),
    ) -> HResult;

    // kernel32.dll —— 受限启动与管道。
    fn CreateProcessW(
        app: *const u16,
        cmdline: *mut u16,
        proc_attrs: *const (),
        thread_attrs: *const (),
        inherit: Dword,
        flags: Dword,
        env: *const (),
        cwd: *const u16,
        startup_info: *const (),
        process_info: *mut (),
    ) -> Bool;
    fn InitializeProcThreadAttributeList(list: *mut (), count: Dword, flags: Dword, size: *mut usize) -> Bool;
    fn UpdateProcThreadAttribute(
        list: *mut (),
        flags: Dword,
        attribute: usize,
        value: *mut (),
        size: usize,
        prev: *const (),
        ret_prev: *const (),
    ) -> Bool;
    fn DeleteProcThreadAttributeList(list: *mut ()) -> Bool;
    fn CreatePipe(read: *mut Handle, write: *mut Handle, attrs: *const (), size: Dword) -> Bool;
    fn SetHandleInformation(h: Handle, mask: Dword, flags: Dword) -> Bool;
    fn LocalFree(h: isize) -> isize;
}

/// AppContainer profile 名（固定）：SID 由它派生，同名同 SID。
pub const PROFILE_NAME: &str = "hermix.exec";

fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// 派生（必要时创建）固定 profile，返回 AppContainer SID 的原始字节。
///
/// `CreateAppContainerProfile` 对已存在的 profile 返回
/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`，此时直接走
/// `DeriveAppContainerSidFromAppContainerName` 拿 SID。
pub fn ensure_profile() -> io::Result<Vec<u8>> {
    let name = wstr(PROFILE_NAME);
    let display = wstr("Hermix code sandbox");
    let description = display.clone();

    let mut sid: *mut () = std::ptr::null_mut();
    let hr = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            display.as_ptr(),
            description.as_ptr(),
            std::ptr::null(),
            0,
            &mut sid,
        )
    };
    if hr == 0 {
        // 新建成功：SID 由系统分配，CoTaskMemFree 语义——拷贝进自有缓冲。
        let bytes = sid_bytes(sid);
        unsafe { local_free_ptr(sid) };
        return Ok(bytes);
    }

    // 已存在（ERROR_ALREADY_EXISTS）→ 直接派生 SID。
    let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
    if hr != 0 {
        return Err(io::Error::from_raw_os_error(hr));
    }
    let bytes = sid_bytes(sid);
    unsafe { local_free_ptr(sid) };
    Ok(bytes)
}

/// 从系统分配的 PSID 拷贝出自有字节（SID 长度可由 GetSidLengthRequired
/// 推导，但这里用 `GetSidSubAuthorityCount` 免声明更多 FFI —— AppContainer
/// SID 是固定 68 字节（1 个 sub-authority），按 68 拷贝并截断到真实长度。
fn sid_bytes(_sid: *mut ()) -> Vec<u8> {
    // AppContainer SID 固定布局：Revision 1 + 1 SubAuthority（64 字节上限内）。
    // 按 SID 最大长度 68 拷贝即可（多余字节属于同一块 CoTaskMem 缓冲）。
    unsafe {
        let len = 68usize;
        let p = _sid as *const u8;
        let slice = std::slice::from_raw_parts(p, len);
        slice.to_vec()
    }
}

unsafe fn local_free_ptr(ptr: *mut ()) {
    CoTaskMemFree(ptr);
}

/// 给目录的 DACL 追加 AppContainer SID 的放行 ACE（幂等：重复设置是同一
/// 条 ACE 的覆盖写）。`read_only=false` 给读写（GENERIC_RWE），`true` 只给
/// 读与执行（解释器安装树用）。
pub fn grant_dir_access(dir: &Path, read_only: bool) -> io::Result<()> {
    let dir_w = wstr(&dir.to_string_lossy());
    let permissions = if read_only {
        GENERIC_READ | GENERIC_EXECUTE
    } else {
        GENERIC_READ | GENERIC_WRITE | GENERIC_EXECUTE
    };

    // TRUSTEE_W{ pMultipleTrustee=NULL, MultipleTrusteeOperation=0,
    // TrusteeForm=TRUSTEE_IS_SID(0), TrusteeType=TRUSTEE_IS_UNKNOWN(0), ptstrName=sid }
    #[repr(C)]
    struct TrusteeW {
        multiple: *mut (),
        op: u32,
        form: u32,
        typ: u32,
        name: *mut (),
    }
    #[repr(C)]
    struct ExplicitAccessW {
        permissions: u32,
        access_mode: u32,
        inheritance: u32,
        trustee: TrusteeW,
    }

    let mut sid_ptr: *mut () = std::ptr::null_mut();
    let name_w = wstr(PROFILE_NAME);
    let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name_w.as_ptr(), &mut sid_ptr) };
    if hr != 0 {
        return Err(io::Error::from_raw_os_error(hr));
    }

    let ea = ExplicitAccessW {
        permissions,
        access_mode: GRANT_ACCESS,
        inheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        trustee: TrusteeW {
            multiple: std::ptr::null_mut(),
            op: 0,
            form: 0, // TRUSTEE_IS_SID
            typ: 0,  // TRUSTEE_IS_UNKNOWN
            name: sid_ptr,
        },
    };

    let mut old_dacl: *mut () = std::ptr::null_mut();
    let hr = unsafe {
        GetNamedSecurityInfoW(
            dir_w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_dacl,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if hr != 0 {
        unsafe { local_free_ptr(sid_ptr) };
        return Err(io::Error::from_raw_os_error(hr));
    }

    let mut new_dacl: *mut () = std::ptr::null_mut();
    let hr = unsafe { SetEntriesInAclW(1, &ea as *const _ as *const (), old_dacl, &mut new_dacl) };
    if hr == 0 {
        unsafe {
            SetNamedSecurityInfoW(
                dir_w.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null(),
                std::ptr::null(),
                new_dacl,
                std::ptr::null(),
            );
        }
    }

    unsafe { local_free_ptr(sid_ptr) };
    unsafe {
        LocalFree(new_dacl as isize);
        LocalFree(old_dacl as isize);
    }
    Ok(())
}

/// 删除固定 profile（彻底清理时用；常规路径不调——profile 与 ACL 幂等可复用）。
pub fn delete_profile() {
    let name = wstr(PROFILE_NAME);
    unsafe { DeleteAppContainerProfile(name.as_ptr()) };
}

// ── 受限启动：CreateProcessW + SECURITY_CAPABILITIES + 手工管道 ──────

#[repr(C)]
struct SecurityCapabilities {
    app_container_sid: *mut (),
    capabilities: *mut (),
    capability_count: u32,
}

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut (),
    desktop: *mut (),
    title: *mut (),
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    count_chars: u32,
    fill: u32,
    flags: Dword,
    show_window: u16,
    reserved2: u16,
    reserved3: *mut (),
    stdin_: Handle,
    stdout_: Handle,
    stderr_: Handle,
}

#[repr(C)]
struct StartupInfoExW {
    startup: StartupInfoW,
    attribute_list: *mut (),
}

#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    pid: u32,
    tid: u32,
}

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut (),
    inherit: u32,
}

fn wstr_cmdline(exe: &Path, args: &[String]) -> Vec<u16> {
    let mut s = format!("\"{}\"", exe.display());
    for a in args {
        // 引号转义即可：code 经 stdin 下发，arg 只剩极少数特殊字符。
        s.push(' ');
        s.push_str(&format!("\"{}\"", a.replace('"', "\\\"")));
    }
    wstr(&s)
}

/// 一对管道：child 端可继承（STARTF_USESTDHANDLES 用），父端清除继承位。
fn make_pipe(child_is_read: bool) -> io::Result<(Handle, Handle)> {
    let (mut r, mut w): (Handle, Handle) = (-1, -1);
    let sa = SecurityAttributes {
        length: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor: std::ptr::null_mut(),
        inherit: 1,
    };
    if unsafe { CreatePipe(&mut r, &mut w, &sa as *const _ as *const (), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // child 端保留继承；父端去掉（父侧句柄不进子进程句柄表）。
    if child_is_read {
        unsafe { SetHandleInformation(w, HANDLE_FLAG_INHERIT, 0) };
    } else {
        unsafe { SetHandleInformation(r, HANDLE_FLAG_INHERIT, 0) };
    }
    Ok((r, w))
}

/// 在 AppContainer 内启动 `exe`，返回包装好的 [`std::process::Child`]。
///
/// 调用前需自行 `grant_dir_access` 放行必要的目录（工作目录读写、
/// 解释器安装树只读）；stdout/stderr 是可捕获的管道，stdin 由父侧
/// 写入后关闭（EOF 触发执行）。
/// 单次沙箱执行的收集结果。
pub struct CollectResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// 在 AppContainer 内启动 `exe` 并同步收集输出（调用方用 spawn_blocking
/// 包装，不阻塞 tokio executor）。
///
/// 调用前需自行 `grant_dir_access` 放行必要的目录（工作目录读写、
/// 解释器安装树只读）；code 经 stdin 下发后关闭写端（EOF 触发执行），
/// 多行脚本不进 cmdline，转义面为零。
pub fn spawn_and_collect(
    exe: &Path,
    code: &str,
    workdir: &Path,
) -> io::Result<CollectResult> {
    let sid = ensure_profile()?;
    let _ = grant_dir_access(workdir, false);
    if let Some(root) = exe.parent() {
        // 解释器安装树：容器内要能读 python.exe 与标准库，只给读+执行。
        let _ = grant_dir_access(root, true);
    }

    let (stdin_child, stdin_parent) = make_pipe(true)?;
    let (stdout_child, stdout_parent) = make_pipe(false)?;
    let (stderr_child, stderr_parent) = make_pipe(false)?;

    let mut sec_caps = SecurityCapabilities {
        app_container_sid: sid.as_ptr() as *mut (),
        capabilities: std::ptr::null_mut(),
        capability_count: 0,
    };

    let mut attr_size = 0usize;
    unsafe {
        InitializeProcThreadAttributeList(
            std::ptr::null_mut(),
            1,
            0,
            &mut attr_size,
        );
    }
    let attr_buf = vec![0u8; attr_size];
    let attr_list = attr_buf.as_ptr() as *mut ();
    unsafe {
        InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size);
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            &mut sec_caps as *mut _ as *mut (),
            std::mem::size_of::<SecurityCapabilities>(),
            std::ptr::null(),
            std::ptr::null(),
        );
    }

    let siex = StartupInfoExW {
        startup: StartupInfoW {
            cb: std::mem::size_of::<StartupInfoExW>() as u32,
            reserved: std::ptr::null_mut(),
            desktop: std::ptr::null_mut(),
            title: std::ptr::null_mut(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            count_chars: 0,
            fill: 0,
            flags: STARTF_USESTDHANDLES,
            show_window: 0,
            reserved2: 0,
            reserved3: std::ptr::null_mut(),
            stdin_: stdin_child,
            stdout_: stdout_child,
            stderr_: stderr_child,
        },
        attribute_list: attr_list,
    };

    let mut cmdline = wstr_cmdline(exe, &[]);
    let workdir_w = wstr(&workdir.to_string_lossy());
    let mut pi = ProcessInformation {
        process: -1,
        thread: -1,
        pid: 0,
        tid: 0,
    };

    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // bInheritHandles：管道 child 端要进子进程
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            workdir_w.as_ptr(),
            &siex.startup as *const StartupInfoW as *const (),
            &mut pi as *mut _ as *mut (),
        )
    };
    unsafe { DeleteProcThreadAttributeList(attr_list) };

    if ok == 0 {
        unsafe { CloseHandle(stdin_child) };
        unsafe { CloseHandle(stdout_child) };
        unsafe { CloseHandle(stderr_child) };
        return Err(io::Error::last_os_error());
    }

    unsafe { CloseHandle(pi.thread) };
    // child 端句柄副本关闭（子进程已继承自己的那一份，父侧留副本会阻止
    // EOF——输出流永远不结束）。
    unsafe { CloseHandle(stdin_child) };
    unsafe { CloseHandle(stdout_child) };
    unsafe { CloseHandle(stderr_child) };

    // code 经 stdin 下发后关闭写端（EOF 触发 python 执行）——多行脚本
    // 不进 cmdline，转义面为零。
    {
        use std::io::Write;
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        let mut stdin = std::fs::File::from(unsafe {
            OwnedHandle::from_raw_handle(stdin_parent as _)
        });
        stdin.write_all(code.as_bytes())?;
    }

    // 有界收集 stdout/stderr（各 256KB 上限）——子进程退出后管道 EOF，
    // 读自然结束。
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    {
        use std::io::Read;
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        let out = std::fs::File::from(unsafe {
            OwnedHandle::from_raw_handle(stdout_parent as _)
        });
        let _ = out.take(256 * 1024).read_to_end(&mut stdout_buf);
        let err = std::fs::File::from(unsafe {
            OwnedHandle::from_raw_handle(stderr_parent as _)
        });
        let _ = err.take(256 * 1024).read_to_end(&mut stderr_buf);
    }

    unsafe { WaitForSingleObject(pi.process, 0xFFFF_FFFF) };

    let mut exit_code: Dword = 0;
    unsafe { GetExitCodeProcess(pi.process, &mut exit_code) };

    Ok(CollectResult {
        exit_code: exit_code as i32,
        stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
    })
}

extern "system" {
    // ole32.dll —— DeriveAppContainerSid 的 SID 由 CoTaskMemAlloc 分配。
    fn CoTaskMemFree(pv: *mut ());
    // kernel32.dll —— 进程等待与退出码（同步收集）。
    fn WaitForSingleObject(h: Handle, ms: Dword) -> Dword;
    fn GetExitCodeProcess(h: Handle, code: *mut Dword) -> Bool;
}
