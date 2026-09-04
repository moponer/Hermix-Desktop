//! Hermix desktop sidecar — spawns `hermix.exe --stdio` and bridges
//! its stdin/stdout to the frontend via `stdio_bridge`.
//!
//! History: this file previously launched hermix over a TCP socket
//! (`127.0.0.1:3000`) and the frontend talked REST + WebSocket directly.
//! The port-binding model broke two things at once: (1) running two
//! Hermix sessions on the same host fought over port 3000, (2) dev mode
//! alongside another Tauri app on port 1420 needed special-casing.
//! Migrated to stdio JSON Lines (see `docs/transport-stdio-migration.md`).
//! The transport negotiation is one-shot at spawn: the child is launched
//! with `--stdio`, its stdin/stdout are handed to the bridge, and the
//! HTTP/WebSocket surface is dead from the desktop sidecar's point of
//! view. External `hermic serve` invocations on TCP are unaffected —
//! they go through `hermix-server/src/main.rs` without the flag.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;

// 模块级: 供 sidecar spawn (hermix.exe) 与孤儿扫描 (netstat/tasklist/
// taskkill) 共用。Windows GUI 应用 spawn console 子进程时必须带
// CREATE_NO_WINDOW (0x08000000), 否则每次启动/扫描都会闪黑色命令行窗。
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub struct HermixSidecar {
    child: Option<Child>,
    stderr: Option<ChildStderr>,
    /// True after `stdio_bridge::init_global` succeeded with this child's
    /// stdin/stdout. Watchdog / health-check consult this for fast readiness.
    bridge_ready: Arc<AtomicBool>,
    /// Windows Job Object handle — binds the child process lifetime
    /// to the parent. When the parent exits (or Drop runs), all
    /// processes in the job are terminated automatically, preventing
    /// orphan hermix.exe processes from squatting on the port.
    #[cfg(windows)]
    job_handle: Option<usize>,
}

impl HermixSidecar {
    pub fn new() -> Self {
        Self {
            child: None,
            stderr: None,
            bridge_ready: Arc::new(AtomicBool::new(false)),
            #[cfg(windows)]
            job_handle: None,
        }
    }

    /// Handle to the bridge-ready flag. Watchdog / startup probe waits on
    /// this becoming true (set inside `start` after a successful
    /// `stdio_bridge::init_global`).
    pub fn bridge_ready_handle(&self) -> Arc<AtomicBool> {
        self.bridge_ready.clone()
    }

    /// Resolve path to `hermix.exe` (the Rust CLI binary the Tauri
    /// shell spawns as a sidecar).
    ///
    /// Resolution order:
    /// 1. **`HERMIX_SIDECAR_BIN` env var**, if set. Lets the user
    ///    point at an arbitrary binary (e.g. for staging builds or
    ///    `pnpm tauri dev` with a custom cargo target dir).
    /// 2. **Dev mode** (`cfg!(debug_assertions)`, i.e. `pnpm tauri
    ///    dev`): look for the debug build at the workspace root —
    ///    `target/debug/hermix.exe` first, then fall back to
    ///    `target/release/hermix.exe` (some developers keep a
    ///    release build around for perf reasons).
    /// 3. **Release mode** (the bundled installer): `hermix.exe`
    ///    next to the running Tauri executable. The bundler is
    ///    expected to drop it there.
    fn hermix_bin_path() -> PathBuf {
        if let Ok(p) = std::env::var("HERMIX_SIDECAR_BIN") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }

