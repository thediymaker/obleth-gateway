//! `auto` model selection: the data plane's view of the router.
//!
//! The selection core itself — filters, tier floor, scoring, the explanation
//! shape — lives in [`obleth_config::routing`] so the Management API can run the
//! identical pipeline for the routing tuner without depending on this binary
//! crate. It is re-exported wholesale here, so every data-plane call site keeps
//! naming `crate::router::…` exactly as before.
//!
//! What stays here is the part that is runtime rather than pure: the
//! hot-swappable [`ModelRegistry`] the request path reads its candidate snapshot
//! from, and [`splitmix_uniform`], the clock-seeded draw that feeds softmax
//! sampling. Neither is a routing decision; both would drag `arc_swap` and a
//! clock dependency into a crate that is meant to hold plain config types.

use std::sync::Arc;

use arc_swap::ArcSwap;

pub use obleth_config::routing::*;

/// Lock-free, hot-swappable list of auto-routing candidates. Reads clone an
/// `Arc` to the current snapshot; refreshes atomically replace the whole list.
#[derive(Clone)]
pub struct ModelRegistry {
    inner: Arc<ArcSwap<Vec<Candidate>>>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(Vec::new())),
        }
    }

    /// Current candidate snapshot. Cheap; clones a single `Arc`.
    pub fn load(&self) -> Arc<Vec<Candidate>> {
        self.inner.load_full()
    }

    /// Atomically replace the candidate list (used on boot and on refresh).
    pub fn store(&self, candidates: Vec<Candidate>) {
        self.inner.store(Arc::new(candidates));
    }
}

/// One uniform draw in `[0,1)`. Dependency-free splitmix64 seeded from the
/// clock, matching `weighted_order` in proxy.rs — obleth-proxy deliberately
/// carries no `rand` dependency.
pub fn splitmix_uniform() -> f64 {
    let mut z = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((z ^ (z >> 31)) as f64 / u64::MAX as f64).clamp(0.0, 0.999_999_999)
}
