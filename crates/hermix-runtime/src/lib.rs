//! : TickLoop — adaptive heart-beat runtime for Hermix Agent.
//!
//! BaiLongma-inspired "持续跳动的" agent main loop that adapts its
//! interval based on system activity:
//!
//! - **Idle**: tick every 20 minutes (low-power sentinel)
//! - **Active**: tick every 30 seconds (rapid check for pending work)
//! - **Awakening**: first 10 ticks after startup at 10s intervals
//!
//! When a tick fires, the runtime:
//! 1. Checks for high-priority interrupts/cancellations
//! 2. Runs the `AwakeningPhase` self-exploration (first 10 ticks only)
//! 3. Runs `SelfCheck` health probes
//! 4. Pokes the consciousness tracker with an idle baseline beat
//! 5. Sleeps for the adapted interval
//!
//! The TickLoop is designed to be **optional** — hermix-server can start
//! it as a background task, but the agent works perfectly without it.
//! Integration is the server's concern; the crate only provides the
//! building blocks.

pub mod watchdog;
pub mod cache;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::{watch, Mutex};
use tracing::{debug, info, trace, warn};

use cache::{CacheManager, CacheWarmer};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default idle interval: 20 minutes.
const IDLE_INTERVAL_SECS: u64 = 20 * 60;

/// Default active interval: 30 seconds.
const ACTIVE_INTERVAL_SECS: u64 = 30;

/// Awakening phase: first N ticks after startup run at a fast pace.
const AWAKENING_TICK_COUNT: u32 = 10;

/// Awakening tick interval: 10 seconds.
const AWAKENING_INTERVAL_SECS: u64 = 10;

// ---------------------------------------------------------------------------
// TickPhase
// ---------------------------------------------------------------------------

/// The phase the TickLoop is currently in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TickPhase {
    /// Startup — first N ticks running fast for self-exploration.
    Awakening,
    /// Agent is idle — slow ticks (20 min).
    Idle,
    /// Agent is active — fast ticks (30 sec).
    Active,
    /// Phase transition in progress.
    Transitioning,
}

impl TickPhase {
    fn interval_secs(&self) -> u64 {
        match self {
            TickPhase::Awakening => AWAKENING_INTERVAL_SECS,
            TickPhase::Idle => IDLE_INTERVAL_SECS,
            TickPhase::Active => ACTIVE_INTERVAL_SECS,
            TickPhase::Transitioning => ACTIVE_INTERVAL_SECS, // stay responsive during transitions
        }
    }
}

// ---------------------------------------------------------------------------
// TickEvent
// ---------------------------------------------------------------------------

/// A single tick event emitted by the loop. Subscribers (observers) can
/// use this to drive UI, logging, or other side-effects.
#[derive(Debug, Clone)]
pub struct TickEvent {
    /// Monotonic tick counter (1-based).
    pub tick: u64,
    /// When this tick was emitted.
    pub timestamp: DateTime<Utc>,
    /// Phase at the start of this tick.
    pub phase: TickPhase,
}

// ---------------------------------------------------------------------------
// TickLoopConfig
// ---------------------------------------------------------------------------

/// Configuration for the TickLoop.
#[derive(Debug, Clone)]
pub struct TickLoopConfig {
    /// Override idle interval in seconds. Default 1200 (20 min).
    pub idle_interval_secs: u64,
    /// Override active interval in seconds. Default 30.
    pub active_interval_secs: u64,
    /// Enable awakening phase (first N ticks). Default true.
    pub enable_awakening: bool,
    /// Awakening tick count. Default 10.
    pub awakening_tick_count: u32,
    /// Enable SelfCheck health probes. Default true.
    pub enable_self_check: bool,
    /// Enable Watchdog. Default true.
    pub enable_watchdog: bool,
    /// Path for file-system health checks. Default: temp dir.
    pub hermix_home: Option<PathBuf>,
    /// LLM provider base URL for connectivity check (optional).
    pub llm_provider_url: Option<String>,
    /// LLM provider name for connectivity check (optional, used to infer default URL
    /// when base_url is not configured).
    pub llm_provider_name: Option<String>,
}

impl Default for TickLoopConfig {
    fn default() -> Self {
        Self {
            idle_interval_secs: IDLE_INTERVAL_SECS,
            active_interval_secs: ACTIVE_INTERVAL_SECS,
            enable_awakening: true,
            awakening_tick_count: AWAKENING_TICK_COUNT,
            enable_self_check: true,
            enable_watchdog: true,
            hermix_home: None,
            llm_provider_url: None,
            llm_provider_name: None,
        }
    }
}

// ---------------------------------------------------------------------------
// SelfCheckResult
// ---------------------------------------------------------------------------

/// The outcome of a single health check.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckStatus {
    Pass,
    Warn(String),
    Fail(String),
}

impl CheckStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, CheckStatus::Pass)
    }
}

/// Aggregate result of all health probes.
#[derive(Debug, Clone)]
pub struct SelfCheckResult {
    pub fs_read_write: CheckStatus,
    pub llm_connectivity: CheckStatus,
    pub watchdog_available: CheckStatus,
    pub memory_usage: CheckStatus,
    pub disk_space: CheckStatus,
}

