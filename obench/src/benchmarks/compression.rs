//! Back-to-back A/B benchmark for the obleth compression boon (ported from the
//! former bench/compression/ab.py). For each corpus sample it sends the SAME
//! request three ways — off / default / lossy — and diffs the result.

/// Parse the `x-obleth-compression` response header (`before=N;after=M;saved=K`)
/// into `(before, after, saved)`. Returns None if `before` is absent.
pub fn parse_compression_header(val: &str) -> Option<(u64, u64, u64)> {
    let mut before = None;
    let mut after = 0u64;
    let mut saved = 0u64;
    for kv in val.split(';') {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        let n: u64 = v.trim().parse().ok()?;
        match k.trim() {
            "before" => before = Some(n),
            "after" => after = n,
            "saved" => saved = n,
            _ => {}
        }
    }
    before.map(|b| (b, after, saved))
}

/// Percent of input tokens removed. Zero when there were no input tokens.
pub fn pct(before: u64, after: u64) -> f64 {
    if before == 0 {
        0.0
    } else {
        (before as f64 - after as f64) / before as f64 * 100.0
    }
}

/// Modeled net latency win at a given upstream prefill rate:
/// `upstream_ms_saved - gateway_overhead`, where upstream_ms_saved = saved/tps*1000.
/// Positive = compression makes the request faster end-to-end.
pub fn net_ms(saved: u64, tps: u32, overhead_ms: f64) -> f64 {
    if tps == 0 {
        -overhead_ms
    } else {
        saved as f64 / tps as f64 * 1000.0 - overhead_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_header() {
        assert_eq!(
            parse_compression_header("before=100;after=60;saved=40"),
            Some((100, 60, 40))
        );
    }

    #[test]
    fn missing_before_is_none() {
        assert_eq!(parse_compression_header("after=60;saved=40"), None);
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(parse_compression_header(""), None);
        assert_eq!(parse_compression_header("nonsense"), None);
    }

    #[test]
    fn pct_basic_and_zero() {
        assert!((pct(100, 60) - 40.0).abs() < 1e-9);
        assert_eq!(pct(0, 0), 0.0);
    }

    #[test]
    fn net_ms_positive_and_zero_tps() {
        // 4000 tokens saved at 2000 tok/s = 2000 ms upstream, minus 500 ms overhead.
        assert!((net_ms(4000, 2000, 500.0) - 1500.0).abs() < 1e-9);
        assert_eq!(net_ms(4000, 0, 500.0), -500.0);
    }
}
