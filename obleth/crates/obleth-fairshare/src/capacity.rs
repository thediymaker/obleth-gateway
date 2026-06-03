//! Capacity providers decide how many requests may be in flight concurrently.
//!
//! v1 ships [`StaticCapacity`] (a runtime-tunable fixed limit). The trait is the
//! seam the plan calls out: a future `MetricsCapacity` can read vLLM/Aibrix queue
//! depth or KV-cache utilization, or an `SloCapacity` can react to TTFT, without
//! any change to the scheduler.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Source of the global in-flight concurrency budget.
pub trait CapacityProvider: Send + Sync + 'static {
    /// Maximum number of requests allowed in flight right now.
    fn max_in_flight(&self) -> usize;
}

/// A fixed concurrency limit that can be retuned at runtime (e.g. by an admin
/// endpoint) via [`StaticCapacity::set`].
#[derive(Debug)]
pub struct StaticCapacity {
    max: AtomicUsize,
}

impl StaticCapacity {
    pub fn new(max: usize) -> Self {
        StaticCapacity {
            max: AtomicUsize::new(max.max(1)),
        }
    }

    pub fn set(&self, max: usize) {
        self.max.store(max.max(1), Ordering::Relaxed);
    }
}

impl CapacityProvider for StaticCapacity {
    fn max_in_flight(&self) -> usize {
        self.max.load(Ordering::Relaxed)
    }
}
