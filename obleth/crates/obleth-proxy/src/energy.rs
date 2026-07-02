//! Per-request energy & carbon accounting.
//!
//! A background poller reads total cluster power (watts) and live node count
//! from the operator's Prometheus via an operator-supplied PromQL expression.
//! At request completion the proxy charges the request one "sequence slot"
//! share of node power for its serving time and freezes Wh / USD / gCO2 into
//! the usage ledger. Everything here is fail-open: missing settings, a dead
//! Prometheus, or zero divisors record zero energy and never affect requests.

use std::sync::Arc;

use arc_swap::ArcSwap;
use obleth_admin::energy_probe::instant_query;
use obleth_config::EnergySettings;

/// One Prometheus poll result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerReading {
    pub cluster_watts: f64,
    pub node_count: u64,
    pub at_ms: i64,
}

/// Frozen per-request energy figures (zeros when accounting is off).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EnergyFigures {
    pub energy_wh: f64,
    pub energy_cost_usd: f64,
    pub co2_g: f64,
}

/// Hot-swappable settings + latest power reading. Cheap to clone.
#[derive(Clone)]
pub struct EnergyEngine {
    settings: Arc<ArcSwap<EnergySettings>>,
    reading: Arc<ArcSwap<Option<PowerReading>>>,
}

impl EnergyEngine {
    pub fn new(initial: EnergySettings) -> Self {
        EnergyEngine {
            settings: Arc::new(ArcSwap::from_pointee(initial)),
            reading: Arc::new(ArcSwap::from_pointee(None)),
        }
    }

    pub fn settings(&self) -> Arc<EnergySettings> {
        self.settings.load_full()
    }

    pub fn update(&self, settings: EnergySettings) {
        self.settings.store(Arc::new(settings));
    }

    pub fn store_reading(&self, r: PowerReading) {
        self.reading.store(Arc::new(Some(r)));
    }

    pub fn reading(&self) -> Option<PowerReading> {
        **self.reading.load()
    }

    /// Slot-share energy for one request. Zeros when disabled, the model
    /// declares no slots, no reading has arrived, or divisors are zero.
    pub fn compute(
        &self,
        energy_slots_per_node: i64,
        total_ms: u32,
        queue_wait_ms: u32,
    ) -> EnergyFigures {
        let s = self.settings.load();
        if !s.enabled || energy_slots_per_node <= 0 {
            return EnergyFigures::default();
        }
        let Some(r) = self.reading() else {
            return EnergyFigures::default();
        };
        if r.node_count == 0 || r.cluster_watts <= 0.0 {
            return EnergyFigures::default();
        }
        let serving_ms = total_ms.saturating_sub(queue_wait_ms) as f64;
        let watts_per_slot =
            r.cluster_watts / r.node_count as f64 / energy_slots_per_node as f64 * s.pue;
        let energy_wh = watts_per_slot * serving_ms / 3_600_000.0;
        EnergyFigures {
            energy_wh,
            energy_cost_usd: energy_wh / 1000.0 * s.energy_cost_per_kwh,
            co2_g: energy_wh / 1000.0 * s.carbon_g_per_kwh,
        }
    }
}

/// Poll `sum()`/`count()` of the configured power query. Always spawned; while
/// the feature is inactive it re-checks settings every 15s so enabling from
/// the settings tab takes effect without a restart. Failures keep the last
/// reading and raise a deduped alert.
pub fn spawn_energy_poller(
    engine: EnergyEngine,
    http: reqwest::Client,
    alerts: obleth_admin::AlertDispatcher,
) {
    tokio::spawn(async move {
        loop {
            let settings = engine.settings();
            if !settings.active() {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                continue;
            }
            let base = settings.prometheus_url.clone();
            let q = settings.power_query.trim();
            let sum = instant_query(&http, &base, &format!("sum({q})")).await;
            let count = instant_query(&http, &base, &format!("count({q})")).await;
            match (sum, count) {
                (Ok(watts), Ok(nodes)) => {
                    engine.store_reading(PowerReading {
                        cluster_watts: watts,
                        node_count: nodes.max(0.0) as u64,
                        at_ms: now_ms(),
                    });
                }
                (Err(e), _) | (_, Err(e)) => {
                    tracing::warn!(error = %e, "energy power poll failed");
                    alerts.issue(
                        "energy_power_poll_failed",
                        "Energy power poll failed",
                        format!(
                            "Prometheus `{base}` query `{q}`: {e}. Requests record \
                             energy from the last good reading (or zero if none)."
                        ),
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(
                settings.poll_interval_secs.max(5),
            ))
            .await;
        }
    });
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with(reading: Option<PowerReading>, enabled: bool) -> EnergyEngine {
        let e = EnergyEngine::new(obleth_config::EnergySettings {
            enabled,
            prometheus_url: "http://prom".into(),
            power_query: "watts".into(),
            poll_interval_secs: 60,
            energy_cost_per_kwh: 0.10,
            carbon_g_per_kwh: 400.0,
            pue: 1.0,
        });
        if let Some(r) = reading {
            e.store_reading(r);
        }
        e
    }

    fn reading_409kw_178n() -> PowerReading {
        PowerReading {
            cluster_watts: 409_000.0,
            node_count: 178,
            at_ms: 0,
        }
    }

    #[test]
    fn computes_slot_share_energy() {
        let e = engine_with(Some(reading_409kw_178n()), true);
        // 409000/178/8 = 287.219... W per slot; 1h serving => same in Wh.
        let f = e.compute(8, 3_600_000, 0);
        assert!((f.energy_wh - 287.219).abs() < 0.01);
        assert!((f.energy_cost_usd - 287.219 / 1000.0 * 0.10).abs() < 1e-6);
        assert!((f.co2_g - 287.219 / 1000.0 * 400.0).abs() < 0.01);
    }

    #[test]
    fn excludes_queue_wait() {
        let e = engine_with(Some(reading_409kw_178n()), true);
        let full = e.compute(8, 3_600_000, 0);
        let half = e.compute(8, 3_600_000, 1_800_000);
        assert!((half.energy_wh - full.energy_wh / 2.0).abs() < 1e-6);
        // queue wait longer than total saturates to zero, never negative
        let z = e.compute(8, 1_000, 2_000);
        assert_eq!(z.energy_wh, 0.0);
    }

    #[test]
    fn zero_guards_yield_zero() {
        // disabled
        assert_eq!(
            engine_with(Some(reading_409kw_178n()), false)
                .compute(8, 1000, 0)
                .energy_wh,
            0.0
        );
        // no slots declared
        assert_eq!(
            engine_with(Some(reading_409kw_178n()), true)
                .compute(0, 1000, 0)
                .energy_wh,
            0.0
        );
        // no reading yet
        assert_eq!(engine_with(None, true).compute(8, 1000, 0).energy_wh, 0.0);
        // zero node count
        let r = PowerReading {
            cluster_watts: 1000.0,
            node_count: 0,
            at_ms: 0,
        };
        assert_eq!(
            engine_with(Some(r), true).compute(8, 1000, 0).energy_wh,
            0.0
        );
    }

    #[test]
    fn pue_multiplies() {
        let mut s = engine_with(Some(reading_409kw_178n()), true)
            .settings()
            .as_ref()
            .clone();
        s.pue = 1.5;
        let e = EnergyEngine::new(s);
        e.store_reading(reading_409kw_178n());
        let f = e.compute(8, 3_600_000, 0);
        assert!((f.energy_wh - 287.219 * 1.5).abs() < 0.01);
    }
}