        if cfg!(debug_assertions) {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let ws_root = manifest_dir.join("../../..");
            let debug = ws_root.join("release/target/debug/hermix.exe");
            if debug.exists() {
                return debug;
            }
            let debug_fallback = ws_root.join("target/debug/hermix.exe");
            if debug_fallback.exists() {
                return debug_fallback;
            }
            ws_root.join("release/target/release/hermix.exe")
        } else {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            exe_dir.join("hermix.exe")
        }
    }

    /// Spawn `hermix.exe --stdio`. Stdin/stdout are handed to the global
    /// stdio bridge so the Tauri commands and the rest of the desktop app
    /// can route REST + streaming frames through a single async multiplex.
    /// Stderr stays attached to a logging drain (so the user still sees
    /// panics / config errors in the host process log).
    pub fn start(&mut self, app: &tauri::AppHandle) -> Result<(), String> {
        if self.child.is_some() {
            return Ok(()); // Already running
        }

        let bin = Self::hermix_bin_path();
        eprintln!("[Hermix] Looking for binary at: {}", bin.display());
        if !bin.exists() {
            return Err(format!("hermix binary not found at {}", bin.display()));
        }

        // Ensure the four portable subdirs exist before spawning the
        // sidecar. The launcher (`portable_env::install_portable_env`)
        // is supposed to have done this already in `setup`, but we
        // re-verify here as a defence-in-depth.
        if let Some(data_dir) = crate::portable_env::portable_data_dir() {
            Self::ensure_portable_subdirs(&data_dir);
        }

        // Reset the readiness flag before spawn so a stale "ready"
        // from a previous generation cannot leak.
        self.bridge_ready.store(false, Ordering::SeqCst);

        // Build the child process. We rely on the launcher having
        // already injected the portable env vars (HERMIX_HOME,
        // HERMIX_PORTABLE_DATA_DIR, etc.) — Command::new does NOT
        // clear the parent env, so the child inherits all of them.
        let mut cmd = Command::new(&bin);
        cmd.arg("--stdio")
            .stdin(Stdio::piped())  // bridge writes frames here
            .stdout(Stdio::piped()) // bridge reads frames here
            .stderr(Stdio::piped()) // drained separately to stderr
            .creation_flags(CREATE_NO_WINDOW);
        for (k, v) in crate::portable_env::current_portable_env() {
            cmd.env(k, v);
        }
        cmd.env("ORT_DISABLE_DML", "1");

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn hermix: {e}"))?;

        // Crash-on-startup probe (the original 4×60ms try_wait loop).
        // The child can exit almost instantly if a runtime DLL is
        // missing, the config is unparseable, or the data dir is
        // read-only. Without this probe we would hand a dead child to
        // the bridge, the next `wait_until_ready` would hang for 30s,
        // and `stop()` would silently succeed.
        let mut crashed = false;
        for _ in 0..4 {
            match child.try_wait() {
                Ok(Some(status)) => {
                    crashed = true;
                    eprintln!(
                        "[Hermix] sidecar exited during startup with status {:?}",
                        status
                    );
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(60)),
                Err(e) => {
                    eprintln!("[Hermix] try_wait error during startup probe: {e}");
                    break;
                }
            }
        }
        if crashed {
            let mut err_msg = String::from("hermix exited during startup");
            if let Some(mut stderr) = child.stderr.take() {
                use std::io::Read;
                let mut buf = String::new();
                if stderr.read_to_string(&mut buf).is_ok() && !buf.trim().is_empty() {
                    err_msg.push_str(": ");
                    err_msg.push_str(buf.trim());
                }
            }
            let _ = child.wait();
            return Err(err_msg);
        }

        // Hand stdin/stdout to the global stdio bridge. From this point
        // onward, every HTTP request / WS-style stream frame flows
        // through `bridge.http_request(...)` / `bridge.send_to_channel(...)`.
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "no stdin from hermix child".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "no stdout from hermix child".to_string())?;
        let stderr = child.stderr.take();

        if !crate::stdio_bridge::init_global(app.clone(), stdin, stdout) {
            // Bridge already initialised — a previous sidecar generation
            // is somehow still hanging around. Kill the new child and
            // surface the diagnostic so `setup` aborts instead of
            // creating a zombie.
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "stdio bridge already initialised — a previous sidecar generation is still alive"
                    .into(),
            );
        }
        self.bridge_ready.store(true, Ordering::SeqCst);

        // stderr stays as a logging drain — the bridge owns stdout.
        if let Some(err) = stderr {
            Self::spawn_drain("stderr", err);
        }

        // Windows Job Object: bind child lifetime to parent.
        #[cfg(windows)]
        {
            let pid = child.id();
            unsafe {
                if let Some(handle) = windows_job_object::create_and_assign(pid) {
                    self.job_handle = Some(handle);
                    eprintln!("[Hermix] Sidecar bound to Job Object (pid={pid})");
                } else {
                    eprintln!("[Hermix] Warning: failed to create Job Object for pid={pid}");
                }
            }
        }

        self.child = Some(child);
        self.stderr = None;
        eprintln!("[Hermix] Started sidecar in stdio mode");
        Ok(())
    }

    /// Drain a child stderr pipe to host stderr so panic messages and
    /// config-load diagnostics surface in the parent log.
    fn spawn_drain(tag: &'static str, pipe: impl Read + Send + 'static) {
        std::thread::Builder::new()
            .name(format!("hermix-{tag}-drain"))
            .spawn(move || {
                use std::io::{BufRead, BufReader};
                let reader = BufReader::new(pipe);
                for line in reader.lines().map_while(Result::ok) {
                    eprintln!("[Hermix:{tag}] {line}");
                }
            })
            .ok();
    }

    /// Make sure the four portable subdirs (`data`, `cache`, `logs`,
    /// `webview-cache`) exist under the install root. Best-effort:
    /// failures are logged to stderr but never propagated.
    fn ensure_portable_subdirs(data_dir: &std::path::Path) {
        let install_root = data_dir.parent().unwrap_or(data_dir);
        for sub in ["data", "cache", "logs", "webview-cache"] {
            let p = install_root.join(sub);
            if let Err(e) = std::fs::create_dir_all(&p) {
                eprintln!(
                    "[Hermix] warning: cannot create portable subdir {}: {e}",
                    p.display()
                );
            }
        }
    }

    /// Try graceful shutdown first (write `{"channel":"shutdown",…}` on
    /// stdin so the server can flush SQLite WAL and stop its TickLoop),
    /// then hard-kill after 3s. Mirrors the TCP-era shutdown handshake
    /// but the transport is one line of JSON instead of an HTTP request.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = Self::try_graceful_shutdown();

            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        eprintln!("[Hermix] Sidecar stopped gracefully");
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(_) => break,
                }
            }

            if child.try_wait().ok().flatten().is_none() {
                eprintln!("[Hermix] Graceful shutdown timed out, force killing sidecar");
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("[Hermix] Sidecar stopped (force killed)");
            } else {
                eprintln!("[Hermix] Sidecar stopped");
            }
        }
        // Drop the bridge readiness flag so callers stop treating this
        // generation as healthy.
        self.bridge_ready.store(false, Ordering::SeqCst);
        // Release Job Object handle (triggers KILL_ON_JOB_CLOSE if any
        // child processes remain, but we already killed above).
        #[cfg(windows)]
        if let Some(handle) = self.job_handle.take() {
            unsafe { windows_job_object::close(handle); }
        }
        self.stderr.take();
    }

    /// Send a one-shot shutdown frame on the bridge's stdin. Best-effort:
    /// if the bridge is gone (already torn down) this is a silent no-op.
    fn try_graceful_shutdown() {
        if let Some(bridge) = crate::stdio_bridge::global() {
            let _ = bridge.send_to_channel("shutdown", "");
        }
    }

    /// Stop (if running) and start a fresh instance. Used by the desktop
    /// UI's "restart" button when the backend goes offline. The bridge
    /// becomes invalid the moment `stop()` drains stdout, so any
    /// in-flight HTTP request will receive its 503 wakeup; a follow-up
    /// `start()` re-initialises the bridge.
    pub fn restart(&mut self, app: &tauri::AppHandle) -> Result<(), String> {
        self.stop();
        self.start(app)
    }

    /// True when a child handle exists AND has not exited. Cheaper than
    /// `is_healthy()` because it does not consult the atomic flag.
    /// `try_wait` takes `&mut Child`; the borrow is scoped to this
    /// expression and released before the function returns.
    pub fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| c.try_wait().ok().flatten().is_none())
            .unwrap_or(false)
    }

    /// Bridge is initialised AND child is alive. This is the readiness
    /// gate the startup probe and the watchdog wait on.
    pub fn is_healthy(&mut self) -> bool {
        self.bridge_ready.load(Ordering::SeqCst) && self.is_running()
    }

    /// Round-trip an HTTP `/health` through the bridge until the server
    /// responds or the deadline elapses. Used by the setup probe so the
    /// `hermix:server-ready` event fires only after the multiplex is
    /// actually serving requests, not just after `spawn()` returned.
    ///
    /// Free function (not `&self` method) because the bridge handle is
    /// global (`crate::stdio_bridge::global()`) and we deliberately
    /// don't want a caller-side `MutexGuard<HermixSidecar>` borrowing
    /// `self` to survive across the `.await` — that would be `!Send`
    /// and break the `tauri::async_runtime::spawn` boundary.
    pub async fn wait_until_ready(deadline: Duration) -> bool {
        let start = std::time::Instant::now();
        while std::time::Instant::now() < start + deadline {
            if let Some(bridge) = crate::stdio_bridge::global() {
                if let Ok(reply) = bridge.http_request("GET", "/health", None).await {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&reply) {
                        let body = parsed
                            .get("body")
                            .and_then(|b| b.as_str())
                            .unwrap_or("");
                        if body.contains("\"status\":\"ok\"") || body.contains("status\":\"ok\"") {
                            return true;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        false
    }

    /// Best-effort health probe used by `get_server_status`. Mirrors
    /// `wait_until_ready` but with a 1-shot 2s budget.
    pub async fn health_check() -> bool {
        Self::wait_until_ready(Duration::from_secs(2)).await
    }

    /// Start a background watchdog that periodically probes the bridge
    /// and restarts the sidecar after `max_consecutive_failures`
    /// consecutive failures. The returned `JoinHandle` aborts the
    /// watcher when dropped (i.e. when the Tauri shell exits).
    ///
    /// Health signal: `is_healthy()` — bridge initialised AND child
    /// process has not exited. We deliberately do NOT round-trip an HTTP
    /// `/health` every tick because that would leak HTTP request_ids
    /// into the server's audit trail every 5 seconds; the bridge_ready
    /// flag + `child.try_wait()` cover all failure modes the watchdog
    /// actually cares about (process exit, init failure).
    pub fn start_watchdog(
        sb: &Arc<std::sync::Mutex<Self>>,
        config: WatchdogConfig,
        app_handle: Option<tauri::AppHandle>,
    ) -> tauri::async_runtime::JoinHandle<()> {
        let arc = sb.clone();
        tauri::async_runtime::spawn(async move {
            let interval = config.check_interval;
            let max_failures = config.max_consecutive_failures;
            let max_restarts = config.max_restarts_before_give_up;
            let mut consecutive_failures: u32 = 0;
            let mut total_restarts: u32 = 0;

            loop {
                let backoff_secs = match total_restarts {
                    0 => 2,
                    1 => 5,
                    _ => 15,
                };
                if consecutive_failures == 0 {
                    tokio::time::sleep(interval).await;
                } else {
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                }

                let healthy = {
                    let mut guard = arc.lock().expect("sidecar mutex poisoned");
                    guard.is_healthy()
                };

                if healthy {
                    consecutive_failures = 0;
                    total_restarts = 0;
                    continue;
                }

                consecutive_failures += 1;
                eprintln!(
                    "[Hermix] Sidecar health check failed ({consecutive_failures}/{max_failures})"
                );

                if consecutive_failures >= max_failures {
                    if total_restarts >= max_restarts {
                        eprintln!(
                            "[Hermix] Watchdog: giving up after {} restarts. Manual restart required.",
                            total_restarts
                        );
                        if let Some(ref handle) = app_handle {
                            let _ = handle.emit("hermix:server-gave-up", ());
                        }
                        continue;
                    }

                    eprintln!(
                        "[Hermix] Watchdog: restarting sidecar (attempt {}/{}, backoff {}s)",
                        total_restarts + 1,
                        max_restarts,
                        backoff_secs
                    );
                    if let Some(ref handle) = app_handle {
                        let _ = handle.emit("hermix:server-restarting", total_restarts + 1);
                    }
                    let app_for_restart = app_handle.clone();
                    let mut guard = arc.lock().expect("sidecar mutex poisoned");
                    let restart_result = match app_for_restart.as_ref() {
                        Some(app) => guard.restart(app),
                        None => {
                            // No AppHandle — we cannot re-init the bridge
                            // because init_global needs an AppHandle to
                            // emit frames to. Surface a clear error so
                            // the operator understands why the watchdog
                            // is firing blanks.
                            Err("watchdog restart needs the original AppHandle".into())
                        }
                    };
                    if let Err(e) = restart_result {
                        eprintln!("[Hermix] Watchdog: restart() failed: {e}");
                    } else {
                        consecutive_failures = 0;
                        total_restarts += 1;
                    }
                }
            }
        })
    }
}

/// Configuration for the sidecar watchdog loop.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// How often to poll `is_healthy()`.
    pub check_interval: Duration,
    /// Restart after this many consecutive failures.
    pub max_consecutive_failures: u32,
    /// Maximum number of auto-restarts before giving up (requires manual restart).
    pub max_restarts_before_give_up: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            // Same calibration as the TCP era: a cold start of `hermix.exe`
            // can take ~70s (embedding model load, toolset init) before
            // the bridge round-trips a `/health` response. 30 failures ×
            // 5s = 150s tolerance window: a cold start always wins, a
            // genuinely hung sidecar still restarts after 2.5min.
            max_consecutive_failures: 30,
            max_restarts_before_give_up: 5,
        }
    }
}

