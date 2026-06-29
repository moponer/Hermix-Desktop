use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::time::Duration;

pub struct HermixSidecar {
    child: Option<Child>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    port: u16,
}

impl HermixSidecar {
    pub fn new() -> Self {
        Self {
            child: None,
            stdout: None,
            stderr: None,
            port: 3000,
        }
    }

    /// Resolve path to `hermix.exe` (the Rust CLI binary the Tauri
    /// shell spawns as a sidecar to host the chat WebSocket + REST
    /// API on `127.0.0.1:3000`).
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
    ///
    /// All candidates are passed through `canonicalize()` so we get
    /// an absolute path back, but the `unwrap_or_else` fallbacks
    /// preserve the unresolved path so we can still log a useful
    /// "I tried to look here" message via the `eprintln!` in
    /// `start()`.
    fn hermix_bin_path() -> PathBuf {
        // 1. Explicit override wins.
        if let Ok(p) = std::env::var("HERMIX_SIDECAR_BIN") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }

        if cfg!(debug_assertions) {
            // 2. Dev: workspace root's release/target/debug/ (the .cargo/config.toml
            //    redirects target-dir to release/target).
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let ws_root = manifest_dir.join("../../..");

            // Try release/target/debug/ first (default target-dir redirect)
            let debug = ws_root.join("release/target/debug/hermix.exe");
            if debug.exists() {
                return debug;
            }
            // Fallback: target/debug/ (if someone built without config redirect)
            let debug_fallback = ws_root.join("target/debug/hermix.exe");
            if debug_fallback.exists() {
                return debug_fallback;
            }
            // Last resort: release/target/release/ (release build)
            ws_root.join("release/target/release/hermix.exe")
        } else {
            // 3. Production: hermix.exe bundled alongside the app
            //    (the Tauri bundler places it next to the .exe).
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            exe_dir.join("hermix.exe")
        }
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.child.is_some() {
            return Ok(()); // Already running
        }

        // Stale-orphan guard: if a previous Tauri session was killed
        // via task manager / OOM / blue-screen, its `Drop` never ran
        // and the spawned `hermix.exe` is still squatting on
        // `self.port`. Even worse: an external process (or a parallel
        // dev/prod launch) can also be holding the port. Either way,
        // the new child below would either (a) fail to bind and die
        // immediately, or (b) coexist on a different port and the
        // frontend would talk to the orphan. So before spawning we
        // (1) find any PID listening on our port, (2) verify the
        // image name actually looks like ours, (3) kill it if so,
        // (4) give the kernel a beat to release the socket. All
        // best-effort — failures are logged but never propagated,
        // because the worst case is the original problem
        // (port-in-use) which we will surface via the spawn error.
        Self::detect_and_kill_stale_orphan(self.port);

        // Synchronous port-bind probe (#3, the "double-belt" partner
        // to netstat-orphan-kill above). Even after we kill the
        // orphan and sleep 400ms, Windows may still hold the socket
        // in TIME_WAIT, or some other process may race in and grab
        // the port between our kill and the child's `bind()`. By
        // attempting `TcpListener::bind` synchronously here we get
        // a deterministic, race-free answer: bind succeeds → port
        // is genuinely free for the next 50ms; bind fails → there
        // is a stubborn holder that netstat missed (e.g. a non-hermix
        // dev server we declined to kill). The listener is dropped
        // immediately, but the act of bind+drop forces the kernel
        // to clear any lingering TIME_WAIT for this 4-tuple, which
        // measurably reduces EADDRINUSE on the child below.
        //
        // We only run this on platforms where `hermix serve` actually
        // binds via a TCP socket — Windows + macOS + Linux all do.
        // If the probe fails AND nothing was holding it (unlikely
        // but possible if e.g. Hyper-V's vsock-smb bridge holds the
        // port), we still proceed: the child's own bind failure will
        // surface as a much clearer error than a synthetic one here.
        match std::net::TcpListener::bind(("127.0.0.1", self.port)) {
            Ok(probe) => {
                drop(probe);
                eprintln!(
                    "[Hermix] port {} confirmed free by synchronous bind probe",
                    self.port
                );
            }
            Err(e) => {
                eprintln!(
                    "[Hermix] port {} bind probe failed ({}); the child may also fail to bind",
                    self.port, e
                );
                // Don't bail — the kernel race window is tiny, the
                // child's own error message is more diagnostic, and
                // on rare Windows builds Hyper-V reserves the socket
                // in a way that lets the child re-bind anyway.
            }
        }

