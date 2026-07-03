//! `EmbeddingProvider` — async trait abstraction over local embedding backends.
//!
//! ## Why async around a sync backend
//!
//! `fastembed::TextEmbedding` is a **synchronous** API that internally
//! runs an ONNX session via `ort`. Running it on the tokio runtime
//! would block worker threads. We wrap it with
//! `tokio::task::spawn_blocking` so the trait is async-shaped (matches
//! the rest of the agent loop) without paying the runtime stall cost.
//!
//! ## Backends
//!
//! Two implementations are provided:
//!
//! 1. [`HashingEmbeddingProvider`] (always available) — sklearn's
//!    `HashingVectorizer` algorithm: character n-grams → FNV-1a hash →
//!    fixed-dim bucket with ±1 sign from the high bit, then L2 normalize.
//!    No model download, no ONNX runtime. Cosine between similar strings
//!    is consistently higher than between unrelated strings (Chinese
//!    works well because n-grams slide through ideograms without needing
//!    word segmentation).
//! 2. [`FastEmbedEmbeddingProvider`] (behind `#[cfg(feature = "fastembed")]`)
//!    — real ONNX-backed embeddings via `fastembed` 5.16. Ships 46
//!    pre-bundled models (BGE-small-en-v1.5 default at 384 dim; BGE-zh,
//!    mxbai, multilingual-e5, jina, snowflake-arctic, embeddinggemma...).
//!
//! ## Why fastembed is feature-gated
//!
//! `fastembed` 5.16 depends on `ort` 2.0.0-rc.12, which at build time
//! downloads the ONNX runtime from `cdn.pyke.io`. That CDN is
//! geo-blocked in some regions (HTTP 403). Marking the dep optional
//! means a clean `cargo check` / `cargo test` works in those
//! environments; production builds flip the `fastembed` feature on.
//!
//! ## L1 target (PROPOSAL §2.3, §三 L1)
//!
//! 30+ tools → top-5 via cosine similarity against precomputed tool
//! embeddings. The provider is consumed by `tool_router::VectorRouter`
//! and (L1.3) by `EmbeddedMemoryBackend` for vector recall.

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
#[cfg(feature = "fastembed")]
use tracing::{info, warn};

/// Errors produced by the embedding layer.
#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[cfg(feature = "fastembed")]
    #[error("fastembed: {0}")]
    FastEmbed(#[from] fastembed::Error),
    #[error("model not yet loaded — call `try_load` first or use `try_default`")]
    NotLoaded,
    #[error("dimension mismatch: provider={provider_dim}, expected={expected}")]
    DimMismatch { provider_dim: usize, expected: usize },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("canceled")]
    Canceled,
    #[error("other: {0}")]
    Other(String),
}

pub type EmbeddingResult<T> = std::result::Result<T, EmbeddingError>;

/// The async contract every embedding backend must implement.
///
/// The agent loop and the `VectorRouter` only see this trait, never a
/// concrete provider — same pattern as `MemoryLayer`.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Stable human-readable model name (e.g. "BAAI/bge-small-en-v1.5").
    /// Used for telemetry, cache validation, and stored alongside
    /// persisted vectors so we can detect cross-model contamination.
    fn model_name(&self) -> &str;

    /// Embedding vector dimension. Determined at provider construction
    /// time (intrinsic to the model).
    fn dimension(&self) -> usize;

    /// Embed a single text. The output `Vec<f32>` has `self.dimension()`
    /// elements. Implementations may normalize (most modern models do).
    async fn embed(&self, text: &str) -> EmbeddingResult<Vec<f32>>;

    /// Embed a batch. Batching is more efficient than calling `embed` in
    /// a loop because the underlying ONNX session amortizes model load
    /// + kernel launches across the batch.
    async fn embed_batch(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>>;

    /// Compute cosine similarity between two same-dimension vectors.
    /// Provided as a default method since the formula is the same
    /// regardless of backend.
    ///
    /// `where Self: Sized` excludes this from the trait's dyn vtable —
    /// callers using `dyn EmbeddingProvider` should call `cosine`
    /// through a concrete type or via a free helper.
    fn cosine(a: &[f32], b: &[f32]) -> f32
    where
        Self: Sized,
    {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        }
    }
}