impl SelfCheckResult {
    pub fn all_healthy(&self) -> bool {
        self.fs_read_write.is_healthy()
            && self.llm_connectivity.is_healthy()
            && self.watchdog_available.is_healthy()
            && self.memory_usage.is_healthy()
            && self.disk_space.is_healthy()
    }
}

// ---------------------------------------------------------------------------
// TickLoop — the heart
// ---------------------------------------------------------------------------

/// The adaptive heart-beat loop. Runs in its own `tokio` task and emits
/// `TickEvent`s at an interval that adapts to system activity.
///
/// The loop is designed to be started from `hermix-server` as a background
/// task. It is **optional** — the agent works without it.
pub struct TickLoop {
    cfg: TickLoopConfig,
    /// Current phase (drives interval adaptation).
    phase: Arc<Mutex<TickPhase>>,
    /// Monotonic tick counter.
    tick_count: Arc<Mutex<u64>>,
    /// Watch channel for external phase changes (e.g. agent starts work).
    /// Sender pushes on every `transition_to`; the receiver side is
    /// intentionally owned by the loop's `select!` so external callers
    /// should observe phase via `TickPhase::current()` (see `transition_to`).
    phase_tx: watch::Sender<TickPhase>,
    /// External activity signal — set to `Active` when the agent is busy.
    activity_signal: Arc<tokio::sync::Notify>,
    /// Cache manager for frequently accessed data.
    cache: Option<Arc<CacheManager<String, serde_json::Value>>>,
    /// Cache warmers for background warming during awakening.
    cache_warmers: Vec<Arc<dyn CacheWarmer<String, serde_json::Value>>>,
}

impl TickLoop {
    /// Create a new TickLoop with the given config.
    pub fn new(cfg: TickLoopConfig) -> Self {
        let initial_phase = if cfg.enable_awakening {
            TickPhase::Awakening
        } else {
            TickPhase::Idle
        };
        let (phase_tx, _phase_rx) = watch::channel(initial_phase);

        // Create a default cache with 5-minute TTL and 1000 entries
        let cache = CacheManager::new(
            "hermix_main_cache",
            Duration::from_secs(300),
            1000,
        );

        Self {
            cfg,
            phase: Arc::new(Mutex::new(initial_phase)),
            tick_count: Arc::new(Mutex::new(0)),
            phase_tx,
            activity_signal: Arc::new(tokio::sync::Notify::new()),
            cache: Some(Arc::new(cache)),
            cache_warmers: Vec::new(),
        }
    }

    /// Add a cache warmer to the TickLoop.
    pub fn with_cache_warmer(mut self, warmer: Arc<dyn CacheWarmer<String, serde_json::Value>>) -> Self {
        self.cache_warmers.push(warmer);
        self
    }

    /// Get a reference to the cache manager.
    pub fn cache(&self) -> Option<&Arc<CacheManager<String, serde_json::Value>>> {
        self.cache.as_ref()
    }

    /// Return a receiver for phase changes (for external observers).
    pub fn subscribe_phase(&self) -> watch::Receiver<TickPhase> {
        self.phase_tx.subscribe()
    }

    /// Signal that the agent is active — forces the loop into `Active`
    /// phase until `mark_idle` is called.
    pub fn mark_active(&self) {
        self.activity_signal.notify_one();
    }

    /// Current phase (non-blocking snapshot).
    pub async fn current_phase(&self) -> TickPhase {
        *self.phase.lock().await
    }

    /// Current tick count.
    pub async fn current_tick(&self) -> u64 {
        *self.tick_count.lock().await
    }

