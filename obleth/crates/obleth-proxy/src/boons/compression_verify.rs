//! End-to-end VERIFICATION harness for the lossless structural compression boon.
//!
//! This is not an ordinary unit test. It runs the REAL `compression::apply`
//! pipeline (the same code path a live request hits) over a corpus of realistic
//! payloads and, per fixture, measures the token reduction AND proves the result
//! is lossless by decoding the compacted output back to the exact original value.
//!
//! It serves three purposes at once:
//!   1. A regression suite — each fixture asserts its expected behavior.
//!   2. A losslessness proof — every compressed fixture round-trips exactly.
//!   3. A human-readable, tunable report. To see the table:
//!        cargo test -p obleth-proxy compression_verify::report -- --nocapture
//!
//! To TUNE: the report prints results at the production default (`min_tokens=512`)
//! AND at an aggressive `min_tokens=16`, so the effect of the per-segment floor
//! is visible. Change the settings at the call sites to explore other values.

#![cfg(test)]

use obleth_config::CompressionBoonSettings;
use obleth_tokenizer::{HeuristicTokenizer, Tokenizer};
use serde_json::{json, Value};

use super::{compression, structural_json};

// ── Fixture corpus ───────────────────────────────────────────────────────────

/// How a fixture's compacted segment is checked for losslessness.
enum Verify {
    /// The whole compacted segment is a structural form; decoding it must yield
    /// this exact `Value`.
    WholeSegment(Value),
    /// Compacted prose: output must be `prefix` + <structural region> + `suffix`
    /// with the surrounding bytes preserved, and the region must decode to `data`.
    EmbeddedSpan { prefix: String, suffix: String, data: Value },
    /// The segment must come back byte-identical (we deliberately did not touch it).
    Unchanged,
}

/// What we expect the boon to do with this fixture at the PRODUCTION default.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Compresses,
    LeavesUnchanged,
}

struct Fixture {
    name: &'static str,
    /// One-line description of the payload shape being exercised.
    shape: &'static str,
    /// The single message content under test (request is one user message).
    content: String,
    expect: Expect,
    verify: Verify,
}

/// A realistic record like the one in the reported trace: id/name/email/role/
/// active/score. `i` varies every field that would vary in real data.
fn users(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            json!({
                "id": i,
                "name": format!("user{i}"),
                "email": format!("u{i}@example.com"),
                "role": "member",
                "active": true,
                "score": 180 + i
            })
        })
        .collect()
}

/// Objects with DIFFERING key sets (sparse / non-uniform), to exercise the
/// union-of-keys table encoder.
fn sparse_records(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                json!({"id": i, "name": format!("u{i}"), "active": true})
            } else {
                json!({"id": i, "email": format!("u{i}@x.com"), "score": i * 2})
            }
        })
        .collect()
}

fn orders(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| json!({"order_id": 1000 + i, "qty": i % 7, "total": i * 3, "paid": i % 2 == 0}))
        .collect()
}

