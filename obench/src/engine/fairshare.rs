//! Per-tenant fairshare sampling and convergence analysis.
//!
//! The gateway's `/api/v1/fairshare/live` endpoint exposes a point-in-time view
//! of the admission scheduler. Polling it on a fixed tick and integrating
//! `in_flight` over time yields each tenant's **realized share of concurrency**,
//! which is the quantity the fairshare mechanism actually allocates.
//!
//! Realized slot occupancy is the headline metric rather than served-token
//! deltas because the scheduler clamps a returning tenant's `served_tokens` up
//! to the current virtual time when it re-enters the queue. That clamp credits
//! service that never happened, so token deltas contain phantom service;
//! occupancy does not.
//!
//! This module is pure — it holds aggregates only, never the full series, so
//! memory is flat regardless of run length. The sampler task owns all I/O.

use std::collections::BTreeMap;

/// One tenant's state in a single `/fairshare/live` poll.
#[derive(Clone, Debug, PartialEq)]
pub struct TenantSample {
    pub name: String,
    pub group: String,
    pub weight: i64,
    /// This tenant's per-model in-flight ceiling, when one is configured.
    pub max_in_flight: Option<u64>,
    pub in_flight: u64,
    pub queued: u64,
    pub served_tokens: f64,
    /// The scheduler's own view of this tenant's entitlement, in [0,1].
    pub weight_share: f64,
}

impl TenantSample {
    /// Whether this tenant was taking part in scheduling at this instant.
    ///
    /// The scheduler never evicts admission state, so `/fairshare/live` keeps
    /// reporting tenants that were deleted runs ago — they show up as bare
    /// UUIDs holding nothing, queueing nothing, and entitled to nothing. Left
    /// in, they bury the real tenants in the plotting series.
    pub fn is_participating(&self) -> bool {
        self.in_flight > 0 || self.queued > 0 || self.weight_share > 0.0
    }
}

/// One fairshare group's state in a single `/fairshare/live` poll.
///
/// Captured separately from tenants because `slot_cap` — the integer number of
/// concurrency slots the group was actually apportioned — is not derivable from
/// tenant rows, and it is what the hierarchical split really hands out.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupSample {
    pub name: String,
    pub weight: i64,
    pub in_flight: u64,
    pub queued: u64,
    pub slot_cap: u64,
    /// Slots held above `slot_cap`, lent by a sibling group under demand.
    pub borrowed: u64,
    pub served_tokens: f64,
    pub weight_share: f64,
}

impl GroupSample {
    /// Whether this group was taking part in scheduling at this instant.
    pub fn is_participating(&self) -> bool {
        self.in_flight > 0 || self.queued > 0 || self.slot_cap > 0
    }
}

/// One key's state in a single poll, inside one pool.
#[derive(Clone, Debug, PartialEq)]
pub struct KeySample {
    pub tenant: String,
    pub name: String,
    pub weight: i64,
    pub max_in_flight: Option<u64>,
    pub in_flight: u64,
    pub queued: u64,
    pub served_tokens: f64,
}

/// One model pool in a single poll.
#[derive(Clone, Debug)]
pub struct PoolSample {
    pub model: String,
    pub cap: u64,
    pub in_flight: u64,
    pub queued: u64,
    pub tenants: Vec<TenantSample>,
    pub keys: Vec<KeySample>,
}

#[derive(Clone, Debug, Default)]
pub struct KeyAcc {
    pub weight: i64,
    pub slot_seconds: f64,
    pub active_seconds: f64,
}

/// One key's convergence inside its tenant, in one pool.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyFairnessResult {
    pub tenant: String,
    pub name: String,
    pub weight: i64,
    pub slot_seconds: f64,
    /// This key's slot-seconds over its tenant's slot-seconds in the pool.
    pub realized_share: f64,
    /// This key's weight over the summed weight of the tenant's competing keys.
    pub expected_share: f64,
    pub share_ratio: f64,
}

/// Per-pool convergence accumulator: tenant fairness reuses
/// [`FairshareAccumulator`] with the pool cap as capacity; key fairness is
/// measured inside each tenant.
#[derive(Clone, Debug)]
pub struct PoolAccumulator {
    model: String,
    tenants: FairshareAccumulator,
    keys: BTreeMap<(String, String), KeyAcc>,
    idle_with_backlog_ticks: u64,
    cap_violations: u64,
}

#[derive(Clone, Debug)]
pub struct PoolSummary {
    pub model: String,
    pub cap: u64,
    pub tenants: FairshareSummary,
    pub keys: Vec<KeyFairnessResult>,
    /// Jain's index over key share ratios, across all tenants in the pool.
    pub key_jain_index: f64,
    /// Polls folded into this pool, so `idle_with_backlog_ticks` can be read as
    /// a rate rather than a raw count.
    pub samples: u64,
    /// Ticks where the pool had free slots and a standing queue at once:
    /// the scheduler (or the ceiling) left capacity idle under demand.
    ///
    /// Ticks where *every* waiting tenant and key was sitting on its own
    /// ceiling are excluded: the pool having room nobody is eligible to use is
    /// the caps working, not wasted capacity. A backlog behind an entity that
    /// has no cap (or is below it) still counts.
    pub idle_with_backlog_ticks: u64,
    /// Samples where a pool, tenant, or key exceeded its cap.
    pub cap_violations: u64,
}

impl PoolAccumulator {
    pub fn new(model: &str, cap: u64) -> Self {
        let mut tenants = FairshareAccumulator::new();
        tenants.note_config("", cap);
        Self {
            model: model.into(),
            tenants,
            keys: BTreeMap::new(),
            idle_with_backlog_ticks: 0,
            cap_violations: 0,
        }
    }

    pub fn note_cap(&mut self, cap: u64) {
        self.tenants.note_config("", cap);
    }

