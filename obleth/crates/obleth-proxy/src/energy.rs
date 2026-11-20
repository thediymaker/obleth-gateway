//! Per-request energy & carbon accounting.
//!
//! A background poller reads power from the operator's Prometheus with
//! operator-supplied PromQL, in one or both of two forms:
//!
//! - **Measured per model** (`model_power_query`): watts per model. Each
//!   model's power is split across the requests it is serving, counted
//!   across every gateway replica, so the energy charged to a model's
//!   requests adds up to what it drew while busy. Its idle draw is charged
//!   to nobody. When other clients share the servers, an optional query of
//!   the servers' own request counts sets the floor on that split, so the
//!   gateway's requests are not charged for theirs.
//! - **Node slot share** (`power_query`): total cluster power and node count.
//!   A request is charged one of its model's declared `energy_slots_per_node`
//!   shares of an average node for its serving time.
//!
//! A model that matches a measured series uses that; otherwise it falls back
//! to the slot share. Wh / USD / gCO2 are frozen into the usage ledger at
//! completion. Everything here is fail-open: missing settings, a dead
//! Prometheus, or zero divisors record zero energy and never affect requests.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use obleth_admin::energy_probe::{instant_query, instant_vector, match_power_series};
use obleth_config::{EnergySettings, ResolvedModel};

/// How often the cluster-wide in-flight counts behind measured power are
/// refreshed while a metered model has requests running here.
const IN_FLIGHT_SYNC_INTERVAL: Duration = Duration::from_secs(2);

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

/// One model's measured power and the share of it each in-flight request
/// has accrued so far.
///
/// `per_request_wh` is the running integral of `watts / in_flight` over
/// time, where `in_flight` is this replica's requests plus the other
/// replicas' last reported count, or the servers' own count when that is
/// larger. A request's energy is the difference
/// between its value at the start and the end of the request, which splits
/// the model's draw exactly between the requests that overlapped it.
#[derive(Debug)]
struct MeterState {
    watts: f64,
    at: Instant,
    per_request_wh: f64,
    local: usize,
    peers: usize,
    /// Requests the model's servers last reported handling, from every
    /// client; 0 without a requests query.
    backend: usize,
    /// Admission pools whose requests this series serves, for the cluster
    /// in-flight count.
    pools: BTreeSet<String>,
}

#[derive(Debug)]
struct Meter(Mutex<MeterState>);

impl Meter {
    fn new(watts: f64, now: Instant) -> Self {
        Meter(Mutex::new(MeterState {
            watts,
            at: now,
            per_request_wh: 0.0,
            local: 0,
            peers: 0,
            backend: 0,
            pools: BTreeSet::new(),
        }))
    }

    fn with<T>(&self, now: Instant, f: impl FnOnce(&mut MeterState) -> T) -> T {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let n = (m.local + m.peers).max(m.backend);
        if m.local > 0 && n > 0 {
            let hours = now.saturating_duration_since(m.at).as_secs_f64() / 3600.0;
            m.per_request_wh += m.watts / n as f64 * hours;
        }
        m.at = m.at.max(now);
        f(&mut m)
    }

    /// Count a request in from `now`, returning its starting share.
    fn open(&self, now: Instant, pool: &str) -> f64 {
        self.with(now, |m| {
            m.local += 1;
            if !m.pools.contains(pool) {
                m.pools.insert(pool.to_string());
            }
            m.per_request_wh
        })
    }

    fn accrued(&self, now: Instant) -> f64 {
        self.with(now, |m| m.per_request_wh)
    }

    fn close(&self, now: Instant) {
        self.with(now, |m| m.local = m.local.saturating_sub(1));
    }
}

/// A request's claim on a measured power series, taken at admission. Its
/// energy so far is [`EnergyTicket::wh`]; dropping it stops the count.
#[derive(Debug)]
pub struct EnergyTicket {
    meter: Arc<Meter>,
    start_wh: f64,
}