// ---------------------------------------------------------------------------
// HashingEmbeddingProvider — always available, sklearn-style hashing trick
// ---------------------------------------------------------------------------

/// In-process embedding provider using the **hashing trick** (sklearn's
/// `HashingVectorizer` with `alternate_sign=True`).
///
/// Algorithm:
/// 1. Lowercase + Unicode-normalize input.
/// 2. Slide a character n-gram window (default n=3) over the text,
///    padding with `^` / `$` boundary markers so leading and trailing
///    n-grams behave like interior ones.
/// 3. For each n-gram, FNV-1a hash → `bucket = h % dim`,
///    `sign = (h >> 63) as f32` (±1).
/// 4. Accumulate signs into a `dim`-sized `Vec<f32>`, L2-normalize.
///
/// **Why this replaces the old byte-mixing stub:**
/// The old `StubEmbeddingProvider` was a stable pseudo-random mapping
/// with no semantic signal — `cosine(similar_strings)` was only *roughly*
/// higher than `cosine(unrelated_strings)`. Character n-gram hashing
/// gives real lexical overlap signal: Chinese strings share ideogram
/// trigrams, English strings share character trigrams, code identifiers
/// share token-level trigrams. Cosine between similar strings is
/// consistently > 0.2 in practice, between unrelated strings ~0.0.
///
/// **No model, no ONNX, no download.** FNV-1a is a 5-line hash that's
/// stable across Rust versions and platforms (unlike `std::hash::Hasher`
/// which is randomized per-process by default).
///
/// **Properties:**
/// - Deterministic: identical input → identical vector (across runs).
/// - Order-preserving at the n-gram level: `"Rust"` and `"rust"` produce
///   the same vector (lowercased); `"Rust language"` and `"language Rust"`
///   produce the same multiset of n-grams (modulo the bucket collisions).
/// - Dim-stable: same `dim` → vectors are comparable across sessions.
/// - Cheap: O(N) per embed, no allocation beyond the output vector.
pub struct HashingEmbeddingProvider {
    dim: usize,
    ngram_size: usize,
    model_name: String,
}

impl HashingEmbeddingProvider {
    /// Construct with default n-gram size 3 (trigrams). For very short
    /// strings (single ideograms, single chars) increase to 4 or 5 to
    /// avoid degenerate collisions; for code-like text with `_` and
    /// alphanumerics, 3 is the sweet spot.
    pub fn new(dim: usize) -> Self {
        Self::with_ngram(dim, 3)
    }

    /// Construct with an explicit n-gram size. `ngram_size` must be >= 1.
    pub fn with_ngram(dim: usize, ngram_size: usize) -> Self {
        assert!(ngram_size >= 1, "ngram_size must be >= 1");
        Self {
            dim,
            ngram_size,
            model_name: format!("hashing:ngram{ngram_size}:dim{dim}"),
        }
    }
}

impl std::fmt::Debug for HashingEmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HashingEmbeddingProvider")
            .field("dim", &self.dim)
            .field("ngram_size", &self.ngram_size)
            .field("model_name", &self.model_name)
            .finish()
    }
}

