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

use serde_json::{json, Value};

/// An OpenAI-style `messages` array.
pub type Messages = Vec<Value>;

fn user(content: String) -> Messages {
    vec![json!({ "role": "user", "content": content })]
}

/// Repetitive syslog lines — the near-free deterministic (log template-collapse) path.
pub fn logs_payload(n_lines: usize) -> Messages {
    let hosts = ["web-01", "web-02", "db-03", "cache-05"];
    let svc = ["nginx", "systemd", "kernel", "sshd"];
    let mut lines = Vec::with_capacity(n_lines);
    for i in 0..n_lines {
        let h = hosts[i % hosts.len()];
        let s = svc[i % svc.len()];
        lines.push(format!(
            "Jun 30 12:{:02}:{:02} {h} {s}[{}]: request {i} completed in {}ms status=200 bytes={}",
            i % 60,
            (i * 7) % 60,
            1000 + i,
            12 + (i % 40),
            2048 + i
        ));
    }
    user(format!("Summarize these logs:\n{}", lines.join("\n")))
}

/// Uniform JSON rows — the structural (json) deterministic path.
pub fn json_payload(n_rows: usize) -> Messages {
    let rows: Vec<Value> = (0..n_rows)
        .map(|i| {
            json!({ "id": i, "user": format!("user{i}"), "status": "active", "score": i * 3, "region": "us-east" })
        })
        .collect();
    let blob = serde_json::to_string(&json!({ "results": rows })).unwrap();
    user(format!("Analyze this data:\n{blob}"))
}

/// Whitespace-heavy code — the code compactor path.
pub fn code_payload(n_funcs: usize) -> Messages {
    let mut parts = Vec::with_capacity(n_funcs);
    for i in 0..n_funcs {
        parts.push(format!(
            "def handler_{i}(request,   context):\n    # process the incoming request for endpoint {i}\n    result   =   compute({i},  request.payload)\n\n\n    return    result\n"
        ));
    }
    user(format!("Review this code:\n```python\n{}\n```", parts.join("\n")))
}

/// Low-density human prose — the neural lossy path (where the sidecar overhead
/// makes the latency crossover interesting).
pub fn prose_payload(n_paras: usize) -> Messages {
    let filler = "As you can probably imagine, there are a great many different things one might reasonably want to take into careful consideration here, and it is, at the end of the day, genuinely important to keep all of them in mind as we move forward together on this particular initiative. ";
    let dense = "Revenue grew 12% to $4.2M in Q3, driven by enterprise renewals; churn fell to 3.1%. The migration finished at 02:14 UTC with zero data loss. ";
    let para = format!("{dense}{}", filler.repeat(3));
    let paras: Vec<String> = (0..n_paras).map(|_| para.clone()).collect();
    user(format!("Read this report:\n\n{}", paras.join("\n\n")))
}

/// One large block sent twice in a single request → exercises cross-turn dedup.
pub fn repeated_payload(n_rows: usize) -> Messages {
    let doc: Vec<Value> = (0..n_rows)
        .map(|i| json!({ "k": i, "v": format!("value-{i}"), "note": "reference" }))
        .collect();
    let block = serde_json::to_string(&json!({ "doc": doc })).unwrap();
    vec![
        json!({ "role": "user", "content": format!("Here is the document:\n{block}") }),
        json!({ "role": "assistant", "content": "Understood, I have the document." }),
        json!({ "role": "user", "content": format!("Using the SAME document again:\n{block}\nWhat changed?") }),
    ]
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

    #[test]
    fn logs_payload_has_one_user_turn() {
        let m = logs_payload(120);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0]["role"], "user");
        assert!(m[0]["content"].as_str().unwrap().lines().count() >= 120);
    }

    #[test]
    fn repeated_payload_reuses_block_across_turns() {
        let m = repeated_payload(50);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0]["role"], "user");
        assert_eq!(m[1]["role"], "assistant");
        assert_eq!(m[2]["role"], "user");
        // The same document block appears in both user turns (dedup target).
        let first = m[0]["content"].as_str().unwrap();
        let block = &first["Here is the document:\n".len()..];
        assert!(m[2]["content"].as_str().unwrap().contains(block));
    }

    #[test]
    fn json_and_code_and_prose_are_single_user_turns() {
        for m in [json_payload(120), code_payload(40), prose_payload(6)] {
            assert_eq!(m.len(), 1);
            assert_eq!(m[0]["role"], "user");
            assert!(!m[0]["content"].as_str().unwrap().is_empty());
        }
    }
}