fn corpus() -> Vec<Fixture> {
    // Embedded-in-prose fixture bounds (must be preserved byte-for-byte).
    let prose_prefix = "Here is the data you asked for: ";
    let prose_suffix = " — please summarize the trends.";
    let fence_prefix = "```json\n";
    let fence_suffix = "\n```";

    vec![
        Fixture {
            name: "standalone_user_array_80",
            shape: "top-level array of 80 uniform objects (the reported trace shape)",
            content: serde_json::to_string(&json!(users(80))).unwrap(),
            expect: Expect::Compresses,
            verify: Verify::WholeSegment(json!(users(80))),
        },
        Fixture {
            name: "wrapped_results_array_200",
            shape: "array WRAPPED in an object: {\"results\":[200…],\"meta\":{…}}",
            content: serde_json::to_string(&json!({
                "results": users(200),
                "meta": {"total": 200, "page": 1}
            }))
            .unwrap(),
            expect: Expect::Compresses,
            verify: Verify::WholeSegment(json!({
                "results": users(200),
                "meta": {"total": 200, "page": 1}
            })),
        },
        Fixture {
            name: "array_embedded_in_prose_100",
            shape: "100-object array pasted INSIDE a sentence",
            content: format!(
                "{prose_prefix}{}{prose_suffix}",
                serde_json::to_string(&json!(users(100))).unwrap()
            ),
            expect: Expect::Compresses,
            verify: Verify::EmbeddedSpan {
                prefix: prose_prefix.to_string(),
                suffix: prose_suffix.to_string(),
                data: json!(users(100)),
            },
        },
        Fixture {
            name: "fenced_json_array_60",
            shape: "60-object array inside a ```json fenced block",
            content: format!(
                "{fence_prefix}{}{fence_suffix}",
                serde_json::to_string(&json!(users(60))).unwrap()
            ),
            expect: Expect::Compresses,
            verify: Verify::EmbeddedSpan {
                prefix: fence_prefix.to_string(),
                suffix: fence_suffix.to_string(),
                data: json!(users(60)),
            },
        },
        Fixture {
            name: "sparse_keys_array_120",
            shape: "top-level array of 120 objects with NON-UNIFORM keys",
            content: serde_json::to_string(&json!(sparse_records(120))).unwrap(),
            expect: Expect::Compresses,
            verify: Verify::WholeSegment(json!(sparse_records(120))),
        },
        Fixture {
            name: "multiple_arrays_in_object",
            shape: "object holding TWO qualifying arrays: {users:[40], orders:[40]}",
            content: serde_json::to_string(&json!({
                "users": users(40),
                "orders": orders(40)
            }))
            .unwrap(),
            expect: Expect::Compresses,
            verify: Verify::WholeSegment(json!({
                "users": users(40),
                "orders": orders(40)
            })),
        },
        Fixture {
            name: "plain_prose_thread_control",
            shape: "CONTROL: long human prose, no JSON (Phase 1 must NOT touch it)",
            content: prose_thread(),
            expect: Expect::LeavesUnchanged,
            verify: Verify::Unchanged,
        },
        Fixture {
            name: "small_array_below_floor",
            shape: "CONTROL: tiny 5-object array, below the 512-token floor",
            content: serde_json::to_string(&json!(users(5))).unwrap(),
            expect: Expect::LeavesUnchanged,
            // It would compress fine if the floor allowed it (see the aggressive
            // report) — so verify the compacted form when it IS rewritten.
            verify: Verify::WholeSegment(json!(users(5))),
        },
    ]
}

/// A long, genuinely prose paragraph set with no JSON structures — comfortably
/// over the 512-token floor so it is scanned but (correctly) left untouched.
fn prose_thread() -> String {
    let para = "The quarterly review covered onboarding, retention, and the support \
        backlog in depth. Several reviewers noted that response times improved after \
        the new routing policy, though the weekend coverage gap remains a concern. \
        Action items were assigned across the team with owners and target dates. ";
    para.repeat(12)
}

// ── Runner + metrics ─────────────────────────────────────────────────────────

struct Metrics {
    before: u32,
    after: u32,
    compressed: u32,
    changed: bool,
    lossless: bool,
}

impl Metrics {
    fn saved_pct(&self) -> f64 {
        if self.before == 0 {
            0.0
        } else {
            (self.before - self.after) as f64 / self.before as f64 * 100.0
        }
    }
}

/// Run one fixture through the real `apply` pipeline and verify losslessness.
fn run(fix: &Fixture, cfg: &CompressionBoonSettings) -> Metrics {
    let tk = HeuristicTokenizer::new();
    let before = tk.count_text(&fix.content);

    let mut body = json!({
        "model": "verify",
        "messages": [{"role": "user", "content": fix.content.clone()}]
    });
    let stats = compression::apply(cfg, false, &mut body);

    let after_text = body["messages"][0]["content"].as_str().unwrap().to_string();
    let after = tk.count_text(&after_text);
    let changed = after_text != fix.content;

    // Losslessness is only meaningful when the segment was actually rewritten. A
    // segment left untouched (below the floor, or nothing structural to do) is
    // trivially faithful — the report shows it as "kept", not pass/fail.
    let lossless = if !changed {
        true
    } else {
        match &fix.verify {
            // We expected this segment untouched, but it changed → not OK.
            Verify::Unchanged => false,
            Verify::WholeSegment(data) => {
                structural_json::decode_for_verification(&after_text).as_ref() == Some(data)
            }
            Verify::EmbeddedSpan { prefix, suffix, data } => {
                after_text.starts_with(prefix.as_str())
                    && after_text.ends_with(suffix.as_str())
                    && after_text.len() >= prefix.len() + suffix.len()
                    && {
                        let mid = &after_text[prefix.len()..after_text.len() - suffix.len()];
                        structural_json::decode_for_verification(mid).as_ref() == Some(data)
                    }
            }
        }
    };

    Metrics { before, after, compressed: stats.compressed, changed, lossless }
}