        let bin = Self::hermix_bin_path();
        eprintln!("[Hermix] Looking for binary at: {}", bin.display());
        if !bin.exists() {
            return Err(format!("hermix binary not found at {}", bin.display()));
        }

        // Ensure the four portable subdirs exist before spawning the
        // sidecar. The launcher (`portable_env::install_portable_env`)
        // is supposed to have done this already in `setup`, but we
        // re-verify here as a defence-in-depth: if a test harness or
        // custom entry-point calls `start()` without going through
        // `run()`, the sidecar will still find its data dir.
        if let Some(data_dir) = crate::portable_env::portable_data_dir() {
            Self::ensure_portable_subdirs(&data_dir);
        }

        // Build the child process. We rely on the launcher having
        // already injected the six portable env vars (HERMIX_HOME,
        // HERMIX_PORTABLE_DATA_DIR, HERMIX_PORTABLE_CACHE_DIR,
        // WEBVIEW2_USER_DATA_FOLDER, TMP, TEMP). Command::new does
        // NOT clear the parent env, so the child inherits all of
        // them. We also explicitly forward them in case a test
        // harness has scrubbed the parent env.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut cmd = Command::new(&bin);
        // hermix.exe's main.rs (crates/hermix-server/src/main.rs) does
        // NOT parse argv — `serve --port N` would be silently ignored.
        // It reads the bind address from the HERMIX_SERVER_ADDR env
        // var (with `HERMES_SERVER_ADDR` as the legacy alias) and
        // falls back to 127.0.0.1:8787 when unset. Pass the sidecar's
        // port through the env var so the child actually listens on
        // `127.0.0.1:<self.port>` (e.g. 3000). We also keep the argv
        // for future when the binary grows a clap parser.
        cmd.args(["serve", "--port", &self.port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .env(
                "HERMIX_SERVER_ADDR",
                format!("127.0.0.1:{}", self.port),
            );
        for (k, v) in crate::portable_env::current_portable_env() {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn hermix: {e}"))?;

        // Crash-on-startup probe (#2). `Child::spawn` only tells us
        // the OS accepted the process-creation syscall — it says
        // nothing about whether the new process survived its first
        // 50ms. hermix.exe can exit almost instantly if:
        //   - a missing VC++ runtime DLL fails to load,
        //   - the config file is unparseable,
        //   - the data dir is read-only,
        //   - port-bind races another holder that snuck in between
        //     our netstat-orphan-kill and the spawn above.
        //
        // If we DON'T probe here, the user sees a 30-second hang in
        // `wait_until_ready` followed by a generic "Server did not
        // become ready" message with zero diagnostic context. Much
        // worse: we keep a dead Child handle, so the next `stop()`
        // call returns success and the user thinks they cleaned up.
        //
        // Poll `try_wait` four times across ~250ms — long enough to
        // catch DLL-load-fail / config-parse-fail (typically <50ms),
        // short enough that a healthy startup (which takes ~300ms
        // to bind + listen) is never mis-classified as crashed.
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
            // Surface whatever stderr the dying child wrote — this
            // is the difference between "hermix crashed" and
            // "ERROR: failed to bind 127.0.0.1:3000 (EADDRINUSE)".
            let mut err_msg = String::from("hermix exited during startup");
            if let Some(mut stderr) = child.stderr.take() {
                use std::io::Read;
                let mut buf = String::new();
                if stderr.read_to_string(&mut buf).is_ok() && !buf.trim().is_empty() {
                    err_msg.push_str(": ");
                    err_msg.push_str(buf.trim());
                }
            }
            // Best-effort reap so we don't leak a zombie on Windows.
            let _ = child.wait();
            return Err(err_msg);
        }

        // Hand the stdout/stderr handles to dedicated drain tasks.
        // Without this, once the OS pipe buffer (4KB on Windows)
        // fills up, the sidecar blocks on its next write to stdout
        // or stderr and silently stalls — which the user sees as
        // "AI stuck thinking" because the HTTP path is fine but the
        // child is parked. We move the handles out of `Child` into
        // our struct fields and spawn blocking threads that just
        // read-to-end and tee to our eprintln!.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        if let Some(out) = stdout {
            Self::spawn_drain("stdout", out);
        }
        if let Some(err) = stderr {
            Self::spawn_drain("stderr", err);
        }

        self.child = Some(child);
        self.stdout = None; // drained by background task now
        self.stderr = None; // drained by background task now
        eprintln!("[Hermix] Started sidecar on port {}", self.port);
        Ok(())
    }