impl Drop for HermixSidecar {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Windows Job Object FFI (prevents orphan child processes)
// ---------------------------------------------------------------------------

/// Raw Windows API declarations for Job Object lifecycle.
/// We use raw FFI to avoid adding the `windows` crate dependency.
#[cfg(windows)]
mod windows_job_object {
    use std::ffi::c_void;

    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[repr(C)]
    struct JobObjectExtendedLimitInformation {
        basic_limit_information: JobObjectBasicLimitInformation,
        _io_info: IoCounter,
        _process_memory_limit: usize,
        _job_memory_limit: usize,
        _peak_process_memory_used: usize,
        _peak_job_memory_used: usize,
    }

    #[repr(C)]
    struct JobObjectBasicLimitInformation {
        _process_user_mode_time: i64,
        _process_kernel_mode_time: i64,
        _min_working_set_size: usize,
        _max_working_set_size: usize,
        _active_process_limit: u32,
        _affinity: usize,
        _priority_class: u32,
        _scheduling_class: u32,
    }

    #[repr(C)]
    struct IoCounter {
        _read_operation_count: u64,
        _write_operation_count: u64,
        _other_operation_count: u64,
        _read_transfer_count: u64,
        _write_transfer_count: u64,
        _other_transfer_count: u64,
    }