    pub fn observe(&mut self, tenants: &[TenantSample], keys: &[KeySample], dt_s: f64) {
        let cap = self.tenants.max_in_flight;
        let in_flight: u64 = tenants.iter().map(|t| t.in_flight).sum();
        let queued: u64 = tenants.iter().map(|t| t.queued).sum();
        // Free pool slots are only excused when *every* entity that is waiting
        // sits on its own ceiling: then the queue is waiting on those caps, not
        // on the pool. One capped entity elsewhere in the pool explains
        // nothing, so a backlog behind an uncapped tenant or key is still
        // wasted capacity.
        let backlogged = tenants.iter().any(|t| t.queued > 0) || keys.iter().any(|k| k.queued > 0);
        let backlogged_capped = tenants
            .iter()
            .filter(|t| t.queued > 0)
            .all(|t| matches!(t.max_in_flight, Some(c) if t.in_flight >= c))
            && keys
                .iter()
                .filter(|k| k.queued > 0)
                .all(|k| matches!(k.max_in_flight, Some(c) if k.in_flight >= c));
        let entity_at_cap = backlogged && backlogged_capped;
        // A gateway that does not report a pool size gives nothing to measure
        // free slots or an overrun against.
        let pool_cap_known = cap > 0;
        if pool_cap_known && !entity_at_cap && in_flight < cap && queued > 0 {
            self.idle_with_backlog_ticks += 1;
        }
        if pool_cap_known && in_flight > cap {
            self.cap_violations += 1;
        }
        for t in tenants {
            if matches!(t.max_in_flight, Some(c) if t.in_flight > c) {
                self.cap_violations += 1;
            }
        }
        for k in keys {
            if matches!(k.max_in_flight, Some(c) if k.in_flight > c) {
                self.cap_violations += 1;
            }
            let acc = self
                .keys
                .entry((k.tenant.clone(), k.name.clone()))
                .or_default();
            acc.weight = k.weight;
            acc.slot_seconds += k.in_flight as f64 * dt_s;
            if k.in_flight > 0 || k.queued > 0 {
                acc.active_seconds += dt_s;
            }
        }
        self.tenants.observe(tenants, dt_s);
    }

    pub fn summarize(&self) -> PoolSummary {
        let keys = key_fairness(&self.keys);
        let ratios: Vec<f64> = keys
            .iter()
            .filter(|k| k.expected_share > 0.0)
            .map(|k| k.share_ratio)
            .collect();
        let tenants = self.tenants.summarize();
        PoolSummary {
            model: self.model.clone(),
            cap: self.tenants.max_in_flight,
            samples: tenants.samples,
            tenants,
            key_jain_index: jain_index(&ratios),
            keys,
            idle_with_backlog_ticks: self.idle_with_backlog_ticks,
            cap_violations: self.cap_violations,
        }
    }
}

/// Key fairness inside each tenant: realized = key slot-seconds over the
/// tenant's slot-seconds; expected = key weight over the competing keys'
/// summed weight. Keys that never competed are skipped.
pub fn key_fairness(keys: &BTreeMap<(String, String), KeyAcc>) -> Vec<KeyFairnessResult> {
    let mut tenant_slots: BTreeMap<&str, f64> = BTreeMap::new();
    let mut tenant_weight: BTreeMap<&str, i64> = BTreeMap::new();
    for ((tenant, _), acc) in keys {
        if acc.active_seconds > 0.0 {
            *tenant_slots.entry(tenant).or_insert(0.0) += acc.slot_seconds;
            *tenant_weight.entry(tenant).or_insert(0) += acc.weight.max(1);
        }
    }
    keys.iter()
        .filter(|(_, acc)| acc.active_seconds > 0.0)
        .map(|((tenant, name), acc)| {
            let slots = tenant_slots.get(tenant.as_str()).copied().unwrap_or(0.0);
            let weight = tenant_weight.get(tenant.as_str()).copied().unwrap_or(1) as f64;
            let realized_share = if slots > 0.0 {
                acc.slot_seconds / slots
            } else {
                0.0
            };
            let expected_share = acc.weight.max(1) as f64 / weight;
            KeyFairnessResult {
                tenant: tenant.clone(),
                name: name.clone(),
                weight: acc.weight,
                slot_seconds: acc.slot_seconds,
                realized_share,
                expected_share,
                share_ratio: if expected_share > 0.0 {
                    realized_share / expected_share
                } else {
                    0.0
                },
            }
        })
        .collect()
}

/// Fold per-pool findings into the run verdict.
pub fn apply_pool_verdicts(
    verdict: crate::engine::stats::Verdict,
    pools: &[PoolSummary],
) -> crate::engine::stats::Verdict {
    use crate::engine::stats::Verdict;
    let mut issues = Vec::new();
    for p in pools {
        if p.cap_violations > 0 {
            issues.push(format!(
                "pool {}: {} cap violation sample(s)",
                p.model, p.cap_violations
            ));
        }
        // One tick can be a poll racing a release, and a handful over a long run
        // is noise; a sustained fraction of the run is real.
        if p.idle_with_backlog_ticks > 2 && p.idle_with_backlog_ticks * 10 > p.samples {
            issues.push(format!(
                "pool {}: {} tick(s) with free slots and a standing queue",
                p.model, p.idle_with_backlog_ticks
            ));
        }
        if !p.tenants.starved.is_empty() {
            issues.push(format!(
                "pool {}: starved {}",
                p.model,
                p.tenants.starved.join(", ")
            ));
        }
    }
    if issues.is_empty() {
        return verdict;
    }
    match verdict {
        Verdict::Pass => Verdict::Fail(issues),
        Verdict::Fail(mut existing) => {
            existing.extend(issues);
            Verdict::Fail(existing)
        }
    }
}

/// Per-tenant aggregates accumulated across a run.
#[derive(Clone, Debug, Default)]
struct TenantAcc {
    group: String,
    weight: i64,
    slot_seconds: f64,
    active_seconds: f64,
    weight_share_seconds: f64,
    first_served: Option<f64>,
    last_served: f64,
    peak_queued: u64,
    backlogged_ticks: u64,
}

/// Integrates `/fairshare/live` polls into per-tenant occupancy aggregates.
#[derive(Clone, Debug, Default)]
pub struct FairshareAccumulator {
    tenants: BTreeMap<String, TenantAcc>,
    samples: u64,
    elapsed_s: f64,
    algorithm: String,
    max_in_flight: u64,
    capacity_seconds: f64,
}