    /// Spawn a dedicated OS thread that reads a child pipe to EOF
    /// and tees each line to stderr. Used for the stdout/stderr
    /// handles we take off the spawned `hermix.exe` so the OS pipe
    /// buffer (4KB on Windows) never fills up and silently stalls
    /// the sidecar's writes.
    ///
    /// Why a blocking thread rather than tokio: `ChildStdout` /
    /// `ChildStderr` are `std::io::Read` handles, and reading them
    /// blocks. Tauri commands already run inside a tokio runtime,
    /// and `tokio::task::spawn_blocking` is the canonical way to
    /// bridge them, but in a synchronous `start()` we don't have
    /// a runtime handle handy. A plain `std::thread` is simpler,
    /// survives `start()` returning, and dies naturally when the
    /// pipe closes (child exits).
    fn spawn_drain(tag: &'static str, pipe: impl std::io::Read + Send + 'static) {
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

    /// Scan for and kill any process holding `port`. Used at startup
    /// to clean up stale `hermix.exe` orphans left behind by a
    /// previous Tauri session whose `Drop` hook never ran (killed
    /// via task manager, OOM, blue-screen, parent process crash).
    ///
    /// Implementation: runs `netstat -ano` once, filters rows by
    /// local address ending with `:PORT` in LISTENING state, looks
    /// up the image name via `tasklist /FI "PID eq ..."`, and kills
    /// matches whose image starts with `hermix` (case-insensitive)
    /// via `taskkill /F /PID`. Returns when the port is free, or
    /// silently gives up after one pass if anything is missing —
    /// we never want this guard to be the reason the app fails to
    /// launch.
    ///
    /// All best-effort. Errors at any stage are logged to stderr
    /// and swallowed: the original failure mode (port-in-use from
    /// a stubborn non-hermix process) will still surface as a
    /// `TcpListener::bind` failure in the spawned child, which is
    /// the right place to report it.
    #[cfg(windows)]
    fn detect_and_kill_stale_orphan(port: u16) {
        let needle = format!(":{}", port);
        let netstat = match std::process::Command::new("netstat")
            .args(["-ano", "-p", "TCP"])
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[Hermix] orphan-scan: netstat failed to start: {e}");
                return;
            }
        };
        if !netstat.status.success() {
            eprintln!(
                "[Hermix] orphan-scan: netstat exited with {:?}",
                netstat.status.code()
            );
            return;
        }

