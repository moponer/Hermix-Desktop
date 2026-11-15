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
    ///    point at an arbitrary binary (e.g. for staging builds).
    /// 2. **Dev mode** (`cfg!(debug_assertions)`, i.e. `pnpm tauri
    ///    dev`): the single workspace's root `target/` only —
    ///    debug then release, **max-by-mtime** among the ones that
    ///    exist. first-exists 曾让陈旧副本永远命中（装着几小时前的
    ///    旧代码去测，浪费两天排查），同一教训也催生了
    ///    `scripts/check-sidecar-freshness.sh`。
    /// 3. **Release mode** (the bundled installer): `hermix.exe`
    ///    next to the running Tauri executable. The bundler is
    ///    expected to drop it there.
    fn hermix_bin_path() -> Result<PathBuf, String> {
        if let Ok(p) = std::env::var("HERMIX_SIDECAR_BIN") {
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }

        if cfg!(debug_assertions) {
            // 单 workspace 后产物只在仓库根 target/，历史布局
            // （release/target/、src-tauri/target/）一律不再候选。
            let ws_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
            let candidates = [
                ws_root.join("target/debug/hermix.exe"),
                ws_root.join("target/release/hermix.exe"),
            ];
            let best = candidates
                .iter()
                .filter(|p| p.exists())
                .max_by_key(|p| {
                    p.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                })
                .ok_or_else(|| {
                    format!(
                        "hermix binary not found; build it first:\n    cargo build -p hermix-server --bin hermix\n(searched: {})",
                        candidates
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            Ok(best.clone())
        } else {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            Ok(exe_dir.join("hermix.exe"))
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

        let bin = Self::hermix_bin_path()?;
        crate::desktop_log!("[Hermix] Looking for binary at: {}", bin.display());
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
                    crate::desktop_log!(
                        "[Hermix] sidecar exited during startup with status {:?}",
                        status
                    );
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(60)),
                Err(e) => {
                    crate::desktop_log!("[Hermix] try_wait error during startup probe: {e}");
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
            // 只可能发生在 BRIDGE 读写锁中毒时 —— 正常重启路径下
            // `init_global` 会用本代替换旧代并返回 true。
            let _ = child.kill();
            let _ = child.wait();
            return Err("stdio bridge lock poisoned — cannot hand over stdio".into());
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
                    crate::desktop_log!("[Hermix] Sidecar bound to Job Object (pid={pid})");
                } else {
                    crate::desktop_log!("[Hermix] Warning: failed to create Job Object for pid={pid}");
                }
            }
        }

        self.child = Some(child);
        self.stderr = None;
        crate::desktop_log!("[Hermix] Started sidecar in stdio mode");
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
                    crate::desktop_log!("[Hermix:{tag}] {line}");
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
    /// then hard-kill after the grace window. Mirrors the TCP-era shutdown
    /// handshake but the transport is one line of JSON instead of an HTTP
    /// request.
    ///
    /// 停机期间的 HTTP 请求会立刻拿 503 `backend offline`（`mark_dead`），前端
    /// 统一渲染成「后端未连接（正在重启）」——所以这里**先**发帧再等在退出，
    /// 顺序不能反。
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let started = std::time::Instant::now();
            let _ = Self::try_graceful_shutdown();

            // 先发帧、**随后立刻**标记本代已死，再去等子进程退。
            //
            // 顺序很关键：等待期最长 5s，这段时间子进程已经不再读 stdin。若等到
            // 退出后才 mark_dead，这 5s 里进来的 HTTP 请求会被写进黑洞并一直挂到
            // 桥的 120s / 前端 10s 死线（用户看到的是转圈），而不是立刻得到
            // 「后端正在重启」。`backend_http_request` / `backend_ready` 都看这个
            // 标志，所以前端能秒级转离线并在恢复后自愈。
            self.bridge_ready.store(false, Ordering::SeqCst);
            crate::stdio_bridge::mark_dead();

            // 5s（原 3s）：后端退出要做 IM 关连接 + 刷日志，3s 卡在临界点上，
            // 实测每次都走到强杀（见 desktop.log 的 Graceful shutdown timed out）。
            // 服务端已改为有界收尾（stdio_session.rs 里 IM 停止 2s 超时 +
            // main.rs 显式 exit），正常应在数百毫秒内退干净；留 5s 是给冷
            // SQLite checkpoint 的余量。
            let deadline = started + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        crate::desktop_log!(
                            "[Hermix] Sidecar exited gracefully (code={status:?}, {}ms)",
                            started.elapsed().as_millis()
                        );
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }

            if child.try_wait().ok().flatten().is_none() {
                crate::desktop_log!(
                    "[Hermix] Graceful shutdown timed out after {}ms, force killing sidecar",
                    started.elapsed().as_millis()
                );
                let _ = child.kill();
                let _ = child.wait();
                crate::desktop_log!("[Hermix] Sidecar stopped (force killed)");
            }
        }
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

    /// 健康度的两项分解，只用于把 watchdog 的失败日志写成可诊断的。
    /// 返回 `(bridge_ready, child_alive)`：前者 = 桥接上了这一代子进程，
    /// 后者 = 子进程还没退出。两者都真才算健康。
    pub fn health_detail(&mut self) -> (bool, bool) {
        (
            self.bridge_ready.load(Ordering::SeqCst),
            self.is_running(),
        )
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
                // 用真实 /health 往返判定就绪。注意探测的是**这一代**桥：
                // 重启后 `global()` 已指向新代，不会拿旧代的成功结果
                // 误判新代为就绪。
                // 预算收在桥内部（request_with_budget）：外层 timeout 丢
                // future 会跳过挂起项清理，探针越频繁泄漏越多。
                let probe = bridge
                    .request_with_budget("GET", "/health", None, Duration::from_secs(3))
                    .await;
                let http_ok = match probe {
                    Ok(reply) => serde_json::from_str::<serde_json::Value>(&reply)
                        .ok()
                        .and_then(|p| {
                            p.get("body")
                                .and_then(|b| b.as_str())
                                .map(|b| b.contains("\"status\":\"ok\"") || b.contains("status\":\"ok\""))
                        })
                        .unwrap_or(false),
                    _ => false,
                };
                if http_ok {
                    return true;
                }
                // 探测未通过不直接返回 true：那会让启动页在服务真正可用前
            // 就报"就绪"，用户随后发消息必然失败。继续在 deadline 内重试，
            // 把"慢启动"和"起不来"区分开。
                crate::desktop_log!("[Hermix] /health probe not ok yet, retrying…");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        crate::desktop_log!("[Hermix] wait_until_ready: deadline elapsed without a healthy /health");
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
    /// Health signal is two-layered:
    /// 1. every tick (cheap): `is_healthy()` — bridge initialised AND
    ///    child process has not exited (covers process exit, init failure);
    /// 2. every `HEALTH_PROBE_EVERY_TICKS` healthy ticks: a real `/health`
    ///    round-trip (covers hung sidecars — process alive but the tokio
    ///    runtime / executor never answers). Frequency is deliberately low
    ///    (≈30s) because each probe leaves one request_id in the server's
    ///    audit trail; a per-tick probe was rejected for exactly that
    ///    reason, but no probe at all left hung sidecars unrecoverable —
    ///    the frontend could detect offline yet nobody restarted it.
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
            // give-up 稳态下的静默 tick 计数（日志降噪用）。
            let mut gave_up_ticks: u32 = 0;
            // 健康 tick 计数：驱动低频 /health 探测（hung 型唯一可检手段）。
            let mut healthy_ticks: u32 = 0;
            const HEALTH_PROBE_EVERY_TICKS: u32 = 6;
            const HEALTH_PROBE_BUDGET: Duration = Duration::from_secs(2);

            loop {
                // 退出流程已经开始（`cleanup()` 停掉了 sidecar）：立刻收工。
                // 否则本循环会把「子进程没了」读成故障，6 次之后把刚停掉的
                // sidecar 重新拉起来 —— 2026-10-30 实锤：主窗销毁 → cleanup
                // 停 sidecar → 进程没退 → watchdog 15s 后重生它，两个进程
                // 一起留在任务管理器里。
                if crate::shutdown::is_cleaned() {
                    crate::desktop_log!("[Hermix] Watchdog: 退出流程已启动，停止看护");
                    break;
                }
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

                let (mut healthy, bridge_ready, child_alive) = {
                    let mut guard = arc.lock().expect("sidecar mutex poisoned");
                    let (ready, alive) = guard.health_detail();
                    (ready && alive, ready, alive)
                };

                // 低频 /health 往返：把「进程活着但不应答」的 hung 型送进
                // 同一条重启预算（此前它永远判健康、永不重启）。
                if healthy {
                    healthy_ticks = healthy_ticks.wrapping_add(1);
                    if healthy_ticks % HEALTH_PROBE_EVERY_TICKS == 0 {
                        if let Some(bridge) = crate::stdio_bridge::global() {
                            let probe_ok = bridge
                                .request_with_budget(
                                    "GET",
                                    "/health",
                                    None,
                                    HEALTH_PROBE_BUDGET,
                                )
                                .await
                                .map(|reply| {
                                    serde_json::from_str::<serde_json::Value>(&reply)
                                        .ok()
                                        .and_then(|v| v.get("status").and_then(|s| s.as_u64()))
                                        .is_some_and(|s| (200..300).contains(&s))
                                })
                                .unwrap_or(false);
                            if !probe_ok {
                                crate::desktop_log!(
                                    "[Hermix] Watchdog: /health probe failed while process alive (hung sidecar?)"
                                );
                                healthy = false;
                            }
                        }
                    }
                }

                if healthy {
                    consecutive_failures = 0;
                    total_restarts = 0;
                    continue;
                }

                consecutive_failures += 1;

                // give-up 稳态：重启预算耗尽且子进程仍不健康，条件不会自己变好。
                // 原实现每个 tick（15s）打两条日志，2026-11-04 安装版崩溃循环
                // 实机打出 400+ 条重复行，把首尾有用信息淹掉。这里每 20 个
                // tick（≈5min）打一条心跳；`hermix:server-gave-up` 事件照发
                // （前端 setGaveUp 幂等，靠它维持「重启失败，点击重试」UI）。
                if total_restarts >= max_restarts && consecutive_failures >= max_failures {
                    gave_up_ticks += 1;
                    if gave_up_ticks == 1 || gave_up_ticks % 20 == 0 {
                        // 必须走 desktop_log!：GUI 进程的 eprintln! 没有去处，
                        // 2026-11-04 崩溃循环正是靠这些行定案的。
                        crate::desktop_log!(
                            "[Hermix] Watchdog: giving up after {total_restarts} restarts; \
                             still unhealthy (bridge_ready={bridge_ready} child_alive={child_alive}, \
                             quiet tick {gave_up_ticks}). Manual restart required."
                        );
                    }
                    if let Some(ref handle) = app_handle {
                        let _ = handle.emit("hermix:server-gave-up", ());
                    }
                    continue;
                }

                // 带上分解项：`bridge_ready=false child_alive=false` = 没有子进程，
                // `bridge_ready=false child_alive=true` = 子进程在但桥没接上（最
                // 难查的一种）。此前只有一句 "failed (n/30)"，无法区分。
                crate::desktop_log!(
                    "[Hermix] Sidecar health check failed ({consecutive_failures}/{max_failures}) \
                     bridge_ready={bridge_ready} child_alive={child_alive}"
                );

                if consecutive_failures >= max_failures {
                    crate::desktop_log!(
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
                        crate::desktop_log!("[Hermix] Watchdog: restart() failed: {e}");
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
            // 6 次（≈ 5s + 5×2s backoff ≈ 15s，原 30 次≈60s）。
            //
            // 旧值按「冷启动要 70s 才应答 /health」标定，但 `is_healthy()` 根本不
            // 探 /health —— 它只看 `bridge_ready`（spawn 后立刻置位）+ 子进程是否
            // 存活，慢启动不会消耗失败预算。所以失败只可能来自「子进程没了」或
            // 「桥没接上」，那是真故障，等 60s 只会让用户对着一个死界面狂点。
            // 2026-10-29 实测：sidecar 被停后 60s 才重启，期间所有请求都在报错。
            max_consecutive_failures: 6,
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

    /// 字段顺序与宽度必须逐字节对齐 `JOBOBJECT_BASIC_LIMIT_INFORMATION`。
    /// 这里的 `LimitFlags` 是**第 3 个**字段（offset 16）；曾把它写在末尾
    /// 并省掉该字段，导致结构体只有 136 字节（权威值 144），
    /// `SetInformationJobObject` 返回 `ERROR_BAD_LENGTH (24)`，
    /// KILL_ON_JOB_CLOSE 从未真正生效。
    #[repr(C)]
    struct JobObjectBasicLimitInformation {
        _per_process_user_time_limit: i64,
        _per_job_user_time_limit: i64,
        limit_flags: u32,
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
        fn GetLastError() -> u32;
    }

    pub unsafe fn create_and_assign(pid: u32) -> Option<usize> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }

            let info = JobObjectExtendedLimitInformation {
                basic_limit_information: JobObjectBasicLimitInformation {
                    _per_process_user_time_limit: 0,
                    _per_job_user_time_limit: 0,
                    limit_flags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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
            let class = 9; // JobObjectExtendedLimitInformation
            // 必须查返回值：结构体布局写错时这里会以 ERROR_BAD_LENGTH 失败，
            // 但 AssignProcessToJobObject 仍会成功 —— Job 里没有任何限制，
            // 于是「绑定成功」的假象掩盖了「兜底不存在」。静默失败过一次，
            // 代价是 sidecar 在父进程 exit() 后长期残留，不再重蹈。
            let set_ok = SetInformationJobObject(
                job,
                class,
                &info as *const _ as *const c_void,
                std::mem::size_of_val(&info) as u32,
            );
            if set_ok == 0 {
                eprintln!(
                    "[Hermix] Warning: SetInformationJobObject failed (err={}), \
                     KILL_ON_JOB_CLOSE not armed — sidecar would outlive the app",
                    GetLastError()
                );
                CloseHandle(job);
                return None;
            }

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

    #[cfg(test)]
    mod layout_tests {
        use super::*;

        /// 结构体布局是 `SetInformationJobObject` 的入参契约：尺寸差一个
        /// 字节它就返回 `ERROR_BAD_LENGTH (24)`，而 `AssignProcessToJobObject`
        /// 照样成功 —— 兜底静默消失，sidecar 在父进程退出后残留。
        /// 曾经 BasicLimit 少了 `LimitFlags` 字段（56 vs 64），
        /// Extended 落到 136 vs 144。这两个数字就是契约本身。
        #[test]
        fn job_object_structs_match_windows_abi() {
            assert_eq!(
                std::mem::size_of::<JobObjectBasicLimitInformation>(),
                64,
                "JOBOBJECT_BASIC_LIMIT_INFORMATION must be 64 bytes on x64 — \
                 check that `limit_flags` is the 3rd field and no field is missing"
            );
            assert_eq!(
                std::mem::size_of::<JobObjectExtendedLimitInformation>(),
                144,
                "JOBOBJECT_EXTENDED_LIMIT_INFORMATION must be 144 bytes on x64"
            );
        }

        /// `limit_flags` 必须在 offset 16（两个 i64 之后）。
        /// 这个偏移一旦挪位，KILL_ON_JOB_CLOSE 就写进了别的字段。
        #[test]
        fn limit_flags_sits_at_offset_16() {
            let info = JobObjectExtendedLimitInformation {
                basic_limit_information: JobObjectBasicLimitInformation {
                    _per_process_user_time_limit: 0,
                    _per_job_user_time_limit: 0,
                    limit_flags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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
            let base = &info as *const _ as usize;
            let flags = &info.basic_limit_information.limit_flags as *const _ as usize;
            assert_eq!(
                flags - base,
                16,
                "limit_flags must sit right after the two i64 time limits"
            );
            assert_eq!(info.basic_limit_information.limit_flags, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);
        }
    }
}