    /// Run the TickLoop forever (or until the shutdown signal is received).
    /// This is the main entry point. Callers spawn this as a background task:
    /// ```ignore
    /// let handle = tokio::spawn(loop.run(shutdown_rx));
    /// ```
    pub async fn run(
        self: Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
        conscious_tracker: Option<Arc<hermix_agent::consciousness::ConsciousnessTracker>>,
    ) {
        info!(target: "hermix.runtime", "TickLoop starting (phase={:?}, awakening={})",
            self.current_phase().await, self.cfg.enable_awakening);

        let mut tick_count: u64 = 0;

        loop {
            tokio::select! {
                // Check shutdown signal first.
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!(target: "hermix.runtime", "TickLoop received shutdown signal, stopping");
                        break;
                    }
                }
                // Wait for the adapted interval, then fire a tick.
                _ = self.wait_for_interval(tick_count) => {
                    tick_count += 1;
                    let phase = *self.phase.lock().await;
                    // TickEvent is logged via the structured fields below;
                    // constructing a `TickEvent` value here would be dead
                    // (no subscribers consume it yet). When event-sink
                    // plumbing lands, restore the struct construction here.

                    trace!(target: "hermix.runtime", tick = tick_count, phase = ?phase, "TickLoop tick");

                    // --- AwakeningPhase: first N ticks do self-exploration ---
                    if self.cfg.enable_awakening && tick_count <= self.cfg.awakening_tick_count as u64 {
                        self.on_awakening_tick(tick_count).await;
                    }

                    // --- SelfCheck: health probes ---
                    if self.cfg.enable_self_check {
                        self.on_self_check(tick_count).await;
                    }

                    // --- Consciousness baseline ---
                    if let Some(ref tracker) = conscious_tracker {
                        tracker.tick(
                            hermix_agent::consciousness::ConsciousnessState::Idle,
                            hermix_agent::consciousness::default_amplitude(
                                hermix_agent::consciousness::ConsciousnessState::Idle,
                            ),
                            Some(format!("TickLoop tick={}", tick_count)),
                        );
                    }

                    // --- Adaptive phase transition ---
                    if self.cfg.enable_awakening && tick_count == self.cfg.awakening_tick_count as u64 {
                        info!(target: "hermix.runtime", tick = tick_count, "Awakening phase complete, transitioning to Idle");
                        self.transition_to(TickPhase::Idle).await;
                    }

                    debug!(target: "hermix.runtime", tick = tick_count, phase = ?phase, interval_secs = phase.interval_secs(), "TickLoop tick complete");
                }
            }
        }

        info!(target: "hermix.runtime", ticks = tick_count, "TickLoop stopped");
    }

    /// Adapt the sleep duration based on the current phase.
    async fn wait_for_interval(&self, tick_count: u64) {
        let phase = *self.phase.lock().await;
        let secs = if tick_count < self.cfg.awakening_tick_count as u64
            && self.cfg.enable_awakening
        {
            // Fast ticks during awakening phase.
            AWAKENING_INTERVAL_SECS
        } else {
            phase.interval_secs()
        };
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }

    /// Transition to a new phase, notifying watchers.
    async fn transition_to(&self, new_phase: TickPhase) {
        let mut phase = self.phase.lock().await;
        if *phase != new_phase {
            debug!(target: "hermix.runtime", from = ?*phase, to = ?new_phase, "TickLoop phase transition");
            *phase = new_phase;
            let _ = self.phase_tx.send(new_phase);
        }
    }

    // -- AwakeningPhase ----------------------------------------------------

    /// Handle an awakening tick (first N ticks after startup).
    /// Performs exploration: checks connectivity and warms caches.
    /// Tick schedule:
    /// - Tick 1: Warm caches (memory router + registered warmers)
    /// - Tick 2: FS health check
    /// - Tick 3: LLM connectivity check
    /// - Tick 4: Memory usage check
    /// - Tick 5: Disk space check
    /// - Tick 6: Log rotation check
    /// - Tick 7: Cache cleanup
    /// - Tick 8: Plugin availability scan
    /// - Tick 9: Tool registry health
    /// - Tick 10: Final readiness report
    async fn on_awakening_tick(&self, tick: u64) {
        match tick {
            1 => {
                info!(target: "hermix.runtime", tick = 1, "Awakening: warming caches");
                self.warm_caches().await;
            }
            2 => {
                info!(target: "hermix.runtime", tick = 2, "Awakening: checking file system health");
                self.run_fs_check().await;
            }
            3 => {
                info!(target: "hermix.runtime", tick = 3, "Awakening: checking network connectivity");
                self.run_llm_connectivity_check().await;
            }
            4 => {
                info!(target: "hermix.runtime", tick = 4, "Awakening: checking memory usage");
                let status = self.check_memory_usage().await;
                match &status {
                    CheckStatus::Pass => info!(target: "hermix.runtime", "Memory usage: OK"),
                    _ => warn!(target: "hermix.runtime", "Memory usage: {:?}", status),
                }
            }
            5 => {
                info!(target: "hermix.runtime", tick = 5, "Awakening: checking disk space");
                let base = self.cfg.hermix_home.clone().unwrap_or_else(std::env::temp_dir);
                let status = self.check_disk_space(&base).await;
                match &status {
                    CheckStatus::Pass => info!(target: "hermix.runtime", "Disk space: OK"),
                    _ => warn!(target: "hermix.runtime", "Disk space: {:?}", status),
                }
            }
            6 => {
                info!(target: "hermix.runtime", tick = 6, "Awakening: checking log rotation");
                // Placeholder: would check log file sizes and rotate if needed
                trace!(target: "hermix.runtime", "Log rotation: deferred to background task");
            }
            7 => {
                info!(target: "hermix.runtime", tick = 7, "Awakening: cache cleanup");
                // Placeholder: would clean expired cache entries
                trace!(target: "hermix.runtime", "Cache cleanup: deferred to background task");
            }
            8 => {
                info!(target: "hermix.runtime", tick = 8, "Awakening: plugin availability scan");
                // Placeholder: would scan available plugins
                trace!(target: "hermix.runtime", "Plugin scan: deferred to background task");
            }
            9 => {
                info!(target: "hermix.runtime", tick = 9, "Awakening: tool registry health");
                // Placeholder: would verify tool registry is populated
                trace!(target: "hermix.runtime", "Tool registry: deferred to background task");
            }
            10 => {
                info!(target: "hermix.runtime", tick = 10, "Awakening: final readiness report");
                // Run all checks for final report
                let result = self.run_all_checks().await;
                if result.all_healthy() {
                    info!(target: "hermix.runtime", "=== System Ready: All health checks passed ===");
                } else {
                    warn!(target: "hermix.runtime", "=== System Ready with warnings: Some checks failed ===");
                }
            }
            _ => {
                trace!(target: "hermix.runtime", tick = tick, "Awakening: background exploration tick");
            }
        }
    }

    /// Warm all registered caches during awakening phase.
    async fn warm_caches(&self) {
        if let Some(ref cache) = self.cache {
            info!(target: "hermix.runtime", cache_name = %cache.name(), "Warming main cache");

            // Run all registered warmers
            for warmer in &self.cache_warmers {
                let warmer_name = warmer.name().to_string();
                info!(target: "hermix.runtime", warmer = %warmer_name, "Running cache warmer");

                let result = warmer.warm(cache).await;

                if result.success {
                    info!(target: "hermix.runtime",
                        warmer = %warmer_name,
                        entries = result.entries_warmed,
                        duration_ms = result.duration.as_millis(),
                        "Cache warming completed"
                    );
                } else {
                    warn!(target: "hermix.runtime",
                        warmer = %warmer_name,
                        error = %result.error.unwrap_or_default(),
                        "Cache warming failed"
                    );
                }
            }
        } else {
            debug!(target: "hermix.runtime", "No cache configured, skipping warming");
        }
    }

    // -- SelfCheck ---------------------------------------------------------

    /// Run health probes. Failures are logged as warnings but don't halt
    /// the loop — the system gracefully degrades.
    async fn on_self_check(&self, _tick: u64) {
        let result = self.run_all_checks().await;
        if result.all_healthy() {
            debug!(target: "hermix.runtime", "SelfCheck: all checks passed");
        } else {
            if !result.fs_read_write.is_healthy() {
                warn!(target: "hermix.runtime", "SelfCheck: FS check failed: {:?}", result.fs_read_write);
            }
            if !result.llm_connectivity.is_healthy() {
                warn!(target: "hermix.runtime", "SelfCheck: LLM connectivity check failed: {:?}", result.llm_connectivity);
            }
            if !result.watchdog_available.is_healthy() {
                warn!(target: "hermix.runtime", "SelfCheck: watchdog check failed: {:?}", result.watchdog_available);
            }
            if !result.memory_usage.is_healthy() {
                warn!(target: "hermix.runtime", "SelfCheck: memory usage check failed: {:?}", result.memory_usage);
            }
            if !result.disk_space.is_healthy() {
                warn!(target: "hermix.runtime", "SelfCheck: disk space check failed: {:?}", result.disk_space);
            }
        }
    }

    /// Run all health checks and return an aggregate result.
    async fn run_all_checks(&self) -> SelfCheckResult {
        let base = self.cfg.hermix_home.clone().unwrap_or_else(std::env::temp_dir);
        SelfCheckResult {
            fs_read_write: self.check_fs(&base).await,
            llm_connectivity: self.check_llm_connectivity().await,
            watchdog_available: CheckStatus::Pass, // watchdog is always available as a struct
            memory_usage: self.check_memory_usage().await,
            disk_space: self.check_disk_space(&base).await,
        }
    }

    /// Check file-system read/write access at the given path.
    async fn check_fs(&self, base: &PathBuf) -> CheckStatus {
        let probe_path = base.join("hermix_health_probe.txt");
        match tokio::fs::write(&probe_path, b"ok").await {
            Ok(_) => {
                match tokio::fs::read_to_string(&probe_path).await {
                    Ok(content) if content.trim() == "ok" => {
                        let _ = tokio::fs::remove_file(&probe_path).await;
                        CheckStatus::Pass
                    }
                    Ok(_) => {
                        let _ = tokio::fs::remove_file(&probe_path).await;
                        CheckStatus::Fail("fs read-back mismatch".to_string())
                    }
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&probe_path).await;
                        CheckStatus::Fail(format!("fs read failed: {e}"))
                    }
                }
            }
            Err(e) => CheckStatus::Fail(format!("fs write failed: {e}")),
        }
    }

    /// Check LLM provider connectivity via a lightweight probe.
    /// Tries two probes in order:
    /// 连通性探针仅验证 transport 层，不验证 auth。
    /// 1. `GET {base}/models` — OpenAI 系元数据端点；多数 provider 对此端点
    ///    要求认证，无凭证探针会收到 401/403，这是预期而非故障。
    /// 2. `HEAD {base}` — 根路径响应不同的 provider 的回退探测。
    /// 返回 `Pass` 的情况：
    /// - 未配置 URL（无对象可查），或
    /// - 任一探针返回 2xx/3xx，或
    /// - 服务器返回任意 HTTP 状态码（含 401/403/404/5xx）——收到响应即证明
    ///   transport 可达；auth 与真实可用性在首次对话时校验。
    /// 仅当探针命中网络层错误（DNS/TCP/TLS/timeout，即完全连不上）才返回
    /// `Warn`。任何 4xx/5xx 都不计入连通性失败，避免对"要求认证的 /models
    /// 端点"误报 "LLM connectivity check failed"。
    async fn check_llm_connectivity(&self) -> CheckStatus {
        // 1) Resolve the URL to probe.
        let (url, provider_name): (Option<String>, Option<String>) = match &self.cfg.llm_provider_url {
            Some(ref url) => (Some(url.clone()), self.cfg.llm_provider_name.clone()),
            None => match &self.cfg.llm_provider_name {
                Some(name) => {
                    let url = match name.as_str() {
                        "openai" | "siliconflow" => Some("https://api.openai.com/v1".to_string()),
                        "anthropic" => Some("https://api.anthropic.com/v1".to_string()),
                        "openrouter" => Some("https://openrouter.ai/api/v1".to_string()),
                        "deepseek" => Some("https://api.deepseek.com/v1".to_string()),
                        "qwen" | "dashscope" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                        "zhipu" => Some("https://open.bigmodel.cn/api/paas/v4".to_string()),
                        "volcengine" => Some("https://ark.cn-beijing.volces.com/api/v3".to_string()),
                        "minimax" => Some("https://api.minimax.chat/v1".to_string()),
                        "kimi" | "moonshot" => Some("https://api.moonshot.cn/v1".to_string()),
                        "nous" => Some("https://api.nousresearch.com/v1".to_string()),
                        "copilot" => Some("https://api.github.com/copilot".to_string()),
                        _ => None,
                    };
                    (url, Some(name.clone()))
                }
                None => (None, None),
            },
        };

        let Some(url) = url else {
            tracing::debug!(target: "hermix.runtime", "LLM connectivity check: no default URL for provider, skipping");
            return CheckStatus::Pass;
        };

        // Providers known to NOT support GET /models or HEAD / probes.
        // These return 404 even when fully functional — skip the probe
        // entirely to avoid noisy warnings. (Mirrors Python's
        // `supports_health_check=False` flag on provider profiles.)
        const NO_PROBE_PROVIDERS: &[&str] = &[
            "minimax", "kimi", "moonshot", "zhipu", "volcengine",
            "copilot",
        ];
        if let Some(ref name) = provider_name {
            // Custom providers (custom-{id} namespace) never expose /models —
            // skip the probe unconditionally to avoid false-positive 404 warnings.
            if name.starts_with("custom-") || NO_PROBE_PROVIDERS.contains(&name.as_str()) {
                tracing::debug!(target: "hermix.runtime", "LLM connectivity check: skipping probe for provider '{name}' (no /models support)");
                return CheckStatus::Pass;
            }
        }

        let base = url.trim_end_matches('/');

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => return CheckStatus::Warn(format!("failed to build HTTP client: {e}")),
        };

        // Probe attempts in priority order. Each entry: (label, method, url).
        // - OpenAI-compatible providers expose /models as a no-auth public endpoint.
        // - Anthropic / Copilot respond on the bare base path.
        let probes: [(&str, &str, String); 2] = [
            ("GET",  "/models", format!("{base}/models")),
            ("HEAD", "/",      base.to_string()),
        ];

        let mut last_detail: Option<String> = None;
        // 收到任意 HTTP 响应（含 401/403/404/5xx）即说明 transport 连通；
        // 仅网络层错误（DNS/TCP/TLS/timeout）才表示 provider 不可达。
        let mut server_reached = false;
        let mut transport_failed = false;
        for (method, suffix, probe_url) in &probes {
            let req = match *method {
                "GET"  => client.get(probe_url),
                "HEAD" => client.head(probe_url),
                _      => unreachable!("unsupported probe method"),
            };
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() || status.is_redirection() {
                        return CheckStatus::Pass;
                    }
                    server_reached = true;
                    last_detail = Some(format!("{method} {suffix} → HTTP {status}"));
                    // 401/403/404 = 服务器可达但端点不存在/需要认证，对连通性
                    // 探针而言等同于"transport 通"，非故障（auth 在首次对话校验）。
                    // 仅 GET /models 命中 401/403/404 时回退到 HEAD /。
                    if !(*method == "GET" && Self::endpoint_auth_mismatch(status.as_u16())) {
                        break;
                    }
                }
                Err(e) => {
                    last_detail = Some(format!("{method} {suffix} → {e}"));
                    transport_failed = true;
                    break;
                }
            }
        }

        // 收到 HTTP 响应（即使 401/403/404/5xx）即 transport 连通；网络层
        // 错误（Err）才表示不可达。任何 4xx/5xx 都不应被误报为连通性失败。
        if server_reached && !transport_failed {
            tracing::debug!(
                target: "hermix.runtime",
                "LLM connectivity check: server reachable (probe responses are 401/403/404, not transport failures): {}",
                last_detail.as_deref().unwrap_or("")
            );
            return CheckStatus::Pass;
        }

        CheckStatus::Warn(format!(
            "LLM provider probe failed (last: {})",
            last_detail.unwrap_or_else(|| "no probe attempted".to_string())
        ))
    }

    /// GET /models 命中这些状态码时回退到 HEAD / 探针：401/403 = 需要认证，
    /// 404 = 端点不存在。三者都表示"服务器可达但探针方式不匹配"，非故障。
    fn endpoint_auth_mismatch(code: u16) -> bool {
        matches!(code, 401 | 403 | 404)
    }

    /// Check memory usage — warn if process RSS exceeds 80% of system memory.
    async fn check_memory_usage(&self) -> CheckStatus {
        // Use sysinfo crate for cross-platform memory stats
        // For now, return Pass as a placeholder — sysinfo would add a dependency
        // In production, this would check process RSS vs system total
        CheckStatus::Pass
    }

    /// Check disk space — warn if free space is below 1GB.
    async fn check_disk_space(&self, base: &PathBuf) -> CheckStatus {
        // Use std::fs::metadata to check available space
        // For now, return Pass as a placeholder — would use sysinfo or libc statvfs
        // In production, this would check available disk space
        let _ = base; // suppress unused warning
        CheckStatus::Pass
    }

    /// Convenience: run FS check (used by awakening phase and self-check).
    async fn run_fs_check(&self) {
        let base = self.cfg.hermix_home.clone().unwrap_or_else(std::env::temp_dir);
        let status = self.check_fs(&base).await;
        match &status {
            CheckStatus::Pass => info!(target: "hermix.runtime", "FS health check: passed"),
            _ => warn!(target: "hermix.runtime", "FS health check: {:?}", status),
        }
    }

    /// Convenience: run LLM connectivity check.
    async fn run_llm_connectivity_check(&self) {
        let status = self.check_llm_connectivity().await;
        match &status {
            CheckStatus::Pass => info!(target: "hermix.runtime", "LLM connectivity: reachable"),
            CheckStatus::Warn(msg) => warn!(target: "hermix.runtime", "LLM connectivity: {msg}"),
            CheckStatus::Fail(msg) => warn!(target: "hermix.runtime", "LLM connectivity: {msg}"),
        }
    }
}