    extern "system" {
        fn CreateJobObjectW(lpjobattributes: *const c_void, lpname: *const c_void) -> *mut c_void;
        fn SetInformationJobObject(
            hjob: *mut c_void,
            jobobjectinfoclass: u32,
            lpjobobjectinfo: *const c_void,
            cbjobobjectinfolength: u32,
        ) -> i32;
        fn AssignProcessToJobObject(hjob: *mut c_void, hprocess: *mut c_void) -> i32;
        fn CloseHandle(hobject: *mut c_void) -> i32;
    }

    pub unsafe fn create_and_assign(pid: u32) -> Option<usize> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }

            let mut info = JobObjectExtendedLimitInformation {
                basic_limit_information: JobObjectBasicLimitInformation {
                    _process_user_mode_time: 0,
                    _process_kernel_mode_time: 0,
                    _min_working_set_size: 0,
                    _max_working_set_size: 0,
                    _active_process_limit: 0,
                    _affinity: 0,
                    _priority_class: 0,
                    _scheduling_class: 0,
                },
                _io_info: IoCounter {
                    _read_operation_count: 0,
                    _write_operation_count: 0,
                    _other_operation_count: 0,
                    _read_transfer_count: 0,
                    _write_transfer_count: 0,
                    _other_transfer_count: 0,
                },
                _process_memory_limit: 0,
                _job_memory_limit: 0,
                _peak_process_memory_used: 0,
                _peak_job_memory_used: 0,
            };
            info.basic_limit_information._priority_class = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let class = 9; // JobObjectExtendedLimitInformation
            SetInformationJobObject(job, class, &info as *const _ as *const c_void, std::mem::size_of_val(&info) as u32);

            const PROCESS_SET_QUOTA: u32 = 0x0100;
            const PROCESS_TERMINATE: u32 = 0x0001;
            extern "system" {
                fn OpenProcess(dwdesiredaccess: u32, binherithandle: i32, dwprocessid: u32) -> *mut c_void;
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                CloseHandle(job);
                return None;
            }

            let ok = AssignProcessToJobObject(job, process);
            CloseHandle(process);

            if ok == 0 {
                CloseHandle(job);
                return None;
            }

            Some(job as usize)
        }
    }

    pub unsafe fn close(handle: usize) {
        unsafe {
            CloseHandle(handle as *mut c_void);
        }
    }
}