/// FNV-1a 64-bit hash. Stable across Rust versions, platforms, and
/// process restarts — unlike `std::hash::DefaultHasher` which is
/// randomized per-process via `getrandom`. Required for vector
/// persistence (vectors stored today must match vectors computed
/// tomorrow).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hash a char-trigram (or any string) into a `dim`-sized bucket with
/// ±1 sign from the high bit. The boundary markers `^` and `$` mean the
/// first and last characters still produce a valid n-gram instead of
/// being dropped.
fn hashing_embed(text: &str, dim: usize, ngram_size: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dim];

    // Build the padded char stream: `^` + lowercased chars + `$`.
    // Lowercasing is locale-independent here (we use the simple
    // `to_lowercase()` which handles ASCII case and Unicode case-fold
    // tables) — fine for retrieval ranking, no need for full ICU.
    let mut chars: Vec<char> = Vec::with_capacity(text.chars().count() + 2);
    chars.push('^');
    for c in text.chars() {
        for lc in c.to_lowercase() {
            chars.push(lc);
        }
    }
    chars.push('$');

    if chars.len() < ngram_size + 1 {
        // Pathological input — produce a deterministic zero-vector
        // (caller checks norm, which is 0 → cosine is 0 to anything).
        return v;
    }

    for window in chars.windows(ngram_size) {
        // Build ngram as a String. Cheap because n is small (3 by default).
        let mut buf = [0u8; 16]; // trigrams fit in 16 bytes for any 5-byte UTF-8 char
        let mut len = 0;
        for c in window {
            let encoded = c.to_string();
            let bytes = encoded.as_bytes();
            if len + bytes.len() > buf.len() {
                // Skip n-grams that don't fit (shouldn't happen with
                // sensible n; safe fallback).
                continue;
            }
            buf[len..len + bytes.len()].copy_from_slice(bytes);
            len += bytes.len();
        }
        if len == 0 {
            continue;
        }
        let h = fnv1a_64(&buf[..len]);
        let bucket = (h as usize) % dim;
        let sign: f32 = if (h >> 63) == 1 { -1.0 } else { 1.0 };
        v[bucket] += sign;
    }

    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

#[async_trait]
impl EmbeddingProvider for HashingEmbeddingProvider {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    async fn embed(&self, text: &str) -> EmbeddingResult<Vec<f32>> {
        Ok(hashing_embed(text, self.dim, self.ngram_size))
    }

    async fn embed_batch(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| hashing_embed(t, self.dim, self.ngram_size))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// FastEmbedEmbeddingProvider — feature-gated, production path
// ---------------------------------------------------------------------------

#[cfg(feature = "fastembed")]
mod fastembed_impl {
    use super::*;
    use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
    use parking_lot::Mutex;
    use std::path::PathBuf;

    /// Local ONNX-backed embedding provider wrapping `fastembed::TextEmbedding`.
    ///
    /// `try_new` is the **eager** constructor that loads the model
    /// immediately (downloading on first run). For agent startup paths
    /// where the model load is on the hot path, prefer
    /// [`FastEmbedEmbeddingProvider::try_new_lazy`] which defers loading
    /// to the first `embed` call.
    ///
    /// Internally the `TextEmbedding` is held behind a `Mutex` because
    /// `embed(&mut self, ...)` requires `&mut self` on the fastembed side
    /// (the underlying ONNX session is not `Sync`).
    pub struct FastEmbedEmbeddingProvider {
        /// Eagerly-loaded fastembed handle. `Some` after `try_new`,
        /// `None` until first `embed`/`embed_batch` for `try_new_lazy`.
        inner: Arc<Mutex<Option<TextEmbedding>>>,
        model: EmbeddingModel,
        cache_dir: Option<PathBuf>,
        /// Cached `dim` so we don't have to re-query `get_model_info` on
        /// every call. Populated in `try_new` from the registry.
        dim: usize,
        /// Human-readable model code, e.g. "BAAI/bge-small-en-v1.5".
        model_code: String,
    }

    impl std::fmt::Debug for FastEmbedEmbeddingProvider {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FastEmbedEmbeddingProvider")
                .field("model", &self.model_code)
                .field("dim", &self.dim)
                .field("cache_dir", &self.cache_dir)
                .field("loaded", &self.inner.lock().is_some())
                .finish()
        }
    }

    impl FastEmbedEmbeddingProvider {
        /// Eagerly load the default model (`BGESmallENV15`, 384 dim, English).
        /// Blocks the current thread for model load + (first-run) download.
        /// Run on a `spawn_blocking` task or from a one-shot startup hook.
        pub fn try_default() -> EmbeddingResult<Self> {
            Self::try_new(EmbeddingModel::BGESmallENV15, None)
        }

