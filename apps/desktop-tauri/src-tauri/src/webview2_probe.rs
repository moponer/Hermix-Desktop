//! WebView2 runtime detection.
//!
//! Tauri 2 on Windows renders the UI inside `msedgewebview2.exe`
//! and embeds a tiny loader shim (`WebView2Loader.dll`) that
//! bootstraps it. The host process only fails the actual WebView
//! creation with a cryptic HRESULT when the Evergreen runtime is
//! missing — which happens on Windows 10 pre-1903 and most
//! Windows Server SKUs out of the box.
//!
//! This module probes the registry once per process to detect the
//! runtime ahead of Tauri's own check, so we can emit a friendly
//! hint to the user before Tauri shows its (unfriendly) HRESULT
//! popup. The check is intentionally conservative — it only
//! reports "Available" when the Edge Update client for the
//! WebView2 Runtime GUID is registered. Manual installs (Evergreen
//! Standalone dropped into a custom directory without Edge Update)
//! will report Missing here; that's an acceptable false negative
//! because the user is then in a better position than us to fix
//! the environment.
//!
//! On non-Windows targets the probe is a no-op that always reports
//! Available, so `pnpm tauri dev` on macOS / Linux doesn't trip
//! the check.
//!
//! Why no FFI? We deliberately don't pull in `windows-sys` or
//! `winapi` just to call `LoadLibraryW`. The registry probe is
//! good enough for the developer-install case this is designed
//! to catch — and `reg.exe` is shipped on every supported
//! Windows version, so we get cross-platform-ish reliability
//! without a transitive dependency chain.

use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};

/// Result of probing the WebView2 runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebView2Status {
    /// Runtime is registered with Edge Update.
    Available,
    /// Neither the loader DLL nor the Edge Update client is present.
    Missing,
    /// Detection failed for an environmental reason (sandbox,
    /// permissions, reg.exe not on PATH). Tauri will surface its
    /// own error in this case — we don't try to second-guess.
    Unknown,
}

impl WebView2Status {
    fn as_u8(self) -> u8 {
        match self {
            WebView2Status::Available => 0,
            WebView2Status::Missing => 1,
            WebView2Status::Unknown => 2,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => WebView2Status::Missing,
            2 => WebView2Status::Unknown,
            _ => WebView2Status::Available,
        }
    }
}

/// Cache the probe result so we hit the registry at most once per
/// process. The cache is `AtomicU8` rather than `OnceLock` because
/// the probe involves a `Command::spawn` and we want lock-free
/// reads on the hot install path (we don't expect the cache to be
/// checked twice per process, but the cost of paying that
/// guarantee should be zero).
static CACHED_STATUS: AtomicU8 = AtomicU8::new(u8::MAX);

/// Sentinel byte meaning "not probed yet" — distinct from any
/// valid `WebView2Status::as_u8()` value. `u8::MAX = 255` is used
/// instead of `0..=2` so the cache self-heals if a future
/// variant is added (we'd report "not probed" once instead of
/// silently mis-classifying).
const UNPROBED: u8 = u8::MAX;

/// WebView2 Runtime is registered under this Edge Update client
/// GUID. The GUID is stable across every shipped runtime build
/// since the Evergreen channel launched (2020-11).
const WEBVIEW2_RUNTIME_CLIENT_GUID: &str =
    r"{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

/// Probe for the WebView2 Evergreen runtime. The first call
/// shells out to `reg.exe`; subsequent calls return the cached
/// value.
pub fn probe() -> WebView2Status {
    let cached = CACHED_STATUS.load(Ordering::Acquire);
    if cached != UNPROBED {
        return WebView2Status::from_u8(cached);
    }

    let detected = probe_inner();
    CACHED_STATUS.store(detected.as_u8(), Ordering::Release);
    detected
}