/// One tenant's convergence result over a whole run.
#[derive(Clone, Debug, PartialEq)]
pub struct TenantResult {
    pub name: String,
    pub group: String,
    pub weight: i64,
    /// Integral of `in_flight` over the run, in slot-seconds.
    pub slot_seconds: f64,
    /// Seconds this tenant was competing (holding or waiting for a slot).
    pub active_seconds: f64,
    /// This tenant's slot-seconds as a fraction of **total capacity** over the
    /// run, i.e. `max_in_flight x elapsed`.
    ///
    /// Deliberately not a fraction of *observed* occupancy: dividing by slots
    /// actually used hides under-utilization and inflates the share of whoever
    /// was backlogged while capacity sat idle.
    pub realized_share: f64,
    /// Time-weighted mean of the scheduler's `weight_share` while active, as
    /// the gateway reported it.
    pub expected_share: f64,
    /// [`Self::expected_share`] renormalized across the tenants that actually
    /// competed, so entitlements sum to 1.
    ///
    /// The gateway divides `weight_share` by the total weight of every tenant
    /// it still holds admission state for, including ones long finished — that
    /// map is never evicted. On a long-lived gateway the raw entitlements
    /// therefore sum to well under 1 and every ratio is inflated.
    pub normalized_expected_share: f64,
    /// `realized_share / normalized_expected_share`. 1.0 is perfect weighted
    /// fairness.
    pub share_ratio: f64,
    pub served_tokens_delta: f64,
    pub token_share: f64,
    pub peak_queued: u64,
    pub backlogged_ticks: u64,
    /// Backlogged at some point, yet never served across the entire run.
    pub starved: bool,
}

/// Whole-run fairshare convergence summary.
#[derive(Clone, Debug, PartialEq)]
pub struct FairshareSummary {
    /// Scheduler mode the run actually executed under. Recorded because the
    /// mode is boot-time env config, so a hierarchical run and a weighted run
    /// are otherwise indistinguishable in the artifacts.
    pub algorithm: String,
    pub max_in_flight: u64,
    pub samples: u64,
    pub elapsed_s: f64,
    pub tenants: Vec<TenantResult>,
    /// Jain's fairness index over `share_ratio`, so perfect *weighted* fairness
    /// reads 1.0 rather than the unweighted 1/n.
    pub jain_index: f64,
    pub starved: Vec<String>,
    /// Slot-seconds used as a fraction of slot-seconds available. Below 1.0 with
    /// a standing queue means the scheduler left capacity idle, and any share
    /// figure from the run describes offered load rather than admission.
    pub utilization: f64,
}

impl FairshareSummary {
    /// Only the tenants that actually competed during the run.
    ///
    /// A shared gateway always carries tenants that sent nothing — the control
    /// plane's own identity, leftovers from earlier runs — and reporting them
    /// as "0.0% of 0.0% expected" is noise, not a result.
    pub fn competed(&self) -> Vec<&TenantResult> {
        self.tenants
            .iter()
            .filter(|t| t.active_seconds > 0.0)
            .collect()
    }
}

impl FairshareAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the scheduler configuration the run executed under. Called on
    /// every poll; the last non-empty value wins.
    pub fn note_config(&mut self, algorithm: &str, max_in_flight: u64) {
        if !algorithm.is_empty() {
            self.algorithm = algorithm.to_string();
        }
        if max_in_flight > 0 {
            self.max_in_flight = max_in_flight;
        }
    }

    /// Fold one poll into the aggregates. `dt_s` is the interval this sample
    /// represents (the tick length), used to weight the occupancy integral.
    pub fn observe(&mut self, samples: &[TenantSample], dt_s: f64) {
        self.samples += 1;
        self.elapsed_s += dt_s;
        // Integrated rather than taken from a final `max_in_flight`, so a run
        // whose capacity is changed mid-flight still divides by what was
        // actually available.
        self.capacity_seconds += self.max_in_flight as f64 * dt_s;
        for sample in samples {
            let acc = self.tenants.entry(sample.name.clone()).or_default();
            acc.group = sample.group.clone();
            acc.weight = sample.weight;
            acc.slot_seconds += sample.in_flight as f64 * dt_s;

            // A tenant only accrues entitlement while it is actually competing;
            // averaging `weight_share` over idle ticks would understate it.
            if sample.in_flight > 0 || sample.queued > 0 {
                acc.active_seconds += dt_s;
                acc.weight_share_seconds += sample.weight_share * dt_s;
            }
            if sample.queued > 0 {
                acc.backlogged_ticks += 1;
                acc.peak_queued = acc.peak_queued.max(sample.queued);
            }
            acc.first_served.get_or_insert(sample.served_tokens);
            acc.last_served = sample.served_tokens;
        }
    }

    pub fn summarize(&self) -> FairshareSummary {
        let total_slot_seconds: f64 = self.tenants.values().map(|t| t.slot_seconds).sum();
        let deltas: BTreeMap<&String, f64> = self
            .tenants
            .iter()
            .map(|(name, t)| {
                (
                    name,
                    (t.last_served - t.first_served.unwrap_or(0.0)).max(0.0),
                )
            })
            .collect();
        let total_delta: f64 = deltas.values().sum();

        // The gateway's entitlements are diluted by tenants it still holds
        // admission state for, so renormalize across the ones that competed
        // before any ratio is taken.
        let raw_expected = |t: &TenantAcc| {
            if t.active_seconds > 0.0 {
                t.weight_share_seconds / t.active_seconds
            } else {
                0.0
            }
        };
        let total_expected: f64 = self.tenants.values().map(raw_expected).sum();

        let tenants: Vec<TenantResult> = self
            .tenants
            .iter()
            .map(|(name, t)| {
                let realized_share = if self.capacity_seconds > 0.0 {
                    t.slot_seconds / self.capacity_seconds
                } else {
                    0.0
                };
                let expected_share = raw_expected(t);
                let normalized_expected_share = if total_expected > 0.0 {
                    expected_share / total_expected
                } else {
                    0.0
                };
                let share_ratio = if normalized_expected_share > 0.0 {
                    realized_share / normalized_expected_share
                } else {
                    0.0
                };
                let served_tokens_delta = deltas.get(name).copied().unwrap_or(0.0);
                TenantResult {
                    name: name.clone(),
                    group: t.group.clone(),
                    weight: t.weight,
                    slot_seconds: t.slot_seconds,
                    active_seconds: t.active_seconds,
                    realized_share,
                    normalized_expected_share,
                    expected_share,
                    share_ratio,
                    served_tokens_delta,
                    token_share: if total_delta > 0.0 {
                        served_tokens_delta / total_delta
                    } else {
                        0.0
                    },
                    peak_queued: t.peak_queued,
                    backlogged_ticks: t.backlogged_ticks,
                    // Occupancy alone would false-positive on a tenant whose
                    // requests all completed between two polls, so served-token
                    // growth is accepted as independent evidence of service.
                    starved: t.backlogged_ticks > 0
                        && t.slot_seconds == 0.0
                        && served_tokens_delta == 0.0,
                }
            })
            .collect();

        // Only tenants that actually competed have a meaningful entitlement, so
        // idle ones must not drag the index down.
        let ratios: Vec<f64> = tenants
            .iter()
            .filter(|t| t.expected_share > 0.0)
            .map(|t| t.share_ratio)
            .collect();
        let starved = tenants
            .iter()
            .filter(|t| t.starved)
            .map(|t| t.name.clone())
            .collect();

        FairshareSummary {
            algorithm: self.algorithm.clone(),
            max_in_flight: self.max_in_flight,
            samples: self.samples,
            elapsed_s: self.elapsed_s,
            jain_index: jain_index(&ratios),
            tenants,
            starved,
            utilization: if self.capacity_seconds > 0.0 {
                total_slot_seconds / self.capacity_seconds
            } else {
                0.0
            },
        }
    }
}

