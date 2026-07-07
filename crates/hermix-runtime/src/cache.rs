//! M4: Cache layer with TTL and background warming.
//!
//! Provides a `CacheManager` that wraps `moka` for high-performance
//! concurrent caching with TTL support. Includes a `CacheWarmer` trait
//! for implementing background cache warming during the awakening phase.
//!
//! ## Design
//!
//! - **CacheManager**: Generic cache wrapper with TTL support
//! - **CacheWarmer**: Trait for implementing cache warming strategies
//! - **WarmingResult**: Result of a cache warming operation
//!
//! ## Usage
//!
//! ```rust,no_run
//! use hermix_runtime::cache::{CacheManager, CacheWarmer, WarmingResult};
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! // Create a cache with 5-minute TTL
//! let cache: CacheManager<String, String> = CacheManager::new(
//!     "my_cache",
//!     Duration::from_secs(300),
//!     1000, // max capacity
//! );
//!
//! // Insert a value
//! cache.insert("key1".to_string(), "value1".to_string()).await;
//!
//! // Get a value
//! let value = cache.get(&"key1".to_string()).await;
//! ```

use std::fmt::Display;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moka::future::Cache;
use tokio::sync::Mutex;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// CacheManager
// ---------------------------------------------------------------------------

/// A high-performance concurrent cache with TTL support.
///
/// Wraps `moka::future::Cache` with convenience methods and metrics.
pub struct CacheManager<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// The name of this cache (for logging/metrics).
    name: String,
    /// The underlying moka cache.
    cache: Cache<K, V>,
    /// Default TTL for entries.
    ttl: Duration,
    /// Cache metrics for monitoring.
    metrics: Arc<Mutex<CacheMetrics>>,
    /// Live entry count, tracked in the wrapper.
    ///
    /// moka 0.12's `future::Cache::entry_count()` returns a stale estimate
    /// until `run_pending_tasks()` is awaited. Tracking here gives callers
    /// an exact, lock-free count that reflects wrapper-mediated insert /
    /// remove / invalidate operations.
    live_count: Arc<AtomicU64>,
}

impl<K, V> CacheManager<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + Display + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Create a new cache with the given name, TTL, and max capacity.
    pub fn new(name: impl Into<String>, ttl: Duration, max_capacity: u64) -> Self {
        let name = name.into();
        let live_count = Arc::new(AtomicU64::new(0));
        let live_count_for_listener = Arc::clone(&live_count);
        let cache = Cache::builder()
            .max_capacity(max_capacity)
            .time_to_live(ttl)
            // The eviction listener decrements our tracked live_count when
            // moka evicts an entry due to capacity pressure or TTL expiry.
            // This keeps `entry_count()` exact and lock-free for callers
            // without requiring them to `await run_pending_tasks()` first.
            .async_eviction_listener(move |_key, _value, _cause| {
                let counter = Arc::clone(&live_count_for_listener);
                Box::pin(async move {
                    counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                        Some(n.saturating_sub(1))
                    }).ok();
                })
            })
            .build();

        info!(target: "hermix.runtime.cache", name = %name, ttl_secs = ttl.as_secs(), max_capacity, "CacheManager created");

        Self {
            name,
            cache,
            ttl,
            metrics: Arc::new(Mutex::new(CacheMetrics::default())),
            live_count,
        }
    }

    /// Insert a value into the cache.
    pub async fn insert(&self, key: K, value: V) {
        self.cache.insert(key, value).await;
        self.live_count.fetch_add(1, Ordering::Relaxed);
        let mut metrics = self.metrics.lock().await;
        metrics.insertions += 1;
        debug!(target: "hermix.runtime.cache", name = %self.name, "Cache entry inserted");
    }

    /// Get a value from the cache.
    pub async fn get(&self, key: &K) -> Option<V> {
        let result = self.cache.get(key).await;
        let mut metrics = self.metrics.lock().await;
        if result.is_some() {
            metrics.hits += 1;
        } else {
            metrics.misses += 1;
        }
        result
    }

    /// Remove a value from the cache.
    pub async fn remove(&self, key: &K) {
        if self.cache.remove(key).await.is_some() {
            // Only decrement if the key was actually present, so re-removes
            // of an absent key don't push the counter negative.
            self.live_count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            }).ok();
        }
        debug!(target: "hermix.runtime.cache", name = %self.name, "Cache entry removed");
    }

    /// Check if a key exists in the cache.
    pub async fn contains_key(&self, key: &K) -> bool {
        self.cache.contains_key(key)
    }

    /// Get the number of entries in the cache.
    ///
    /// This is a wrapper-tracked exact count, updated on every `insert`,
    /// `remove`, and `invalidate_all` call (see `live_count`). It is
    /// preferred over the underlying moka `entry_count()`, which only
    /// returns an accurate value after `run_pending_tasks()` is awaited.
    pub fn entry_count(&self) -> u64 {
        self.live_count.load(Ordering::Relaxed)
    }

    /// Invalidate all entries in the cache.
    pub async fn invalidate_all(&self) {
        self.cache.invalidate_all();
        self.live_count.store(0, Ordering::Relaxed);
        info!(target: "hermix.runtime.cache", name = %self.name, "Cache invalidated all entries");
    }

    /// Get the cache name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the default TTL.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Get current cache metrics.
    pub async fn metrics(&self) -> CacheMetrics {
        self.metrics.lock().await.clone()
    }
}

// ---------------------------------------------------------------------------
// CacheWarmer
// ---------------------------------------------------------------------------

