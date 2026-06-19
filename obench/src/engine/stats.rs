use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct RequestOutcome {
    pub status: u16,
    pub ttfb_ms: u64,
    pub total_ms: u64,
    pub in_tokens: u64,
    pub out_tokens: u64,
    pub usage_estimated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Pass,
    Fail(Vec<String>),
}

#[derive(Default)]
pub struct Stats {
    pub ok: u64,
    pub rejected: u64,
    pub error: u64,
    pub ttfb: Vec<u64>,
    pub total: Vec<u64>,
    pub statuses: BTreeMap<u16, u64>,
    pub in_tokens: u64,
    pub out_tokens: u64,
    pub any_estimated: bool,
    /// True if a sample window saw zero completions while load was active.
    pub stalled: bool,
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub attempts: u64,
    pub completed: u64,
    pub rejected: u64,
    pub errors: u64,
    pub error_rate: f64,
    pub req_per_s: f64,
    pub p50_ttfb_ms: u64,
    pub p90_ttfb_ms: u64,
    pub p99_ttfb_ms: u64,
    pub p50_total_ms: u64,
    pub p99_total_ms: u64,
    pub in_tokens: u64,
    pub out_tokens: u64,
    pub any_estimated: bool,
    pub verdict: Verdict,
}

/// Nearest-rank percentile over an unsorted slice. Matches the .mjs harness.
pub fn percentile(values: &[u64], p: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let idx = ((p / 100.0) * sorted.len() as f64).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

impl Stats {
    pub fn record(&mut self, r: &RequestOutcome) {
        *self.statuses.entry(r.status).or_insert(0) += 1;
        match r.status {
            200 => {
                self.ok += 1;
                self.ttfb.push(r.ttfb_ms);
                self.total.push(r.total_ms);
                self.in_tokens += r.in_tokens;
                self.out_tokens += r.out_tokens;
                if r.usage_estimated {
                    self.any_estimated = true;
                }
            }
            429 => self.rejected += 1,
            _ => self.error += 1,
        }
    }

    pub fn summarize(&self, elapsed_s: f64, max_error_rate: f64) -> Summary {
        let attempts = self.ok + self.rejected + self.error;
        let error_rate = if attempts > 0 {
            self.error as f64 / attempts as f64
        } else {
            0.0
        };
        let req_per_s = if elapsed_s > 0.0 {
            self.ok as f64 / elapsed_s
        } else {
            0.0
        };

        let mut issues = Vec::new();
        if error_rate > max_error_rate {
            issues.push(format!(
                "error rate {:.2}% exceeded threshold {:.2}%",
                error_rate * 100.0,
                max_error_rate * 100.0
            ));
        }
        if self.stalled {
            issues.push("traffic stalled: a sample window saw zero completions".to_string());
        }
        let verdict = if issues.is_empty() {
            Verdict::Pass
        } else {
            Verdict::Fail(issues)
        };

        Summary {
            attempts,
            completed: self.ok,
            rejected: self.rejected,
            errors: self.error,
            error_rate,
            req_per_s,
            p50_ttfb_ms: percentile(&self.ttfb, 50.0),
            p90_ttfb_ms: percentile(&self.ttfb, 90.0),
            p99_ttfb_ms: percentile(&self.ttfb, 99.0),
            p50_total_ms: percentile(&self.total, 50.0),
            p99_total_ms: percentile(&self.total, 99.0),
            in_tokens: self.in_tokens,
            out_tokens: self.out_tokens,
            any_estimated: self.any_estimated,
            verdict,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(ttfb: u64, total: u64) -> RequestOutcome {
        RequestOutcome { status: 200, ttfb_ms: ttfb, total_ms: total, in_tokens: 10, out_tokens: 20, usage_estimated: false }
    }

    #[test]
    fn percentile_nearest_rank() {
        let v = vec![10, 20, 30, 40, 50];
        assert_eq!(percentile(&v, 50.0), 30);
        assert_eq!(percentile(&v, 99.0), 50);
        assert_eq!(percentile(&[], 50.0), 0);
    }

    #[test]
    fn clean_run_passes() {
        let mut s = Stats::default();
        for _ in 0..100 { s.record(&ok(10, 20)); }
        let sum = s.summarize(10.0, 0.05);
        assert_eq!(sum.verdict, Verdict::Pass);
        assert_eq!(sum.completed, 100);
        assert!((sum.req_per_s - 10.0).abs() < 0.01);
    }

    #[test]
    fn high_error_rate_fails() {
        let mut s = Stats::default();
        for _ in 0..90 { s.record(&ok(10, 20)); }
        for _ in 0..10 {
            s.record(&RequestOutcome { status: 500, ttfb_ms: 0, total_ms: 0, in_tokens: 0, out_tokens: 0, usage_estimated: false });
        }
        let sum = s.summarize(10.0, 0.05);
        match sum.verdict { Verdict::Fail(v) => assert!(v[0].contains("error rate")), _ => panic!("expected fail") }
    }

    #[test]
    fn stall_fails_even_with_no_errors() {
        let mut s = Stats::default();
        s.record(&ok(10, 20));
        s.stalled = true;
        let sum = s.summarize(10.0, 0.05);
        assert!(matches!(sum.verdict, Verdict::Fail(_)));
    }

    #[test]
    fn rejected_429_is_not_an_error() {
        let mut s = Stats::default();
        s.record(&RequestOutcome { status: 429, ttfb_ms: 0, total_ms: 0, in_tokens: 0, out_tokens: 0, usage_estimated: false });
        let sum = s.summarize(1.0, 0.0);
        assert_eq!(sum.errors, 0);
        assert_eq!(sum.rejected, 1);
    }
}