fn prod_cfg() -> CompressionBoonSettings {
    // Exactly the production defaults except the master switch is on.
    CompressionBoonSettings { enabled: true, ..Default::default() }
}

fn aggressive_cfg() -> CompressionBoonSettings {
    CompressionBoonSettings { enabled: true, min_tokens: 16, ..Default::default() }
}

// ── Regression assertions ────────────────────────────────────────────────────

/// Every corpus fixture behaves as expected at the PRODUCTION default settings,
/// and every compressed segment is provably lossless.
#[test]
fn corpus_behaves_as_expected_at_production_defaults() {
    let cfg = prod_cfg();
    for fix in corpus() {
        let m = run(&fix, &cfg);
        assert!(m.lossless, "{}: NOT lossless (changed={})", fix.name, m.changed);
        match fix.expect {
            Expect::Compresses => assert!(
                m.compressed >= 1 && m.after < m.before,
                "{}: expected compression, got before={} after={} compressed={}",
                fix.name, m.before, m.after, m.compressed
            ),
            Expect::LeavesUnchanged => assert!(
                m.compressed == 0 && !m.changed,
                "{}: expected unchanged, but it was modified (compressed={})",
                fix.name, m.compressed
            ),
        }
    }
}

/// The 512-token floor is what suppressed compression in the field: the same
/// small array that is left alone at the default IS compressed (losslessly) once
/// the floor is lowered. This is the knob to tune.
#[test]
fn min_tokens_floor_gates_small_payloads() {
    let small = serde_json::to_string(&json!(users(5))).unwrap();
    let fix = Fixture {
        name: "small_array_tuning",
        shape: "tiny array under the default floor",
        content: small,
        expect: Expect::Compresses,
        verify: Verify::WholeSegment(json!(users(5))),
    };

    // At the production floor it is skipped …
    let at_default = run(&fix, &prod_cfg());
    assert_eq!(at_default.compressed, 0, "small array should be below the 512 floor");
    assert!(!at_default.changed);

    // … and at a lower floor it compresses, still losslessly.
    let at_low = run(&fix, &aggressive_cfg());
    assert!(at_low.compressed >= 1 && at_low.after < at_low.before, "lowering the floor should compress it");
    assert!(at_low.lossless, "still lossless at the lower floor");
}

// ── Human-readable report ────────────────────────────────────────────────────

fn print_report(title: &str, cfg: &CompressionBoonSettings) {
    println!("\n{title}");
    println!(
        "settings: enabled={} min_tokens={} max_segments={} code_compaction={}",
        cfg.enabled, cfg.min_tokens, cfg.max_segments, cfg.code_compaction
    );
    println!("{}", "─".repeat(104));
    println!(
        "{:<30} {:>10} {:>10} {:>9} {:>6} {:>10}  {}",
        "fixture", "tok_before", "tok_after", "saved", "segs", "lossless", "shape"
    );
    println!("{}", "─".repeat(104));

    let (mut tot_before, mut tot_after) = (0u32, 0u32);
    for fix in corpus() {
        let m = run(&fix, cfg);
        tot_before += m.before;
        tot_after += m.after;
        let lossless = if m.compressed > 0 {
            if m.lossless { "OK ✓" } else { "FAIL ✗" }
        } else if m.lossless {
            "— (kept)"
        } else {
            "FAIL ✗"
        };
        println!(
            "{:<30} {:>10} {:>10} {:>8.1}% {:>6} {:>10}  {}",
            fix.name, m.before, m.after, m.saved_pct(), m.compressed, lossless, fix.shape
        );
    }
    println!("{}", "─".repeat(104));
    let total_pct = if tot_before == 0 {
        0.0
    } else {
        (tot_before - tot_after) as f64 / tot_before as f64 * 100.0
    };
    println!(
        "{:<30} {:>10} {:>10} {:>8.1}%",
        "TOTAL", tot_before, tot_after, total_pct
    );
}

/// Prints the verification report. Run with:
///   cargo test -p obleth-proxy compression_verify::report -- --nocapture
#[test]
fn report() {
    print_report(
        "COMPRESSION VERIFICATION — PRODUCTION DEFAULTS",
        &prod_cfg(),
    );
    print_report(
        "COMPRESSION VERIFICATION — AGGRESSIVE (min_tokens=16) — shows the floor's effect",
        &aggressive_cfg(),
    );
    println!(
        "\nNote: 'plain_prose_thread_control' is SUPPOSED to stay at 0% — deterministic \
         lossless compression cannot shrink human prose without changing meaning. That is \
         Phase 2 (the neural prose compressor).\n"
    );
}
