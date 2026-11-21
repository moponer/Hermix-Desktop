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
/// 不给控制台程序新建可见窗口（父进程是 GUI 应用时必需，否则弹黑窗）。
const CREATE_NO_WINDOW: Dword = 0x0800_0000;
/// WaitForSingleObject 超时返回值。
const WAIT_TIMEOUT: Dword = 0x0000_0102;
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
        // PSECURITY_DESCRIPTOR* —— 原声明成 `*mut ()` 少了一层指针：拿不到
        // 描述符就无法正确释放（old_dacl 指向它内部，不是独立分配块）。
        security: *mut *mut (),
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
    fn GetLengthSid(sid: *mut ()) -> u32;
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

/// 从系统分配的 PSID 拷贝出自有字节（长度用 `GetLengthSid` 取 —— 2026-11-19
/// 修正：原先硬拷 68 字节是越界读，由名字派生的 AppContainer SID 只有 16 字节）。
fn sid_bytes(_sid: *mut ()) -> Vec<u8> {
    // 2026-11-19：原来按「AppContainer SID 固定 68 字节」硬拷 —— **错的**。
    // SID 长度 = 8 字节头 + SubAuthority 数×4；由名字派生的 AppContainer SID
    // 只有 1 个 sub-authority（16 字节），系统分配的缓冲也正好是这么多。
    // 硬拷 68 字节 = 越过分配尾部读 52 字节（堆越界读）。改用 GetLengthSid。
    let len = unsafe { GetLengthSid(_sid) } as usize;
    if len == 0 || len > 68 {
        return Vec::new();
    }
    unsafe {
        let p = _sid as *const u8;
        std::slice::from_raw_parts(p, len).to_vec()
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
    // 2026-11-19：**堆损坏的根因**。GetNamedSecurityInfoW 返回的 DACL 是
    // 「安全描述符内部」的指针 —— 它**不是**独立分配块，不能单独 LocalFree。
    // 原代码 LocalFree(old_dacl) 释放了内部指针 → STATUS_HEAP_CORRUPTION，
    // 进程级崩溃（2026-11-18/19 实测：AppContainer 通道一用就崩）。
    // 正确做法：把描述符本身要出来，**只** LocalFree 描述符。
    let mut descriptor: *mut () = std::ptr::null_mut();
    let hr = unsafe {
        GetNamedSecurityInfoW(
            dir_w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if hr != 0 {
        unsafe { local_free_ptr(sid_ptr) };
        if !descriptor.is_null() {
            unsafe { LocalFree(descriptor as isize) };
        }
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
        // 释放描述符（old_dacl 指向它内部，不能单独释放）
        if !descriptor.is_null() {
            LocalFree(descriptor as isize);
        }
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
    // dwXCountChars / dwYCountChars / dwFillAttribute / dwFlags —— **四个** u32。
    // 2026-11-19：原结构只写了三个（少 dwYCountChars），整个结构短 8 字节，
    // 且 dwFlags 之后的每个字段全部错位 4 字节。后果：
    //   - 内核读 dwFlags 时读到的是 dwFillAttribute（0）→ STARTF_USESTDHANDLES 失效；
    //   - STARTUPINFOEX 的 lpAttributeList 实际落在偏移 96，而内核按
    //     sizeof(STARTUPINFOEX)=112 去读 → 读到垃圾指针 → CreateProcessW 报
    //     ERROR_INVALID_PARAMETER (87)。这就是 AppContainer 通道「永远建不起来」
    //     的真正原因（2026-11-19 实测：普通 CreateProcessW 成功、带
    //     EXTENDED_STARTUPINFO_PRESENT 必失败，二分即定位到这一层）。
    count_chars: u32,
    count_chars_y: u32,
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

/// 一对管道，返回 **(子进程端, 父进程端)**。继承位永远在子进程端（
/// STARTF_USESTDHANDLES 要求），父进程端清掉（父侧句柄不进子进程句柄表）。
///
/// 2026-11-19：原实现返回 (r, w) 且只在「子读」时清 w 的继承位 —— 于是
/// stdout/stderr 取到的是 (读端, 写端)：**父子两端都拿反了**。子进程拿读端
/// 当 stdout 写不进去，父进程拿写端当 stdout 读不出来，而且父进程自己攥着
/// 写端 → 永远等不到 EOF。这是 AppContainer 通道一直崩/挂的主因之一。
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
    if child_is_read {
        // 子进程读 stdin：读端继承，写端（父进程写）不继承。
        unsafe { SetHandleInformation(w, HANDLE_FLAG_INHERIT, 0) };
        Ok((r, w))
    } else {
        // 子进程写 stdout/stderr：写端继承，读端（父进程读）不继承。
        unsafe { SetHandleInformation(r, HANDLE_FLAG_INHERIT, 0) };
        Ok((w, r))
    }
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
    /// 是否因超时被终止（此时 exit_code 固定为 124）。
    pub timed_out: bool,
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
    run_capture(exe, &[], Some(code), workdir, None)
}

/// 在 AppContainer 内跑一条命令并收集输出（可传 argv、可给超时）。
///
/// 与 [`spawn_and_collect`] 同一套启动原语，区别只有三点：能带 argv、能只在
/// 需要时下发 stdin 脚本、能限时。`terminal` 的沙箱通道走这里 ——
/// AppContainer 用 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 启动，
/// **普通用户不需要任何特权**；而 `CreateProcessAsUserW` 在缺
/// SE_ASSIGNPRIMARYTOKEN / SE_INCREASE_QUOTA 的机器上恒报 1314，沙箱于是
/// 永远建不起来（2026-11-18 实况：整台机器上 terminal 从未进过沙箱）。
/// 低完整性 + 无 capability 让它连网络都出不去，隔离强度不低于受限 token。
/// [KNOWN BUG 2026-11-19] On this machine (Medium IL parent too), the spawn path in
/// this module corrupts the heap (STATUS_HEAP_CORRUPTION, process-level crash).
/// Pre-existing FFI defect, root cause not yet located (suspect: hand-rolled
/// StartupInfoExW / attribute-list layout). Do NOT wire this channel into any
/// long-lived process until fixed: execute_code fallback cannot catch it -
/// the crash is not an Err, the whole process dies.
pub fn run_capture(
    exe: &Path,
    args: &[&str],
    stdin_script: Option<&str>,
    workdir: &Path,
    timeout: Option<std::time::Duration>,
) -> io::Result<CollectResult> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
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

    // 2026-11-19（三处 FFI 修正，合起来是「AppContainer 通道必崩」的根因）：
    //
    // ① 属性列表必须**指针对齐**。原实现用 `vec![0u8; attr_size]` —— u8 的
    //    对齐是 1，而 PROC_THREAD_ATTRIBUTE_LIST 内部是指针数组，
    //    InitializeProcThreadAttributeList 会按 8 字节偏移写指针 → 错位写 →
    //    **堆损坏**（STATUS_HEAP_CORRUPTION）。改用 u64 数组（8 字节对齐）。
    // ② InitializeProcThreadAttributeList / UpdateProcThreadAttribute 的返回值
    //    原来全部丢弃 —— 失败时进程会**不带 AppContainer SID 启动**（等于没
    //    沙箱），而调用方以为它沙箱了。现在失败即报错。
    //
    let mut attr_size = 0usize;
    unsafe {
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size);
    }
    if attr_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("InitializeProcThreadAttributeList(size) failed: {}", io::Error::last_os_error()),
        ));
    }
    let words = attr_size.div_ceil(8).max(1);
    let mut attr_words = vec![0u64; words];
    let attr_list = attr_words.as_mut_ptr() as *mut ();
    unsafe {
        if InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size) == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("InitializeProcThreadAttributeList(init) failed: {}", io::Error::last_os_error()),
            ));
        }
        if UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            &mut sec_caps as *mut _ as *mut (),
            std::mem::size_of::<SecurityCapabilities>(),
            std::ptr::null(),
            std::ptr::null(),
        ) == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("UpdateProcThreadAttribute failed: {}", io::Error::last_os_error()),
            ));
        }
    }

    let siex = StartupInfoExW {
        startup: StartupInfoW {
            // 带 EXTENDED_STARTUPINFO_PRESENT 时 cb 必须是 STARTUPINFOEX 的
            // 尺寸（内核按 cb 读整个扩展结构 + attribute list）。
            cb: std::mem::size_of::<StartupInfoExW>() as u32,
            reserved: std::ptr::null_mut(),
            desktop: std::ptr::null_mut(),
            title: std::ptr::null_mut(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            count_chars: 0,
            count_chars_y: 0,
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

    let mut cmdline = wstr_cmdline(exe, &args);
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
            // CREATE_NO_WINDOW：父进程是 GUI 应用（hermix-desktop.exe 是
            // windows_subsystem="windows"），不带这个标志 spawn 控制台程序
            // 会新建一个**可见**的控制台窗口 —— 用户看到的就是"每执行一条
            // 命令弹一个黑窗"。AppContainer 内没有交互桌面可附着，但窗口
            // 仍会在用户桌面上闪一下，必须显式压掉。
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
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
        if let Some(script) = stdin_script {
            stdin.write_all(script.as_bytes())?;
        }
        // 无论有没有脚本，写端都要在这里关掉：留着 = 子进程永远等不到 EOF。
    }

    // 有界收集 stdout/stderr（各 256KB 上限），读线程与等待并行 ——
    // 先阻塞读再等退出会被"孙进程握着写端"卡死（terminal 的 cargo/rustc
    // 场景经常如此），先等再读则可能把超时判定拖成无限。
    fn drain(h: Handle) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            use std::io::Read;
            use std::os::windows::io::{FromRawHandle, OwnedHandle};
            let f = std::fs::File::from(unsafe { OwnedHandle::from_raw_handle(h as _) });
            let mut buf = Vec::new();
            let _ = f.take(256 * 1024).read_to_end(&mut buf);
            buf
        })
    }
    let out_thread = drain(stdout_parent);
    let err_thread = drain(stderr_parent);

    let wait_ms = timeout
        .map(|t| t.as_millis().min(u32::MAX as u128) as Dword)
        .unwrap_or(0xFFFF_FFFF);
    let mut timed_out = false;
    if unsafe { WaitForSingleObject(pi.process, wait_ms) } == WAIT_TIMEOUT {
        timed_out = true;
        // 超时先杀顶层，再让读线程随管道 EOF 自然收尾。
        unsafe { TerminateProcess(pi.process, 124) };
        unsafe { WaitForSingleObject(pi.process, 5_000) };
    }

    let mut exit_code: Dword = 0;
    unsafe { GetExitCodeProcess(pi.process, &mut exit_code) };
    unsafe { CloseHandle(pi.process) };

    let stdout_buf = out_thread.join().unwrap_or_default();
    let stderr_buf = err_thread.join().unwrap_or_default();

    Ok(CollectResult {
        exit_code: if timed_out { 124 } else { exit_code as i32 },
        stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
        timed_out,
    })
}

extern "system" {
    // ole32.dll —— DeriveAppContainerSid 的 SID 由 CoTaskMemAlloc 分配。
    fn CoTaskMemFree(pv: *mut ());
    // kernel32.dll —— 进程等待与退出码（同步收集）。
    fn WaitForSingleObject(h: Handle, ms: Dword) -> Dword;
    fn TerminateProcess(h: Handle, code: u32) -> Bool;
    fn GetExitCodeProcess(h: Handle, code: *mut Dword) -> Bool;
}