//! 单条目 TTL 缓存（进程内）。
//!
//! 用途：把「每次请求都扫盘/扫目录」的只读端点压到"最多每 N 秒一次"。
//! 刻意不做多条目/淘汰策略——这些端点的 key 只有一个（或没有），单槽足够，
//! 而带淘汰的缓存要处理容量、LRU、失效广播，是另一个量级的复杂度。
//!
//! 失效语义：TTL 到期 or 调用方显式 `invalidate()`（写操作后必须调）。

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) struct TtlCache<K, V> {
    ttl: Duration,
    slot: Mutex<Option<(Instant, K, V)>>,
}

impl<K, V> TtlCache<K, V> {
    pub(crate) const fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            slot: Mutex::new(None),
        }
    }
}

impl<K: PartialEq, V: Clone> TtlCache<K, V> {
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        let guard = self.slot.lock().ok()?;
        let (at, cached_key, value) = guard.as_ref()?;
        if cached_key == key && at.elapsed() < self.ttl {
            Some(value.clone())
        } else {
            None
        }
    }

    pub(crate) fn put(&self, key: K, value: V) {
        if let Ok(mut guard) = self.slot.lock() {
            *guard = Some((Instant::now(), key, value));
        }
    }

    pub(crate) fn invalidate(&self) {
        if let Ok(mut guard) = self.slot.lock() {
            *guard = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_within_ttl_and_misses_after_key_change() {
        let cache: TtlCache<&str, u32> = TtlCache::new(Duration::from_secs(60));
        assert!(cache.get(&"a").is_none());
        cache.put("a", 1);
        assert_eq!(cache.get(&"a"), Some(1));
        // 不同 key → 未命中（单槽只保留最近一次）
        assert!(cache.get(&"b").is_none());
        cache.invalidate();
        assert!(cache.get(&"a").is_none());
    }

    #[test]
    fn expires_after_ttl() {
        let cache: TtlCache<(), u32> = TtlCache::new(Duration::from_millis(0));
        cache.put((), 7);
        assert!(cache.get(&()).is_none(), "TTL 为 0 时立刻过期");
    }
}
