//! Salience reranking helpers (`l2_normalize`, `dot`, `rerank_by_salience`).
use chrono::{DateTime, Utc};

use super::*;

use crate::memory_layer::MemoryHit;
use crate::salience::{salience, SalienceConfig};

impl EmbeddedMemoryBackend {
    pub fn rerank_by_salience(
        &self,
        hits: Vec<MemoryHit>,
        now: DateTime<Utc>,
    ) -> Vec<MemoryHit> {
        let mut hits = hits;
        for hit in &mut hits {
            let touch_count = hit
                .metadata
                .get("touch_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let pin_count = hit
                .metadata
                .get("pin_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let s = salience(hit.created_at, touch_count, pin_count, now, &self.salience_cfg);
            // Combine: take the max of (backend salience, router-side
            // recency/freq/pin salience). The backend is the source of
            // truth for relevance; the router is the source of truth
            // for "this thing is hot right now".
            hit.salience = hit.salience.max(s.total);
        }
        hits.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits
    }

    /// H5: replace the salience config (e.g. for tuning the
    /// recency/freq/pin weights in production).
    pub fn with_salience_cfg(mut self, cfg: SalienceConfig) -> Self {
        self.salience_cfg = cfg;
        self
    }
}

pub(crate) fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let mut norm = 0.0_f32;
    for &x in v {
        norm += x * x;
    }
    let n = norm.sqrt();
    if n == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / n).collect()
}

/// L1.3: dot product of two equal-length f32 vectors. Returns 0.0
/// if the lengths differ — the caller filters by dim upstream
/// so this is just a safety net for corrupt rows.
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut acc = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        acc += x * y;
    }
    acc
}