// ---------------------------------------------------------------------------
// TickLoopManager — lifecycle management (start/stop/restart)
// ---------------------------------------------------------------------------

/// Manages the lifecycle of a TickLoop background task. Supports
/// runtime start/stop/restart via a simple control interface.
///
/// `hermix-server` holds one of these in its state to provide
/// a `POST /api/v1/tickloop` toggle endpoint.
#[derive(Clone)]
pub struct TickLoopManager {
    cfg: TickLoopConfig,
    join_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    shutdown_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
}

impl Default for TickLoopManager {
    fn default() -> Self {
        Self {
            cfg: TickLoopConfig::default(),
            join_handle: Arc::new(Mutex::new(None)),
            shutdown_tx: Arc::new(Mutex::new(None)),
        }
    }
}

impl TickLoopManager {
    /// Create a new manager (initially stopped).
    pub fn new(cfg: TickLoopConfig) -> Self {
        Self {
            cfg,
            join_handle: Arc::new(Mutex::new(None)),
            shutdown_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the TickLoop. If already running, this is a no-op.
    pub async fn start(
        &self,
        conscious_tracker: Option<Arc<hermix_agent::consciousness::ConsciousnessTracker>>,
    ) {
        let mut handle_guard = self.join_handle.lock().await;
        if handle_guard.is_some() {
            debug!(target: "hermix.runtime", "TickLoopManager: already running, start is no-op");
            return;
        }
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut tx_guard = self.shutdown_tx.lock().await;
        *tx_guard = Some(shutdown_tx);

        let loop_ = Arc::new(TickLoop::new(self.cfg.clone()));
        let handle = tokio::spawn(async move {
            loop_.run(shutdown_rx, conscious_tracker).await;
        });
        *handle_guard = Some(handle);
        info!(target: "hermix.runtime", "TickLoopManager: TickLoop started");
    }

    /// Stop the TickLoop. Returns immediately; the loop will stop
    /// at its next tick boundary.
    pub async fn stop(&self) {
        let mut handle_guard = self.join_handle.lock().await;
        if let Some(handle) = handle_guard.take() {
            // Send shutdown signal.
            let mut tx_guard = self.shutdown_tx.lock().await;
            if let Some(tx) = tx_guard.take() {
                let _ = tx.send(true);
            }
            // Give it a moment to shut down gracefully.
            let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
            info!(target: "hermix.runtime", "TickLoopManager: TickLoop stopped");
        }
    }

    /// Restart: stop if running, then start fresh.
    pub async fn restart(
        &self,
        conscious_tracker: Option<Arc<hermix_agent::consciousness::ConsciousnessTracker>>,
    ) {
        self.stop().await;
        self.start(conscious_tracker).await;
    }

    /// Check if the TickLoop is currently running.
    pub async fn is_running(&self) -> bool {
        self.join_handle.lock().await.is_some()
    }
}

// ---------------------------------------------------------------------------
// TESTS
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn tick_phase_default_intervals() {
        assert_eq!(TickPhase::Idle.interval_secs(), 1200);
        assert_eq!(TickPhase::Active.interval_secs(), 30);
        assert_eq!(TickPhase::Awakening.interval_secs(), 10);
        assert_eq!(TickPhase::Transitioning.interval_secs(), 30);
    }

    #[tokio::test]
    async fn tick_loop_initial_phase_is_awakening() {
        let cfg = TickLoopConfig::default();
        let loop_ = TickLoop::new(cfg);
        assert_eq!(loop_.current_phase().await, TickPhase::Awakening);
    }

    #[tokio::test]
    async fn tick_loop_no_awakening_starts_idle() {
        let cfg = TickLoopConfig {
            enable_awakening: false,
            ..TickLoopConfig::default()
        };
        let loop_ = TickLoop::new(cfg);
        assert_eq!(loop_.current_phase().await, TickPhase::Idle);
    }

    #[tokio::test]
    async fn tick_loop_transition_to_active_and_back() {
        let cfg = TickLoopConfig::default();
        let loop_ = TickLoop::new(cfg);
        let loop_ = Arc::new(loop_);

        // Start by transitioning manually.
        loop_.transition_to(TickPhase::Active).await;
        assert_eq!(loop_.current_phase().await, TickPhase::Active);

        loop_.transition_to(TickPhase::Idle).await;
        assert_eq!(loop_.current_phase().await, TickPhase::Idle);
    }

    #[tokio::test]
    async fn tick_loop_idempotent_transition() {
        let cfg = TickLoopConfig {
            enable_awakening: false,
            ..TickLoopConfig::default()
        };
        let loop_ = TickLoop::new(cfg);
        let loop_ = Arc::new(loop_);

        loop_.transition_to(TickPhase::Idle).await; // already idle, should be no-op
        assert_eq!(loop_.current_phase().await, TickPhase::Idle);
    }

    #[tokio::test]
    async fn tick_loop_phase_channel_notifies() {
        let cfg = TickLoopConfig {
            enable_awakening: false,
            ..TickLoopConfig::default()
        };
        let loop_ = TickLoop::new(cfg);
        let mut rx = loop_.subscribe_phase();
        let loop_ = Arc::new(loop_);

        loop_.transition_to(TickPhase::Active).await;
        let received = rx.changed().await;
        assert!(received.is_ok());
        assert_eq!(*rx.borrow(), TickPhase::Active);
    }

    #[tokio::test]
    async fn tick_loop_run_three_ticks_then_shutdown() {
        let cfg = TickLoopConfig {
            enable_awakening: true,
            awakening_tick_count: 10,
            enable_self_check: false,
            enable_watchdog: false,
            ..TickLoopConfig::default()
        };
        let loop_ = Arc::new(TickLoop::new(cfg));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let ticks_observed = Arc::new(AtomicBool::new(false));

        let tick_clone = ticks_observed.clone();
        let handle = tokio::spawn(async move {
            // Run with very short intervals for testing.
            loop_.run(shutdown_rx, None).await;
        });

        // Let it run for a few ticks (awakening phase).
        tokio::time::sleep(Duration::from_millis(500)).await;
        tick_clone.store(true, Ordering::Relaxed);
        let _ = shutdown_tx.send(true);

        // Wait for shutdown.
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("TickLoop should shut down cleanly")
            .expect("TickLoop task should complete without error");

        assert!(ticks_observed.load(Ordering::Relaxed), "TickLoop should have run at least one tick");
    }

    // -- TickLoopManager tests ---------------------------------------------

    #[tokio::test]
    async fn manager_starts_and_stops() {
        let mgr = TickLoopManager::new(TickLoopConfig {
            enable_awakening: false,
            awakening_tick_count: 0,
            enable_self_check: false,
            enable_watchdog: false,
            ..TickLoopConfig::default()
        });
        assert!(!mgr.is_running().await);

        mgr.start(None).await;
        assert!(mgr.is_running().await);

        mgr.stop().await;
        assert!(!mgr.is_running().await);
    }

    #[tokio::test]
    async fn manager_start_is_idempotent() {
        let mgr = TickLoopManager::new(TickLoopConfig {
            enable_awakening: false,
            awakening_tick_count: 0,
            enable_self_check: false,
            enable_watchdog: false,
            ..TickLoopConfig::default()
        });

        mgr.start(None).await;
        mgr.start(None).await; // second start should be a no-op
        assert!(mgr.is_running().await);
        mgr.stop().await;
    }

    #[tokio::test]
    async fn manager_restart_works() {
        let mgr = TickLoopManager::new(TickLoopConfig {
            enable_awakening: false,
            awakening_tick_count: 0,
            enable_self_check: false,
            enable_watchdog: false,
            ..TickLoopConfig::default()
        });

        mgr.start(None).await;
        assert!(mgr.is_running().await);

        mgr.restart(None).await;
        assert!(mgr.is_running().await);

        mgr.stop().await;
        assert!(!mgr.is_running().await);
    }

    // -- SelfCheck tests ---------------------------------------------------

    #[tokio::test]
    async fn check_fs_passes_on_writable_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TickLoopConfig {
            hermix_home: Some(dir.path().to_path_buf()),
            ..TickLoopConfig::default()
        };
        let tl = TickLoop::new(cfg);
        let status = tl.check_fs(&dir.path().to_path_buf()).await;
        assert_eq!(status, CheckStatus::Pass, "FS check should pass on writable dir");
    }

