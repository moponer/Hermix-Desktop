//! Watchdog — enforce a maximum execution duration for tool calls and
//! agent turns. Inspired by BaiLongma's `runTurn` 180s abort.
//!
//! The watchdog is a simple timeout mechanism:
//!
//! - Start a timer when a long-running operation begins.
//! - If the timer expires before the operation completes, the watchdog
//!   fires an abort signal (via `InterruptController`).
//! - On abort, the agent loop can retry or return a timeout error.
//!
//! The watchdog is designed to be cooperative — it doesn't kill threads,
//! it just signals that time is up.

use std::time::Duration;

use tokio::sync::watch;
use tracing::warn;

/// Default tool-call timeout: 180 seconds (BaiLongma standard).
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 180;

/// Default turn timeout: 600 seconds (10 minutes).
pub const DEFAULT_TURN_TIMEOUT_SECS: u64 = 600;

// ---------------------------------------------------------------------------
// WatchdogConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// Max duration for a single tool call. Default 180s.
    pub tool_timeout: Duration,
    /// Max duration for an entire agent turn. Default 600s.
    pub turn_timeout: Duration,
    /// Whether the watchdog is enabled. Default true.
    pub enabled: bool,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            tool_timeout: Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS),
            turn_timeout: Duration::from_secs(DEFAULT_TURN_TIMEOUT_SECS),
            enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Watchdog
// ---------------------------------------------------------------------------

/// A simple timeout watchdog. Start it when a long operation begins;
/// if it expires before being cancelled, an abort signal is sent.
pub struct Watchdog {
    cfg: WatchdogConfig,
    /// Broadcast channel: receiver gets `true` when a timeout fires.
    abort_tx: watch::Sender<bool>,
}

impl Watchdog {
    pub fn new(cfg: WatchdogConfig) -> Self {
        let (abort_tx, _abort_rx) = watch::channel(false);
        Self {
            cfg,
            abort_tx,
        }
    }

    /// Subscribe to abort signals.
    pub fn subscribe_abort(&self) -> watch::Receiver<bool> {
        self.abort_tx.subscribe()
    }

    /// Start a tool-call timer. Returns a guard that must be held for the
    /// duration of the call; dropping the guard cancels the timer.
    ///
    /// The watchdog does NOT cancel `tokio` tasks — it only sends a signal.
    /// The caller is responsible for checking `abort_rx` and stopping work.
    pub fn watch_tool(&self) -> WatchdogGuard {
        let abort_tx = self.abort_tx.clone();
        let timeout = self.cfg.tool_timeout;

        if !self.cfg.enabled {
            return WatchdogGuard {
                _inner: tokio::spawn(async {}),
            };
        }

        let handle = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            warn!(target: "hermix.runtime.watchdog", timeout_ms = timeout.as_millis(),
                "Watchdog: tool timeout fired, sending abort signal");
            let _ = abort_tx.send(true);
        });

        WatchdogGuard { _inner: handle }
    }

    /// Reset the abort signal (after handling a timeout).
    pub fn reset(&self) {
        let _ = self.abort_tx.send(false);
    }
}

/// A RAII guard that cancels the watchdog when dropped.
pub struct WatchdogGuard {
    _inner: tokio::task::JoinHandle<()>,
}

impl Drop for WatchdogGuard {
    fn drop(&mut self) {
        self._inner.abort();
    }
}

// ---------------------------------------------------------------------------
// TESTS
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watchdog_cancels_on_guard_drop() {
        let wd = Watchdog::new(WatchdogConfig {
            tool_timeout: Duration::from_millis(500),
            ..WatchdogConfig::default()
        });
        let rx = wd.subscribe_abort();

        // Start a tool timer and immediately drop the guard.
        {
            let _guard = wd.watch_tool();
        }
        // Guard was dropped, so the timer should have been cancelled.
        tokio::time::sleep(Duration::from_millis(600)).await;
        // The abort signal should not have fired.
        assert!(!*rx.borrow(), "Watchdog should NOT have fired after guard was dropped");
    }

    #[tokio::test]
    async fn watchdog_fires_after_timeout() {
        let wd = Watchdog::new(WatchdogConfig {
            tool_timeout: Duration::from_millis(100),
            ..WatchdogConfig::default()
        });
        let rx = wd.subscribe_abort();

        let _guard = wd.watch_tool();
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(*rx.borrow(), "Watchdog should have fired after timeout");
    }

    #[tokio::test]
    async fn disabled_watchdog_never_fires() {
        let wd = Watchdog::new(WatchdogConfig {
            enabled: false,
            tool_timeout: Duration::from_millis(50),
            ..WatchdogConfig::default()
        });
        let rx = wd.subscribe_abort();

        let _guard = wd.watch_tool();
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(!*rx.borrow(), "Disabled watchdog should NOT have fired");
    }

    #[tokio::test]
    async fn watchdog_reset_clears_signal() {
        let wd = Watchdog::new(WatchdogConfig {
            tool_timeout: Duration::from_millis(100),
            ..WatchdogConfig::default()
        });
        let rx = wd.subscribe_abort();

        let _guard = wd.watch_tool();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(*rx.borrow(), "Watchdog should have fired");

        wd.reset();
        assert!(!*rx.borrow(), "After reset, signal should be false");
    }
}