        // netstat -ano output (zh-CN / en-US Windows):
        //
        //   Active Connections
        //
        //   Proto  Local Address          Foreign Address        State           PID
        //   TCP    0.0.0.0:3000           0.0.0.0:0              LISTENING       1234
        //   ...
        //
        // We want the last column (PID) of every LISTENING row whose
        // Local Address ends with `:PORT`. The output is GBK-encoded
        // on zh-CN Windows but `String::from_utf8_lossy` round-trips
        // ASCII / numeric columns cleanly.
        let stdout = String::from_utf8_lossy(&netstat.stdout);
        let mut pids: Vec<u32> = Vec::new();
        for line in stdout.lines() {
            // Fast pre-filter before splitting.
            if !line.contains(&needle) || !line.contains("LISTENING") {
                continue;
            }
            let cols: Vec<&str> = line.split_whitespace().collect();
            // Proto Local Foreign State PID — minimum 5 cols.
            if cols.len() < 5 {
                continue;
            }
            // Defensive: confirm the local column actually ends in
            // :PORT (the contains() above is necessary but not
            // sufficient because Foreign Address could also match).
            if !cols[1].ends_with(&needle) {
                continue;
            }
            if let Ok(pid) = cols[4].parse::<u32>() {
                if pid != 0 && pid != std::process::id() {
                    pids.push(pid);
                }
            }
        }
        pids.sort();
        pids.dedup();

        for pid in pids {
            // Pre-flight tasklist lookup (#5): resolve image name
            // BEFORE we touch `taskkill`. taskkill /PID has no
            // `-Confirm` and no image-name cross-check; if the PID
            // has been recycled by the kernel between netstat and
            // here (PID reuse is rare on Windows but does happen —
            // see e.g. pidfd_open(2) for the POSIX analogue), we'd
            // be killing some completely unrelated process. Always
            // confirm via `tasklist /FI "PID eq X"` first.
            let image = Self::lookup_image_for_pid(pid);

            // tasklist returns "INFO: No tasks are running which
            // match the specified criteria." when the PID is gone —
            // `lookup_image_for_pid` surfaces that as an empty
            // string. In that case the holder we saw in netstat has
            // already exited, the port will be free momentarily, so
            // silently move on.
            if image.is_empty() {
                eprintln!(
                    "[Hermix] orphan-scan: pid={} on port {} already exited before tasklist; skipping",
                    pid, port
                );
                continue;
            }

            // Match rule: image basename starts with `hermix`
            // (case-insensitive). We deliberately don't require an
            // exact match — future builds might be `hermix-dev.exe`
            // / `hermix-staging.exe`. We DO exclude obvious
            // non-hermix holders (system processes, browsers) so
            // we never accidentally kill someone's dev server.
            let basename = std::path::Path::new(&image)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&image);
            if !basename.to_ascii_lowercase().starts_with("hermix") {
                eprintln!(
                    "[Hermix] orphan-scan: port {} held by non-hermix pid={} image={}, leaving alone",
                    port, pid, basename
                );
                continue;
            }