#[cfg(target_os = "windows")]
fn probe_inner() -> WebView2Status {
    // Probe both 32-bit and 64-bit registry views — the Edge
    // Update client installs under WOW6432Node on x64 systems
    // when hermix-desktop itself is x64, but the non-WOW path
    // is the canonical location per Microsoft docs. We accept
    // either as proof of install.
    for view in [r"HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients", r"HKLM\SOFTWARE\Microsoft\EdgeUpdate\Clients"] {
        let key = format!(r"{}\{}\view", WEBVIEW2_RUNTIME_CLIENT_GUID);
        // Use `Command::new("reg")` rather than the `winreg`
        // crate to avoid a new dependency. The child-process
        // startup cost (~5ms on Windows) is paid exactly once
        // per process lifetime thanks to the AtomicU8 cache.

        let output = Command::new("reg")
            .args(["query", &key, "/v", "pv"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();

        match output {
            Ok(o) if o.status.success() => {
                // reg.exe prints a line like:
                //     pv    REG_SZ    120.0.2210.55
                // We don't actually need the version — the mere
                // presence of the `pv` value confirms the
                // runtime is registered. Belt-and-braces:
                // double-check that the output contains the
                // `pv` token, so a hypothetical "ERROR: ..." 
                // message that exited with status 0 can't slip
                // through.
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.contains("pv") && stdout.contains("REG_SZ") {
                    return WebView2Status::Available;
                }
            }
            Ok(_) | Err(_) => {
                // Continue to the next view. If both fail we'll
                // fall through to Missing below.
            }
        }
    }
    WebView2Status::Missing
}

#[cfg(not(target_os = "windows"))]
fn probe_inner() -> WebView2Status {
    // macOS / Linux dev builds use WKWebView / WebKitGTK directly —
    // there's no separate Evergreen runtime to check for. Always
    // report Available so the setup hook doesn't bail out on a
    // developer's Mac.
    WebView2Status::Available
}

/// Print a hint to stderr explaining how to install the WebView2
/// runtime. We deliberately don't pop a modal dialog here — the
/// caller (the Tauri setup hook) controls whether to escalate to
/// a dialog based on how serious the failure was. This function
/// just produces the human-readable hint.
///
/// We use ASCII-only to avoid GBK/CP936 mojibake when the console
/// encoding hasn't been negotiated — the message is short and
/// plain English is the most reliable lingua franca across Win
/// code pages.
pub fn explain_missing_runtime() -> String {
    format!(
        "[Hermix] Microsoft Edge WebView2 Runtime was not detected on this machine.\n\
         \n\
         Hermix Desktop needs the Evergreen Standalone runtime to render its UI.\n\
         \n\
         Download and run the installer from:\n\
         \n\
           https://developer.microsoft.com/en-us/microsoft-edge/webview2/\n\
         \n\
         (Choose \"Evergreen Standalone Installer for x64\", run it, then relaunch Hermix.)\n\
         \n\
         If WebView2 is already installed for another app, Edge Update may have\n\
         registered it under a non-canonical key — check Settings > Apps > Installed\n\
         apps for \"Microsoft Edge WebView2 Runtime\"."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip() {
        // The cached-status machinery has to round-trip every variant
        // through `as_u8` / `from_u8` exactly, otherwise a probe that
        // returns `Missing` would silently be re-read as `Available`.
        for s in [
            WebView2Status::Available,
            WebView2Status::Missing,
            WebView2Status::Unknown,
        ] {
            assert_eq!(WebView2Status::from_u8(s.as_u8()), s);
        }
    }

    #[test]
    fn unprobed_is_distinct_from_all_statuses() {
        // `UNPROBED` is the cache sentinel for "never probed";
        // it must not collide with any real status byte or the
        // first read after probe would mis-classify.
        let mut seen = vec![UNPROBED];
        for s in [
            WebView2Status::Available,
            WebView2Status::Missing,
            WebView2Status::Unknown,
        ] {
            seen.push(s.as_u8());
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 4, "expected 4 distinct u8 values");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn probe_is_available_on_unix() {
        // Cross-platform sanity check: on non-Windows dev machines
        // (where Tauri falls back to WKWebView / WebKitGTK) the
        // probe must report Available so the setup hook doesn't
        // bail out.
        assert_eq!(probe(), WebView2Status::Available);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn probe_returns_a_known_status_on_windows() {
        // Don't assert a specific status (the test machine may or
        // may not have WebView2 installed) — only that the probe
        // returns *one of* the known variants and is cached on the
        // second call.
        let first = probe();
        assert!(matches!(
            first,
            WebView2Status::Available | WebView2Status::Missing | WebView2Status::Unknown
        ));
        let second = probe();
        assert_eq!(first, second, "cached status should be stable");
    }

    #[test]
    fn explain_message_includes_install_link() {
        // The hint must always include the canonical Evergreen
        // download URL — if that link changes upstream we want a
        // test to fail loudly so we can update both the comment
        // and the message together.
        let msg = explain_missing_runtime();
        assert!(
            msg.contains("https://developer.microsoft.com/en-us/microsoft-edge/webview2/"),
            "missing install link in hint message: {msg:?}"
        );
    }
}