impl EnergyTicket {
    /// Measured Wh accrued since admission, before PUE.
    pub fn wh(&self) -> f64 {
        (self.meter.accrued(Instant::now()) - self.start_wh).max(0.0)
    }
}

impl Drop for EnergyTicket {
    fn drop(&mut self) {
        self.meter.close(Instant::now());
    }
}

/// What a request carries to settlement for its energy figures.
#[derive(Debug, Clone, Default)]
pub struct RequestEnergy {
    /// The model's declared slots per node, for the slot-share fallback.
    pub slots: i64,
    /// Present when the model matched a measured power series at admission.
    pub ticket: Option<Arc<EnergyTicket>>,
}

/// Hot-swappable settings + latest power readings. Cheap to clone.
#[derive(Clone)]
pub struct EnergyEngine {
    settings: Arc<ArcSwap<EnergySettings>>,
    reading: Arc<ArcSwap<Option<PowerReading>>>,
    measured: Arc<ArcSwap<BTreeMap<String, Arc<Meter>>>>,
}

impl EnergyEngine {
    pub fn new(initial: EnergySettings) -> Self {
        EnergyEngine {
            settings: Arc::new(ArcSwap::from_pointee(initial)),
            reading: Arc::new(ArcSwap::from_pointee(None)),
            measured: Arc::new(ArcSwap::from_pointee(BTreeMap::new())),
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

    fn clear_reading(&self) {
        self.reading.store(Arc::new(None));
    }

    pub fn reading(&self) -> Option<PowerReading> {
        **self.reading.load()
    }

    /// Replace the measured watts per series, and with `requests` the
    /// servers' request counts (`None` keeps the last ones). A series that is
    /// still reported keeps its accrued share; one that is gone reads zero
    /// watts from now on, for requests already holding it, and takes no new
    /// ones.
    pub fn store_measured(
        &self,
        watts: &BTreeMap<String, f64>,
        requests: Option<&BTreeMap<String, f64>>,
    ) {
        let now = Instant::now();
        let old = self.measured.load_full();
        let mut next = BTreeMap::new();
        for (name, w) in watts {
            let meter = old
                .get(name)
                .cloned()
                .unwrap_or_else(|| Arc::new(Meter::new(*w, now)));
            let backend = requests.map(|r| r.get(name).map_or(0, |n| n.round() as usize));
            meter.with(now, |s| {
                s.watts = *w;
                if let Some(backend) = backend {
                    s.backend = backend;
                }
            });
            next.insert(name.clone(), meter);
        }
        for (name, m) in old.iter() {
            if !next.contains_key(name) {
                m.with(now, |s| s.watts = 0.0);
            }
        }
        self.measured.store(Arc::new(next));
    }

    fn clear_measured(&self) {
        if !self.measured.load().is_empty() {
            self.store_measured(&BTreeMap::new(), None);
        }
    }

    /// Start metering a request admitted to `route`'s pool, when energy is
    /// on and the model matches a measured power series.
    pub fn start(&self, route: &ResolvedModel) -> Option<EnergyTicket> {
        if !self.settings.load().enabled {
            return None;
        }
        let measured = self.measured.load();
        let (_, meter) = match_power_series(
            measured.as_ref(),
            &route.model_name,
            route.capacity_service.as_deref(),
            &route.upstream_model,
        )?;
        let start_wh = meter.open(Instant::now(), &route.model_name);
        Some(EnergyTicket {
            meter: meter.clone(),
            start_wh,
        })
    }

    /// The energy request for `route`: its declared slots and, when it is
    /// metered, its ticket. Call once the request is admitted.
    pub fn request(&self, route: Option<&ResolvedModel>) -> RequestEnergy {
        RequestEnergy {
            slots: route.map(|r| r.energy_slots_per_node).unwrap_or(0),
            ticket: route.and_then(|r| self.start(r)).map(Arc::new),
        }
    }

    /// Frozen figures for a settled request: its measured share when it was
    /// metered, else its slot share.
    pub fn figures(
        &self,
        energy: &RequestEnergy,
        total_ms: u32,
        queue_wait_ms: u32,
    ) -> EnergyFigures {
        match &energy.ticket {
            Some(ticket) => {
                let s = self.settings.load();
                if !s.enabled {
                    return EnergyFigures::default();
                }
                priced(&s, ticket.wh() * s.pue)
            }
            None => self.compute(energy.slots, total_ms, queue_wait_ms),
        }
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
        // NaN passes `<= 0.0` (every comparison with it is false).
        if r.node_count == 0 || !r.cluster_watts.is_finite() || r.cluster_watts <= 0.0 {
            return EnergyFigures::default();
        }
        let serving_ms = total_ms.saturating_sub(queue_wait_ms) as f64;
        let watts_per_slot =
            r.cluster_watts / r.node_count as f64 / energy_slots_per_node as f64 * s.pue;
        priced(&s, watts_per_slot * serving_ms / 3_600_000.0)
    }

    /// Metered series with requests running on this replica, with the pools
    /// they serve.
    fn busy_meters(&self) -> Vec<(Arc<Meter>, Vec<String>)> {
        let now = Instant::now();
        self.measured
            .load()
            .values()
            .filter_map(|m| {
                m.with(now, |s| {
                    (s.local > 0).then(|| s.pools.iter().cloned().collect::<Vec<_>>())
                })
                .map(|pools| (m.clone(), pools))
            })
            .collect()
    }
}

/// Price `energy_wh` (PUE already applied). A NaN in the frozen ledger
/// serialises as JSON null and sums to NaN in every rollup, so a non-finite
/// figure is recorded as no reading.
fn priced(s: &EnergySettings, energy_wh: f64) -> EnergyFigures {
    let figures = EnergyFigures {
        energy_wh,
        energy_cost_usd: energy_wh / 1000.0 * s.energy_cost_per_kwh,
        co2_g: energy_wh / 1000.0 * s.carbon_g_per_kwh,
    };
    if [figures.energy_wh, figures.energy_cost_usd, figures.co2_g]
        .iter()
        .all(|v| v.is_finite())
    {
        figures
    } else {
        EnergyFigures::default()
    }
}

/// Poll the configured power queries. Always spawned; while the feature is
/// inactive it re-checks settings every 15s so enabling from the settings
/// tab takes effect without a restart. Failures keep the last reading and
/// raise a deduped alert.
pub fn spawn_energy_poller(
    engine: EnergyEngine,
    http: reqwest::Client,
    alerts: obleth_admin::AlertDispatcher,
) {
    tokio::spawn(async move {
        loop {
            let settings = engine.settings();
            if !settings.active() {
                engine.clear_reading();
                engine.clear_measured();
                tokio::time::sleep(Duration::from_secs(15)).await;
                continue;
            }
            let base = settings.prometheus_url.clone();
            let q = settings.power_query.trim();
            if q.is_empty() {
                engine.clear_reading();
            } else {
                let sum = instant_query(&http, &base, &format!("sum({q})")).await;
                let count = instant_query(&http, &base, &format!("count({q})")).await;
                match (sum, count) {
                    (Ok(watts), Ok(nodes)) => {
                        engine.store_reading(PowerReading {
                            cluster_watts: watts,
                            node_count: nodes.max(0.0) as u64,
                            at_ms: crate::tracer::now_ms(),
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
            }
            let mq = settings.model_power_query.trim();
            if mq.is_empty() {
                engine.clear_measured();
            } else {
                match instant_vector(&http, &base, mq, settings.model_label()).await {
                    Ok(watts) => {
                        let rq = settings.model_requests_query.trim();
                        let requests = if rq.is_empty() {
                            Some(BTreeMap::new())
                        } else {
                            match instant_vector(&http, &base, rq, settings.model_label()).await {
                                Ok(r) => Some(r),
                                Err(e) => {
                                    tracing::warn!(error = %e, "energy model requests poll failed");
                                    alerts.issue(
                                        "energy_model_requests_poll_failed",
                                        "Energy per-model requests poll failed",
                                        format!(
                                            "Prometheus `{base}` query `{rq}`: {e}. Measured \
                                             power keeps the last request counts."
                                        ),
                                    );
                                    None
                                }
                            }
                        };
                        engine.store_measured(&watts, requests.as_ref());
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "energy model power poll failed");
                        alerts.issue(
                            "energy_model_power_poll_failed",
                            "Energy per-model power poll failed",
                            format!(
                                "Prometheus `{base}` query `{mq}`: {e}. Metered models \
                                 keep the last good reading."
                            ),
                        );
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(settings.poll_interval_secs.max(5))).await;
        }
    });
}

/// Keep each metered series' count of requests on the other replicas
/// current, so measured power is split across the whole fleet's requests.
/// Reads the shared slot counts; without them (split mode or a backend
/// outage) it assumes the other replicas carry as many as this one.
pub fn spawn_energy_in_flight_sync(engine: EnergyEngine, fairshare: obleth_fairshare::FairShare) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(IN_FLIGHT_SYNC_INTERVAL).await;
            let busy = engine.busy_meters();
            if busy.is_empty() {
                continue;
            }
            let pools: Vec<String> = busy
                .iter()
                .flat_map(|(_, p)| p.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let keys: Vec<_> = pools
                .iter()
                .map(|p| obleth_fairshare::PoolKey::Model(p.clone()))
                .collect();
            let counts = fairshare.cluster_in_flight(&keys).await.map(|(_, per)| {
                pools
                    .iter()
                    .cloned()
                    .zip(per)
                    .collect::<BTreeMap<String, usize>>()
            });
            let replicas = fairshare
                .stats()
                .replicas
                .load(std::sync::atomic::Ordering::Relaxed)
                .max(1);
            let now = Instant::now();
            for (meter, pools) in busy {
                let cluster = counts
                    .as_ref()
                    .map(|c| pools.iter().filter_map(|p| c.get(p)).sum::<usize>());
                meter.with(now, |m| m.peers = peers(m.local, cluster, replicas));
            }
        }
    });
}

/// Requests on the other replicas: the cluster count less this replica's,
/// or, with no cluster count, an even spread over `replicas`.
fn peers(local: usize, cluster: Option<usize>, replicas: usize) -> usize {
    match cluster {
        Some(total) => total.saturating_sub(local),
        None => local * replicas.saturating_sub(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with(reading: Option<PowerReading>, enabled: bool) -> EnergyEngine {
        let e = EnergyEngine::new(obleth_config::EnergySettings {
            enabled,
            prometheus_url: "http://prom".into(),
            power_query: "watts".into(),
            model_power_query: String::new(),
            model_power_label: String::new(),
            model_requests_query: String::new(),
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
    fn non_finite_inputs_yield_zero() {
        for watts in [f64::NAN, f64::INFINITY] {
            let r = PowerReading {
                cluster_watts: watts,
                node_count: 178,
                at_ms: 0,
            };
            assert_eq!(
                engine_with(Some(r), true).compute(8, 1000, 0),
                EnergyFigures::default()
            );
        }
        // A finite reading with a non-finite rate must not leak NaN either.
        let mut s = engine_with(None, true).settings().as_ref().clone();
        s.carbon_g_per_kwh = f64::NAN;
        let e = EnergyEngine::new(s);
        e.store_reading(reading_409kw_178n());
        assert_eq!(e.compute(8, 1000, 0), EnergyFigures::default());
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

    const HOUR: Duration = Duration::from_secs(3600);

    fn close_to(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn measured_power_splits_between_overlapping_requests() {
        // 1000 W; A runs 0-2h, B runs 1-3h. Each gets 1000 Wh alone plus
        // 500 Wh shared, and together they account for all 3000 Wh.
        let t0 = Instant::now();
        let m = Meter::new(1000.0, t0);
        let a = m.open(t0, "p");
        let b = m.open(t0 + HOUR, "p");
        let a_wh = m.accrued(t0 + 2 * HOUR) - a;
        m.close(t0 + 2 * HOUR);
        let b_wh = m.accrued(t0 + 3 * HOUR) - b;
        m.close(t0 + 3 * HOUR);
        assert!(close_to(a_wh, 1500.0), "{a_wh}");
        assert!(close_to(b_wh, 1500.0), "{b_wh}");
    }

    #[test]
    fn idle_power_is_charged_to_nobody() {
        let t0 = Instant::now();
        let m = Meter::new(1000.0, t0);
        let start = m.open(t0 + HOUR, "p");
        let wh = m.accrued(t0 + 2 * HOUR) - start;
        assert!(
            close_to(wh, 1000.0),
            "the idle first hour is not charged: {wh}"
        );
    }

    #[test]
    fn other_replicas_requests_take_their_share() {
        let t0 = Instant::now();
        let m = Meter::new(1000.0, t0);
        let start = m.open(t0, "p");
        m.with(t0, |s| s.peers = 3);
        let wh = m.accrued(t0 + HOUR) - start;
        assert!(close_to(wh, 250.0), "{wh}");
    }

    #[test]
    fn a_power_change_applies_from_when_it_is_read() {
        let t0 = Instant::now();
        let m = Meter::new(1000.0, t0);
        let start = m.open(t0, "p");
        m.with(t0 + HOUR, |s| s.watts = 2000.0);
        let wh = m.accrued(t0 + 2 * HOUR) - start;
        assert!(close_to(wh, 3000.0), "{wh}");
    }

    #[test]
    fn peers_come_from_the_cluster_count_or_an_even_spread() {
        assert_eq!(peers(2, Some(7), 3), 5);
        assert_eq!(
            peers(4, Some(1), 3),
            0,
            "a lagging count never goes negative"
        );
        assert_eq!(peers(2, None, 3), 4);
        assert_eq!(peers(2, None, 1), 0);
    }

    fn measured_engine(enabled: bool) -> EnergyEngine {
        let mut s = engine_with(None, enabled).settings().as_ref().clone();
        s.model_power_query = "sum by (model) (w)".into();
        s.pue = 1.5;
        EnergyEngine::new(s)
    }

    fn route(name: &str, upstream: &str) -> ResolvedModel {
        let mut r = crate::boons::test_support::test_route();
        r.model_name = name.into();
        r.upstream_model = upstream.into();
        r.capacity_service = None;
        r.energy_slots_per_node = 8;
        r
    }

    #[test]
    fn matched_models_get_a_ticket_and_others_fall_back_to_slots() {
        let e = measured_engine(true);
        e.store_measured(&[("up-a".to_string(), 1000.0)].into_iter().collect(), None);
        e.store_reading(reading_409kw_178n());
        let metered = e.request(Some(&route("a", "up-a")));
        assert!(metered.ticket.is_some());
        let fallback = e.request(Some(&route("b", "up-b")));
        assert!(fallback.ticket.is_none());
        assert_eq!(fallback.slots, 8);
        let f = e.figures(&fallback, 3_600_000, 0);
        assert!(
            (f.energy_wh - 287.219 * 1.5).abs() < 0.01,
            "slot share: {f:?}"
        );
        assert!(e.request(None).ticket.is_none());
    }

    #[test]
    fn tickets_count_requests_until_dropped() {
        let e = measured_engine(true);
        e.store_measured(&[("a".to_string(), 1000.0)].into_iter().collect(), None);
        let one = e.request(Some(&route("a", "x")));
        let two = e.request(Some(&route("a", "x")));
        assert_eq!(e.busy_meters().len(), 1);
        assert_eq!(e.busy_meters()[0].1, vec!["a".to_string()]);
        drop(one);
        assert_eq!(e.busy_meters().len(), 1);
        let f = e.figures(&two, 0, 0);
        assert!(f.energy_wh >= 0.0 && f.energy_wh.is_finite());
        drop(two);
        assert!(e.busy_meters().is_empty());
    }

    #[test]
    fn a_vanished_series_reads_zero_and_takes_no_new_requests() {
        let e = measured_engine(true);
        e.store_measured(&[("a".to_string(), 1000.0)].into_iter().collect(), None);
        let held = e.request(Some(&route("a", "x")));
        e.store_measured(&[("b".to_string(), 1000.0)].into_iter().collect(), None);
        let meter = held.ticket.as_ref().unwrap().meter.clone();
        assert_eq!(meter.with(Instant::now(), |s| s.watts), 0.0);
        assert!(e.request(Some(&route("a", "x"))).ticket.is_none());
    }

    #[test]
    fn measured_energy_is_off_when_disabled() {
        let e = measured_engine(false);
        e.store_measured(&[("a".to_string(), 1000.0)].into_iter().collect(), None);
        assert!(e.request(Some(&route("a", "x"))).ticket.is_none());
        let on = measured_engine(true);
        on.store_measured(&[("a".to_string(), 1000.0)].into_iter().collect(), None);
        let held = on.request(Some(&route("a", "x")));
        let mut off = on.settings().as_ref().clone();
        off.enabled = false;
        on.update(off);
        assert_eq!(on.figures(&held, 1000, 0), EnergyFigures::default());
    }

    #[test]
    fn measured_figures_apply_pue_and_prices() {
        let e = measured_engine(true);
        let s = e.settings();
        let f = priced(&s, 2.0 * s.pue);
        assert!(close_to(f.energy_wh, 3.0));
        assert!(close_to(f.energy_cost_usd, 3.0 / 1000.0 * 0.10));
        assert!(close_to(f.co2_g, 3.0 / 1000.0 * 400.0));
    }

    #[test]
    fn the_servers_count_sets_the_floor_on_the_split() {
        // Two other clients' requests on the servers, none on other replicas:
        // the gateway's one request pays a third.
        let t0 = Instant::now();
        let m = Meter::new(900.0, t0);
        let start = m.open(t0, "p");
        m.with(t0, |s| s.backend = 3);
        assert!(close_to(m.accrued(t0 + HOUR) - start, 300.0));
        // A server count below the gateway's own never raises the share.
        let m = Meter::new(900.0, t0);
        let start = m.open(t0, "p");
        m.with(t0, |s| {
            s.peers = 2;
            s.backend = 1;
        });
        assert!(close_to(m.accrued(t0 + HOUR) - start, 300.0));
    }

    #[test]
    fn request_counts_update_with_power_or_are_kept() {
        let e = measured_engine(true);
        let watts: BTreeMap<String, f64> = [("a".to_string(), 1000.0)].into_iter().collect();
        let counts: BTreeMap<String, f64> = [("a".to_string(), 4.0)].into_iter().collect();
        e.store_measured(&watts, Some(&counts));
        let held = e.request(Some(&route("a", "x")));
        let meter = held.ticket.as_ref().unwrap().meter.clone();
        assert_eq!(meter.with(Instant::now(), |s| s.backend), 4);
        e.store_measured(&watts, None);
        assert_eq!(
            meter.with(Instant::now(), |s| s.backend),
            4,
            "a failed count keeps the last"
        );
        e.store_measured(&watts, Some(&BTreeMap::new()));
        assert_eq!(
            meter.with(Instant::now(), |s| s.backend),
            0,
            "no series reads as none"
        );
    }
}