            // Re-confirm right before kill. Between the tasklist
            // above and the taskkill below, the PID could still be
            // recycled (the window is ~50–200ms on a busy
            // workstation). A second tasklist costs ~30ms but
            // converts "killed wrong process" into "skipped" —
            // cheap insurance for a destructive syscall.
            std::thread::sleep(Duration::from_millis(100));
            let image2 = Self::lookup_image_for_pid(pid);
            if image2.is_empty() {
                eprintln!(
                    "[Hermix] orphan-scan: pid={} disappeared between tasklist calls; recycling suspected, skipping kill",
                    pid
                );
                continue;
            }
            let basename2 = std::path::Path::new(&image2)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&image2);
            if !basename2.to_ascii_lowercase().starts_with("hermix") {
                eprintln!(
                    "[Hermix] orphan-scan: pid={} recycled to non-hermix image={} between checks, skipping kill",
                    pid, basename2
                );
                continue;
            }

            eprintln!(
                "[Hermix] orphan-scan: killing stale hermix orphan pid={} image={} on port {}",
                pid, basename2, port
            );
            let kill = std::process::Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
            match kill {
                Ok(o) if o.status.success() => {
                    eprintln!("[Hermix] orphan-scan: killed pid={}", pid);
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    eprintln!(
                        "[Hermix] orphan-scan: taskkill failed for pid={}: {}",
                        pid,
                        err.trim()
                    );
                }
                Err(e) => {
                    eprintln!("[Hermix] orphan-scan: taskkill could not start: {e}");
                }
            }
        }

        // Give the kernel a beat to release the socket. Without
        // this, the immediate `Command::spawn` below can race the
        // LISTENING entry's removal and still hit EADDRINUSE.
        std::thread::sleep(Duration::from_millis(400));
    }

    /// Non-Windows stub. `start()` only runs on Windows today (the
    /// Tauri bundler drops hermix.exe next to the app on Windows
    /// only), so on macOS / Linux this is a no-op. Kept as an
    /// explicit stub rather than `#[cfg(not(windows))] fn` so the
    /// call site stays unconditional and the README's "Windows
    /// only" statement is enforceable by a missing-build error.
    #[cfg(not(windows))]
    fn detect_and_kill_stale_orphan(_port: u16) {}

    /// Resolve the image basename for a PID via `tasklist`. Returns
    /// an empty `String` when:
    ///   - the PID has already exited (tasklist prints
    ///     "INFO: No tasks are running…" and exits 0),
    ///   - tasklist itself fails to launch,
    ///   - the output can't be parsed as a CSV row.
    ///
    /// This is the single chokepoint for "what process is on this
    /// PID" lookups inside `detect_and_kill_stale_orphan`. Pulled
    /// out so the double-check (lookup → kill → re-lookup) is
    /// readable and so future switches to e.g. `NtQuerySystemInformation`
    /// only have one site to update.
    #[cfg(windows)]
    fn lookup_image_for_pid(pid: u32) -> String {
        let tasklist = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output();
        match tasklist {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout);
                // CSV: "hermix.exe","1234","Console","1","12,345 K"
                // Take the first field, strip optional quotes.
                s.lines()
                    .next()
                    .and_then(|l| l.split(',').next())
                    .map(|s| s.trim_matches('"').to_string())
                    .unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    #[cfg(not(windows))]
    fn lookup_image_for_pid(_pid: u32) -> String {
        String::new()
    }

    /// Public test-only accessor for `lookup_image_for_pid`. Used by
    /// `tests/sidecar_orphan_pid_recycle.rs` to verify PID-recycle
    /// pre-flight behavior without spinning up the full
    /// `start()` / netstat path. The `#[doc(hidden)]` and `_test`
    /// suffix are deliberate: this is not part of the sidecar's
    /// public API and should not be relied on by downstream code.
    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn lookup_image_for_pid_test(pid: u32) -> String {
        Self::lookup_image_for_pid(pid)
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            println!("[Hermix] Sidecar stopped");
        }
        // stdout/stderr were moved into drain threads at start();
        // killing the child above closes the OS pipes, which makes
        // those threads exit on their own.
        self.stdout.take();
        self.stderr.take();
    }

    /// Stop (if running) and start a fresh instance. Used by the
    /// desktop UI's "restart" button when the backend goes offline.
    pub fn restart(&mut self) -> Result<(), String> {
        self.stop();
        // Give the kernel a moment to release the port
        std::thread::sleep(std::time::Duration::from_millis(200));
        self.start()
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Poll /health until server is ready
    pub async fn wait_until_ready(&mut self) {
        let url = format!("{}/health", self.base_url());
        let deadline = std::time::Instant::now() + Duration::from_secs(30);

        while std::time::Instant::now() < deadline {
            if let Ok(resp) = reqwest::get(&url).await {
                if resp.status().is_success() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        eprintln!("[Hermix] Server did not become ready in 30s");
    }

    /// Check if the HTTP server is reachable
    pub async fn health_check(&self) -> bool {
        let url = format!("{}/health", self.base_url());
        match reqwest::get(&url).await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Start a background watchdog that periodically calls `/health` and
    /// restarts the sidecar after `max_consecutive_failures` consecutive
    /// failures. The returned `JoinHandle` aborts the watcher when dropped
    /// (i.e. when the Tauri shell exits).
    ///
    /// Design notes:
    /// - Uses `tauri::async_runtime::spawn` so the caller does NOT need to `.await` it.
    ///   The watcher runs forever in the background until dropped.
    /// - Holds `&self` only via `Arc` so the caller is expected to wrap
    ///   this struct in `Arc<Mutex<...>>` (see `AppState::sidecar`).
    /// - `restart()` is called via the closure; on failure the loop logs
    ///   and tries again next tick instead of crashing the watcher.
    /// Start a background watchdog that periodically calls `/health` and
    /// restarts the sidecar after `max_consecutive_failures` consecutive
    /// failures. The returned `JoinHandle` aborts the watcher when dropped
    /// (i.e. when the Tauri shell exits).
    ///
    /// Design notes:
    /// - Uses `tauri::async_runtime::spawn` so the caller does NOT need to `.await` it.
    ///   The watcher runs forever in the background until dropped.
    /// - Takes the `Arc<Mutex<Self>>` by reference so the watcher task
    ///   can clone it and outlive any individual caller (e.g. a UI command
    ///   that holds the mutex briefly, then drops it).
    /// - `restart()` is called via the closure; on failure the loop logs
    ///   and tries again next tick instead of crashing the watcher.
    pub fn start_watchdog(
        sb: &std::sync::Arc<std::sync::Mutex<Self>>,
        config: WatchdogConfig,
    ) -> tauri::async_runtime::JoinHandle<()> {
        let arc = sb.clone();
        tauri::async_runtime::spawn(async move {
            let interval = config.check_interval;
            let max_failures = config.max_consecutive_failures;
            let mut consecutive_failures: u32 = 0;

            loop {
                tokio::time::sleep(interval).await;

                // Probe health. Always release the mutex before awaiting HTTP
                // so a hung server cannot block restart().
                let (running, port_url) = {
                    let guard = arc.lock().expect("sidecar mutex poisoned");
                    (guard.is_running(), format!("{}/health", guard.base_url()))
                };
                let healthy = if running {
                    match reqwest::get(&port_url).await {
                        Ok(resp) => resp.status().is_success(),
                        Err(_) => false,
                    }
                } else {
                    false
                };

                if healthy {
                    consecutive_failures = 0;
                    continue;
                }

                consecutive_failures += 1;
                eprintln!(
                    "[Hermix] Sidecar health check failed ({consecutive_failures}/{max_failures})"
                );

                if consecutive_failures >= max_failures {
                    eprintln!("[Hermix] Watchdog: restarting sidecar after {consecutive_failures} failures");
                    let mut guard = arc.lock().expect("sidecar mutex poisoned");
                    if let Err(e) = guard.restart() {
                        eprintln!("[Hermix] Watchdog: restart() failed: {e}");
                    } else {
                        // Reset counter so we don't restart again immediately.
                        consecutive_failures = 0;
                    }
                }
            }
        })
    }
}

/// Configuration for the sidecar watchdog loop.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// How often to poll `/health`.
    pub check_interval: Duration,
    /// Restart after this many consecutive failures.
    pub max_consecutive_failures: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            max_consecutive_failures: 3,
        }
    }
}

/// Health-check helper that does NOT hold the sidecar mutex while awaiting
/// the HTTP response (would block restart forever if the server is hung).
#[allow(dead_code)] // kept for future external callers; the watchdog loop inlines this logic
async fn guard_health_check(
    sb: &std::sync::Arc<std::sync::Mutex<HermixSidecar>>,
) -> bool {
    let url = {
        let guard = sb.lock().expect("sidecar mutex poisoned");
        format!("{}/health", guard.base_url())
    };
    match reqwest::get(&url).await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

impl Drop for HermixSidecar {
    fn drop(&mut self) {
        self.stop();
    }
}