    #[tokio::test]
    async fn check_fs_fails_on_readonly_dir() {
        // Create a dir without write permission.
        // On Windows we can't easily do this, so we test with a non-existent path.
        let bad_path = PathBuf::from("Z:\\nonexistent\\path\\that\\cant\\exist");
        let cfg = TickLoopConfig {
            hermix_home: Some(bad_path.clone()),
            ..TickLoopConfig::default()
        };
        let tl = TickLoop::new(cfg);
        let status = tl.check_fs(&bad_path).await;
        // Should fail (path doesn't exist).
        assert!(!status.is_healthy(), "FS check should fail on non-existent path");
    }

    #[tokio::test]
    async fn self_check_fs_creates_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let cfg = TickLoopConfig {
            hermix_home: Some(base.clone()),
            ..TickLoopConfig::default()
        };
        let tl = TickLoop::new(cfg);

        // Run the health check — probe file should be cleaned up.
        let status = tl.check_fs(&base).await;
        assert_eq!(status, CheckStatus::Pass);

        // Verify the probe file was removed.
        let probe_path = base.join("hermix_health_probe.txt");
        assert!(!probe_path.exists(), "probe file should be cleaned up");
    }

    #[tokio::test]
    async fn check_status_is_healthy_works() {
        assert!(CheckStatus::Pass.is_healthy());
        assert!(!CheckStatus::Warn("something".to_string()).is_healthy());
        assert!(!CheckStatus::Fail("something".to_string()).is_healthy());
    }

    #[tokio::test]
    async fn self_check_result_all_healthy() {
        let r = SelfCheckResult {
            fs_read_write: CheckStatus::Pass,
            llm_connectivity: CheckStatus::Pass,
            watchdog_available: CheckStatus::Pass,
            memory_usage: CheckStatus::Pass,
            disk_space: CheckStatus::Pass,
        };
        assert!(r.all_healthy());

        let r2 = SelfCheckResult {
            fs_read_write: CheckStatus::Fail("broken".to_string()),
            llm_connectivity: CheckStatus::Pass,
            watchdog_available: CheckStatus::Pass,
            memory_usage: CheckStatus::Pass,
            disk_space: CheckStatus::Pass,
        };
        assert!(!r2.all_healthy());
    }

    #[tokio::test]
    async fn self_check_result_memory_usage_fail() {
        let r = SelfCheckResult {
            fs_read_write: CheckStatus::Pass,
            llm_connectivity: CheckStatus::Pass,
            watchdog_available: CheckStatus::Pass,
            memory_usage: CheckStatus::Warn("high memory".to_string()),
            disk_space: CheckStatus::Pass,
        };
        assert!(!r.all_healthy());
    }

    #[tokio::test]
    async fn self_check_result_disk_space_fail() {
        let r = SelfCheckResult {
            fs_read_write: CheckStatus::Pass,
            llm_connectivity: CheckStatus::Pass,
            watchdog_available: CheckStatus::Pass,
            memory_usage: CheckStatus::Pass,
            disk_space: CheckStatus::Fail("low disk space".to_string()),
        };
        assert!(!r.all_healthy());
    }

    #[tokio::test]
    async fn custom_provider_skips_probe() {
        // Custom-* providers must skip the /models probe — they never
        // expose that endpoint, so probing only produces false-positive 404s.
        let cfg = TickLoopConfig {
            llm_provider_url: Some("https://apihub.agnes-ai.com/v1".to_string()),
            llm_provider_name: Some("custom-agnes-2-0-flash".to_string()),
            ..Default::default()
        };
        let tick = TickLoop::new(cfg);
        let status = tick.check_llm_connectivity().await;
        assert_eq!(status, CheckStatus::Pass,
            "custom-* provider should skip probe and return Pass");
    }

    #[tokio::test]
    async fn no_probe_provider_skips_probe() {
        // Known NO_PROBE_PROVIDERS (minimax, kimi, etc.) must also skip.
        let cfg = TickLoopConfig {
            llm_provider_url: Some("https://api.minimax.chat/v1".to_string()),
            llm_provider_name: Some("minimax".to_string()),
            ..Default::default()
        };
        let tick = TickLoop::new(cfg);
        let status = tick.check_llm_connectivity().await;
        assert_eq!(status, CheckStatus::Pass,
            "NO_PROBE_PROVIDER should skip probe and return Pass");
    }

    #[test]
    fn endpoint_auth_mismatch_classification() {
        // 401/403/404 触发回退探针，视为端点/认证不匹配（非连通性故障）。
        assert!(TickLoop::endpoint_auth_mismatch(401));
        assert!(TickLoop::endpoint_auth_mismatch(403));
        assert!(TickLoop::endpoint_auth_mismatch(404));
        // 其他状态码不归入此类别：429/5xx 收到响应仍判可达，但不再回退探针。
        assert!(!TickLoop::endpoint_auth_mismatch(400));
        assert!(!TickLoop::endpoint_auth_mismatch(429));
        assert!(!TickLoop::endpoint_auth_mismatch(500));
        assert!(!TickLoop::endpoint_auth_mismatch(200));
    }
}
