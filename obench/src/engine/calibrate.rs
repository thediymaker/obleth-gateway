#[derive(Clone, Copy, Debug)]
pub struct StepResult {
    pub conc: u32,
    pub req_per_s: f64,
    pub error_rate: f64,
    pub p99_ttfb_ms: u64,
    /// Reserved for a future queued-depth signal; currently always 0 and not
    /// read by `evaluate`. Keep the field to avoid a breaking struct-literal
    /// change when the fairshare sampler is wired in.
    #[allow(dead_code)]
    pub max_queued: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct KneeConfig {
    /// Any step above this error rate is the knee.
    pub error_ceiling: f64,
    /// p99 growth factor vs previous step that counts as "latency climbing".
    pub latency_growth: f64,
    /// Throughput gain factor below which more load is not buying throughput.
    pub throughput_floor_gain: f64,
}

impl Default for KneeConfig {
    fn default() -> Self {
        Self {
            error_ceiling: 0.01,
            latency_growth: 1.5,
            throughput_floor_gain: 1.1,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Continue,
    Stop {
        last_clean_conc: u32,
        reason: String,
    },
}

pub fn evaluate(history: &[StepResult], latest: &StepResult, cfg: &KneeConfig) -> Decision {
    let prev_clean = history.last().map(|s| s.conc).unwrap_or(0);

    if latest.error_rate > cfg.error_ceiling {
        return Decision::Stop {
            last_clean_conc: prev_clean,
            reason: format!(
                "error rate {:.2}% crossed the ceiling",
                latest.error_rate * 100.0
            ),
        };
    }

    if let Some(prev) = history.last() {
        let latency_climb =
            latest.p99_ttfb_ms as f64 >= prev.p99_ttfb_ms as f64 * cfg.latency_growth;
        let throughput_flat = latest.req_per_s < prev.req_per_s * cfg.throughput_floor_gain;
        if latency_climb && throughput_flat {
            return Decision::Stop {
                last_clean_conc: prev.conc,
                reason: "latency climbed while throughput flattened".to_string(),
            };
        }
    }

    Decision::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(conc: u32, rps: f64, err: f64, p99: u64) -> StepResult {
        StepResult {
            conc,
            req_per_s: rps,
            error_rate: err,
            p99_ttfb_ms: p99,
            max_queued: 0,
        }
    }

    #[test]
    fn continues_while_clean_and_scaling() {
        let hist = vec![step(64, 1000.0, 0.0, 40)];
        let latest = step(128, 1900.0, 0.0, 45);
        assert_eq!(
            evaluate(&hist, &latest, &KneeConfig::default()),
            Decision::Continue
        );
    }

    #[test]
    fn stops_on_errors_keeping_last_clean() {
        let hist = vec![step(128, 1900.0, 0.0, 45)];
        let latest = step(256, 2000.0, 0.05, 60);
        match evaluate(&hist, &latest, &KneeConfig::default()) {
            Decision::Stop {
                last_clean_conc, ..
            } => assert_eq!(last_clean_conc, 128),
            _ => panic!("expected stop"),
        }
    }

    #[test]
    fn stops_when_latency_climbs_and_throughput_flattens() {
        let hist = vec![step(128, 2000.0, 0.0, 50)];
        let latest = step(256, 2050.0, 0.0, 90); // +2.5% rps, +80% p99
        match evaluate(&hist, &latest, &KneeConfig::default()) {
            Decision::Stop {
                last_clean_conc, ..
            } => assert_eq!(last_clean_conc, 128),
            _ => panic!("expected stop"),
        }
    }

    #[test]
    fn first_step_always_continues() {
        let latest = step(64, 1000.0, 0.0, 40);
        assert_eq!(
            evaluate(&[], &latest, &KneeConfig::default()),
            Decision::Continue
        );
    }

    #[test]
    fn first_step_errors_reports_zero_as_no_clean_level() {
        let latest = step(64, 100.0, 0.05, 40); // first step already over the 1% ceiling
        match evaluate(&[], &latest, &KneeConfig::default()) {
            Decision::Stop {
                last_clean_conc, ..
            } => assert_eq!(last_clean_conc, 0),
            _ => panic!("expected stop at first-step error"),
        }
    }
}