/// Effective fairshare sampling interval, in milliseconds. `0` disables it.
///
/// Sampling polls the scheduler task once per tick, so it is suppressed for the
/// `extreme` profile: that run exists to measure gateway overhead, and the
/// instrument must not contribute to the load it is reporting on.
pub fn sample_interval_ms(requested_ms: u64, profile: &str, gateway_observable: bool) -> u64 {
    if !gateway_observable || profile.eq_ignore_ascii_case("extreme") {
        return 0;
    }
    requested_ms
}

/// Fold a starvation finding into the run verdict. Starvation is only
/// meaningful under contention between tenants, so a run with fewer than two
/// competing tenants never fails on this basis.
pub fn apply_starvation_verdict(
    verdict: crate::engine::stats::Verdict,
    summary: &FairshareSummary,
) -> crate::engine::stats::Verdict {
    use crate::engine::stats::Verdict;
    let competing = summary
        .tenants
        .iter()
        .filter(|t| t.expected_share > 0.0)
        .count();
    if summary.starved.is_empty() || competing < 2 {
        return verdict;
    }
    let issue = format!(
        "fairshare starved {} tenant(s) under contention: {}",
        summary.starved.len(),
        summary.starved.join(", ")
    );
    match verdict {
        Verdict::Pass => Verdict::Fail(vec![issue]),
        Verdict::Fail(mut issues) => {
            issues.push(issue);
            Verdict::Fail(issues)
        }
    }
}