/// Result of a cache warming operation.
#[derive(Debug, Clone)]
pub struct WarmingResult {
    /// Number of entries warmed.
    pub entries_warmed: usize,
    /// Duration of the warming operation.
    pub duration: Duration,
    /// Whether the warming was successful.
    pub success: bool,
    /// Error message if warming failed.
    pub error: Option<String>,
}

/// Trait for implementing cache warming strategies.
///
/// Implementors should populate the cache with initial data
/// during the awakening phase of the TickLoop.
#[async_trait::async_trait]
pub trait CacheWarmer<K, V>: Send + Sync
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Warm the cache with initial data.
    ///
    /// This is called during the awakening phase to pre-populate
    /// the cache with frequently accessed data.
    async fn warm(&self, cache: &CacheManager<K, V>) -> WarmingResult;

    /// Get the name of this warmer (for logging).
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// CacheMetrics
// ---------------------------------------------------------------------------

/// Metrics for cache performance monitoring.
#[derive(Debug, Clone, Default)]
pub struct CacheMetrics {
    /// Total number of cache hits.
    pub hits: u64,
    /// Total number of cache misses.
    pub misses: u64,
    /// Total number of insertions.
    pub insertions: u64,
    /// Total number of evictions.
    pub evictions: u64,
}

impl CacheMetrics {
    /// Calculate the hit rate as a percentage.
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            (self.hits as f64 / total as f64) * 100.0
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_manager_basic_operations() {
        let cache: CacheManager<String, String> = CacheManager::new(
            "test_cache",
            Duration::from_secs(60),
            100,
        );

        // Insert
        cache.insert("key1".to_string(), "value1".to_string()).await;

        // Get
        let value = cache.get(&"key1".to_string()).await;
        assert_eq!(value, Some("value1".to_string()));

        // Contains
        assert!(cache.contains_key(&"key1".to_string()).await);
        assert!(!cache.contains_key(&"key2".to_string()).await);

        // Entry count
        assert_eq!(cache.entry_count(), 1);

        // Remove
        cache.remove(&"key1".to_string()).await;
        assert_eq!(cache.get(&"key1".to_string()).await, None);
    }

    #[tokio::test]
    async fn cache_manager_ttl_expiration() {
        let cache: CacheManager<String, String> = CacheManager::new(
            "ttl_test",
            Duration::from_millis(100), // Very short TTL
            100,
        );

        cache.insert("key1".to_string(), "value1".to_string()).await;
        assert!(cache.contains_key(&"key1".to_string()).await);

        // Wait for TTL to expire
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Entry should be expired
        assert!(!cache.contains_key(&"key1".to_string()).await);
    }

    #[tokio::test]
    async fn cache_manager_invalidate_all() {
        let cache: CacheManager<String, String> = CacheManager::new(
            "invalidate_test",
            Duration::from_secs(60),
            100,
        );

        cache.insert("key1".to_string(), "value1".to_string()).await;
        cache.insert("key2".to_string(), "value2".to_string()).await;
        assert_eq!(cache.entry_count(), 2);

        cache.invalidate_all().await;

        // Entries should be invalidated
        assert!(!cache.contains_key(&"key1".to_string()).await);
        assert!(!cache.contains_key(&"key2".to_string()).await);
    }

    #[tokio::test]
    async fn cache_manager_max_capacity() {
        let cache: CacheManager<String, String> = CacheManager::new(
            "capacity_test",
            Duration::from_secs(60),
            2, // Only 2 entries allowed
        );

        cache.insert("key1".to_string(), "value1".to_string()).await;
        cache.insert("key2".to_string(), "value2".to_string()).await;
        cache.insert("key3".to_string(), "value3".to_string()).await;

        // moka's eviction is async; flush pending tasks (which also runs
        // the eviction listener) before asserting. Without this, the
        // listener hasn't run yet and our tracked count still reads 3.
        cache.cache.run_pending_tasks().await;

        // One entry should be evicted
        assert!(cache.entry_count() <= 2);
    }

    #[tokio::test]
    async fn cache_metrics_hit_rate() {
        let mut metrics = CacheMetrics::default();
        assert_eq!(metrics.hit_rate(), 0.0);

        metrics.hits = 7;
        metrics.misses = 3;
        assert!((metrics.hit_rate() - 70.0).abs() < f64::EPSILON);
    }

    // Mock warmer for testing
    struct MockWarmer {
        name: String,
        entries_to_warm: usize,
    }

    impl MockWarmer {
        fn new(name: impl Into<String>, entries: usize) -> Self {
            Self {
                name: name.into(),
                entries_to_warm: entries,
            }
        }
    }

    #[async_trait::async_trait]
    impl CacheWarmer<String, String> for MockWarmer {
        async fn warm(&self, cache: &CacheManager<String, String>) -> WarmingResult {
            let start = std::time::Instant::now();

            for i in 0..self.entries_to_warm {
                cache.insert(
                    format!("key_{}", i),
                    format!("value_{}", i),
                ).await;
            }

            WarmingResult {
                entries_warmed: self.entries_to_warm,
                duration: start.elapsed(),
                success: true,
                error: None,
            }
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    #[tokio::test]
    async fn cache_warmer_mock() {
        let cache: CacheManager<String, String> = CacheManager::new(
            "warmer_test",
            Duration::from_secs(60),
            1000,
        );

        let warmer = MockWarmer::new("test_warmer", 5);
        let result = warmer.warm(&cache).await;

        assert!(result.success);
        assert_eq!(result.entries_warmed, 5);
        assert!(result.error.is_none());
        assert_eq!(cache.entry_count(), 5);
    }
}