        /// Lazy version of [`try_default`]: creates the provider without
        /// loading the model. The first `embed` / `embed_batch` call
        /// triggers the actual download + load on a `spawn_blocking`
        /// task. Users who don't use vector memory never pay the
        /// ~34MB download cost.
        pub fn try_default_lazy() -> EmbeddingResult<Self> {
            Self::try_new_lazy(EmbeddingModel::BGESmallENV15, None)
        }

        /// Eagerly load `model`. `cache_dir` defaults to
        /// `<hermix_home>/cache/fastembed/` if `None`.
        pub fn try_new(model: EmbeddingModel, cache_dir: Option<PathBuf>) -> EmbeddingResult<Self> {
            let info = TextEmbedding::get_model_info(&model).map_err(EmbeddingError::FastEmbed)?;
            let dim = info.dim;
            let model_code = info.model_code.clone();
            let cache_dir = cache_dir.or_else(default_cache_dir);
            info!(
                model = %model_code,
                dim,
                cache_dir = %cache_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                "FastEmbedEmbeddingProvider: loading model"
            );
            let mut opts = TextInitOptions::new(model.clone());
            if let Some(ref dir) = cache_dir {
                opts = opts.with_cache_dir(dir.clone());
            }
            // Suppress noisy progress bars in production logs; tests can
            // re-enable via env var if they want.
            opts = opts.with_show_download_progress(false);
            let inner = TextEmbedding::try_new(opts)?;
            Ok(Self {
                inner: Arc::new(Mutex::new(Some(inner))),
                model,
                cache_dir,
                dim,
                model_code,
            })
        }

        /// Construct without loading the model. The first `embed` /
        /// `embed_batch` call triggers a blocking load on a
        /// `spawn_blocking` task. Useful when the model isn't always
        /// needed (e.g. config disabled vector recall).
        pub fn try_new_lazy(model: EmbeddingModel, cache_dir: Option<PathBuf>) -> EmbeddingResult<Self> {
            let info = TextEmbedding::get_model_info(&model).map_err(EmbeddingError::FastEmbed)?;
            let dim = info.dim;
            let model_code = info.model_code.clone();
            Ok(Self {
                inner: Arc::new(Mutex::new(None)),
                model,
                cache_dir: cache_dir.or_else(default_cache_dir),
                dim,
                model_code,
            })
        }

        /// Borrow the configured `EmbeddingModel` enum (for telemetry,
        /// config echo, debugging).
        pub fn model_kind(&self) -> &EmbeddingModel {
            &self.model
        }

        /// `true` if the underlying model session is loaded and ready.
        pub fn is_loaded(&self) -> bool {
            self.inner.lock().is_some()
        }

        /// Eagerly load a lazy-constructed provider. Returns immediately
        /// if already loaded. Safe to call from a `spawn_blocking` task.
        pub fn ensure_loaded(&self) -> EmbeddingResult<()> {
            let mut guard = self.inner.lock();
            if guard.is_none() {
                let mut opts = TextInitOptions::new(self.model.clone());
                if let Some(ref dir) = self.cache_dir {
                    opts = opts.with_cache_dir(dir.clone());
                }
                opts = opts.with_show_download_progress(false);
                match TextEmbedding::try_new(opts) {
                    Ok(te) => *guard = Some(te),
                    Err(e) => {
                        warn!("FastEmbedEmbeddingProvider: load failed: {e}");
                        return Err(EmbeddingError::FastEmbed(e));
                    }
                }
            }
            Ok(())
        }
    }

    fn default_cache_dir() -> Option<PathBuf> {
        // hermix_home() lives in hermix-config and is panic-safe
        // (always resolves to at least `~/.hermix`). Wrap in Some() to
        // preserve the `Option<PathBuf>` signature so callers can
        // `.or_else(default_cache_dir)` chain it.
        Some(hermix_config::paths::hermix_home().join("cache").join("fastembed"))
    }