/// Jain's fairness index over a set of realized/expected ratios.
/// Returns 0.0 for an empty input.
pub fn jain_index(ratios: &[f64]) -> f64 {
    if ratios.is_empty() {
        return 0.0;
    }
    let sum: f64 = ratios.iter().sum();
    let sum_sq: f64 = ratios.iter().map(|r| r * r).sum();
    if sum_sq == 0.0 {
        return 0.0;
    }
    sum * sum / (ratios.len() as f64 * sum_sq)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str, in_flight: u64, queued: u64, served: f64, weight_share: f64) -> TenantSample {
        TenantSample {
            name: name.into(),
            group: "g".into(),
            weight: 100,
            max_in_flight: None,
            in_flight,
            queued,
            served_tokens: served,
            weight_share,
        }
    }

    fn k(tenant: &str, name: &str, weight: i64, in_flight: u64, queued: u64) -> KeySample {
        KeySample {
            tenant: tenant.into(),
            name: name.into(),
            weight,
            max_in_flight: None,
            in_flight,
            queued,
            served_tokens: 0.0,
        }
    }

    fn find<'a>(sum: &'a FairshareSummary, name: &str) -> &'a TenantResult {
        sum.tenants
            .iter()
            .find(|t| t.name == name)
            .expect("tenant present in summary")
    }

    #[test]
    fn equal_tenants_served_equally_score_unit_ratios() {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        for _ in 0..10 {
            acc.observe(&[s("a", 4, 0, 0.0, 0.5), s("b", 4, 0, 0.0, 0.5)], 1.0);
        }
        let sum = acc.summarize();
        assert!((find(&sum, "a").realized_share - 0.5).abs() < 1e-9);
        assert!((find(&sum, "a").share_ratio - 1.0).abs() < 1e-9);
        assert!((sum.jain_index - 1.0).abs() < 1e-9);
    }

    #[test]
    fn one_tenant_taking_everything_halves_jain_for_two_equal_tenants() {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        for _ in 0..10 {
            acc.observe(&[s("a", 8, 0, 0.0, 0.5), s("b", 0, 3, 0.0, 0.5)], 1.0);
        }
        let sum = acc.summarize();
        // ratios are 2.0 and 0.0 -> J = (2)^2 / (2 * 4) = 0.5
        assert!((sum.jain_index - 0.5).abs() < 1e-9);
    }

    #[test]
    fn realized_share_is_measured_against_capacity_not_observed_occupancy() {
        // 8 slots available; the two tenants together use only 2 of them.
        // Sharing "of what was used" would report 50% each and hide the 75%
        // of capacity left idle.
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        for _ in 0..10 {
            acc.observe(&[s("a", 1, 4, 0.0, 0.5), s("b", 1, 4, 0.0, 0.5)], 1.0);
        }
        let sum = acc.summarize();
        assert!((find(&sum, "a").realized_share - 0.125).abs() < 1e-9);
        assert!((sum.utilization - 0.25).abs() < 1e-9);
    }

    #[test]
    fn utilization_is_one_when_every_slot_is_working() {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        acc.observe(&[s("a", 5, 2, 0.0, 0.5), s("b", 3, 2, 0.0, 0.5)], 1.0);
        let sum = acc.summarize();
        assert!((sum.utilization - 1.0).abs() < 1e-9);
        assert!((find(&sum, "a").realized_share - 0.625).abs() < 1e-9);
    }

    #[test]
    fn realized_share_is_time_weighted_not_sample_counted() {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        // "a" holds 1 slot for 9 s; "b" holds 9 slots for 1 s. Equal area.
        acc.observe(&[s("a", 1, 0, 0.0, 0.5), s("b", 9, 0, 0.0, 0.5)], 1.0);
        acc.observe(&[s("a", 1, 0, 0.0, 0.5), s("b", 0, 1, 0.0, 0.5)], 8.0);
        let sum = acc.summarize();
        assert!((find(&sum, "a").slot_seconds - 9.0).abs() < 1e-9);
        assert!((find(&sum, "b").slot_seconds - 9.0).abs() < 1e-9);
        // Equal areas give equal share. 9 slot-seconds each against 8 slots
        // over 9 s of capacity is 12.5%, and the pair leave 75% of the pool
        // idle — which the capacity-relative denominator makes visible.
        assert!((find(&sum, "a").realized_share - 0.125).abs() < 1e-9);
        assert!((find(&sum, "b").realized_share - 0.125).abs() < 1e-9);
        assert!((sum.utilization - 0.25).abs() < 1e-9);
    }

    #[test]
    fn backlogged_tenant_never_served_is_starved() {
        let mut acc = FairshareAccumulator::new();
        for _ in 0..10 {
            acc.observe(
                &[s("hog", 8, 0, 100.0, 0.5), s("victim", 0, 5, 0.0, 0.5)],
                1.0,
            );
        }
        let sum = acc.summarize();
        assert!(find(&sum, "victim").starved);
        assert!(!find(&sum, "hog").starved);
        assert_eq!(sum.starved, vec!["victim".to_string()]);
    }

    #[test]
    fn tenant_served_between_polls_is_not_starved() {
        // in_flight samples always land on zero, but served_tokens grows — the
        // tenant was served, the 1 Hz poll just never caught it holding a slot.
        let mut acc = FairshareAccumulator::new();
        for i in 0..10 {
            acc.observe(&[s("quick", 0, 2, i as f64 * 50.0, 0.5)], 1.0);
        }
        let sum = acc.summarize();
        assert!(!find(&sum, "quick").starved);
        assert!(sum.starved.is_empty());
    }

    #[test]
    fn idle_tenant_that_never_queued_is_not_starved() {
        let mut acc = FairshareAccumulator::new();
        for _ in 0..5 {
            acc.observe(
                &[s("busy", 4, 0, 10.0, 0.5), s("idle", 0, 0, 0.0, 0.5)],
                1.0,
            );
        }
        let sum = acc.summarize();
        assert!(!find(&sum, "idle").starved);
    }

    #[test]
    fn hierarchical_500_50_realizes_slot_caps_not_the_continuous_ratio() {
        // The paper's worked example: max_in_flight 8, weights 500/50. The
        // scheduler reports the continuous entitlement (0.909/0.0909) but the
        // integer slot caps it can actually hand out are 7 and 1.
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        for _ in 0..100 {
            acc.observe(
                &[
                    TenantSample {
                        name: "chatbot".into(),
                        group: "prod".into(),
                        weight: 500,
                        max_in_flight: None,
                        in_flight: 7,
                        queued: 4,
                        served_tokens: 0.0,
                        weight_share: 500.0 / 550.0,
                    },
                    TenantSample {
                        name: "api-batch".into(),
                        group: "dev".into(),
                        weight: 50,
                        max_in_flight: None,
                        in_flight: 1,
                        queued: 4,
                        served_tokens: 0.0,
                        weight_share: 50.0 / 550.0,
                    },
                ],
                1.0,
            );
        }
        let sum = acc.summarize();
        assert!((find(&sum, "chatbot").realized_share - 0.875).abs() < 1e-9);
        assert!((find(&sum, "api-batch").realized_share - 0.125).abs() < 1e-9);
        // The min-one guarantee over-serves the small tenant by ~37%.
        assert!((find(&sum, "api-batch").share_ratio - 1.375).abs() < 1e-6);
        assert!(sum.starved.is_empty());
    }

    #[test]
    fn token_share_uses_first_to_last_served_delta() {
        let mut acc = FairshareAccumulator::new();
        acc.observe(&[s("a", 1, 0, 100.0, 0.5), s("b", 1, 0, 500.0, 0.5)], 1.0);
        acc.observe(&[s("a", 1, 0, 400.0, 0.5), s("b", 1, 0, 600.0, 0.5)], 1.0);
        let sum = acc.summarize();
        // deltas: a = 300, b = 100 -> shares 0.75 / 0.25
        assert!((find(&sum, "a").served_tokens_delta - 300.0).abs() < 1e-9);
        assert!((find(&sum, "a").token_share - 0.75).abs() < 1e-9);
    }

    #[test]
    fn peak_queued_and_backlogged_ticks_are_tracked() {
        let mut acc = FairshareAccumulator::new();
        acc.observe(&[s("a", 1, 0, 0.0, 1.0)], 1.0);
        acc.observe(&[s("a", 1, 9, 0.0, 1.0)], 1.0);
        acc.observe(&[s("a", 1, 3, 0.0, 1.0)], 1.0);
        let sum = acc.summarize();
        assert_eq!(find(&sum, "a").peak_queued, 9);
        assert_eq!(find(&sum, "a").backlogged_ticks, 2);
    }

    #[test]
    fn expected_share_ignores_ticks_where_the_tenant_was_absent() {
        let mut acc = FairshareAccumulator::new();
        // "late" only appears for the last tick, entitled to 0.25 there.
        acc.observe(&[s("early", 4, 0, 0.0, 1.0)], 1.0);
        acc.observe(
            &[s("early", 4, 0, 0.0, 0.75), s("late", 1, 0, 0.0, 0.25)],
            1.0,
        );
        let sum = acc.summarize();
        assert!((find(&sum, "late").expected_share - 0.25).abs() < 1e-9);
        // "early" averages 1.0 and 0.75 over its two active seconds.
        assert!((find(&sum, "early").expected_share - 0.875).abs() < 1e-9);
    }

    #[test]
    fn summary_carries_the_scheduler_config_the_run_used() {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        acc.observe(&[s("a", 1, 0, 0.0, 1.0)], 1.0);
        let sum = acc.summarize();
        assert_eq!(sum.algorithm, "hierarchical");
        assert_eq!(sum.max_in_flight, 8);
    }

    #[test]
    fn blank_algorithm_does_not_overwrite_a_known_one() {
        // A degraded poll (older gateway, partial payload) must not erase the
        // label that makes the artifact self-describing.
        let mut acc = FairshareAccumulator::new();
        acc.note_config("weighted", 16);
        acc.note_config("", 0);
        let sum = acc.summarize();
        assert_eq!(sum.algorithm, "weighted");
        assert_eq!(sum.max_in_flight, 16);
    }

    #[test]
    fn share_ratio_renormalizes_entitlement_diluted_by_departed_tenants() {
        // Both tenants split the gateway evenly, but the gateway reports each
        // as entitled to only 25% because stale `served` entries inflate its
        // weight total. Ratios must still read 1.0, not 2.0.
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        for _ in 0..10 {
            acc.observe(&[s("a", 4, 1, 0.0, 0.25), s("b", 4, 1, 0.0, 0.25)], 1.0);
        }
        let sum = acc.summarize();
        let a = find(&sum, "a");
        assert!(
            (a.expected_share - 0.25).abs() < 1e-9,
            "raw value preserved"
        );
        assert!((a.normalized_expected_share - 0.5).abs() < 1e-9);
        assert!((a.share_ratio - 1.0).abs() < 1e-9);
        assert!((sum.jain_index - 1.0).abs() < 1e-9);
    }

    #[test]
    fn renormalization_preserves_relative_entitlement() {
        // 500:50 diluted to a third of its true magnitude still yields the same
        // 10:1 relationship after renormalizing.
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        for _ in 0..10 {
            acc.observe(
                &[s("big", 7, 2, 0.0, 0.303), s("small", 1, 2, 0.0, 0.0303)],
                1.0,
            );
        }
        let sum = acc.summarize();
        assert!((find(&sum, "big").normalized_expected_share - 0.909).abs() < 1e-3);
        assert!((find(&sum, "small").normalized_expected_share - 0.0909).abs() < 1e-3);
        assert!((find(&sum, "small").share_ratio - 1.375).abs() < 1e-2);
    }

    #[test]
    fn competed_excludes_tenants_that_never_held_or_queued_a_request() {
        let mut acc = FairshareAccumulator::new();
        for _ in 0..5 {
            acc.observe(
                &[
                    s("worker", 4, 1, 500.0, 0.5),
                    // Shaped like the gateway's own idle identity: present in
                    // every poll, never competing, entitlement reported as 0.
                    s("__control_plane__", 0, 0, 139160.0, 0.0),
                ],
                1.0,
            );
        }
        let sum = acc.summarize();
        assert_eq!(sum.tenants.len(), 2, "both are retained in the full record");
        let competed: Vec<&str> = sum.competed().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(competed, vec!["worker"]);
    }

    #[test]
    fn active_seconds_counts_only_ticks_where_the_tenant_competed() {
        let mut acc = FairshareAccumulator::new();
        acc.observe(&[s("a", 2, 0, 0.0, 1.0)], 1.0);
        acc.observe(&[s("a", 0, 0, 0.0, 0.0)], 1.0);
        acc.observe(&[s("a", 0, 3, 0.0, 1.0)], 1.0);
        let sum = acc.summarize();
        assert!((find(&sum, "a").active_seconds - 2.0).abs() < 1e-9);
    }

    #[test]
    fn empty_accumulator_summarizes_without_panicking() {
        let sum = FairshareAccumulator::new().summarize();
        assert_eq!(sum.samples, 0);
        assert_eq!(sum.algorithm, "");
        assert!(sum.tenants.is_empty());
        assert_eq!(sum.jain_index, 0.0);
        assert!(sum.starved.is_empty());
    }

    #[test]
    fn run_with_no_service_at_all_yields_zero_shares() {
        let mut acc = FairshareAccumulator::new();
        acc.observe(&[s("a", 0, 0, 0.0, 0.5), s("b", 0, 0, 0.0, 0.5)], 1.0);
        let sum = acc.summarize();
        assert_eq!(find(&sum, "a").realized_share, 0.0);
        assert_eq!(find(&sum, "a").share_ratio, 0.0);
    }

    // ── participation filter ──────────────────────────────────────────────────

    #[test]
    fn a_deleted_tenant_still_in_the_scheduler_is_not_participating() {
        // What /fairshare/live actually returns for a torn-down obench tenant:
        // a bare UUID, no slots, no queue, no entitlement.
        let ghost = s("3ac00070-90ba-4d02-b418-f1b5ff65a273", 0, 0, 4820.0, 0.0);
        assert!(!ghost.is_participating());
    }

    #[test]
    fn a_tenant_holding_or_waiting_for_a_slot_is_participating() {
        assert!(s("a", 3, 0, 0.0, 0.0).is_participating());
        assert!(s("b", 0, 5, 0.0, 0.0).is_participating());
    }

    #[test]
    fn a_momentarily_idle_tenant_with_entitlement_is_still_participating() {
        // Between two requests a real tenant reports 0/0, but its group is
        // still active so the gateway still credits it a share. Dropping it
        // would punch holes in the convergence series.
        assert!(s("a", 0, 0, 100.0, 0.25).is_participating());
    }

    #[test]
    fn a_group_with_no_slots_queue_or_cap_is_not_participating() {
        let g = |in_flight, queued, slot_cap| GroupSample {
            name: "default".into(),
            weight: 100,
            in_flight,
            queued,
            slot_cap,
            borrowed: 0,
            served_tokens: 141622.0,
            weight_share: 0.0,
        };
        assert!(!g(0, 0, 0).is_participating());
        assert!(g(0, 0, 1).is_participating());
        assert!(g(1, 0, 0).is_participating());
        assert!(g(0, 2, 0).is_participating());
    }

    // ── sampler gating ────────────────────────────────────────────────────────

    #[test]
    fn sampling_uses_the_requested_interval_for_a_normal_profile() {
        assert_eq!(sample_interval_ms(1000, "heavy", true), 1000);
        assert_eq!(sample_interval_ms(250, "light", true), 250);
    }

    #[test]
    fn sampling_is_off_for_the_extreme_overhead_profile() {
        // The instrument must not perturb the measurement it reports on.
        assert_eq!(sample_interval_ms(1000, "extreme", true), 0);
    }

    #[test]
    fn sampling_is_off_without_an_observable_gateway() {
        // A remote live gateway gives obench no admin token to poll with.
        assert_eq!(sample_interval_ms(1000, "heavy", false), 0);
    }

    #[test]
    fn sampling_honours_an_explicit_disable() {
        assert_eq!(sample_interval_ms(0, "heavy", true), 0);
    }

    // ── starvation verdict ────────────────────────────────────────────────────

    use crate::engine::stats::Verdict;

    fn summary_with(starved: Vec<&str>, active_tenants: usize) -> FairshareSummary {
        let mut acc = FairshareAccumulator::new();
        let mut samples = Vec::new();
        for i in 0..active_tenants {
            let name = format!("t{i}");
            let starving = starved.contains(&name.as_str());
            samples.push(TenantSample {
                name,
                group: "g".into(),
                weight: 100,
                max_in_flight: None,
                in_flight: if starving { 0 } else { 4 },
                queued: 3,
                served_tokens: if starving { 0.0 } else { 100.0 },
                weight_share: 1.0 / active_tenants as f64,
            });
        }
        acc.observe(&samples, 1.0);
        acc.summarize()
    }

    #[test]
    fn starvation_under_contention_fails_an_otherwise_passing_run() {
        let sum = summary_with(vec!["t1"], 2);
        assert_eq!(sum.starved, vec!["t1".to_string()]);
        match apply_starvation_verdict(Verdict::Pass, &sum) {
            Verdict::Fail(issues) => {
                assert!(issues.iter().any(|i| i.contains("starved")), "{issues:?}")
            }
            Verdict::Pass => panic!("starvation must fail the run"),
        }
    }

    #[test]
    fn starvation_appends_to_an_already_failing_run() {
        let sum = summary_with(vec!["t1"], 2);
        let existing = Verdict::Fail(vec!["error rate too high".into()]);
        match apply_starvation_verdict(existing, &sum) {
            Verdict::Fail(issues) => {
                assert_eq!(issues.len(), 2);
                assert_eq!(issues[0], "error rate too high");
            }
            Verdict::Pass => panic!("must stay failed"),
        }
    }

    #[test]
    fn a_single_competing_tenant_cannot_be_starved_by_unfairness() {
        // One tenant backlogged and unserved is a stall, which the watchdog
        // already owns — it is not a fairness finding.
        let sum = summary_with(vec!["t0"], 1);
        assert_eq!(apply_starvation_verdict(Verdict::Pass, &sum), Verdict::Pass);
    }

    #[test]
    fn no_starvation_leaves_the_verdict_untouched() {
        let sum = summary_with(vec![], 3);
        assert_eq!(apply_starvation_verdict(Verdict::Pass, &sum), Verdict::Pass);
        assert_eq!(
            apply_starvation_verdict(Verdict::Fail(vec!["x".into()]), &sum),
            Verdict::Fail(vec!["x".into()])
        );
    }

    // ── per-pool and per-key convergence ──────────────────────────────────────

    #[test]
    fn keys_split_their_tenant_by_weight() {
        let mut acc = PoolAccumulator::new("m", 4);
        for _ in 0..10 {
            acc.observe(&[], &[k("t", "a", 100, 1, 1), k("t", "b", 300, 3, 1)], 1.0);
        }
        let sum = acc.summarize();
        let a = sum.keys.iter().find(|r| r.name == "a").unwrap();
        let b = sum.keys.iter().find(|r| r.name == "b").unwrap();
        assert!((a.expected_share - 0.25).abs() < 1e-9);
        assert!((a.realized_share - 0.25).abs() < 1e-9);
        assert!((b.share_ratio - 1.0).abs() < 1e-9);
        assert!((sum.key_jain_index - 1.0).abs() < 1e-9);
    }

    #[test]
    fn one_key_hogging_its_tenant_lowers_key_jain() {
        let mut acc = PoolAccumulator::new("m", 4);
        for _ in 0..10 {
            acc.observe(&[], &[k("t", "a", 100, 4, 0), k("t", "b", 100, 0, 3)], 1.0);
        }
        assert!((acc.summarize().key_jain_index - 0.5).abs() < 1e-9);
    }

    #[test]
    fn idle_capacity_with_a_backlog_is_counted() {
        let mut acc = PoolAccumulator::new("m", 4);
        acc.observe(&[s("t", 2, 0, 0.0, 1.0)], &[], 1.0); // fine: under cap, nothing waiting
        acc.observe(&[s("t", 2, 3, 0.0, 1.0)], &[], 1.0); // 2 of 4 used, 3 waiting -> idle with backlog
        acc.observe(&[s("t", 4, 3, 0.0, 1.0)], &[], 1.0); // full: fine
        assert_eq!(acc.summarize().idle_with_backlog_ticks, 1);
    }

    #[test]
    fn an_entity_on_its_own_cap_does_not_read_as_idle_capacity() {
        // 2 of 4 pool slots used with 3 waiting, and everything that is waiting
        // is at its own ceiling of 2 — the queue is waiting on those caps, not
        // on the pool. The fairshare fleet seeds exactly this shape.
        let mut acc = PoolAccumulator::new("m", 4);
        let mut key = k("t", "a", 100, 2, 3);
        key.max_in_flight = Some(2);
        let mut tenant = s("t", 2, 3, 0.0, 1.0);
        tenant.max_in_flight = Some(2);
        acc.observe(&[tenant], &[key], 1.0);

        // Same shape with only the tenant's ceiling reported.
        let mut tenant = s("t", 2, 3, 0.0, 1.0);
        tenant.max_in_flight = Some(2);
        acc.observe(&[tenant], &[], 1.0);

        let sum = acc.summarize();
        assert_eq!(sum.idle_with_backlog_ticks, 0);
        assert_eq!(sum.cap_violations, 0);
        assert_eq!(sum.samples, 2);
    }

    #[test]
    fn a_capped_key_with_nothing_waiting_does_not_excuse_an_idle_pool() {
        // The capped key sits on its ceiling but has no backlog; the queue
        // belongs to an uncapped tenant and 2 of the 4 pool slots are free, so
        // the scheduler really is leaving capacity idle under demand.
        let mut acc = PoolAccumulator::new("m", 4);
        let mut capped = k("t", "a", 100, 2, 0);
        capped.max_in_flight = Some(2);
        acc.observe(&[s("t", 2, 3, 0.0, 1.0)], &[capped], 1.0);
        let sum = acc.summarize();
        assert_eq!(sum.idle_with_backlog_ticks, 1);
        assert_eq!(sum.cap_violations, 0);
    }

    #[test]
    fn a_pool_with_no_reported_cap_counts_nothing() {
        // An older gateway omits the pool size. With no cap there is no such
        // thing as a free slot or an overrun, so neither may be inferred.
        let mut acc = PoolAccumulator::new("m", 0);
        acc.observe(&[s("t", 5, 4, 0.0, 1.0)], &[k("t", "a", 100, 5, 4)], 1.0);
        let sum = acc.summarize();
        assert_eq!(sum.cap, 0);
        assert_eq!(sum.idle_with_backlog_ticks, 0);
        assert_eq!(sum.cap_violations, 0);
    }

    #[test]
    fn a_few_idle_ticks_in_a_long_run_do_not_fail_it() {
        // Three idle-with-backlog ticks out of 100 polls is noise, not a
        // scheduler leaving the pool idle under demand.
        let mut acc = PoolAccumulator::new("m", 4);
        for _ in 0..3 {
            acc.observe(&[s("t", 1, 5, 0.0, 1.0)], &[], 1.0);
        }
        for _ in 0..97 {
            acc.observe(&[s("t", 4, 5, 0.0, 1.0)], &[], 1.0);
        }
        let sum = acc.summarize();
        assert_eq!(sum.idle_with_backlog_ticks, 3);
        assert_eq!(sum.samples, 100);
        assert_eq!(
            apply_pool_verdicts(crate::engine::stats::Verdict::Pass, &[sum]),
            crate::engine::stats::Verdict::Pass
        );
    }

    #[test]
    fn pool_issues_append_to_an_already_failing_run() {
        let mut bad = PoolAccumulator::new("bad", 4);
        for _ in 0..3 {
            bad.observe(&[s("t", 1, 5, 0.0, 1.0)], &[], 1.0);
        }
        let existing = crate::engine::stats::Verdict::Fail(vec!["error rate too high".into()]);
        match apply_pool_verdicts(existing, &[bad.summarize()]) {
            crate::engine::stats::Verdict::Fail(issues) => {
                assert_eq!(issues[0], "error rate too high");
                assert!(issues.len() > 1, "{issues:?}");
                assert!(issues[1..].iter().any(|i| i.contains("bad")), "{issues:?}");
            }
            crate::engine::stats::Verdict::Pass => panic!("must stay failed"),
        }
    }

    #[test]
    fn cap_violations_are_counted_per_entity() {
        let mut acc = PoolAccumulator::new("m", 2);
        let mut t = s("t", 3, 0, 0.0, 1.0);
        t.max_in_flight = Some(2);
        let mut key = k("t", "a", 100, 2, 0);
        key.max_in_flight = Some(1);
        acc.observe(&[t], &[key], 1.0);
        assert_eq!(acc.summarize().cap_violations, 3); // pool 3>2, tenant 3>2, key 2>1
    }

    #[test]
    fn pool_verdict_fails_on_idle_backlog_and_cap_violation() {
        let mut ok = PoolAccumulator::new("ok", 4);
        ok.observe(&[s("t", 4, 1, 0.0, 1.0)], &[], 1.0);
        let mut bad = PoolAccumulator::new("bad", 4);
        for _ in 0..3 {
            bad.observe(&[s("t", 1, 5, 0.0, 1.0)], &[], 1.0); // three ticks: past the 2-tick grace
        }
        let verdict = apply_pool_verdicts(
            crate::engine::stats::Verdict::Pass,
            &[ok.summarize(), bad.summarize()],
        );
        match verdict {
            crate::engine::stats::Verdict::Fail(issues) => assert!(issues[0].contains("bad")),
            _ => panic!("expected failure"),
        }
    }

    #[test]
    fn jain_index_of_empty_is_zero() {
        assert_eq!(jain_index(&[]), 0.0);
    }

    #[test]
    fn jain_index_of_identical_ratios_is_one() {
        assert!((jain_index(&[1.0, 1.0, 1.0]) - 1.0).abs() < 1e-9);
        assert!((jain_index(&[0.4, 0.4]) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jain_index_of_all_zero_ratios_is_zero() {
        assert_eq!(jain_index(&[0.0, 0.0]), 0.0);
    }
}
