//! Send 去重：同 session 同文本在窗口内只发一次（自 ws.rs 拆分，P1-13）。
use super::*;

/// Compute the dedup key for `(session_id, text)`. Stable across
/// processes for the same inputs (uses `DefaultHasher`'s SipHash).
/// Exposed for testing.
pub(crate) fn dedup_key(session_id: &str, text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{}:{:x}", session_id, hasher.finish())
}

/// Dedup window. Anything within this many seconds of a prior
/// identical (session_id, text) send is rejected.
pub(crate) const DEDUP_WINDOW_SECS: u64 = 30;
/// Map GC horizon. Entries older than this are evicted on every
/// insert so the map stays bounded.
const DEDUP_GC_SECS: u64 = 60;

/// Check whether `(session_id, text)` was sent recently. Returns
/// `Some(elapsed_secs)` if a duplicate was found within the dedup
/// window (caller should reject), or `None` if the send is fresh
/// (caller should proceed and the entry is recorded).
/// extracted from `handle_ws_stream_send` so the dedup
/// contract is unit-testable without a WebSocket.
pub(crate) fn check_recent_send(state: &HttpServerState, session_id: &str, text: &str) -> Option<u64> {
    let key = dedup_key(session_id, text);
    let now = std::time::Instant::now();
    // Dedup reached (diagnostic; suppressed at default tracing level).
    tracing::debug!(
        session = %session_id,
        key = %key,
        text_len = text.len(),
        "dedup_check: entering check_recent_send"
    );
    // Check + Record + GC inside a single critical section so
    // 50-thread storms can't slip past the TOCTOU window between
    // check and insert. Previously the check and insert took the
    // mutex twice — two threads could both see "key absent" before
    // either inserted, each returning `None` (counting as "fresh")
    // and producing duplicate processing. With everything in one
    // lock the first thread sees an empty map → inserts and returns
    // `None`; every subsequent thread sees the freshly-inserted
    // entry and returns `Some(elapsed)` for dedup.
    // Mutex poisoned: fail-open (don't block the user). Caller
    // will see `None` and proceed without dedup.
    let duplicate: Option<u64> = match state.recent_sends.lock() {
        Ok(mut map) => {
            // GC first so an expired entry can't accidentally
            // pass the window check, then re-check.
            map.retain(|_, t| now.duration_since(*t).as_secs() < DEDUP_GC_SECS);
            match map.get(&key) {
                Some(last) => {
                    let e = now.duration_since(*last).as_secs();
                    if e < DEDUP_WINDOW_SECS {
                        Some(e)
                    } else {
                        // Expired — drop and fall through to insert.
                        map.insert(key.clone(), now);
                        None
                    }
                }
                None => {
                    map.insert(key.clone(), now);
                    None
                }
            }
        }
        Err(_) => None,
    };
    if duplicate.is_some() {
        tracing::debug!(
            session = %session_id,
            elapsed_secs = duplicate.unwrap_or(0),
            "dedup_check: DUPLICATE found"
        );
        return duplicate;
    }
    tracing::debug!(
        session = %session_id,
        key = %key,
        "dedup_check: FRESH — inserted into map"
    );
    None
}