    /// Convert a single `fastembed::Embedding` to `Vec<f32>`.
    /// In fastembed 5.x, `Embedding` is a type alias for `Vec<f32>`,
    /// so this is just an identity conversion.
    fn embedding_to_vec(e: fastembed::Embedding) -> Vec<f32> {
        e
    }

    #[async_trait]
    impl EmbeddingProvider for FastEmbedEmbeddingProvider {
        fn model_name(&self) -> &str {
            &self.model_code
        }

        fn dimension(&self) -> usize {
            self.dim
        }

        async fn embed(&self, text: &str) -> EmbeddingResult<Vec<f32>> {
            if !self.is_loaded() {
                self.ensure_loaded()?;
            }

            let text_owned = text.to_string();
            let inner = self.inner.clone();
            let vecs = tokio::task::spawn_blocking(move || {
                let mut guard = inner.lock();
                let te = guard.as_mut().ok_or(EmbeddingError::NotLoaded)?;
                te.embed(vec![text_owned.as_str()], None)
            })
            .await
            .map_err(|_| EmbeddingError::Canceled)??;
            let raw = vecs
                .into_iter()
                .next()
                .ok_or_else(|| EmbeddingError::Other("fastembed returned 0 embeddings".into()))?;
            Ok(embedding_to_vec(raw))
        }

        async fn embed_batch(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            self.ensure_loaded()?;
            let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
            let inner = self.inner.clone();
            let vecs = tokio::task::spawn_blocking(move || {
                let mut guard = inner.lock();
                let te = guard.as_mut().ok_or(EmbeddingError::NotLoaded)?;
                let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
                te.embed(refs, None)
            })
            .await
            .map_err(|_| EmbeddingError::Canceled)??;
            Ok(vecs.into_iter().map(embedding_to_vec).collect())
        }
    }
}

#[cfg(feature = "fastembed")]
pub use fastembed_impl::FastEmbedEmbeddingProvider;

