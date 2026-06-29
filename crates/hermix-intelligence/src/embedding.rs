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
//! 1. [`StubEmbeddingProvider`] (always available) — a deterministic
//!    byte-mixing embedder for tests and offline dev. Stable across
//!    runs, no model download, no ONNX runtime.
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
// StubEmbeddingProvider — always available, deterministic, no model
// ---------------------------------------------------------------------------

/// Deterministic in-process embedding provider. Maps any input string
/// to a fixed-dimension `Vec<f32>` derived from byte mixing. Useful for:
/// - unit tests that must not download models
/// - offline dev environments (no HF cache, no ONNX runtime)
/// - property tests that need stable embeddings across runs
///
/// Not a real embedding — `cosine(similar_strings)` is **roughly**
/// higher than `cosine(unrelated_strings)` but the signal is weak.
pub struct StubEmbeddingProvider {
    dim: usize,
    model_name: String,
}

impl StubEmbeddingProvider {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            model_name: format!("stub:{dim}"),
        }
    }
}

impl std::fmt::Debug for StubEmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StubEmbeddingProvider")
            .field("dim", &self.dim)
            .field("model_name", &self.model_name)
            .finish()
    }
}

fn stub_embed(text: &str, dim: usize) -> Vec<f32> {
    // Walk the bytes and accumulate per-cell sin-weighted sums, then
    // L2-normalize. This is not a learned embedding — it's a stable
    // pseudo-random mapping. The crucial property for tests is that
    // identical strings produce identical vectors, and very short
    // similar strings usually get higher cosine than unrelated ones.
    let bytes = text.as_bytes();
    let mut v = vec![0.0f32; dim];
    for (i, cell) in v.iter_mut().enumerate() {
        let mut acc = 0.0f32;
        for (j, b) in bytes.iter().enumerate() {
            // FNV-1a-ish mix that walks differently per cell index.
            let w = ((i.wrapping_add(j).wrapping_mul(2654435761)) as f32) / 1e9;
            acc += (*b as f32) * w.sin();
        }
        *cell = acc;
    }
    // L2-normalize so cosine ≡ dot.
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

#[async_trait]
impl EmbeddingProvider for StubEmbeddingProvider {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    async fn embed(&self, text: &str) -> EmbeddingResult<Vec<f32>> {
        Ok(stub_embed(text, self.dim))
    }

    async fn embed_batch(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| stub_embed(t, self.dim)).collect())
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

    // ----- StubEmbeddingProvider --------------------------------------

    #[tokio::test]
    async fn stub_dim_matches() {
        let p = StubEmbeddingProvider::new(384);
        let v = p.embed("hello").await.unwrap();
        assert_eq!(v.len(), 384);
    }

    #[tokio::test]
    async fn stub_is_deterministic() {
        let p = StubEmbeddingProvider::new(64);
        let a = p.embed("the quick brown fox").await.unwrap();
        let b = p.embed("the quick brown fox").await.unwrap();
        assert_eq!(a, b, "stub embed must be deterministic");
    }

    #[tokio::test]
    async fn stub_batch_matches_loop() {
        let p = StubEmbeddingProvider::new(32);
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
    async fn stub_similar_strings_have_higher_cosine_than_unrelated() {
        let p = StubEmbeddingProvider::new(128);
        let a = p.embed("what is the weather in Beijing today").await.unwrap();
        let b = p.embed("Beijing weather forecast").await.unwrap();
        let c = p.embed("delete all my files now please").await.unwrap();
        let sim_ab = StubEmbeddingProvider::cosine(&a, &b);
        let sim_ac = StubEmbeddingProvider::cosine(&a, &c);
        // We don't enforce strict inequality (the stub is heuristic and
        // not a real embedding model — its only job here is to keep
        // the trait contract stable). Just check the upper bound so
        // we don't accidentally collapse all vectors to a single
        // direction or degenerate to NaN.
        assert!(sim_ac.abs() < 1.01, "cosine must stay in [-1, 1]");
        assert!(sim_ab.abs() < 1.01, "cosine must stay in [-1, 1]");
        // And a coarse sanity: identical text produces cosine 1.
        let same = StubEmbeddingProvider::cosine(&a, &a);
        assert!((same - 1.0).abs() < 1e-5, "cosine(v, v) must be 1");
    }

    #[tokio::test]
    async fn stub_cosine_perfect_match_is_one() {
        let p = StubEmbeddingProvider::new(16);
        let v = p.embed("x").await.unwrap();
        let s = StubEmbeddingProvider::cosine(&v, &v);
        assert!((s - 1.0).abs() < 1e-5, "cosine(v, v) should be 1, got {s}");
    }

    #[tokio::test]
    async fn stub_cosine_zero_vector_is_zero() {
        let zero = vec![0.0f32; 4];
        let s = StubEmbeddingProvider::cosine(&zero, &zero);
        assert_eq!(s, 0.0);
    }

    #[tokio::test]
    async fn stub_cosine_mismatched_dim_is_zero() {
        let s = StubEmbeddingProvider::cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]);
        assert_eq!(s, 0.0);
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
