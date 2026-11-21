//! Settings every gateway replica applies from memory: the alert targets and
//! the global in-flight ceiling. Both are saved in Postgres. The replica that
//! saves one applies it at once and tells the others on the invalidation
//! channel (`settings:<name>`); every replica also re-reads them on the
//! registry tick, in case a message was missed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use obleth_fairshare::{CapacityProvider, StaticCapacity};
use obleth_redis::SavedSetting;
use obleth_store::Store;
use serde::Serialize;
use utoipa::ToSchema;

use crate::AlertDispatcher;

/// Where the global ceiling in effect comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CeilingSource {
    /// Saved with `PUT /api/v1/capacity`; it wins over the environment.
    Saved,
    /// `OBLETH_GLOBAL_MAX_IN_FLIGHT`, as nothing is saved.
    Environment,
}

/// The global in-flight ceiling the scheduler reads, and where it came from.
pub struct GlobalCeiling {
    capacity: Arc<StaticCapacity>,
    environment: usize,
    saved: AtomicBool,
}

impl GlobalCeiling {
    /// A ceiling at `environment` (`OBLETH_GLOBAL_MAX_IN_FLIGHT`) until a
    /// saved value is applied.
    pub fn new(capacity: Arc<StaticCapacity>, environment: usize) -> Self {
        capacity.set(environment);
        GlobalCeiling {
            capacity,
            environment: environment.max(1),
            saved: AtomicBool::new(false),
        }
    }

    pub fn max_in_flight(&self) -> usize {
        self.capacity.max_in_flight()
    }

    /// `OBLETH_GLOBAL_MAX_IN_FLIGHT`, which applies while nothing is saved.
    pub fn environment(&self) -> usize {
        self.environment
    }

    pub fn source(&self) -> CeilingSource {
        if self.saved.load(Ordering::Relaxed) {
            CeilingSource::Saved
        } else {
            CeilingSource::Environment
        }
    }

    /// Apply the saved ceiling, or with `None` the environment's.
    pub fn apply(&self, saved: Option<usize>) {
        let max = saved.unwrap_or(self.environment);
        if max != self.capacity.max_in_flight() {
            tracing::info!(
                max_in_flight = max,
                saved = saved.is_some(),
                "global ceiling set"
            );
        }
        self.capacity.set(max);
        self.saved.store(saved.is_some(), Ordering::Relaxed);
    }
}

/// Re-reads the replica-wide settings from Postgres into this replica.
#[derive(Clone)]
pub struct ReplicaSettings {
    store: Store,
    alerts: AlertDispatcher,
    ceiling: Arc<GlobalCeiling>,
}

impl ReplicaSettings {
    pub fn new(store: Store, alerts: AlertDispatcher, ceiling: Arc<GlobalCeiling>) -> Self {
        ReplicaSettings {
            store,
            alerts,
            ceiling,
        }
    }

    /// Re-read one setting. A failed read is logged and leaves it as it is;
    /// unset alert settings leave the ones this replica started with.
    pub async fn reload(&self, which: SavedSetting) {
        match which {
            SavedSetting::Alerts => match self.store.get_alert_settings().await {
                Ok(Some(settings)) => {
                    let enabled = settings.any_channel_enabled();
                    if self.alerts.update(settings) {
                        tracing::info!(enabled, "alert settings reloaded");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "alert settings refresh failed"),
            },
            SavedSetting::Capacity => match self.store.get_global_ceiling().await {
                Ok(saved) => self.ceiling.apply(saved),
                Err(e) => tracing::warn!(error = %e, "global ceiling refresh failed"),
            },
        }
    }

    /// Re-read every replica-wide setting.
    pub async fn reload_all(&self) {
        for which in [SavedSetting::Alerts, SavedSetting::Capacity] {
            self.reload(which).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_ceiling_wins_and_clearing_it_returns_to_the_environment() {
        let capacity = Arc::new(StaticCapacity::new(1));
        let ceiling = GlobalCeiling::new(capacity.clone(), 4096);
        assert_eq!(
            (capacity.max_in_flight(), ceiling.source()),
            (4096, CeilingSource::Environment)
        );
        ceiling.apply(Some(512));
        assert_eq!(
            (capacity.max_in_flight(), ceiling.source()),
            (512, CeilingSource::Saved)
        );
        ceiling.apply(None);
        assert_eq!(
            (capacity.max_in_flight(), ceiling.source()),
            (4096, CeilingSource::Environment)
        );
    }
}