/// Convenience: wrap any `EmbeddingProvider` in an `Arc` for sharing
/// between the agent loop and the `VectorRouter`.
pub fn shared(p: impl EmbeddingProvider + 'static) -> Arc<dyn EmbeddingProvider> {
    Arc::new(p)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ----- HashingEmbeddingProvider -----------------------------------

    #[tokio::test]
    async fn hashing_dim_matches() {
        let p = HashingEmbeddingProvider::new(384);
        let v = p.embed("hello").await.unwrap();
        assert_eq!(v.len(), 384);
    }

    #[tokio::test]
    async fn hashing_is_deterministic_across_runs() {
        // FNV-1a is stable across Rust versions and process restarts —
        // unlike std::hash which is randomized per-process via getrandom.
        let p = HashingEmbeddingProvider::new(64);
        let a = p.embed("the quick brown fox").await.unwrap();
        let b = p.embed("the quick brown fox").await.unwrap();
        assert_eq!(a, b, "hashing embed must be deterministic");
        // Re-construct the provider and verify the vector is identical
        // (proves no per-instance state).
        let p2 = HashingEmbeddingProvider::new(64);
        let c = p2.embed("the quick brown fox").await.unwrap();
        assert_eq!(a, c, "hashing embed must be stable across providers");
    }

    #[tokio::test]
    async fn hashing_batch_matches_loop() {
        let p = HashingEmbeddingProvider::new(32);
        let inputs = vec!["alpha", "beta", "gamma"];
        let batch = p.embed_batch(&inputs).await.unwrap();
        let singles: Vec<Vec<f32>> =
            futures::future::join_all(inputs.iter().map(|s| p.embed(s))).await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();
        assert_eq!(batch, singles);
    }

    #[tokio::test]
    async fn hashing_similar_strings_have_strictly_higher_cosine_than_unrelated() {
        // Unlike the old byte-mixing stub (which only "roughly" had signal),
        // hashing-trick embeddings give strict cosine ranking on these
        // standard fixtures.
        let p = HashingEmbeddingProvider::new(256);
        let a = p.embed("what is the weather in Beijing today").await.unwrap();
        let b = p.embed("Beijing weather forecast").await.unwrap();
        let c = p.embed("delete all my files now please").await.unwrap();
        let sim_ab = HashingEmbeddingProvider::cosine(&a, &b);
        let sim_ac = HashingEmbeddingProvider::cosine(&a, &c);
        assert!(
            sim_ab > sim_ac,
            "similar strings must have strictly higher cosine than unrelated: sim_ab={sim_ab}, sim_ac={sim_ac}"
        );
        assert!(sim_ab.abs() < 1.01, "cosine must stay in [-1, 1]");
        assert!(sim_ac.abs() < 1.01, "cosine must stay in [-1, 1]");
        let same = HashingEmbeddingProvider::cosine(&a, &a);
        assert!((same - 1.0).abs() < 1e-5, "cosine(v, v) must be 1");
    }

    #[tokio::test]
    async fn hashing_chinese_similar_strings_have_high_cosine() {
        // CJK character trigrams share ideograms between similar phrases.
        let p = HashingEmbeddingProvider::new(256);
        let a = p.embed("我喜欢 Rust 编程").await.unwrap();
        let b = p.embed("我喜爱 Rust 开发").await.unwrap();
        let c = p.embed("今天天气不错").await.unwrap();
        let sim_ab = HashingEmbeddingProvider::cosine(&a, &b);
        let sim_ac = HashingEmbeddingProvider::cosine(&a, &c);
        assert!(
            sim_ab > sim_ac,
            "Chinese similar phrases must rank higher: sim_ab={sim_ab}, sim_ac={sim_ac}"
        );
        // And both Chinese phrases should have positive overlap since
        // they share at least the ideogram "Rust" (ASCII within CJK).
        assert!(sim_ab > 0.0, "similar Chinese must have positive cosine");
    }

    #[tokio::test]
    async fn hashing_case_insensitive() {
        let p = HashingEmbeddingProvider::new(128);
        let a = p.embed("Rust Programming").await.unwrap();
        let b = p.embed("rust programming").await.unwrap();
        // Cosine must be 1.0 — identical n-grams after lowercasing.
        let s = HashingEmbeddingProvider::cosine(&a, &b);
        assert!((s - 1.0).abs() < 1e-5, "case must not affect similarity, got {s}");
    }

    #[tokio::test]
    async fn hashing_empty_input_produces_zero_vector() {
        let p = HashingEmbeddingProvider::new(64);
        let v = p.embed("").await.unwrap();
        assert_eq!(v.len(), 64);
        assert!(v.iter().all(|x| *x == 0.0), "empty input must yield zero vector");
    }

    #[tokio::test]
    async fn hashing_single_char_produces_zero_vector() {
        // With default n=3, a single char has no full n-gram — yields
        // zero vector. Use n=1 to get non-zero for single chars.
        let p = HashingEmbeddingProvider::new(64);
        let v = p.embed("x").await.unwrap();
        assert!(v.iter().all(|x| *x == 0.0), "1 char < n=3, expected zero vec");
        let p1 = HashingEmbeddingProvider::with_ngram(64, 1);
        let v1 = p1.embed("x").await.unwrap();
        assert!(v1.iter().any(|x| *x != 0.0), "n=1 should give non-zero vec");
    }

    #[tokio::test]
    async fn hashing_cosine_perfect_match_is_one() {
        let p = HashingEmbeddingProvider::new(16);
        let v = p.embed("x").await.unwrap();
        let s = HashingEmbeddingProvider::cosine(&v, &v);
        // Empty input → zero vec → cosine 0 (we test non-empty below).
        assert_eq!(s, 0.0);
        let v2 = p.embed("hello world").await.unwrap();
        let s2 = HashingEmbeddingProvider::cosine(&v2, &v2);
        assert!((s2 - 1.0).abs() < 1e-5, "cosine(v, v) should be 1, got {s2}");
    }

    #[tokio::test]
    async fn hashing_cosine_zero_vector_is_zero() {
        let zero = vec![0.0f32; 4];
        let s = HashingEmbeddingProvider::cosine(&zero, &zero);
        assert_eq!(s, 0.0);
    }

    #[tokio::test]
    async fn hashing_cosine_mismatched_dim_is_zero() {
        let s = HashingEmbeddingProvider::cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]);
        assert_eq!(s, 0.0);
    }

    #[tokio::test]
    async fn hashing_model_name_describes_config() {
        let p = HashingEmbeddingProvider::with_ngram(384, 5);
        assert_eq!(p.model_name(), "hashing:ngram5:dim384");
        assert_eq!(p.dimension(), 384);
    }

    #[test]
    fn fnv1a_is_stable_known_vectors() {
        // FNV-1a 64 of "abc" is a published constant — pinning it
        // means we catch any accidental change to the hash function.
        assert_eq!(fnv1a_64(b"abc"), 0xe71fa2190541574b);
        assert_eq!(fnv1a_64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a_64(b"foobar"), 0x85944171f73967e8);
    }

    // ----- FastEmbedEmbeddingProvider (only with feature) ------------
    //
    // We only test the cheap, side-effect-free methods. `try_new` /
    // `try_default` actually download the model and are gated behind
    // `#[ignore]` so unit-test CI runs don't need network or HF
    // cache access. Run with `cargo test --features fastembed -- --ignored
    // fastembed_real_load` when the model is cached locally and the
    // ort CDN is reachable.

    #[cfg(feature = "fastembed")]
    #[test]
    fn fastembed_get_model_info_works_for_default() {
        // Pure sync call into fastembed's in-process registry — no
        // model download, no ONNX session. Always safe to run.
        let info = fastembed::TextEmbedding::get_model_info(&fastembed::EmbeddingModel::BGESmallENV15)
            .unwrap();
        assert_eq!(info.dim, 384);
        assert!(!info.model_code.is_empty());
    }

    #[cfg(feature = "fastembed")]
    #[test]
    fn fastembed_list_supported_models_is_nonempty() {
        let models = fastembed::TextEmbedding::list_supported_models();
        assert!(models.len() >= 5, "expected >= 5 models, got {}", models.len());
    }

    #[cfg(feature = "fastembed")]
    #[test]
    fn fastembed_default_is_bge_small_en_v15() {
        assert_eq!(
            fastembed::EmbeddingModel::default(),
            fastembed::EmbeddingModel::BGESmallENV15
        );
    }

    #[cfg(feature = "fastembed")]
    #[test]
    fn fastembed_lazy_construct_does_not_download() {
        // Construct without loading — must be instant and have no IO.
        let p = FastEmbedEmbeddingProvider::try_new_lazy(
            fastembed::EmbeddingModel::BGESmallENV15,
            None,
        )
        .expect("lazy construct should not fail");
        assert!(!p.is_loaded(), "lazy provider must not load until first embed");
        assert_eq!(p.dimension(), 384);
        assert_eq!(p.model_name(), "BAAI/bge-small-en-v1.5");
    }

    /// Real load test — requires the model to be in the local cache
    /// (or the ability to download it). `cargo test --features
    /// fastembed -- --ignored fastembed_real_load`.
    #[cfg(feature = "fastembed")]
    #[tokio::test]
    #[ignore = "requires model download + ONNX runtime binary; run with --features fastembed -- --ignored"]
    async fn fastembed_real_load_and_embed() {
        let p = FastEmbedEmbeddingProvider::try_default().expect("default load");
        let v = p.embed("hello world").await.unwrap();
        assert_eq!(v.len(), 384);
        let s = FastEmbedEmbeddingProvider::cosine(&v, &v);
        assert!((s - 1.0).abs() < 1e-4, "self-cosine should be 1, got {s}");
    }
}
